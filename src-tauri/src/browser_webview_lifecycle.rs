use std::{
    sync::{Condvar, Mutex, MutexGuard},
    time::Duration,
};

use crate::chromium_capability::WebView2Permit;

#[derive(Debug, Eq, PartialEq)]
enum SuspendWaitState {
    Waiting,
    Completed(Result<bool, String>),
    TimedOut,
    Delivered,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SuspendCompletionAction {
    NotifyWaiter,
    ResumeLateSuccess,
    Ignore,
}

#[derive(Debug, Eq, PartialEq)]
enum SuspendWaitOutcome {
    Completed(Result<bool, String>),
    TimedOut,
}

impl SuspendWaitState {
    fn complete(&mut self, result: Result<bool, String>) -> SuspendCompletionAction {
        match self {
            Self::Waiting => {
                *self = Self::Completed(result);
                SuspendCompletionAction::NotifyWaiter
            }
            Self::TimedOut if result == Ok(true) => SuspendCompletionAction::ResumeLateSuccess,
            Self::TimedOut | Self::Completed(_) | Self::Delivered => {
                SuspendCompletionAction::Ignore
            }
        }
    }

    fn finish_wait(&mut self) -> SuspendWaitOutcome {
        match std::mem::replace(self, Self::Delivered) {
            Self::Completed(result) => SuspendWaitOutcome::Completed(result),
            Self::Waiting | Self::TimedOut => {
                *self = Self::TimedOut;
                SuspendWaitOutcome::TimedOut
            }
            Self::Delivered => SuspendWaitOutcome::TimedOut,
        }
    }
}

struct SuspendWait {
    state: Mutex<SuspendWaitState>,
    ready: Condvar,
}

impl SuspendWait {
    fn new() -> Self {
        Self {
            state: Mutex::new(SuspendWaitState::Waiting),
            ready: Condvar::new(),
        }
    }

    fn lock_state(&self) -> MutexGuard<'_, SuspendWaitState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn complete(&self, result: Result<bool, String>) -> SuspendCompletionAction {
        let action = self.lock_state().complete(result);
        if action == SuspendCompletionAction::NotifyWaiter {
            self.ready.notify_one();
        }
        action
    }

    fn timed_out(&self) -> bool {
        matches!(*self.lock_state(), SuspendWaitState::TimedOut)
    }

    fn wait(&self, timeout: Duration) -> SuspendWaitOutcome {
        let state = self.lock_state();
        let (mut state, _) = self
            .ready
            .wait_timeout_while(state, timeout, |state| {
                matches!(state, SuspendWaitState::Waiting)
            })
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.finish_wait()
    }
}

#[derive(Debug, Eq, PartialEq)]
enum ResumeWaitState {
    Waiting,
    Completed(Result<(), String>),
    TimedOut,
    Delivered,
}

#[derive(Debug, Eq, PartialEq)]
enum ResumeWaitOutcome {
    Completed(Result<(), String>),
    TimedOut,
}

impl ResumeWaitState {
    fn finish_wait(&mut self) -> ResumeWaitOutcome {
        match std::mem::replace(self, Self::Delivered) {
            Self::Completed(result) => ResumeWaitOutcome::Completed(result),
            Self::Waiting | Self::TimedOut => {
                *self = Self::TimedOut;
                ResumeWaitOutcome::TimedOut
            }
            Self::Delivered => ResumeWaitOutcome::TimedOut,
        }
    }
}

struct ResumeWait {
    state: Mutex<ResumeWaitState>,
    ready: Condvar,
}

impl ResumeWait {
    fn new() -> Self {
        Self {
            state: Mutex::new(ResumeWaitState::Waiting),
            ready: Condvar::new(),
        }
    }

    fn lock_state(&self) -> MutexGuard<'_, ResumeWaitState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Claims the deadline and executes the synchronous WebView call while
    /// holding it. If the waiter marks the operation timed out first, the call
    /// is skipped. If execution starts first, the waiter observes its actual
    /// result instead of returning a timeout that could diverge from WebView.
    fn run_if_waiting(&self, run: impl FnOnce() -> Result<(), String>) {
        let mut state = self.lock_state();
        if !matches!(*state, ResumeWaitState::Waiting) {
            return;
        }
        let result = run();
        *state = ResumeWaitState::Completed(result);
        drop(state);
        self.ready.notify_one();
    }

