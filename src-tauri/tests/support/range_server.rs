//! Deterministic local HTTP fixture for probe/range tests.
//!
//! Binds 127.0.0.1:0, serves in background threads, supports concurrent
//! connections, and shuts down cleanly on Drop. No public internet.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, Default)]
pub struct RecordedRequest {
    pub method: String,
    pub path: String,
    pub headers: HashMap<String, String>,
    pub range: Option<String>,
}

/// A byte range served in a response body, inclusive start and end.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServedRange {
    pub start: u64,
    pub end: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum RangeMode {
    #[default]
    HonorRange,
    IgnoreRange,
    Probe206Then200,
}

#[derive(Clone, Debug)]
pub struct FixtureConfig {
    pub body: Vec<u8>,
    pub content_type: Option<String>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub content_disposition: Option<String>,
    pub status_override: Option<u16>,
    pub expected_authorization: Option<String>,
    pub redirect_location: Option<String>,
    pub redirect_status: Option<u16>,
    pub range_mode: RangeMode,
    pub truncate_after: Option<usize>,
    pub rate_limit_429: bool,
    /// Limit how many requests receive a 429. `None` means every request.
    pub rate_limit_times: Option<usize>,
    pub retry_after: Option<String>,
    /// Return `fail_status` for the first N requests, then serve normally.
    pub fail_first_n: usize,
    pub fail_status: u16,
    /// On the first N large ranged responses, declare the full Content-Length
    /// but write only this many body bytes before closing the connection.
    pub truncate_first_ranged_after: Option<usize>,
    pub truncate_ranged_times: usize,
    /// Append this many extra bytes past the requested range in a 206 body.
    pub overrun_bytes: Option<usize>,
    /// Report a 206 Content-Range start offset by this delta after the probe.
    pub invalid_content_range_after_probe: Option<u64>,
    /// Accept a request and wait before writing any response bytes.
    pub stall_before_response: Option<Duration>,
    pub unknown_length: bool,
    pub response_416_zero: bool,
    pub response_416_nonzero: Option<u64>,
    pub large_sparse_logical_size: Option<u64>,
    /// Optional per-body streaming delay used to keep downloads in-flight.
    pub stream_delay: Option<Duration>,
    /// Chunk size for delayed streaming.
    pub stream_chunk_size: usize,
    /// Delay per streamed chunk for ranges that start in the first half of the
    /// body. When set, first-half ranges stream slower than second-half ranges.
    pub slow_first_half_delay: Option<Duration>,
    /// Number of ranged body bytes served with 206 before later range requests
    /// receive a full 200 response. One 206 stream runs at a time so the
    /// threshold crossing stays deterministic with concurrent connections.
    pub ignore_after_bytes: Option<u64>,
}

impl Default for FixtureConfig {
    fn default() -> Self {
        Self {
            body: Vec::new(),
            content_type: Some("application/octet-stream".to_string()),
            etag: None,
            last_modified: None,
            content_disposition: None,
            status_override: None,
            expected_authorization: None,
            redirect_location: None,
            redirect_status: None,
            range_mode: RangeMode::HonorRange,
            truncate_after: None,
            rate_limit_429: false,
            rate_limit_times: None,
            retry_after: None,
            fail_first_n: 0,
            fail_status: 500,
            truncate_first_ranged_after: None,
            truncate_ranged_times: 1,
            overrun_bytes: None,
            invalid_content_range_after_probe: None,
            stall_before_response: None,
            unknown_length: false,
            response_416_zero: false,
            response_416_nonzero: None,
            large_sparse_logical_size: None,
            stream_delay: None,
            stream_chunk_size: 16 * 1024,
            slow_first_half_delay: None,
            ignore_after_bytes: None,
        }
    }
}

#[derive(Default)]
struct SharedState {
    requests: Vec<RecordedRequest>,
    served_ranges: Vec<ServedRange>,
    highest_offset_served: u64,
    probe206_seen: bool,
    /// Body bytes already written by honored 206 responses, excluding the
    /// single-byte probe.
    ignore_bytes_committed: u64,
    /// Whether one 206 stream is currently writing its body.
    ignore_range_active: bool,
    /// Number of large ranged responses already truncated.
    truncate_hits: usize,
    runtime_status_override: Option<Option<u16>>,
    runtime_rate_limit_429: Option<bool>,
    runtime_etag: Option<Option<String>>,
    runtime_last_modified: Option<Option<String>>,
    runtime_total: Option<u64>,
}

