//! Behavior tests for the loopback pairing store, token service, capture
//! protocol validation, and admission primitives.
//!
//! The tests use only the agreed public `oneget_lib::api` interface and a
//! temporary SQLite file per pairing store.

use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::Connection;
use tempfile::{tempdir, TempDir};

use oneget_lib::api::clock::{Clock, SystemClock};
use oneget_lib::api::error::{self, ApiError};
use oneget_lib::api::pairing::{
    generate_token, hash_secret, verify_secret_hash, CODE_TTL_MS, LAST_USED_UPDATE_INTERVAL_MS,
    LOCKOUT_MS, MAX_FAILED_ATTEMPTS, TOKEN_BYTES,
};
use oneget_lib::api::pairing_store::{PairResult, PairedClient, PairingStore};
use oneget_lib::api::protocol::{
    is_chromium_extension_origin, loopback_authority, validate_capture_request, CaptureHeader,
    CaptureIntent, CaptureItem, CaptureKind, CaptureRequest, CaptureValidation, API_HOST, API_PORT,
    API_PROTOCOL_VERSION, MAX_BODY_BYTES, MAX_CAPTURE_HEADERS, MAX_CAPTURE_ITEMS,
    MAX_FILENAME_BYTES, MAX_HEADER_NAME_BYTES, MAX_HEADER_VALUE_BYTES, MAX_URL_BYTES,
    SAFE_CAPTURE_HEADERS,
};
use oneget_lib::api::rate_limit::{RateLimitPolicy, RateLimiter};

const NOW_MS: i64 = 1_800_000_000_000;

fn open_store() -> (TempDir, PairingStore) {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("pairing.db");
    let store = PairingStore::open(&path).expect("open pairing database");
    (dir, store)
}

fn extension_origin(id_char: char) -> String {
    format!("chrome-extension://{}", id_char.to_string().repeat(32))
}

fn pair_once(
    store: &mut PairingStore,
    now_ms: i64,
    origin: &str,
    label: &str,
) -> (String, PairedClient) {
    let code = store.generate_code(now_ms).expect("generate pairing code");
    match store
        .pair(&code.code, origin, label, now_ms)
        .expect("pairing attempt")
    {
        PairResult::Paired { token, client } => (token, client),
        _ => panic!("expected a paired result"),
    }
}

fn epoch_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock after the Unix epoch")
        .as_millis() as i64
}

fn valid_item() -> CaptureItem {
    CaptureItem {
        url: "https://example.com/file.bin".to_string(),
        filename: Some("file.bin".to_string()),
        referer: Some("https://example.com/page".to_string()),
        mime: Some("application/octet-stream".to_string()),
        size: Some(1_024),
        kind: Some(CaptureKind::File),
        headers: Vec::new(),
    }
}

fn capture_request(intent: CaptureIntent, items: Vec<CaptureItem>) -> CaptureRequest {
    CaptureRequest {
        protocol_version: API_PROTOCOL_VERSION,
        intent,
        source_id: None,
        items,
    }
}

fn request_error(request: &CaptureRequest) -> ApiError {
    match validate_capture_request(request) {
        Ok(_) => panic!("expected a request-level validation error"),
        Err(error) => error,
    }
}

fn rejected_code(validation: &CaptureValidation, index: usize) -> Option<&'static str> {
    validation
        .rejected
        .iter()
        .find(|entry| entry.index == index)
        .map(|entry| entry.code)
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

