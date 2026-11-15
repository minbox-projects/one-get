#![allow(dead_code)] // Shared fixture helpers are consumed by different integration targets.

//! Deterministic local SOCKS5 proxy fixture with optional RFC 1929
//! username/password authentication.
//!
//! The fixture refuses the no-auth negotiation when credentials are required,
//! performs the username/password handshake, and forwards CONNECT tunnels to a
//! loopback origin. It records the username, the CONNECT target authority, and
//! whether the tunnel was authenticated. It binds 127.0.0.1:0 and never
//! contacts a public network.

use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// SOCKS5 method: no authentication.
const NO_AUTH: u8 = 0x00;
/// SOCKS5 method: username/password authentication (RFC 1929).
const USERPASS: u8 = 0x02;
/// SOCKS5 method selection: no acceptable method.
const NO_ACCEPTABLE: u8 = 0xff;
/// SOCKS5 command: CONNECT.
const CMD_CONNECT: u8 = 0x01;
/// SOCKS5 reply: succeeded.
const REP_OK: u8 = 0x00;
/// SOCKS5 reply: connection refused.
const REP_CONNECTION_REFUSED: u8 = 0x05;

/// RFC 1929 credentials the proxy requires.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Socks5Credentials {
    pub username: String,
    pub password: String,
}

/// One tunnel attempt observed by the SOCKS5 fixture.
#[derive(Debug, Clone, Default)]
pub struct Socks5Tunnel {
    /// Username presented during authentication, when one was supplied.
    pub username: Option<String>,
    /// Target authority of the CONNECT request, `host:port`.
    pub target: Option<String>,
    /// Whether authentication completed and the CONNECT was accepted.
    pub authenticated: bool,
}