pub struct RangeServer {
    addr: String,
    state: Arc<Mutex<SharedState>>,
    shutdown: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl RangeServer {
    pub fn start(config: FixtureConfig) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fixture");
        let addr = listener.local_addr().expect("local addr").to_string();
        listener
            .set_nonblocking(true)
            .expect("set nonblocking listener");
        let state = Arc::new(Mutex::new(SharedState::default()));
        let shutdown = Arc::new(AtomicBool::new(false));
        let state_clone = Arc::clone(&state);
        let shutdown_clone = Arc::clone(&shutdown);
        let config = Arc::new(config);
        let join = thread::spawn(move || loop {
            if shutdown_clone.load(Ordering::SeqCst) {
                break;
            }
            match listener.accept() {
                Ok((stream, _)) => {
                    let cfg = Arc::clone(&config);
                    let st = Arc::clone(&state_clone);
                    thread::spawn(move || {
                        let _ = handle_connection(stream, &cfg, &st);
                    });
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(5));
                }
                Err(_) => {
                    if shutdown_clone.load(Ordering::SeqCst) {
                        break;
                    }
                    thread::sleep(Duration::from_millis(5));
                }
            }
        });
        Self {
            addr,
            state,
            shutdown,
            join: Some(join),
        }
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://{}{}", self.addr, path)
    }

    pub fn host_url(&self, host: &str, path: &str) -> String {
        let port = self.addr.rsplit(':').next().unwrap_or("0");
        format!("http://{}:{}{}", host, port, path)
    }

    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.state.lock().expect("lock").requests.clone()
    }

    pub fn served_ranges(&self) -> Vec<ServedRange> {
        self.state.lock().expect("lock").served_ranges.clone()
    }

    pub fn highest_offset_served(&self) -> u64 {
        self.state.lock().expect("lock").highest_offset_served
    }

    /// Override the configured HTTP status for subsequent requests.
    pub fn set_runtime_status_override(&self, status: Option<u16>) {
        self.state.lock().expect("lock").runtime_status_override = Some(status);
    }

    /// Clear any runtime status override and fall back to the fixture config.
    pub fn clear_runtime_status_override(&self) {
        self.state.lock().expect("lock").runtime_status_override = None;
    }

    /// Override rate limiting for subsequent requests.
    pub fn set_runtime_rate_limit_429(&self, enabled: bool) {
        self.state.lock().expect("lock").runtime_rate_limit_429 = Some(enabled);
    }

    /// Override the ETag header for subsequent responses. `None` omits the header.
    pub fn set_runtime_etag(&self, etag: Option<String>) {
        self.state.lock().expect("lock").runtime_etag = Some(etag);
    }

    /// Clear any runtime ETag override and fall back to the fixture config.
    pub fn clear_runtime_etag(&self) {
        self.state.lock().expect("lock").runtime_etag = None;
    }

    /// Override the Last-Modified header for subsequent responses. `None` omits the header.
    pub fn set_runtime_last_modified(&self, last_modified: Option<String>) {
        self.state.lock().expect("lock").runtime_last_modified = Some(last_modified);
    }

    /// Clear any runtime Last-Modified override and fall back to the fixture config.
    pub fn clear_runtime_last_modified(&self) {
        self.state.lock().expect("lock").runtime_last_modified = None;
    }

    /// Override the effective total byte size reported in subsequent responses.
    pub fn set_runtime_total(&self, total: u64) {
        self.state.lock().expect("lock").runtime_total = Some(total);
    }

    /// Clear any runtime total override and fall back to the fixture config.
    pub fn clear_runtime_total(&self) {
        self.state.lock().expect("lock").runtime_total = None;
    }
}

