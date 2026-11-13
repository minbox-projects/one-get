//! Behavior tests for the loopback capture API: admission, pairing, capture
//! intents, takeover/prefill delivery, revocation, rate limits, log redaction,
//! and real-socket startup/shutdown.
//!
//! The tests use only the agreed public interface:
//! - `oneget_lib::api::server::{ApiServerConfig, ApiState, build_router,
//!   ApiServerHandle}`
//! - `oneget_lib::api::desktop::{DesktopBridge, PendingPrefill,
//!   CAPTURE_PREFILL_EVENT}`
//! - `oneget_lib::api::pairing_store::PairingStore`,
//!   `oneget_lib::api::protocol` capture types, and
//!   `oneget_lib::api::rate_limit::RateLimitPolicy`.
//!
//! Expected RED at authoring time: `api::server` and `api::desktop` do not
//! exist yet, so this target fails to compile. GREEN is left to the
//! implementation step.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{header, Method, Request, Response};
use axum::Router;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tempfile::{tempdir, TempDir};
use tower::ServiceExt;

use oneget_lib::api::clock::Clock;
use oneget_lib::api::desktop::{DesktopBridge, PendingPrefill, CAPTURE_PREFILL_EVENT};
use oneget_lib::api::error::ApiError;
use oneget_lib::api::pairing_store::{PairResult, PairingStore};
use oneget_lib::api::protocol::{
    CaptureIntent, CaptureItem, CaptureKind, CaptureRequest, API_PROTOCOL_VERSION,
};
use oneget_lib::api::rate_limit::RateLimitPolicy;
use oneget_lib::api::server::{build_router, ApiServerConfig, ApiServerHandle, ApiState};

const PORT: u16 = 45_678;
const NOW_MS: i64 = 1_800_000_000_000;
const TEST_APP_VERSION: &str = "1.2.3";

fn host_value() -> String {
    format!("127.0.0.1:{PORT}")
}

fn extension_origin(id_char: char) -> String {
    format!("chrome-extension://{}", id_char.to_string().repeat(32))
}

#[derive(Debug)]
struct FakeClock {
    now_ms: AtomicI64,
}

impl FakeClock {
    fn new(now_ms: i64) -> Arc<Self> {
        Arc::new(Self {
            now_ms: AtomicI64::new(now_ms),
        })
    }
}

impl Clock for FakeClock {
    fn now_ms(&self) -> i64 {
        self.now_ms.load(Ordering::SeqCst)
    }
}

#[derive(Default, Clone)]
struct BridgeRecords {
    takeovers: Vec<(CaptureItem, i64)>,
    prefills: Vec<PendingPrefill>,
    focus_calls: usize,
    events: Vec<(String, Value)>,
}

struct FakeBridge {
    records: Mutex<BridgeRecords>,
    takeover_error: Mutex<Option<ApiError>>,
    prefill_error: Mutex<Option<ApiError>>,
    created_tasks: AtomicUsize,
}

impl FakeBridge {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            records: Mutex::new(BridgeRecords::default()),
            takeover_error: Mutex::new(None),
            prefill_error: Mutex::new(None),
            created_tasks: AtomicUsize::new(0),
        })
    }

    fn records(&self) -> BridgeRecords {
        self.records.lock().expect("bridge records lock").clone()
    }

    fn fail_takeover(&self, error: ApiError) {
        *self.takeover_error.lock().expect("takeover error lock") = Some(error);
    }

    fn fail_prefill(&self, error: ApiError) {
        *self.prefill_error.lock().expect("prefill error lock") = Some(error);
    }
}

impl DesktopBridge for FakeBridge {
    fn create_takeover_task(&self, item: &CaptureItem, now_ms: i64) -> Result<String, ApiError> {
        self.records
            .lock()
            .expect("bridge records lock")
            .takeovers
            .push((item.clone(), now_ms));
        if let Some(error) = self
            .takeover_error
            .lock()
            .expect("takeover error lock")
            .clone()
        {
            return Err(error);
        }
        let index = self.created_tasks.fetch_add(1, Ordering::SeqCst) + 1;
        Ok(format!("task-{index}"))
    }

    fn queue_prefill(&self, prefill: PendingPrefill) -> Result<(), ApiError> {
        self.records
            .lock()
            .expect("bridge records lock")
            .prefills
            .push(prefill);
        if let Some(error) = self
            .prefill_error
            .lock()
            .expect("prefill error lock")
            .clone()
        {
            return Err(error);
        }
        Ok(())
    }

    fn focus_main_window(&self) {
        self.records
            .lock()
            .expect("bridge records lock")
            .focus_calls += 1;
    }

    fn log_event(&self, event: &str, fields: Value) {
        self.records
            .lock()
            .expect("bridge records lock")
            .events
            .push((event.to_string(), fields));
    }
}

struct Harness {
    _temp: TempDir,
    store: Arc<Mutex<PairingStore>>,
    bridge: Arc<FakeBridge>,
    clock: Arc<FakeClock>,
    router: Router,
}

impl Harness {
    fn new(pair_capacity: usize, token_capacity: usize) -> Self {
        let temp = tempdir().expect("tempdir");
        let store = Arc::new(Mutex::new(
            PairingStore::open(temp.path().join("pairing.db")).expect("pairing store"),
        ));
        let bridge = FakeBridge::new();
        let clock = FakeClock::new(NOW_MS);
        let router = build_router(ApiState::new(server_config(
            PORT,
            Arc::clone(&store),
            Arc::clone(&bridge),
            Arc::clone(&clock),
            pair_capacity,
            token_capacity,
        )));
        Self {
            _temp: temp,
            store,
            bridge,
            clock,
            router,
        }
    }

    fn generate_code(&self) -> String {
        self.store
            .lock()
            .expect("pairing store lock")
            .generate_code(self.clock.now_ms())
            .expect("generate pairing code")
            .code
    }

    fn issue_token(&self, origin: &str, label: &str) -> String {
        let mut store = self.store.lock().expect("pairing store lock");
        let code = store
            .generate_code(self.clock.now_ms())
            .expect("generate pairing code");
        match store
            .pair(&code.code, origin, label, self.clock.now_ms())
            .expect("pairing attempt")
        {
            PairResult::Paired { token, .. } => token,
            _ => panic!("expected a paired result"),
        }
    }
}

