//! Behavior tests for the desktop pairing command service and the pairing
//! runtime carried by `AppState`.
//!
//! The tests use only the agreed public interface:
//! - `oneget_lib::app_state::{AppState, PairingRuntime}`
//! - `oneget_lib::commands::pairing::{pairing_status, pairing_generate_code,
//!   pairing_list_clients, pairing_revoke, pairing_take_pending_prefill}`
//! - `oneget_lib::api::desktop::{PendingPrefill, CAPTURE_PREFILL_EVENT}`.
//!
//! Expected RED at authoring time: `commands::pairing`, `PairingRuntime`, and
//! the prefill slot accessors do not exist yet, so this target fails to
//! compile. GREEN is left to the implementation step.

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::Value;
use tempfile::tempdir;

use oneget_lib::api::clock::Clock;
use oneget_lib::api::desktop::{PendingPrefill, CAPTURE_PREFILL_EVENT};
use oneget_lib::api::pairing_store::{PairResult, PairingStore};
use oneget_lib::api::protocol::{CaptureItem, CaptureKind};
use oneget_lib::app_state::{AppState, PairingRuntime};
use oneget_lib::commands::app::{
    AutostartService, NotificationSendResult, NotificationService, NotificationStatus, QuitService,
    SecretRoundTripResult, SecretService,
};
use oneget_lib::commands::pairing::{
    pairing_generate_code, pairing_list_clients, pairing_revoke, pairing_status,
    pairing_take_pending_prefill, PairingStatus,
};
use oneget_lib::config::SettingsStore;
use oneget_lib::error::AppError;
use oneget_lib::logging::{LogConfig, Logger};
use oneget_lib::platform::paths::AppPaths;

const NOW_MS: i64 = 1_800_000_000_000;
const CODE_TTL_MS: i64 = 600_000;

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

    fn set(&self, now_ms: i64) {
        self.now_ms.store(now_ms, Ordering::SeqCst);
    }
}

impl Clock for FakeClock {
    fn now_ms(&self) -> i64 {
        self.now_ms.load(Ordering::SeqCst)
    }
}

#[derive(Clone, Copy)]
struct NoopSecrets;

impl SecretService for NoopSecrets {
    fn roundtrip(&self, _secret: &str) -> Result<SecretRoundTripResult, AppError> {
        Ok(SecretRoundTripResult { success: true })
    }
}

#[derive(Clone, Copy)]
struct NoopNotifications;

impl NotificationService for NoopNotifications {
    fn status(&self) -> Result<NotificationStatus, AppError> {
        Ok(NotificationStatus {
            permission: "granted".to_string(),
        })
    }

    fn send_test(&self) -> Result<NotificationSendResult, AppError> {
        Ok(NotificationSendResult { sent: true })
    }
}

#[derive(Clone, Copy)]
struct NoopAutostart;

impl AutostartService for NoopAutostart {
    fn is_enabled(&self) -> Result<bool, AppError> {
        Ok(false)
    }

    fn set_enabled(&self, enabled: bool) -> Result<bool, AppError> {
        Ok(enabled)
    }
}

#[derive(Clone, Copy)]
struct NoopQuit;

impl QuitService for NoopQuit {
    fn request_quit(&self) -> Result<(), AppError> {
        Ok(())
    }
}

struct Harness {
    _temp: tempfile::TempDir,
    state: AppState,
    store: Arc<Mutex<PairingStore>>,
    clock: Arc<FakeClock>,
}

fn base_state() -> (tempfile::TempDir, AppState) {
    let temp = tempdir().expect("tempdir");
    let paths = AppPaths::with_root(temp.path().join("app")).expect("app paths");
    let settings = SettingsStore::load(paths.data.join("settings.json")).expect("settings store");
    let logger = Logger::new(LogConfig::with_defaults(paths.logs.clone())).expect("logger");
    let state = AppState::new(
        paths,
        settings,
        logger,
        Arc::new(NoopSecrets),
        Arc::new(NoopNotifications),
        Arc::new(NoopAutostart),
        Arc::new(NoopQuit),
    );
    (temp, state)
}

