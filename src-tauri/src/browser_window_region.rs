//! Trusted browser-chrome window regions.
//!
//! On Windows, WRY hosts the remote page in a child HWND that is stacked above the trusted
//! React WebView. A DOM menu in the trusted WebView therefore cannot cover the remote page with
//! CSS alone. This module can punch the menu rectangle out of the remote child HWND without
//! moving or resizing it, so the trusted menu remains visible and receives pointer input while
//! page viewport coordinates stay stable.
//!
//! `LogicalRect` coordinates are local to the remote WebView, not screen or application-window
//! coordinates. Callers must reapply the hole when either the menu rectangle, WebView bounds, or
//! scale factor changes, and call [`restore_full_region_ordered`] when the menu closes.

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use tauri::Webview;

use crate::chromium_capability::WebView2Permit;

const WINDOW_REGION_TIMEOUT: Duration = Duration::from_secs(2);

/// Per-browser ordering for asynchronous native window-region work.
///
/// `Webview::with_webview` may run its UI-thread closure after the caller has timed out. Every
/// open, ResizeObserver update, and close therefore claims a strictly increasing generation.
/// The ticket is checked again inside the UI-thread closure so an old hole can never overwrite a
/// newer close (or a newer menu layout).
#[derive(Clone, Default)]
pub(crate) struct WindowRegionOrder {
    /// The same mutex both publishes generations and guards the final native call. An internal
    /// epoch lets lifecycle code invalidate callbacks without consuming the renderer's sequence.
    state: Arc<Mutex<WindowRegionOrderState>>,
}

#[derive(Default)]
struct WindowRegionOrderState {
    latest_renderer_generation: u64,
    epoch: u64,
    intent: WindowRegionIntent,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum WindowRegionIntent {
    #[default]
    Full,
    Hole,
}

#[derive(Clone)]
pub(crate) struct WindowRegionTicket {
    order: WindowRegionOrder,
    renderer_generation: u64,
    epoch: u64,
}

impl WindowRegionOrder {
    /// Claims an explicit renderer generation. Duplicate and older requests are stale.
    pub(crate) fn claim(
        &self,
        generation: u64,
        opens_hole: bool,
    ) -> Result<Option<WindowRegionTicket>, String> {
        if generation == 0 {
            return Err("浏览器菜单区域 generation 必须大于 0".into());
        }
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if generation <= state.latest_renderer_generation {
            return Ok(None);
        }
        let next_epoch = state
            .epoch
            .checked_add(1)
            .ok_or_else(|| "浏览器菜单窗口区域 epoch 已耗尽".to_owned())?;
        state.latest_renderer_generation = generation;
        state.epoch = next_epoch;
        state.intent = if opens_hole {
            WindowRegionIntent::Hole
        } else {
            WindowRegionIntent::Full
        };
        Ok(Some(WindowRegionTicket {
            order: self.clone(),
            renderer_generation: generation,
            epoch: state.epoch,
        }))
    }

    /// Invalidates every queued callback and makes the full native region the current intent.
    ///
    /// This advances only the backend epoch, so the renderer's next consecutive generation is
    /// still accepted after a hide, suspend, layout reset, or native timeout.
    pub(crate) fn supersede_with_full_region(&self) -> Result<WindowRegionTicket, String> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.epoch = state
            .epoch
            .checked_add(1)
            .ok_or_else(|| "浏览器菜单窗口区域 epoch 已耗尽".to_owned())?;
        state.intent = WindowRegionIntent::Full;
        Ok(WindowRegionTicket {
            order: self.clone(),
            renderer_generation: state.latest_renderer_generation,
            epoch: state.epoch,
        })
    }

    pub(crate) fn current_ticket(&self) -> WindowRegionTicket {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        WindowRegionTicket {
            order: self.clone(),
            renderer_generation: state.latest_renderer_generation,
            epoch: state.epoch,
        }
    }

