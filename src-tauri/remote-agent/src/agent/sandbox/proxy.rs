//! The proxy a cell's processes reach the network through: HTTP (`CONNECT`,
//! and plain requests in absolute form) and SOCKS5 (`CONNECT`), on one port —
//! the first byte of a connection tells them apart, as in sandbox-runtime.
//!
//! The proxy itself decides nothing. Every destination goes to a [`Dial`],
//! which is the network policy outside the sandbox: on macOS and Windows the
//! proxy runs there and dials directly; on Linux it runs inside the cell's
//! network namespace and dials by asking the agent outside (see
//! `agent::tunnel`). Either way what a process can reach is decided where it
//! cannot interfere.
//!
//! A token in the proxy's credentials (`http://mework:<token>@…`) ties a
//! connection to its cell, for a proxy shared by several cells, and keeps
//! other local programs from borrowing one.

use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

pub use crate::tunnel::{relay, Outbound, Upstream};

/// Longest request head a client may send before the proxy gives up on it.
const MAX_HEAD: usize = 64 * 1024;
/// How long a client has to say where it wants to go.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);
/// Most connections the proxy serves at once.
const MAX_CONNECTIONS: usize = crate::tunnel::MAX_CONNECTIONS;

/// Why a destination could not be reached.
#[derive(Clone, Debug)]
pub struct DialError {
    /// The policy refused it, as opposed to the network failing.
    pub refused: bool,
    pub message: String,
}

/// Opens connections for the proxy, by policy.
pub trait Dial: Send + Sync + 'static {
    fn dial(&self, host: &str, port: u16) -> Result<Upstream, DialError>;
}

/// Which dialer, if any, a client presenting a proxy password gets: the
/// policy of the cell that password belongs to. `None` for a proxy without
/// credentials.
pub type Authorize = Arc<dyn Fn(Option<&str>) -> Option<Arc<dyn Dial>> + Send + Sync>;

/// A running proxy. Dropping it stops accepting; connections already open
/// run to their end.
pub struct Proxy {
    port: u16,
    stopped: Arc<AtomicBool>,
}

impl Proxy {
    /// Serves `listener` until dropped. With a `token`, a client must present
    /// it as its proxy password.
    pub fn start(listener: TcpListener, token: Option<String>, dialer: Arc<dyn Dial>) -> io::Result<Self> {
        let authorize: Authorize = Arc::new(move |presented: Option<&str>| match (&token, presented) {
            (None, _) => Some(Arc::clone(&dialer)),
            (Some(token), Some(presented)) if constant_time_eq(token.as_bytes(), presented.as_bytes()) => {
                Some(Arc::clone(&dialer))
            }
            _ => None,
        });
        Self::start_authorized(listener, authorize)
    }

    /// Serves `listener` for several cells at once, each known by its own
    /// password.
    pub fn start_authorized(listener: TcpListener, authorize: Authorize) -> io::Result<Self> {
        let port = listener.local_addr()?.port();
        let stopped = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&stopped);
        let open = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        std::thread::Builder::new()
            .name("sandbox-proxy".into())
            .spawn(move || {
                for client in listener.incoming() {
                    if stop.load(Ordering::SeqCst) {
                        return;
                    }
                    let Ok(client) = client else {
                        std::thread::sleep(Duration::from_millis(50));
                        continue;
                    };
                    // Each connection is a thread or two here; a sandbox
                    // does not get to decide how many.
                    if open.load(Ordering::SeqCst) >= MAX_CONNECTIONS {
                        drop(client);
                        continue;
                    }
                    let slot = Slot::take(&open);
                    let authorize = Arc::clone(&authorize);
                    let _ = std::thread::Builder::new()
                        .name("sandbox-proxy-conn".into())
                        .spawn(move || {
                            let _slot = slot;
                            serve(client, &authorize);
                        });
                }
            })?;
        Ok(Self { port, stopped })
    }

    pub fn port(&self) -> u16 {
        self.port
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::SeqCst);
        // Wakes the accepting thread so it sees the flag.
        let _ = TcpStream::connect_timeout(&([127, 0, 0, 1], self.port).into(), Duration::from_millis(200));
    }
}

/// One of the proxy's connections, counted while it lives — and given back
/// even when its thread never started.
struct Slot(Arc<std::sync::atomic::AtomicUsize>);

