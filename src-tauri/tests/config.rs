use oneget_lib::config::{
    CloseBehavior, Language, ProxyMode, ProxyType, SettingsStore, Theme, VideoQualityPreference,
    CURRENT_SCHEMA_VERSION,
};
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

    assert_eq!(
        CURRENT_SCHEMA_VERSION, 2,
        "this build must understand settings schema 2"
    );
    assert_eq!(settings.schema_version, 2);
    assert!(matches!(settings.theme, Theme::System));
    assert!(matches!(settings.log_level, LogLevel::Info));
    assert!(matches!(settings.language, Language::System));
    assert!(settings.default_directory.is_none());
    assert_eq!(settings.default_connections, 8);
    assert_eq!(settings.max_concurrent_tasks, 3);
    assert!(settings.global_rate_limit.is_none());
    assert!(!store.is_read_only(), "default load must be writable");

    assert!(path.exists(), "missing settings file must be created");
    let raw: Value = serde_json::from_str(&fs::read_to_string(&path).expect("read settings"))
        .expect("settings must be valid JSON");
    assert_eq!(raw.get("schemaVersion"), Some(&json!(2)));
    assert_eq!(raw.get("theme"), Some(&json!("system")));
    assert_eq!(raw.get("logLevel"), Some(&json!("info")));
    assert_eq!(raw.get("language"), Some(&json!("system")));
    assert_eq!(raw.get("defaultConnections"), Some(&json!(8)));
    assert_eq!(raw.get("maxConcurrentTasks"), Some(&json!(3)));
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
    assert_eq!(reloaded.settings().schema_version, 2);
}

#[test]
fn corrupt_json_is_backed_up_and_defaults_are_loaded() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("settings.json");
    let original = "{ definitely not valid json";
    fs::write(&path, original).expect("write corrupt settings");

    let store = SettingsStore::load(&path).expect("load corrupt settings without panic");
    assert_eq!(store.settings().schema_version, 2);
    assert!(matches!(store.settings().theme, Theme::System));
    assert!(matches!(store.settings().log_level, LogLevel::Info));
    assert!(matches!(store.settings().language, Language::System));
    assert_eq!(store.settings().default_connections, 8);
    assert_eq!(store.settings().max_concurrent_tasks, 3);
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
        2,
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
    assert_eq!(raw.get("schemaVersion"), Some(&json!(2)));
    assert_eq!(raw.get("theme"), Some(&json!("dark")));
}

#[test]
fn schema_1_document_migrates_to_schema_2_in_memory_and_saves_version_2() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("settings.json");
    let schema_1 = json!({
        "schemaVersion": 1,
        "theme": "dark",
        "logLevel": "debug",
        "extraFlag": "preserve-me",
        "nestedExtra": { "keep": true }
    });
    fs::write(
        &path,
        serde_json::to_vec_pretty(&schema_1).expect("serialize schema-1 settings"),
    )
    .expect("write schema-1 settings");

    let mut store = SettingsStore::load(&path).expect("load schema-1 settings");
    let migrated = store.settings();
    assert_eq!(
        migrated.schema_version, 2,
        "schema-1 settings must migrate in memory to version 2"
    );
    assert!(matches!(migrated.theme, Theme::Dark));
    assert!(matches!(migrated.log_level, LogLevel::Debug));
    assert!(matches!(migrated.language, Language::System));
    assert!(migrated.default_directory.is_none());
    assert_eq!(migrated.default_connections, 8);
    assert_eq!(migrated.max_concurrent_tasks, 3);
    assert!(migrated.global_rate_limit.is_none());

    store
        .save(store.settings())
        .expect("save migrated settings");

    let raw: Value = serde_json::from_str(&fs::read_to_string(&path).expect("read settings"))
        .expect("settings must remain valid JSON");
    assert_eq!(raw.get("schemaVersion"), Some(&json!(2)));
    assert_eq!(raw.get("theme"), Some(&json!("dark")));
    assert_eq!(raw.get("logLevel"), Some(&json!("debug")));
    assert_eq!(raw.get("extraFlag"), Some(&json!("preserve-me")));
    assert_eq!(raw.get("nestedExtra"), Some(&json!({ "keep": true })));
}