    pub(crate) fn current_hole_ticket(&self) -> Option<WindowRegionTicket> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        (state.intent == WindowRegionIntent::Hole).then(|| WindowRegionTicket {
            order: self.clone(),
            renderer_generation: state.latest_renderer_generation,
            epoch: state.epoch,
        })
    }

    /// On a native error, retire only the failed request. A concurrently published newer request
    /// keeps ownership of the region and must not be overwritten by error recovery.
    pub(crate) fn supersede_failed_with_full_region(
        &self,
        failed: &WindowRegionTicket,
    ) -> Result<Option<WindowRegionTicket>, String> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.latest_renderer_generation != failed.renderer_generation
            || state.epoch != failed.epoch
        {
            return Ok(None);
        }
        state.epoch = state
            .epoch
            .checked_add(1)
            .ok_or_else(|| "浏览器菜单窗口区域 epoch 已耗尽".to_owned())?;
        state.intent = WindowRegionIntent::Full;
        Ok(Some(WindowRegionTicket {
            order: self.clone(),
            renderer_generation: state.latest_renderer_generation,
            epoch: state.epoch,
        }))
    }
}

impl WindowRegionTicket {
    pub(crate) fn identity(&self) -> (u64, u64) {
        (self.renderer_generation, self.epoch)
    }

    pub(crate) fn is_current(&self) -> bool {
        let state = self
            .order
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.latest_renderer_generation == self.renderer_generation && state.epoch == self.epoch
    }

    fn run_if_current(&self, operation: impl FnOnce() -> Result<(), String>) -> Result<(), String> {
        let state = self
            .order
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.latest_renderer_generation != self.renderer_generation || state.epoch != self.epoch
        {
            return Ok(());
        }
        operation()
    }
}

/// A rectangle in logical pixels, relative to the remote WebView's top-left corner.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct LogicalRect {
    pub(crate) x: f64,
    pub(crate) y: f64,
    pub(crate) width: f64,
    pub(crate) height: f64,
}

impl LogicalRect {
    pub(crate) const fn new(x: f64, y: f64, width: f64, height: f64) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PhysicalRect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

/// Exposes the trusted menu by removing its rectangle (plus shadow margin) from the remote
/// WebView's native child window region.
///
/// An empty or fully out-of-bounds rectangle restores the full region. `shadow_margin` is in
/// logical pixels and is expanded on every side before converting to physical pixels.
///
/// The generation is checked on the caller thread and again in the WebView UI closure, because
/// the latter can outlive a timeout.
pub(crate) fn apply_menu_hole_ordered(
    page: &Webview,
    tail_permit: WebView2Permit,
    menu_rect: LogicalRect,
    scale_factor: f64,
    shadow_margin: f64,
    ticket: WindowRegionTicket,
) -> Result<(), String> {
    if !ticket.is_current() {
        return Ok(());
    }

    #[cfg(windows)]
    {
        with_parent_hwnd(page, tail_permit, move |parent| {
            ticket.run_if_current(|| unsafe {
                apply_menu_hole_to_hwnd(parent, menu_rect, scale_factor, shadow_margin)
            })
        })
    }

    #[cfg(not(windows))]
    {
        let _ = (
            page,
            tail_permit,
            menu_rect,
            scale_factor,
            shadow_margin,
            ticket,
        );
        Ok(())
    }
}

/// Removes any previously installed native region, restoring the full remote WebView window.
///
/// Ordered close counterpart to [`apply_menu_hole_ordered`].
pub(crate) fn restore_full_region_ordered(
    page: &Webview,
    tail_permit: WebView2Permit,
    ticket: WindowRegionTicket,
) -> Result<(), String> {
    if !ticket.is_current() {
        return Ok(());
    }

    #[cfg(windows)]
    {
        with_parent_hwnd(page, tail_permit, move |parent| {
            ticket.run_if_current(|| unsafe { restore_full_region_for_hwnd(parent) })
        })
    }

    #[cfg(not(windows))]
    {
        let _ = (page, tail_permit, ticket);
        Ok(())
    }
}

/// Moves the remote child HWND to the bottom (`parked`) or the top of its siblings.
///
/// The trusted React WebView is a sibling that covers the whole main window, so a page at the
/// bottom of the z-order is fully covered: nothing of it shows and no pointer input reaches it,
/// while Chromium still sees an on-screen window and keeps compositing frames for it. That is
/// what lets a page the user is not looking at keep answering the Agent's input at full speed.
pub(crate) fn set_page_stacking(
    page: &Webview,
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
                return Err(last_windows_error("failed to restack the Chromium child window"));
            }
            Ok(())
        })
    }

    #[cfg(not(windows))]
    {
        let _ = (page, tail_permit, parked);
        Ok(())
    }
}

