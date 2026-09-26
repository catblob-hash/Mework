//! The host's side of one remote session: its output as ordinary readers, its
//! input as an ordinary writer, and its end as something to wait on.
//!
//! The mirror keeps what arrived by offset, exactly like the agent's rings, so
//! a reconnect can resume from where this side actually is and a duplicate
//! chunk is recognized as one. Input is kept too, until the agent confirms it
//! arrived, so keystrokes typed while the link is down are delivered after it
//! returns rather than lost.

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use super::{lock, Link};
use crate::protocol::{ExitInfo, SignalKind, Stream, TerminalSize};
use crate::ring::Ring;

/// Output the host holds for a reader that has not taken it yet.
const HOST_OUTPUT_LIMIT: usize = 16 << 20;
/// Input kept for retransmission. Typing and protocol messages stay far below
/// it; past it the oldest unconfirmed input is given up.
const INPUT_RETAIN: usize = 1 << 20;

pub(super) struct PipeState {
    stdout: Ring,
    stderr: Ring,
    /// What each reader has consumed.
    consumed: [u64; 2],
    pub(super) exit: Option<ExitInfo>,
    /// Bytes the agent reported dropped before they reached this side.
    pub(super) lost: u64,
    /// Why the session can no longer be followed, when the link gave up or the
    /// agent no longer knows it.
    pub(super) failed: Option<String>,
    /// Whether the agent confirmed the spawn.
    pub(super) spawned: bool,
    pub(super) input: InputBuffer,
}

/// Input from `base` to `base + bytes.len()`, of which everything before `sent`
/// has been handed to some connection.
#[derive(Default)]
pub(super) struct InputBuffer {
    base: u64,
    bytes: VecDeque<u8>,
    sent: u64,
    closed: bool,
}

impl InputBuffer {
    fn next(&self) -> u64 {
        self.base + self.bytes.len() as u64
    }

    fn push(&mut self, data: &[u8]) {
        self.bytes.extend(data);
        let overflow = self.bytes.len().saturating_sub(INPUT_RETAIN);
        if overflow > 0 {
            self.bytes.drain(..overflow);
            self.base += overflow as u64;
            self.sent = self.sent.max(self.base);
        }
    }

    /// The agent has everything before `confirmed`; resend from there.
    pub(super) fn rewind(&mut self, confirmed: u64) {
        let confirmed = confirmed.min(self.next());
        if confirmed > self.base {
            let count = (confirmed - self.base) as usize;
            self.bytes.drain(..count);
            self.base = confirmed;
        }
        self.sent = confirmed.max(self.base);
    }

    /// What has not been handed to a connection yet, as offset and bytes.
    pub(super) fn unsent(&self) -> Option<(u64, Vec<u8>)> {
        if self.sent >= self.next() {
            return None;
        }
        let skip = (self.sent - self.base) as usize;
        Some((self.sent, self.bytes.iter().skip(skip).copied().collect()))
    }

    pub(super) fn mark_sent(&mut self, through: u64) {
        self.sent = self.sent.max(through);
    }
}

/// Everything the host knows about one remote session.
pub struct SessionPipe {
    pub(super) sid: String,
    pub(super) state: Mutex<PipeState>,
    pub(super) cond: Condvar,
    /// Held while input is being written, so two writers of one session put
    /// it on the wire in order. Separate from `state`, which the link's
    /// reader needs to deliver output and must never wait on the network for.
    pub(super) sending: Mutex<()>,
}

impl SessionPipe {
    pub(super) fn new(sid: String) -> Self {
        Self {
            sid,
            state: Mutex::new(PipeState {
                stdout: Ring::new(HOST_OUTPUT_LIMIT),
                stderr: Ring::new(HOST_OUTPUT_LIMIT),
                consumed: [0, 0],
                exit: None,
                lost: 0,
                failed: None,
                spawned: false,
                input: InputBuffer::default(),
            }),
            cond: Condvar::new(),
            sending: Mutex::new(()),
        }
    }

    /// How far each output stream has been received.
    pub(super) fn received(&self) -> (u64, u64) {
        let state = lock(&self.state);
        (state.stdout.end(), state.stderr.end())
    }

    /// Takes one output chunk. A chunk that overlaps what is already here is
    /// trimmed to its new part; one that starts past the end means the bytes
    /// in between were lost, and the stream moves over them.
    pub(super) fn deliver(&self, stream: Stream, offset: u64, data: &[u8]) {
        let mut state = lock(&self.state);
        let ring = match stream {
            Stream::Stdout => &mut state.stdout,
            Stream::Stderr => &mut state.stderr,
        };
        let end = ring.end();
        let chunk_end = offset + data.len() as u64;
        if chunk_end <= end {
            return;
        }
        let mut lost = 0;
        if offset > end {
            lost = offset - end;
            ring.skip_to(offset);
            ring.push(data);
        } else {
            ring.push(&data[(end - offset) as usize..]);
        }
        state.lost += lost;
        drop(state);
        self.cond.notify_all();
    }

