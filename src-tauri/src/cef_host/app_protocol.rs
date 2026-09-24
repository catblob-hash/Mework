//! Chromium's contract with the AppKit application object.
//!
//! Chromium, and therefore CEF, expects `NSApp` to answer `isHandlingSendEvent` and
//! `setHandlingSendEvent:` (`CrAppProtocol`, `CrAppControlProtocol`, `CefAppProtocol`) and to
//! raise that flag for the duration of every `sendEvent:`. Without it a context menu or a
//! `<select>` popup ends in an unrecognized-selector crash, and nested run loops mis-schedule.
//!
//! CEF applications normally satisfy this by subclassing `NSApplication`. Here tao owns the
//! application object (its runtime-built `TaoApp`), and replacing it would lose tao's own
//! `sendEvent:` fixes, while subclassing it would recurse through tao's `[self superclass]`
//! lookup. So the methods and protocols are added at run time to whatever class `NSApp`
//! already is, and its `sendEvent:` is wrapped — what JCEF and QCefView do under a foreign
//! application object.

use std::cell::Cell;
use std::ffi::CStr;
use std::sync::OnceLock;

use objc2::ffi;
use objc2::runtime::{AnyClass, AnyObject, AnyProtocol, Bool, Imp, Sel};
use objc2::{sel, MainThreadMarker};
use objc2_app_kit::NSApplication;

thread_local! {
    /// AppKit delivers events on the main thread only, and so is the only reader.
    static HANDLING_SEND_EVENT: Cell<bool> = const { Cell::new(false) };
}

/// `sendEvent:` as the application class implemented it before it was wrapped.
static ORIGINAL_SEND_EVENT: OnceLock<usize> = OnceLock::new();

unsafe extern "C-unwind" fn is_handling_send_event(_this: *mut AnyObject, _cmd: Sel) -> Bool {
    Bool::new(HANDLING_SEND_EVENT.with(Cell::get))
}

unsafe extern "C-unwind" fn set_handling_send_event(_this: *mut AnyObject, _cmd: Sel, value: Bool) {
    HANDLING_SEND_EVENT.with(|flag| flag.set(value.as_bool()));
}

unsafe extern "C-unwind" fn send_event(this: *mut AnyObject, cmd: Sel, event: *mut AnyObject) {
    let Some(&original) = ORIGINAL_SEND_EVENT.get() else {
        return;
    };
    let original: unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject) =
        unsafe { std::mem::transmute(original) };
    let was_handling = HANDLING_SEND_EVENT.with(|flag| flag.replace(true));
    unsafe { original(this, cmd, event) };
    HANDLING_SEND_EVENT.with(|flag| flag.set(was_handling));
}

/// Makes the running application object satisfy Chromium's protocols. Main thread only;
/// idempotent. Must run after the CEF framework is loaded, which is what declares the
/// protocols the class is made to conform to.
pub fn install() -> Result<(), String> {
    let mtm = MainThreadMarker::new().ok_or("CEF must be set up on the main thread")?;
    let app = NSApplication::sharedApplication(mtm);
    let class: &'static AnyClass = AnyObject::class(&app);
    if ORIGINAL_SEND_EVENT.get().is_some() {
        return Ok(());
    }
    let class_ptr = class as *const AnyClass as *mut AnyClass;
    unsafe {
        // Resolved through the superclass chain, so this is the implementation `NSApp`
        // actually runs, whether its class overrides `sendEvent:` or inherits it.
        let original = class
            .instance_method(sel!(sendEvent:))
            .ok_or("the application class has no sendEvent: implementation")?
            .implementation();
        let _ = ORIGINAL_SEND_EVENT.set(original as usize);

        let is_handling: Imp = std::mem::transmute(
            is_handling_send_event as unsafe extern "C-unwind" fn(*mut AnyObject, Sel) -> Bool,
        );
        let set_handling: Imp = std::mem::transmute(
            set_handling_send_event as unsafe extern "C-unwind" fn(*mut AnyObject, Sel, Bool),
        );
        let wrapped_send: Imp = std::mem::transmute(
            send_event as unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject),
        );
        // `c` is Objective-C's BOOL on arm64 as well: the type string only documents the
        // signature for introspection, and Chromium calls these through ordinary sends.
        ffi::class_replaceMethod(
            class_ptr,
            sel!(isHandlingSendEvent),
            is_handling,
            c"c@:".as_ptr(),
        );
        ffi::class_replaceMethod(
            class_ptr,
            sel!(setHandlingSendEvent:),
            set_handling,
            c"v@:c".as_ptr(),
        );
        ffi::class_replaceMethod(class_ptr, sel!(sendEvent:), wrapped_send, c"v@:@".as_ptr());

        for name in [c"CrAppProtocol", c"CrAppControlProtocol", c"CefAppProtocol"] {
            add_protocol(class_ptr, name);
        }
    }
    Ok(())
}

unsafe fn add_protocol(class: *mut AnyClass, name: &CStr) {
    // Declared by the CEF framework; absent only when it was not loaded, in which case CEF
    // itself cannot run and the conformance check never happens.
    let protocol: *const AnyProtocol = unsafe { ffi::objc_getProtocol(name.as_ptr()) };
    if !protocol.is_null() {
        unsafe { ffi::class_addProtocol(class, protocol) };
    }
}
