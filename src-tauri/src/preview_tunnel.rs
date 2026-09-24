//! The network a page of a remote workspace uses: its machine's.
//!
//! A dev server on an SSH machine listens on that machine's loopback, and so,
//! usually, does everything it talks to — the API on the next port, the database
//! admin UI, a name only that machine's resolver knows. A page of that workspace
//! is rendered here, where scrolling and typing cost nothing, but every
//! connection it opens is opened *there*: `localhost` in its address bar is the
//! machine's, and so is every host name it resolves. Claude's own cloud previews
//! are built the same way — the page local, its traffic tunnelled to where the
//! server is — and this is that tunnel for a machine reached over SSH.
//!
//! The tunnel is a SOCKS5 endpoint on this computer's loopback, one per machine,
//! which a page's engine is pointed at ([`crate::browser`] sets the page's proxy,
//! loopback included). Each connection the page opens becomes one relay session
//! of the machine's agent (`mework-remote net connect`), carried by the link the
//! machine already has. A relay is a session like any other, so a connection
//! survives the link dropping under it: the bytes wait on the machine, keyed by
//! offset, and resume when the link is back.
//!
//! Latency is the whole game here, and it is spent in round trips:
//!
//! * The machine's name resolution and TCP handshake happen there, next to the
//!   server, and cost nothing across the link.
//! * A relay is started before it is needed. Each machine keeps
//!   [`READY_CONNECTORS`] connectors waiting for a target on their input, so a
//!   new connection sends its target and its first bytes in one go instead of
//!   asking for a process and waiting for the answer first.
//! * The page is answered only once the machine has connected, so a refused
//!   port reaches the page as the refusal it is — a start page's "not running
//!   yet" — rather than as a connection that opened and then broke.

use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, Ipv6Addr, Shutdown, TcpListener, TcpStream};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::Duration;

use remote_agent::client::RemoteProcess;
use remote_agent::protocol::SELF_PROGRAM;

use crate::preview_remote::RemoteMachine;
use crate::remote_link;

/// Connectors each machine keeps started and waiting for a target.
const READY_CONNECTORS: usize = 2;
/// How long the page's SOCKS greeting and request may take. The page is on this computer; a
/// client this slow is not a browser.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a new connection waits for the machine's link, a reconnect included, before the page
/// is told the network is unreachable.
const LINK_PATIENCE: Duration = Duration::from_secs(20);
/// Bytes moved per read toward the machine.
const RELAY_CHUNK: usize = 64 * 1024;

const SOCKS_VERSION: u8 = 5;
const REPLY_SUCCEEDED: u8 = 0;
const REPLY_GENERAL_FAILURE: u8 = 1;
const REPLY_NETWORK_UNREACHABLE: u8 = 3;
const REPLY_HOST_UNREACHABLE: u8 = 4;
const REPLY_CONNECTION_REFUSED: u8 = 5;
const REPLY_COMMAND_NOT_SUPPORTED: u8 = 7;
const REPLY_ADDRESS_NOT_SUPPORTED: u8 = 8;

/// Starts a relay on a machine, given the `net connect` arguments after `connect`.
type Connector = Arc<dyn Fn(Vec<String>) -> Result<RemoteProcess, String> + Send + Sync>;

/// One machine's endpoint.
struct MachineTunnel {
    connector: Mutex<Connector>,
    port: u16,
    /// Connectors already started, waiting for a target.
    ready: Mutex<Vec<RemoteProcess>>,
    /// Whether a refill is already under way, so a burst of connections starts one, not many.
    refilling: Mutex<bool>,
}

fn tunnels() -> &'static Mutex<HashMap<String, Arc<MachineTunnel>>> {
    static TUNNELS: OnceLock<Mutex<HashMap<String, Arc<MachineTunnel>>>> = OnceLock::new();
    TUNNELS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The proxy a page of a workspace on `machine` is pointed at: `socks5://127.0.0.1:<port>`.
///
/// The endpoint is started the first time a machine needs one and kept for the life of the
/// process — it costs a listening socket and nothing else until a page uses it. A machine whose
/// settings changed since (its host, its key) gets them on its next connection.
pub fn proxy_for(machine: &RemoteMachine) -> Result<String, String> {
    let key = machine.key().to_owned();
    let machine = machine.clone().with_patience(LINK_PATIENCE);
    let connector: Connector = Arc::new(move |target| {
        let link = machine.link()?;
        let mut argv = vec![SELF_PROGRAM.to_owned(), "net".to_owned(), "connect".to_owned()];
        argv.extend(target);
        remote_link::spawn_relay(&link, machine.runner(), argv)
    });
    endpoint(&key, connector).map(proxy_url)
}