#[test]
fn pairing_issues_one_lowercase_hex_token_and_lists_one_client() {
    let (_dir, mut store) = open_store();
    let origin = extension_origin('a');

    let code = store.generate_code(NOW_MS).expect("generate code");
    assert_eq!(code.code.len(), 8, "pairing codes are exactly eight digits");
    assert!(code.code.bytes().all(|byte| byte.is_ascii_digit()));
    assert_eq!(code.expires_at_ms, NOW_MS + CODE_TTL_MS);

    let (token, client) = match store
        .pair(&code.code, &origin, "Chrome extension", NOW_MS + 1_000)
        .expect("pair with a fresh code")
    {
        PairResult::Paired { token, client } => (token, client),
        _ => panic!("a fresh correct code must pair"),
    };
    assert_eq!(token.len(), 64);
    assert!(token
        .bytes()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)));
    assert_eq!(client.origin, origin);
    assert_eq!(client.label, "Chrome extension");
    assert_eq!(client.created_at_ms, NOW_MS + 1_000);

    let clients = store.list_clients().expect("list paired clients");
    assert_eq!(clients.len(), 1);
    assert_eq!(clients[0].id, client.id);

    let reused = store
        .pair(&code.code, &origin, "Chrome extension", NOW_MS + 2_000)
        .expect("second use returns a pairing result");
    assert!(
        matches!(reused, PairResult::InvalidCode | PairResult::Expired),
        "a consumed code must not pair a second time"
    );
}

#[test]
fn pairing_database_contains_no_raw_code_or_token_and_reports_one_client() {
    let (dir, mut store) = open_store();
    let origin = extension_origin('b');
    let code = store.generate_code(NOW_MS).expect("generate code");
    let entered_code = code.code.clone();
    let token = match store
        .pair(&entered_code, &origin, "Chrome extension", NOW_MS)
        .expect("pair")
    {
        PairResult::Paired { token, .. } => token,
        _ => panic!("expected pairing to succeed"),
    };
    assert_eq!(store.list_clients().expect("list clients").len(), 1);
    drop(store);

    let db_path = dir.path().join("pairing.db");
    let connection = Connection::open(&db_path).expect("reopen pairing database");
    let user_version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .expect("read schema version");
    assert_eq!(user_version, 1);
    drop(connection);

    let mut scanned = 0;
    for entry in std::fs::read_dir(dir.path()).expect("read pairing directory") {
        let path = entry.expect("directory entry").path();
        if !path.is_file() {
            continue;
        }
        let bytes = std::fs::read(&path).expect("read database file");
        assert!(
            !contains_bytes(&bytes, entered_code.as_bytes()),
            "pairing files must not contain the raw code"
        );
        assert!(
            !contains_bytes(&bytes, token.as_bytes()),
            "pairing files must not contain the raw token"
        );
        scanned += 1;
    }
    assert!(
        scanned >= 1,
        "at least the pairing database file must be inspected"
    );
}

#[test]
fn pairing_and_authorize_failures_do_not_echo_submitted_secrets() {
    let (_dir, mut store) = open_store();
    let origin = extension_origin('c');
    let submitted_code = "91827364";
    let submitted_token = "0123456789abcdef".repeat(4);
    let mut surfaced_errors = Vec::new();

    match store.pair(submitted_code, &origin, "Chrome extension", NOW_MS) {
        Ok(result) => assert!(!matches!(result, PairResult::Paired { .. })),
        Err(error) => surfaced_errors.push(error),
    }
    match store.authorize(&submitted_token, &origin, NOW_MS) {
        Ok(client) => assert!(client.is_none()),
        Err(error) => surfaced_errors.push(error),
    }

    let (real_token, _client) = pair_once(&mut store, NOW_MS + 1_000, &origin, "Chrome extension");
    match store.authorize(&real_token, &origin, NOW_MS + 2_000) {
        Ok(client) => assert_eq!(client.expect("active client").origin, origin),
        Err(error) => surfaced_errors.push(error),
    }

    for error in &surfaced_errors {
        let display = format!("{error}");
        let debug = format!("{error:?}");
        assert!(!display.contains(submitted_code));
        assert!(!display.contains(&submitted_token));
        assert!(!debug.contains(submitted_code));
        assert!(!debug.contains(&submitted_token));
    }
}