fn server_config(
    port: u16,
    store: Arc<Mutex<PairingStore>>,
    bridge: Arc<FakeBridge>,
    clock: Arc<FakeClock>,
    pair_capacity: usize,
    token_capacity: usize,
) -> ApiServerConfig {
    ApiServerConfig {
        port,
        app_version: TEST_APP_VERSION.to_string(),
        pairing_store: store,
        bridge: bridge as Arc<dyn DesktopBridge>,
        clock: clock as Arc<dyn Clock>,
        pair_origin_policy: RateLimitPolicy::new(pair_capacity, 60_000),
        token_policy: RateLimitPolicy::new(token_capacity, 60_000),
    }
}

fn health_request(host: &str, origin: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder()
        .method(Method::GET)
        .uri("/health")
        .header(header::HOST, host);
    if let Some(origin) = origin {
        builder = builder.header(header::ORIGIN, origin);
    }
    builder.body(Body::empty()).expect("health request")
}

fn write_request(
    path: &str,
    origin: &str,
    token: Option<&str>,
    content_type: Option<&str>,
    body: impl Into<Body>,
) -> Request<Body> {
    let mut builder = Request::builder()
        .method(Method::POST)
        .uri(path)
        .header(header::HOST, host_value())
        .header(header::ORIGIN, origin);
    if let Some(content_type) = content_type {
        builder = builder.header(header::CONTENT_TYPE, content_type);
    }
    if let Some(token) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    builder.body(body.into()).expect("write request")
}

async fn send(router: &Router, request: Request<Body>) -> Response<Body> {
    router
        .clone()
        .oneshot(request)
        .await
        .expect("router must answer")
}

async fn response_json(response: Response<Body>) -> Value {
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("collect response body")
        .to_bytes();
    serde_json::from_slice(&bytes).unwrap_or_else(|_| {
        panic!(
            "expected a JSON response body, got: {}",
            String::from_utf8_lossy(&bytes)
        )
    })
}

fn retry_after(response: &Response<Body>) -> Option<u64> {
    response
        .headers()
        .get(header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse().ok())
}

fn pairing_body(code: &str, protocol_version: u32) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "protocolVersion": protocol_version,
        "code": code,
        "label": "Chrome extension",
    }))
    .expect("pairing body")
}

fn file_item(url: &str, filename: &str) -> CaptureItem {
    CaptureItem {
        url: url.to_string(),
        filename: Some(filename.to_string()),
        referer: Some("https://page.example/".to_string()),
        mime: Some("application/octet-stream".to_string()),
        size: Some(1_024),
        kind: Some(CaptureKind::File),
        headers: Vec::new(),
    }
}

fn capture_body(
    intent: CaptureIntent,
    source_id: Option<&str>,
    items: Vec<CaptureItem>,
) -> Vec<u8> {
    serde_json::to_vec(&CaptureRequest {
        protocol_version: API_PROTOCOL_VERSION,
        intent,
        source_id: source_id.map(str::to_string),
        items,
    })
    .expect("capture body")
}

fn contains_secret(haystack: &str, needle: &str) -> bool {
    !needle.is_empty() && haystack.contains(needle)
}

// --- Health and host/origin admission ---

#[tokio::test]
async fn health_reports_capabilities_with_an_absent_origin() {
    let harness = Harness::new(10, 10);

    let response = send(&harness.router, health_request(&host_value(), None)).await;
    assert_eq!(response.status().as_u16(), 200);
    let body = response_json(response).await;

    assert_eq!(body["ok"], json!(true));
    assert_eq!(body["appVersion"], json!(TEST_APP_VERSION));
    assert_eq!(body["protocolVersion"], json!(1));
    let capabilities = body["capabilities"].as_array().expect("capabilities array");
    for expected in ["capture", "prefill", "takeover"] {
        assert!(
            capabilities.iter().any(|value| value == expected),
            "health must advertise {expected}"
        );
    }
    for forbidden in ["token", "client", "url", "path"] {
        assert!(
            body.get(forbidden).is_none(),
            "health must not expose {forbidden}"
        );
    }
}

#[tokio::test]
async fn health_rejects_a_present_non_extension_origin() {
    let harness = Harness::new(10, 10);

    for origin in ["https://example.com", "null", "chrome-extension://zzzz"] {
        let response = send(&harness.router, health_request(&host_value(), Some(origin))).await;
        assert_eq!(response.status().as_u16(), 403, "origin {origin}");
        let body = response_json(response).await;
        assert_eq!(body["code"], json!("INVALID_ORIGIN"));
        assert!(body["message"].as_str().is_some());
    }
}

#[tokio::test]
async fn health_rejects_wrong_missing_or_localhost_host() {
    let harness = Harness::new(10, 10);

    for host in [
        "evil.test".to_string(),
        format!("localhost:{PORT}"),
        format!("127.0.0.1:{}", PORT + 1),
    ] {
        let response = send(&harness.router, health_request(&host, None)).await;
        assert_eq!(response.status().as_u16(), 403, "host {host}");
        assert_eq!(response_json(response).await["code"], json!("INVALID_HOST"));
    }

    let missing = Request::builder()
        .method(Method::GET)
        .uri("/health")
        .body(Body::empty())
        .expect("request without host");
    let response = send(&harness.router, missing).await;
    assert_eq!(response.status().as_u16(), 403);
    assert_eq!(response_json(response).await["code"], json!("INVALID_HOST"));
}

#[tokio::test]
async fn admission_guards_reject_header_count_header_size_and_body_size() {
    let harness = Harness::new(10, 10);

    let mut too_many = Request::builder()
        .method(Method::GET)
        .uri("/health")
        .header(header::HOST, host_value());
    for index in 0..65 {
        too_many = too_many.header(format!("x-pad-{index}"), "1");
    }
    let response = send(
        &harness.router,
        too_many.body(Body::empty()).expect("many headers"),
    )
    .await;
    assert_eq!(response.status().as_u16(), 400);
    assert_eq!(response_json(response).await["code"], json!("HEADER_LIMIT"));

    let mut too_large = Request::builder()
        .method(Method::GET)
        .uri("/health")
        .header(header::HOST, host_value());
    for index in 0..17 {
        too_large = too_large.header(format!("x-large-{index}"), "a".repeat(1_000));
    }
    let response = send(
        &harness.router,
        too_large.body(Body::empty()).expect("large headers"),
    )
    .await;
    assert_eq!(response.status().as_u16(), 400);
    assert_eq!(response_json(response).await["code"], json!("HEADER_LIMIT"));

    let origin = extension_origin('a');
    let token = harness.issue_token(&origin, "Chrome extension");
    let oversized = write_request(
        "/capture",
        &origin,
        Some(&token),
        Some("application/json"),
        vec![b'a'; 70_000],
    );
    let response = send(&harness.router, oversized).await;
    assert_eq!(response.status().as_u16(), 413);
    assert_eq!(
        response_json(response).await["code"],
        json!("PAYLOAD_TOO_LARGE")
    );
    assert!(harness.bridge.records().takeovers.is_empty());
}

