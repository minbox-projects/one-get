//! Step 2 RED: behavior tests for safe HTTP transport and probe.
//!
//! Chosen signatures (reqwest types inconvenient in tests without dep):
//! - `validate_url(&str) -> Result<String, EngineError>`
//! - `check_redirect(&str, &str) -> Result<(), EngineError>`
//!
//! 416 with `Content-Range: bytes */123` is asserted as an outcome with no
//! range support and total 123 (single-connection fallback), not a typed error.
//!
//! Expected RED: compile/build failure because `oneget_lib::engine::probe`
//! (and/or `engine::http`) and reqwest are not yet implemented/added.

mod support;

use std::collections::HashMap;

use oneget_lib::engine::error::EngineError;
use oneget_lib::engine::http::{check_redirect, validate_url};
use oneget_lib::engine::probe::{probe, ProbeOptions, ProbeOutcome};
use support::fixtures::pattern_bytes;
use support::range_server::{FixtureConfig, RangeMode, RangeServer};

fn opts_default() -> ProbeOptions {
    ProbeOptions::default()
}

fn opts_allow_html() -> ProbeOptions {
    ProbeOptions {
        allow_html: true,
        extra_headers: Vec::new(),
    }
}

fn opts_with_headers(headers: Vec<(String, String)>) -> ProbeOptions {
    ProbeOptions {
        allow_html: false,
        extra_headers: headers,
    }
}

fn assert_code(err: &EngineError, code: &str) {
    assert_eq!(
        err.code(),
        code,
        "expected code {code}, got {}: {}",
        err.code(),
        err
    );
}

#[tokio::test]
async fn probe_206_valid_content_range_reports_metadata_and_single_range_request() {
    let body = pattern_bytes(1, 128);
    let config = FixtureConfig {
        body: body.clone(),
        content_type: Some("application/octet-stream".to_string()),
        etag: Some("\"etag-1\"".to_string()),
        last_modified: Some("Wed, 21 Oct 2015 07:28:00 GMT".to_string()),
        content_disposition: Some("attachment; filename=\"file.bin\"".to_string()),
        range_mode: RangeMode::HonorRange,
        ..Default::default()
    };
    let server = RangeServer::start(config);
    let url = server.url("/dl/file.bin");

    let out: ProbeOutcome = probe(&url, &opts_default())
        .await
        .expect("probe should succeed");

    assert!(out.range_supported, "206 should report range support");
    assert_eq!(out.total_bytes, Some(128));
    assert_eq!(
        out.content_type.as_deref(),
        Some("application/octet-stream")
    );
    assert_eq!(out.etag.as_deref(), Some("\"etag-1\""));
    assert_eq!(
        out.last_modified.as_deref(),
        Some("Wed, 21 Oct 2015 07:28:00 GMT")
    );
    assert_eq!(out.filename, "file.bin");
    assert_eq!(out.final_url, url);

    let reqs = server.requests();
    assert_eq!(reqs.len(), 1, "fixture must observe exactly one request");
    assert_eq!(reqs[0].method, "GET");
    assert_eq!(reqs[0].path, "/dl/file.bin");
    assert_eq!(reqs[0].range.as_deref(), Some("bytes=0-0"));
    assert_eq!(
        server.highest_offset_served(),
        0,
        "probe must request only bytes=0-0"
    );
}

