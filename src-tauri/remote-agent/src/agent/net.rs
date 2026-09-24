//! `mework-remote net …`: the machine's own network, for the host that cannot see it.
//!
//! A dev server started on this machine listens on *this* machine's loopback,
//! which the host on the other end of the SSH link cannot reach. Everything the
//! host needs to know about that loopback — whether a port is taken, whether a
//! server has started answering — and every byte it needs to send to it, goes
//! through these small verbs, run as ordinary sessions of the daemon:
//!
//! ```text
//! net probe <port>                  {"port":…,"bindable":…,"listening":…}
//! net free-port                     {"port":…}: one the system assigned
//! net wait <port> <ms> [--https]    exits 0 once the port answers, 1 at the deadline
//! net connect <host> <port>         a TCP stream on standard input and output
//! ```
//!
//! Running them as sessions rather than as protocol operations is deliberate:
//! a session already survives a dropped link — its output is kept by offset and
//! its input is resent from where it stopped — so a connection relayed through
//! one outlives a network hiccup exactly the way a command does, and the wire
//! protocol stays what it was.
//!
//! `connect` reports on standard error, one line, before any data moves:
//! `ok` once the connection is up, or `error <kind> <detail>` and exit code 2,
//! where `<kind>` is `refused`, `unreachable`, `timeout`, `resolve` or
//! `invalid`. The host waits for that line, so a refusal is known before it
//! answers whoever asked it for the connection; the data stream itself then
//! carries nothing but the socket's bytes.

use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

/// How long `connect` tries one address.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// How long `connect` gives an address before it tries the next one alongside it, RFC 8305's
/// "connection attempt delay": the first address still wins whenever it answers promptly.
const NEXT_ATTEMPT_DELAY: Duration = Duration::from_millis(250);
/// [`NEXT_ATTEMPT_DELAY`] after a loopback address, where a listening server answers in
/// microseconds. Windows takes two seconds to report a refused loopback connection, so a name
/// like `localhost` that lists `::1` first would otherwise cost a server listening on
/// `127.0.0.1` alone — Flask, Django, most servers that name an address — two seconds for
/// every connection a page opens.
const NEXT_LOOPBACK_ATTEMPT_DELAY: Duration = Duration::from_millis(25);
/// How long a readiness probe waits for one connection attempt.
const PROBE_CONNECT_TIMEOUT: Duration = Duration::from_millis(500);
/// Between readiness attempts: short, because the whole point of waiting here
/// rather than on the host is that each attempt costs no network round trip.
const WAIT_RETRY: Duration = Duration::from_millis(150);
/// How long the HTTP leg of a readiness check may take.
const WAIT_HTTP_TIMEOUT: Duration = Duration::from_secs(2);
/// Bytes moved per read in either direction of a relay.
const RELAY_CHUNK: usize = 64 * 1024;

/// Exit code of a `connect` that never connected.
pub const CONNECT_FAILED: i32 = 2;

