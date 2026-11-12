//! Behavior tests for secret redaction across structured logs, the SQLite task
//! store, user-facing errors, and the configured network destinations.
//!
//! Exact engine configuration boundary exercised by this suite:
//!
//! ```text
//! impl Engine {
//!     pub fn set_proxy(&self, proxy: Option<ProxyConfig>) -> Result<(), EngineError>;
//!     pub fn set_cookie_file(&self, path: Option<std::path::PathBuf>);
//!     pub fn set_logger(&self, logger: std::sync::Arc<oneget_lib::logging::Logger>);
//! }
//!
//! pub struct ProxyConfig {
//!     pub url: String,
//!     pub username: Option<String>,
//!     pub password: Option<String>,
//! }
//! ```
//!
//! `ProxyConfig` lives in `oneget_lib::engine::http`. Privacy wins over replay:
//! a custom header value must appear in neither the structured log nor the
//! SQLite store. The store may persist safe header names with redacted or empty
//! values so a fresh engine knows which safe headers were configured, but it
//! must never retain a header value for replay. Forbidden and CRLF-bearing
//! headers are never persisted, and cookie values, proxy credentials, and URL
//! query tokens must appear in neither the log nor the store.

mod support;

#[path = "support/cookies.rs"]
mod cookies;
#[path = "support/proxy_server.rs"]
mod proxy_server;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use cookies::{future_expiry, write_cookie_file, NetscapeCookie};
use oneget_lib::engine::http::ProxyConfig;
use oneget_lib::engine::scheduler::{Engine, EngineOptions};
use oneget_lib::engine::state::TaskState;
use oneget_lib::engine::worker::DownloadSpec;
use oneget_lib::logging::{LogConfig, LogLevel, Logger};
use oneget_lib::store::Database;
use proxy_server::{ProxyCredentials, ProxyServer};
use sha2::{Digest, Sha256};
use support::range_server::{FixtureConfig, RangeServer};
use tempfile::tempdir;
use tokio::time::{sleep, timeout, Instant};

const POLL_INTERVAL: Duration = Duration::from_millis(15);
const WAIT_TIMEOUT: Duration = Duration::from_secs(20);

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// A 251-byte cycle that does not align with any 1 MiB range boundary.
fn non_periodic_bytes(len: usize) -> Vec<u8> {
    (0..len)
        .map(|index| ((index * 37 + 11) % 251 + 1) as u8)
        .collect()
}

fn open_store(directory: &tempfile::TempDir) -> Database {
    Database::open(directory.path().join("tasks.db")).expect("open store")
}

fn make_spec(
    url: String,
    filename: &str,
    connections: u32,
    expected_sha256: Option<String>,
    extra_headers: Vec<(String, String)>,
) -> DownloadSpec {
    DownloadSpec {
        url,
        filename: filename.to_string(),
        connections,
        allow_html: false,
        expected_sha256,
        extra_headers,
    }
}

async fn wait_for_terminal(store: &Database, id: &str, deadline: Duration) -> TaskState {
    let start = Instant::now();
    loop {
        let task = store.task(id).expect("load task").expect("task exists");
        if matches!(
            task.state,
            TaskState::Completed | TaskState::Failed | TaskState::Cancelled | TaskState::Paused
        ) {
            return task.state;
        }
        if start.elapsed() > deadline {
            panic!(
                "timed out waiting for task {id}, state {:?}, downloaded={}",
                task.state, task.downloaded_bytes
            );
        }
        sleep(POLL_INTERVAL).await;
    }
}

async fn run_engine_to_terminal(engine: &Engine, store: &Database, id: &str) -> TaskState {
    let ((), state) = tokio::join!(
        async {
            timeout(WAIT_TIMEOUT, engine.run())
                .await
                .expect("engine run timed out")
                .expect("engine run failed");
        },
        wait_for_terminal(store, id, WAIT_TIMEOUT),
    );
    state
}

