use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use oneget_lib::app_state::AppState;
use oneget_lib::commands::app::{
    app_graceful_quit, app_info, deep_link_last, diagnostics_autostart_get,
    diagnostics_autostart_set, diagnostics_notification_status, diagnostics_secret_roundtrip,
    diagnostics_send_notification, handle_deep_link, settings_get, settings_save, AutostartService,
    NotificationService, NotificationStatus, QuitService, SecretService,
};
use oneget_lib::config::{SettingsStore, Theme};
use oneget_lib::error::AppError;
use oneget_lib::logging::{LogConfig, Logger};
use oneget_lib::platform::deeplink::{parse_deep_link, DeepLinkEvent};
use oneget_lib::platform::paths::AppPaths;
use serde_json::Value;
use tempfile::tempdir;

// --- Test fakes (test-only) ---

#[derive(Clone, Default)]
struct FakeSecretService {
    succeed: bool,
}

impl FakeSecretService {
    fn succeeding() -> Self {
        Self { succeed: true }
    }

    fn failing() -> Self {
        Self { succeed: false }
    }
}

impl SecretService for FakeSecretService {
    fn roundtrip(
        &self,
        _secret: &str,
    ) -> Result<oneget_lib::commands::app::SecretRoundTripResult, AppError> {
        if self.succeed {
            Ok(oneget_lib::commands::app::SecretRoundTripResult { success: true })
        } else {
            Err(AppError::new(
                "secret_roundtrip_failed",
                "secret roundtrip failed",
            ))
        }
    }
}

#[derive(Clone)]
struct FakeNotificationService {
    permission: String,
    send_allowed: bool,
}

impl FakeNotificationService {
    fn granted() -> Self {
        Self {
            permission: "granted".to_string(),
            send_allowed: true,
        }
    }

    fn denied() -> Self {
        Self {
            permission: "denied".to_string(),
            send_allowed: false,
        }
    }
}

impl NotificationService for FakeNotificationService {
    fn status(&self) -> Result<NotificationStatus, AppError> {
        Ok(NotificationStatus {
            permission: self.permission.clone(),
        })
    }

    fn send_test(&self) -> Result<oneget_lib::commands::app::NotificationSendResult, AppError> {
        if self.send_allowed {
            Ok(oneget_lib::commands::app::NotificationSendResult { sent: true })
        } else {
            Err(AppError::new(
                "notification_denied",
                "notification permission denied",
            ))
        }
    }
}

#[derive(Clone, Default)]
struct FakeAutostartService {
    enabled: Arc<Mutex<bool>>,
}

impl FakeAutostartService {
    fn new(initial: bool) -> Self {
        Self {
            enabled: Arc::new(Mutex::new(initial)),
        }
    }
}

impl AutostartService for FakeAutostartService {
    fn is_enabled(&self) -> Result<bool, AppError> {
        Ok(*self.enabled.lock().expect("autostart lock"))
    }

    fn set_enabled(&self, enabled: bool) -> Result<bool, AppError> {
        let mut guard = self.enabled.lock().expect("autostart lock");
        *guard = enabled;
        Ok(*guard)
    }
}

#[derive(Clone, Default)]
struct FakeQuitService {
    called: Arc<AtomicBool>,
}

impl FakeQuitService {
    fn was_called(&self) -> bool {
        self.called.load(Ordering::SeqCst)
    }
}

impl QuitService for FakeQuitService {
    fn request_quit(&self) -> Result<(), AppError> {
        self.called.store(true, Ordering::SeqCst);
        Ok(())
    }
}

struct TestHarness {
    _temp: tempfile::TempDir,
    state: AppState,
    settings_path: std::path::PathBuf,
    log_dir: std::path::PathBuf,
    quit: FakeQuitService,
}

fn build_harness(
    secret: Arc<dyn SecretService>,
    notification: Arc<dyn NotificationService>,
    autostart: Arc<dyn AutostartService>,
    quit: FakeQuitService,
) -> TestHarness {
    let temp = tempdir().expect("tempdir");
    let root = temp.path().join("app");
    let paths = AppPaths::with_root(&root).expect("app paths");
    let settings_path = paths.data.join("settings.json");
    let settings = SettingsStore::load(&settings_path).expect("settings store");
    let log_dir = paths.logs.clone();
    let logger = Logger::new(LogConfig::with_defaults(log_dir.clone())).expect("logger");
    let state = AppState::new(
        paths,
        settings,
        logger,
        secret,
        notification,
        autostart,
        Arc::new(quit.clone()),
    );
    TestHarness {
        _temp: temp,
        state,
        settings_path,
        log_dir,
        quit,
    }
}