#[test]
fn schema_2_general_and_download_fields_round_trip_with_contract_wire_values() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("settings.json");
    let default_directory = dir.path().join("downloads");
    fs::create_dir_all(&default_directory).expect("create default directory");

    let mut store = SettingsStore::load(&path).expect("load settings");
    let mut settings = store.settings().clone();
    settings.language = Language::EnUs;
    settings.close_behavior = CloseBehavior::Quit;
    settings.default_directory = Some(default_directory.clone());
    settings.default_connections = 16;
    settings.max_concurrent_tasks = 5;
    settings.global_rate_limit = Some(2_097_152);
    store.save(&settings).expect("save schema-2 settings");

    let raw: Value = serde_json::from_str(&fs::read_to_string(&path).expect("read settings"))
        .expect("settings must be valid JSON");
    assert_eq!(raw.get("schemaVersion"), Some(&json!(2)));
    assert_eq!(raw.get("language"), Some(&json!("en-US")));
    assert_eq!(raw.get("closeBehavior"), Some(&json!("quit")));
    assert_eq!(
        raw.get("defaultDirectory"),
        Some(&json!(default_directory
            .to_str()
            .expect("default directory is valid UTF-8")))
    );
    assert_eq!(raw.get("defaultConnections"), Some(&json!(16)));
    assert_eq!(raw.get("maxConcurrentTasks"), Some(&json!(5)));
    assert_eq!(raw.get("globalRateLimit"), Some(&json!(2_097_152)));

    let reloaded = SettingsStore::load(&path)
        .expect("reload settings")
        .settings();
    assert_eq!(reloaded.schema_version, 2);
    assert!(matches!(reloaded.language, Language::EnUs));
    assert!(matches!(reloaded.close_behavior, CloseBehavior::Quit));
    assert_eq!(reloaded.default_directory, Some(default_directory));
    assert_eq!(reloaded.default_connections, 16);
    assert_eq!(reloaded.max_concurrent_tasks, 5);
    assert_eq!(reloaded.global_rate_limit, Some(2_097_152));
}

#[test]
fn language_and_close_behavior_use_the_contract_wire_values() {
    assert_eq!(
        serde_json::to_value(Language::System).expect("serialize language"),
        json!("system")
    );
    assert_eq!(
        serde_json::to_value(Language::ZhCn).expect("serialize language"),
        json!("zh-CN")
    );
    assert_eq!(
        serde_json::to_value(Language::EnUs).expect("serialize language"),
        json!("en-US")
    );
    assert_eq!(
        serde_json::to_value(CloseBehavior::Tray).expect("serialize close behavior"),
        json!("tray")
    );
    assert_eq!(
        serde_json::to_value(CloseBehavior::Quit).expect("serialize close behavior"),
        json!("quit")
    );
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

// --- Network and video settings fields (schema stays 2) ---

#[test]
fn missing_file_creates_and_writes_network_and_video_defaults() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("settings.json");
    assert!(!path.exists(), "settings file should start missing");

    let store = SettingsStore::load(&path).expect("load missing settings");
    let settings = store.settings();

    assert_eq!(settings.schema_version, 2);
    assert!(matches!(settings.proxy_mode, ProxyMode::Direct));
    assert!(matches!(settings.proxy_type, ProxyType::Http));
    assert_eq!(settings.proxy_host, None);
    assert_eq!(settings.proxy_port, None);
    assert_eq!(settings.default_cookies_file, None);
    assert_eq!(settings.user_agent, None);
    assert_eq!(settings.connect_timeout_secs, 15);
    assert_eq!(settings.read_timeout_secs, 30);
    assert_eq!(settings.retry_attempts, 5);
    assert_eq!(settings.hf_connection_cap, 8);
    assert!(settings.always_ask_quality);
    assert!(matches!(
        settings.video_quality_preference,
        VideoQualityPreference::Mp4Compatible
    ));

    assert!(path.exists(), "missing settings file must be created");
    let raw: Value = serde_json::from_str(&fs::read_to_string(&path).expect("read settings"))
        .expect("settings must be valid JSON");
    assert_eq!(raw.get("schemaVersion"), Some(&json!(2)));
    assert_eq!(raw.get("proxyMode"), Some(&json!("direct")));
    assert_eq!(raw.get("proxyType"), Some(&json!("http")));
    assert_eq!(raw.get("connectTimeoutSecs"), Some(&json!(15)));
    assert_eq!(raw.get("readTimeoutSecs"), Some(&json!(30)));
    assert_eq!(raw.get("retryAttempts"), Some(&json!(5)));
    assert_eq!(raw.get("hfConnectionCap"), Some(&json!(8)));
    assert_eq!(raw.get("alwaysAskQuality"), Some(&json!(true)));
    assert_eq!(
        raw.get("videoQualityPreference"),
        Some(&json!("mp4Compatible"))
    );
}