/// The endpoint filed under `key`, started with `connector` if there is none yet. An existing one
/// takes the new connector, so a machine whose settings changed is reached the new way from its
/// next connection on.
fn endpoint(key: &str, connector: Connector) -> Result<u16, String> {
    let mut tunnels = lock(tunnels());
    if let Some(tunnel) = tunnels.get(key) {
        *lock(&tunnel.connector) = connector;
        return Ok(tunnel.port);
    }
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .map_err(|error| format!("无法为远程预览开启本地网络入口：{error}"))?;
    let port = listener
        .local_addr()
        .map_err(|error| format!("无法为远程预览开启本地网络入口：{error}"))?
        .port();
    let tunnel = Arc::new(MachineTunnel {
        connector: Mutex::new(connector),
        port,
        ready: Mutex::new(Vec::new()),
        refilling: Mutex::new(false),
    });
    tunnels.insert(key.to_owned(), tunnel.clone());
    drop(tunnels);
    let accepting = tunnel.clone();
    thread::Builder::new()
        .name(format!("preview-tunnel-{port}"))
        .spawn(move || accept_loop(listener, accepting))
        .map_err(|error| format!("无法为远程预览开启本地网络入口：{error}"))?;
    refill(&tunnel);
    Ok(port)
}

fn proxy_url(port: u16) -> String {
    format!("socks5://127.0.0.1:{port}")
}

fn accept_loop(listener: TcpListener, tunnel: Arc<MachineTunnel>) {
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        // Loopback only, and only this computer's: the endpoint is a way onto another machine's
        // network, and nothing but a page here may use it.
        let local = stream
            .peer_addr()
            .map(|address| address.ip().is_loopback())
            .unwrap_or(false);
        if !local {
            continue;
        }
        let tunnel = tunnel.clone();
        let _ = thread::Builder::new()
            .name("preview-tunnel-connection".into())
            .spawn(move || {
                let _ = serve(stream, &tunnel);
            });
    }
}

/// Starts connectors in the background until the machine has [`READY_CONNECTORS`] waiting.
fn refill(tunnel: &Arc<MachineTunnel>) {
    {
        let mut refilling = lock(&tunnel.refilling);
        if *refilling {
            return;
        }
        *refilling = true;
    }
    let tunnel = tunnel.clone();
    let _ = thread::Builder::new()
        .name("preview-tunnel-refill".into())
        .spawn(move || {
            while lock(&tunnel.ready).len() < READY_CONNECTORS {
                match start_connector(&tunnel, vec!["-".to_owned()]) {
                    Ok(process) => lock(&tunnel.ready).push(process),
                    // The machine is away. The next connection asks for one directly, and
                    // refills again once one works.
                    Err(_) => break,
                }
            }
            *lock(&tunnel.refilling) = false;
        });
}

/// Starts a relay on the machine: `net connect -` waiting for its target, or `net connect <host>
/// <port>` directly.
fn start_connector(tunnel: &MachineTunnel, target: Vec<String>) -> Result<RemoteProcess, String> {
    let connector = lock(&tunnel.connector).clone();
    connector(target)
}

/// A connector that is still there: one whose relay has not ended while it waited.
fn take_ready(tunnel: &MachineTunnel) -> Option<RemoteProcess> {
    let mut ready = lock(&tunnel.ready);
    while let Some(process) = ready.pop() {
        if matches!(process.try_wait(), Ok(None)) {
            return Some(process);
        }
    }
    None
}

/// Where a page asked to connect, as the machine should resolve it.
#[derive(Debug, PartialEq, Eq)]
struct Target {
    host: String,
    port: u16,
}