fn assert_stable_error_code(err: &AppError) {
    assert!(
        !err.code.trim().is_empty(),
        "error code must be non-empty and stable"
    );
}

fn read_all_log_text(log_dir: &Path) -> String {
    let mut combined = String::new();
    if let Ok(entries) = fs::read_dir(log_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                if let Ok(text) = fs::read_to_string(&path) {
                    combined.push_str(&text);
                    combined.push('\n');
                }
            }
        }
    }
    combined
}

// 1. Deep-link parser matrix

#[test]
fn deep_link_accepts_valid_http_https_percent_encoded_and_ignores_unknown_params() {
    let http = parse_deep_link("oneget://download?url=http://example.com/file.zip")
        .expect("valid http deep link");
    assert_eq!(http.url, "http://example.com/file.zip");

    let https = parse_deep_link("oneget://download?url=https://example.com/file.zip")
        .expect("valid https deep link");
    assert_eq!(https.url, "https://example.com/file.zip");

    let encoded =
        parse_deep_link("oneget://download?url=https%3A%2F%2Fexample.com%2Fpath%3Fq%3D1%26r%3D2")
            .expect("percent-encoded inner URL");
    assert_eq!(encoded.url, "https://example.com/path?q=1&r=2");

    let with_unknown =
        parse_deep_link("oneget://download?foo=bar&url=https://example.com/a&extra=1")
            .expect("unknown params should be ignored");
    assert_eq!(with_unknown.url, "https://example.com/a");
}

#[test]
fn deep_link_rejects_missing_duplicate_and_unsupported_host() {
    let missing = parse_deep_link("oneget://download").expect_err("missing url must fail");
    assert_stable_error_code(&missing);

    let missing_empty = parse_deep_link("oneget://download?url=").expect_err("empty url must fail");
    assert_stable_error_code(&missing_empty);

    let duplicate =
        parse_deep_link("oneget://download?url=https://a.example/&url=https://b.example/")
            .expect_err("duplicate url params must fail");
    assert_stable_error_code(&duplicate);

    let bad_host = parse_deep_link("oneget://open?url=https://example.com/")
        .expect_err("unsupported host must fail");
    assert_stable_error_code(&bad_host);
}

#[test]
fn deep_link_rejects_wrong_scheme_malformed_encoding_inner_schemes_and_credentials() {
    let wrong_scheme = parse_deep_link("http://download?url=https://example.com/")
        .expect_err("wrong outer scheme must fail");
    assert_stable_error_code(&wrong_scheme);

    let malformed = parse_deep_link("oneget://download?url=https%3")
        .expect_err("malformed percent encoding must fail");
    assert_stable_error_code(&malformed);

    let malformed_hex = parse_deep_link("oneget://download?url=https%ZZexample.com")
        .expect_err("invalid percent hex must fail");
    assert_stable_error_code(&malformed_hex);

    for inner in [
        "file:///etc/passwd",
        "javascript:alert(1)",
        "data:text/plain,hello",
        "FILE:///C:/Windows/win.ini",
        "JAVASCRIPT:void(0)",
        "DATA:text/html,x",
    ] {
        let link = format!("oneget://download?url={inner}");
        let err = parse_deep_link(&link).expect_err("non-http(s) inner scheme must fail");
        assert_stable_error_code(&err);
    }

    let creds = parse_deep_link("oneget://download?url=https://user:pass@example.com/file")
        .expect_err("embedded credentials must fail");
    assert_stable_error_code(&creds);
}

#[test]
fn deep_link_rejects_over_8192_bytes_without_panic() {
    let base = "oneget://download?url=https://example.com/";
    let padding_len = 8193usize.saturating_sub(base.len()) + 1;
    let long_link = format!("{base}{}", "a".repeat(padding_len));
    assert!(
        long_link.len() > 8192,
        "constructed link must exceed 8192 bytes"
    );

    let result = std::panic::catch_unwind(|| parse_deep_link(&long_link));
    assert!(result.is_ok(), "parser must not panic on oversized input");
    let err = result.unwrap().expect_err("oversized link must fail");
    assert_stable_error_code(&err);
}

#[test]
fn deep_link_rejects_control() {
    let accepted = parse_deep_link("oneget://download?url=https://example.com/file.zip")
        .expect("normal accepted link must still be accepted");
    assert_eq!(accepted.url, "https://example.com/file.zip");

    for encoded in ["%0A", "%0D", "%09", "%00"] {
        let link = format!("oneget://download?url=https://example.com/path{encoded}tail");
        let result = std::panic::catch_unwind(|| parse_deep_link(&link));
        assert!(
            result.is_ok(),
            "parser must not panic for encoded control char {encoded}"
        );
        let err = result.unwrap().expect_err(&format!(
            "decoded control char {encoded} inside inner URL must be rejected"
        ));
        assert_stable_error_code(&err);
    }
}