#[tokio::test]
async fn probe_206_invalid_content_range_returns_invalid_response() {
    // Server will answer 206 probe but with mismatched/invalid Content-Range by
    // using a config path that triggers parse failure behavior: we simulate by
    // responding 206 with bad range via override not directly supported; instead
    // request a range the server cannot form consistently is hard, so use a
    // dedicated malformed case: server returns 416 for unparseable? No.
    // Provide body smaller/odd total via custom handling not available; easiest:
    // set large_sparse_logical_size to force Content-Range total mismatch for 0-0?
    // Actually server emits bytes start-end/total correctly. To get invalid
    // Content-Range (e.g. bytes 5-5/100 for 0-0 request) we need a crafted
    // response; approximate by asking fixture to behave normally is insufficient.
    // Alternative: use status/redirect not possible. We'll configure a server that
    // honors range but we can't malform it here - however requirement states
    // fixture must be deterministic; we encode malformed by special mode via
    // content_disposition trick? No. Instead, start server and have probe hit it:
    // easiest deterministic malformed CR: serve 206 but report range 5-5/100 while
    // client asked 0-0 => invalid. Implement by setting body len 100 and hacking
    // is not possible without extra mode. To satisfy "syntactically valid" and
    // contract, we rely on a request to a path that server treats specially is
    // not in config; so we use Probe206Then200? no.
    // Pragmatic: point at a normal honor-range server is wrong for this case.
    // We'll create a custom raw TCP listener inline for malformed Content-Range.
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().unwrap();
    thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            let body = b"x";
            let resp = format!(
                "HTTP/1.1 206 Partial Content\r\nAccept-Ranges: bytes\r\nContent-Type: application/octet-stream\r\nContent-Range: bytes 5-5/100\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(resp.as_bytes());
            let _ = stream.write_all(body);
        }
    });
    let url = format!("http://{}/bad", addr);
    let err = probe(&url, &opts_default()).await.expect_err("must fail");
    assert_code(&err, "INVALID_RESPONSE");
}

#[tokio::test]
async fn probe_200_with_content_length_reports_no_range_and_total() {
    let body = pattern_bytes(2, 64);
    let config = FixtureConfig {
        body,
        range_mode: RangeMode::IgnoreRange,
        ..Default::default()
    };
    let server = RangeServer::start(config);
    let url = server.url("/full.bin");

    let out = probe(&url, &opts_default()).await.expect("probe succeeds");
    assert!(!out.range_supported);
    assert_eq!(out.total_bytes, Some(64));
}

#[tokio::test]
async fn probe_200_without_content_length_reports_no_range_and_no_total() {
    let body = pattern_bytes(3, 32);
    let config = FixtureConfig {
        body,
        range_mode: RangeMode::IgnoreRange,
        unknown_length: true,
        ..Default::default()
    };
    let server = RangeServer::start(config);
    let url = server.url("/chunked-like.bin");

    let out = probe(&url, &opts_default()).await.expect("probe succeeds");
    assert!(!out.range_supported);
    assert_eq!(out.total_bytes, None);
}

#[tokio::test]
async fn probe_html_rejected_by_default_and_allowed_when_opted_in() {
    let body = b"<html>hi</html>".to_vec();
    let config = FixtureConfig {
        body: body.clone(),
        content_type: Some("text/html; charset=utf-8".to_string()),
        range_mode: RangeMode::IgnoreRange,
        ..Default::default()
    };
    let server = RangeServer::start(config);
    let url = server.url("/page");

    let err = probe(&url, &opts_default())
        .await
        .expect_err("html must be rejected by default");
    assert_code(&err, "HTML_RESPONSE");

    let out = probe(&url, &opts_allow_html()).await.expect("html allowed");
    assert_eq!(
        out.content_type.as_deref(),
        Some("text/html; charset=utf-8")
    );
    assert_eq!(out.total_bytes, Some(body.len() as u64));
}

#[tokio::test]
async fn probe_416_zero_yields_valid_empty_file_outcome() {
    let config = FixtureConfig {
        body: Vec::new(),
        response_416_zero: true,
        ..Default::default()
    };
    let server = RangeServer::start(config);
    let url = server.url("/empty");

    let out = probe(&url, &opts_default()).await.expect("empty outcome");
    assert_eq!(out.total_bytes, Some(0));
}

#[tokio::test]
async fn probe_416_nonzero_yields_no_range_and_total_fallback() {
    // Chosen: outcome with no range support and total 123 (single-connection fallback).
    let config = FixtureConfig {
        body: pattern_bytes(4, 200),
        response_416_nonzero: Some(123),
        ..Default::default()
    };
    let server = RangeServer::start(config);
    let url = server.url("/partial-unsat");

    let out = probe(&url, &opts_default())
        .await
        .expect("fallback outcome");
    assert!(!out.range_supported);
    assert_eq!(out.total_bytes, Some(123));
}