// --- Pairing ---

#[tokio::test]
async fn pairing_rejects_web_origins_and_non_json_content_types() {
    let harness = Harness::new(10, 10);

    let web_origin = write_request(
        "/pair",
        "https://example.com",
        None,
        Some("application/json"),
        pairing_body("12345678", API_PROTOCOL_VERSION),
    );
    let response = send(&harness.router, web_origin).await;
    assert_eq!(response.status().as_u16(), 403);
    assert_eq!(
        response_json(response).await["code"],
        json!("INVALID_ORIGIN")
    );

    let extension = extension_origin('a');
    let missing_content_type = write_request(
        "/pair",
        &extension,
        None,
        None,
        pairing_body("12345678", API_PROTOCOL_VERSION),
    );
    let response = send(&harness.router, missing_content_type).await;
    assert_eq!(response.status().as_u16(), 415);
    assert_eq!(
        response_json(response).await["code"],
        json!("UNSUPPORTED_MEDIA_TYPE")
    );
}

#[tokio::test]
async fn pairing_issues_one_hex_token_and_rejects_reuse_of_the_code() {
    let harness = Harness::new(10, 10);
    let origin = extension_origin('a');
    let code = harness.generate_code();

    let response = send(
        &harness.router,
        write_request(
            "/pair",
            &origin,
            None,
            Some("application/json; charset=utf-8"),
            pairing_body(&code, API_PROTOCOL_VERSION),
        ),
    )
    .await;
    assert_eq!(response.status().as_u16(), 200);
    let body = response_json(response).await;

    let token = body["token"].as_str().expect("token string");
    assert_eq!(token.len(), 64);
    assert!(
        token
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "token must be lowercase hexadecimal"
    );
    let client = &body["client"];
    assert_eq!(client["origin"], json!(origin));
    assert_eq!(client["label"], json!("Chrome extension"));
    assert!(client["id"].as_str().is_some_and(|id| !id.is_empty()));
    assert_eq!(client["createdAtMs"], json!(NOW_MS));
    assert_eq!(client["lastUsedAtMs"], json!(NOW_MS));

    let reused = send(
        &harness.router,
        write_request(
            "/pair",
            &origin,
            None,
            Some("application/json"),
            pairing_body(&code, API_PROTOCOL_VERSION),
        ),
    )
    .await;
    assert_eq!(reused.status().as_u16(), 401);
    assert_eq!(response_json(reused).await["code"], json!("INVALID_CODE"));
}

#[tokio::test]
async fn pairing_protocol_mismatch_is_409_and_does_not_consume_the_code() {
    let harness = Harness::new(10, 10);
    let origin = extension_origin('a');
    let code = harness.generate_code();

    let mismatch = send(
        &harness.router,
        write_request(
            "/pair",
            &origin,
            None,
            Some("application/json"),
            pairing_body(&code, API_PROTOCOL_VERSION + 1),
        ),
    )
    .await;
    assert_eq!(mismatch.status().as_u16(), 409);
    assert_eq!(
        response_json(mismatch).await["code"],
        json!("PROTOCOL_MISMATCH")
    );

    let accepted = send(
        &harness.router,
        write_request(
            "/pair",
            &origin,
            None,
            Some("application/json"),
            pairing_body(&code, API_PROTOCOL_VERSION),
        ),
    )
    .await;
    assert_eq!(accepted.status().as_u16(), 200);
}

#[tokio::test]
async fn pairing_locks_after_five_wrong_codes_and_advertises_retry_after() {
    let harness = Harness::new(100, 10);
    let origin = extension_origin('a');
    let code = harness.generate_code();
    let wrong = if let Some(rest) = code.strip_prefix('0') {
        format!("1{rest}")
    } else {
        format!("0{}", &code[1..])
    };
    assert_ne!(wrong, code);

    for attempt in 1..=5 {
        let response = send(
            &harness.router,
            write_request(
                "/pair",
                &origin,
                None,
                Some("application/json"),
                pairing_body(&wrong, API_PROTOCOL_VERSION),
            ),
        )
        .await;
        let status = response.status().as_u16();
        let body = response_json(response).await;
        if attempt < 5 {
            assert_eq!(status, 401, "attempt {attempt}");
            assert_eq!(body["code"], json!("INVALID_CODE"));
        } else {
            assert_eq!(status, 429, "fifth failure locks pairing");
            assert_eq!(body["code"], json!("PAIRING_LOCKED"));
        }
    }

    let locked = send(
        &harness.router,
        write_request(
            "/pair",
            &origin,
            None,
            Some("application/json"),
            pairing_body(&code, API_PROTOCOL_VERSION),
        ),
    )
    .await;
    assert_eq!(locked.status().as_u16(), 429);
    assert_eq!(response_json(locked).await["code"], json!("PAIRING_LOCKED"));
}

#[tokio::test]
async fn pairing_rate_limits_per_origin_with_retry_after() {
    let harness = Harness::new(2, 10);
    let origin = extension_origin('a');

    for _ in 0..2 {
        let response = send(
            &harness.router,
            write_request(
                "/pair",
                &origin,
                None,
                Some("application/json"),
                pairing_body("00000000", API_PROTOCOL_VERSION),
            ),
        )
        .await;
        assert_ne!(response.status().as_u16(), 429);
    }

    let limited = send(
        &harness.router,
        write_request(
            "/pair",
            &origin,
            None,
            Some("application/json"),
            pairing_body("00000000", API_PROTOCOL_VERSION),
        ),
    )
    .await;
    assert_eq!(limited.status().as_u16(), 429);
    assert!(
        retry_after(&limited).is_some_and(|seconds| seconds >= 1),
        "rate-limited responses carry Retry-After seconds"
    );
    assert_eq!(response_json(limited).await["code"], json!("RATE_LIMITED"));
}