#[test]
fn document_missing_new_fields_loads_with_defaults_and_stays_schema_2() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("settings.json");
    let legacy = json!({
        "schemaVersion": 2,
        "theme": "dark",
        "logLevel": "warn",
        "nestedExtra": { "keep": true }
    });
    fs::write(
        &path,
        serde_json::to_vec_pretty(&legacy).expect("serialize settings"),
    )
    .expect("write settings without new fields");

    let mut store = SettingsStore::load(&path).expect("load settings without new fields");
    let settings = store.settings();
    assert_eq!(settings.schema_version, 2);
    assert!(matches!(settings.theme, Theme::Dark));
    assert!(matches!(settings.proxy_mode, ProxyMode::Direct));
    assert!(matches!(settings.proxy_type, ProxyType::Http));
    assert_eq!(settings.proxy_host, None);
    assert_eq!(settings.proxy_port, None);
    assert_eq!(settings.default_cookies_file, None);
    assert_eq!(settings.user_agent, None);
    assert_eq!(settings.connect_timeout_secs, 15);
    assert_eq!(settings.read_timeout_secs, 30);
    assert_eq!(settings.retry_attempts, 5);
    assert_eq!(settings.hf_connection_cap, 8);
    assert!(settings.always_ask_quality);
    assert!(matches!(
        settings.video_quality_preference,
        VideoQualityPreference::Mp4Compatible
    ));
    assert!(!store.is_read_only());

    store
        .save(store.settings())
        .expect("save reloaded defaults");
    let raw: Value = serde_json::from_str(&fs::read_to_string(&path).expect("read settings"))
        .expect("settings must remain valid JSON");
    assert_eq!(raw.get("schemaVersion"), Some(&json!(2)));
    assert_eq!(raw.get("proxyMode"), Some(&json!("direct")));
    assert_eq!(raw.get("connectTimeoutSecs"), Some(&json!(15)));
    assert_eq!(raw.get("nestedExtra"), Some(&json!({ "keep": true })));
}

#[test]
fn full_network_and_video_document_round_trips_without_loss_and_preserves_unknown_fields() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("settings.json");
    let cookies_path = dir.path().join("cookies.txt");
    fs::write(&cookies_path, b"# fixture cookies\n").expect("write cookies fixture");

    let document = json!({
        "schemaVersion": 2,
        "theme": "dark",
        "logLevel": "debug",
        "language": "zh-CN",
        "closeBehavior": "quit",
        "proxyMode": "manual",
        "proxyType": "socks5",
        "proxyHost": "proxy.example.test",
        "proxyPort": 1080,
        "defaultCookiesFile": cookies_path.to_str().expect("cookies path is valid UTF-8"),
        "userAgent": "OneGetFixture/1.0",
        "connectTimeoutSecs": 60,
        "readTimeoutSecs": 120,
        "retryAttempts": 10,
        "hfConnectionCap": 4,
        "alwaysAskQuality": false,
        "videoQualityPreference": "highest",
        "unknownTopLevel": { "keep": true },
        "unknownString": "preserve-me"
    });
    fs::write(
        &path,
        serde_json::to_vec_pretty(&document).expect("serialize full settings"),
    )
    .expect("write full settings");

    let mut store = SettingsStore::load(&path).expect("load full settings");
    let settings = store.settings();
    assert!(matches!(settings.proxy_mode, ProxyMode::Manual));
    assert!(matches!(settings.proxy_type, ProxyType::Socks5));
    assert_eq!(settings.proxy_host.as_deref(), Some("proxy.example.test"));
    assert_eq!(settings.proxy_port, Some(1080));
    assert_eq!(
        settings.default_cookies_file.as_deref(),
        Some(cookies_path.as_path())
    );
    assert_eq!(settings.user_agent.as_deref(), Some("OneGetFixture/1.0"));
    assert_eq!(settings.connect_timeout_secs, 60);
    assert_eq!(settings.read_timeout_secs, 120);
    assert_eq!(settings.retry_attempts, 10);
    assert_eq!(settings.hf_connection_cap, 4);
    assert!(!settings.always_ask_quality);
    assert!(matches!(
        settings.video_quality_preference,
        VideoQualityPreference::Highest
    ));

    store.save(store.settings()).expect("save full settings");

    let reloaded = SettingsStore::load(&path)
        .expect("reload settings")
        .settings();
    assert!(matches!(reloaded.proxy_mode, ProxyMode::Manual));
    assert!(matches!(reloaded.proxy_type, ProxyType::Socks5));
    assert_eq!(reloaded.proxy_host.as_deref(), Some("proxy.example.test"));
    assert_eq!(reloaded.proxy_port, Some(1080));
    assert_eq!(
        reloaded.default_cookies_file.as_deref(),
        Some(cookies_path.as_path())
    );
    assert_eq!(reloaded.user_agent.as_deref(), Some("OneGetFixture/1.0"));
    assert_eq!(reloaded.connect_timeout_secs, 60);
    assert_eq!(reloaded.read_timeout_secs, 120);
    assert_eq!(reloaded.retry_attempts, 10);
    assert_eq!(reloaded.hf_connection_cap, 4);
    assert!(!reloaded.always_ask_quality);
    assert!(matches!(
        reloaded.video_quality_preference,
        VideoQualityPreference::Highest
    ));

    let raw: Value = serde_json::from_str(&fs::read_to_string(&path).expect("read settings"))
        .expect("settings must remain valid JSON");
    assert_eq!(raw.get("schemaVersion"), Some(&json!(2)));
    assert_eq!(raw.get("proxyMode"), Some(&json!("manual")));
    assert_eq!(raw.get("proxyType"), Some(&json!("socks5")));
    assert_eq!(raw.get("proxyHost"), Some(&json!("proxy.example.test")));
    assert_eq!(raw.get("proxyPort"), Some(&json!(1080)));
    assert_eq!(
        raw.get("defaultCookiesFile").and_then(Value::as_str),
        cookies_path.to_str()
    );
    assert_eq!(raw.get("userAgent"), Some(&json!("OneGetFixture/1.0")));
    assert_eq!(raw.get("connectTimeoutSecs"), Some(&json!(60)));
    assert_eq!(raw.get("readTimeoutSecs"), Some(&json!(120)));
    assert_eq!(raw.get("retryAttempts"), Some(&json!(10)));
    assert_eq!(raw.get("hfConnectionCap"), Some(&json!(4)));
    assert_eq!(raw.get("alwaysAskQuality"), Some(&json!(false)));
    assert_eq!(raw.get("videoQualityPreference"), Some(&json!("highest")));
    assert_eq!(raw.get("unknownTopLevel"), Some(&json!({ "keep": true })));
    assert_eq!(raw.get("unknownString"), Some(&json!("preserve-me")));
}