#[tokio::test]
async fn probe_maps_http_error_codes() {
    for (status, expected, retryable) in [
        (401u16, "AUTH_REQUIRED", false),
        (403, "AUTH_REQUIRED", false),
        (404, "NOT_FOUND", false),
        (429, "RATE_LIMITED", true),
        (500, "SERVER_ERROR", true),
    ] {
        if status == 429 {
            let config = FixtureConfig {
                rate_limit_429: true,
                retry_after: Some("2".to_string()),
                ..Default::default()
            };
            let server = RangeServer::start(config);
            let url = server.url("/err");
            let err = probe(&url, &opts_default()).await.expect_err("must fail");
            assert_code(&err, expected);
            assert_eq!(err.is_retryable(), retryable, "retryable mismatch for 429");
        } else {
            let config = FixtureConfig {
                status_override: Some(status),
                ..Default::default()
            };
            let server = RangeServer::start(config);
            let url = server.url("/err");
            let err = probe(&url, &opts_default()).await.expect_err("must fail");
            assert_code(&err, expected);
            assert_eq!(
                err.is_retryable(),
                retryable,
                "retryable mismatch for {status}"
            );
        }
    }
}

#[tokio::test]
async fn probe_follows_three_hop_redirect_and_records_final_url_and_count() {
    // 3-hop chain: /a -> /b -> /c -> /final, so 4 requests total.
    let final_server = RangeServer::start(FixtureConfig {
        body: pattern_bytes(5, 16),
        range_mode: RangeMode::HonorRange,
        ..Default::default()
    });
    let final_url = final_server.url("/final");
    let c = RangeServer::start(FixtureConfig {
        redirect_location: Some(final_url.clone()),
        redirect_status: Some(302),
        ..Default::default()
    });
    let c_url = c.url("/c");
    let b = RangeServer::start(FixtureConfig {
        redirect_location: Some(c_url.clone()),
        redirect_status: Some(302),
        ..Default::default()
    });
    let b_url = b.url("/b");
    let a = RangeServer::start(FixtureConfig {
        redirect_location: Some(b_url.clone()),
        redirect_status: Some(302),
        ..Default::default()
    });
    let start = a.url("/a");

    let out = probe(&start, &opts_default())
        .await
        .expect("follow redirects");
    assert_eq!(out.final_url, final_url, "final URL must be recorded");
    let total_requests = a.requests().len()
        + b.requests().len()
        + c.requests().len()
        + final_server.requests().len();
    assert_eq!(total_requests, 4, "3 hops => 4 requests");

    // 11-hop chain returns TOO_MANY_REDIRECTS.
    // Build 11 redirecting servers chained to a final one (12th hop would exceed?),
    // easier: chain 11 redirects so request count would become 12 > allowed.
    let end = RangeServer::start(FixtureConfig {
        body: pattern_bytes(6, 8),
        range_mode: RangeMode::HonorRange,
        ..Default::default()
    });
    let mut next = end.url("/end");
    let mut servers = Vec::new();
    for i in 0..11 {
        let s = RangeServer::start(FixtureConfig {
            redirect_location: Some(next.clone()),
            redirect_status: Some(302),
            ..Default::default()
        });
        next = s.url(&format!("/r{i}"));
        servers.push(s);
    }
    let err = probe(&next, &opts_default())
        .await
        .expect_err("11-hop chain must be rejected");
    assert_code(&err, "TOO_MANY_REDIRECTS");
    drop(servers);
    drop(end);
}

#[tokio::test]
async fn check_redirect_rejects_https_to_http_and_validate_url_rejects_unsafe_inputs() {
    let err = check_redirect("https://example.com/a", "http://example.com/b")
        .expect_err("downgrade must be rejected");
    assert_code(&err, "INSECURE_REDIRECT");

    for bad in [
        "file:///etc/passwd",
        "javascript:alert(1)",
        "data:text/plain,hi",
        "https://user:pass@example.com/file",
        "https://exa\0mple.com/file",
        "https://example.com/line\nbreak",
    ] {
        let err = validate_url(bad).expect_err(&format!("must reject: {bad}"));
        let code = err.code();
        assert!(
            code == "INVALID_URL" || code == "CREDENTIALED_URL",
            "unexpected code {code} for {bad}"
        );
        if bad.contains("user:pass@") {
            assert_code(&err, "CREDENTIALED_URL");
        } else {
            assert_code(&err, "INVALID_URL");
        }
    }

    let long = format!("https://example.com/{}", "a".repeat(9000));
    assert!(long.len() > 8 * 1024, "must exceed 8 KiB");
    let err = validate_url(&long).expect_err("long URL rejected");
    assert_code(&err, "INVALID_URL");
}