#[test]
fn pairing_code_is_valid_until_ten_minutes_and_expires_at_the_boundary() {
    assert_eq!(CODE_TTL_MS, 600_000);
    let origin = extension_origin('a');

    let (_valid_dir, mut valid_store) = open_store();
    let valid_code = valid_store
        .generate_code(NOW_MS)
        .expect("generate valid code");
    let valid = valid_store
        .pair(
            &valid_code.code,
            &origin,
            "Chrome extension",
            NOW_MS + 599_999,
        )
        .expect("pair at the last valid millisecond");
    assert!(matches!(valid, PairResult::Paired { .. }));

    let (_expired_dir, mut expired_store) = open_store();
    let expired_code = expired_store
        .generate_code(NOW_MS)
        .expect("generate expired code");
    let expired = expired_store
        .pair(
            &expired_code.code,
            &origin,
            "Chrome extension",
            NOW_MS + 600_000,
        )
        .expect("pair at the expiry boundary");
    assert!(matches!(expired, PairResult::Expired));
}

#[test]
fn generating_a_new_code_supersedes_the_previous_pending_code() {
    let (_dir, mut store) = open_store();
    let origin = extension_origin('a');

    let first = store.generate_code(NOW_MS).expect("first code");
    let mut replacement = store
        .generate_code(NOW_MS + 1_000)
        .expect("replacement code");
    let mut attempts = 0;
    while replacement.code == first.code && attempts < 5 {
        replacement = store
            .generate_code(NOW_MS + 1_000)
            .expect("replacement code");
        attempts += 1;
    }
    assert_ne!(replacement.code, first.code);

    let old = store
        .pair(&first.code, &origin, "Chrome extension", NOW_MS + 2_000)
        .expect("old code result");
    assert!(
        matches!(old, PairResult::InvalidCode | PairResult::Expired),
        "a superseded code must be rejected"
    );

    let current = store
        .pair(
            &replacement.code,
            &origin,
            "Chrome extension",
            NOW_MS + 3_000,
        )
        .expect("new code result");
    assert!(matches!(current, PairResult::Paired { .. }));
}

#[test]
fn five_wrong_codes_lock_pairing_until_the_lockout_interval_elapses() {
    let (_dir, mut store) = open_store();
    let origin = extension_origin('a');
    assert_eq!(MAX_FAILED_ATTEMPTS, 5);
    assert_eq!(LOCKOUT_MS, 300_000);

    let code = store.generate_code(NOW_MS).expect("generate code");
    let wrong = if code.code == "00000000" {
        "11111111"
    } else {
        "00000000"
    };

    for attempt in 1..=MAX_FAILED_ATTEMPTS {
        let result = store
            .pair(
                wrong,
                &origin,
                "Chrome extension",
                NOW_MS + i64::from(attempt) * 1_000,
            )
            .expect("wrong code result");
        if attempt < MAX_FAILED_ATTEMPTS {
            assert!(
                matches!(result, PairResult::InvalidCode),
                "attempt {attempt} must not lock early"
            );
        } else {
            match result {
                PairResult::Locked { retry_after_secs } => {
                    assert!(retry_after_secs > 0);
                }
                _ => panic!("the fifth wrong code must lock pairing"),
            }
        }
    }

    let locked = store
        .pair(
            &code.code,
            &origin,
            "Chrome extension",
            NOW_MS + MAX_FAILED_ATTEMPTS as i64 * 1_000 + 1_000,
        )
        .expect("correct code during lockout");
    match locked {
        PairResult::Locked { retry_after_secs } => assert!(retry_after_secs > 0),
        _ => panic!("the correct code must stay locked during the lockout window"),
    }

    let unlock_at = NOW_MS + MAX_FAILED_ATTEMPTS as i64 * 1_000 + LOCKOUT_MS;
    let unlocked = store
        .pair(&code.code, &origin, "Chrome extension", unlock_at)
        .expect("correct code at the lockout boundary");
    assert!(matches!(unlocked, PairResult::Paired { .. }));
}