pub fn run(arguments: &[String]) -> Result<(), String> {
    match arguments.first().map(String::as_str) {
        Some("probe") => {
            let port = port_argument(arguments.get(1))?;
            let (bindable, listening) = probe(port);
            println!(r#"{{"port":{port},"bindable":{bindable},"listening":{listening}}}"#);
            Ok(())
        }
        Some("free-port") => {
            let port = free_port().map_err(|error| format!("no free port: {error}"))?;
            println!(r#"{{"port":{port}}}"#);
            Ok(())
        }
        Some("wait") => {
            let port = port_argument(arguments.get(1))?;
            let millis: u64 = arguments
                .get(2)
                .ok_or("wait needs a timeout in milliseconds")?
                .parse()
                .map_err(|_| "the timeout is not a number")?;
            let https = arguments.iter().skip(3).any(|argument| argument == "--https");
            if wait_until_ready(port, Duration::from_millis(millis), https) {
                println!("ready");
                Ok(())
            } else {
                println!("timeout");
                std::process::exit(1);
            }
        }
        // `connect -` is a connection started before anyone asked for it: it waits for its target
        // as one line, `<host> <port>`, on standard input. A host keeps one or two of these ready
        // per machine, so a new connection costs the network half a round trip instead of a
        // spawn request and its reply first.
        Some("connect") if arguments.get(1).map(String::as_str) == Some("-") => {
            let line = read_target_line();
            let mut fields = line.split_whitespace();
            let (Some(host), Some(port), None) = (fields.next(), fields.next(), fields.next()) else {
                fail("invalid", "the target line must be `<host> <port>`");
            };
            let Ok(port) = port.parse::<u16>() else {
                fail("invalid", "the port must be 0-65535");
            };
            connect_and_relay(host, port)
        }
        Some("connect") => {
            let host = arguments.get(1).ok_or("connect needs a host")?;
            let port = port_argument(arguments.get(2))?;
            connect_and_relay(host, port)
        }
        _ => Err("usage: mework-remote net probe <port> | free-port | wait <port> <ms> [--https] | connect <host> <port>".into()),
    }
}

fn port_argument(value: Option<&String>) -> Result<u16, String> {
    value
        .ok_or("a port is required")?
        .parse::<u16>()
        .map_err(|_| "the port must be 0-65535".to_owned())
}

/// Whether `port` can be bound on this machine's loopback, and whether
/// something already answers on it there — over IPv4 or IPv6, since a server
/// that bound only `::1` still owns the port for everyone who asks `localhost`.
pub fn probe(port: u16) -> (bool, bool) {
    let listening = port != 0
        && [IpAddr::V4(Ipv4Addr::LOCALHOST), IpAddr::V6(Ipv6Addr::LOCALHOST)]
            .into_iter()
            .any(|ip| TcpStream::connect_timeout(&SocketAddr::new(ip, port), PROBE_CONNECT_TIMEOUT).is_ok());
    let bindable = !listening && TcpListener::bind((Ipv4Addr::LOCALHOST, port)).is_ok();
    (bindable, listening)
}

/// A port the system hands out as free right now.
pub fn free_port() -> io::Result<u16> {
    TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?
        .local_addr()
        .map(|address| address.port())
}

/// Polls until `port` accepts a connection on this machine and, when it does,
/// answers an HTTP request with anything at all. A server that accepts
/// connections before it can serve is still starting.
pub fn wait_until_ready(port: u16, timeout: Duration, https: bool) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        for ip in [IpAddr::V4(Ipv4Addr::LOCALHOST), IpAddr::V6(Ipv6Addr::LOCALHOST)] {
            let address = SocketAddr::new(ip, port);
            if let Ok(stream) = TcpStream::connect_timeout(&address, PROBE_CONNECT_TIMEOUT) {
                // TLS is not spoken here: a listener that accepted is as much of
                // an answer as the handshake would be.
                if https || answers_http(stream, port) {
                    return true;
                }
            }
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(WAIT_RETRY);
    }
}

/// Whether a server answers a `HEAD /` with a status line.
fn answers_http(mut stream: TcpStream, port: u16) -> bool {
    let _ = stream.set_read_timeout(Some(WAIT_HTTP_TIMEOUT));
    let _ = stream.set_write_timeout(Some(WAIT_HTTP_TIMEOUT));
    let request = format!("HEAD / HTTP/1.1\r\nHost: localhost:{port}\r\nConnection: close\r\n\r\n");
    if stream.write_all(request.as_bytes()).is_err() {
        return false;
    }
    let mut head = [0u8; 12];
    let mut filled = 0;
    while filled < head.len() {
        match stream.read(&mut head[filled..]) {
            Ok(0) => break,
            Ok(count) => filled += count,
            Err(_) => break,
        }
    }
    // Anything that starts like a response is a server that is serving; a
    // server that is not HTTP at all never gets this far and still counts once
    // it has held a connection open this long without closing it.
    head[..filled].starts_with(b"HTTP/") || filled == 0
}

/// The first line of standard input, read a byte at a time so nothing after it — the
/// connection's own bytes — is consumed here. Standard input ending first means the connection
/// was never wanted, which ends the process quietly.
fn read_target_line() -> String {
    let mut input = io::stdin().lock();
    let mut line = Vec::with_capacity(64);
    let mut byte = [0u8; 1];
    loop {
        match input.read(&mut byte) {
            Ok(0) => std::process::exit(0),
            Ok(_) if byte[0] == b'\n' => break,
            Ok(_) => {
                if line.len() >= 300 {
                    fail("invalid", "the target line is too long");
                }
                line.push(byte[0]);
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => std::process::exit(0),
        }
    }
    String::from_utf8_lossy(&line).trim().to_owned()
}

fn report(line: &str) {
    let mut stderr = io::stderr().lock();
    let _ = writeln!(stderr, "{line}");
    let _ = stderr.flush();
}

fn fail(kind: &str, detail: impl std::fmt::Display) -> ! {
    report(&format!("error {kind} {detail}"));
    std::process::exit(CONNECT_FAILED);
}

/// Connects to `host:port` as this machine resolves it and relays the
/// connection over standard input and output until either side is done.
fn connect_and_relay(host: &str, port: u16) -> Result<(), String> {
    if host.is_empty() || host.len() > 253 || host.chars().any(|c| c.is_control() || c.is_whitespace()) {
        fail("invalid", "the host name is not valid");
    }
    let addresses: Vec<SocketAddr> = match (host, port).to_socket_addrs() {
        Ok(addresses) => addresses.collect(),
        Err(error) => fail("resolve", error),
    };
    if addresses.is_empty() {
        fail("resolve", format!("{host} has no address"));
    }
    let stream = match connect_any(&addresses) {
        Ok(stream) => stream,
        Err(error) => {
            let kind = match error.kind() {
                io::ErrorKind::ConnectionRefused => "refused",
                io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => "timeout",
                _ => "unreachable",
            };
            fail(kind, error);
        }
    };
    let _ = stream.set_nodelay(true);
    report("ok");
    relay(stream);
    Ok(())
}

/// Connects to the first of `addresses` that answers, in order of preference: each attempt
/// starts once the one before has failed or has had its [`attempt_delay`], and the first
/// connection made is the one used. An attempt still running then is left to finish on its own
/// thread, and whatever it connects is closed. The error is the last attempt's.
fn connect_any(addresses: &[SocketAddr]) -> io::Result<TcpStream> {
    if let [only] = addresses {
        return TcpStream::connect_timeout(only, CONNECT_TIMEOUT);
    }
    let (sender, results) = std::sync::mpsc::channel();
    let mut next = 0;
    let mut running = 0;
    let mut last = None;
    loop {
        if let Some(address) = addresses.get(next).copied() {
            next += 1;
            running += 1;
            let sender = sender.clone();
            std::thread::spawn(move || {
                let _ = sender.send(TcpStream::connect_timeout(&address, CONNECT_TIMEOUT));
            });
        }
        if running == 0 {
            break;
        }
        let wait = match addresses.get(next) {
            Some(_) => attempt_delay(&addresses[next - 1]),
            None => CONNECT_TIMEOUT + Duration::from_secs(1),
        };
        match results.recv_timeout(wait) {
            Ok(Ok(stream)) => return Ok(stream),
            Ok(Err(error)) => {
                running -= 1;
                last = Some(error);
            }
            // The next address, alongside this one.
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) if next < addresses.len() => {}
            Err(_) => break,
        }
    }
    Err(last.unwrap_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "no address answered in time")))
}