/// One connection from a page: the SOCKS5 exchange, the connection on the machine, then the
/// relay until either side is done.
fn serve(mut stream: TcpStream, tunnel: &Arc<MachineTunnel>) -> io::Result<()> {
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(HANDSHAKE_TIMEOUT))?;
    let target = match read_request(&mut stream)? {
        Ok(target) => target,
        Err(reply) => {
            write_reply(&mut stream, reply)?;
            return Ok(());
        }
    };
    stream.set_read_timeout(None)?;

    // A ready connector gets the target as its first line; without one, a connector is started
    // for this target directly. Either way the machine answers with one status line.
    let (process, pending) = match take_ready(tunnel) {
        Some(process) => (process, Some(format!("{} {}\n", target.host, target.port))),
        None => match start_connector(tunnel, vec![target.host.clone(), target.port.to_string()]) {
            Ok(process) => (process, None),
            Err(_) => {
                write_reply(&mut stream, REPLY_NETWORK_UNREACHABLE)?;
                return Ok(());
            }
        },
    };
    refill(tunnel);
    let mut process = process;
    let mut input = process.stdin();
    if let Some(line) = pending {
        if input.write_all(line.as_bytes()).is_err() {
            write_reply(&mut stream, REPLY_GENERAL_FAILURE)?;
            return Ok(());
        }
    }
    let Some(stderr) = process.take_stderr() else {
        write_reply(&mut stream, REPLY_GENERAL_FAILURE)?;
        return Ok(());
    };
    let mut status = String::new();
    let mut stderr = BufReader::new(stderr);
    if stderr.read_line(&mut status).unwrap_or(0) == 0 {
        write_reply(&mut stream, REPLY_GENERAL_FAILURE)?;
        return Ok(());
    }
    let reply = status_reply(status.trim_end());
    write_reply(&mut stream, reply)?;
    if reply != REPLY_SUCCEEDED {
        return Ok(());
    }
    let Some(output) = process.take_stdout() else {
        return Ok(());
    };
    relay(stream, process, input, output);
    Ok(())
}

/// The SOCKS5 reply a connector's status line calls for.
fn status_reply(line: &str) -> u8 {
    if line == "ok" {
        return REPLY_SUCCEEDED;
    }
    match line.split_whitespace().nth(1) {
        Some("refused") => REPLY_CONNECTION_REFUSED,
        Some("resolve" | "unreachable" | "timeout") => REPLY_HOST_UNREACHABLE,
        _ => REPLY_GENERAL_FAILURE,
    }
}

/// Reads the greeting and the request. `Ok(Err(reply))` is a request this endpoint refuses, with
/// the reply that says why.
fn read_request(stream: &mut TcpStream) -> io::Result<Result<Target, u8>> {
    let mut header = [0u8; 2];
    stream.read_exact(&mut header)?;
    if header[0] != SOCKS_VERSION {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "not SOCKS5"));
    }
    let mut methods = vec![0u8; usize::from(header[1])];
    stream.read_exact(&mut methods)?;
    // No authentication, which is all a browser offers; the endpoint is loopback-only.
    if !methods.contains(&0) {
        stream.write_all(&[SOCKS_VERSION, 0xFF])?;
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "no usable method"));
    }
    stream.write_all(&[SOCKS_VERSION, 0])?;

    let mut request = [0u8; 4];
    stream.read_exact(&mut request)?;
    if request[0] != SOCKS_VERSION {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "not SOCKS5"));
    }
    let host = match request[3] {
        1 => {
            let mut octets = [0u8; 4];
            stream.read_exact(&mut octets)?;
            Ipv4Addr::from(octets).to_string()
        }
        3 => {
            let mut length = [0u8; 1];
            stream.read_exact(&mut length)?;
            let mut name = vec![0u8; usize::from(length[0])];
            stream.read_exact(&mut name)?;
            String::from_utf8_lossy(&name).into_owned()
        }
        4 => {
            let mut octets = [0u8; 16];
            stream.read_exact(&mut octets)?;
            Ipv6Addr::from(octets).to_string()
        }
        _ => return Ok(Err(REPLY_ADDRESS_NOT_SUPPORTED)),
    };
    let mut port = [0u8; 2];
    stream.read_exact(&mut port)?;
    // Only CONNECT: a page opens streams, it does not listen or send datagrams.
    if request[1] != 1 {
        return Ok(Err(REPLY_COMMAND_NOT_SUPPORTED));
    }
    // The machine resolves the name; what reaches it is one token it can read back.
    if host.is_empty() || host.len() > 253 || host.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Ok(Err(REPLY_HOST_UNREACHABLE));
    }
    Ok(Ok(Target {
        host,
        port: u16::from_be_bytes(port),
    }))
}

fn write_reply(stream: &mut TcpStream, reply: u8) -> io::Result<()> {
    // The bound address is not the page's business; it is reported as unspecified.
    stream.write_all(&[SOCKS_VERSION, reply, 0, 1, 0, 0, 0, 0, 0, 0])
}