fn unavailable_harness() -> Harness {
    let (temp, state) = base_state();
    let store = Arc::new(Mutex::new(
        PairingStore::open(temp.path().join("unused-pairing.db")).expect("scratch pairing store"),
    ));
    Harness {
        _temp: temp,
        state,
        store,
        clock: FakeClock::new(NOW_MS),
    }
}

fn available_harness() -> Harness {
    let (temp, state) = base_state();
    let store = Arc::new(Mutex::new(
        PairingStore::open(temp.path().join("pairing.db")).expect("pairing store"),
    ));
    let clock = FakeClock::new(NOW_MS);
    let runtime =
        PairingRuntime::available(Arc::clone(&store), Arc::clone(&clock) as Arc<dyn Clock>);
    Harness {
        _temp: temp,
        state: state.with_pairing(runtime),
        store,
        clock,
    }
}

fn extension_origin(id_char: char) -> String {
    format!("chrome-extension://{}", id_char.to_string().repeat(32))
}

fn capture_item(url: &str, filename: &str) -> CaptureItem {
    CaptureItem {
        url: url.to_string(),
        filename: Some(filename.to_string()),
        referer: Some("https://page.example/".to_string()),
        mime: Some("application/octet-stream".to_string()),
        size: Some(2_048),
        kind: Some(CaptureKind::File),
        headers: Vec::new(),
    }
}

fn pending_prefill(received_at_ms: i64, urls: &[(&str, &str)]) -> PendingPrefill {
    PendingPrefill {
        items: urls
            .iter()
            .map(|(url, filename)| capture_item(url, filename))
            .collect(),
        received_at_ms,
    }
}

/// Pair one client through the store and return the raw token plus client id.
fn pair_client(harness: &Harness, origin: &str, label: &str) -> (String, String) {
    let code = pairing_generate_code(&harness.state)
        .expect("generated pairing code")
        .code;
    let mut store = harness.store.lock().expect("pairing store lock");
    match store
        .pair(&code, origin, label, harness.clock.now_ms())
        .expect("pairing attempt")
    {
        PairResult::Paired { token, client } => (token, client.id),
        _ => panic!("expected a paired result"),
    }
}

fn contains_64_hex(value: &str) -> bool {
    value
        .as_bytes()
        .windows(64)
        .any(|window| window.iter().all(u8::is_ascii_hexdigit))
}

fn serialized<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_string(value).expect("serialize command output")
}

fn serialized_value<T: serde::Serialize>(value: &T) -> Value {
    serde_json::to_value(value).expect("serialize command output")
}

// --- Availability and status ---

#[test]
fn a_default_state_reports_pairing_unavailable_without_code_or_clients() {
    let harness = unavailable_harness();

    let status = pairing_status(&harness.state).expect("pairing status");
    assert!(!status.available);
    let reason = status.reason.expect("unavailable reason");
    assert!(!reason.is_empty(), "the unavailable reason is stable text");
    assert!(status.code.is_none());
    assert!(status.clients.is_empty());

    let again = pairing_status(&harness.state).expect("pairing status again");
    assert_eq!(again.reason.as_deref(), Some(reason.as_str()));
}

#[test]
fn an_available_state_reports_protocol_version_one_and_no_clients() {
    let harness = available_harness();

    let status = pairing_status(&harness.state).expect("pairing status");
    assert!(status.available);
    assert!(status.reason.is_none());
    assert_eq!(status.protocol_version, 1);
    assert!(status.code.is_none());
    assert!(status.clients.is_empty());
    assert!(pairing_list_clients(&harness.state)
        .expect("client list")
        .is_empty());
}

#[test]
fn generating_a_code_returns_eight_digits_expiring_after_ten_minutes_and_status_exposes_it() {
    let harness = available_harness();

    let view = pairing_generate_code(&harness.state).expect("generate code");
    assert_eq!(view.code.len(), 8);
    assert!(view.code.bytes().all(|byte| byte.is_ascii_digit()));
    assert_eq!(view.expires_at_ms, NOW_MS + CODE_TTL_MS);

    let status = pairing_status(&harness.state).expect("pairing status");
    let exposed = status.code.expect("status exposes the pending code");
    assert_eq!(exposed.code, view.code);
    assert_eq!(exposed.expires_at_ms, view.expires_at_ms);
}