impl Drop for RangeServer {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        // Wake accept loop.
        if let Ok(stream) = TcpStream::connect(&self.addr) {
            let _ = stream.shutdown(Shutdown::Both);
        }
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn record_served(state: &Arc<Mutex<SharedState>>, start: u64, end: u64) {
    let mut st = state.lock().expect("lock");
    st.served_ranges.push(ServedRange { start, end });
    st.highest_offset_served = st.highest_offset_served.max(end);
}

fn write_body(
    stream: &mut TcpStream,
    body: &[u8],
    delay: Option<Duration>,
    chunk_size: usize,
) -> std::io::Result<()> {
    if body.is_empty() {
        return Ok(());
    }
    if let Some(delay) = delay {
        let chunk_size = chunk_size.max(1);
        for chunk in body.chunks(chunk_size) {
            stream.write_all(chunk)?;
            let _ = stream.flush();
            thread::sleep(delay);
        }
        Ok(())
    } else {
        stream.write_all(body)
    }
}

fn stream_delay_for(config: &FixtureConfig, start: u64, total: u64) -> Option<Duration> {
    if start < total / 2 {
        config.slow_first_half_delay.or(config.stream_delay)
    } else {
        config.stream_delay
    }
}

fn handle_connection(
    mut stream: TcpStream,
    config: &FixtureConfig,
    state: &Arc<Mutex<SharedState>>,
) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
    // Accepted sockets can inherit the listener's non-blocking flag on some
    // platforms; response bodies are written with blocking semantics.
    let _ = stream.set_nonblocking(false);
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    loop {
        match stream.read(&mut tmp) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&tmp[..n]);
                if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
                if buf.len() > 64 * 1024 {
                    break;
                }
            }
            Err(err)
                if err.kind() == std::io::ErrorKind::WouldBlock
                    || err.kind() == std::io::ErrorKind::TimedOut =>
            {
                break;
            }
            Err(_) => break,
        }
    }
    let req_text = String::from_utf8_lossy(&buf);
    let mut lines = req_text.split("\r\n");
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("/").to_string();
    let mut headers = HashMap::new();
    for line in lines {
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    let range = headers.get("range").cloned();

    {
        let mut st = state.lock().expect("lock");
        st.requests.push(RecordedRequest {
            method: method.clone(),
            path: path.clone(),
            headers: headers.clone(),
            range: range.clone(),
        });
    }

    let (
        runtime_status_override,
        runtime_rate_limit_429,
        runtime_etag,
        runtime_last_modified,
        runtime_total,
    ) = {
        let st = state.lock().expect("lock");
        (
            st.runtime_status_override,
            st.runtime_rate_limit_429,
            st.runtime_etag.clone(),
            st.runtime_last_modified.clone(),
            st.runtime_total,
        )
    };
    let effective_rate_limit = runtime_rate_limit_429.unwrap_or(config.rate_limit_429);
    let effective_status_override = match runtime_status_override {
        Some(value) => value,
        None => config.status_override,
    };
    let request_number = state.lock().expect("lock").requests.len();

    if let Some(delay) = config.stall_before_response {
        thread::sleep(delay);
    }

    if let Some(expected) = &config.expected_authorization {
        let got = headers.get("authorization").cloned().unwrap_or_default();
        if &got != expected {
            let body = b"unauthorized";
            let resp = format!(
                "HTTP/1.1 401 Unauthorized\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(resp.as_bytes())?;
            stream.write_all(body)?;
            return Ok(());
        }
    }

    if config.fail_first_n > 0 && request_number <= config.fail_first_n {
        write_status_response(&mut stream, config.fail_status)?;
        return Ok(());
    }

    let rate_limit_active = effective_rate_limit
        && config
            .rate_limit_times
            .is_none_or(|limit| request_number <= limit);
    if rate_limit_active {
        let body = b"rate limited";
        let retry = config
            .retry_after
            .clone()
            .unwrap_or_else(|| "1".to_string());
        let resp = format!(
            "HTTP/1.1 429 Too Many Requests\r\nRetry-After: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            retry,
            body.len()
        );
        stream.write_all(resp.as_bytes())?;
        stream.write_all(body)?;
        return Ok(());
    }

    if let Some(status) = effective_status_override {
        write_status_response(&mut stream, status)?;
        return Ok(());
    }

    if let Some(location) = &config.redirect_location {
        let code = config.redirect_status.unwrap_or(302);
        let status = match code {
            301 => "301 Moved Permanently",
            302 => "302 Found",
            307 => "307 Temporary Redirect",
            308 => "308 Permanent Redirect",
            _ => "302 Found",
        };
        let resp = format!(
            "HTTP/1.1 {}\r\nLocation: {}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            status, location
        );
        stream.write_all(resp.as_bytes())?;
        return Ok(());
    }

    if config.response_416_zero {
        let resp = "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */0\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        stream.write_all(resp.as_bytes())?;
        return Ok(());
    }

    if let Some(total) = config.response_416_nonzero {
        let resp = format!(
            "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            total
        );
        stream.write_all(resp.as_bytes())?;
        return Ok(());
    }

    let effective_total = runtime_total.unwrap_or(
        config
            .large_sparse_logical_size
            .unwrap_or(config.body.len() as u64),
    );
    let effective_etag = runtime_etag.unwrap_or_else(|| config.etag.clone());
    let effective_last_modified =
        runtime_last_modified.unwrap_or_else(|| config.last_modified.clone());
    let mut body = config.body.clone();
    if let Some(limit) = config.truncate_after {
        if body.len() > limit {
            body.truncate(limit);
        }
    }
    let content_type = if config.content_type.is_some() {
        config.content_type.clone().unwrap()
    } else {
        "application/octet-stream".to_string()
    };

    let is_probe_zero = range.as_deref() == Some("bytes=0-0");
    let mut is_probe206_then200_other = false;
    let mut use_range = match config.range_mode {
        RangeMode::IgnoreRange => false,
        RangeMode::HonorRange => range.is_some(),
        RangeMode::Probe206Then200 => {
            if is_probe_zero {
                let mut st = state.lock().expect("lock");
                st.probe206_seen = true;
                true
            } else {
                is_probe206_then200_other = true;
                false
            }
        }
    };

    // Cumulative-threshold mode. Ranged requests are honored with 206 until
    // enough body bytes have been written; a later ranged request then gets a
    // full 200. Only one 206 body streams at a time so the crossing is
    // deterministic when the engine opens several connections concurrently.
    let mut ignore_range_turn = false;
    if use_range && !is_probe206_then200_other {
        if let Some(threshold) = config.ignore_after_bytes {
            if !is_probe_zero {
                let deadline = Instant::now() + Duration::from_secs(5);
                loop {
                    let mut st = state.lock().expect("lock");
                    if !st.ignore_range_active {
                        if st.ignore_bytes_committed >= threshold {
                            use_range = false;
                        } else {
                            st.ignore_range_active = true;
                            ignore_range_turn = true;
                        }
                        break;
                    }
                    if Instant::now() >= deadline {
                        // Never stall a fixture forever; answer the full body.
                        use_range = false;
                        break;
                    }
                    drop(st);
                    thread::sleep(Duration::from_millis(2));
                }
            }
        }
    }

    if let Some(r) = &range {
        if use_range && !is_probe206_then200_other {
            if let Some((start, end)) = parse_single_range(r, effective_total) {
                let start_usize = start as usize;
                let end_usize = end as usize;
                let slice: Vec<u8> = if (end_usize as u64) < config.body.len() as u64 {
                    config.body[start_usize..=end_usize].to_vec()
                } else if (start_usize as u64) < config.body.len() as u64 {
                    config.body[start_usize..].to_vec()
                } else {
                    Vec::new()
                };
                let mut truncate_limit = None;
                if let Some(limit) = config.truncate_first_ranged_after {
                    if !is_probe_zero && slice.len() > 64 * 1024 {
                        let mut st = state.lock().expect("lock");
                        if st.truncate_hits < config.truncate_ranged_times {
                            st.truncate_hits += 1;
                            truncate_limit = Some(limit);
                        }
                    }
                }
                let declared_len = slice.len();
                let write_len =
                    truncate_limit.map_or(declared_len, |limit| limit.min(declared_len));
                if write_len > 0 {
                    let actual_end = start + write_len as u64 - 1;
                    record_served(state, start, actual_end.min(end));
                }
                let overrun = config.overrun_bytes.unwrap_or(0);
                let reported_start = if is_probe_zero {
                    start
                } else {
                    start.saturating_add(config.invalid_content_range_after_probe.unwrap_or(0))
                };
                let mut resp = String::new();
                resp.push_str("HTTP/1.1 206 Partial Content\r\n");
                resp.push_str("Accept-Ranges: bytes\r\n");
                resp.push_str(&format!("Content-Type: {}\r\n", content_type));
                if let Some(etag) = &effective_etag {
                    resp.push_str(&format!("ETag: {}\r\n", etag));
                }
                if let Some(lm) = &effective_last_modified {
                    resp.push_str(&format!("Last-Modified: {}\r\n", lm));
                }
                if let Some(cd) = &config.content_disposition {
                    resp.push_str(&format!("Content-Disposition: {}\r\n", cd));
                }
                resp.push_str(&format!(
                    "Content-Range: bytes {}-{}/{}\r\n",
                    reported_start, end, effective_total
                ));
                resp.push_str(&format!(
                    "Content-Length: {}\r\n",
                    declared_len.saturating_add(overrun)
                ));
                resp.push_str("Connection: close\r\n\r\n");
                let delay = stream_delay_for(config, start, effective_total);
                let result = (|| -> std::io::Result<()> {
                    stream.write_all(resp.as_bytes())?;
                    write_body(
                        &mut stream,
                        &slice[..write_len],
                        delay,
                        config.stream_chunk_size,
                    )?;
                    if overrun > 0 {
                        let extra = vec![0xEEu8; overrun];
                        write_body(&mut stream, &extra, None, config.stream_chunk_size)?;
                    }
                    Ok(())
                })();
                if ignore_range_turn {
                    let mut st = state.lock().expect("lock");
                    if result.is_ok() {
                        st.ignore_bytes_committed =
                            st.ignore_bytes_committed.saturating_add(slice.len() as u64);
                    }
                    st.ignore_range_active = false;
                }
                return result;
            } else {
                if ignore_range_turn {
                    let mut st = state.lock().expect("lock");
                    st.ignore_range_active = false;
                }
                // Malformed/unsatisfiable as invalid response path; send 416 generic.
                let resp = format!(
                    "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    effective_total
                );
                stream.write_all(resp.as_bytes())?;
                return Ok(());
            }
        }
    }

    // 200 OK full response (or range ignored / probe206then200 later request)
    if !body.is_empty() {
        let end = body.len().saturating_sub(1) as u64;
        record_served(state, 0, end);
    }
    let mut resp = String::new();
    resp.push_str("HTTP/1.1 200 OK\r\n");
    if config.range_mode == RangeMode::HonorRange || config.range_mode == RangeMode::Probe206Then200
    {
        resp.push_str("Accept-Ranges: bytes\r\n");
    }
    resp.push_str(&format!("Content-Type: {}\r\n", content_type));
    if let Some(etag) = &effective_etag {
        resp.push_str(&format!("ETag: {}\r\n", etag));
    }
    if let Some(lm) = &effective_last_modified {
        resp.push_str(&format!("Last-Modified: {}\r\n", lm));
    }
    if let Some(cd) = &config.content_disposition {
        resp.push_str(&format!("Content-Disposition: {}\r\n", cd));
    }
    if !config.unknown_length {
        resp.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    resp.push_str("Connection: close\r\n\r\n");
    stream.write_all(resp.as_bytes())?;
    write_body(
        &mut stream,
        &body,
        config.stream_delay,
        config.stream_chunk_size,
    )?;
    Ok(())
}

fn write_status_response(stream: &mut TcpStream, status: u16) -> std::io::Result<()> {
    let (status_line, body): (&str, &[u8]) = match status {
        401 => ("401 Unauthorized", b"auth required"),
        403 => ("403 Forbidden", b"auth required"),
        404 => ("404 Not Found", b"not found"),
        500 => ("500 Internal Server Error", b"server error"),
        502 => ("502 Bad Gateway", b"server error"),
        503 => ("503 Service Unavailable", b"server error"),
        _ => ("500 Internal Server Error", b"error"),
    };
    let resp = format!(
        "HTTP/1.1 {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        status_line,
        body.len()
    );
    stream.write_all(resp.as_bytes())?;
    stream.write_all(body)
}

fn parse_single_range(header: &str, total: u64) -> Option<(u64, u64)> {
    let h = header.trim();
    let rest = h.strip_prefix("bytes=")?;
    if rest.contains(',') {
        return None;
    }
    let (s, e) = rest.split_once('-')?;
    if s.is_empty() {
        return None;
    }
    let start: u64 = s.parse().ok()?;
    let end: u64 = if e.is_empty() {
        total.saturating_sub(1)
    } else {
        e.parse().ok()?
    };
    if end < start {
        return None;
    }
    Some((start, end.min(total.saturating_sub(1))))
}
