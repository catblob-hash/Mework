//! Second-launch activation for a tray-resident instance.
//!
//! A new Mework process that loses the app-data lease normally means the user
//! double-clicked the shortcut while the earlier instance lives on in the tray
//! with its window hidden. Rather than fail, the newcomer asks that instance to
//! bring its window forward and then exits quietly.
//!
//! Windows only. The channel is a named auto-reset event in the per-logon
//! `Local\` namespace, named from the app-data document path so installs with
//! separate data never wake each other. The signal carries no payload and its
//! only effect is to reveal the main window, so any process in the session may
//! raise it without gaining anything. Other platforms have no channel and keep
//! reporting the conflict.
//!
//! The listener is paused while the resident instance drains for exit: the
//! event object disappears, so a launch that arrives during the drain finds no
//! one to wake, waits for the lease instead, and starts normally once the old
//! process is gone.

use std::path::Path;

#[cfg(windows)]
use std::sync::{Arc, Mutex};

use crate::document_store::ProcessAuthorityError;

/// A permanently signaled activation event (a foreign process holding the
/// name) would otherwise turn the listener into a busy loop; this also folds a
/// double-clicked shortcut into one activation.
#[cfg(windows)]
const ACTIVATION_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(250);

/// How often a launch that found the lease taken re-checks both channels.
const HANDOVER_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);

/// What a launch should do after negotiating for the app-data lease.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum LaunchHandover {
    /// This process holds the lease and starts normally.
    Acquired,
    /// A resident instance was told to show its window; this process exits quietly.
    ResidentSignaled,
    /// Nobody answered and the lease never freed, or acquisition failed outright.
    Unresolved(ProcessAuthorityError),
}

/// Resolves a launch against whoever holds the lease on `document_path`.
///
/// A contended lease has two live explanations, and which one applies can
/// change while we wait: the holder may still be starting up (lease taken,
/// listener not yet installed), or on its way out (listener paused, lease about
/// to free). So every poll tries both — acquire the lease, else wake the
/// resident — until one succeeds or `timeout` passes. The first attempt is
/// immediate, so the ordinary "window closed into the tray" case never sleeps.
pub(crate) fn negotiate_launch(
    document_path: &Path,
    timeout: std::time::Duration,
    mut acquire: impl FnMut() -> Result<(), ProcessAuthorityError>,
) -> LaunchHandover {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match acquire() {
            Ok(()) => return LaunchHandover::Acquired,
            Err(ProcessAuthorityError::Contended) => {
                if signal_running_instance(document_path) {
                    return LaunchHandover::ResidentSignaled;
                }
                if std::time::Instant::now() >= deadline {
                    return LaunchHandover::Unresolved(ProcessAuthorityError::Contended);
                }
                std::thread::sleep(HANDOVER_POLL_INTERVAL);
            }
            Err(error) => return LaunchHandover::Unresolved(error),
        }
    }
}

#[cfg(windows)]
fn event_name(document_path: &Path) -> Vec<u16> {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(document_path.to_string_lossy().as_bytes());
    let hex = digest
        .iter()
        .take(16)
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("Local\\Mework.ActivateWindow.{hex}")
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect()
}

/// A kernel event handle, closed exactly once when its last owner drops. Event
/// handles may be waited on and signaled from any thread.
#[cfg(windows)]
struct OwnedHandle(windows_sys::Win32::Foundation::HANDLE);

#[cfg(windows)]
unsafe impl Send for OwnedHandle {}
#[cfg(windows)]
unsafe impl Sync for OwnedHandle {}

#[cfg(windows)]
impl OwnedHandle {
    /// Going through a method makes a closure capture the whole wrapper (which
    /// is `Send`) rather than the bare pointer field (which is not).
    fn raw(&self) -> windows_sys::Win32::Foundation::HANDLE {
        self.0
    }
}

