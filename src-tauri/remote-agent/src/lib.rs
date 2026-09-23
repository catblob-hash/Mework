//! Mework's persistent presence on a remote machine, and the host's link to it.
//!
//! A conversation's workspaces can live on several machines at once, and the
//! host — model calls, approvals, conversation state — stays on the user's own
//! computer. What used to cross to an SSH machine was one `ssh` process per
//! operation, each handing a command line to whatever login shell the account
//! had. That paid a full SSH handshake per file read, depended on the login
//! shell's dialect, and tied every remote process to the SSH session that
//! started it: a dropped connection took the command, the terminal and the
//! language server down with it.
//!
//! This crate replaces that with a small program the host uploads to the
//! machine once and keeps running there:
//!
//! * the **daemon** ([`agent`]) owns every process the host starts on the
//!   machine. It is detached from any SSH session, so a dropped link only
//!   pauses the conversation with it; the processes keep running and their
//!   output keeps accumulating until the host is back.
//! * the **proxy** is the one thing an SSH session runs: it connects the SSH
//!   channel's stdin and stdout to the daemon's machine-local socket. The login
//!   shell's only job is to start it, which every shell can do.
//! * the **link** ([`client`]) is the host's end: one multiplexed stream per
//!   machine, kept alive by heartbeats, re-established by itself after a drop,
//!   and resumed exactly where it stopped ([`protocol`] describes how).
//!
//! Reclamation runs the other way. A session belongs to the host that started
//! it; when that host goes quiet for longer than its policy allows, or comes
//! back as a new process, the daemon ends the session's whole process group,
//! and a daemon with nothing left to do exits by itself.

pub mod protocol;
pub mod ring;

#[cfg(feature = "agent")]
pub mod agent;

#[cfg(feature = "client")]
pub mod client;

/// The version the host and the agent compare, next to the build digest.
pub const AGENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The executable's name on a Unix machine; Windows adds `.exe`.
pub const AGENT_BINARY: &str = "mework-remote";
