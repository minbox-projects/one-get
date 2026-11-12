#![allow(dead_code)] // Shared fixture helpers are consumed by different integration targets.

//! Deterministic local HTTP forward proxy with optional basic authentication.
//!
//! The proxy accepts absolute-form HTTP requests, checks `Proxy-Authorization`,
//! forwards the request to the origin without the proxy credential, and records
//! both the client-visible headers and the headers actually sent to the origin.
//! It binds 127.0.0.1:0 and never contacts a public network.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// Basic-auth credentials the proxy requires.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyCredentials {
    pub username: String,
    pub password: String,
}

/// One request observed by the proxy.
#[derive(Debug, Clone, Default)]
pub struct ProxyRequest {
    pub method: String,
    /// Absolute-form request target supplied by the client.
    pub target: String,
    /// Request headers as received by the proxy, including `Proxy-Authorization`.
    pub headers: HashMap<String, String>,
    /// Headers actually forwarded to the origin, with proxy credentials removed.
    /// `None` means the proxy rejected the request and did not forward it.
    pub forwarded_headers: Option<HashMap<String, String>>,
}

/// A local forward proxy that records every request.
pub struct ProxyServer {
    addr: String,
    expected_authorization: Option<String>,
    state: Arc<Mutex<Vec<ProxyRequest>>>,
    shutdown: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl ProxyServer {
    /// Start a proxy on an ephemeral port. `credentials` enables basic auth.
    pub fn start(credentials: Option<ProxyCredentials>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind proxy");
        let addr = listener.local_addr().expect("proxy local addr").to_string();
        listener
            .set_nonblocking(true)
            .expect("set nonblocking proxy listener");

        let expected_authorization = credentials.as_ref().map(|credentials| {
            format!(
                "Basic {}",
                base64_encode(
                    format!("{}:{}", credentials.username, credentials.password).as_bytes()
                )
            )
        });
        let state = Arc::new(Mutex::new(Vec::new()));
        let shutdown = Arc::new(AtomicBool::new(false));
        let state_clone = Arc::clone(&state);
        let shutdown_clone = Arc::clone(&shutdown);
        let expected_clone = expected_authorization.clone();

        let join = thread::spawn(move || loop {
            if shutdown_clone.load(Ordering::SeqCst) {
                break;
            }
            match listener.accept() {
                Ok((stream, _)) => {
                    let state = Arc::clone(&state_clone);
                    let expected = expected_clone.clone();
                    thread::spawn(move || {
                        let _ = handle_connection(stream, expected.as_deref(), &state);
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
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
            expected_authorization,
            state,
            shutdown,
            join: Some(join),
        }
    }

    /// Proxy URL without embedded credentials.
    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// `host:port` authority of the proxy listener.
    pub fn addr(&self) -> &str {
        &self.addr
    }

    /// The exact `Proxy-Authorization` value the proxy accepts.
    pub fn expected_authorization(&self) -> Option<&str> {
        self.expected_authorization.as_deref()
    }

    /// Recorded requests, in arrival order.
    pub fn requests(&self) -> Vec<ProxyRequest> {
        self.state.lock().expect("lock proxy state").clone()
    }
}

impl Drop for ProxyServer {
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

fn handle_connection(
    mut stream: TcpStream,
    expected: Option<&str>,
    state: &Arc<Mutex<Vec<ProxyRequest>>>,
) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
    let _ = stream.set_nonblocking(false);

    let request_bytes = read_request_head(&mut stream)?;
    let request_text = String::from_utf8_lossy(&request_bytes);
    let mut lines = request_text.split("\r\n");
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let target = parts.next().unwrap_or("").to_string();

    let mut headers = HashMap::new();
    for line in lines {
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }

    let request_index = {
        let mut requests = state.lock().expect("lock proxy state");
        requests.push(ProxyRequest {
            method: method.clone(),
            target: target.clone(),
            headers: headers.clone(),
            forwarded_headers: None,
        });
        requests.len() - 1
    };

    if let Some(expected) = expected {
        let observed = headers
            .get("proxy-authorization")
            .map(String::as_str)
            .unwrap_or("");
        if observed != expected {
            write_response(
                &mut stream,
                407,
                "Proxy Authentication Required",
                &[("Proxy-Authenticate", "Basic realm=\"oneget\"")],
                b"proxy authentication required",
            )?;
            return Ok(());
        }
    }

    let Some((authority, path)) = split_absolute_target(&target) else {
        write_response(
            &mut stream,
            400,
            "Bad Request",
            &[],
            b"proxy requires an absolute http URL",
        )?;
        return Ok(());
    };

    let mut forwarded = HashMap::new();
    let mut origin_request = format!("{} {} HTTP/1.1\r\n", method, path);
    let mut has_host = false;
    for (name, value) in &headers {
        if matches!(
            name.as_str(),
            "proxy-authorization" | "proxy-connection" | "connection"
        ) {
            continue;
        }
        if name == "host" {
            has_host = true;
        }
        forwarded.insert(name.clone(), value.clone());
        origin_request.push_str(&format!("{}: {}\r\n", name, value));
    }
    if !has_host {
        forwarded.insert("host".to_string(), authority.clone());
        origin_request.push_str(&format!("Host: {}\r\n", authority));
    }
    origin_request.push_str("Connection: close\r\n\r\n");

    {
        let mut requests = state.lock().expect("lock proxy state");
        requests[request_index].forwarded_headers = Some(forwarded);
    }

    let mut origin = match TcpStream::connect(&authority) {
        Ok(stream) => stream,
        Err(_) => {
            write_response(
                &mut stream,
                502,
                "Bad Gateway",
                &[],
                b"proxy could not reach the origin",
            )?;
            return Ok(());
        }
    };
    origin.set_read_timeout(Some(Duration::from_secs(15))).ok();
    let _ = origin.set_nonblocking(false);
    origin.write_all(origin_request.as_bytes())?;
    let _ = origin.flush();

    let mut response = Vec::new();
    let mut buffer = [0u8; 8192];
    loop {
        match origin.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => response.extend_from_slice(&buffer[..read]),
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || error.kind() == std::io::ErrorKind::TimedOut =>
            {
                break;
            }
            Err(_) => break,
        }
    }

    stream.write_all(&response)
}

fn read_request_head(stream: &mut TcpStream) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 4096];
    loop {
        match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => {
                bytes.extend_from_slice(&buffer[..read]);
                if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
                if bytes.len() > 64 * 1024 {
                    break;
                }
            }
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || error.kind() == std::io::ErrorKind::TimedOut =>
            {
                break;
            }
            Err(error) => return Err(error),
        }
    }
    Ok(bytes)
}

fn write_response(
    stream: &mut TcpStream,
    status: u16,
    reason: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> std::io::Result<()> {
    let mut response = format!(
        "HTTP/1.1 {} {}\r\nContent-Length: {}\r\nConnection: close\r\n",
        status,
        reason,
        body.len()
    );
    for (name, value) in headers {
        response.push_str(&format!("{}: {}\r\n", name, value));
    }
    response.push_str("\r\n");
    stream.write_all(response.as_bytes())?;
    stream.write_all(body)
}

fn split_absolute_target(target: &str) -> Option<(String, String)> {
    let rest = target.strip_prefix("http://")?;
    let (authority, path) = match rest.find('/') {
        Some(index) => (&rest[..index], &rest[index..]),
        None => (rest, "/"),
    };
    if authority.is_empty() {
        return None;
    }
    let authority = if authority.contains(':') {
        authority.to_string()
    } else {
        format!("{}:80", authority)
    };
    let path = if path.is_empty() { "/" } else { path };
    Some((authority, path.to_string()))
}

fn base64_encode(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    let mut output = String::new();
    for chunk in input.chunks(3) {
        let first = chunk[0];
        let second = *chunk.get(1).unwrap_or(&0);
        let third = *chunk.get(2).unwrap_or(&0);

        output.push(TABLE[(first >> 2) as usize] as char);
        output.push(TABLE[(((first & 0x03) << 4) | (second >> 4)) as usize] as char);
        if chunk.len() > 1 {
            output.push(TABLE[(((second & 0x0f) << 2) | (third >> 6)) as usize] as char);
        } else {
            output.push('=');
        }
        if chunk.len() > 2 {
            output.push(TABLE[(third & 0x3f) as usize] as char);
        } else {
            output.push('=');
        }
    }
    output
}
