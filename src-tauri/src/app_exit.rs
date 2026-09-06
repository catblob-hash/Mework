use std::sync::{
    atomic::{AtomicBool, AtomicU8, Ordering},
    Arc,
};

const EXIT_RUNNING: u8 = 0;
const EXIT_DRAINING: u8 = 1;
const EXIT_READY: u8 = 2;

/// Coordinates a deferred application exit without ever waiting on the main event loop.
///
/// An exit callback synchronously changes `Running -> Draining` and then moves the slow wait to a
/// worker. A failed worker returns to `Running`; a successful worker publishes `Ready` before
/// asking Tauri to exit again, so the second pass through the same callback stops preventing it.
#[derive(Clone, Default)]
pub struct AppExitCoordinator {
    inner: Arc<AppExitCoordinatorInner>,
}

#[derive(Default)]
struct AppExitCoordinatorInner {
    phase: AtomicU8,
    cleanup_started: AtomicBool,
}

impl AppExitCoordinator {
    #[cfg_attr(not(feature = "browser-dev"), allow(dead_code))]
    pub fn try_begin_draining(&self) -> bool {
        self.inner
            .phase
            .compare_exchange(
                EXIT_RUNNING,
                EXIT_DRAINING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    #[cfg_attr(not(feature = "browser-dev"), allow(dead_code))]
    pub fn abort_draining(&self) -> bool {
        self.inner
            .phase
            .compare_exchange(
                EXIT_DRAINING,
                EXIT_RUNNING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    #[cfg_attr(not(feature = "browser-dev"), allow(dead_code))]
    pub fn mark_ready(&self) {
        self.inner.phase.store(EXIT_READY, Ordering::Release);
    }

    #[cfg_attr(not(feature = "browser-dev"), allow(dead_code))]
    pub fn is_ready(&self) -> bool {
        self.inner.phase.load(Ordering::Acquire) == EXIT_READY
    }

    /// Publish readiness only after the persistence/release barrier confirmed.
    pub fn finish_draining<T>(&self, barrier: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
        match barrier() {
            Ok(value) => {
                self.mark_ready();
                Ok(value)
            }
            Err(error) => {
                self.abort_draining();
                Err(error)
            }
        }
    }

    #[cfg(test)]
    pub fn is_draining(&self) -> bool {
        self.inner.phase.load(Ordering::Acquire) == EXIT_DRAINING
    }

    /// Returns true exactly once for the process-level, non-preventable `RunEvent::Exit` cleanup.
    pub fn begin_cleanup(&self) -> bool {
        !self.inner.cleanup_started.swap(true, Ordering::AcqRel)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{Arc, Barrier},
        thread,
    };

    use super::AppExitCoordinator;

    #[test]
    fn only_one_exit_surface_can_start_a_drain() {
        let coordinator = AppExitCoordinator::default();
        let barrier = Arc::new(Barrier::new(9));
        let mut workers = Vec::new();
        for _ in 0..8 {
            let coordinator = coordinator.clone();
            let barrier = barrier.clone();
            workers.push(thread::spawn(move || {
                barrier.wait();
                coordinator.try_begin_draining()
            }));
        }
        barrier.wait();
        let winners = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .filter(|won| *won)
            .count();
        assert_eq!(winners, 1);
        assert!(coordinator.is_draining());
    }

    #[test]
    fn an_aborted_drain_can_resume_and_a_later_attempt_can_finish() {
        let coordinator = AppExitCoordinator::default();
        assert!(coordinator.try_begin_draining());
        assert!(coordinator.abort_draining());
        assert!(!coordinator.is_draining());
        assert!(!coordinator.is_ready());

        assert!(coordinator.try_begin_draining());
        coordinator.mark_ready();
        assert!(coordinator.is_ready());
        assert!(!coordinator.abort_draining());
        assert!(!coordinator.try_begin_draining());
    }

    #[test]
    fn failed_persistence_never_publishes_ready_and_can_be_retried() {
        let coordinator = AppExitCoordinator::default();
        assert!(coordinator.try_begin_draining());
        let result = coordinator.finish_draining(|| {
            assert!(!coordinator.is_ready());
            assert!(!coordinator.try_begin_draining());
            Err::<(), _>("disk full".to_owned())
        });
        assert_eq!(result.unwrap_err(), "disk full");
        assert!(!coordinator.is_ready());
        assert!(coordinator.try_begin_draining());
        coordinator.finish_draining(|| {
            assert!(!coordinator.is_ready());
            Ok(())
        }).unwrap();
        assert!(coordinator.is_ready());
    }

    #[test]
    fn final_cleanup_is_single_flight() {
        let coordinator = AppExitCoordinator::default();
        assert!(coordinator.begin_cleanup());
        assert!(!coordinator.begin_cleanup());
        assert!(!coordinator.clone().begin_cleanup());
    }
}