#[test]
fn revoking_by_client_id_or_token_removes_access_and_the_listed_client() {
    let (_dir, mut store) = open_store();
    let origin = extension_origin('a');

    let (first_token, first_client) = pair_once(&mut store, NOW_MS, &origin, "Chrome extension");
    assert_eq!(store.list_clients().expect("list clients").len(), 1);
    assert!(store
        .authorize(&first_token, &origin, NOW_MS + 1_000)
        .expect("authorize")
        .is_some());
    assert!(store.revoke(&first_client.id).expect("revoke by id"));
    assert!(store
        .authorize(&first_token, &origin, NOW_MS + 2_000)
        .expect("authorize")
        .is_none());
    assert!(store.list_clients().expect("list clients").is_empty());

    let (second_token, _second_client) =
        pair_once(&mut store, NOW_MS + 3_000, &origin, "Chrome extension");
    assert!(store.revoke_token(&second_token).expect("revoke by token"));
    assert!(store
        .authorize(&second_token, &origin, NOW_MS + 4_000)
        .expect("authorize")
        .is_none());
    assert!(store.list_clients().expect("list clients").is_empty());
}

#[test]
fn opening_a_pairing_database_from_a_newer_schema_is_refused() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("pairing.db");
    {
        let connection = Connection::open(&path).expect("create database");
        connection
            .pragma_update(None, "user_version", 2)
            .expect("set future schema version");
    }

    let error = match PairingStore::open(&path) {
        Ok(_) => panic!("a newer pairing schema must be refused"),
        Err(error) => error,
    };
    assert_eq!(error.status, 503);
    assert_eq!(error.code, "PAIRING_SCHEMA_UNSUPPORTED");
}

#[test]
fn authorize_updates_last_used_time_at_most_once_per_minute() {
    let (_dir, mut store) = open_store();
    let origin = extension_origin('a');
    assert_eq!(LAST_USED_UPDATE_INTERVAL_MS, 60_000);

    let (token, paired) = pair_once(&mut store, NOW_MS, &origin, "Chrome extension");
    let initial_last_used = paired.last_used_at_ms;

    let soon = store
        .authorize(&token, &origin, NOW_MS + 10_000)
        .expect("authorize at ten seconds")
        .expect("active client");
    assert_eq!(
        soon.last_used_at_ms, initial_last_used,
        "a recent authorization must not rewrite last_used_at_ms"
    );

    let later = store
        .authorize(&token, &origin, NOW_MS + 61_000)
        .expect("authorize after sixty seconds")
        .expect("active client");
    assert_eq!(later.last_used_at_ms, NOW_MS + 61_000);

    let still_throttled = store
        .authorize(&token, &origin, NOW_MS + 70_000)
        .expect("authorize again")
        .expect("active client");
    assert_eq!(still_throttled.last_used_at_ms, NOW_MS + 61_000);

    let listed = store.list_clients().expect("list clients");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].last_used_at_ms, NOW_MS + 61_000);
}

#[test]
fn tokens_authorize_only_for_the_exact_paired_extension_origin() {
    let (_dir, mut store) = open_store();
    let paired_origin = extension_origin('a');
    let other_origin = extension_origin('b');

    let (token, _client) = pair_once(&mut store, NOW_MS, &paired_origin, "Chrome extension");
    assert!(store
        .authorize(&token, &paired_origin, NOW_MS + 1_000)
        .expect("authorize with the paired origin")
        .is_some());
    assert!(store
        .authorize(&token, &other_origin, NOW_MS + 2_000)
        .expect("authorize with another extension origin")
        .is_none());
    assert!(store
        .authorize(&token, "https://example.com", NOW_MS + 3_000)
        .expect("authorize with a web origin")
        .is_none());
}

#[test]
fn rate_limiter_allows_exactly_capacity_per_window_and_isolates_keys() {
    let mut limiter = RateLimiter::new(RateLimitPolicy::new(3, 1_000), 8);

    for _ in 0..3 {
        assert!(limiter.check("alpha", 0).allowed);
    }
    let denied = limiter.check("alpha", 0);
    assert!(!denied.allowed);
    assert_eq!(denied.retry_after_secs, 1);

    assert!(limiter.check("beta", 0).allowed, "the limit is per key");

    assert!(
        limiter.check("alpha", 1_001).allowed,
        "capacity returns after the window"
    );
    assert!(limiter.check("alpha", 1_001).allowed);
    assert!(limiter.check("alpha", 1_001).allowed);
    let denied_again = limiter.check("alpha", 1_001);
    assert!(!denied_again.allowed);
    assert!(denied_again.retry_after_secs >= 1);
}