impl Slot {
    fn take(open: &Arc<std::sync::atomic::AtomicUsize>) -> Self {
        open.fetch_add(1, Ordering::SeqCst);
        Self(Arc::clone(open))
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// The environment that points a process at the proxy. `own_loopback`: the
/// cell has a loopback of its own (Linux), where its processes talk to each
/// other directly; otherwise loopback is the machine's, closed to the cell,
/// and reached through the proxy like any other destination — only where the
/// policy names it.
pub fn environment(port: u16, token: &str, own_loopback: bool) -> Vec<(String, String)> {
    let http = format!("http://mework:{token}@127.0.0.1:{port}");
    let socks = format!("socks5h://mework:{token}@127.0.0.1:{port}");
    let mut env = Vec::new();
    for name in ["HTTP_PROXY", "HTTPS_PROXY", "http_proxy", "https_proxy", "GRPC_PROXY", "grpc_proxy"] {
        env.push((name.to_owned(), http.clone()));
    }
    for name in ["ALL_PROXY", "all_proxy", "FTP_PROXY", "ftp_proxy"] {
        env.push((name.to_owned(), socks.clone()));
    }
    let bypass = if own_loopback { "localhost,127.0.0.1,::1" } else { "" };
    for name in ["NO_PROXY", "no_proxy"] {
        env.push((name.to_owned(), bypass.to_owned()));
    }
    // Node's built-in fetch reads the proxy variables only when asked to.
    env.push(("NODE_USE_ENV_PROXY".to_owned(), "1".to_owned()));
    env
}

fn serve(mut client: TcpStream, authorize: &Authorize) {
    let _ = client.set_read_timeout(Some(HANDSHAKE_TIMEOUT));
    let _ = client.set_nodelay(true);
    let mut first = [0u8; 1];
    if client.read_exact(&mut first).is_err() {
        return;
    }
    let upstream = if first[0] == 5 {
        socks5(&mut client, authorize)
    } else {
        http(&mut client, first[0], authorize)
    };
    let Some((upstream, pending)) = upstream else {
        return;
    };
    let _ = client.set_read_timeout(None);
    relay(client, pending, upstream);
}

fn socks5(client: &mut TcpStream, authorize: &Authorize) -> Option<(Upstream, Vec<u8>)> {
    let mut count = [0u8; 1];
    client.read_exact(&mut count).ok()?;
    let mut methods = vec![0u8; count[0] as usize];
    client.read_exact(&mut methods).ok()?;
    // Without credentials only a proxy that asks for none lets a client in.
    let dialer = if methods.contains(&2) {
        client.write_all(&[5, 2]).ok()?;
        let mut version = [0u8; 2];
        client.read_exact(&mut version).ok()?;
        let mut user = vec![0u8; version[1] as usize];
        client.read_exact(&mut user).ok()?;
        let mut length = [0u8; 1];
        client.read_exact(&mut length).ok()?;
        let mut password = vec![0u8; length[0] as usize];
        client.read_exact(&mut password).ok()?;
        let password = String::from_utf8(password).ok();
        match password.as_deref().and_then(|password| authorize(Some(password))) {
            Some(dialer) => {
                client.write_all(&[1, 0]).ok()?;
                dialer
            }
            None => {
                let _ = client.write_all(&[1, 1]);
                return None;
            }
        }
    } else if methods.contains(&0) {
        match authorize(None) {
            Some(dialer) => {
                client.write_all(&[5, 0]).ok()?;
                dialer
            }
            None => {
                let _ = client.write_all(&[5, 0xff]);
                return None;
            }
        }
    } else {
        let _ = client.write_all(&[5, 0xff]);
        return None;
    };
    let mut request = [0u8; 4];
    client.read_exact(&mut request).ok()?;
    if request[0] != 5 || request[1] != 1 {
        // Only CONNECT: no BIND, no UDP.
        let _ = client.write_all(&[5, 7, 0, 1, 0, 0, 0, 0, 0, 0]);
        return None;
    }
    let host = match request[3] {
        1 => {
            let mut address = [0u8; 4];
            client.read_exact(&mut address).ok()?;
            std::net::Ipv4Addr::from(address).to_string()
        }
        3 => {
            let mut length = [0u8; 1];
            client.read_exact(&mut length).ok()?;
            let mut name = vec![0u8; length[0] as usize];
            client.read_exact(&mut name).ok()?;
            String::from_utf8(name).ok()?
        }
        4 => {
            let mut address = [0u8; 16];
            client.read_exact(&mut address).ok()?;
            format!("[{}]", std::net::Ipv6Addr::from(address))
        }
        _ => {
            let _ = client.write_all(&[5, 8, 0, 1, 0, 0, 0, 0, 0, 0]);
            return None;
        }
    };
    let mut port = [0u8; 2];
    client.read_exact(&mut port).ok()?;
    let port = u16::from_be_bytes(port);
    match dialer.as_ref().dial(&host, port) {
        Ok(upstream) => {
            client.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]).ok()?;
            Some((upstream, Vec::new()))
        }
        Err(error) => {
            // 2: not allowed by the rule set; 5: refused by the destination.
            let code = if error.refused { 2 } else { 5 };
            let _ = client.write_all(&[5, code, 0, 1, 0, 0, 0, 0, 0, 0]);
            None
        }
    }
}

