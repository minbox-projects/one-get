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
use std::sync::atomic::{AtomicBool, Ordering};
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
}

#[derive(Clone, Debug)]
pub enum ResolverBehavior {
    /// Redirect resolve/blob requests to this absolute CDN URL.
    RedirectToCdn(String),
    /// Reject gated resolver requests.
    Unauthorized,
    /// Serve bytes directly (not typically used for HF, but flexible).
    Bytes(Vec<u8>),
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
}

impl Default for HfFixtureConfig {
    fn default() -> Self {
        Self {
            resolver_behavior: ResolverBehavior::Unauthorized,
            cdn_body: Vec::new(),
            cdn_stream_delay: None,
            cdn_stream_chunk_size: 16 * 1024,
            cdn_concurrency_limit: None,
        }
    }
}

#[derive(Default)]
struct SharedState {
    resolver_requests: Vec<RecordedRequest>,
    cdn_requests: Vec<RecordedRequest>,
    active_cdn: usize,
    max_concurrent_cdn: usize,
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
                    thread::spawn(move || {
                        let _ = handle_connection(stream, &cfg, &st, &gate);
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
        return respond_resolver(&mut stream, config);
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
    }
    let result = respond_cdn(&mut stream, config, &headers);
    {
        let mut st = state.lock().expect("lock hf state");
        st.active_cdn = st.active_cdn.saturating_sub(1);
    }
    gate.exit();
    result
}

fn respond_resolver(stream: &mut TcpStream, config: &HfFixtureConfig) -> std::io::Result<()> {
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
    headers: &HashMap<String, String>,
) -> std::io::Result<()> {
    let body = &config.cdn_body;
    let total = body.len() as u64;
    let range = headers.get("range").cloned();
    if let Some((start, end)) = range.as_deref().and_then(|r| parse_single_range(r, total)) {
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
        stream.write_all(resp.as_bytes())?;
        write_body(
            stream,
            slice,
            config.cdn_stream_delay,
            config.cdn_stream_chunk_size,
        )?;
        return Ok(());
    }

    let resp = format!(
        "HTTP/1.1 200 OK\r\nAccept-Ranges: bytes\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(resp.as_bytes())?;
    write_body(
        stream,
        body,
        config.cdn_stream_delay,
        config.cdn_stream_chunk_size,
    )
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