#[cfg(windows)]
impl Drop for OwnedHandle {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

#[cfg(windows)]
struct RunningListener {
    stop: Arc<OwnedHandle>,
    thread: std::thread::JoinHandle<()>,
}

#[cfg(windows)]
impl RunningListener {
    fn stop(self) {
        unsafe {
            windows_sys::Win32::System::Threading::SetEvent(self.stop.raw());
        }
        let _ = self.thread.join();
    }
}

#[cfg(windows)]
struct ListenerState {
    name: Vec<u16>,
    on_activate: Arc<dyn Fn() + Send + Sync>,
    running: Option<RunningListener>,
}

/// Owns the listener thread; dropping it stops the thread.
pub(crate) struct ActivationListener {
    #[cfg(windows)]
    state: Mutex<ListenerState>,
}

impl ActivationListener {
    /// Stops answering until [`Self::resume`]. The named event goes away with
    /// the thread, so a concurrent launch sees no listener and waits for the
    /// lease instead of waking a process that is leaving.
    pub(crate) fn pause(&self) {
        #[cfg(windows)]
        {
            // Held across the join so a concurrent resume cannot recreate the
            // name before the old thread has closed its handle.
            let mut state = self.lock();
            if let Some(running) = state.running.take() {
                running.stop();
            }
        }
    }

    /// Starts answering again after an exit was abandoned. A no-op while running.
    pub(crate) fn resume(&self) -> Result<(), String> {
        #[cfg(windows)]
        {
            let mut state = self.lock();
            if state.running.is_none() {
                state.running = Some(start(&state.name, state.on_activate.clone())?);
            }
        }
        Ok(())
    }

    #[cfg(windows)]
    fn lock(&self) -> std::sync::MutexGuard<'_, ListenerState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl Drop for ActivationListener {
    fn drop(&mut self) {
        self.pause();
    }
}

#[cfg(windows)]
fn start(
    name: &[u16],
    on_activate: Arc<dyn Fn() + Send + Sync>,
) -> Result<RunningListener, String> {
    use windows_sys::Win32::{
        Foundation::{GetLastError, SetLastError, ERROR_ALREADY_EXISTS, WAIT_OBJECT_0},
        System::Threading::{CreateEventW, WaitForMultipleObjects, INFINITE},
    };

    // Auto-reset: each SetEvent wakes exactly one wait. The last error is
    // cleared first so a stale ERROR_ALREADY_EXISTS cannot survive a fresh create.
    let activate = unsafe {
        SetLastError(0);
        CreateEventW(std::ptr::null(), 0, 0, name.as_ptr())
    };
    if activate.is_null() {
        return Err(format!("无法创建实例激活事件: 错误码 {}", unsafe {
            GetLastError()
        }));
    }
    let activate = OwnedHandle(activate);
    // Only this process may hold this name while it owns the lease, so an
    // existing object belongs to a foreign process whose reset mode and
    // signaling we do not control; waiting on it would be waiting on them.
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        return Err("实例激活事件已被其他进程占用，拒绝监听".to_owned());
    }
    // Manual-reset and private: once set, every wait on it returns until the
    // handle is gone, so the thread cannot miss the stop request.
    let stop = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
    if stop.is_null() {
        return Err(format!("无法创建实例激活停止事件: 错误码 {}", unsafe {
            GetLastError()
        }));
    }
    let stop = Arc::new(OwnedHandle(stop));
    let stop_for_thread = stop.clone();

    let thread = std::thread::Builder::new()
        .name("mework-instance-activation".to_owned())
        .spawn(move || {
            // A wait-any returns the lowest signaled index, so listing `stop`
            // first lets it win even against a permanently signaled activation.
            let handles = [stop_for_thread.raw(), activate.raw()];
            loop {
                let waited = unsafe {
                    WaitForMultipleObjects(handles.len() as u32, handles.as_ptr(), 0, INFINITE)
                };
                if waited != WAIT_OBJECT_0 + 1 {
                    // Stop requested, or the wait itself failed: either way stop listening.
                    break;
                }
                on_activate();
                std::thread::sleep(ACTIVATION_DEBOUNCE);
            }
        })
        .map_err(|error| format!("无法启动实例激活监听线程: {error}"))?;

    Ok(RunningListener { stop, thread })
}