    fn wait(&self, timeout: Duration) -> ResumeWaitOutcome {
        let state = self.lock_state();
        let (mut state, _) = self
            .ready
            .wait_timeout_while(state, timeout, |state| {
                matches!(state, ResumeWaitState::Waiting)
            })
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.finish_wait()
    }
}

fn validate_timeout(timeout: Duration) -> Result<(), String> {
    if timeout.is_zero() {
        Err("浏览器睡眠切换超时必须大于 0 毫秒".to_owned())
    } else {
        Ok(())
    }
}

/// Requests WebView2's native sleeping-tab state and waits for the completion
/// callback. `Ok(false)` is a normal best-effort refusal (for example, while a
/// page is running work that prevents suspension); callers must not count that
/// page as sleeping.
///
/// The WebView2 controller must already be invisible. This blocking adapter
/// must run off Tauri's UI thread because WebView2 delivers the asynchronous
/// completion callback on that thread. A timeout does not cancel the request:
/// the caller must conservatively count the page as awake, even though WebView2
/// may finish suspending it later (showing it will automatically resume it).
pub(crate) fn try_suspend(
    page: &tauri::Webview,
    tail_permit: WebView2Permit,
    timeout: Duration,
) -> Result<bool, String> {
    validate_timeout(timeout)?;
    try_suspend_platform(page, tail_permit, timeout)
}

/// Resumes a WebView2 sleeping tab and returns only after the synchronous
/// `Resume` call has run on the WebView thread. A queued call that loses the
/// deadline race is skipped, so returning a timeout never leaves the physical
/// page resumed while its caller still treats it as sleeping. If the WebView
/// thread begins `Resume` first, this waits for and returns that actual result.
pub(crate) fn resume(
    page: &tauri::Webview,
    tail_permit: WebView2Permit,
    timeout: Duration,
) -> Result<(), String> {
    validate_timeout(timeout)?;
    resume_platform(page, tail_permit, timeout)
}

#[cfg(windows)]
fn try_suspend_platform(
    page: &tauri::Webview,
    tail_permit: WebView2Permit,
    timeout: Duration,
) -> Result<bool, String> {
    use std::sync::Arc;

    use webview2_com::{
        Microsoft::Web::WebView2::Win32::ICoreWebView2_3, TrySuspendCompletedHandler,
    };

    let wait = Arc::new(SuspendWait::new());
    let scheduling_wait = wait.clone();
    let callback_token = tail_permit.callback_token();
    page.with_webview(move |platform| {
        // Retain blocking authority only while the queued task inspects the controller and
        // registers TrySuspend. A completion callback is not guaranteed to arrive.
        let _dispatch_permit = tail_permit;
        // `with_webview` is queued onto Tauri's UI thread. If that queue itself
        // was stalled past the caller's deadline, do not start a request whose
        // only possible useful outcome would immediately need undoing.
        if scheduling_wait.timed_out() {
            return;
        }

        let scheduled = (|| -> Result<(), String> {
            let controller = platform.controller();
            let core = unsafe { controller.CoreWebView2() }
                .map_err(|error| hresult_error("取得 WebView2 核心", error.code().0))?;
            let core_3 =
                unsafe { query_interface::<_, ICoreWebView2_3>(&core, &IID_CORE_WEBVIEW2_3) }
                    .map_err(|code| hresult_error("取得 WebView2 睡眠接口", code))?;
            let callback_core = core_3.clone();
            let completion_wait = scheduling_wait.clone();
            let callback =
                TrySuspendCompletedHandler::create(Box::new(move |status, successful| {
                    // Reacquire exact-generation authority before publishing the result or
                    // issuing the compensating late Resume. Once teardown has invalidated this
                    // generation, a late callback becomes a no-op.
                    let Ok(_callback_permit) = callback_token.permit() else {
                        return Ok(());
                    };
                    let result = status
                        .map(|_| successful)
                        .map_err(|error| hresult_error("WebView2 睡眠请求未完成", error.code().0));
                    if completion_wait.complete(result)
                        == SuspendCompletionAction::ResumeLateSuccess
                    {
                        // The caller has already conservatively counted this
                        // page as awake. Undo a successful late transition on
                        // the WebView thread so physical and logical state
                        // converge again. Showing the controller remains a
                        // second WebView2-provided recovery path if Resume
                        // itself cannot run because the page was destroyed.
                        let _ = unsafe { callback_core.Resume() };
                    }
                    Ok(())
                }));
            unsafe { core_3.TrySuspend(&callback) }
                .map_err(|error| hresult_error("启动 WebView2 睡眠请求", error.code().0))
        })();
        if let Err(error) = scheduled {
            scheduling_wait.complete(Err(error));
        }
    })
    .map_err(|_| "无法调度 WebView2 睡眠请求".to_owned())?;

    match wait.wait(timeout) {
        SuspendWaitOutcome::Completed(result) => result,
        SuspendWaitOutcome::TimedOut => Err(format!(
            "等待 WebView2 进入睡眠超时（{} 毫秒；请求结果未知，调用方应按活动状态计数）",
            timeout.as_millis()
        )),
    }
}