fn sqlite_text_dump(path: &Path) -> String {
    let connection = rusqlite::Connection::open(path).expect("open sqlite for scan");
    let mut statement = connection
        .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
        .expect("list tables");
    let tables: Vec<String> = statement
        .query_map([], |row| row.get(0))
        .expect("query tables")
        .collect::<rusqlite::Result<Vec<String>>>()
        .expect("collect tables");
    drop(statement);

    let mut dump = String::new();
    for table in tables {
        if table.starts_with("sqlite_") {
            continue;
        }
        let escaped = table.replace('"', "\"\"");
        let mut statement = connection
            .prepare(&format!("SELECT * FROM \"{}\"", escaped))
            .expect("prepare table scan");
        let columns = statement.column_count();
        let mut rows = statement.query([]).expect("query table rows");
        while let Some(row) = rows.next().expect("next table row") {
            for index in 0..columns {
                let value: rusqlite::types::Value =
                    row.get(index).expect("read table cell as a value");
                match value {
                    rusqlite::types::Value::Text(text) => dump.push_str(&text),
                    rusqlite::types::Value::Blob(bytes) => {
                        dump.push_str(&String::from_utf8_lossy(&bytes))
                    }
                    _ => {}
                }
                dump.push('\u{1f}');
            }
        }
    }
    dump
}

fn read_log_files(logger: &Logger) -> String {
    let mut logs = String::new();
    for path in logger.files().expect("list log files") {
        if let Ok(text) = std::fs::read_to_string(&path) {
            logs.push_str(&text);
            logs.push('\n');
        }
    }
    logs
}

fn debug_logger(directory: &Path) -> Arc<Logger> {
    Arc::new(
        Logger::new(
            LogConfig::new(directory.to_path_buf(), 10 * 1024 * 1024, 5)
                .with_level(LogLevel::Debug),
        )
        .expect("create logger"),
    )
}

fn assert_absent(haystack: &str, secret: &str, label: &str) {
    assert!(
        !haystack.contains(secret),
        "{label} leaked the fixture secret {secret}"
    );
}

fn assert_no_internal_path(haystack: &str, path: &Path) {
    let text = path.display().to_string();
    assert!(
        !haystack.contains(&text),
        "user-visible text leaked the internal path {text}:\n{haystack}"
    );
}

