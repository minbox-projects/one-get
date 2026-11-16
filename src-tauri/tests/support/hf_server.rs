//! Deterministic loopback HTTP forward-proxy fixture for Hugging Face tests.
//!
//! The engine can reach this fixture without public DNS by routing requests
//! through it as an HTTP proxy. It recognizes absolute-form resolver and CDN
//! URLs, serves responses itself, and records requests and concurrency. It never
//! contacts the public network.

#![allow(dead_code)]

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

#[derive(Clone, Debug, Default)]
pub struct RecordedRequest {
    pub method: String,
    pub target: String,
    pub host: String,
    pub path: String,
    pub headers: HashMap<String, String>,
    pub authorization: Option<String>,
    pub range: Option<String>,
    /// Full absolute-form request target; used as signed URL identity for CDN
    /// requests where query/path tokens change.
    pub signed_url_identity: String,
}

#[derive(Clone, Debug)]
pub enum ResolverBehavior {
    /// Redirect resolve/blob requests to this absolute CDN URL.
    RedirectToCdn(String),
    /// Redirect to a fresh signed CDN URL per resolver request by appending a
    /// unique token to the base URL.
    RedirectToFreshCdn(String),
    /// Reject gated resolver requests.
    Unauthorized,
    /// Serve bytes directly (not typically used for HF, but flexible).
    Bytes(Vec<u8>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CdnRangeMode {
    /// Honor byte ranges and return 206 for valid ranged requests.
    Honor,
    /// Always ignore Range and return a full 200 body.
    RejectAlways,
    /// Honor Range for probe-like requests (`bytes=0-0`) but return a full 200
    /// body for non-probe ranged requests once at least `after_bytes` bytes have
    /// been served successfully.
    HonorProbeThenIgnoreAfterBytes(u64),
}

#[derive(Clone, Debug)]
pub struct HfFixtureConfig {
    pub resolver_behavior: ResolverBehavior,
    /// Bytes served for recognized CDN requests.
    pub cdn_body: Vec<u8>,
    /// Delay applied while serving each CDN body chunk to make concurrency
    /// observable.
    pub cdn_stream_delay: Option<Duration>,
    pub cdn_stream_chunk_size: usize,
    /// Optional server-side cap on concurrent CDN responses. Requests above the
    /// cap wait until a slot is available.
    pub cdn_concurrency_limit: Option<usize>,
    /// Range handling mode for CDN responses.
    pub cdn_range_mode: CdnRangeMode,
    /// After this many successful CDN body responses, all subsequent CDN
    /// requests return 403.
    pub cdn_forbidden_after_successes: Option<usize>,
    /// All CDN requests return 403.
    pub cdn_persistent_403: bool,
    /// When set, after resolver has issued this many fresh redirects (resolver_counter >= value),
    /// all CDN requests return 403. This models "persistent 403 after N refreshes" in terms of
    /// refresh attempts observed by the resolver side.
    pub cdn_persistent_403_after_resolver_redirects: Option<usize>,
    /// Reject CDN requests (403) while resolver redirect count is less than this value.
    /// Example: value 4 means allow CDN only after >= 4 resolver redirects have been issued.
    pub cdn_allow_after_resolver_redirects_ge: Option<usize>,
    /// Serve 429 with this Retry-After value once, then succeed normally.
    pub cdn_429_retry_after_once: Option<String>,
}

impl Default for HfFixtureConfig {
    fn default() -> Self {
        Self {
            resolver_behavior: ResolverBehavior::Unauthorized,
            cdn_body: Vec::new(),
            cdn_stream_delay: None,
            cdn_stream_chunk_size: 16 * 1024,
            cdn_concurrency_limit: None,
            cdn_range_mode: CdnRangeMode::Honor,
            cdn_forbidden_after_successes: None,
            cdn_persistent_403: false,
            cdn_persistent_403_after_resolver_redirects: None,
            cdn_allow_after_resolver_redirects_ge: None,
            cdn_429_retry_after_once: None,
        }
    }
}

#[derive(Default)]
struct SharedState {
    resolver_requests: Vec<RecordedRequest>,
    cdn_requests: Vec<RecordedRequest>,
    active_cdn: usize,
    max_concurrent_cdn: usize,
    max_concurrent_cdn_after_first_429: usize,
    saw_429: bool,
}

struct RuntimeState {
    resolver_counter: AtomicUsize,
    cdn_success_responses: AtomicUsize,
    cdn_bytes_served: AtomicU64,
    cdn_429_served: AtomicBool,
}

impl RuntimeState {
    fn new() -> Self {
        Self {
            resolver_counter: AtomicUsize::new(0),
            cdn_success_responses: AtomicUsize::new(0),
            cdn_bytes_served: AtomicU64::new(0),
            cdn_429_served: AtomicBool::new(false),
        }
    }
}

struct CdnGate {
    limit: Option<usize>,
    state: Mutex<usize>,
    cv: Condvar,
}

impl CdnGate {
    fn new(limit: Option<usize>) -> Self {
        Self {
            limit,
            state: Mutex::new(0),
            cv: Condvar::new(),
        }
    }

    fn enter(&self) {
        let Some(limit) = self.limit else {
            return;
        };
        let mut active = self.state.lock().expect("cdn gate lock");
        while *active >= limit {
            active = self.cv.wait(active).expect("cdn gate wait");
        }
        *active += 1;
    }

    fn exit(&self) {
        if self.limit.is_none() {
            return;
        }
        let mut active = self.state.lock().expect("cdn gate lock");
        *active = active.saturating_sub(1);
        self.cv.notify_all();
    }
}

pub struct HfFixtureServer {
    addr: String,
    state: Arc<Mutex<SharedState>>,
    shutdown: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl HfFixtureServer {
    pub fn start(config: HfFixtureConfig) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind hf fixture");
        let addr = listener.local_addr().expect("hf fixture addr").to_string();
        listener
            .set_nonblocking(true)
            .expect("set nonblocking listener");
        let state = Arc::new(Mutex::new(SharedState::default()));
        let shutdown = Arc::new(AtomicBool::new(false));
        let gate = Arc::new(CdnGate::new(config.cdn_concurrency_limit));
        let runtime = Arc::new(RuntimeState::new());
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
                    let gate = Arc::clone(&gate);
                    let runtime = Arc::clone(&runtime);
                    thread::spawn(move || {
                        let _ = handle_connection(stream, &cfg, &st, &gate, &runtime);
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

    pub fn proxy_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub fn resolver_requests(&self) -> Vec<RecordedRequest> {
        self.state
            .lock()
            .expect("lock hf state")
            .resolver_requests
            .clone()
    }

    pub fn cdn_requests(&self) -> Vec<RecordedRequest> {
        self.state
            .lock()
            .expect("lock hf state")
            .cdn_requests
            .clone()
    }

    pub fn max_concurrent_cdn(&self) -> usize {
        self.state.lock().expect("lock hf state").max_concurrent_cdn
    }

    pub fn max_concurrent_cdn_after_first_429(&self) -> usize {
        self.state
            .lock()
            .expect("lock hf state")
            .max_concurrent_cdn_after_first_429
    }
}

impl Drop for HfFixtureServer {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        if let Ok(stream) = TcpStream::connect(&self.addr) {
            let _ = stream.shutdown(Shutdown::Both);
        }
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn is_resolver_host(host: &str) -> bool {
    matches!(
        host,
        "huggingface.co" | "www.huggingface.co" | "hf.co" | "www.hf.co"
    )
}

fn is_cdn_host(host: &str) -> bool {
    if host == "cas-bridge.xethub.hf.co" || host == "xethub.hf.co" {
        return true;
    }
    let host = host.strip_prefix("cdn-lfs.").unwrap_or(host);
    host.ends_with("huggingface.co") || host.ends_with("hf.co")
}

fn is_hf_recognized_host(host: &str) -> bool {
    is_resolver_host(host) || is_cdn_host(host)
}

fn handle_connection(
    mut stream: TcpStream,
    config: &HfFixtureConfig,
    state: &Arc<Mutex<SharedState>>,
    gate: &Arc<CdnGate>,
    runtime: &Arc<RuntimeState>,
) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
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
    let target = parts.next().unwrap_or("").to_string();

    let mut headers = HashMap::new();
    for line in lines {
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }

    let (host, path) = split_absolute_target(&target).unwrap_or_else(|| {
        let host = headers.get("host").cloned().unwrap_or_default();
        (host, target.clone())
    });
    let host_lc = host.to_ascii_lowercase();
    let recorded = RecordedRequest {
        method: method.clone(),
        target: target.clone(),
        host: host_lc.clone(),
        path: path.clone(),
        headers: headers.clone(),
        authorization: headers.get("authorization").cloned(),
        range: headers.get("range").cloned(),
        signed_url_identity: target.clone(),
    };

    if !is_hf_recognized_host(&host_lc) {
        let body = b"unknown upstream";
        let resp = format!(
            "HTTP/1.1 502 Bad Gateway\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(resp.as_bytes())?;
        stream.write_all(body)?;
        return Ok(());
    }

    if is_resolver_host(&host_lc) {
        state
            .lock()
            .expect("lock hf state")
            .resolver_requests
            .push(recorded);
        return respond_resolver(&mut stream, config, runtime);
    }

    // CDN host
    gate.enter();
    {
        let mut st = state.lock().expect("lock hf state");
        st.cdn_requests.push(recorded);
        st.active_cdn += 1;
        if st.active_cdn > st.max_concurrent_cdn {
            st.max_concurrent_cdn = st.active_cdn;
        }
        if st.saw_429 && st.active_cdn > st.max_concurrent_cdn_after_first_429 {
            st.max_concurrent_cdn_after_first_429 = st.active_cdn;
        }
    }
    let (result, served_429) = respond_cdn(&mut stream, config, runtime, &headers);
    {
        let mut st = state.lock().expect("lock hf state");
        st.active_cdn = st.active_cdn.saturating_sub(1);
        if served_429 {
            st.saw_429 = true;
        }
    }
    gate.exit();
    result
}

fn respond_resolver(
    stream: &mut TcpStream,
    config: &HfFixtureConfig,
    runtime: &Arc<RuntimeState>,
) -> std::io::Result<()> {
    match &config.resolver_behavior {
        ResolverBehavior::Unauthorized => {
            let body = b"gated";
            let resp = format!(
                "HTTP/1.1 401 Unauthorized\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(resp.as_bytes())?;
            stream.write_all(body)
        }
        ResolverBehavior::RedirectToCdn(location) => {
            let resp = format!(
                "HTTP/1.1 302 Found\r\nLocation: {}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                location
            );
            stream.write_all(resp.as_bytes())
        }
        ResolverBehavior::RedirectToFreshCdn(base_location) => {
            let n = runtime.resolver_counter.fetch_add(1, Ordering::SeqCst) + 1;
            // A fresh signature restores CDN access: the response allowance is
            // scoped to the signed URL that the resolver just issued.
            runtime.cdn_success_responses.store(0, Ordering::SeqCst);
            let sep = if base_location.contains('?') {
                '&'
            } else {
                '?'
            };
            let location = format!("{base_location}{sep}token=fresh-{n}");
            let resp = format!(
                "HTTP/1.1 302 Found\r\nLocation: {}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                location
            );
            stream.write_all(resp.as_bytes())
        }
        ResolverBehavior::Bytes(body) => {
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(resp.as_bytes())?;
            stream.write_all(body)
        }
    }
}

fn respond_cdn(
    stream: &mut TcpStream,
    config: &HfFixtureConfig,
    runtime: &Arc<RuntimeState>,
    headers: &HashMap<String, String>,
) -> (std::io::Result<()>, bool) {
    if config.cdn_persistent_403 {
        return (write_simple(stream, 403, "Forbidden", b"forbidden"), false);
    }
    if let Some(threshold) = config.cdn_allow_after_resolver_redirects_ge {
        let redirects = runtime.resolver_counter.load(Ordering::SeqCst);
        if redirects < threshold {
            return (write_simple(stream, 403, "Forbidden", b"not ready"), false);
        }
    }
    if let Some(limit) = config.cdn_persistent_403_after_resolver_redirects {
        let redirects = runtime.resolver_counter.load(Ordering::SeqCst);
        if redirects >= limit {
            return (
                write_simple(stream, 403, "Forbidden", b"signature expired"),
                false,
            );
        }
    }
    if let Some(limit) = config.cdn_forbidden_after_successes {
        let successes = runtime.cdn_success_responses.load(Ordering::SeqCst);
        if successes >= limit {
            return (
                write_simple(stream, 403, "Forbidden", b"signature expired"),
                false,
            );
        }
    }
    if let Some(retry_after) = &config.cdn_429_retry_after_once {
        if !runtime.cdn_429_served.swap(true, Ordering::SeqCst) {
            let body = b"rate limited";
            let resp = format!(
                "HTTP/1.1 429 Too Many Requests\r\nRetry-After: {}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                retry_after,
                body.len()
            );
            let r = (|| {
                stream.write_all(resp.as_bytes())?;
                stream.write_all(body)
            })();
            return (r, true);
        }
    }

    let body = &config.cdn_body;
    let total = body.len() as u64;
    let range = headers.get("range").cloned();
    let parsed_range = range.as_deref().and_then(|r| parse_single_range(r, total));

    let honor_range = match &config.cdn_range_mode {
        CdnRangeMode::Honor => true,
        CdnRangeMode::RejectAlways => false,
        CdnRangeMode::HonorProbeThenIgnoreAfterBytes(after_bytes) => {
            if range.as_deref() == Some("bytes=0-0") {
                true
            } else {
                runtime.cdn_bytes_served.load(Ordering::SeqCst) < *after_bytes
            }
        }
    };

    if honor_range {
        if let Some((start, end)) = parsed_range {
            let start_usize = start as usize;
            let end_usize = end as usize;
            let slice = &body[start_usize..=end_usize];
            let resp = format!(
                "HTTP/1.1 206 Partial Content\r\nAccept-Ranges: bytes\r\nContent-Type: application/octet-stream\r\nContent-Range: bytes {}-{}/{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                start,
                end,
                total,
                slice.len()
            );
            runtime.cdn_success_responses.fetch_add(1, Ordering::SeqCst);
            runtime
                .cdn_bytes_served
                .fetch_add(slice.len() as u64, Ordering::SeqCst);
            if let Err(e) = stream.write_all(resp.as_bytes()) {
                return (Err(e), false);
            }
            if let Err(e) = write_body(
                stream,
                slice,
                config.cdn_stream_delay,
                config.cdn_stream_chunk_size,
            ) {
                return (Err(e), false);
            }
            return (Ok(()), false);
        }
    }

    let resp = format!(
        "HTTP/1.1 200 OK\r\nAccept-Ranges: bytes\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    runtime.cdn_success_responses.fetch_add(1, Ordering::SeqCst);
    runtime
        .cdn_bytes_served
        .fetch_add(body.len() as u64, Ordering::SeqCst);
    if let Err(e) = stream.write_all(resp.as_bytes()) {
        return (Err(e), false);
    }
    let result = write_body(
        stream,
        body,
        config.cdn_stream_delay,
        config.cdn_stream_chunk_size,
    );
    (result.map(|_| ()), false)
}

fn write_simple(
    stream: &mut TcpStream,
    status: u16,
    reason: &str,
    body: &[u8],
) -> std::io::Result<()> {
    let resp = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        status,
        reason,
        body.len()
    );
    stream.write_all(resp.as_bytes())?;
    stream.write_all(body)
}

fn write_body(
    stream: &mut TcpStream,
    body: &[u8],
    delay: Option<Duration>,
    chunk_size: usize,
) -> std::io::Result<usize> {
    if body.is_empty() {
        return Ok(0);
    }
    let mut written = 0usize;
    if let Some(delay) = delay {
        let chunk_size = chunk_size.max(1);
        let mut chunks = body.chunks(chunk_size).peekable();
        while let Some(chunk) = chunks.next() {
            stream.write_all(chunk)?;
            let _ = stream.flush();
            written += chunk.len();
            if chunks.peek().is_some() {
                thread::sleep(delay);
            }
        }
    } else {
        stream.write_all(body)?;
        written = body.len();
    }
    Ok(written)
}

fn split_absolute_target(target: &str) -> Option<(String, String)> {
    let rest = target
        .strip_prefix("http://")
        .or_else(|| target.strip_prefix("https://"))?;
    let scheme_len = target.len() - rest.len();
    let _scheme = &target[..scheme_len];
    let (authority, path) = match rest.find('/') {
        Some(index) => (&rest[..index], &rest[index..]),
        None => (rest, "/"),
    };
    if authority.is_empty() {
        return None;
    }
    // Strip userinfo if any (not expected here).
    let host = authority
        .rsplit('@')
        .next()
        .unwrap_or(authority)
        .to_string();
    Some((host, path.to_string()))
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
    if start >= total || end < start {
        return None;
    }
    Some((start, end.min(total.saturating_sub(1))))
}