// --- Capture admission ---

#[tokio::test]
async fn capture_rejects_missing_or_foreign_origin_and_bad_tokens() {
    let harness = Harness::new(10, 10);
    let origin = extension_origin('a');
    let token = harness.issue_token(&origin, "Chrome extension");
    let body = || {
        capture_body(
            CaptureIntent::Takeover,
            Some("dl-1"),
            vec![file_item("https://example.com/file.bin", "file.bin")],
        )
    };

    let website_origin = write_request(
        "/capture",
        "https://example.com",
        Some(&token),
        Some("application/json"),
        body(),
    );
    let response = send(&harness.router, website_origin).await;
    assert_eq!(response.status().as_u16(), 403);
    assert_eq!(
        response_json(response).await["code"],
        json!("INVALID_ORIGIN")
    );

    let missing_token = write_request("/capture", &origin, None, Some("application/json"), body());
    let response = send(&harness.router, missing_token).await;
    assert_eq!(response.status().as_u16(), 401);
    assert_eq!(response_json(response).await["code"], json!("UNAUTHORIZED"));

    let invalid_token = write_request(
        "/capture",
        &origin,
        Some("not-a-token"),
        Some("application/json"),
        body(),
    );
    let response = send(&harness.router, invalid_token).await;
    assert_eq!(response.status().as_u16(), 401);
    assert_eq!(response_json(response).await["code"], json!("UNAUTHORIZED"));

    let foreign_origin = extension_origin('b');
    let foreign = write_request(
        "/capture",
        &foreign_origin,
        Some(&token),
        Some("application/json"),
        body(),
    );
    let response = send(&harness.router, foreign).await;
    assert_eq!(response.status().as_u16(), 401);
    assert_eq!(response_json(response).await["code"], json!("UNAUTHORIZED"));

    let query_token = write_request(
        &format!("/capture?token={token}"),
        &origin,
        None,
        Some("application/json"),
        body(),
    );
    let response = send(&harness.router, query_token).await;
    assert_eq!(response.status().as_u16(), 401);
    assert_eq!(response_json(response).await["code"], json!("UNAUTHORIZED"));

    assert!(harness.bridge.records().takeovers.is_empty());
}

#[tokio::test]
async fn capture_rejects_non_json_content_type_and_invalid_request_shapes() {
    let harness = Harness::new(10, 10);
    let origin = extension_origin('a');
    let token = harness.issue_token(&origin, "Chrome extension");

    let wrong_type = write_request(
        "/capture",
        &origin,
        Some(&token),
        Some("text/plain"),
        capture_body(
            CaptureIntent::Prefill,
            None,
            vec![file_item("https://example.com/a.bin", "a.bin")],
        ),
    );
    let response = send(&harness.router, wrong_type).await;
    assert_eq!(response.status().as_u16(), 415);
    assert_eq!(
        response_json(response).await["code"],
        json!("UNSUPPORTED_MEDIA_TYPE")
    );

    let wrong_protocol = serde_json::to_vec(&json!({
        "protocolVersion": API_PROTOCOL_VERSION + 1,
        "intent": "prefill",
        "items": [file_item("https://example.com/a.bin", "a.bin")],
    }))
    .expect("protocol body");
    let response = send(
        &harness.router,
        write_request(
            "/capture",
            &origin,
            Some(&token),
            Some("application/json"),
            wrong_protocol,
        ),
    )
    .await;
    assert_eq!(response.status().as_u16(), 409);
    assert_eq!(
        response_json(response).await["code"],
        json!("PROTOCOL_MISMATCH")
    );

    let empty = send(
        &harness.router,
        write_request(
            "/capture",
            &origin,
            Some(&token),
            Some("application/json"),
            capture_body(CaptureIntent::Prefill, None, Vec::new()),
        ),
    )
    .await;
    assert_eq!(empty.status().as_u16(), 400);
    assert_eq!(response_json(empty).await["code"], json!("EMPTY_CAPTURE"));

    let too_many_items = (0..51)
        .map(|index| file_item(&format!("https://example.com/{index}.bin"), "item.bin"))
        .collect();
    let too_many = send(
        &harness.router,
        write_request(
            "/capture",
            &origin,
            Some(&token),
            Some("application/json"),
            capture_body(CaptureIntent::Prefill, None, too_many_items),
        ),
    )
    .await;
    assert_eq!(too_many.status().as_u16(), 400);
    assert_eq!(
        response_json(too_many).await["code"],
        json!("TOO_MANY_ITEMS")
    );

    let two_takeover_items = send(
        &harness.router,
        write_request(
            "/capture",
            &origin,
            Some(&token),
            Some("application/json"),
            capture_body(
                CaptureIntent::Takeover,
                None,
                vec![
                    file_item("https://example.com/a.bin", "a.bin"),
                    file_item("https://example.com/b.bin", "b.bin"),
                ],
            ),
        ),
    )
    .await;
    assert_eq!(two_takeover_items.status().as_u16(), 400);
    assert_eq!(
        response_json(two_takeover_items).await["code"],
        json!("TAKEOVER_ITEM_COUNT")
    );

    assert!(harness.bridge.records().takeovers.is_empty());
    assert!(harness.bridge.records().prefills.is_empty());
}