#[test]
fn generating_a_new_code_replaces_the_code_exposed_by_status() {
    let harness = available_harness();

    let first = pairing_generate_code(&harness.state).expect("first code");
    let mut second = pairing_generate_code(&harness.state).expect("second code");
    for _ in 0..5 {
        if second.code != first.code {
            break;
        }
        second = pairing_generate_code(&harness.state).expect("replacement code");
    }
    assert_ne!(first.code, second.code);

    let status = pairing_status(&harness.state).expect("pairing status");
    assert_eq!(status.code.expect("pending code").code, second.code);
}

#[test]
fn pairing_through_the_store_makes_status_and_list_expose_one_client() {
    let harness = available_harness();
    let origin = extension_origin('a');
    let (_, client_id) = pair_client(&harness, &origin, "Chrome extension");

    let status = pairing_status(&harness.state).expect("pairing status");
    assert_eq!(status.clients.len(), 1);
    let client = &status.clients[0];
    assert_eq!(client.id, client_id);
    assert_eq!(client.label, "Chrome extension");
    assert_eq!(client.origin, origin);
    assert_eq!(client.created_at_ms, NOW_MS);
    assert_eq!(client.last_used_at_ms, NOW_MS);

    let listed = pairing_list_clients(&harness.state).expect("client list");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, client_id);
    assert_eq!(listed[0].origin, origin);
}

#[test]
fn revoke_removes_a_paired_client_and_unknown_ids_return_false() {
    let harness = available_harness();
    let (_, client_id) = pair_client(&harness, &extension_origin('a'), "Chrome extension");

    assert!(pairing_revoke(&harness.state, &client_id).expect("revoke client"));
    assert!(pairing_status(&harness.state)
        .expect("status after revoke")
        .clients
        .is_empty());
    assert!(
        !pairing_revoke(&harness.state, "unknown-client").expect("revoke unknown client"),
        "an unknown client id reports false"
    );
}

#[test]
fn configured_commands_serialize_with_camel_case_field_names() {
    let harness = available_harness();
    pair_client(&harness, &extension_origin('a'), "Chrome extension");
    let code = pairing_generate_code(&harness.state).expect("generate code");

    let status = pairing_status(&harness.state).expect("pairing status");
    let value = serialized_value(&status);
    for field in ["available", "protocolVersion", "code", "clients"] {
        assert!(value.get(field).is_some(), "status needs {field}");
    }
    assert!(value.get("protocol_version").is_none());
    let client = &value["clients"][0];
    for field in ["id", "label", "origin", "createdAtMs", "lastUsedAtMs"] {
        assert!(client.get(field).is_some(), "client needs {field}");
    }

    let code_value = serialized_value(&code);
    assert!(code_value.get("code").is_some());
    assert!(code_value.get("expiresAtMs").is_some());
    assert!(code_value.get("expires_at_ms").is_none());
}

// --- Pending prefill ---

#[test]
fn queued_prefill_is_returned_exactly_once() {
    let harness = available_harness();
    let queued = pending_prefill(NOW_MS, &[("https://example.com/one.bin", "one.bin")]);

    harness.state.queue_prefill(queued.clone());
    let delivered = pairing_take_pending_prefill(&harness.state).expect("take pending prefill");
    assert_eq!(delivered, Some(queued));
    assert_eq!(
        pairing_take_pending_prefill(&harness.state).expect("take again"),
        None
    );
}

#[test]
fn a_second_queued_prefill_replaces_the_first() {
    let harness = available_harness();
    let first = pending_prefill(NOW_MS, &[("https://example.com/first.bin", "first.bin")]);
    let second = pending_prefill(
        NOW_MS + 10,
        &[("https://example.com/second.bin", "second.bin")],
    );

    harness.state.queue_prefill(first);
    harness.state.queue_prefill(second.clone());

    assert_eq!(
        pairing_take_pending_prefill(&harness.state).expect("take pending prefill"),
        Some(second)
    );
}