#[test]
fn rate_limiter_never_tracks_more_keys_than_configured() {
    let mut limiter = RateLimiter::new(RateLimitPolicy::new(2, 1_000), 2);
    for index in 0..20_i64 {
        let key = format!("client-{index}");
        assert!(limiter.check(&key, 5_000 + index).allowed);
    }
    assert!(limiter.tracked_keys() <= 2);
}

#[test]
fn capture_validation_rejects_protocol_version_and_request_level_limits() {
    let takeover = request_error(&capture_request(
        CaptureIntent::Takeover,
        vec![valid_item(), valid_item()],
    ));
    assert_eq!(takeover.code, "TAKEOVER_ITEM_COUNT");
    assert_eq!(takeover.status, 400);

    let many_items: Vec<CaptureItem> = (0..=MAX_CAPTURE_ITEMS)
        .map(|index| {
            let mut item = valid_item();
            item.url = format!("https://example.com/{index}.bin");
            item
        })
        .collect();
    assert_eq!(many_items.len(), MAX_CAPTURE_ITEMS + 1);
    let too_many = request_error(&capture_request(CaptureIntent::Prefill, many_items));
    assert_eq!(too_many.code, "TOO_MANY_ITEMS");
    assert_eq!(too_many.status, 400);

    let mut future = capture_request(CaptureIntent::Prefill, vec![valid_item()]);
    future.protocol_version = API_PROTOCOL_VERSION + 1;
    let mismatch = request_error(&future);
    assert_eq!(mismatch.code, "PROTOCOL_MISMATCH");
    assert_eq!(mismatch.status, 409);

    let empty = request_error(&capture_request(CaptureIntent::Prefill, Vec::new()));
    assert_eq!(empty.code, "EMPTY_CAPTURE");
    assert_eq!(empty.status, 400);
}

#[test]
fn capture_validation_rejects_unsupported_kinds_and_bad_referers() {
    let mut unsupported_kind = valid_item();
    unsupported_kind.kind = Some(CaptureKind::Hls);
    let mut invalid_referer = valid_item();
    invalid_referer.referer = Some("not-a-url".to_string());
    let mut accepted = valid_item();
    accepted.url = "https://example.com/accepted.bin".to_string();

    let request = capture_request(
        CaptureIntent::Prefill,
        vec![unsupported_kind, invalid_referer, accepted],
    );
    let validation =
        validate_capture_request(&request).expect("per-item problems are not request-level errors");
    assert_eq!(validation.accepted.len(), 1);
    assert_eq!(
        validation.accepted[0].url,
        "https://example.com/accepted.bin"
    );
    assert_eq!(validation.rejected.len(), 2);
    assert_eq!(rejected_code(&validation, 0), Some("UNSUPPORTED_KIND"));
    assert_eq!(rejected_code(&validation, 1), Some("INVALID_REFERER"));
}

#[test]
fn capture_validation_rejects_invalid_oversized_and_credentialed_urls() {
    let mut invalid = valid_item();
    invalid.url = "not a url".to_string();
    let mut non_http = valid_item();
    non_http.url = "ftp://example.com/file.bin".to_string();
    let mut credentialed = valid_item();
    credentialed.url = "https://user@example.com/file.bin".to_string();
    let mut oversized = valid_item();
    oversized.url = format!("https://example.com/{}", "a".repeat(MAX_URL_BYTES));
    let mut boundary = valid_item();
    boundary.url = format!(
        "https://example.com/{}",
        "a".repeat(MAX_URL_BYTES - "https://example.com/".len())
    );

    let request = capture_request(
        CaptureIntent::Prefill,
        vec![invalid, non_http, credentialed, oversized, boundary],
    );
    let validation =
        validate_capture_request(&request).expect("per-item problems are not request-level errors");
    assert_eq!(validation.accepted.len(), 1);
    assert_eq!(rejected_code(&validation, 0), Some("INVALID_URL"));
    assert_eq!(rejected_code(&validation, 1), Some("INVALID_URL"));
    assert_eq!(rejected_code(&validation, 2), Some("CREDENTIALED_URL"));
    assert_eq!(rejected_code(&validation, 3), Some("INVALID_URL"));
    assert!(validation.rejected.iter().all(|entry| entry.index != 4));
}