#[cfg(windows)]
fn resume_platform(
    page: &tauri::Webview,
    tail_permit: WebView2Permit,
    timeout: Duration,
) -> Result<(), String> {
    use std::sync::Arc;

    use webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2_3;

    let wait = Arc::new(ResumeWait::new());
    let scheduling_wait = wait.clone();
    page.with_webview(move |platform| {
        // Even when the caller wins the deadline race and this closure skips `Resume`, controller
        // teardown must not destroy the platform object before the queued closure is discarded.
        let _tail_permit = tail_permit;
        scheduling_wait.run_if_waiting(|| {
            let controller = platform.controller();
            let core = unsafe { controller.CoreWebView2() }
                .map_err(|error| hresult_error("取得 WebView2 核心", error.code().0))?;
            let core_3 =
                unsafe { query_interface::<_, ICoreWebView2_3>(&core, &IID_CORE_WEBVIEW2_3) }
                    .map_err(|code| hresult_error("取得 WebView2 睡眠接口", code))?;
            unsafe { core_3.Resume() }
                .map_err(|error| hresult_error("恢复 WebView2 页面", error.code().0))
        });
    })
    .map_err(|_| "无法调度 WebView2 页面恢复".to_owned())?;

    match wait.wait(timeout) {
        ResumeWaitOutcome::Completed(result) => result,
        ResumeWaitOutcome::TimedOut => Err(format!(
            "等待 WebView2 页面恢复超时（{} 毫秒；恢复调用未执行）",
            timeout.as_millis()
        )),
    }
}

#[cfg(not(windows))]
fn try_suspend_platform(
    _page: &tauri::Webview,
    _tail_permit: WebView2Permit,
    _timeout: Duration,
) -> Result<bool, String> {
    Err("当前平台不支持 WebView2 原生睡眠".to_owned())
}

#[cfg(not(windows))]
fn resume_platform(
    _page: &tauri::Webview,
    _tail_permit: WebView2Permit,
    _timeout: Duration,
) -> Result<(), String> {
    Err("当前平台不支持 WebView2 原生恢复".to_owned())
}

#[cfg(windows)]
fn hresult_error(stage: &str, code: i32) -> String {
    format!("{stage}失败（HRESULT 0x{:08X}）", code as u32)
}