    pub(super) fn gap(&self, stream: Stream, to: u64) {
        let mut state = lock(&self.state);
        let ring = match stream {
            Stream::Stdout => &mut state.stdout,
            Stream::Stderr => &mut state.stderr,
        };
        let end = ring.end();
        if to > end {
            ring.skip_to(to);
            state.lost += to - end;
        }
        drop(state);
        self.cond.notify_all();
    }

    pub(super) fn finish(&self, exit: ExitInfo) {
        let mut state = lock(&self.state);
        // The exit is final; a copy sent again after a reconnect changes nothing.
        if state.exit.is_none() {
            // Anything the agent no longer holds before the end is lost too.
            if exit.ends.stdout > state.stdout.end() {
                state.stdout.skip_to(exit.ends.stdout);
            }
            if exit.ends.stderr > state.stderr.end() {
                state.stderr.skip_to(exit.ends.stderr);
            }
            state.exit = Some(exit);
        }
        drop(state);
        self.cond.notify_all();
    }

    pub(super) fn fail(&self, reason: &str) {
        let mut state = lock(&self.state);
        if state.exit.is_none() && state.failed.is_none() {
            state.failed = Some(reason.to_owned());
        }
        drop(state);
        self.cond.notify_all();
    }

    /// Output bytes the agent had to drop before they reached the host.
    pub fn lost_bytes(&self) -> u64 {
        lock(&self.state).lost
    }
}

/// A process on the remote machine, used the way a local child is.
pub struct RemoteProcess {
    link: Link,
    pipe: Arc<SessionPipe>,
    pid: u32,
    stdout: Option<SessionReader>,
    stderr: Option<SessionReader>,
}

impl RemoteProcess {
    pub(super) fn new(link: Link, pipe: Arc<SessionPipe>, pid: u32) -> Self {
        let stdout = SessionReader {
            pipe: Arc::clone(&pipe),
            stream: Stream::Stdout,
        };
        let stderr = SessionReader {
            pipe: Arc::clone(&pipe),
            stream: Stream::Stderr,
        };
        Self {
            link,
            pipe,
            pid,
            stdout: Some(stdout),
            stderr: Some(stderr),
        }
    }

    /// The process id on the remote machine.
    pub fn pid(&self) -> u32 {
        self.pid
    }

    pub fn take_stdout(&mut self) -> Option<SessionReader> {
        self.stdout.take()
    }

    pub fn take_stderr(&mut self) -> Option<SessionReader> {
        self.stderr.take()
    }

    /// A writer for the process's input. Writes are queued and delivered in
    /// order across reconnects; they never block on the network for long.
    pub fn stdin(&self) -> SessionWriter {
        SessionWriter {
            link: self.link.clone(),
            pipe: Arc::clone(&self.pipe),
        }
    }

    pub fn close_stdin(&self) {
        lock(&self.pipe.state).input.closed = true;
        self.link.send_input(&self.pipe);
        self.link.request_detached(crate::protocol::Op::CloseStdin {
            sid: self.pipe.sid.clone(),
        });
    }

    /// The exit, if the process has ended and all its output has arrived.
    pub fn try_wait(&self) -> io::Result<Option<ExitInfo>> {
        self.wait_timeout(Duration::ZERO)
    }

    /// Waits up to `timeout` for the exit. While the link is reconnecting the
    /// process is simply still running as far as this side knows; an error
    /// means the host can no longer find out how it ended.
    pub fn wait_timeout(&self, timeout: Duration) -> io::Result<Option<ExitInfo>> {
        let deadline = Instant::now() + timeout;
        let mut state = lock(&self.pipe.state);
        loop {
            if let Some(exit) = &state.exit {
                return Ok(Some(exit.clone()));
            }
            if let Some(reason) = &state.failed {
                return Err(io::Error::new(io::ErrorKind::ConnectionAborted, reason.clone()));
            }
            let now = Instant::now();
            if now >= deadline {
                return Ok(None);
            }
            state = self
                .pipe
                .cond
                .wait_timeout(state, deadline - now)
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .0;
        }
    }

    pub fn wait(&self) -> io::Result<ExitInfo> {
        loop {
            if let Some(exit) = self.wait_timeout(Duration::from_secs(3600))? {
                return Ok(exit);
            }
        }
    }