#[test]
fn pending_prefill_event_and_payload_use_the_contract_names() {
    assert_eq!(CAPTURE_PREFILL_EVENT, "app://capture-prefill");

    let prefill = pending_prefill(NOW_MS, &[("https://example.com/one.bin", "one.bin")]);
    let value = serialized_value(&prefill);
    assert!(value.get("items").is_some());
    assert_eq!(value["receivedAtMs"], serde_json::json!(NOW_MS));
    assert!(value.get("received_at_ms").is_none());
    let item = &value["items"][0];
    assert_eq!(
        item["url"],
        serde_json::json!("https://example.com/one.bin")
    );
    assert_eq!(item["filename"], serde_json::json!("one.bin"));
}

// --- Clock, degraded state, and non-disclosure ---

#[test]
fn now_ms_reads_the_injected_clock_and_advances_with_it() {
    let harness = available_harness();

    assert_eq!(harness.state.now_ms(), NOW_MS);
    harness.clock.set(NOW_MS + 12_345);
    assert_eq!(harness.state.now_ms(), NOW_MS + 12_345);
}

#[test]
fn unavailable_generate_and_revoke_return_typed_errors_instead_of_panicking() {
    let harness = unavailable_harness();

    let generate_error = pairing_generate_code(&harness.state).expect_err("generate must fail");
    assert!(!generate_error.code.is_empty());
    assert!(!generate_error.message.is_empty());

    let revoke_error = pairing_revoke(&harness.state, "any-client").expect_err("revoke must fail");
    assert!(!revoke_error.code.is_empty());
    assert!(!revoke_error.message.is_empty());
}

#[test]
fn set_pairing_unavailable_reports_a_reason_and_blocks_generation() {
    let harness = available_harness();
    harness
        .state
        .set_pairing_unavailable("loopback port 37652 is occupied");

    let status = pairing_status(&harness.state).expect("degraded status");
    assert!(!status.available);
    assert!(status.code.is_none());
    assert!(status.clients.is_empty());
    assert!(
        status
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("occupied")),
        "the degraded reason explains the failure"
    );
    assert!(pairing_generate_code(&harness.state).is_err());
}

#[test]
fn pairing_unavailable_reason_is_a_stable_code_not_prose() {
    let (_temp, state) = base_state();
    let status = pairing_status(&state).expect("default pairing status");
    assert!(!status.available);
    assert_eq!(status.reason.as_deref(), Some("PAIRING_UNAVAILABLE"));
    assert!(status.code.is_none());
    assert!(status.clients.is_empty());

    state.set_pairing_unavailable("API_BIND_FAILED");
    let degraded = pairing_status(&state).expect("degraded pairing status");
    assert!(!degraded.available);
    assert_eq!(degraded.reason.as_deref(), Some("API_BIND_FAILED"));
    assert!(pairing_generate_code(&state).is_err());
}

#[test]
fn pairing_command_output_never_contains_a_raw_token() {
    let harness = available_harness();
    let (token, _) = pair_client(&harness, &extension_origin('a'), "Chrome extension");

    let status = pairing_status(&harness.state).expect("pairing status");
    let clients = pairing_list_clients(&harness.state).expect("client list");
    let code = pairing_generate_code(&harness.state).expect("generate code");
    let prefill = pending_prefill(NOW_MS, &[("https://example.com/one.bin", "one.bin")]);

    let outputs = [
        serialized(&status),
        serialized(&clients),
        serialized(&code),
        serialized(&prefill),
    ];
    for output in outputs {
        assert!(
            !output.contains(&token),
            "serialized command output leaked the bearer token"
        );
        assert!(
            !contains_64_hex(&output),
            "serialized command output contains a 64-character hex token"
        );
    }
}

#[test]
fn pairing_status_type_is_exported_with_expected_fields() {
    let harness = available_harness();
    let status: PairingStatus = pairing_status(&harness.state).expect("pairing status");
    assert_eq!(status.protocol_version, 1);
}