#[tokio::test]
async fn capture_reports_per_item_rejections_and_rejects_an_all_invalid_batch() {
    let harness = Harness::new(10, 10);
    let origin = extension_origin('a');
    let token = harness.issue_token(&origin, "Chrome extension");

    let mut hls = file_item("https://example.com/stream.m3u8", "stream.m3u8");
    hls.kind = Some(CaptureKind::Hls);
    let invalid = file_item("ftp://example.com/not-http.bin", "not-http.bin");

    let response = send(
        &harness.router,
        write_request(
            "/capture",
            &origin,
            Some(&token),
            Some("application/json"),
            capture_body(
                CaptureIntent::Prefill,
                None,
                vec![
                    file_item("https://example.com/ok.bin", "ok.bin"),
                    hls,
                    invalid.clone(),
                ],
            ),
        ),
    )
    .await;
    assert_eq!(response.status().as_u16(), 200);
    let body = response_json(response).await;
    assert_eq!(body["accepted"], json!(1));
    let rejected = body["rejected"].as_array().expect("rejected array");
    assert_eq!(rejected.len(), 2);
    assert_eq!(rejected[0]["index"], json!(1));
    assert_eq!(rejected[0]["code"], json!("UNSUPPORTED_KIND"));
    assert_eq!(rejected[1]["index"], json!(2));
    assert_eq!(rejected[1]["code"], json!("INVALID_URL"));
    assert_eq!(harness.bridge.records().prefills.len(), 1);
    assert_eq!(harness.bridge.records().prefills[0].items.len(), 1);

    let all_invalid = send(
        &harness.router,
        write_request(
            "/capture",
            &origin,
            Some(&token),
            Some("application/json"),
            capture_body(CaptureIntent::Prefill, None, vec![invalid]),
        ),
    )
    .await;
    assert_eq!(all_invalid.status().as_u16(), 400);
    assert_eq!(
        response_json(all_invalid).await["code"],
        json!("INVALID_CAPTURE_ITEM")
    );
    assert_eq!(harness.bridge.records().prefills.len(), 1);
}

// --- Capture behavior ---

#[tokio::test]
async fn takeover_creates_one_task_focuses_once_and_suppresses_duplicates() {
    let harness = Harness::new(10, 10);
    let origin = extension_origin('a');
    let token = harness.issue_token(&origin, "Chrome extension");
    let body = capture_body(
        CaptureIntent::Takeover,
        Some("browser-download-1"),
        vec![file_item("https://example.com/file.bin", "file.bin")],
    );

    let first = send(
        &harness.router,
        write_request(
            "/capture",
            &origin,
            Some(&token),
            Some("application/json"),
            body.clone(),
        ),
    )
    .await;
    assert_eq!(first.status().as_u16(), 200);
    let first_body = response_json(first).await;
    assert_eq!(first_body["intent"], json!("takeover"));
    assert_eq!(first_body["accepted"], json!(1));
    assert_eq!(first_body["duplicate"], json!(false));
    let task_id = first_body["taskId"]
        .as_str()
        .expect("takeover returns a task id")
        .to_string();

    let records = harness.bridge.records();
    assert_eq!(records.takeovers.len(), 1);
    assert_eq!(records.takeovers[0].1, NOW_MS);
    assert_eq!(records.focus_calls, 1);

    let second = send(
        &harness.router,
        write_request(
            "/capture",
            &origin,
            Some(&token),
            Some("application/json"),
            body,
        ),
    )
    .await;
    assert_eq!(second.status().as_u16(), 200);
    let second_body = response_json(second).await;
    assert_eq!(second_body["duplicate"], json!(true));
    assert_eq!(second_body["taskId"], json!(task_id));
    assert_eq!(
        harness.bridge.records().takeovers.len(),
        1,
        "a duplicate capture must not create a second task"
    );
}

#[tokio::test]
async fn takeover_duplicate_suppression_is_scoped_to_the_token() {
    let harness = Harness::new(10, 10);
    let origin = extension_origin('a');
    let first_token = harness.issue_token(&origin, "Chrome extension");
    let second_token = harness.issue_token(&origin, "Second extension");
    let body = capture_body(
        CaptureIntent::Takeover,
        Some("browser-download-1"),
        vec![file_item("https://example.com/file.bin", "file.bin")],
    );

    let first = send(
        &harness.router,
        write_request(
            "/capture",
            &origin,
            Some(&first_token),
            Some("application/json"),
            body.clone(),
        ),
    )
    .await;
    let first_body = response_json(first).await;
    let first_task = first_body["taskId"]
        .as_str()
        .expect("first task id")
        .to_string();

    let second = send(
        &harness.router,
        write_request(
            "/capture",
            &origin,
            Some(&second_token),
            Some("application/json"),
            body,
        ),
    )
    .await;
    assert_eq!(second.status().as_u16(), 200);
    let second_body = response_json(second).await;
    assert_eq!(second_body["duplicate"], json!(false));
    assert_ne!(second_body["taskId"], json!(first_task));
    assert_eq!(harness.bridge.records().takeovers.len(), 2);
}

#[tokio::test]
async fn prefill_queues_all_accepted_items_and_replaces_the_pending_slot() {
    let harness = Harness::new(10, 10);
    let origin = extension_origin('a');
    let token = harness.issue_token(&origin, "Chrome extension");

    let first_items = vec![
        file_item("https://example.com/one.bin", "one.bin"),
        file_item("https://example.com/two.bin", "two.bin"),
    ];
    let first = send(
        &harness.router,
        write_request(
            "/capture",
            &origin,
            Some(&token),
            Some("application/json"),
            capture_body(
                CaptureIntent::Prefill,
                Some("prefill-1"),
                first_items.clone(),
            ),
        ),
    )
    .await;
    assert_eq!(first.status().as_u16(), 200);
    let first_body = response_json(first).await;
    assert_eq!(first_body["intent"], json!("prefill"));
    assert_eq!(first_body["accepted"], json!(2));
    assert!(
        first_body.get("taskId").is_none(),
        "a prefill must not report a task id"
    );

    let first_records = harness.bridge.records();
    assert!(first_records.takeovers.is_empty());
    assert_eq!(first_records.prefills.len(), 1);
    assert_eq!(first_records.prefills[0].items, first_items);
    assert_eq!(first_records.prefills[0].received_at_ms, NOW_MS);
    assert_eq!(first_records.focus_calls, 1);

    let second_items = vec![file_item("https://example.com/three.bin", "three.bin")];
    let second = send(
        &harness.router,
        write_request(
            "/capture",
            &origin,
            Some(&token),
            Some("application/json"),
            capture_body(
                CaptureIntent::Prefill,
                Some("prefill-2"),
                second_items.clone(),
            ),
        ),
    )
    .await;
    assert_eq!(second.status().as_u16(), 200);
    let records = harness.bridge.records();
    assert_eq!(records.prefills.len(), 2);
    assert_eq!(
        records.prefills.last().expect("second prefill").items,
        second_items,
        "the later prefill replaces the earlier pending one"
    );
    assert!(records.takeovers.is_empty());
}

