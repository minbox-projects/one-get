//! Deterministic loopback HTTP fixture for signed component catalog fetches.
//!
//! Binds `127.0.0.1:0`, serves an in-memory route table, counts requests, and
//! shuts down cleanly on Drop. No public network is contacted.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const CATALOG_PATH: &str = "/catalog.json";

/// Route table and behavior switches for a [`KernelServer`].
#[derive(Clone, Debug)]
pub struct KernelServerConfig {
    routes: HashMap<String, Vec<u8>>,
    delay: Option<Duration>,
    oversized_catalog_len: Option<usize>,
}

impl KernelServerConfig {
    /// Serve `catalog` at `/catalog.json`.
    pub fn new(catalog: impl Into<Vec<u8>>) -> Self {
        let mut routes = HashMap::new();
        routes.insert(CATALOG_PATH.to_string(), catalog.into());
        Self {
            routes,
            delay: None,
            oversized_catalog_len: None,
        }
    }

    /// Serve `bytes` at `/artifacts/<name>`.
    pub fn artifact(mut self, name: &str, bytes: impl Into<Vec<u8>>) -> Self {
        self.routes
            .insert(format!("/artifacts/{name}"), bytes.into());
        self
    }

    /// Delay every response by `delay` after the request is recorded.
    pub fn with_delay(mut self, delay: Duration) -> Self {
        self.delay = Some(delay);
        self
    }

    /// Replace the `/catalog.json` body with `len` filler bytes.
    pub fn with_oversized_catalog(mut self, len: usize) -> Self {
        self.oversized_catalog_len = Some(len);
        self
    }
}

#[derive(Default)]
struct ServerState {
    requests: usize,
}

/// A running loopback fixture server.
pub struct KernelServer {
    addr: String,
    state: Arc<Mutex<ServerState>>,
    shutdown: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl KernelServer {
    /// Bind `127.0.0.1:0` and serve `config` in a background thread.
    pub fn start(config: KernelServerConfig) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind kernel fixture");
        let addr = listener.local_addr().expect("local addr").to_string();
        listener
            .set_nonblocking(true)
            .expect("set nonblocking listener");
        let state = Arc::new(Mutex::new(ServerState::default()));
        let shutdown = Arc::new(AtomicBool::new(false));
        let config = Arc::new(config);
        let state_for_loop = Arc::clone(&state);
        let shutdown_for_loop = Arc::clone(&shutdown);
        let join = thread::spawn(move || loop {
            if shutdown_for_loop.load(Ordering::SeqCst) {
                break;
            }
            match listener.accept() {
                Ok((stream, _)) => {
                    let cfg = Arc::clone(&config);
                    let st = Arc::clone(&state_for_loop);
                    let sd = Arc::clone(&shutdown_for_loop);
                    thread::spawn(move || {
                        let _ = handle_connection(stream, &cfg, &st, &sd);
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(5));
                }
                Err(_) => {
                    if shutdown_for_loop.load(Ordering::SeqCst) {
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

    /// Base URL such as `http://127.0.0.1:54321`.
    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// URL of the served signed catalog.
    pub fn catalog_url(&self) -> String {
        format!("{}{}", self.base_url(), CATALOG_PATH)
    }

    /// URL of a served artifact route.
    pub fn artifact_url(&self, name: &str) -> String {
        format!("{}/artifacts/{}", self.base_url(), name)
    }

    /// Number of requests the fixture has accepted so far.
    pub fn request_count(&self) -> usize {
        self.state.lock().expect("lock").requests
    }
}

impl Drop for KernelServer {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        // Wake the non-blocking accept loop so it observes the shutdown flag.
        if let Ok(stream) = TcpStream::connect(&self.addr) {
            let _ = stream.shutdown(Shutdown::Both);
        }
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn handle_connection(
    mut stream: TcpStream,
    config: &KernelServerConfig,
    state: &Arc<Mutex<ServerState>>,
    shutdown: &AtomicBool,
) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
    let _ = stream.set_nonblocking(false);
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => {
                buf.extend_from_slice(&chunk[..read]);
                if buf.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
                if buf.len() > 64 * 1024 {
                    break;
                }
            }
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || error.kind() == std::io::ErrorKind::TimedOut =>
            {
                break;
            }
            Err(_) => break,
        }
    }

    let request_text = String::from_utf8_lossy(&buf);
    let request_line = request_text.lines().next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let _method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("/");
    let path = target.split('?').next().unwrap_or(target).to_string();

    {
        let mut current = state.lock().expect("lock");
        current.requests = current.requests.saturating_add(1);
    }

    if let Some(delay) = config.delay {
        if !sleep_interruptible(delay, shutdown) {
            return Ok(());
        }
    }

    let body = if path == CATALOG_PATH {
        match config.oversized_catalog_len {
            Some(len) => vec![b'a'; len],
            None => match config.routes.get(&path) {
                Some(body) => body.clone(),
                None => return write_not_found(&mut stream),
            },
        }
    } else {
        match config.routes.get(&path) {
            Some(body) => body.clone(),
            None => return write_not_found(&mut stream),
        }
    };

    let header = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(header.as_bytes())?;
    stream.write_all(&body)
}

fn write_not_found(stream: &mut TcpStream) -> std::io::Result<()> {
    stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
}

fn sleep_interruptible(delay: Duration, shutdown: &AtomicBool) -> bool {
    let deadline = Instant::now() + delay;
    while Instant::now() < deadline {
        if shutdown.load(Ordering::SeqCst) {
            return false;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        thread::sleep(remaining.min(Duration::from_millis(10)));
    }
    !shutdown.load(Ordering::SeqCst)
}