    /// Signals the process's whole tree on the machine. Queued if the link is
    /// down; the agent acts on it when the link returns.
    pub fn signal(&self, signal: SignalKind) {
        self.link.request_detached(crate::protocol::Op::Signal {
            sid: self.pipe.sid.clone(),
            signal,
        });
    }

    pub fn kill(&self) {
        self.signal(SignalKind::Kill);
    }

    pub fn resize(&self, size: TerminalSize) {
        self.link.request_detached(crate::protocol::Op::Resize {
            sid: self.pipe.sid.clone(),
            size,
        });
    }

    /// Output bytes the agent had to drop before they reached the host.
    pub fn lost_bytes(&self) -> u64 {
        self.pipe.lost_bytes()
    }
}

impl Drop for RemoteProcess {
    /// Nobody can collect the output of a handle that is gone, so the agent
    /// is told to stop keeping it: its output is discarded and, if it still
    /// runs, it is ended.
    fn drop(&mut self) {
        self.link.release(&self.pipe.sid);
    }
}

impl std::fmt::Debug for RemoteProcess {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RemoteProcess")
            .field("sid", &self.pipe.sid)
            .field("pid", &self.pid)
            .finish()
    }
}

/// One output stream of a remote session, read like a pipe: it blocks for
/// data, returns end of file once the process ended and everything it wrote
/// was read, and fails only when the host can no longer follow the session.
pub struct SessionReader {
    pipe: Arc<SessionPipe>,
    stream: Stream,
}

impl SessionReader {
    fn index(&self) -> usize {
        match self.stream {
            Stream::Stdout => 0,
            Stream::Stderr => 1,
        }
    }
}

impl Read for SessionReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        let index = self.index();
        let mut state = lock(&self.pipe.state);
        loop {
            let consumed = state.consumed[index];
            let ring = match self.stream {
                Stream::Stdout => &mut state.stdout,
                Stream::Stderr => &mut state.stderr,
            };
            if consumed < ring.end() {
                let (from, bytes) = ring.read_from(consumed, buffer.len());
                if bytes.is_empty() {
                    // All that lies ahead is a gap: those bytes are gone,
                    // and counted as lost. Returning the empty read would
                    // look like the end of the stream to the caller.
                    state.consumed[index] = from;
                    continue;
                }
                let count = bytes.len();
                buffer[..count].copy_from_slice(&bytes);
                let next = from + count as u64;
                ring.consume_to(next);
                state.consumed[index] = next;
                return Ok(count);
            }
            if let Some(exit) = &state.exit {
                if consumed >= exit.ends.get(self.stream) {
                    return Ok(0);
                }
            }
            if let Some(reason) = &state.failed {
                return Err(io::Error::new(io::ErrorKind::ConnectionAborted, reason.clone()));
            }
            state = self
                .pipe
                .cond
                .wait(state)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }
}

/// The input of a remote session.
#[derive(Clone)]
pub struct SessionWriter {
    link: Link,
    pipe: Arc<SessionPipe>,
}

impl Write for SessionWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        {
            let mut state = lock(&self.pipe.state);
            if let Some(reason) = &state.failed {
                return Err(io::Error::new(io::ErrorKind::BrokenPipe, reason.clone()));
            }
            if state.exit.is_some() || state.input.closed {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "the remote process no longer takes input",
                ));
            }
            state.input.push(buffer);
        }
        self.link.send_input(&self.pipe);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{ExitReason, StreamEnds};

    /// A gap and the output after it arrive as two frames. A reader woken by
    /// the first has nothing to read yet, which is not the end of the stream.
    #[test]
    fn a_reader_woken_by_a_gap_waits_for_the_output_after_it() {
        let pipe = Arc::new(SessionPipe::new("s1".into()));
        let mut reader = SessionReader {
            pipe: Arc::clone(&pipe),
            stream: Stream::Stdout,
        };
        pipe.gap(Stream::Stdout, 10);
        let feeder = {
            let pipe = Arc::clone(&pipe);
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(100));
                pipe.deliver(Stream::Stdout, 10, b"tail");
                pipe.finish(ExitInfo {
                    code: Some(0),
                    signal: None,
                    reason: ExitReason::Exited,
                    ends: StreamEnds { stdout: 14, stderr: 0 },
                    runtime_ms: None,
                });
            })
        };
        let mut received = Vec::new();
        reader.read_to_end(&mut received).unwrap();
        feeder.join().unwrap();
        assert_eq!(received, b"tail");
        assert_eq!(pipe.lost_bytes(), 10);
    }
}