#[test]
fn new_enums_use_the_contract_wire_values() {
    assert_eq!(
        serde_json::to_value(ProxyMode::Direct).expect("serialize proxy mode"),
        json!("direct")
    );
    assert_eq!(
        serde_json::to_value(ProxyMode::System).expect("serialize proxy mode"),
        json!("system")
    );
    assert_eq!(
        serde_json::to_value(ProxyMode::Manual).expect("serialize proxy mode"),
        json!("manual")
    );
    assert_eq!(
        serde_json::to_value(ProxyType::Http).expect("serialize proxy type"),
        json!("http")
    );
    assert_eq!(
        serde_json::to_value(ProxyType::Https).expect("serialize proxy type"),
        json!("https")
    );
    assert_eq!(
        serde_json::to_value(ProxyType::Socks5).expect("serialize proxy type"),
        json!("socks5")
    );
    assert_eq!(
        serde_json::to_value(VideoQualityPreference::Mp4Compatible)
            .expect("serialize video quality preference"),
        json!("mp4Compatible")
    );
    assert_eq!(
        serde_json::to_value(VideoQualityPreference::Highest)
            .expect("serialize video quality preference"),
        json!("highest")
    );

    assert!(matches!(
        serde_json::from_value::<ProxyMode>(json!("direct")).expect("parse proxy mode"),
        ProxyMode::Direct
    ));
    assert!(matches!(
        serde_json::from_value::<ProxyMode>(json!("system")).expect("parse proxy mode"),
        ProxyMode::System
    ));
    assert!(matches!(
        serde_json::from_value::<ProxyMode>(json!("manual")).expect("parse proxy mode"),
        ProxyMode::Manual
    ));
    assert!(matches!(
        serde_json::from_value::<ProxyType>(json!("http")).expect("parse proxy type"),
        ProxyType::Http
    ));
    assert!(matches!(
        serde_json::from_value::<ProxyType>(json!("https")).expect("parse proxy type"),
        ProxyType::Https
    ));
    assert!(matches!(
        serde_json::from_value::<ProxyType>(json!("socks5")).expect("parse proxy type"),
        ProxyType::Socks5
    ));
    assert!(matches!(
        serde_json::from_value::<VideoQualityPreference>(json!("mp4Compatible"))
            .expect("parse video quality preference"),
        VideoQualityPreference::Mp4Compatible
    ));
    assert!(matches!(
        serde_json::from_value::<VideoQualityPreference>(json!("highest"))
            .expect("parse video quality preference"),
        VideoQualityPreference::Highest
    ));
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
