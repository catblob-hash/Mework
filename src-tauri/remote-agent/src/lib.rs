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

pub mod files;
pub mod protocol;
pub mod ring;
pub mod tunnel;

#[cfg(feature = "agent")]
pub mod agent;

#[cfg(feature = "client")]
pub mod client;

/// The version the host and the agent compare, next to the build digest.
pub const AGENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The executable's name on a Unix machine; Windows adds `.exe`.
pub const AGENT_BINARY: &str = "mework-remote";

/// The source this crate was built from: a digest of the agent's manifest,
/// build script and `src/` (see `build.rs`).
///
/// [`AGENT_VERSION`] does not change while the agent is being developed, and a
/// build's digest says which bytes it is, not what it does, so neither tells a
/// host whether a build it holds is the agent it was compiled against. This
/// does: the host links this crate and carries the same value as every agent
/// built from the same source, and a different one from any other. A host
/// installs only builds whose [`SOURCE_MARKER`] names its own `SOURCE_ID`.
pub const SOURCE_ID: &str = env!("MEWORK_AGENT_SOURCE_ID");

const SOURCE_MARKER_PREFIX: &str = "mework-remote-source:";

/// [`SOURCE_ID`] spelled out, whole, in every agent executable, so a host can
/// tell which source a build was made from by reading its bytes — it cannot
/// run a build made for another platform.
pub static SOURCE_MARKER: &str = concat!("mework-remote-source:", env!("MEWORK_AGENT_SOURCE_ID"));

/// [`SOURCE_ID`] read out of [`SOURCE_MARKER`] at run time. An agent reports
/// this rather than the constant so that the marker is what its code uses, and
/// no optimization can leave the executable without it.
pub fn marked_source_id() -> &'static str {
    &std::hint::black_box(SOURCE_MARKER)[SOURCE_MARKER_PREFIX.len()..]
}

/// The [`SOURCE_ID`] an agent executable was built with, found in its bytes;
/// `None` for a build older than source identities, or not an agent at all.
pub fn source_of_executable(bytes: &[u8]) -> Option<&str> {
    let prefix = SOURCE_MARKER_PREFIX.as_bytes();
    let mut rest = bytes;
    while let Some(at) = rest.windows(prefix.len()).position(|window| window == prefix) {
        let candidate = &rest[at + prefix.len()..];
        if let Some(id) = candidate.get(..SOURCE_ID.len()) {
            if id.iter().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f')) {
                return std::str::from_utf8(id).ok();
            }
        }
        rest = candidate;
    }
    None
}

#[cfg(test)]
mod source_tests {
    use super::*;

    #[test]
    fn the_source_id_names_a_sha256() {
        assert_eq!(SOURCE_ID.len(), 64);
        assert!(SOURCE_ID.bytes().all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()));
        assert_eq!(marked_source_id(), SOURCE_ID);
    }

    #[test]
    fn an_executable_is_known_by_the_marker_in_its_bytes() {
        let mut build = b"\0\x7fELF...mework-remote-source:not-a-digest...".to_vec();
        build.extend_from_slice(SOURCE_MARKER.as_bytes());
        build.extend_from_slice(b"\0rest of the binary");
        assert_eq!(source_of_executable(&build), Some(SOURCE_ID));
        assert_eq!(source_of_executable(b"an agent from before source identities"), None);
        let other = format!("{SOURCE_MARKER_PREFIX}{}", "0".repeat(64));
        assert_eq!(source_of_executable(other.as_bytes()), Some("0".repeat(64).as_str()));
    }
}
