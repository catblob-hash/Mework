//! TCP connections carried inside the link, for a sandboxed cell that has no
//! network of its own.
//!
//! On Linux a cell lives in a network namespace with nothing but loopback. Its
//! processes reach the proxy on that loopback; the proxy asks for each
//! destination with [`Message::Dial`] over the cell's standard output, and the
//! agent that started the cell — outside the sandbox, where the network policy
//! is — answers with [`Message::Dialed`] and relays the bytes. Standard input
//! and output are the one channel a sandboxed process cannot reach past: no
//! socket of the machine's is involved.
//!
//! Each connection has a window in each direction ([`TUNNEL_WINDOW`]): a side
//! sends only what the other has room for, and the other acknowledges what it
//! delivered. A download the sandboxed process reads slowly therefore waits at
//! its source instead of piling up in either agent.

use std::collections::{HashMap, VecDeque};
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::{Duration, Instant};

use crate::protocol::{Message, OUTPUT_CHUNK_BYTES, TUNNEL_WINDOW};

/// How long a cell waits for the answer to a dial.
const DIAL_TIMEOUT: Duration = Duration::from_secs(45);

/// Most connections one link carries at once. The side outside a sandbox
/// holds a socket and a thread for each, and does not let the side inside
/// decide how many that is.
pub const MAX_CONNECTIONS: usize = 256;

/// The writing half of a connection.
pub trait Outbound: Write + Send {
    /// Nothing more will be written; what the other end sends still arrives.
    fn finish(&mut self);
    /// The connection is over in both directions.
    fn abort(&mut self);
}

impl Outbound for TcpStream {
    fn finish(&mut self) {
        let _ = self.shutdown(Shutdown::Write);
    }

    fn abort(&mut self) {
        let _ = self.shutdown(Shutdown::Both);
    }
}

/// Both halves of a connection to a destination.
pub struct Upstream {
    pub reader: Box<dyn Read + Send>,
    pub writer: Box<dyn Outbound>,
}

impl Upstream {
    pub fn tcp(stream: TcpStream) -> io::Result<Self> {
        Ok(Self {
            reader: Box::new(stream.try_clone()?),
            writer: Box::new(stream),
        })
    }
}

