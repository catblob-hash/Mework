//! Stacking of the untrusted browser page's native child window.
//!
//! On Windows, WRY hosts the remote page in a child HWND that is stacked above the trusted React
//! WebView, so nothing the trusted WebView draws in CSS can ever appear over the page. Covering
//! the page is therefore a z-order question and nothing else: sinking it below its siblings puts
//! it behind the full-window React WebView, which hides it completely and takes every pointer
//! event that would have reached it.
//!
//! Geometry deliberately stays out of this module. The page keeps its size, its position and its
//! compositing whether it is on top or at the bottom, so page viewport coordinates never move and
//! a covered page keeps answering the Agent at foreground speed. What the user sees in its place
//! is a still frame the renderer captured and paints in the DOM.

use std::time::Duration;

use crate::browser::PageWebview;
use crate::chromium_capability::WebView2Permit;

#[cfg_attr(not(windows), allow(dead_code))]
const PAGE_STACKING_TIMEOUT: Duration = Duration::from_secs(2);

/// Moves the remote child HWND to the bottom (`parked`) or the top of its siblings.
///
/// The trusted React WebView is a sibling that covers the whole main window, so a page at the
/// bottom of the z-order is fully covered: nothing of it shows and no pointer input reaches it,
/// while Chromium still sees an on-screen window and keeps compositing frames for it. That is
/// what lets a page the user is not looking at keep answering the Agent's input at full speed.
pub(crate) fn set_page_stacking(
    page: &PageWebview,
    tail_permit: WebView2Permit,
    parked: bool,
) -> Result<(), String> {
    #[cfg(windows)]
    {
        with_parent_hwnd(page, tail_permit, move |parent| unsafe {
            use windows_sys::Win32::UI::WindowsAndMessaging::{
                SetWindowPos, HWND_BOTTOM, HWND_TOP, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
            };
            let insert_after = if parked { HWND_BOTTOM } else { HWND_TOP };
            if SetWindowPos(
                parent,
                insert_after,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            ) == 0
            {
                return Err(last_windows_error(
                    "failed to restack the Chromium child window",
                ));
            }
            Ok(())
        })
    }

    // On macOS the page is a CEF view beside the React WKWebView in the window's content view;
    // the same covering is a sibling-order question there.
    #[cfg(target_os = "macos")]
    {
        let _tail_permit = tail_permit;
        page.set_stacking(parked)
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = (page, tail_permit, parked);
        Ok(())
    }
}

#[cfg(windows)]
fn with_parent_hwnd(
    page: &PageWebview,
    tail_permit: WebView2Permit,
    operation: impl FnOnce(windows_sys::Win32::Foundation::HWND) -> Result<(), String> + Send + 'static,
) -> Result<(), String> {
    use std::sync::mpsc;

    let (sender, receiver) = mpsc::sync_channel(1);
    page.with_webview(move |platform| {
        // The caller may stop waiting before this UI-thread closure runs. Keep the controller
        // generation in-flight until the queued native stacking operation actually returns.
        let _tail_permit = tail_permit;
        let result = (|| {
            let controller = platform.controller();
            let mut parent = Default::default();
            unsafe { controller.ParentWindow(&mut parent) }
                .map_err(|error| format!("无法取得 Chromium 子窗口句柄: {error}"))?;
            if parent.0.is_null() {
                return Err("Chromium 子窗口句柄为空".into());
            }
            operation(parent.0)
        })();
        let _ = sender.try_send(result);
    })
    .map_err(|error| format!("无法调度 Chromium 子窗口层级更新: {error}"))?;

    receiver
        .recv_timeout(PAGE_STACKING_TIMEOUT)
        .map_err(|error| match error {
            mpsc::RecvTimeoutError::Timeout => {
                format!(
                    "等待 Chromium 子窗口层级更新超时（{} ms）",
                    PAGE_STACKING_TIMEOUT.as_millis()
                )
            }
            mpsc::RecvTimeoutError::Disconnected => "Chromium 子窗口层级更新通道已关闭".into(),
        })?
}

#[cfg(windows)]
fn last_windows_error(context: &str) -> String {
    format!("{context}: {}", std::io::Error::last_os_error())
}