// 2. DeepLinkEvent JSON

#[test]
fn deep_link_event_serialization_accepted_and_rejected() {
    let accepted = DeepLinkEvent::accepted("https://example.com/file.bin".to_string());
    let accepted_json: Value = serde_json::to_value(&accepted).expect("serialize accepted event");
    assert_eq!(accepted_json.get("accepted"), Some(&Value::Bool(true)));
    assert_eq!(
        accepted_json.get("url"),
        Some(&Value::String("https://example.com/file.bin".to_string()))
    );
    assert!(
        accepted_json.get("error").is_none() || accepted_json.get("error") == Some(&Value::Null),
        "accepted event should not carry an error object"
    );

    let rejected = DeepLinkEvent::rejected(
        "invalid_deep_link".to_string(),
        "deep link is invalid".to_string(),
    );
    let rejected_json: Value = serde_json::to_value(&rejected).expect("serialize rejected event");
    assert_eq!(rejected_json.get("accepted"), Some(&Value::Bool(false)));
    let err_obj = rejected_json
        .get("error")
        .and_then(Value::as_object)
        .expect("rejected event must have error object");
    assert_eq!(
        err_obj.get("code"),
        Some(&Value::String("invalid_deep_link".to_string()))
    );
    assert_eq!(
        err_obj.get("message"),
        Some(&Value::String("deep link is invalid".to_string()))
    );

    let text = serde_json::to_string(&rejected_json).expect("stringify rejected event");
    assert!(
        !text.contains("\"secret\""),
        "rejected event must not contain secret field"
    );
}

// 3. handle_deep_link and last_deep_link

#[test]
fn handle_deep_link_stores_accepted_and_rejected_as_last_event() {
    let quit = FakeQuitService::default();
    let harness = build_harness(
        Arc::new(FakeSecretService::succeeding()),
        Arc::new(FakeNotificationService::granted()),
        Arc::new(FakeAutostartService::new(false)),
        quit,
    );

    let accepted = handle_deep_link(
        &harness.state,
        "oneget://download?url=https://example.com/good.zip",
    );
    let accepted_json = serde_json::to_value(&accepted).expect("accepted json");
    assert_eq!(accepted_json.get("accepted"), Some(&Value::Bool(true)));
    assert_eq!(
        accepted_json.get("url"),
        Some(&Value::String("https://example.com/good.zip".to_string()))
    );

    let last = deep_link_last(&harness.state);
    let last_json = serde_json::to_value(&last).expect("last json");
    assert_eq!(last_json.get("accepted"), Some(&Value::Bool(true)));
    assert_eq!(
        last_json.get("url"),
        Some(&Value::String("https://example.com/good.zip".to_string()))
    );

    let rejected = handle_deep_link(&harness.state, "oneget://download?url=file:///etc/passwd");
    let rejected_json = serde_json::to_value(&rejected).expect("rejected json");
    assert_eq!(rejected_json.get("accepted"), Some(&Value::Bool(false)));
    assert!(
        rejected_json
            .get("error")
            .and_then(|e| e.get("code"))
            .and_then(Value::as_str)
            .map(|s| !s.is_empty())
            .unwrap_or(false),
        "rejected event must have a non-empty error code"
    );

    let last_rejected = deep_link_last(&harness.state);
    let last_rejected_json = serde_json::to_value(&last_rejected).expect("last rejected json");
    assert_eq!(
        last_rejected_json.get("accepted"),
        Some(&Value::Bool(false))
    );
    // Parsing only; no file is opened as a side effect (observable via returned/stored event).
}

// 4. App info and settings

#[test]
fn app_info_and_settings_round_trip_via_temp_paths() {
    let quit = FakeQuitService::default();
    let harness = build_harness(
        Arc::new(FakeSecretService::succeeding()),
        Arc::new(FakeNotificationService::granted()),
        Arc::new(FakeAutostartService::new(false)),
        quit,
    );

    let info = app_info(&harness.state);
    let info_json = serde_json::to_value(&info).expect("app info json");
    assert_eq!(
        info_json.get("name"),
        Some(&Value::String("OneGet".to_string()))
    );
    assert_eq!(
        info_json.get("version"),
        Some(&Value::String("0.1.0".to_string()))
    );
    let log_path = info_json
        .get("logPath")
        .and_then(Value::as_str)
        .expect("app info must include logPath");
    assert!(!log_path.is_empty(), "logPath must be non-empty");

    let defaults = settings_get(&harness.state);
    assert!(
        matches!(defaults.theme, Theme::System),
        "default theme should be system"
    );

    let mut updated = defaults.clone();
    updated.theme = Theme::Dark;
    let saved = settings_save(&harness.state, updated).expect("settings save");
    assert!(matches!(saved.theme, Theme::Dark));

    let reloaded_store =
        SettingsStore::load(&harness.settings_path).expect("reload settings store");
    let reloaded = reloaded_store.settings();
    assert!(
        matches!(reloaded.theme, Theme::Dark),
        "theme change must be visible in a reloaded store"
    );
}

