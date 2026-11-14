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

// ---------------------------------------------------------------------------
// Hand-crafted stored-method ZIP builder.
//
// The `zip` crate is intentionally not a test dependency, so archives are
// assembled byte by byte: a local file header per entry, a central directory,
// and an end-of-central-directory record. Every entry uses compression method
// 0 (stored) with a correct CRC-32 over the real data, which keeps the archive
// valid while leaving the declared uncompressed size independently
// overridable for compression-ratio metadata cases.
// ---------------------------------------------------------------------------

const ZIP_LOCAL_HEADER_SIGNATURE: u32 = 0x0403_4b50;
const ZIP_CENTRAL_HEADER_SIGNATURE: u32 = 0x0201_4b50;
const ZIP_EOCD_SIGNATURE: u32 = 0x0605_4b50;
/// Unix host (3) and ZIP specification version 2.0 (20) in the high/low bytes.
const ZIP_VERSION_MADE_BY_UNIX: u16 = 0x0314;

/// One member of a hand-crafted stored-method ZIP archive.
#[derive(Clone, Debug)]
pub struct ZipEntry {
    /// Member name exactly as written into the archive.
    pub name: String,
    /// Member bytes.
    pub data: Vec<u8>,
    /// Unix mode stored in the external attributes, for example `0o100644`.
    pub unix_mode: u32,
    /// Declared uncompressed size, defaulting to the real data length.
    pub declared_uncompressed_size: Option<u32>,
}

impl ZipEntry {
    /// A regular file member.
    pub fn file(name: impl Into<String>, data: impl Into<Vec<u8>>) -> Self {
        Self {
            name: name.into(),
            data: data.into(),
            unix_mode: 0o100_644,
            declared_uncompressed_size: None,
        }
    }

    /// A symbolic-link member whose data is the link target.
    pub fn symlink(name: impl Into<String>, target: impl Into<Vec<u8>>) -> Self {
        Self {
            name: name.into(),
            data: target.into(),
            unix_mode: 0o120_777,
            declared_uncompressed_size: None,
        }
    }

    /// A directory member.
    pub fn directory(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            data: Vec::new(),
            unix_mode: 0o040_755,
            declared_uncompressed_size: None,
        }
    }

    /// Override the declared uncompressed size without changing stored bytes.
    pub fn with_declared_uncompressed_size(mut self, size: u32) -> Self {
        self.declared_uncompressed_size = Some(size);
        self
    }
}

/// Build a valid stored-method ZIP archive from `entries`.
///
/// Entries are written in order, so duplicate member names are preserved for
/// duplicate-entry cases. Callers must stay within the `u16` entry/name limits
/// of the ZIP format.
pub fn build_stored_zip(entries: &[ZipEntry]) -> Vec<u8> {
    assert!(
        entries.len() <= u16::MAX as usize,
        "stored zip fixture supports at most 65535 entries"
    );
    let mut out = Vec::new();
    let mut central = Vec::new();
    let mut offsets = Vec::with_capacity(entries.len());

    for entry in entries {
        let name = entry.name.as_bytes();
        assert!(
            name.len() <= u16::MAX as usize,
            "stored zip fixture member names must fit the ZIP name length"
        );
        let name_len = name.len() as u16;
        let crc = crc32(&entry.data);
        let compressed_size = entry.data.len() as u32;
        let uncompressed_size = entry.declared_uncompressed_size.unwrap_or(compressed_size);
        let offset = out.len() as u32;
        offsets.push(offset);

        push_u32(&mut out, ZIP_LOCAL_HEADER_SIGNATURE);
        push_u16(&mut out, 20);
        push_u16(&mut out, 0);
        push_u16(&mut out, 0);
        push_u16(&mut out, 0);
        push_u16(&mut out, 0x0021);
        push_u32(&mut out, crc);
        push_u32(&mut out, compressed_size);
        push_u32(&mut out, uncompressed_size);
        push_u16(&mut out, name_len);
        push_u16(&mut out, 0);
        out.extend_from_slice(name);
        out.extend_from_slice(&entry.data);
    }

    let central_offset = out.len() as u32;
    for (entry, offset) in entries.iter().zip(offsets) {
        let name = entry.name.as_bytes();
        let name_len = name.len() as u16;
        let crc = crc32(&entry.data);
        let compressed_size = entry.data.len() as u32;
        let uncompressed_size = entry.declared_uncompressed_size.unwrap_or(compressed_size);

        push_u32(&mut central, ZIP_CENTRAL_HEADER_SIGNATURE);
        push_u16(&mut central, ZIP_VERSION_MADE_BY_UNIX);
        push_u16(&mut central, 20);
        push_u16(&mut central, 0);
        push_u16(&mut central, 0);
        push_u16(&mut central, 0);
        push_u16(&mut central, 0x0021);
        push_u32(&mut central, crc);
        push_u32(&mut central, compressed_size);
        push_u32(&mut central, uncompressed_size);
        push_u16(&mut central, name_len);
        push_u16(&mut central, 0);
        push_u16(&mut central, 0);
        push_u16(&mut central, 0);
        push_u16(&mut central, 0);
        push_u32(&mut central, entry.unix_mode << 16);
        push_u32(&mut central, offset);
        central.extend_from_slice(name);
    }

    let central_size = central.len() as u32;
    out.extend_from_slice(&central);

    push_u32(&mut out, ZIP_EOCD_SIGNATURE);
    push_u16(&mut out, 0);
    push_u16(&mut out, 0);
    push_u16(&mut out, entries.len() as u16);
    push_u16(&mut out, entries.len() as u16);
    push_u32(&mut out, central_size);
    push_u32(&mut out, central_offset);
    push_u16(&mut out, 0);
    out
}

fn push_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn push_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

/// CRC-32 (IEEE 802.3) as required by the ZIP format.
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}