#[tokio::test]
async fn bridge_failures_return_503_without_creating_tasks_or_prefills() {
    let harness = Harness::new(10, 10);
    let origin = extension_origin('a');
    let token = harness.issue_token(&origin, "Chrome extension");

    harness.bridge.fail_takeover(ApiError::new(
        503,
        "ENGINE_UNAVAILABLE",
        "the download engine is unavailable",
    ));
    let takeover = send(
        &harness.router,
        write_request(
            "/capture",
            &origin,
            Some(&token),
            Some("application/json"),
            capture_body(
                CaptureIntent::Takeover,
                Some("browser-download-1"),
                vec![file_item("https://example.com/file.bin", "file.bin")],
            ),
        ),
    )
    .await;
    assert_eq!(takeover.status().as_u16(), 503);
    assert_eq!(
        response_json(takeover).await["code"],
        json!("ENGINE_UNAVAILABLE")
    );
    assert!(harness.bridge.records().prefills.is_empty());

    harness.bridge.fail_prefill(ApiError::new(
        503,
        "PREFILL_UNAVAILABLE",
        "prefill delivery failed",
    ));
    let prefill = send(
        &harness.router,
        write_request(
            "/capture",
            &origin,
            Some(&token),
            Some("application/json"),
            capture_body(
                CaptureIntent::Prefill,
                None,
                vec![file_item("https://example.com/file.bin", "file.bin")],
            ),
        ),
    )
    .await;
    assert_eq!(prefill.status().as_u16(), 503);
    assert!(response_json(prefill).await["code"].as_str().is_some());
}

#[tokio::test]
async fn revoke_invalidates_the_bearer_token_for_later_captures() {
    let harness = Harness::new(10, 10);
    let origin = extension_origin('a');
    let token = harness.issue_token(&origin, "Chrome extension");

    let revoke = send(
        &harness.router,
        write_request("/pair/revoke", &origin, Some(&token), None, Body::empty()),
    )
    .await;
    assert_eq!(revoke.status().as_u16(), 200);
    assert_eq!(response_json(revoke).await["revoked"], json!(true));

    let after = send(
        &harness.router,
        write_request(
            "/capture",
            &origin,
            Some(&token),
            Some("application/json"),
            capture_body(
                CaptureIntent::Takeover,
                Some("browser-download-1"),
                vec![file_item("https://example.com/file.bin", "file.bin")],
            ),
        ),
    )
    .await;
    assert_eq!(after.status().as_u16(), 401);
    assert_eq!(response_json(after).await["code"], json!("UNAUTHORIZED"));
    assert!(harness.bridge.records().takeovers.is_empty());
}

#[tokio::test]
async fn capture_rate_limits_per_token_with_retry_after() {
    let harness = Harness::new(10, 1);
    let origin = extension_origin('a');
    let token = harness.issue_token(&origin, "Chrome extension");

    let first = send(
        &harness.router,
        write_request(
            "/capture",
            &origin,
            Some(&token),
            Some("application/json"),
            capture_body(
                CaptureIntent::Prefill,
                Some("prefill-1"),
                vec![file_item("https://example.com/one.bin", "one.bin")],
            ),
        ),
    )
    .await;
    assert_eq!(first.status().as_u16(), 200);

    let limited = send(
        &harness.router,
        write_request(
            "/capture",
            &origin,
            Some(&token),
            Some("application/json"),
            capture_body(
                CaptureIntent::Prefill,
                Some("prefill-2"),
                vec![file_item("https://example.com/two.bin", "two.bin")],
            ),
        ),
    )
    .await;
    assert_eq!(limited.status().as_u16(), 429);
    assert!(
        retry_after(&limited).is_some_and(|seconds| seconds >= 1),
        "rate-limited responses carry Retry-After seconds"
    );
    assert_eq!(response_json(limited).await["code"], json!("RATE_LIMITED"));
    assert_eq!(harness.bridge.records().prefills.len(), 1);
}

#[tokio::test]
async fn bridge_event_log_never_contains_codes_tokens_urls_filenames_or_authorization() {
    let harness = Harness::new(10, 10);
    let origin = extension_origin('a');
    let code = harness.generate_code();

    let paired = send(
        &harness.router,
        write_request(
            "/pair",
            &origin,
            None,
            Some("application/json"),
            pairing_body(&code, API_PROTOCOL_VERSION),
        ),
    )
    .await;
    assert_eq!(paired.status().as_u16(), 200);
    let token = response_json(paired).await["token"]
        .as_str()
        .expect("token")
        .to_string();

    let capture = send(
        &harness.router,
        write_request(
            "/capture",
            &origin,
            Some(&token),
            Some("application/json"),
            capture_body(
                CaptureIntent::Takeover,
                Some("source-secret"),
                vec![file_item(
                    "https://secret.example/private-file.bin?token=page-secret",
                    "private-file.bin",
                )],
            ),
        ),
    )
    .await;
    assert_eq!(capture.status().as_u16(), 200);

    assert_eq!(CAPTURE_PREFILL_EVENT, "app://capture-prefill");
    let events = harness.bridge.records().events;
    for (event, fields) in events {
        let serialized = serde_json::to_string(&fields).expect("serialize logged fields");
        for secret in [
            code.as_str(),
            token.as_str(),
            "secret.example",
            "/private-file.bin",
            "private-file.bin",
            "page-secret",
        ] {
            assert!(
                !contains_secret(&event, secret) && !contains_secret(&serialized, secret),
                "log event {event:?} leaked {secret:?}: {serialized}"
            );
        }
    }
}

// --- Firefox extension origins ---

const FIREFOX_ORIGIN: &str = "moz-extension://12345678-90ab-cdef-1234-567890abcdef";

fn write_request_without_origin(
    path: &str,
    token: Option<&str>,
    content_type: Option<&str>,
    body: impl Into<Body>,
) -> Request<Body> {
    let mut builder = Request::builder()
        .method(Method::POST)
        .uri(path)
        .header(header::HOST, host_value());
    if let Some(content_type) = content_type {
        builder = builder.header(header::CONTENT_TYPE, content_type);
    }
    if let Some(token) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    builder.body(body.into()).expect("write request")
}

fn has_wildcard_cors(response: &Response<Body>) -> bool {
    match response
        .headers()
        .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
        .and_then(|value| value.to_str().ok())
    {
        Some(value) => value == "*",
        None => false,
    }
}

