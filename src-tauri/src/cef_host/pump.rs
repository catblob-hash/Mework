//! CEF's external message pump, driven from the AppKit run loop tao already runs.
//!
//! With `Settings::external_message_pump` CEF runs no loop of its own; it asks the host to call
//! `cef::do_message_loop_work` through `OnScheduleMessagePumpWork(delay)`. This is cefclient's
//! `main_message_loop_external_pump{,_mac}` as ported by tauri-runtime-cef
//! (<https://github.com/tauri-apps/tauri>, `crates/tauri-runtime-cef/src/external_message_pump`,
//! Apache-2.0 OR MIT): requests are posted to the main thread with
//! `performSelector:onThread:`, and delayed work rides an `NSTimer` installed in the common
//! and event-tracking run-loop modes, so CEF keeps painting while AppKit spins a nested menu
//! or tracking loop that tao never observes.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex, Weak,
};

use objc2::{define_class, msg_send, rc::Retained, sel, AnyThread, DefinedClass};
use objc2_app_kit::NSEventTrackingRunLoopMode;
use objc2_foundation::{
    NSNumber, NSObject, NSObjectNSThreadPerformAdditions, NSObjectProtocol, NSRunLoop,
    NSRunLoopCommonModes, NSThread, NSTimer,
};

/// Set once CEF shuts down: timers still in flight must not call into it afterwards.
static STOPPED: AtomicBool = AtomicBool::new(false);

/// Stops all further message-loop work. Main thread, before `cef::shutdown`.
pub fn stop() {
    STOPPED.store(true, Ordering::SeqCst);
}

/// Placeholder delay meaning "the maximum", kept 32-bit for the AppKit timer API.
const TIMER_DELAY_PLACEHOLDER: i64 = i32::MAX as i64;
/// The longest CEF may go without a `do_message_loop_work` (30 fps).
const MAX_TIMER_DELAY: i64 = 1000 / 30;

/// Shared handle; the timer and its target are released with the last clone.
#[derive(Clone)]
pub struct ExternalPump {
    state: Arc<PumpState>,
}

impl ExternalPump {
    /// Must be created on the main thread, which becomes the thread CEF work runs on.
    pub fn new() -> Self {
        let state = Arc::new_cyclic(|weak| PumpState {
            is_active: AtomicBool::new(false),
            reentrancy_detected: AtomicBool::new(false),
            platform: Mutex::new(PlatformPump::new(weak.clone())),
        });
        Self { state }
    }

    /// `OnScheduleMessagePumpWork`. Any thread.
    pub fn schedule(&self, delay_ms: i64) {
        self.state.on_schedule_message_pump_work(delay_ms);
    }

    /// An explicit tick. Main thread only.
    pub fn do_work(&self) {
        self.state.do_work();
    }
}

struct PumpState {
    is_active: AtomicBool,
    reentrancy_detected: AtomicBool,
    platform: Mutex<PlatformPump>,
}

impl PumpState {
    fn on_schedule_message_pump_work(&self, delay_ms: i64) {
        if STOPPED.load(Ordering::SeqCst) {
            return;
        }
        if let Ok(mut platform) = self.platform.lock() {
            platform.post_schedule(delay_ms);
        }
    }

    fn on_schedule_work(&self, mut delay_ms: i64) {
        {
            let Ok(mut platform) = self.platform.lock() else {
                return;
            };
            if delay_ms == TIMER_DELAY_PLACEHOLDER && platform.is_timer_pending() {
                // Don't replace a pending timer with the maximum one DoWork() asks for.
                return;
            }
            platform.kill_timer();
        }
        if delay_ms <= 0 {
            self.do_work();
        } else if let Ok(mut platform) = self.platform.lock() {
            delay_ms = delay_ms.min(MAX_TIMER_DELAY);
            platform.set_timer(delay_ms);
        }
    }

    fn on_timer_timeout(&self) {
        if let Ok(mut platform) = self.platform.lock() {
            platform.kill_timer();
        }
        self.do_work();
    }