#[tokio::test]
async fn logs_store_and_errors_redact_query_cookies_proxy_and_header_secrets() {
    const TOKEN: &str = "oneget-query-token-secret";
    const SIGNATURE: &str = "oneget-signature-secret";
    const COOKIE_VALUE: &str = "oneget-cookie-secret";
    const PROXY_USER: &str = "oneget-proxy-user";
    const PROXY_PASSWORD: &str = "oneget-proxy-password-secret";
    const HEADER_VALUE: &str = "oneget-header-secret";

    let body = non_periodic_bytes(1024 * 1024);
    let hash = sha256_hex(&body);
    let fixture = RangeServer::start(FixtureConfig {
        body: body.clone(),
        ..Default::default()
    });
    let missing_fixture = RangeServer::start(FixtureConfig {
        body: non_periodic_bytes(4096),
        status_override: Some(404),
        ..Default::default()
    });
    let proxy = ProxyServer::start(Some(ProxyCredentials {
        username: PROXY_USER.to_string(),
        password: PROXY_PASSWORD.to_string(),
    }));
    let dest = tempdir().expect("dest");
    let store_dir = tempdir().expect("store");
    let log_dir = tempdir().expect("logs");
    let cookie_dir = tempdir().expect("cookies");
    let logger = debug_logger(log_dir.path());
    let cookie_path = write_cookie_file(
        cookie_dir.path(),
        &[NetscapeCookie {
            domain: "127.0.0.1".to_string(),
            include_subdomains: false,
            path: "/private".to_string(),
            secure: false,
            expires: future_expiry(),
            name: "session_cookie".to_string(),
            value: COOKIE_VALUE.to_string(),
        }],
    );

    let engine = Engine::new(
        open_store(&store_dir),
        dest.path().to_path_buf(),
        EngineOptions { max_concurrent: 1 },
    );
    engine
        .set_proxy(Some(ProxyConfig {
            url: proxy.url(),
            username: Some(PROXY_USER.to_string()),
            password: Some(PROXY_PASSWORD.to_string()),
        }))
        .expect("configure the authenticated proxy");
    engine.set_cookie_file(Some(cookie_path));
    engine.set_logger(Arc::clone(&logger));

    let id = engine
        .add_task(make_spec(
            fixture.url(&format!(
                "/private/redaction.bin?token={TOKEN}&sig={SIGNATURE}"
            )),
            "redaction.bin",
            1,
            Some(hash.clone()),
            vec![("X-Custom-Trace".to_string(), HEADER_VALUE.to_string())],
        ))
        .expect("add task");

    let state = run_engine_to_terminal(&engine, engine.store(), &id).await;
    logger.flush().expect("flush logs");

    assert_eq!(
        state,
        TaskState::Completed,
        "the redaction download must complete"
    );
    let task = engine.store().task(&id).expect("load task").expect("task");
    assert_eq!(task.actual_sha256.as_deref(), Some(hash.as_str()));
    assert_eq!(
        std::fs::read(dest.path().join("redaction.bin")).expect("read final"),
        body
    );

    let logs = read_log_files(&logger);
    assert!(
        !logs.trim().is_empty(),
        "the engine must emit at least one structured log event"
    );
    for secret in [
        TOKEN,
        SIGNATURE,
        COOKIE_VALUE,
        PROXY_USER,
        PROXY_PASSWORD,
        HEADER_VALUE,
    ] {
        assert_absent(&logs, secret, "the structured log");
    }

    let failed_id = engine
        .add_task(make_spec(
            missing_fixture.url(&format!("/missing.bin?token={TOKEN}&sig={SIGNATURE}")),
            "missing.bin",
            1,
            None,
            Vec::new(),
        ))
        .expect("add failing task");
    let failed_state = run_engine_to_terminal(&engine, engine.store(), &failed_id).await;
    logger.flush().expect("flush logs");

    assert_eq!(failed_state, TaskState::Failed);
    let failed = engine
        .store()
        .task(&failed_id)
        .expect("load failed task")
        .expect("failed task exists");
    assert_eq!(failed.error_code.as_deref(), Some("NOT_FOUND"));
    let message = failed.error_message.as_deref().unwrap_or("");
    for secret in [
        TOKEN,
        SIGNATURE,
        COOKIE_VALUE,
        PROXY_USER,
        PROXY_PASSWORD,
        HEADER_VALUE,
    ] {
        assert_absent(message, secret, "the user-facing error message");
    }
    assert_no_internal_path(message, dest.path());
    assert_no_internal_path(message, store_dir.path());
    assert_no_internal_path(message, log_dir.path());

    let logs = read_log_files(&logger);
    for secret in [
        TOKEN,
        SIGNATURE,
        COOKIE_VALUE,
        PROXY_USER,
        PROXY_PASSWORD,
        HEADER_VALUE,
    ] {
        assert_absent(&logs, secret, "the structured log");
    }

    let dump = sqlite_text_dump(&store_dir.path().join("tasks.db"));
    for secret in [
        TOKEN,
        SIGNATURE,
        COOKIE_VALUE,
        PROXY_USER,
        PROXY_PASSWORD,
        HEADER_VALUE,
    ] {
        assert_absent(&dump, secret, "the SQLite store");
    }
    // Safe header names may be kept with a redacted or empty value so a fresh
    // engine knows which safe headers were configured, but the value itself
    // must never be stored. Dropping headers entirely is also privacy-correct,
    // so the name is not required; the value scan above is the invariant.

    // The only observed network destinations are the local fixtures reached
    // through the configured proxy; the origin fixtures never see the proxy
    // credential.
    let proxy_requests = proxy.requests();
    assert!(
        !proxy_requests.is_empty(),
        "the configured proxy must observe every download request"
    );
    let fixture_prefix = fixture.url("");
    let missing_prefix = missing_fixture.url("");
    for request in &proxy_requests {
        assert!(
            request.target.starts_with(&fixture_prefix)
                || request.target.starts_with(&missing_prefix),
            "the engine contacted an unexpected destination through the proxy: {}",
            request.target
        );
    }
    let origin_requests = fixture.requests();
    assert!(!origin_requests.is_empty());
    assert!(!missing_fixture.requests().is_empty());
    for request in origin_requests
        .iter()
        .chain(missing_fixture.requests().iter())
    {
        assert!(
            !request.headers.contains_key("proxy-authorization"),
            "the origin must never observe proxy credentials"
        );
    }
}