/// Copies both directions between `local` and `upstream` until both ended,
/// passing each end of file on as a half close. `pending` is owed to the
/// upstream before anything `local` sends.
pub fn relay(local: TcpStream, pending: Vec<u8>, upstream: Upstream) {
    let Upstream { mut reader, mut writer } = upstream;
    let Ok(mut local_reader) = local.try_clone() else {
        writer.abort();
        return;
    };
    let mut local_writer = local;
    let uploader = std::thread::Builder::new()
        .name("tunnel-up".into())
        .spawn(move || {
            if !pending.is_empty() && writer.write_all(&pending).and_then(|_| writer.flush()).is_err() {
                writer.abort();
                let _ = local_reader.shutdown(Shutdown::Both);
                return;
            }
            let mut buffer = vec![0u8; OUTPUT_CHUNK_BYTES];
            loop {
                match local_reader.read(&mut buffer) {
                    Ok(0) => {
                        writer.finish();
                        return;
                    }
                    Ok(count) => {
                        if writer.write_all(&buffer[..count]).and_then(|_| writer.flush()).is_err() {
                            writer.abort();
                            let _ = local_reader.shutdown(Shutdown::Both);
                            return;
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    Err(_) => {
                        writer.abort();
                        return;
                    }
                }
            }
        });
    let mut buffer = vec![0u8; OUTPUT_CHUNK_BYTES];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => {
                let _ = local_writer.shutdown(Shutdown::Write);
                break;
            }
            Ok(count) => {
                if local_writer.write_all(&buffer[..count]).is_err() {
                    let _ = local_writer.shutdown(Shutdown::Both);
                    break;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => {
                let _ = local_writer.shutdown(Shutdown::Both);
                break;
            }
        }
    }
    if let Ok(uploader) = uploader {
        let _ = uploader.join();
    }
}

/// Sends one frame on the link; `false` when the link is gone.
pub type SendFrame = Arc<dyn Fn(&Message, &[u8]) -> bool + Send + Sync>;

#[derive(Default)]
struct ConnState {
    /// Received from the other side, not yet read here.
    inbound: VecDeque<Vec<u8>>,
    /// Bytes in `inbound`: never more than the window this side granted.
    inbound_bytes: u64,
    inbound_ended: bool,
    /// How much more this side may send.
    credit: u64,
    outbound_ended: bool,
    closed: bool,
    /// The answer to this side's dial, while it waits for one.
    dialed: Option<Result<(), (bool, String)>>,
}

struct Conn {
    id: u64,
    state: Mutex<ConnState>,
    cond: Condvar,
}

/// The connections carried by one link, at either end of it.
pub struct Tunnels {
    send: SendFrame,
    conns: Mutex<HashMap<u64, Arc<Conn>>>,
    next: AtomicU64,
    this: Weak<Tunnels>,
}

impl Tunnels {
    pub fn new(send: SendFrame) -> Arc<Self> {
        Arc::new_cyclic(|this| Self {
            send,
            conns: Mutex::new(HashMap::new()),
            next: AtomicU64::new(1),
            this: this.clone(),
        })
    }

    /// From inside a cell: asks the other end for a connection to
    /// `host:port`. The error says whether the policy refused it.
    pub fn dial(&self, host: &str, port: u16) -> Result<Upstream, (bool, String)> {
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        let conn = self.insert(id);
        let asked = (self.send)(
            &Message::Dial {
                conn: id,
                host: host.to_owned(),
                port,
            },
            &[],
        );
        if !asked {
            self.remove(id);
            return Err((false, "the sandbox lost its link to the agent".into()));
        }
        let deadline = Instant::now() + DIAL_TIMEOUT;
        let mut state = lock(&conn.state);
        loop {
            if let Some(answer) = state.dialed.take() {
                drop(state);
                return match answer {
                    Ok(()) => Ok(self.halves(&conn)),
                    Err(error) => {
                        self.remove(id);
                        Err(error)
                    }
                };
            }
            if state.closed {
                drop(state);
                self.remove(id);
                return Err((false, "the sandbox lost its link to the agent".into()));
            }
            let now = Instant::now();
            if now >= deadline {
                drop(state);
                self.remove(id);
                let _ = (self.send)(&Message::TunnelClose { conn: id }, &[]);
                return Err((false, format!("no answer for {host}:{port} in time")));
            }
            state = conn
                .cond
                .wait_timeout(state, deadline - now)
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .0;
        }
    }

    /// From outside a cell: accepts the cell's connection `id`, now open to
    /// its destination, and tells the cell so.
    pub fn accept(&self, id: u64) -> Option<Upstream> {
        {
            let conns = lock(&self.conns);
            if conns.contains_key(&id) || conns.len() >= MAX_CONNECTIONS {
                return None;
            }
        }
        let conn = self.insert(id);
        if !(self.send)(
            &Message::Dialed {
                conn: id,
                error: None,
                refused: false,
            },
            &[],
        ) {
            self.remove(id);
            return None;
        }
        Some(self.halves(&conn))
    }

    /// From outside a cell: refuses connection `id`.
    pub fn refuse(&self, id: u64, refused: bool, error: String) {
        let _ = (self.send)(
            &Message::Dialed {
                conn: id,
                error: Some(error),
                refused,
            },
            &[],
        );
    }

    /// Takes a tunnel frame from the link. Returns `false` for a message that
    /// is not one.
    pub fn dispatch(&self, message: &Message, body: &[u8]) -> bool {
        let (id, apply): (u64, Box<dyn FnOnce(&mut ConnState)>) = match message {
            Message::Dialed { conn, error, refused } => {
                let answer = match error {
                    None => Ok(()),
                    Some(error) => Err((*refused, error.clone())),
                };
                (*conn, Box::new(move |state| state.dialed = Some(answer)))
            }
            Message::Tunnel { conn } => {
                let data = body.to_vec();
                (*conn, Box::new(move |state| {
                    if data.is_empty() || state.inbound_ended {
                        return;
                    }
                    // A peer that sends past the window it was given is
                    // not flow-controlled; its connection ends rather than
                    // grow this side's memory.
                    if state.inbound_bytes + data.len() as u64 > TUNNEL_WINDOW {
                        state.closed = true;
                        return;
                    }
                    state.inbound_bytes += data.len() as u64;
                    state.inbound.push_back(data);
                }))
            }
            Message::TunnelAck { conn, bytes } => {
                let bytes = (*bytes).min(TUNNEL_WINDOW);
                (*conn, Box::new(move |state| state.credit = (state.credit + bytes).min(TUNNEL_WINDOW)))
            }
            Message::TunnelEnd { conn } => (*conn, Box::new(|state| state.inbound_ended = true)),
            Message::TunnelClose { conn } => (*conn, Box::new(|state| state.closed = true)),
            _ => return false,
        };
        let conn = lock(&self.conns).get(&id).cloned();
        if let Some(conn) = conn {
            let overrun = {
                let mut state = lock(&conn.state);
                let was_closed = state.closed;
                apply(&mut state);
                !was_closed && state.closed && matches!(message, Message::Tunnel { .. })
            };
            if overrun {
                let _ = (self.send)(&Message::TunnelClose { conn: id }, &[]);
            }
            conn.cond.notify_all();
            self.collect(&conn);
        }
        true
    }

    /// Ends every connection: the link they ran over is gone.
    pub fn close_all(&self) {
        let conns: Vec<Arc<Conn>> = lock(&self.conns).drain().map(|(_, conn)| conn).collect();
        for conn in conns {
            lock(&conn.state).closed = true;
            conn.cond.notify_all();
        }
    }

    pub fn open_count(&self) -> usize {
        lock(&self.conns).len()
    }

    fn insert(&self, id: u64) -> Arc<Conn> {
        let conn = Arc::new(Conn {
            id,
            state: Mutex::new(ConnState {
                credit: TUNNEL_WINDOW,
                ..ConnState::default()
            }),
            cond: Condvar::new(),
        });
        lock(&self.conns).insert(id, Arc::clone(&conn));
        conn
    }

    fn remove(&self, id: u64) {
        lock(&self.conns).remove(&id);
    }

    /// Forgets a connection that is over in both directions.
    fn collect(&self, conn: &Conn) {
        let over = {
            let state = lock(&conn.state);
            state.closed || (state.inbound_ended && state.outbound_ended && state.inbound.is_empty())
        };
        if over {
            self.remove(conn.id);
        }
    }

    fn halves(&self, conn: &Arc<Conn>) -> Upstream {
        let tunnels = self.this.upgrade().expect("the tunnels outlive their connections");
        Upstream {
            reader: Box::new(TunnelReader {
                tunnels: Arc::clone(&tunnels),
                conn: Arc::clone(conn),
            }),
            writer: Box::new(TunnelWriter {
                tunnels,
                conn: Arc::clone(conn),
                done: false,
            }),
        }
    }
}

struct TunnelReader {
    tunnels: Arc<Tunnels>,
    conn: Arc<Conn>,
}

impl Read for TunnelReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        let mut state = lock(&self.conn.state);
        loop {
            if let Some(front) = state.inbound.front_mut() {
                let count = front.len().min(buffer.len());
                buffer[..count].copy_from_slice(&front[..count]);
                front.drain(..count);
                if front.is_empty() {
                    state.inbound.pop_front();
                }
                state.inbound_bytes -= count as u64;
                drop(state);
                let _ = (self.tunnels.send)(
                    &Message::TunnelAck {
                        conn: self.conn.id,
                        bytes: count as u64,
                    },
                    &[],
                );
                return Ok(count);
            }
            if state.closed {
                return Err(io::Error::new(io::ErrorKind::ConnectionReset, "the connection was closed"));
            }
            if state.inbound_ended {
                drop(state);
                self.tunnels.collect(&self.conn);
                return Ok(0);
            }
            state = self
                .conn
                .cond
                .wait(state)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }
}

struct TunnelWriter {
    tunnels: Arc<Tunnels>,
    conn: Arc<Conn>,
    done: bool,
}

impl Write for TunnelWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        let allowed = {
            let mut state = lock(&self.conn.state);
            loop {
                if state.closed {
                    return Err(io::Error::new(io::ErrorKind::BrokenPipe, "the connection was closed"));
                }
                if state.credit > 0 {
                    let allowed = (state.credit as usize).min(buffer.len()).min(OUTPUT_CHUNK_BYTES);
                    state.credit -= allowed as u64;
                    break allowed;
                }
                state = self
                    .conn
                    .cond
                    .wait(state)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
            }
        };
        if !(self.tunnels.send)(&Message::Tunnel { conn: self.conn.id }, &buffer[..allowed]) {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "the link is gone"));
        }
        Ok(allowed)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Outbound for TunnelWriter {
    fn finish(&mut self) {
        if std::mem::replace(&mut self.done, true) {
            return;
        }
        lock(&self.conn.state).outbound_ended = true;
        let _ = (self.tunnels.send)(&Message::TunnelEnd { conn: self.conn.id }, &[]);
        self.tunnels.collect(&self.conn);
    }

    fn abort(&mut self) {
        self.done = true;
        lock(&self.conn.state).closed = true;
        self.conn.cond.notify_all();
        let _ = (self.tunnels.send)(&Message::TunnelClose { conn: self.conn.id }, &[]);
        self.tunnels.collect(&self.conn);
    }
}