fn physical_menu_hole(
    menu_rect: LogicalRect,
    scale_factor: f64,
    shadow_margin: f64,
    client_width: i32,
    client_height: i32,
) -> Result<Option<PhysicalRect>, String> {
    for (label, value) in [
        ("x", menu_rect.x),
        ("y", menu_rect.y),
        ("width", menu_rect.width),
        ("height", menu_rect.height),
        ("scale_factor", scale_factor),
        ("shadow_margin", shadow_margin),
    ] {
        if !value.is_finite() {
            return Err(format!("浏览器菜单区域 {label} 必须是有限数值"));
        }
    }
    if menu_rect.width < 0.0 || menu_rect.height < 0.0 {
        return Err("浏览器菜单区域宽高不能为负数".into());
    }
    if scale_factor <= 0.0 {
        return Err("浏览器菜单区域缩放比例必须大于 0".into());
    }
    if shadow_margin < 0.0 {
        return Err("浏览器菜单区域阴影边距不能为负数".into());
    }
    if menu_rect.width == 0.0 || menu_rect.height == 0.0 || client_width <= 0 || client_height <= 0
    {
        return Ok(None);
    }

    // Expand in logical space, then round outward in physical space. Outward rounding ensures a
    // fractional-DPI shadow pixel is never left behind on top of the trusted menu.
    let left = ((menu_rect.x - shadow_margin) * scale_factor)
        .floor()
        .clamp(0.0, f64::from(client_width)) as i32;
    let top = ((menu_rect.y - shadow_margin) * scale_factor)
        .floor()
        .clamp(0.0, f64::from(client_height)) as i32;
    let right = ((menu_rect.x + menu_rect.width + shadow_margin) * scale_factor)
        .ceil()
        .clamp(0.0, f64::from(client_width)) as i32;
    let bottom = ((menu_rect.y + menu_rect.height + shadow_margin) * scale_factor)
        .ceil()
        .clamp(0.0, f64::from(client_height)) as i32;

    if left >= right || top >= bottom {
        return Ok(None);
    }
    Ok(Some(PhysicalRect {
        left,
        top,
        right,
        bottom,
    }))
}