/// How long an attempt on `address` runs alone before the next address is tried as well.
fn attempt_delay(address: &SocketAddr) -> Duration {
    if address.ip().is_loopback() {
        NEXT_LOOPBACK_ATTEMPT_DELAY
    } else {
        NEXT_ATTEMPT_DELAY
    }
}

/// Moves bytes both ways. Standard input ending is the far side closing its
/// half, which is passed on as a write shutdown; the socket ending is the
/// server closing, which ends the process once its bytes are out.
fn relay(stream: TcpStream) {
    let mut outbound = match stream.try_clone() {
        Ok(clone) => clone,
        Err(error) => fail("unreachable", error),
    };
    std::thread::spawn(move || {
        let mut input = io::stdin().lock();
        let mut buffer = vec![0u8; RELAY_CHUNK];
        loop {
            match input.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(count) => {
                    if outbound.write_all(&buffer[..count]).is_err() {
                        break;
                    }
                }
            }
        }
        let _ = outbound.shutdown(Shutdown::Write);
    });
    let mut inbound = stream;
    let mut output = io::stdout().lock();
    let mut buffer = vec![0u8; RELAY_CHUNK];
    loop {
        match inbound.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(count) => {
                if output.write_all(&buffer[..count]).and_then(|()| output.flush()).is_err() {
                    break;
                }
            }
        }
    }
    let _ = output.flush();
    let _ = inbound.shutdown(Shutdown::Both);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listening() -> (TcpListener, SocketAddr) {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        (listener, address)
    }

    fn refusing() -> SocketAddr {
        let (listener, address) = listening();
        drop(listener);
        address
    }

    #[test]
    fn the_first_address_that_answers_is_used_in_order_of_preference() {
        let (_first, first) = listening();
        let (_second, second) = listening();
        for _ in 0..20 {
            assert_eq!(connect_any(&[first, second]).unwrap().peer_addr().unwrap(), first);
        }
        // A refused address hands over to the next at once.
        let started = Instant::now();
        assert_eq!(connect_any(&[refusing(), second]).unwrap().peer_addr().unwrap(), second);
        assert!(started.elapsed() < Duration::from_secs(1), "{:?}", started.elapsed());
        let refused = connect_any(&[refusing(), refusing()]).unwrap_err();
        assert_eq!(refused.kind(), io::ErrorKind::ConnectionRefused);
    }

    /// An address that does not answer — a documentation address, which goes nowhere — does not
    /// hold up the next one for longer than the attempt delay.
    #[test]
    fn an_address_that_does_not_answer_does_not_hold_up_the_next() {
        let (_listener, answering) = listening();
        let nowhere: SocketAddr = "192.0.2.1:9".parse().unwrap();
        let started = Instant::now();
        let stream = connect_any(&[nowhere, answering]).unwrap();
        assert_eq!(stream.peer_addr().unwrap(), answering);
        assert!(started.elapsed() < NEXT_ATTEMPT_DELAY + Duration::from_secs(1), "{:?}", started.elapsed());
    }

    #[test]
    fn a_taken_port_is_listening_and_not_bindable() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        assert_eq!(probe(port), (false, true));
        drop(listener);
        let (bindable, listening) = probe(port);
        assert!(bindable && !listening);
    }

    #[test]
    fn readiness_waits_for_a_status_line() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 256];
            let _ = stream.read(&mut request);
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n").unwrap();
        });
        assert!(wait_until_ready(port, Duration::from_secs(5), false));
        server.join().unwrap();
        let unused = free_port().unwrap();
        assert!(!wait_until_ready(unused, Duration::from_millis(300), false));
    }
}
