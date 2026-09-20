//! Rust half of the browser-dev controlled E2E restart/shutdown handshake.
//!
//! The Node wrapper (`scripts/browser-dev.mjs`) restarts a Rust backend child — or accepts its
//! final shutdown — only when that exact child printed a request marker, a WebView2 release barrier
//! marker and a post-cleanup commit marker, and then exited with an expected code. Neither half may
//! be inferred from the other: an exit code alone can never turn an ordinary crash into a restart
//! loop, and a request marker alone can never turn a later crash into a controlled exit.
//!
//! This module owns the whole latch so the contract can be exercised without a Tauri application.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

/// `run_return` propagates this for an accepted restart. The wrapper also admits 0 and 101, because
/// an `App::run`-based shutdown can report success and some Cargo/platform paths rewrap the code —
/// but only together with the complete marker handshake.
pub(crate) const RESTART_EXIT_CODE: i32 = 75;
pub(crate) const FINAL_SHUTDOWN_EXIT_CODE: i32 = 0;
/// Deliberately outside the wrapper's accepted restart set. A failed release barrier must stop the
/// run rather than restart into a backend whose predecessor may still hold WebView2 processes open
/// over the isolated data directory.
pub(crate) const RELEASE_FAILURE_EXIT_CODE: i32 = 76;

/// Emitted once the barrier has observed every registered WebView2 browser process exit normally.
/// Like the request and commit markers it is fixed host text carrying no page, profile or instance
/// data, so it can be matched verbatim by the wrapper.
pub(crate) const RELEASE_BARRIER_MARKER: &str = "[browser-dev] E2E WebView2 release barrier passed";

const RESTART_REQUEST_MARKER: &str = "[browser-dev] E2E controlled backend restart requested";
const RESTART_COMMIT_MARKER: &str = "[browser-dev] E2E controlled backend restart committed";
const FINAL_SHUTDOWN_REQUEST_MARKER: &str = "[browser-dev] E2E controlled final shutdown requested";
const FINAL_SHUTDOWN_COMMIT_MARKER: &str = "[browser-dev] E2E controlled final shutdown committed";

const REQUEST_NONE: u8 = 0;
const REQUEST_RESTART: u8 = 1;
const REQUEST_FINAL_SHUTDOWN: u8 = 2;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ControlledExit {
    /// Mid-run restart: the wrapper keeps the Vite session and spawns a replacement backend.
    Restart,
    /// Terminal shutdown: the wrapper stops the whole development session successfully.
    FinalShutdown,
}

impl ControlledExit {
    fn code(self) -> u8 {
        match self {
            Self::Restart => REQUEST_RESTART,
            Self::FinalShutdown => REQUEST_FINAL_SHUTDOWN,
        }
    }

    fn from_code(code: u8) -> Option<Self> {
        match code {
            REQUEST_RESTART => Some(Self::Restart),
            REQUEST_FINAL_SHUTDOWN => Some(Self::FinalShutdown),
            _ => None,
        }
    }

    pub(crate) fn request_marker(self) -> &'static str {
        match self {
            Self::Restart => RESTART_REQUEST_MARKER,
            Self::FinalShutdown => FINAL_SHUTDOWN_REQUEST_MARKER,
        }
    }

    fn commit_marker(self) -> &'static str {
        match self {
            Self::Restart => RESTART_COMMIT_MARKER,
            Self::FinalShutdown => FINAL_SHUTDOWN_COMMIT_MARKER,
        }
    }

    /// The code passed to `AppHandle::exit`, which starts the ordinary graceful-exit path rather
    /// than bypassing document, import and browser cleanup.
    pub(crate) fn requested_exit_code(self) -> i32 {
        match self {
            Self::Restart => RESTART_EXIT_CODE,
            Self::FinalShutdown => FINAL_SHUTDOWN_EXIT_CODE,
        }
    }
}

#[derive(Debug)]
pub(crate) struct ControlledExitHandshake {
    request: AtomicU8,
    require_browser_process: AtomicBool,
    release_barrier_passed: AtomicBool,
    restart_committed: AtomicBool,
    final_shutdown_committed: AtomicBool,
}

impl ControlledExitHandshake {
    pub(crate) const fn new() -> Self {
        Self {
            request: AtomicU8::new(REQUEST_NONE),
            require_browser_process: AtomicBool::new(false),
            release_barrier_passed: AtomicBool::new(false),
            restart_committed: AtomicBool::new(false),
            final_shutdown_committed: AtomicBool::new(false),
        }
    }