// `webview2-com` does not re-export the windows-core `Interface` trait used by
// its generated bindings. QueryInterface is therefore performed through the
// stable IUnknown ABI, matching the profile-data helper in this crate.
#[cfg(windows)]
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RawGuid {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

#[cfg(windows)]
impl RawGuid {
    const fn from_u128(value: u128) -> Self {
        Self {
            data1: (value >> 96) as u32,
            data2: (value >> 80) as u16,
            data3: (value >> 64) as u16,
            data4: (value as u64).to_be_bytes(),
        }
    }
}

#[cfg(windows)]
const IID_CORE_WEBVIEW2_3: RawGuid = RawGuid::from_u128(0xa0d6df20_3b92_416d_aa0c_437a9c727857);

#[cfg(windows)]
#[repr(C)]
struct RawIUnknownVtable {
    query_interface: unsafe extern "system" fn(
        this: *mut std::ffi::c_void,
        iid: *const RawGuid,
        interface: *mut *mut std::ffi::c_void,
    ) -> i32,
    add_ref: unsafe extern "system" fn(this: *mut std::ffi::c_void) -> u32,
    release: unsafe extern "system" fn(this: *mut std::ffi::c_void) -> u32,
}

#[cfg(windows)]
unsafe fn query_interface<Source, Target>(source: &Source, iid: &RawGuid) -> Result<Target, i32> {
    use std::{ffi::c_void, mem};

    assert_eq!(mem::size_of::<Source>(), mem::size_of::<*mut c_void>());
    assert_eq!(mem::size_of::<Target>(), mem::size_of::<*mut c_void>());
    let source_pointer = unsafe { mem::transmute_copy::<Source, *mut c_void>(source) };
    if source_pointer.is_null() {
        return Err(0x8000_4003_u32 as i32);
    }
    let vtable = unsafe { *(source_pointer as *const *const RawIUnknownVtable) };
    if vtable.is_null() {
        return Err(0x8000_4003_u32 as i32);
    }

    let mut target_pointer = std::ptr::null_mut();
    let status = unsafe { ((*vtable).query_interface)(source_pointer, iid, &mut target_pointer) };
    if status < 0 {
        return Err(status);
    }
    if target_pointer.is_null() {
        return Err(0x8000_4003_u32 as i32);
    }
    Ok(unsafe { mem::transmute_copy::<*mut c_void, Target>(&target_pointer) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeout_must_be_positive() {
        assert!(validate_timeout(Duration::from_millis(1)).is_ok());
        assert_eq!(
            validate_timeout(Duration::ZERO),
            Err("浏览器睡眠切换超时必须大于 0 毫秒".to_owned())
        );
    }

    #[test]
    fn completion_that_wins_the_deadline_is_delivered_once() {
        let mut state = SuspendWaitState::Waiting;
        assert_eq!(
            state.complete(Ok(true)),
            SuspendCompletionAction::NotifyWaiter
        );
        assert_eq!(state.finish_wait(), SuspendWaitOutcome::Completed(Ok(true)));
        assert_eq!(state, SuspendWaitState::Delivered);
        assert_eq!(state.complete(Ok(true)), SuspendCompletionAction::Ignore);
    }

    #[test]
    fn timeout_that_wins_the_race_requires_late_success_to_resume() {
        let mut state = SuspendWaitState::Waiting;
        assert_eq!(state.finish_wait(), SuspendWaitOutcome::TimedOut);
        assert_eq!(state, SuspendWaitState::TimedOut);
        assert_eq!(
            state.complete(Ok(true)),
            SuspendCompletionAction::ResumeLateSuccess
        );
        assert_eq!(state, SuspendWaitState::TimedOut);
    }

    #[test]
    fn late_refusal_or_failure_does_not_request_resume() {
        let mut refused = SuspendWaitState::TimedOut;
        assert_eq!(refused.complete(Ok(false)), SuspendCompletionAction::Ignore);

        let mut failed = SuspendWaitState::TimedOut;
        assert_eq!(
            failed.complete(Err("synthetic failure".to_owned())),
            SuspendCompletionAction::Ignore
        );
    }

    #[test]
    fn completion_error_is_delivered_without_browser_data() {
        let mut state = SuspendWaitState::Waiting;
        assert_eq!(
            state.complete(Err("bounded failure".to_owned())),
            SuspendCompletionAction::NotifyWaiter
        );
        assert_eq!(
            state.finish_wait(),
            SuspendWaitOutcome::Completed(Err("bounded failure".to_owned()))
        );
    }

    #[test]
    fn resume_timeout_that_wins_skips_the_queued_call() {
        let wait = ResumeWait::new();
        {
            let mut state = wait.lock_state();
            assert_eq!(state.finish_wait(), ResumeWaitOutcome::TimedOut);
        }

        let called = std::cell::Cell::new(false);
        wait.run_if_waiting(|| {
            called.set(true);
            Ok(())
        });
        assert!(!called.get());
        assert_eq!(*wait.lock_state(), ResumeWaitState::TimedOut);
    }

    #[test]
    fn resume_call_that_wins_is_delivered_instead_of_timing_out() {
        let wait = ResumeWait::new();
        wait.run_if_waiting(|| Ok(()));
        assert_eq!(
            wait.wait(Duration::from_millis(1)),
            ResumeWaitOutcome::Completed(Ok(()))
        );
        assert_eq!(*wait.lock_state(), ResumeWaitState::Delivered);
    }

    #[cfg(windows)]
    #[test]
    fn queried_interface_iid_matches_webview2_bindings() {
        assert_eq!(
            IID_CORE_WEBVIEW2_3,
            RawGuid {
                data1: 0xa0d6df20,
                data2: 0x3b92,
                data3: 0x416d,
                data4: [0xaa, 0x0c, 0x43, 0x7a, 0x9c, 0x72, 0x78, 0x57],
            }
        );
    }

    #[cfg(windows)]
    #[test]
    fn hresult_errors_are_bounded_and_contain_no_page_data() {
        assert_eq!(
            hresult_error("启动 WebView2 睡眠请求", 0x8007_139D_u32 as i32),
            "启动 WebView2 睡眠请求失败（HRESULT 0x8007139D）"
        );
    }
}