fn http(client: &mut TcpStream, first: u8, authorize: &Authorize) -> Option<(Upstream, Vec<u8>)> {
    let mut data = vec![first];
    let head_end = loop {
        if let Some(at) = find(&data, b"\r\n\r\n") {
            break at + 4;
        }
        if data.len() > MAX_HEAD {
            respond(client, 431, "Request Header Fields Too Large", "The request head is too large.\n");
            return None;
        }
        let mut chunk = [0u8; 4096];
        let count = client.read(&mut chunk).ok()?;
        if count == 0 {
            return None;
        }
        data.extend_from_slice(&chunk[..count]);
    };
    let pending = data.split_off(head_end);
    let head = String::from_utf8_lossy(&data).into_owned();
    let mut lines = head.split("\r\n");
    let request_line = lines.next()?.to_owned();
    let headers: Vec<(String, String)> = lines
        .filter(|line| !line.is_empty())
        .filter_map(|line| line.split_once(':').map(|(name, value)| (name.trim().to_owned(), value.trim().to_owned())))
        .collect();
    let mut parts = request_line.split_whitespace();
    let (Some(method), Some(target), Some(version)) = (parts.next(), parts.next(), parts.next()) else {
        respond(client, 400, "Bad Request", "Malformed request line.\n");
        return None;
    };
    let presented = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("proxy-authorization"))
        .and_then(|(_, value)| value.strip_prefix("Basic ").or_else(|| value.strip_prefix("basic ")))
        .and_then(|encoded| base64_decode(encoded.trim()))
        .and_then(|decoded| {
            let text = String::from_utf8(decoded).ok()?;
            text.split_once(':').map(|(_, password)| password.to_owned())
        });
    let Some(dialer) = authorize(presented.as_deref()) else {
        let _ = client.write_all(
            b"HTTP/1.1 407 Proxy Authentication Required\r\nProxy-Authenticate: Basic realm=\"mework-sandbox\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        );
        return None;
    };
    let dialer = dialer.as_ref();
    if method.eq_ignore_ascii_case("CONNECT") {
        let (host, port) = split_host_port(target, 443)?;
        return match dialer.dial(&host, port) {
            Ok(upstream) => {
                client
                    .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                    .ok()?;
                Some((upstream, pending))
            }
            Err(error) => {
                refuse(client, &error);
                None
            }
        };
    }
    // A plain request names its destination in absolute form.
    let Some(rest) = target.strip_prefix("http://") else {
        respond(
            client,
            400,
            "Bad Request",
            "This proxy forwards http:// requests in absolute form and tunnels everything else with CONNECT.\n",
        );
        return None;
    };
    let (authority, path) = match rest.find('/') {
        Some(at) => (&rest[..at], &rest[at..]),
        None => (rest, "/"),
    };
    let (host, port) = split_host_port(authority, 80)?;
    let mut upstream = match dialer.dial(&host, port) {
        Ok(upstream) => upstream,
        Err(error) => {
            refuse(client, &error);
            return None;
        }
    };
    let mut forwarded = format!("{method} {path} {version}\r\n");
    for (name, value) in &headers {
        let lower = name.to_ascii_lowercase();
        if matches!(lower.as_str(), "proxy-authorization" | "proxy-connection" | "connection" | "keep-alive") {
            continue;
        }
        forwarded.push_str(&format!("{name}: {value}\r\n"));
    }
    // One request per connection: a client's next request may be for another
    // host, and this proxy does not parse responses to find where one ends.
    forwarded.push_str("Connection: close\r\n\r\n");
    upstream.writer.write_all(forwarded.as_bytes()).ok()?;
    Some((upstream, pending))
}

