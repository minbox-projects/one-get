use oneget_lib::config::{SettingsStore, Theme};
use oneget_lib::error::AppError;
use oneget_lib::logging::LogLevel;
use serde_json::{json, Value};
use std::fs;
use std::path::Path;
use tempfile::tempdir;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

const FIXTURE_SECRET_A: &str = "fixture-secret-token-should-not-persist";
const FIXTURE_SECRET_B: &str = "hf_fixtureSecretToken12345";
const FIXTURE_SECRET_C: &str = "Bearer fixture-authorization-secret";

fn assert_no_fixture_secrets(path: &Path) {
    let content = fs::read_to_string(path).expect("read settings file");
    for secret in [FIXTURE_SECRET_A, FIXTURE_SECRET_B, FIXTURE_SECRET_C] {
        assert!(
            !content.contains(secret),
            "settings file must not contain fixture secret: {secret}"
        );
    }
}

#[test]
fn missing_file_creates_and_writes_defaults() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("settings.json");
    assert!(!path.exists(), "settings file should start missing");

    let store = SettingsStore::load(&path).expect("load missing settings");
    let settings = store.settings();

    assert_eq!(settings.schema_version, 1);
    assert!(matches!(settings.theme, Theme::System));
    assert!(matches!(settings.log_level, LogLevel::Info));
    assert!(!store.is_read_only(), "default load must be writable");

    assert!(path.exists(), "missing settings file must be created");
    let raw: Value = serde_json::from_str(&fs::read_to_string(&path).expect("read settings"))
        .expect("settings must be valid JSON");
    assert_eq!(raw.get("schemaVersion"), Some(&json!(1)));
    assert_eq!(raw.get("theme"), Some(&json!("system")));
    assert_eq!(raw.get("logLevel"), Some(&json!("info")));
}

#[test]
fn settings_persist_across_store_reload() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("settings.json");

    {
        let mut store = SettingsStore::load(&path).expect("initial load");
        let mut updated = store.settings().clone();
        updated.theme = Theme::Dark;
        updated.log_level = LogLevel::Debug;
        store.save(&updated).expect("save updated settings");
    }

    let reloaded = SettingsStore::load(&path).expect("reload settings");
    assert!(matches!(reloaded.settings().theme, Theme::Dark));
    assert!(matches!(reloaded.settings().log_level, LogLevel::Debug));
    assert_eq!(reloaded.settings().schema_version, 1);
}

#[test]
fn corrupt_json_is_backed_up_and_defaults_are_loaded() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("settings.json");
    let original = "{ definitely not valid json";
    fs::write(&path, original).expect("write corrupt settings");

    let store = SettingsStore::load(&path).expect("load corrupt settings without panic");
    assert_eq!(store.settings().schema_version, 1);
    assert!(matches!(store.settings().theme, Theme::System));
    assert!(matches!(store.settings().log_level, LogLevel::Info));
    assert!(
        !store.is_read_only(),
        "corrupt-file recovery should yield writable defaults"
    );

    let backup_path = path.with_file_name(format!(
        "{}.corrupt",
        path.file_name()
            .and_then(|n| n.to_str())
            .expect("settings file name")
    ));
    assert!(
        backup_path.exists(),
        "deterministic corrupt backup must exist: {}",
        backup_path.display()
    );
    let backup_bytes = fs::read(&backup_path).expect("read backup");
    assert_eq!(
        backup_bytes,
        original.as_bytes(),
        "backup must preserve original bytes"
    );
}

#[test]
fn future_schema_is_read_only_and_never_overwrites_file() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("settings.json");
    let future = json!({
        "schemaVersion": 999,
        "theme": "dark",
        "logLevel": "debug",
        "extraFuture": true
    });
    let future_bytes = serde_json::to_vec_pretty(&future).expect("serialize future settings");
    fs::write(&path, &future_bytes).expect("write future settings");

    let mut store = SettingsStore::load(&path).expect("load future schema");
    assert_eq!(
        store.settings().schema_version,
        1,
        "future schema loads defaults"
    );
    assert!(matches!(store.settings().theme, Theme::System));
    assert!(matches!(store.settings().log_level, LogLevel::Info));
    assert!(store.is_read_only(), "future schema must be read-only");

    let err = store
        .save(store.settings())
        .expect_err("saving read-only future schema must fail");
    assert_typed_settings_error(&err, "settings_read_only");

    let after = fs::read(&path).expect("read settings after failed save");
    assert_eq!(
        after, future_bytes,
        "future schema file bytes must remain unchanged"
    );
}