impl Drop for TunnelWriter {
    fn drop(&mut self) {
        if !self.done {
            self.abort();
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::sync::{mpsc, Mutex};

    /// Two tunnel tables joined back to back, as a cell and the agent outside
    /// it are: a frame one sends is dispatched by the other, in order.
    fn pair() -> (Arc<Tunnels>, Arc<Tunnels>) {
        let (to_outer, from_inner) = mpsc::channel::<(Message, Vec<u8>)>();
        let (to_inner, from_outer) = mpsc::channel::<(Message, Vec<u8>)>();
        let inner = Tunnels::new(Arc::new(move |message: &Message, body: &[u8]| {
            to_outer.send((message.clone(), body.to_vec())).is_ok()
        }));
        let outer = Tunnels::new(Arc::new(move |message: &Message, body: &[u8]| {
            to_inner.send((message.clone(), body.to_vec())).is_ok()
        }));
        {
            let inner = Arc::clone(&inner);
            std::thread::spawn(move || {
                for (message, body) in from_outer {
                    inner.dispatch(&message, &body);
                }
            });
        }
        {
            let outer = Arc::clone(&outer);
            std::thread::spawn(move || {
                for (message, body) in from_inner {
                    if let Message::Dial { conn, port, .. } = message {
                        let outer = Arc::clone(&outer);
                        std::thread::spawn(move || {
                            if port == 1 {
                                outer.refuse(conn, true, "not on the allowlist".into());
                                return;
                            }
                            let stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
                            let upstream = outer.accept(conn).unwrap();
                            relay(stream, Vec::new(), upstream);
                        });
                    } else {
                        outer.dispatch(&message, &body);
                    }
                }
            });
        }
        (inner, outer)
    }

    #[test]
    fn a_tunnelled_connection_carries_more_than_its_window_both_ways() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = stream.try_clone().unwrap();
            let mut writer = stream;
            // Echo everything, then end.
            io::copy(&mut reader, &mut writer).unwrap();
            writer.shutdown(Shutdown::Write).unwrap();
        });
        let (inner, _outer) = pair();
        let Upstream { mut reader, mut writer } = inner.dial("destination", port).unwrap();
        let payload: Vec<u8> = (0..3 * TUNNEL_WINDOW as usize).map(|i| (i % 251) as u8).collect();
        let sent = payload.clone();
        let sender = std::thread::spawn(move || {
            writer.write_all(&sent).unwrap();
            writer.finish();
        });
        let mut echoed = Vec::new();
        reader.read_to_end(&mut echoed).unwrap();
        sender.join().unwrap();
        assert_eq!(echoed.len(), payload.len());
        assert!(echoed == payload);
    }

    #[test]
    fn a_peer_that_ignores_the_window_loses_the_connection() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&sent);
        let outer = Tunnels::new(Arc::new(move |message: &Message, _: &[u8]| {
            record.lock().unwrap().push(message.clone());
            true
        }));
        let Upstream { mut reader, .. } = outer.accept(7).unwrap();
        let chunk = vec![0u8; OUTPUT_CHUNK_BYTES];
        for _ in 0..(TUNNEL_WINDOW as usize / OUTPUT_CHUNK_BYTES) {
            outer.dispatch(&Message::Tunnel { conn: 7 }, &chunk);
        }
        // Exactly the window is fine; one byte more is not.
        outer.dispatch(&Message::Tunnel { conn: 7 }, b"x");
        assert!(sent.lock().unwrap().contains(&Message::TunnelClose { conn: 7 }));
        let mut buffer = vec![0u8; 16];
        assert!(reader.read(&mut buffer).is_err() || outer.open_count() == 0);
    }

    #[test]
    fn a_refused_dial_reports_the_policy_not_the_network() {
        let (inner, _outer) = pair();
        let error = inner.dial("evil.example", 1).err().unwrap();
        assert!(error.0, "refused by policy");
        assert!(error.1.contains("allowlist"));
        assert_eq!(inner.open_count(), 0);
    }
}