    fn do_work(&self) {
        let was_reentrant = self.perform_message_loop_work();
        if was_reentrant {
            self.on_schedule_message_pump_work(0);
        } else if !self.is_timer_pending() {
            self.on_schedule_message_pump_work(TIMER_DELAY_PLACEHOLDER);
        }
    }

    fn is_timer_pending(&self) -> bool {
        self.platform
            .lock()
            .map(|platform| platform.is_timer_pending())
            .unwrap_or(true)
    }

    fn perform_message_loop_work(&self) -> bool {
        if STOPPED.load(Ordering::SeqCst) {
            return false;
        }
        if self.is_active.load(Ordering::SeqCst) {
            // Paint and IPC callbacks inside do_message_loop_work can land here again; repost so
            // the discarded call still happens.
            self.reentrancy_detected.store(true, Ordering::SeqCst);
            return false;
        }
        self.reentrancy_detected.store(false, Ordering::SeqCst);
        self.is_active.store(true, Ordering::SeqCst);
        cef::do_message_loop_work();
        self.is_active.store(false, Ordering::SeqCst);
        self.reentrancy_detected.load(Ordering::SeqCst)
    }
}

define_class! {
    #[unsafe(super(NSObject))]
    #[ivars = Weak<PumpState>]
    struct PumpEventHandler;

    impl PumpEventHandler {
        #[unsafe(method(scheduleWork:))]
        fn handle_schedule_work(&self, delay_ms: &NSNumber) {
            if let Some(state) = self.ivars().upgrade() {
                state.on_schedule_work(delay_ms.as_i64());
            }
        }

        #[unsafe(method(timerTimeout:))]
        fn handle_timer_timeout(&self, _: &NSTimer) {
            if let Some(state) = self.ivars().upgrade() {
                state.on_timer_timeout();
            }
        }
    }

    unsafe impl NSObjectProtocol for PumpEventHandler {}
}

impl PumpEventHandler {
    fn new(state: Weak<PumpState>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(state);
        unsafe { msg_send![super(this), init] }
    }
}

struct PlatformPump {
    owner_thread: Retained<NSThread>,
    event_handler: Retained<PumpEventHandler>,
    timer: Option<Retained<NSTimer>>,
}

// SAFETY: the thread handle is only used as a `performSelector:onThread:` target, which is
// thread-safe; the timer is only created and invalidated on the owner (main) thread, where
// every path that touches it has already been marshalled.
unsafe impl Send for PlatformPump {}

impl PlatformPump {
    fn new(state: Weak<PumpState>) -> Self {
        Self {
            owner_thread: NSThread::currentThread(),
            event_handler: PumpEventHandler::new(state),
            timer: None,
        }
    }

    fn post_schedule(&mut self, delay_ms: i64) {
        let delay_ms = NSNumber::new_i32(delay_ms as i32);
        unsafe {
            self.event_handler
                .performSelector_onThread_withObject_waitUntilDone(
                    sel!(scheduleWork:),
                    &self.owner_thread,
                    Some(&delay_ms),
                    false,
                );
        }
    }

    fn set_timer(&mut self, delay_ms: i64) {
        let timer = unsafe {
            NSTimer::timerWithTimeInterval_target_selector_userInfo_repeats(
                delay_ms as f64 / 1000.0,
                &self.event_handler,
                sel!(timerTimeout:),
                None,
                false,
            )
        };
        let run_loop = NSRunLoop::currentRunLoop();
        unsafe {
            run_loop.addTimer_forMode(&timer, NSRunLoopCommonModes);
            run_loop.addTimer_forMode(&timer, NSEventTrackingRunLoopMode);
        }
        self.timer = Some(timer);
    }

    fn kill_timer(&mut self) {
        if let Some(timer) = self.timer.take() {
            timer.invalidate();
        }
    }

    fn is_timer_pending(&self) -> bool {
        self.timer.is_some()
    }
}