#[test]
fn capture_validation_rejects_forbidden_duplicate_and_oversized_headers() {
    let mut forbidden_cookie = valid_item();
    forbidden_cookie.headers = vec![CaptureHeader {
        name: "Cookie".to_string(),
        value: "session=secret".to_string(),
    }];
    let mut forbidden_unknown = valid_item();
    forbidden_unknown.headers = vec![CaptureHeader {
        name: "X-Custom".to_string(),
        value: "value".to_string(),
    }];
    let mut duplicate = valid_item();
    duplicate.headers = vec![
        CaptureHeader {
            name: "Accept".to_string(),
            value: "text/html".to_string(),
        },
        CaptureHeader {
            name: "accept".to_string(),
            value: "application/json".to_string(),
        },
    ];
    let mut crlf = valid_item();
    crlf.headers = vec![CaptureHeader {
        name: "Accept".to_string(),
        value: "text/html\r\nX-Evil: 1".to_string(),
    }];
    let mut too_many = valid_item();
    too_many.headers = (0..=MAX_CAPTURE_HEADERS)
        .map(|_| CaptureHeader {
            name: "Accept".to_string(),
            value: "*/*".to_string(),
        })
        .collect();
    let mut oversized_value = valid_item();
    oversized_value.headers = vec![CaptureHeader {
        name: "Accept".to_string(),
        value: "v".repeat(MAX_HEADER_VALUE_BYTES + 1),
    }];

    let request = capture_request(
        CaptureIntent::Prefill,
        vec![
            forbidden_cookie,
            forbidden_unknown,
            duplicate,
            crlf,
            too_many,
            oversized_value,
        ],
    );
    let validation =
        validate_capture_request(&request).expect("per-item problems are not request-level errors");
    assert!(validation.accepted.is_empty());
    assert_eq!(validation.rejected.len(), 6);
    assert_eq!(rejected_code(&validation, 0), Some("FORBIDDEN_HEADER"));
    assert_eq!(rejected_code(&validation, 1), Some("FORBIDDEN_HEADER"));
    assert_eq!(rejected_code(&validation, 2), Some("DUPLICATE_HEADER"));
    assert_eq!(rejected_code(&validation, 3), Some("FORBIDDEN_HEADER"));
    assert_eq!(rejected_code(&validation, 4), Some("HEADER_LIMIT"));
    assert_eq!(rejected_code(&validation, 5), Some("HEADER_LIMIT"));
}

#[test]
fn capture_validation_rejects_unsafe_filenames_and_accepts_the_boundary() {
    let unsafe_names = vec![
        String::new(),
        ".".to_string(),
        "..".to_string(),
        "nested/file.bin".to_string(),
        "nested\\file.bin".to_string(),
        "line\nbreak.bin".to_string(),
        "nul\0byte.bin".to_string(),
        "x".repeat(MAX_FILENAME_BYTES + 1),
    ];
    let mut items = Vec::new();
    for name in &unsafe_names {
        let mut item = valid_item();
        item.filename = Some(name.clone());
        items.push(item);
    }

    let request = capture_request(CaptureIntent::Prefill, items);
    let validation =
        validate_capture_request(&request).expect("per-item problems are not request-level errors");
    assert!(validation.accepted.is_empty());
    assert_eq!(validation.rejected.len(), unsafe_names.len());
    for index in 0..unsafe_names.len() {
        assert_eq!(
            rejected_code(&validation, index),
            Some("UNSAFE_FILENAME"),
            "filename at index {index} must be rejected"
        );
    }

    let mut boundary = valid_item();
    boundary.filename = Some("x".repeat(MAX_FILENAME_BYTES));
    let accepted =
        validate_capture_request(&capture_request(CaptureIntent::Prefill, vec![boundary]))
            .expect("valid request");
    assert_eq!(accepted.accepted.len(), 1);
    assert!(accepted.rejected.is_empty());
}