#[tokio::test]
async fn firefox_extension_origin_pairs_captures_and_revokes_with_its_token() {
    let harness = Harness::new(10, 10);

    let health = send(
        &harness.router,
        health_request(&host_value(), Some(FIREFOX_ORIGIN)),
    )
    .await;
    assert_eq!(
        health.status().as_u16(),
        200,
        "health must accept an exact Firefox extension origin"
    );
    assert!(!has_wildcard_cors(&health));

    let code = harness.generate_code();
    let paired = send(
        &harness.router,
        write_request(
            "/pair",
            FIREFOX_ORIGIN,
            None,
            Some("application/json"),
            pairing_body(&code, API_PROTOCOL_VERSION),
        ),
    )
    .await;
    assert_eq!(
        paired.status().as_u16(),
        200,
        "pairing must accept an exact Firefox extension origin"
    );
    let paired_body = response_json(paired).await;
    let token = paired_body["token"]
        .as_str()
        .expect("pairing returns a token")
        .to_string();
    assert_eq!(
        paired_body["client"]["origin"],
        json!(FIREFOX_ORIGIN),
        "the token must be bound to the exact Firefox origin"
    );

    let capture = send(
        &harness.router,
        write_request(
            "/capture",
            FIREFOX_ORIGIN,
            Some(&token),
            Some("application/json"),
            capture_body(
                CaptureIntent::Takeover,
                Some("firefox-download-1"),
                vec![file_item("https://example.com/file.bin", "file.bin")],
            ),
        ),
    )
    .await;
    assert_eq!(
        capture.status().as_u16(),
        200,
        "a Firefox-bound token must authorize a capture"
    );
    assert!(!has_wildcard_cors(&capture));
    let capture_result = response_json(capture).await;
    assert!(capture_result["taskId"].as_str().is_some());
    assert_eq!(harness.bridge.records().takeovers.len(), 1);

    let revoke = send(
        &harness.router,
        write_request(
            "/pair/revoke",
            FIREFOX_ORIGIN,
            Some(&token),
            None,
            Body::empty(),
        ),
    )
    .await;
    assert_eq!(
        revoke.status().as_u16(),
        200,
        "revocation must accept an exact Firefox extension origin"
    );
    assert_eq!(response_json(revoke).await["revoked"], json!(true));

    let after = send(
        &harness.router,
        write_request(
            "/capture",
            FIREFOX_ORIGIN,
            Some(&token),
            Some("application/json"),
            capture_body(
                CaptureIntent::Takeover,
                Some("firefox-download-2"),
                vec![file_item("https://example.com/other.bin", "other.bin")],
            ),
        ),
    )
    .await;
    assert_eq!(after.status().as_u16(), 401);
    assert_eq!(response_json(after).await["code"], json!("UNAUTHORIZED"));
    assert_eq!(
        harness.bridge.records().takeovers.len(),
        1,
        "a revoked token must not create another task"
    );
}

#[tokio::test]
async fn malformed_and_non_extension_origins_are_rejected_without_creating_work() {
    let harness = Harness::new(10, 10);
    let chrome_origin = extension_origin('a');
    let token = harness.issue_token(&chrome_origin, "Chrome extension");
    let invalid_origins = [
        "moz-extension://",
        "moz-extension://12345678-90ab-cdef-1234-567890abcdef/x",
        "moz-extension://12345678-90AB-CDEF-1234-567890ABCDEF",
        "http://example.com",
        "file:///tmp/extension",
        "null",
    ];

    for origin in invalid_origins {
        let health = send(&harness.router, health_request(&host_value(), Some(origin))).await;
        assert_eq!(health.status().as_u16(), 403, "health origin {origin}");
        assert_eq!(response_json(health).await["code"], json!("INVALID_ORIGIN"));

        let pair = send(
            &harness.router,
            write_request(
                "/pair",
                origin,
                None,
                Some("application/json"),
                pairing_body("12345678", API_PROTOCOL_VERSION),
            ),
        )
        .await;
        assert_eq!(pair.status().as_u16(), 403, "pair origin {origin}");
        assert_eq!(response_json(pair).await["code"], json!("INVALID_ORIGIN"));

        let capture = send(
            &harness.router,
            write_request(
                "/capture",
                origin,
                Some(&token),
                Some("application/json"),
                capture_body(
                    CaptureIntent::Takeover,
                    Some("browser-download-1"),
                    vec![file_item("https://example.com/file.bin", "file.bin")],
                ),
            ),
        )
        .await;
        assert_eq!(capture.status().as_u16(), 403, "capture origin {origin}");
        assert_eq!(
            response_json(capture).await["code"],
            json!("INVALID_ORIGIN")
        );

        let revoke = send(
            &harness.router,
            write_request("/pair/revoke", origin, Some(&token), None, Body::empty()),
        )
        .await;
        assert_eq!(revoke.status().as_u16(), 403, "revoke origin {origin}");
        assert_eq!(response_json(revoke).await["code"], json!("INVALID_ORIGIN"));
    }

    let missing_origin_pair = send(
        &harness.router,
        write_request_without_origin(
            "/pair",
            None,
            Some("application/json"),
            pairing_body("12345678", API_PROTOCOL_VERSION),
        ),
    )
    .await;
    assert_eq!(missing_origin_pair.status().as_u16(), 403);
    assert_eq!(
        response_json(missing_origin_pair).await["code"],
        json!("INVALID_ORIGIN")
    );

    let missing_origin_capture = send(
        &harness.router,
        write_request_without_origin(
            "/capture",
            Some(&token),
            Some("application/json"),
            capture_body(
                CaptureIntent::Prefill,
                None,
                vec![file_item("https://example.com/file.bin", "file.bin")],
            ),
        ),
    )
    .await;
    assert_eq!(missing_origin_capture.status().as_u16(), 403);
    assert_eq!(
        response_json(missing_origin_capture).await["code"],
        json!("INVALID_ORIGIN")
    );

    let missing_origin_revoke = send(
        &harness.router,
        write_request_without_origin("/pair/revoke", Some(&token), None, Body::empty()),
    )
    .await;
    assert_eq!(missing_origin_revoke.status().as_u16(), 403);
    assert_eq!(
        response_json(missing_origin_revoke).await["code"],
        json!("INVALID_ORIGIN")
    );

    let records = harness.bridge.records();
    assert!(records.takeovers.is_empty());
    assert!(records.prefills.is_empty());
}