// 5. Secret roundtrip

#[test]
fn secret_roundtrip_success_has_no_secret_field_and_failure_is_typed() {
    let quit = FakeQuitService::default();
    let raw_secret = "super-secret-diagnostic-value-12345";

    let harness_ok = build_harness(
        Arc::new(FakeSecretService::succeeding()),
        Arc::new(FakeNotificationService::granted()),
        Arc::new(FakeAutostartService::new(false)),
        quit.clone(),
    );
    let ok = diagnostics_secret_roundtrip(&harness_ok.state, raw_secret)
        .expect("roundtrip should succeed");
    let ok_json = serde_json::to_value(ok).expect("roundtrip json");
    assert_eq!(ok_json.get("success"), Some(&Value::Bool(true)));
    let text = serde_json::to_string(&ok_json).expect("stringify");
    assert!(
        !text.contains("\"secret\""),
        "roundtrip result must not contain a secret field"
    );
    assert!(
        !text.contains(raw_secret),
        "roundtrip result must not contain the raw secret value"
    );

    let harness_fail = build_harness(
        Arc::new(FakeSecretService::failing()),
        Arc::new(FakeNotificationService::granted()),
        Arc::new(FakeAutostartService::new(false)),
        quit,
    );
    let err = diagnostics_secret_roundtrip(&harness_fail.state, raw_secret)
        .expect_err("failing fake must return typed error");
    assert_stable_error_code(&err);
}

// 6. Notification and autostart

#[test]
fn notification_status_and_denied_send_and_autostart_transitions() {
    let quit = FakeQuitService::default();
    let autostart = FakeAutostartService::new(false);

    let harness = build_harness(
        Arc::new(FakeSecretService::succeeding()),
        Arc::new(FakeNotificationService::granted()),
        Arc::new(autostart.clone()),
        quit.clone(),
    );
    let status = diagnostics_notification_status(&harness.state).expect("notification status");
    assert_eq!(status.permission, "granted");

    let harness_denied = build_harness(
        Arc::new(FakeSecretService::succeeding()),
        Arc::new(FakeNotificationService::denied()),
        Arc::new(autostart.clone()),
        quit.clone(),
    );
    let denied = diagnostics_send_notification(&harness_denied.state)
        .expect_err("denied notification must fail");
    assert_eq!(
        denied.code, "notification_denied",
        "denied send must return notification_denied"
    );

    // Autostart get/set true/false with idempotent disable.
    let harness_auto = build_harness(
        Arc::new(FakeSecretService::succeeding()),
        Arc::new(FakeNotificationService::granted()),
        Arc::new(FakeAutostartService::new(false)),
        quit,
    );
    let initial = diagnostics_autostart_get(&harness_auto.state).expect("autostart get");
    assert!(!initial.enabled);

    let enabled = diagnostics_autostart_set(&harness_auto.state, true).expect("enable autostart");
    assert!(enabled.enabled);

    let still_enabled = diagnostics_autostart_get(&harness_auto.state).expect("get after enable");
    assert!(still_enabled.enabled);

    let disabled =
        diagnostics_autostart_set(&harness_auto.state, false).expect("disable autostart");
    assert!(!disabled.enabled);

    let disabled_again =
        diagnostics_autostart_set(&harness_auto.state, false).expect("idempotent disable");
    assert!(!disabled_again.enabled, "disable must be idempotent");
}

// 7. Quit

#[test]
fn graceful_quit_returns_quitting_logs_shutdown_and_calls_service() {
    let quit = FakeQuitService::default();
    let harness = build_harness(
        Arc::new(FakeSecretService::succeeding()),
        Arc::new(FakeNotificationService::granted()),
        Arc::new(FakeAutostartService::new(false)),
        quit.clone(),
    );

    let result = app_graceful_quit(&harness.state).expect("graceful quit");
    let json = serde_json::to_value(result).expect("quit result json");
    assert_eq!(json.get("quitting"), Some(&Value::Bool(true)));
    assert!(harness.quit.was_called(), "quit service must be invoked");

    // A shutdown record must be logged (before/after requesting quit).
    let logs = read_all_log_text(&harness.log_dir);
    assert!(
        logs.contains("shutdown"),
        "log output must contain a shutdown record"
    );
}