#[test]
fn capture_protocol_serde_uses_kebab_case_enums_and_camel_case_fields() {
    assert_eq!(
        serde_json::to_value(CaptureIntent::Takeover).expect("serialize intent"),
        serde_json::json!("takeover")
    );
    assert_eq!(
        serde_json::to_value(CaptureIntent::Prefill).expect("serialize intent"),
        serde_json::json!("prefill")
    );
    assert_eq!(
        serde_json::to_value(CaptureKind::File).expect("serialize kind"),
        serde_json::json!("file")
    );
    assert_eq!(
        serde_json::to_value(CaptureKind::Hls).expect("serialize kind"),
        serde_json::json!("hls")
    );
    assert_eq!(
        serde_json::to_value(CaptureKind::Dash).expect("serialize kind"),
        serde_json::json!("dash")
    );
    assert_eq!(
        serde_json::to_value(CaptureKind::VideoPlatform).expect("serialize kind"),
        serde_json::json!("video-platform")
    );

    let request = CaptureRequest {
        protocol_version: API_PROTOCOL_VERSION,
        intent: CaptureIntent::Prefill,
        source_id: Some("source-1".to_string()),
        items: vec![CaptureItem {
            url: "https://example.com/file.bin".to_string(),
            filename: Some("file.bin".to_string()),
            referer: Some("https://example.com/page".to_string()),
            mime: Some("application/octet-stream".to_string()),
            size: Some(7),
            kind: Some(CaptureKind::VideoPlatform),
            headers: vec![CaptureHeader {
                name: "Accept".to_string(),
                value: "*/*".to_string(),
            }],
        }],
    };
    let value = serde_json::to_value(&request).expect("serialize capture request");
    assert_eq!(value["protocolVersion"], 1);
    assert_eq!(value["intent"], "prefill");
    assert_eq!(value["sourceId"], "source-1");
    let item = &value["items"][0];
    assert_eq!(item["url"], "https://example.com/file.bin");
    assert_eq!(item["filename"], "file.bin");
    assert_eq!(item["referer"], "https://example.com/page");
    assert_eq!(item["mime"], "application/octet-stream");
    assert_eq!(item["size"], 7);
    assert_eq!(item["kind"], "video-platform");
    assert_eq!(item["headers"][0]["name"], "Accept");
    assert_eq!(item["headers"][0]["value"], "*/*");

    let decoded: CaptureRequest =
        serde_json::from_value(value).expect("deserialize capture request");
    assert_eq!(decoded.protocol_version, API_PROTOCOL_VERSION);
    assert!(matches!(decoded.intent, CaptureIntent::Prefill));
    assert_eq!(decoded.source_id.as_deref(), Some("source-1"));
    assert_eq!(decoded.items.len(), 1);
    assert_eq!(decoded.items[0].url, "https://example.com/file.bin");
    assert_eq!(decoded.items[0].filename.as_deref(), Some("file.bin"));
    assert_eq!(decoded.items[0].size, Some(7));
    assert!(matches!(
        decoded.items[0].kind,
        Some(CaptureKind::VideoPlatform)
    ));

    let minimal: CaptureRequest = serde_json::from_value(serde_json::json!({
        "protocolVersion": 1,
        "intent": "takeover",
        "items": [{
            "url": "https://example.com/file.bin",
            "headers": []
        }]
    }))
    .expect("deserialize minimal capture request");
    assert_eq!(minimal.source_id, None);
    assert!(matches!(minimal.intent, CaptureIntent::Takeover));
    assert_eq!(minimal.items[0].filename, None);
    assert_eq!(minimal.items[0].referer, None);
    assert_eq!(minimal.items[0].mime, None);
    assert_eq!(minimal.items[0].size, None);
    assert!(minimal.items[0].kind.is_none());
    assert!(minimal.items[0].headers.is_empty());
}

