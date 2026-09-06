//! Cancellation signal for one synchronous execution.
//!
//! Stopping has two independent sources, while a blocking synchronous leg such as a
//! `bash`/`powershell` child process or hook command observes only the flag it polls:
//!
//! * **Run-level:** the user stops the whole model run
//!   ([`crate::state::AppState::cancel_model_run`]).
//! * **Task-level:** the user stops one sidebar task, a workflow skips a step, or a
//!   `Promise.race` cancels its losers.
//!
//! A task can survive across turns, so stopping a task and stopping its parent run
//! remain separate operations. `CancelSignal` combines the sources supplied at its
//! construction point; a poll site asks only [`CancelSignal::cancelled`] and need not
//! distinguish task from run.
//!
//! Construction follows ownership: task turns receive their task flag and top-level
//! turns receive their run flag. Sources must not be dynamically combined from a
//! conversation's current run, because that could stop an unrelated foreground run
//! and kill a surviving background task's synchronous leg.
//!
//! An empty signal ([`CancelSignal::default`]) has no sources and never cancels. It is
//! used by IPC-driven tool execution and direct test paths, not to indicate cancellation.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

/// Flags that can stop one synchronous execution. The container has union semantics,
/// although production construction (`RunModelRequest::round_cancellation`) supplies
/// zero or one owner-scoped source. Cloning is cheap.
#[derive(Clone, Default)]
pub struct CancelSignal {
    flags: Vec<Arc<AtomicBool>>,
}

impl CancelSignal {
    /// A single-source signal.
    pub fn from_flag(flag: Arc<AtomicBool>) -> Self {
        Self { flags: vec![flag] }
    }

    /// Adds a source. `None` means that source is absent, not cancelled, so it leaves
    /// the signal unchanged.
    ///
    /// Production construction now supplies one owner-scoped source, but this method
    /// preserves the container's union contract for future multi-source construction.
    #[allow(dead_code)]
    pub fn with(mut self, flag: Option<Arc<AtomicBool>>) -> Self {
        if let Some(flag) = flag {
            self.flags.push(flag);
        }
        self
    }

    /// Returns cancelled when any source is set.
    pub fn cancelled(&self) -> bool {
        self.flags
            .iter()
            .any(|flag| flag.load(Ordering::Acquire))
    }

    /// Returns whether the signal has no sources. Construction uses this to determine
    /// ownership: a nonempty `RunModelRequest::task_cancel` means a task turn; an
    /// empty signal means a top-level or IPC-driven turn. This is not the inverse of
    /// cancellation: an empty signal never cancels, and a sourced signal may be unset.
    pub fn is_empty(&self) -> bool {
        self.flags.is_empty()
    }
}

impl std::fmt::Debug for CancelSignal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CancelSignal")
            .field("sources", &self.flags.len())
            .field("cancelled", &self.cancelled())
            .finish()
    }
}

/// Compares source identity, not state. `RunModelRequest` derives `PartialEq`, so two
/// requests are equal when they reference the same flags, regardless of whether those
/// flags have subsequently been set.
impl PartialEq for CancelSignal {
    fn eq(&self, other: &Self) -> bool {
        self.flags.len() == other.flags.len()
            && self
                .flags
                .iter()
                .zip(other.flags.iter())
                .all(|(left, right)| Arc::ptr_eq(left, right))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A source-free signal never cancels; IPC-driven and direct test paths use it.
    #[test]
    fn a_signal_without_sources_is_never_cancelled() {
        assert!(!CancelSignal::default().cancelled());
    }

    /// `None` means the source is absent, not cancelled.
    #[test]
    fn merging_an_absent_source_changes_nothing() {
        let signal = CancelSignal::default().with(None);
        assert!(!signal.cancelled());
        assert_eq!(signal, CancelSignal::default());
        assert!(signal.is_empty(), "空信号＝不属于任何任务、也没有运行");
        assert!(
            !CancelSignal::from_flag(Arc::new(AtomicBool::new(false))).is_empty(),
            "有来源但未置位 ≠ 空：归属判据与取消状态是两个问题"
        );
    }

    /// With multiple sources, either set flag cancels. Production construction uses
    /// one owner-scoped source, but this preserves the container's union semantics.
    #[test]
    fn either_source_cancels() {
        let run = Arc::new(AtomicBool::new(false));
        let task = Arc::new(AtomicBool::new(false));
        let signal = CancelSignal::from_flag(Arc::clone(&run)).with(Some(Arc::clone(&task)));

        assert!(!signal.cancelled());
        task.store(true, Ordering::Release);
        assert!(signal.cancelled());
        task.store(false, Ordering::Release);
        assert!(!signal.cancelled());
        run.store(true, Ordering::Release);
        assert!(signal.cancelled());
    }

    /// Signals share flags rather than snapshotting them, so a stop requested after
    /// execution begins remains observable.
    #[test]
    fn a_signal_observes_a_flag_raised_after_it_was_built() {
        let flag = Arc::new(AtomicBool::new(false));
        let signal = CancelSignal::from_flag(Arc::clone(&flag));
        let cloned = signal.clone();

        flag.store(true, Ordering::Release);
        assert!(signal.cancelled());
        assert!(cloned.cancelled(), "克隆共享同一批旗标");
    }

    /// Equality is by identity rather than current state, so a request remains equal
    /// to itself before and after cancellation.
    #[test]
    fn equality_is_by_identity_not_by_current_value() {
        let flag = Arc::new(AtomicBool::new(false));
        let left = CancelSignal::from_flag(Arc::clone(&flag));
        let right = left.clone();
        flag.store(true, Ordering::Release);
        assert_eq!(left, right);

        let other = CancelSignal::from_flag(Arc::new(AtomicBool::new(true)));
        assert_ne!(left, other);
        assert_ne!(left, CancelSignal::default());
    }
}