/// Starts waiting for activation requests aimed at the instance that holds
/// the lease on `document_path`. `on_activate` runs on the listener thread
/// once per request and must hand any UI work to the main thread itself.
#[cfg(windows)]
pub(crate) fn listen(
    document_path: &Path,
    on_activate: impl Fn() + Send + Sync + 'static,
) -> Result<ActivationListener, String> {
    let name = event_name(document_path);
    let on_activate: Arc<dyn Fn() + Send + Sync> = Arc::new(on_activate);
    let running = start(&name, on_activate.clone())?;
    Ok(ActivationListener {
        state: Mutex::new(ListenerState {
            name,
            on_activate,
            running: Some(running),
        }),
    })
}

#[cfg(not(windows))]
pub(crate) fn listen(
    _document_path: &Path,
    _on_activate: impl Fn() + Send + Sync + 'static,
) -> Result<ActivationListener, String> {
    Ok(ActivationListener {})
}

/// Asks the instance holding the lease on `document_path` to show its window.
/// Returns false when no such listener exists (an older Mework, an instance
/// that is already leaving, or a platform without the channel), in which case
/// the caller waits for the lease or reports the conflict.
#[cfg(windows)]
pub(crate) fn signal_running_instance(document_path: &Path) -> bool {
    use windows_sys::Win32::{
        Foundation::CloseHandle,
        System::Threading::{OpenEventW, SetEvent, EVENT_MODIFY_STATE},
        UI::WindowsAndMessaging::{AllowSetForegroundWindow, ASFW_ANY},
    };

    let name = event_name(document_path);
    let handle = unsafe { OpenEventW(EVENT_MODIFY_STATE, 0, name.as_ptr()) };
    if handle.is_null() {
        return false;
    }
    // This process was just launched by the user, so it holds the right to set
    // the foreground window. Hand that right on before waking the resident
    // instance; without it Windows refuses its SetForegroundWindow and only
    // flashes the taskbar button.
    unsafe {
        AllowSetForegroundWindow(ASFW_ANY);
    }
    let signaled = unsafe { SetEvent(handle) } != 0;
    unsafe {
        CloseHandle(handle);
    }
    signaled
}

#[cfg(not(windows))]
pub(crate) fn signal_running_instance(_document_path: &Path) -> bool {
    false
}

#[cfg(test)]
mod negotiation_tests {
    use std::{
        cell::Cell,
        time::{Duration, Instant},
    };

    use super::{negotiate_launch, LaunchHandover, ProcessAuthorityError};

    #[test]
    fn a_free_lease_is_taken_on_the_first_attempt_without_waiting() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("document.v1.json");
        let attempts = Cell::new(0);
        let started = Instant::now();
        let outcome = negotiate_launch(&path, Duration::from_secs(5), || {
            attempts.set(attempts.get() + 1);
            Ok(())
        });
        assert_eq!(outcome, LaunchHandover::Acquired);
        assert_eq!(attempts.get(), 1);
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn a_broken_lease_is_reported_at_once() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("document.v1.json");
        let started = Instant::now();
        let outcome = negotiate_launch(&path, Duration::from_secs(5), || {
            Err(ProcessAuthorityError::Failed("disk gone".into()))
        });
        assert_eq!(
            outcome,
            LaunchHandover::Unresolved(ProcessAuthorityError::Failed("disk gone".into()))
        );
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn a_holder_that_leaves_during_the_wait_hands_the_lease_over() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("document.v1.json");
        let attempts = Cell::new(0);
        let outcome = negotiate_launch(&path, Duration::from_secs(5), || {
            attempts.set(attempts.get() + 1);
            if attempts.get() < 3 {
                Err(ProcessAuthorityError::Contended)
            } else {
                Ok(())
            }
        });
        assert_eq!(outcome, LaunchHandover::Acquired);
        assert_eq!(attempts.get(), 3);
    }

    #[test]
    fn a_silent_live_holder_is_reported_as_contended_after_the_timeout() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("document.v1.json");
        let started = Instant::now();
        let outcome = negotiate_launch(&path, Duration::from_millis(300), || {
            Err(ProcessAuthorityError::Contended)
        });
        assert_eq!(
            outcome,
            LaunchHandover::Unresolved(ProcessAuthorityError::Contended)
        );
        assert!(started.elapsed() >= Duration::from_millis(300));
    }
}