/// A local SOCKS5 proxy that records every tunnel attempt.
pub struct Socks5Server {
    addr: String,
    state: Arc<Mutex<Vec<Socks5Tunnel>>>,
    shutdown: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl Socks5Server {
    /// Start a SOCKS5 proxy on an ephemeral port. `credentials` enables RFC 1929
    /// username/password authentication.
    pub fn start(credentials: Option<Socks5Credentials>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind socks5 fixture");
        let addr = listener
            .local_addr()
            .expect("socks5 fixture local addr")
            .to_string();
        listener
            .set_nonblocking(true)
            .expect("set nonblocking socks5 listener");

        let state = Arc::new(Mutex::new(Vec::new()));
        let shutdown = Arc::new(AtomicBool::new(false));
        let state_clone = Arc::clone(&state);
        let shutdown_clone = Arc::clone(&shutdown);

        let join = thread::spawn(move || loop {
            if shutdown_clone.load(Ordering::SeqCst) {
                break;
            }
            match listener.accept() {
                Ok((stream, _)) => {
                    let state = Arc::clone(&state_clone);
                    let credentials = credentials.clone();
                    thread::spawn(move || {
                        let _ = handle_connection(stream, credentials.as_ref(), &state);
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
            state,
            shutdown,
            join: Some(join),
        }
    }

    /// `socks5://host:port` URL of the fixture listener.
    pub fn url(&self) -> String {
        format!("socks5://{}", self.addr)
    }

    /// `host:port` authority of the fixture listener.
    pub fn addr(&self) -> &str {
        &self.addr
    }

    /// Recorded tunnel attempts, in arrival order.
    pub fn tunnels(&self) -> Vec<Socks5Tunnel> {
        self.state.lock().expect("lock socks5 state").clone()
    }
}

impl Drop for Socks5Server {
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
    credentials: Option<&Socks5Credentials>,
    state: &Arc<Mutex<Vec<Socks5Tunnel>>>,
) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
    let _ = stream.set_nonblocking(false);

    // Greeting: VER NMETHODS METHODS...
    let version = read_u8(&mut stream)?;
    if version != 0x05 {
        return Ok(());
    }
    let method_count = read_u8(&mut stream)? as usize;
    let mut methods = vec![0u8; method_count];
    read_exact_n(&mut stream, &mut methods)?;

    let offered_no_auth = methods.contains(&NO_AUTH);
    let offered_userpass = methods.contains(&USERPASS);

    let mut authenticated = false;
    let mut username: Option<String> = None;

    match credentials {
        Some(expected) => {
            if !offered_userpass {
                // Authentication is required; refuse the negotiation otherwise.
                stream.write_all(&[0x05, NO_ACCEPTABLE])?;
                return Ok(());
            }
            stream.write_all(&[0x05, USERPASS])?;

            // RFC 1929: VER ULEN USER PLEN PASS
            let auth_version = read_u8(&mut stream)?;
            if auth_version != 0x01 {
                return Ok(());
            }
            let user_len = read_u8(&mut stream)? as usize;
            let mut user = vec![0u8; user_len];
            read_exact_n(&mut stream, &mut user)?;
            let pass_len = read_u8(&mut stream)? as usize;
            let mut pass = vec![0u8; pass_len];
            read_exact_n(&mut stream, &mut pass)?;

            let user_text = String::from_utf8_lossy(&user).to_string();
            let pass_ok = user_text == expected.username
                && String::from_utf8_lossy(&pass) == expected.password;
            if !pass_ok {
                stream.write_all(&[0x01, 0x01])?;
                state.lock().expect("lock socks5 state").push(Socks5Tunnel {
                    username: Some(user_text),
                    target: None,
                    authenticated: false,
                });
                return Ok(());
            }
            stream.write_all(&[0x01, 0x00])?;
            authenticated = true;
            username = Some(user_text);
        }
        None => {
            if !offered_no_auth {
                stream.write_all(&[0x05, NO_ACCEPTABLE])?;
                return Ok(());
            }
            stream.write_all(&[0x05, NO_AUTH])?;
        }
    }

    // CONNECT request: VER CMD RSV ATYP DST.ADDR DST.PORT
    let request_version = read_u8(&mut stream)?;
    let command = read_u8(&mut stream)?;
    let _reserved = read_u8(&mut stream)?;
    if request_version != 0x05 {
        return Ok(());
    }
    if command != CMD_CONNECT {
        write_reply(&mut stream, 0x07)?;
        return Ok(());
    }
    let address_type = read_u8(&mut stream)?;
    let host = read_address(&mut stream, address_type)?;
    let port_bytes = [read_u8(&mut stream)?, read_u8(&mut stream)?];
    let port = u16::from_be_bytes(port_bytes);
    let target = format!("{host}:{port}");

    let connected = TcpStream::connect(&target);
    match connected {
        Ok(origin) => {
            write_reply(&mut stream, REP_OK)?;
            state.lock().expect("lock socks5 state").push(Socks5Tunnel {
                username,
                target: Some(target),
                authenticated,
            });
            pump_bidirectional(stream, origin);
            Ok(())
        }
        Err(_) => {
            write_reply(&mut stream, REP_CONNECTION_REFUSED)?;
            state.lock().expect("lock socks5 state").push(Socks5Tunnel {
                username,
                target: Some(target),
                authenticated: false,
            });
            Ok(())
        }
    }
}

fn read_address(stream: &mut TcpStream, address_type: u8) -> std::io::Result<String> {
    match address_type {
        0x01 => {
            let mut octets = [0u8; 4];
            read_exact_n(stream, &mut octets)?;
            Ok(format!(
                "{}.{}.{}.{}",
                octets[0], octets[1], octets[2], octets[3]
            ))
        }
        0x03 => {
            let length = read_u8(stream)? as usize;
            let mut domain = vec![0u8; length];
            read_exact_n(stream, &mut domain)?;
            Ok(String::from_utf8_lossy(&domain).to_string())
        }
        0x04 => {
            let mut octets = [0u8; 16];
            read_exact_n(stream, &mut octets)?;
            let segments: Vec<String> = octets
                .chunks(2)
                .map(|pair| format!("{:x}", u16::from_be_bytes([pair[0], pair[1]])))
                .collect();
            Ok(segments.join(":"))
        }
        _ => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "unsupported SOCKS5 address type",
        )),
    }
}

fn read_u8(stream: &mut TcpStream) -> std::io::Result<u8> {
    let mut byte = [0u8; 1];
    read_exact_n(stream, &mut byte)?;
    Ok(byte[0])
}

fn read_exact_n(stream: &mut TcpStream, buffer: &mut [u8]) -> std::io::Result<()> {
    let mut filled = 0;
    while filled < buffer.len() {
        match stream.read(&mut buffer[filled..]) {
            Ok(0) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "the peer closed the connection",
                ))
            }
            Ok(read) => filled += read,
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn write_reply(stream: &mut TcpStream, reply: u8) -> std::io::Result<()> {
    // BND.ADDR is 0.0.0.0:0; the fixture never exposes a real bind address.
    stream.write_all(&[0x05, reply, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
}

fn pump_bidirectional(client: TcpStream, origin: TcpStream) {
    let client_reader = match client.try_clone() {
        Ok(stream) => stream,
        Err(_) => return,
    };
    let origin_writer = match origin.try_clone() {
        Ok(stream) => stream,
        Err(_) => return,
    };
    let forward = thread::spawn(move || pump(client_reader, origin_writer));
    pump(origin, client);
    let _ = forward.join();
}

fn pump(mut from: TcpStream, mut to: TcpStream) {
    let mut buffer = [0u8; 8192];
    loop {
        match from.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => {
                if to.write_all(&buffer[..read]).is_err() {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    let _ = to.shutdown(Shutdown::Write);
}