#[tokio::test]
async fn probe_cross_host_redirect_strips_sensitive_headers_and_keeps_safe_header() {
    // 127.0.0.1 -> localhost, same listener.
    let body_server = RangeServer::start(FixtureConfig {
        body: pattern_bytes(7, 10),
        range_mode: RangeMode::HonorRange,
        ..Default::default()
    });
    let localhost_target = body_server.host_url("localhost", "/target");
    let redirector = RangeServer::start(FixtureConfig {
        redirect_location: Some(localhost_target.clone()),
        redirect_status: Some(302),
        ..Default::default()
    });
    let start = redirector.url("/start");

    let headers = vec![
        ("Cookie".to_string(), "session=secret".to_string()),
        ("Authorization".to_string(), "Bearer token".to_string()),
        ("X-Probe-Test".to_string(), "keep-me".to_string()),
    ];
    let _out = probe(&start, &opts_with_headers(headers))
        .await
        .expect("probe with redirect");

    let redirected_reqs = body_server.requests();
    assert!(
        !redirected_reqs.is_empty(),
        "redirect target must receive request"
    );
    let h: &HashMap<String, String> = &redirected_reqs[0].headers;
    assert!(
        !h.contains_key("cookie"),
        "Cookie must be stripped on cross-host redirect"
    );
    assert!(
        !h.contains_key("authorization"),
        "Authorization must be stripped on cross-host redirect"
    );
    assert_eq!(
        h.get("x-probe-test").map(String::as_str),
        Some("keep-me"),
        "safe header must be preserved"
    );
}

#[tokio::test]
async fn probe_filename_prefers_filename_star_then_filename_then_url_fallback_and_sanitizes() {
    // filename* preferred over filename
    let s1 = RangeServer::start(FixtureConfig {
        body: pattern_bytes(8, 8),
        range_mode: RangeMode::HonorRange,
        content_disposition: Some(
            "attachment; filename=\"plain.txt\"; filename*=UTF-8''fancy%20name.bin".to_string(),
        ),
        ..Default::default()
    });
    let out1 = probe(&s1.url("/x"), &opts_default()).await.unwrap();
    assert_eq!(out1.filename, "fancy name.bin");

    // filename used when no filename*
    let s2 = RangeServer::start(FixtureConfig {
        body: pattern_bytes(9, 8),
        range_mode: RangeMode::HonorRange,
        content_disposition: Some("attachment; filename=\"only.txt\"".to_string()),
        ..Default::default()
    });
    let out2 = probe(&s2.url("/x"), &opts_default()).await.unwrap();
    assert_eq!(out2.filename, "only.txt");

    // URL path fallback when neither
    let s3 = RangeServer::start(FixtureConfig {
        body: pattern_bytes(10, 8),
        range_mode: RangeMode::HonorRange,
        content_disposition: None,
        ..Default::default()
    });
    let out3 = probe(&s3.url("/path/from-url.dat"), &opts_default())
        .await
        .unwrap();
    assert_eq!(out3.filename, "from-url.dat");

    // Adversarial traversal/slashes/backslashes sanitized to single safe component.
    let s4 = RangeServer::start(FixtureConfig {
        body: pattern_bytes(11, 8),
        range_mode: RangeMode::HonorRange,
        content_disposition: Some("attachment; filename=\"../../etc/passwd\"".to_string()),
        ..Default::default()
    });
    let out4 = probe(&s4.url("/x"), &opts_default()).await.unwrap();
    assert!(
        !out4.filename.contains('/') && !out4.filename.contains('\\'),
        "sanitized filename must not contain path separators: {:?}",
        out4.filename
    );
    assert!(
        !out4.filename.starts_with(".."),
        "sanitized filename must not start with traversal prefix: {:?}",
        out4.filename
    );
    assert!(!out4.filename.is_empty(), "filename must not be empty");

    let s5 = RangeServer::start(FixtureConfig {
        body: pattern_bytes(12, 8),
        range_mode: RangeMode::HonorRange,
        content_disposition: Some("attachment; filename=\"a\\\\b/c\"".to_string()),
        ..Default::default()
    });
    let out5 = probe(&s5.url("/x"), &opts_default()).await.unwrap();
    assert!(!out5.filename.contains('/') && !out5.filename.contains('\\'));
}