#[cfg(windows)]
fn with_parent_hwnd(
    page: &Webview,
    tail_permit: WebView2Permit,
    operation: impl FnOnce(windows_sys::Win32::Foundation::HWND) -> Result<(), String> + Send + 'static,
) -> Result<(), String> {
    use std::sync::mpsc;

    let (sender, receiver) = mpsc::sync_channel(1);
    page.with_webview(move |platform| {
        // The caller may stop waiting before this UI-thread closure runs. Keep the controller
        // generation in-flight until the queued native region operation actually returns.
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
    .map_err(|error| format!("无法调度 Chromium 窗口区域更新: {error}"))?;

    receiver
        .recv_timeout(WINDOW_REGION_TIMEOUT)
        .map_err(|error| match error {
            mpsc::RecvTimeoutError::Timeout => {
                format!(
                    "等待 Chromium 窗口区域更新超时（{} ms）",
                    WINDOW_REGION_TIMEOUT.as_millis()
                )
            }
            mpsc::RecvTimeoutError::Disconnected => "Chromium 窗口区域更新通道已关闭".into(),
        })?
}

#[cfg(windows)]
unsafe fn apply_menu_hole_to_hwnd(
    parent: windows_sys::Win32::Foundation::HWND,
    menu_rect: LogicalRect,
    scale_factor: f64,
    shadow_margin: f64,
) -> Result<(), String> {
    use windows_sys::Win32::{
        Foundation::RECT,
        Graphics::Gdi::{CombineRgn, ERROR, RGN_DIFF},
        UI::WindowsAndMessaging::GetWindowRect,
    };

    if parent.is_null() {
        return Err("Chromium 子窗口句柄为空".into());
    }

    // SetWindowRgn coordinates are relative to the full window. WRY creates this parent as an
    // undecorated WS_CHILD, so its window and client origins/extents are identical.
    let mut window = RECT::default();
    if unsafe { GetWindowRect(parent, &mut window) } == 0 {
        return Err(last_windows_error("无法读取 Chromium 子窗口尺寸"));
    }
    let client_width = window.right.saturating_sub(window.left);
    let client_height = window.bottom.saturating_sub(window.top);
    let Some(hole) = physical_menu_hole(
        menu_rect,
        scale_factor,
        shadow_margin,
        client_width,
        client_height,
    )?
    else {
        return unsafe { restore_full_region_for_hwnd(parent) };
    };

    let full_region = unsafe { OwnedRegion::rect(0, 0, client_width, client_height)? };
    let hole_region = unsafe { OwnedRegion::rect(hole.left, hole.top, hole.right, hole.bottom)? };
    let combined_region = unsafe { OwnedRegion::rect(0, 0, 0, 0)? };
    let region_type = unsafe {
        CombineRgn(
            combined_region.raw(),
            full_region.raw(),
            hole_region.raw(),
            RGN_DIFF,
        )
    };
    if region_type == ERROR {
        return Err(last_windows_error("无法构造 Chromium 菜单穿孔窗口区域"));
    }

    // After a successful SetWindowRgn call the operating system owns the HRGN. On failure the
    // OwnedRegion guard retains ownership and deletes it.
    unsafe { combined_region.transfer_to_window(parent) }
}

#[cfg(windows)]
unsafe fn restore_full_region_for_hwnd(
    parent: windows_sys::Win32::Foundation::HWND,
) -> Result<(), String> {
    use std::ptr;

    use windows_sys::Win32::Graphics::Gdi::SetWindowRgn;

    if parent.is_null() {
        return Err("Chromium 子窗口句柄为空".into());
    }
    if unsafe { SetWindowRgn(parent, ptr::null_mut(), 1) } == 0 {
        return Err(last_windows_error("无法恢复 Chromium 完整窗口区域"));
    }
    Ok(())
}

#[cfg(windows)]
struct OwnedRegion(windows_sys::Win32::Graphics::Gdi::HRGN);

#[cfg(windows)]
impl OwnedRegion {
    unsafe fn rect(left: i32, top: i32, right: i32, bottom: i32) -> Result<Self, String> {
        use windows_sys::Win32::Graphics::Gdi::CreateRectRgn;

        let region = unsafe { CreateRectRgn(left, top, right, bottom) };
        if region.is_null() {
            Err(last_windows_error("无法创建 Chromium 窗口区域"))
        } else {
            Ok(Self(region))
        }
    }

    fn raw(&self) -> windows_sys::Win32::Graphics::Gdi::HRGN {
        self.0
    }

    unsafe fn transfer_to_window(
        mut self,
        parent: windows_sys::Win32::Foundation::HWND,
    ) -> Result<(), String> {
        use std::ptr;

        use windows_sys::Win32::Graphics::Gdi::SetWindowRgn;

        if unsafe { SetWindowRgn(parent, self.0, 1) } == 0 {
            return Err(last_windows_error("无法应用 Chromium 菜单穿孔窗口区域"));
        }
        self.0 = ptr::null_mut();
        Ok(())
    }
}

#[cfg(windows)]
impl Drop for OwnedRegion {
    fn drop(&mut self) {
        use windows_sys::Win32::Graphics::Gdi::DeleteObject;

        if !self.0.is_null() {
            unsafe {
                let _ = DeleteObject(self.0);
            }
        }
    }
}

#[cfg(windows)]
fn last_windows_error(context: &str) -> String {
    format!("{context}: {}", std::io::Error::last_os_error())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f64, y: f64, width: f64, height: f64) -> LogicalRect {
        LogicalRect::new(x, y, width, height)
    }

    #[test]
    fn expands_shadow_and_rounds_outward_at_fractional_dpi() {
        assert_eq!(
            physical_menu_hole(rect(100.25, 20.5, 200.0, 300.0), 1.25, 4.0, 800, 600).unwrap(),
            Some(PhysicalRect {
                left: 120,
                top: 20,
                right: 381,
                bottom: 406,
            })
        );
    }

    #[test]
    fn clips_the_expanded_hole_to_the_client_area() {
        assert_eq!(
            physical_menu_hole(rect(-4.0, -8.0, 120.0, 80.0), 2.0, 3.0, 200, 100).unwrap(),
            Some(PhysicalRect {
                left: 0,
                top: 0,
                right: 200,
                bottom: 100,
            })
        );
    }

    #[test]
    fn empty_or_outside_rectangles_restore_the_full_region() {
        assert_eq!(
            physical_menu_hole(rect(10.0, 10.0, 0.0, 20.0), 1.0, 0.0, 100, 100).unwrap(),
            None
        );
        assert_eq!(
            physical_menu_hole(rect(110.0, 10.0, 20.0, 20.0), 1.0, 0.0, 100, 100).unwrap(),
            None
        );
        assert_eq!(
            physical_menu_hole(rect(10.0, 110.0, 20.0, 20.0), 1.0, 0.0, 100, 100).unwrap(),
            None
        );
        assert_eq!(
            physical_menu_hole(rect(0.0, 0.0, 20.0, 20.0), 1.0, 0.0, 0, 100).unwrap(),
            None
        );
    }

    #[test]
    fn a_hole_larger_than_the_client_area_is_clipped_to_the_full_window() {
        assert_eq!(
            physical_menu_hole(rect(-100.0, -100.0, 400.0, 400.0), 1.0, 0.0, 200, 150).unwrap(),
            Some(PhysicalRect {
                left: 0,
                top: 0,
                right: 200,
                bottom: 150,
            })
        );
    }

    #[test]
    fn preserves_a_subpixel_hole_as_one_physical_pixel() {
        assert_eq!(
            physical_menu_hole(rect(10.1, 20.1, 0.01, 0.01), 1.0, 0.0, 100, 100).unwrap(),
            Some(PhysicalRect {
                left: 10,
                top: 20,
                right: 11,
                bottom: 21,
            })
        );
    }

    #[test]
    fn finite_extreme_coordinates_clip_without_integer_overflow() {
        assert_eq!(
            physical_menu_hole(
                rect(-f64::MAX, -f64::MAX, f64::MAX, f64::MAX),
                f64::MAX,
                f64::MAX,
                100,
                80,
            )
            .unwrap(),
            Some(PhysicalRect {
                left: 0,
                top: 0,
                right: 100,
                bottom: 80,
            })
        );
    }

    #[test]
    fn rejects_non_finite_or_invalid_geometry() {
        assert!(physical_menu_hole(rect(f64::NAN, 0.0, 10.0, 10.0), 1.0, 0.0, 100, 100).is_err());
        assert!(physical_menu_hole(rect(0.0, 0.0, -1.0, 10.0), 1.0, 0.0, 100, 100).is_err());
        assert!(physical_menu_hole(rect(0.0, 0.0, 10.0, 10.0), 0.0, 0.0, 100, 100).is_err());
        assert!(physical_menu_hole(rect(0.0, 0.0, 10.0, 10.0), 1.0, -1.0, 100, 100).is_err());
    }

    #[test]
    fn newer_close_invalidates_a_queued_or_timed_out_open() {
        let order = WindowRegionOrder::default();
        let open = order
            .claim(41, true)
            .unwrap()
            .expect("open should be current");
        assert!(open.is_current());

        let close = order
            .claim(42, false)
            .unwrap()
            .expect("close should be newer");
        assert!(!open.is_current());
        assert!(close.is_current());
        assert!(order.claim(41, true).unwrap().is_none());
        assert!(order.claim(42, false).unwrap().is_none());
    }

    #[test]
    fn backend_full_region_epoch_does_not_consume_the_renderer_sequence() {
        let order = WindowRegionOrder::default();
        let renderer = order
            .claim(9_000, true)
            .unwrap()
            .expect("renderer request should be accepted");
        let backend_close = order.supersede_with_full_region().unwrap();
        assert!(!renderer.is_current());
        assert!(backend_close.is_current());
        let next_renderer = order
            .claim(9_001, true)
            .unwrap()
            .expect("next renderer generation must not collide with backend epoch");
        assert!(next_renderer.is_current());
    }

    #[test]
    fn rejects_zero_generation_without_invalidating_current_work() {
        let order = WindowRegionOrder::default();
        let current = order
            .claim(7, true)
            .unwrap()
            .expect("request should be accepted");
        assert!(order.claim(0, false).is_err());
        assert!(current.is_current());
    }

    #[test]
    fn failed_request_recovery_cannot_overwrite_a_newer_close() {
        let order = WindowRegionOrder::default();
        let failed = order
            .claim(70, true)
            .unwrap()
            .expect("hole should be accepted");
        let close = order
            .claim(71, false)
            .unwrap()
            .expect("close should be accepted");
        assert!(order
            .supersede_failed_with_full_region(&failed)
            .unwrap()
            .is_none());
        assert!(close.is_current());
    }

    #[test]
    fn close_publication_waits_for_an_in_flight_native_call_then_wins() {
        use std::{sync::mpsc, thread};

        let order = WindowRegionOrder::default();
        let open = order
            .claim(80, true)
            .unwrap()
            .expect("open should be current");
        let stale_open = open.clone();
        let (operation_started_tx, operation_started_rx) = mpsc::channel();
        let (release_operation_tx, release_operation_rx) = mpsc::channel();
        let native_call = thread::spawn(move || {
            open.run_if_current(|| {
                operation_started_tx.send(()).unwrap();
                release_operation_rx.recv().unwrap();
                Ok(())
            })
            .unwrap();
        });
        operation_started_rx.recv().unwrap();

        let close_order = order.clone();
        let (close_claimed_tx, close_claimed_rx) = mpsc::channel();
        let close_claim = thread::spawn(move || {
            let close = close_order
                .claim(81, false)
                .unwrap()
                .expect("close should be newer");
            close_claimed_tx.send(close).unwrap();
        });
        assert!(
            close_claimed_rx
                .recv_timeout(Duration::from_millis(25))
                .is_err(),
            "close must not publish midway through a native SetWindowRgn call"
        );

        release_operation_tx.send(()).unwrap();
        native_call.join().unwrap();
        let close = close_claimed_rx.recv().unwrap();
        close_claim.join().unwrap();
        assert!(close.is_current());
        assert!(!stale_open.is_current());

        let stale_ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stale_ran_in_call = Arc::clone(&stale_ran);
        stale_open
            .run_if_current(|| {
                stale_ran_in_call.store(true, std::sync::atomic::Ordering::Release);
                Ok(())
            })
            .unwrap();
        assert!(!stale_ran.load(std::sync::atomic::Ordering::Acquire));
    }
}