#[cfg(all(test, windows))]
mod tests {
    use std::{
        sync::mpsc,
        time::{Duration, Instant},
    };

    use super::{
        event_name, listen, negotiate_launch, signal_running_instance, LaunchHandover,
        OwnedHandle, ProcessAuthorityError,
    };

    #[test]
    fn a_holder_that_starts_listening_during_the_wait_gets_woken() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("document.v1.json");
        let (tx, rx) = mpsc::channel();
        let listener_path = path.clone();
        // Stands in for a resident that already holds the lease but has not
        // finished starting up: its listener appears only after a delay.
        let late_listener = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            listen(&listener_path, move || {
                let _ = tx.send(());
            })
            .unwrap()
        });
        let outcome = negotiate_launch(&path, Duration::from_secs(5), || {
            Err(ProcessAuthorityError::Contended)
        });
        assert_eq!(outcome, LaunchHandover::ResidentSignaled);
        rx.recv_timeout(Duration::from_secs(5))
            .expect("the late listener must receive the activation");
        drop(late_listener.join().unwrap());
    }

    #[test]
    fn event_names_are_stable_per_path_and_distinct_across_paths() {
        let a = std::path::Path::new(r"C:\data\one\document.v1.json");
        let b = std::path::Path::new(r"C:\data\two\document.v1.json");
        assert_eq!(event_name(a), event_name(a));
        assert_ne!(event_name(a), event_name(b));
        let name = String::from_utf16(&event_name(a)[..event_name(a).len() - 1]).unwrap();
        assert!(name.starts_with("Local\\Mework.ActivateWindow."), "{name}");
        assert_eq!(*event_name(a).last().unwrap(), 0, "name must be NUL terminated");
    }

    #[test]
    fn signaling_without_a_listener_reports_no_instance() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("document.v1.json");
        assert!(!signal_running_instance(&path));
    }

    #[test]
    fn a_signal_wakes_the_listener_once_per_request_and_stops_on_drop() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("document.v1.json");
        let (tx, rx) = mpsc::channel();
        let listener = listen(&path, move || {
            let _ = tx.send(());
        })
        .unwrap();

        assert!(signal_running_instance(&path));
        rx.recv_timeout(Duration::from_secs(5))
            .expect("first activation must wake the listener");
        assert!(signal_running_instance(&path));
        rx.recv_timeout(Duration::from_secs(5))
            .expect("second activation must wake the listener again");
        assert!(
            rx.recv_timeout(Duration::from_millis(400)).is_err(),
            "an auto-reset event must not deliver spurious extra activations"
        );

        let started = Instant::now();
        drop(listener);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "dropping the listener must stop its thread promptly"
        );
        assert!(
            !signal_running_instance(&path),
            "the named event must disappear with its last handle"
        );
    }

    #[test]
    fn a_paused_listener_is_invisible_and_resume_restores_it() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("document.v1.json");
        let (tx, rx) = mpsc::channel();
        let listener = listen(&path, move || {
            let _ = tx.send(());
        })
        .unwrap();

        listener.pause();
        assert!(
            !signal_running_instance(&path),
            "a paused listener must look like no instance at all"
        );
        listener.pause();
        listener.resume().unwrap();
        listener.resume().unwrap();
        assert!(signal_running_instance(&path));
        rx.recv_timeout(Duration::from_secs(5))
            .expect("a resumed listener must answer again");
        drop(listener);
        assert!(!signal_running_instance(&path));
    }

    #[test]
    fn a_name_already_held_by_another_process_is_refused() {
        use windows_sys::Win32::System::Threading::CreateEventW;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("document.v1.json");
        let name = event_name(&path);
        // Stands in for a foreign process squatting on the name (manual-reset,
        // which the listener could never drain).
        let squatter =
            OwnedHandle(unsafe { CreateEventW(std::ptr::null(), 1, 0, name.as_ptr()) });
        assert!(!squatter.raw().is_null());

        let error = match listen(&path, || {}) {
            Ok(_) => panic!("a name held by another process must be refused"),
            Err(error) => error,
        };
        assert!(error.contains("其他进程"), "{error}");

        drop(squatter);
        let listener = listen(&path, || {}).unwrap();
        drop(listener);
    }
}