#[tokio::test]
async fn tokens_bound_to_another_extension_scheme_are_rejected() {
    let harness = Harness::new(10, 10);
    let chrome_origin = extension_origin('a');
    let chrome_token = harness.issue_token(&chrome_origin, "Chrome extension");
    let firefox_token = harness.issue_token(FIREFOX_ORIGIN, "Firefox extension");
    let body = || {
        capture_body(
            CaptureIntent::Takeover,
            Some("browser-download-1"),
            vec![file_item("https://example.com/file.bin", "file.bin")],
        )
    };

    let chrome_token_at_firefox = send(
        &harness.router,
        write_request(
            "/capture",
            FIREFOX_ORIGIN,
            Some(&chrome_token),
            Some("application/json"),
            body(),
        ),
    )
    .await;
    assert_eq!(
        chrome_token_at_firefox.status().as_u16(),
        401,
        "a Chrome-bound token must not authorize a Firefox origin"
    );
    assert!(!has_wildcard_cors(&chrome_token_at_firefox));
    assert_eq!(
        response_json(chrome_token_at_firefox).await["code"],
        json!("UNAUTHORIZED")
    );

    let revoke_chrome_token_at_firefox = send(
        &harness.router,
        write_request(
            "/pair/revoke",
            FIREFOX_ORIGIN,
            Some(&chrome_token),
            None,
            Body::empty(),
        ),
    )
    .await;
    assert_eq!(revoke_chrome_token_at_firefox.status().as_u16(), 401);
    assert_eq!(
        response_json(revoke_chrome_token_at_firefox).await["code"],
        json!("UNAUTHORIZED")
    );

    let firefox_token_at_chrome = send(
        &harness.router,
        write_request(
            "/capture",
            &chrome_origin,
            Some(&firefox_token),
            Some("application/json"),
            body(),
        ),
    )
    .await;
    assert_eq!(
        firefox_token_at_chrome.status().as_u16(),
        401,
        "a Firefox-bound token must not authorize a Chrome origin"
    );
    assert!(!has_wildcard_cors(&firefox_token_at_chrome));
    assert_eq!(
        response_json(firefox_token_at_chrome).await["code"],
        json!("UNAUTHORIZED")
    );

    assert!(
        harness.bridge.records().takeovers.is_empty(),
        "a cross-scheme token must not create a task"
    );

    let own_chrome = send(
        &harness.router,
        write_request(
            "/capture",
            &chrome_origin,
            Some(&chrome_token),
            Some("application/json"),
            body(),
        ),
    )
    .await;
    assert_eq!(own_chrome.status().as_u16(), 200);
    assert!(!has_wildcard_cors(&own_chrome));

    let own_firefox = send(
        &harness.router,
        write_request(
            "/capture",
            FIREFOX_ORIGIN,
            Some(&firefox_token),
            Some("application/json"),
            body(),
        ),
    )
    .await;
    assert_eq!(own_firefox.status().as_u16(), 200);
}

// --- Real socket and bind failure ---

fn raw_http(port: u16, request: &str) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect to the API");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("read timeout");
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .expect("write timeout");
    stream.write_all(request.as_bytes()).expect("write request");
    let mut response = String::new();
    stream.read_to_string(&mut response).expect("read response");
    response
}

#[test]
fn real_socket_enforces_host_and_origin_then_shutdown_refuses_connections() {
    let temp = tempdir().expect("tempdir");
    let store = Arc::new(Mutex::new(
        PairingStore::open(temp.path().join("pairing.db")).expect("pairing store"),
    ));
    let bridge = FakeBridge::new();
    let clock = FakeClock::new(NOW_MS);
    let mut handle = ApiServerHandle::start(server_config(
        0,
        Arc::clone(&store),
        Arc::clone(&bridge),
        Arc::clone(&clock),
        10,
        10,
    ))
    .expect("start the loopback API on an ephemeral port");

    let port = handle.port();
    assert_ne!(port, 0, "port 0 must select an ephemeral port");
    assert_eq!(handle.origin(), format!("http://127.0.0.1:{port}"));

    let health = raw_http(
        port,
        &format!("GET /health HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"),
    );
    assert!(
        health.starts_with("HTTP/1.1 200"),
        "unexpected health response: {health}"
    );

    let wrong_host = raw_http(
        port,
        "GET /health HTTP/1.1\r\nHost: evil.test\r\nConnection: close\r\n\r\n",
    );
    assert!(
        wrong_host.starts_with("HTTP/1.1 403"),
        "unexpected wrong-host response: {wrong_host}"
    );

    let body = serde_json::to_string(&json!({
        "protocolVersion": API_PROTOCOL_VERSION,
        "code": "12345678",
        "label": "Chrome extension",
    }))
    .expect("pair body");
    let web_origin = raw_http(
        port,
        &format!(
            "POST /pair HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nOrigin: https://example.com\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        ),
    );
    assert!(
        web_origin.starts_with("HTTP/1.1 403"),
        "unexpected web-origin response: {web_origin}"
    );

    handle.shutdown();

    let address = format!("127.0.0.1:{port}");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match TcpStream::connect(&address) {
            Err(_) => break,
            Ok(mut stream) => {
                let _ = stream.set_write_timeout(Some(Duration::from_millis(200)));
                if stream
                    .write_all(
                        format!(
                            "GET /health HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
                        )
                        .as_bytes(),
                    )
                    .is_err()
                {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "the shutdown API still accepts connections"
                );
                std::thread::sleep(Duration::from_millis(25));
            }
        }
    }
}

#[test]
fn a_port_in_use_returns_a_typed_bind_failure_without_panicking() {
    let occupied = TcpListener::bind(("127.0.0.1", 0)).expect("hold a loopback port");
    let port = occupied.local_addr().expect("bound address").port();

    let temp = tempdir().expect("tempdir");
    let store = Arc::new(Mutex::new(
        PairingStore::open(temp.path().join("pairing.db")).expect("pairing store"),
    ));
    let bridge = FakeBridge::new();
    let clock = FakeClock::new(NOW_MS);

    let error = match ApiServerHandle::start(server_config(port, store, bridge, clock, 10, 10)) {
        Ok(_) => panic!("starting on an occupied port must fail"),
        Err(error) => error,
    };
    assert_eq!(error.status, 503);
    assert_eq!(error.code, "API_BIND_FAILED");
    assert!(!error.message.is_empty());
}