    /// Claims this backend instance's single controlled-exit slot. Returns `false` when one was
    /// already accepted, so neither a repeated request nor a renderer still talking to a previous
    /// backend generation can queue a second exit.
    pub(crate) fn accept(&self, exit: ControlledExit) -> bool {
        self.request
            .compare_exchange(
                REQUEST_NONE,
                exit.code(),
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    pub(crate) fn accepted(&self) -> Option<ControlledExit> {
        ControlledExit::from_code(self.request.load(Ordering::Acquire))
    }

    /// Demands that the release barrier observe at least one real browser process, instead of
    /// passing merely because no page happened to be open at exit time.
    pub(crate) fn set_require_browser_process(&self) {
        self.require_browser_process.store(true, Ordering::Release);
    }

    pub(crate) fn requires_browser_process(&self) -> bool {
        self.require_browser_process.load(Ordering::Acquire)
    }

    pub(crate) fn pass_release_barrier(&self) {
        self.release_barrier_passed.store(true, Ordering::Release);
    }

    /// Records the commit half of the handshake and returns the marker to print, or `None` when no
    /// controlled exit was accepted or the release barrier did not pass. Callers reach this only
    /// after normal Tauri exit and final host cleanup.
    pub(crate) fn commit(&self) -> Option<&'static str> {
        if !self.release_barrier_passed.load(Ordering::Acquire) {
            return None;
        }
        let exit = self.accepted()?;
        match exit {
            ControlledExit::Restart => self.restart_committed.store(true, Ordering::Release),
            ControlledExit::FinalShutdown => {
                self.final_shutdown_committed.store(true, Ordering::Release)
            }
        }
        Some(exit.commit_marker())
    }

    /// The committed handshake — not the runtime code — decides what the wrapper sees. Tauri
    /// reports 0 for a shutdown it considers successful, including the one a restart requested.
    pub(crate) fn process_exit_code(&self, runtime_exit_code: i32) -> i32 {
        if self.restart_committed.load(Ordering::Acquire) {
            RESTART_EXIT_CODE
        } else if self.final_shutdown_committed.load(Ordering::Acquire) {
            FINAL_SHUTDOWN_EXIT_CODE
        } else {
            runtime_exit_code
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WRAPPER: &str = include_str!("../../scripts/browser-dev.mjs");
    const WRAPPER_LIFECYCLE: &str = include_str!("../../scripts/browser-dev-lifecycle.mjs");

    fn passed(exit: ControlledExit) -> ControlledExitHandshake {
        let handshake = ControlledExitHandshake::new();
        assert!(handshake.accept(exit));
        handshake.pass_release_barrier();
        handshake
    }

    #[test]
    fn only_one_controlled_exit_is_ever_accepted() {
        let handshake = ControlledExitHandshake::new();
        assert_eq!(handshake.accepted(), None);

        assert!(handshake.accept(ControlledExit::Restart));
        assert!(!handshake.accept(ControlledExit::Restart));
        assert!(!handshake.accept(ControlledExit::FinalShutdown));
        assert_eq!(handshake.accepted(), Some(ControlledExit::Restart));
    }

    #[test]
    fn the_browser_process_requirement_stays_off_until_a_caller_raises_it() {
        let handshake = ControlledExitHandshake::new();
        assert!(!handshake.requires_browser_process());
        handshake.set_require_browser_process();
        assert!(handshake.requires_browser_process());
    }

    #[test]
    fn a_failed_release_barrier_commits_nothing_and_keeps_the_failure_code() {
        for exit in [ControlledExit::Restart, ControlledExit::FinalShutdown] {
            let handshake = ControlledExitHandshake::new();
            assert!(handshake.accept(exit));
            assert_eq!(handshake.commit(), None);
            assert_eq!(
                handshake.process_exit_code(RELEASE_FAILURE_EXIT_CODE),
                RELEASE_FAILURE_EXIT_CODE
            );
        }
    }

    #[test]
    fn an_unrequested_exit_commits_nothing_even_after_a_passing_barrier() {
        let handshake = ControlledExitHandshake::new();
        handshake.pass_release_barrier();
        assert_eq!(handshake.commit(), None);
        assert_eq!(handshake.process_exit_code(1), 1);
        assert_eq!(handshake.process_exit_code(0), 0);
    }

    #[test]
    fn a_committed_restart_reports_the_wrapper_restart_code() {
        let handshake = passed(ControlledExit::Restart);
        assert_eq!(handshake.commit(), Some(RESTART_COMMIT_MARKER));
        // Tauri reported success for the very shutdown the restart asked for.
        assert_eq!(handshake.process_exit_code(0), RESTART_EXIT_CODE);
    }

    #[test]
    fn a_committed_final_shutdown_reports_success() {
        let handshake = passed(ControlledExit::FinalShutdown);
        assert_eq!(handshake.commit(), Some(FINAL_SHUTDOWN_COMMIT_MARKER));
        assert_eq!(
            handshake.process_exit_code(RESTART_EXIT_CODE),
            FINAL_SHUTDOWN_EXIT_CODE
        );
    }

    #[test]
    fn request_and_commit_markers_are_distinct_per_controlled_exit() {
        let markers = [
            ControlledExit::Restart.request_marker(),
            ControlledExit::Restart.commit_marker(),
            ControlledExit::FinalShutdown.request_marker(),
            ControlledExit::FinalShutdown.commit_marker(),
            RELEASE_BARRIER_MARKER,
        ];
        for (index, marker) in markers.iter().enumerate() {
            assert!(!marker.is_empty());
            assert!(!markers[index + 1..].contains(marker), "duplicate {marker}");
        }
    }

    #[test]
    fn every_marker_is_matched_verbatim_by_the_node_wrapper() {
        for marker in [
            ControlledExit::Restart.request_marker(),
            ControlledExit::Restart.commit_marker(),
            ControlledExit::FinalShutdown.request_marker(),
            ControlledExit::FinalShutdown.commit_marker(),
            RELEASE_BARRIER_MARKER,
        ] {
            assert!(
                WRAPPER.contains(&format!("\"{marker}\"")),
                "scripts/browser-dev.mjs no longer matches {marker}"
            );
        }
    }

    #[test]
    fn the_release_failure_code_stays_outside_the_wrapper_restart_set() {
        const ACCEPTED_RESTART_CODES: &str = "new Set([0, 75, 101])";
        assert!(
            WRAPPER_LIFECYCLE.contains(ACCEPTED_RESTART_CODES),
            "scripts/browser-dev-lifecycle.mjs changed its accepted restart codes"
        );
        assert!(ACCEPTED_RESTART_CODES.contains(&RESTART_EXIT_CODE.to_string()));
        assert!(ACCEPTED_RESTART_CODES.contains(&FINAL_SHUTDOWN_EXIT_CODE.to_string()));
        assert!(!ACCEPTED_RESTART_CODES.contains(&RELEASE_FAILURE_EXIT_CODE.to_string()));
    }
}
