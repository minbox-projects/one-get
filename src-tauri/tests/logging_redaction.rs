use oneget_lib::logging::redact::redact_text;
use oneget_lib::logging::{LogConfig, LogLevel, Logger};
use serde_json::json;
use std::fs;
use tempfile::tempdir;

const SECRET_QUERY_TOKEN: &str = "query-token-secret-value";
const SECRET_SIGNATURE: &str = "query-signature-secret-value";
const SECRET_AMZ_SIGNATURE: &str = "amzsig-very-secret";
const SECRET_AMZ_CREDENTIAL: &str = "amzcred-very-secret/20261111/us-east-1/s3/aws4_request";
const SECRET_AUTHORIZATION: &str = "Bearer auth-header-secret";
const SECRET_COOKIE: &str = "session=cookie-secret";
const SECRET_SET_COOKIE: &str = "sid=set-cookie-secret; Path=/; HttpOnly";
const SECRET_PROXY_AUTH: &str = "Basic proxy-auth-secret";
const SECRET_PROXY_PASSWORD: &str = "proxypass-secret";
const SECRET_HF_TOKEN: &str = "hf_secretTokenValue123456";

fn assert_redacted(text: &str, secret: &str, label: &str) {
    assert!(
        !text.contains(secret),
        "raw secret leaked for {label}: {secret}"
    );
    assert!(
        text.contains("[REDACTED]"),
        "redaction marker missing for {label}"
    );
}

#[test]
fn redact_text_replaces_sensitive_values_with_marker() {
    let cases = vec![
        (
            format!("https://example.com/download?token={SECRET_QUERY_TOKEN}&ok=1"),
            SECRET_QUERY_TOKEN,
            "url query token",
        ),
        (
            format!("https://example.com/download?signature={SECRET_SIGNATURE}&ok=1"),
            SECRET_SIGNATURE,
            "url query signature",
        ),
        (
            format!("https://s3.example.com/file?X-Amz-Signature={SECRET_AMZ_SIGNATURE}&x=1"),
            SECRET_AMZ_SIGNATURE,
            "X-Amz-Signature presigned value",
        ),
        (
            format!("https://s3.example.com/file?X-Amz-Credential={SECRET_AMZ_CREDENTIAL}&x=1"),
            SECRET_AMZ_CREDENTIAL,
            "X-Amz-Credential presigned value",
        ),
        (
            format!("Authorization: {SECRET_AUTHORIZATION}"),
            SECRET_AUTHORIZATION,
            "Authorization header",
        ),
        (
            format!("Cookie: {SECRET_COOKIE}"),
            SECRET_COOKIE,
            "Cookie header",
        ),
        (
            format!("Set-Cookie: {SECRET_SET_COOKIE}"),
            SECRET_SET_COOKIE,
            "Set-Cookie header",
        ),
        (
            format!("Proxy-Authorization: {SECRET_PROXY_AUTH}"),
            SECRET_PROXY_AUTH,
            "Proxy-Authorization header",
        ),
        (
            format!("https://user:{SECRET_PROXY_PASSWORD}@proxy.example.com:8080"),
            SECRET_PROXY_PASSWORD,
            "proxy password in userinfo",
        ),
        (
            format!("using token {SECRET_HF_TOKEN} for huggingface"),
            SECRET_HF_TOKEN,
            "HF token",
        ),
    ];

    for (input, secret, label) in cases {
        let redacted = redact_text(&input);
        assert_redacted(&redacted, secret, label);
    }
}

#[test]
fn logger_rotates_redacts_and_writes_complete_json_lines() {
    let dir = tempdir().expect("tempdir");
    let config = LogConfig::new(dir.path().to_path_buf(), 200, 3);
    assert_eq!(
        config.directory,
        dir.path(),
        "LogConfig::new must report configured directory"
    );
    assert_eq!(
        config.max_bytes, 200,
        "LogConfig must honor small max_bytes"
    );
    assert_eq!(config.max_files, 3, "LogConfig must honor small max_files");

    let logger = Logger::new(config).expect("logger should initialize");
    let secrets = vec![
        SECRET_QUERY_TOKEN,
        SECRET_SIGNATURE,
        SECRET_AMZ_SIGNATURE,
        SECRET_AMZ_CREDENTIAL,
        SECRET_AUTHORIZATION,
        SECRET_COOKIE,
        SECRET_SET_COOKIE,
        SECRET_PROXY_AUTH,
        SECRET_PROXY_PASSWORD,
        SECRET_HF_TOKEN,
    ];

    for i in 0..20 {
        let fields = json!({
            "index": i,
            "url": format!("https://example.com/file?token={SECRET_QUERY_TOKEN}&signature={SECRET_SIGNATURE}&X-Amz-Signature={SECRET_AMZ_SIGNATURE}&X-Amz-Credential={SECRET_AMZ_CREDENTIAL}"),
            "authorization": format!("Authorization: {SECRET_AUTHORIZATION}"),
            "cookie": format!("Cookie: {SECRET_COOKIE}"),
            "set_cookie": format!("Set-Cookie: {SECRET_SET_COOKIE}"),
            "proxy_auth": format!("Proxy-Authorization: {SECRET_PROXY_AUTH}"),
            "proxy": format!("https://user:{SECRET_PROXY_PASSWORD}@proxy.example.com"),
            "hf": SECRET_HF_TOKEN,
            "padding": "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
        });
        logger
            .log(LogLevel::Info, "test.event", fields)
            .expect("log write should succeed");
    }

    logger.flush().expect("flush should succeed");

    let files = logger.files().expect("list log files");
    assert!(
        files.len() <= 3,
        "rotation must keep at most max_files total files including current: {:?}",
        files
    );

    for path in &files {
        let content = fs::read_to_string(path).expect("read log file as text");
        for secret in &secrets {
            assert!(
                !content.contains(secret),
                "raw secret leaked in log file {}: {}",
                path.display(),
                secret
            );
        }
        assert!(
            content.contains("[REDACTED]"),
            "redaction marker missing in log file {}",
            path.display()
        );

        for (line_no, line) in content.lines().enumerate() {
            assert!(
                !line.is_empty(),
                "log file {} must not contain empty partial lines at {}",
                path.display(),
                line_no + 1
            );
            serde_json::from_str::<serde_json::Value>(line).unwrap_or_else(|err| {
                panic!(
                    "log file {} line {} is not valid JSON: {err}; line={line:?}",
                    path.display(),
                    line_no + 1
                )
            });
        }
    }

    let current = logger.current_path();
    assert!(
        current.exists(),
        "current log path must exist: {}",
        current.display()
    );
}