#[test]
fn protocol_constants_match_the_capture_and_admission_contract() {
    assert_eq!(API_PROTOCOL_VERSION, 1);
    assert_eq!(MAX_BODY_BYTES, 65_536);
    assert_eq!(MAX_CAPTURE_ITEMS, 50);
    assert_eq!(MAX_URL_BYTES, 8_192);
    assert_eq!(MAX_CAPTURE_HEADERS, 10);
    assert_eq!(MAX_HEADER_NAME_BYTES, 128);
    assert_eq!(MAX_HEADER_VALUE_BYTES, 2_048);
    assert_eq!(MAX_FILENAME_BYTES, 255);
    assert_eq!(
        SAFE_CAPTURE_HEADERS,
        ["referer", "user-agent", "accept", "accept-language"]
    );
    assert_eq!(API_HOST, "127.0.0.1");
    assert_eq!(API_PORT, 37_652);
    assert_eq!(loopback_authority(API_PORT), "127.0.0.1:37652");
    assert_eq!(loopback_authority(80), "127.0.0.1:80");
}

#[test]
fn chromium_extension_origin_requires_an_exact_thirty_two_character_id() {
    let accepted = extension_origin('a');
    assert_eq!(accepted.len(), "chrome-extension://".len() + 32);
    assert!(is_chromium_extension_origin(&accepted));
    assert!(is_chromium_extension_origin(&extension_origin('p')));

    assert!(!is_chromium_extension_origin("https://example.com"));
    assert!(!is_chromium_extension_origin(&extension_origin('q')));
    assert!(!is_chromium_extension_origin(&format!(
        "chrome-extension://{}",
        "A".repeat(32)
    )));
    assert!(!is_chromium_extension_origin(&format!(
        "chrome-extension://{}",
        "a".repeat(31)
    )));
    assert!(!is_chromium_extension_origin(&format!(
        "chrome-extension://{}",
        "a".repeat(33)
    )));
    assert!(!is_chromium_extension_origin(&format!(
        "{accepted}/popup.html"
    )));
    assert!(!is_chromium_extension_origin("chrome-extension://"));
}

#[test]
fn token_generation_and_hashing_are_256_bit_lowercase_hex() {
    assert_eq!(TOKEN_BYTES, 32);
    let token = generate_token();
    assert_eq!(token.len(), 64);
    assert!(token
        .chars()
        .all(|character| character.is_ascii_hexdigit() && !character.is_ascii_uppercase()));
    let other = generate_token();
    assert_ne!(token, other);

    let hash = hash_secret(&token);
    assert_eq!(hash.len(), 64);
    assert!(hash
        .chars()
        .all(|character| character.is_ascii_hexdigit() && !character.is_ascii_uppercase()));
    assert_ne!(hash, token);
    assert_eq!(hash_secret(&token), hash);
    assert_ne!(hash_secret(&other), hash);
    assert!(verify_secret_hash(&token, &hash));
    assert!(!verify_secret_hash(&other, &hash));
}

#[test]
fn system_clock_reports_epoch_milliseconds_and_is_default_constructible() {
    let before = epoch_ms();
    let clock: Box<dyn Clock> = Box::new(SystemClock::default());
    let reading = clock.now_ms();
    let after = epoch_ms();
    assert!(reading >= before && reading <= after);
}

#[test]
fn api_errors_expose_status_codes_and_standard_error_behavior() {
    let error = ApiError::new(400, "BAD_REQUEST", "bad input");
    assert_eq!(error.status, 400);
    assert_eq!(error.code, "BAD_REQUEST");
    assert_eq!(error.message, "bad input");
    assert!(!format!("{error}").is_empty());
    assert_eq!(error.clone(), error);
    let dynamic: &dyn std::error::Error = &error;
    assert!(!dynamic.to_string().is_empty());

    assert_eq!(error::bad_request("BAD_REQUEST", "bad input").status, 400);
    assert_eq!(error::unauthorized("UNAUTHORIZED").status, 401);
    assert_eq!(error::forbidden("FORBIDDEN").status, 403);
    assert_eq!(error::unsupported_media_type().status, 415);
    assert_eq!(error::payload_too_large().status, 413);
    assert_eq!(error::conflict("CONFLICT").status, 409);
    assert_eq!(error::too_many_requests("RATE_LIMITED").status, 429);
    assert_eq!(error::service_unavailable("UNAVAILABLE").status, 503);
}