fn refuse(client: &mut TcpStream, error: &DialError) {
    if error.refused {
        let body = format!("Mework sandbox: {}\n", error.message);
        let response = format!(
            "HTTP/1.1 403 Forbidden\r\nX-Proxy-Error: blocked-by-mework-sandbox\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = client.write_all(response.as_bytes());
    } else {
        respond(client, 502, "Bad Gateway", &format!("Mework sandbox: {}\n", error.message));
    }
}

fn respond(client: &mut TcpStream, code: u16, reason: &str, body: &str) {
    let response = format!(
        "HTTP/1.1 {code} {reason}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = client.write_all(response.as_bytes());
}

/// `host:port`, `[v6]:port` or a bare host with the default port.
fn split_host_port(authority: &str, default_port: u16) -> Option<(String, u16)> {
    let authority = authority.rsplit('@').next()?;
    if let Some(rest) = authority.strip_prefix('[') {
        let (address, tail) = rest.split_once(']')?;
        let port = match tail.strip_prefix(':') {
            Some(port) => port.parse().ok()?,
            None => default_port,
        };
        return Some((format!("[{address}]"), port));
    }
    match authority.rsplit_once(':') {
        Some((host, port)) => Some((host.to_owned(), port.parse().ok()?)),
        None => Some((authority.to_owned(), default_port)),
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|window| window == needle)
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len() && left.iter().zip(right).fold(0u8, |acc, (a, b)| acc | (a ^ b)) == 0
}

fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let mut buffer = 0u32;
    let mut bits = 0;
    for byte in text.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            _ => return None,
        };
        buffer = (buffer << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
            buffer &= (1 << bits) - 1;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Shutdown;
    use std::sync::Mutex;

    /// Dials only `allowed.test`, always to a local echo server, and records
    /// what it was asked for.
    struct Fake {
        echo_port: u16,
        asked: Mutex<Vec<(String, u16)>>,
    }

    impl Dial for Fake {
        fn dial(&self, host: &str, port: u16) -> Result<Upstream, DialError> {
            self.asked.lock().unwrap().push((host.to_owned(), port));
            if host != "allowed.test" {
                return Err(DialError {
                    refused: true,
                    message: format!("{host} is not allowed"),
                });
            }
            let stream = TcpStream::connect(("127.0.0.1", self.echo_port)).unwrap();
            Ok(Upstream::tcp(stream).unwrap())
        }
    }

    fn echo_server() -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                std::thread::spawn(move || {
                    let mut reader = stream.try_clone().unwrap();
                    let mut writer = stream;
                    let _ = io::copy(&mut reader, &mut writer);
                    let _ = writer.shutdown(Shutdown::Write);
                });
            }
        });
        port
    }

    fn proxy(token: Option<&str>) -> (Proxy, Arc<Fake>) {
        let fake = Arc::new(Fake {
            echo_port: echo_server(),
            asked: Mutex::new(Vec::new()),
        });
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let proxy = Proxy::start(listener, token.map(str::to_owned), Arc::clone(&fake) as Arc<dyn Dial>).unwrap();
        (proxy, fake)
    }

    fn basic(token: &str) -> String {
        const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let input = format!("mework:{token}").into_bytes();
        let mut out = String::new();
        for chunk in input.chunks(3) {
            let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
            let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
            for i in 0..4 {
                if i <= chunk.len() {
                    out.push(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize] as char);
                } else {
                    out.push('=');
                }
            }
        }
        out
    }

    #[test]
    fn connect_tunnels_to_an_allowed_host_and_refuses_the_rest() {
        let (proxy, fake) = proxy(Some("s3cret"));
        let mut client = TcpStream::connect(("127.0.0.1", proxy.port())).unwrap();
        write!(
            client,
            "CONNECT allowed.test:443 HTTP/1.1\r\nHost: allowed.test:443\r\nProxy-Authorization: Basic {}\r\n\r\nhello",
            basic("s3cret")
        )
        .unwrap();
        client.shutdown(Shutdown::Write).unwrap();
        let mut answer = String::new();
        client.read_to_string(&mut answer).unwrap();
        assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
        assert!(answer.ends_with("\r\n\r\nhello"), "{answer}");

        let mut refused = TcpStream::connect(("127.0.0.1", proxy.port())).unwrap();
        write!(
            refused,
            "CONNECT evil.test:443 HTTP/1.1\r\nProxy-Authorization: Basic {}\r\n\r\n",
            basic("s3cret")
        )
        .unwrap();
        let mut answer = String::new();
        refused.read_to_string(&mut answer).unwrap();
        assert!(answer.starts_with("HTTP/1.1 403"), "{answer}");
        assert!(answer.contains("evil.test is not allowed"), "{answer}");
        assert_eq!(fake.asked.lock().unwrap().len(), 2);
    }

    #[test]
    fn a_client_without_the_token_is_turned_away_before_anything_is_dialed() {
        let (proxy, fake) = proxy(Some("s3cret"));
        let mut client = TcpStream::connect(("127.0.0.1", proxy.port())).unwrap();
        client.write_all(b"CONNECT allowed.test:443 HTTP/1.1\r\n\r\n").unwrap();
        let mut answer = String::new();
        client.read_to_string(&mut answer).unwrap();
        assert!(answer.starts_with("HTTP/1.1 407"), "{answer}");
        let mut wrong = TcpStream::connect(("127.0.0.1", proxy.port())).unwrap();
        write!(wrong, "CONNECT allowed.test:443 HTTP/1.1\r\nProxy-Authorization: Basic {}\r\n\r\n", basic("nope")).unwrap();
        let mut answer = String::new();
        wrong.read_to_string(&mut answer).unwrap();
        assert!(answer.starts_with("HTTP/1.1 407"), "{answer}");
        assert!(fake.asked.lock().unwrap().is_empty());
    }

    #[test]
    fn a_plain_request_is_forwarded_in_origin_form_without_proxy_headers() {
        let (proxy, _) = proxy(None);
        let mut client = TcpStream::connect(("127.0.0.1", proxy.port())).unwrap();
        client
            .write_all(b"GET http://allowed.test/path?q=1 HTTP/1.1\r\nHost: allowed.test\r\nProxy-Connection: keep-alive\r\n\r\n")
            .unwrap();
        client.shutdown(Shutdown::Write).unwrap();
        let mut answer = String::new();
        client.read_to_string(&mut answer).unwrap();
        // The echo server returns what the proxy sent it.
        assert!(answer.starts_with("GET /path?q=1 HTTP/1.1\r\n"), "{answer}");
        assert!(answer.contains("Host: allowed.test\r\n"));
        assert!(answer.contains("Connection: close\r\n"));
        assert!(!answer.to_ascii_lowercase().contains("proxy-connection"));
    }

    #[test]
    fn socks5_connects_by_name_with_the_token_as_password() {
        let (proxy, fake) = proxy(Some("tok"));
        let mut client = TcpStream::connect(("127.0.0.1", proxy.port())).unwrap();
        client.write_all(&[5, 1, 2]).unwrap();
        let mut reply = [0u8; 2];
        client.read_exact(&mut reply).unwrap();
        assert_eq!(reply, [5, 2]);
        client.write_all(&[1, 6]).unwrap();
        client.write_all(b"mework").unwrap();
        client.write_all(&[3]).unwrap();
        client.write_all(b"tok").unwrap();
        client.read_exact(&mut reply).unwrap();
        assert_eq!(reply, [1, 0]);
        let name = b"allowed.test";
        let mut request = vec![5, 1, 0, 3, name.len() as u8];
        request.extend_from_slice(name);
        request.extend_from_slice(&443u16.to_be_bytes());
        client.write_all(&request).unwrap();
        let mut answer = [0u8; 10];
        client.read_exact(&mut answer).unwrap();
        assert_eq!(answer[1], 0);
        client.write_all(b"ping").unwrap();
        let mut echoed = [0u8; 4];
        client.read_exact(&mut echoed).unwrap();
        assert_eq!(&echoed, b"ping");
        assert_eq!(fake.asked.lock().unwrap()[0], ("allowed.test".to_owned(), 443));
    }

    #[test]
    fn base64_decodes_basic_credentials() {
        assert_eq!(base64_decode("bWV3b3JrOnRvaw==").unwrap(), b"mework:tok");
        assert_eq!(base64_decode("YQ").unwrap(), b"a");
        assert!(base64_decode("not base64!").is_none());
    }
}
