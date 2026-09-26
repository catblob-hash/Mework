//! Keys the React page leaves unhandled stop at tao's view instead of being typed into it.
//!
//! WebKit hands a key the page did not consume — → with the caret at the end of a field, ←
//! at its start, ⌫ in an empty one — back to AppKit, which passes it up the responder chain.
//! Tauri's `unstable` feature, which the browser pane's child pages need, makes the React
//! WKWebView a child of tao's content view instead of wry's `WryWebViewParent`, so the next
//! responder is tao's view. Its `keyDown:` runs `interpretKeyEvents:` for tao's own text input,
//! and the key's private-use character (U+F703 for →) ends up in the page's focused field,
//! drawn as a box (tauri-apps/tauri#10194). `WryWebViewParent` never interprets such keys.
//!
//! So tao's `keyDown:` is wrapped: a key that bubbled up from a descendant — the WKWebView or a
//! CEF page — is dropped. tao's view never passed such keys on or beeped either, and Tauri reads
//! none of the keyboard events tao would have queued for them. A key aimed at tao's view itself,
//! when nothing inside it has focus, still gets tao's handling.
//!
//! Only real key events show the bug: an `NSEvent` synthesized in-process reaches tao's view
//! the same way but inserts nothing, while a `CGEvent` posted to the process does.

use std::sync::OnceLock;

use objc2::ffi;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Imp, Sel};
use objc2::sel;
use objc2_app_kit::NSView;
use tauri::WebviewWindow;

/// `keyDown:` as tao's view class implemented it before it was wrapped.
static ORIGINAL_KEY_DOWN: OnceLock<usize> = OnceLock::new();

unsafe extern "C-unwind" fn key_down(this: *mut AnyObject, cmd: Sel, event: *mut AnyObject) {
    let Some(&original) = ORIGINAL_KEY_DOWN.get() else {
        return;
    };
    // SAFETY: only installed on an `NSView` subclass, and AppKit sends `keyDown:` on the main
    // thread, where the view is alive for the duration of the call.
    let view = unsafe { &*(this as *const NSView) };
    let aimed_at_view = view
        .window()
        .and_then(|window| window.firstResponder())
        .is_some_and(|responder| Retained::as_ptr(&responder).cast::<AnyObject>() == this);
    if !aimed_at_view {
        return;
    }
    let original: unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject) =
        unsafe { std::mem::transmute(original) };
    unsafe { original(this, cmd, event) };
}

/// Wraps `keyDown:` of the class of `window`'s content view, tao's view. Every tao window
/// shares that class, so one call covers them all; later calls do nothing.
pub(crate) fn install(window: &WebviewWindow) -> Result<(), String> {
    if ORIGINAL_KEY_DOWN.get().is_some() {
        return Ok(());
    }
    let view = window
        .ns_view()
        .map_err(|error| format!("无法取得窗口内容视图: {error}"))?;
    if view.is_null() {
        return Err("窗口没有内容视图".into());
    }
    // SAFETY: tao's content view, retained by its window, which outlives this call.
    let class: &'static AnyClass = AnyObject::class(unsafe { &*(view as *const AnyObject) });
    // Only an override the class declares itself is replaced: replacing an inherited
    // `keyDown:` would add one where there was none, and on `NSView` would reach every view.
    if !class
        .instance_methods()
        .iter()
        .any(|method| method.name() == sel!(keyDown:))
    {
        return Err(format!(
            "窗口内容视图 {} 没有自己的 keyDown:",
            class.name().to_string_lossy()
        ));
    }
    let original = class
        .instance_method(sel!(keyDown:))
        .ok_or("窗口内容视图没有 keyDown:")?
        .implementation();
    let _ = ORIGINAL_KEY_DOWN.set(original as usize);
    unsafe {
        let wrapped: Imp = std::mem::transmute(
            key_down as unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject),
        );
        ffi::class_replaceMethod(
            class as *const AnyClass as *mut AnyClass,
            sel!(keyDown:),
            wrapped,
            c"v@:@".as_ptr(),
        );
    }
    Ok(())
}