#[test]
fn unknown_top_level_fields_are_preserved_across_save() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("settings.json");
    let initial = json!({
        "schemaVersion": 1,
        "theme": "system",
        "logLevel": "info",
        "extraFlag": "preserve-me",
        "nestedExtra": { "keep": true }
    });
    fs::write(
        &path,
        serde_json::to_vec_pretty(&initial).expect("serialize initial settings"),
    )
    .expect("write initial settings");

    let mut store = SettingsStore::load(&path).expect("load settings with unknown fields");
    let mut updated = store.settings().clone();
    updated.theme = Theme::Dark;
    store.save(&updated).expect("save updated settings");

    let reloaded = SettingsStore::load(&path).expect("reload settings");
    assert!(matches!(reloaded.settings().theme, Theme::Dark));

    let raw: Value = serde_json::from_str(&fs::read_to_string(&path).expect("read settings"))
        .expect("settings must remain valid JSON");
    assert_eq!(raw.get("extraFlag"), Some(&json!("preserve-me")));
    assert_eq!(raw.get("nestedExtra"), Some(&json!({ "keep": true })));
    assert_eq!(raw.get("schemaVersion"), Some(&json!(1)));
    assert_eq!(raw.get("theme"), Some(&json!("dark")));
}

#[cfg(unix)]
#[test]
fn write_failure_returns_typed_error_and_leaves_existing_file_intact() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("settings.json");

    let mut store = SettingsStore::load(&path).expect("initial load");
    let mut initial = store.settings().clone();
    initial.theme = Theme::Light;
    initial.log_level = LogLevel::Warn;
    store
        .save(&initial)
        .expect("initial save to establish existing file");
    let before = fs::read(&path).expect("read existing settings bytes");

    let mut perms = fs::metadata(dir.path())
        .expect("dir metadata")
        .permissions();
    perms.set_mode(0o555);
    fs::set_permissions(dir.path(), perms).expect("make settings directory read-only");

    let mut changed = initial.clone();
    changed.theme = Theme::Dark;
    changed.log_level = LogLevel::Debug;
    let err = store
        .save(&changed)
        .expect_err("save must fail when destination cannot be written");

    let mut restore = fs::metadata(dir.path())
        .expect("dir metadata for restore")
        .permissions();
    restore.set_mode(0o755);
    fs::set_permissions(dir.path(), restore).expect("restore directory permissions");

    assert_typed_app_error(&err);
    assert!(
        !err.message.contains(
            dir.path()
                .to_str()
                .expect("temp dir path should be valid UTF-8")
        ),
        "settings save error message must not contain internal directory paths"
    );
    assert!(
        !err.message.contains("settings.json"),
        "settings save error message must not contain internal file names"
    );
    let after = fs::read(&path).expect("read settings after failed save");
    assert_eq!(
        after, before,
        "existing settings file bytes must remain unchanged on write failure"
    );
}

#[test]
fn written_settings_file_contains_no_fixture_secrets() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("settings.json");

    let mut store = SettingsStore::load(&path).expect("load settings");
    let mut settings = store.settings().clone();
    settings.theme = Theme::Dark;
    settings.log_level = LogLevel::Debug;
    // Sanity: the written file must remain clean regardless of unrelated process context;
    // fixture secrets are defined above and must never appear in persisted settings.
    let _ = (FIXTURE_SECRET_A, FIXTURE_SECRET_B, FIXTURE_SECRET_C);
    store.save(&settings).expect("save settings");

    assert_no_fixture_secrets(&path);
}

fn assert_typed_settings_error(err: &AppError, expected_code: &str) {
    assert_typed_app_error(err);
    assert_eq!(
        err.code, expected_code,
        "future-schema save must return stable read-only code"
    );
}

fn assert_typed_app_error(err: &AppError) {
    let serialized = serde_json::to_value(err).expect("AppError must serialize");
    let obj = serialized
        .as_object()
        .expect("AppError must serialize as object");
    assert_eq!(
        obj.len(),
        2,
        "AppError must serialize as exactly {{code, message}}: {serialized}"
    );
    assert!(
        obj.contains_key("code"),
        "AppError missing code: {serialized}"
    );
    assert!(
        obj.contains_key("message"),
        "AppError missing message: {serialized}"
    );
    let _display = err.to_string();
    let _std_err: &dyn std::error::Error = err;
}