/// Moves bytes both ways until both are done. The page closing its side is passed on as the end
/// of the relay's input; the machine's side ending closes the page's.
///
/// Bytes the machine could not hold while the link was down are a hole in the stream, and a TCP
/// stream with a hole in it is worse than a closed one: the connection is cut instead, and the
/// page retries it the way it retries any dropped connection.
fn relay(
    stream: TcpStream,
    process: RemoteProcess,
    mut input: remote_agent::client::SessionWriter,
    mut output: remote_agent::client::SessionReader,
) {
    let Ok(mut upstream) = stream.try_clone() else {
        return;
    };
    let process = Arc::new(process);
    let closer = process.clone();
    let sending = thread::spawn(move || {
        let mut buffer = vec![0u8; RELAY_CHUNK];
        loop {
            match upstream.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(count) => {
                    if input.write_all(&buffer[..count]).is_err() {
                        break;
                    }
                }
            }
        }
        closer.close_stdin();
    });
    let mut downstream = stream;
    let mut buffer = vec![0u8; RELAY_CHUNK];
    loop {
        match output.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(count) => {
                if process.lost_bytes() > 0 {
                    break;
                }
                if downstream.write_all(&buffer[..count]).is_err() {
                    break;
                }
            }
        }
    }
    let _ = downstream.shutdown(Shutdown::Both);
    let _ = sending.join();
    // Released on drop: the relay is ended on the machine if it is somehow still running.
    drop(process);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exchange(request: &[u8]) -> (Vec<u8>, io::Result<Result<Target, u8>>) {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let request = request.to_vec();
        let client = thread::spawn(move || {
            let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
            stream.write_all(&request).unwrap();
            stream.shutdown(Shutdown::Write).unwrap();
            let mut answer = Vec::new();
            let _ = stream.read_to_end(&mut answer);
            answer
        });
        let (mut stream, _) = listener.accept().unwrap();
        let parsed = read_request(&mut stream);
        drop(stream);
        (client.join().unwrap(), parsed)
    }

    #[test]
    fn a_browser_request_by_name_reaches_the_machine_as_its_name() {
        let mut request = vec![5, 1, 0, 5, 1, 0, 3, 9];
        request.extend_from_slice(b"localhost");
        request.extend_from_slice(&5173u16.to_be_bytes());
        let (answer, parsed) = exchange(&request);
        assert_eq!(answer, vec![5, 0], "no authentication is chosen");
        assert_eq!(
            parsed.unwrap().unwrap(),
            Target {
                host: "localhost".into(),
                port: 5173
            }
        );
    }

    #[test]
    fn literal_addresses_and_refusals() {
        let mut ipv4 = vec![5, 1, 0, 5, 1, 0, 1, 127, 0, 0, 1];
        ipv4.extend_from_slice(&8080u16.to_be_bytes());
        assert_eq!(exchange(&ipv4).1.unwrap().unwrap().host, "127.0.0.1");
        let mut ipv6 = vec![5, 1, 0, 5, 1, 0, 4];
        ipv6.extend_from_slice(&Ipv6Addr::LOCALHOST.octets());
        ipv6.extend_from_slice(&80u16.to_be_bytes());
        assert_eq!(exchange(&ipv6).1.unwrap().unwrap().host, "::1");
        let mut bind = vec![5, 1, 0, 5, 2, 0, 1, 127, 0, 0, 1];
        bind.extend_from_slice(&80u16.to_be_bytes());
        assert_eq!(exchange(&bind).1.unwrap().unwrap_err(), REPLY_COMMAND_NOT_SUPPORTED);
        let (answer, parsed) = exchange(&[5, 1, 2]);
        assert_eq!(answer, vec![5, 0xFF], "a client that insists on a password is turned away");
        assert!(parsed.is_err());
    }

    /// The agent this workspace builds, run as its own proxy the way an SSH session runs it —
    /// the same stand-in for a remote machine the agent's own end-to-end tests use. Absent until
    /// `cargo build -p mework-remote-agent` has run, in which case the test says so and passes.
    #[cfg(unix)]
    fn built_agent() -> Option<std::path::PathBuf> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("debug")
            .join(remote_agent::AGENT_BINARY);
        path.is_file().then_some(path)
    }

    #[cfg(unix)]
    struct DirectLauncher {
        agent: std::path::PathBuf,
        root: std::path::PathBuf,
    }

    #[cfg(unix)]
    impl remote_agent::client::Launcher for DirectLauncher {
        fn launch(
            &self,
            nonce: &str,
        ) -> Result<remote_agent::client::Transport, remote_agent::client::LaunchError> {
            use remote_agent::client::{LaunchError, Transport};
            use std::process::{Command, Stdio};
            let mut child = Command::new(&self.agent)
                .args(["proxy", "--sync", nonce, "--idle-exit", "5", "--tick-ms", "100"])
                .env("MEWORK_REMOTE_ROOT", &self.root)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|error| LaunchError::Unreachable(error.to_string()))?;
            let mut reader = child.stdout.take().unwrap();
            let writer = child.stdin.take().unwrap();
            match remote_agent::protocol::read_preamble(&mut reader, nonce) {
                Ok(remote_agent::protocol::Preamble::Ready) => {}
                other => return Err(LaunchError::Unreachable(format!("{other:?}"))),
            }
            let child = Arc::new(Mutex::new(child));
            Ok(Transport {
                reader: Box::new(reader),
                writer: Box::new(writer),
                closer: Box::new(move || {
                    let mut child = lock(&child);
                    let _ = child.kill();
                    let _ = child.wait();
                }),
            })
        }
    }

    #[cfg(unix)]
    fn socks_connect(proxy_port: u16, host: &str, port: u16) -> (TcpStream, u8) {
        let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, proxy_port)).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
        stream.write_all(&[5, 1, 0]).unwrap();
        let mut chosen = [0u8; 2];
        stream.read_exact(&mut chosen).unwrap();
        assert_eq!(chosen, [5, 0]);
        let mut request = vec![5, 1, 0, 3, host.len() as u8];
        request.extend_from_slice(host.as_bytes());
        request.extend_from_slice(&port.to_be_bytes());
        stream.write_all(&request).unwrap();
        let mut reply = [0u8; 10];
        stream.read_exact(&mut reply).unwrap();
        (stream, reply[1])
    }

    /// A page's request, end to end: the SOCKS exchange here, the connection opened by the agent
    /// on "the machine" by name, the response back — and a port nothing listens on refused as
    /// such, before a byte of data moves.
    #[cfg(unix)]
    #[test]
    fn a_page_connection_reaches_the_machines_loopback_through_the_agent() {
        let Some(agent) = built_agent() else {
            eprintln!("skipped: build mework-remote-agent to run the tunnel end to end");
            return;
        };
        let root = tempfile::Builder::new().prefix("mwt").tempdir_in("/tmp").unwrap();
        let link = remote_agent::client::Link::start(
            remote_agent::client::LinkConfig::new("tunnel-test", "e1"),
            DirectLauncher {
                agent,
                root: root.path().to_path_buf(),
            },
        );
        link.wait_connected(Duration::from_secs(30)).unwrap();
        let relay_link = link.clone();
        let connector: Connector = Arc::new(move |target| {
            let mut argv = vec![SELF_PROGRAM.to_owned(), "net".to_owned(), "connect".to_owned()];
            argv.extend(target);
            remote_link::spawn_relay(
                &relay_link,
                &crate::run_environment::ShellRunner::default(),
                argv,
            )
        });
        let proxy_port = endpoint("tunnel-test-machine", connector).unwrap();

        let server = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let server_port = server.local_addr().unwrap().port();
        let serving = thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream, _) = server.accept().unwrap();
                let mut request = Vec::new();
                let mut byte = [0u8; 1];
                while !request.ends_with(b"\r\n\r\n") {
                    stream.read_exact(&mut byte).unwrap();
                    request.push(byte[0]);
                }
                let body = "x".repeat(200_000);
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        });
        // Twice: the first through a connector started ahead of time, the second after the
        // refill — both have to land on the same server by the name the machine resolves.
        for _ in 0..2 {
            let (mut stream, reply) = socks_connect(proxy_port, "localhost", server_port);
            assert_eq!(reply, REPLY_SUCCEEDED);
            stream
                .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
                .unwrap();
            let mut response = Vec::new();
            stream.read_to_end(&mut response).unwrap();
            let text = String::from_utf8_lossy(&response);
            assert!(text.starts_with("HTTP/1.1 200 OK"), "{}", &text[..text.len().min(80)]);
            assert!(text.ends_with(&"x".repeat(200_000)));
        }
        serving.join().unwrap();

        let free = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let (_, reply) = socks_connect(proxy_port, "127.0.0.1", free);
        assert_eq!(reply, REPLY_CONNECTION_REFUSED);
        link.close(true);
    }

    #[test]
    fn the_machines_verdict_becomes_the_matching_reply() {
        assert_eq!(status_reply("ok"), REPLY_SUCCEEDED);
        assert_eq!(status_reply("error refused Connection refused"), REPLY_CONNECTION_REFUSED);
        assert_eq!(status_reply("error resolve no such host"), REPLY_HOST_UNREACHABLE);
        assert_eq!(status_reply("error timeout timed out"), REPLY_HOST_UNREACHABLE);
        assert_eq!(status_reply("anything else"), REPLY_GENERAL_FAILURE);
    }
}
