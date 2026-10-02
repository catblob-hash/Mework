//! System tray residency: the process outlives its window.
//!
//! Closing the main window only hides it; background subagents, workflows,
//! shell tasks and the sidecar keep running. The tray menu offers exactly two
//! actions — bring the window back, or quit — and quitting is the only one
//! that runs the exit barrier. If the tray cannot be installed the window
//! close falls back to quitting, so the process can never become unreachable.
//!
//! On macOS the tray is a menu bar extra, and the app keeps to the menu bar
//! the way it keeps to the notification area on Windows: a closed window takes
//! it out of the Dock and the app switcher, and bringing the window back
//! restores both.

use tauri::{
    image::Image,
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager,
};

use crate::model::ResolvedLanguage;

pub(crate) const TRAY_ID: &str = "main";
const OPEN_WINDOW_ID: &str = "tray-open-window";
const QUIT_ID: &str = "tray-quit";
const TOOLTIP: &str = "Mework";

/// Handles the tray keeps so a language change can relabel the menu in place.
/// Managed on the desktop app only; `try_state` doubles as "is a tray installed".
pub(crate) struct AppTray {
    open_window: MenuItem<tauri::Wry>,
    quit: MenuItem<tauri::Wry>,
    /// The menu a right click lends the menu bar item (see `pop_up_menu`).
    #[cfg(target_os = "macos")]
    menu: Menu<tauri::Wry>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TrayLabels {
    pub open_window: &'static str,
    pub quit: &'static str,
}

/// A Mac menu bar extra speaks the Mac's words: the window is the app there,
/// and its quit is the same 退出 as the application menu's.
#[cfg(target_os = "macos")]
pub(crate) fn tray_labels(language: ResolvedLanguage) -> TrayLabels {
    match language {
        ResolvedLanguage::ZhCn => TrayLabels {
            open_window: "打开 Mework",
            quit: "退出 Mework",
        },
        ResolvedLanguage::EnUs => TrayLabels {
            open_window: "Open Mework",
            quit: "Quit Mework",
        },
    }
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn tray_labels(language: ResolvedLanguage) -> TrayLabels {
    match language {
        ResolvedLanguage::ZhCn => TrayLabels {
            open_window: "打开 Mework 窗口",
            quit: "关闭 Mework",
        },
        ResolvedLanguage::EnUs => TrayLabels {
            open_window: "Open Mework window",
            quit: "Quit Mework",
        },
    }
}

/// Builds the tray icon and its menu. Must run on the main thread (Tauri's
/// `setup` does) after the main window exists.
pub(crate) fn install(
    app: &AppHandle,
    main_window_label: &'static str,
    language: ResolvedLanguage,
    on_quit: impl Fn(&AppHandle) + Send + Sync + 'static,
) -> Result<(), String> {
    if !notification_area_available() {
        return Err("系统任务栏未运行，没有可放置图标的通知区域".to_owned());
    }
    let icon = tray_icon(app)?;
    let labels = tray_labels(language);
    let open_window =
        MenuItem::with_id(app, OPEN_WINDOW_ID, labels.open_window, true, None::<&str>)
            .map_err(|error| format!("无法创建托盘菜单项: {error}"))?;
    let quit = MenuItem::with_id(app, QUIT_ID, labels.quit, true, None::<&str>)
        .map_err(|error| format!("无法创建托盘菜单项: {error}"))?;
    let separator = PredefinedMenuItem::separator(app)
        .map_err(|error| format!("无法创建托盘菜单分隔线: {error}"))?;
    let menu = Menu::with_items(app, &[&open_window, &separator, &quit])
        .map_err(|error| format!("无法创建托盘菜单: {error}"))?;

    let builder = TrayIconBuilder::with_id(TRAY_ID);
    #[cfg(target_os = "macos")]
    let builder = builder.icon_as_template(true);
    #[cfg(not(target_os = "macos"))]
    let builder = builder.menu(&menu).show_menu_on_left_click(false);
    builder
        .icon(icon)
        .tooltip(TOOLTIP)
        .on_menu_event(move |app, event| {
            let id = event.id().as_ref();
            if id == OPEN_WINDOW_ID {
                show_main_window(app, main_window_label);
            } else if id == QUIT_ID {
                on_quit(app);
            }
        })
        .on_tray_icon_event(move |tray, event| match event {
            TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            }
            | TrayIconEvent::DoubleClick {
                button: MouseButton::Left,
                ..
            } => show_main_window(tray.app_handle(), main_window_label),
            #[cfg(target_os = "macos")]
            TrayIconEvent::Click {
                button: MouseButton::Right,
                button_state: MouseButtonState::Down,
                ..
            } => pop_up_menu(tray),
            _ => {}
        })
        .build(app)
        .map_err(|error| format!("无法创建系统托盘图标: {error}"))?;

    app.manage(AppTray {
        open_window,
        quit,
        #[cfg(target_os = "macos")]
        menu,
    });
    Ok(())
}

/// Opens the menu for a right click on the menu bar item.
///
/// macOS gives every click on a status item that owns a menu to that menu, so
/// with the menu attached a left click opened it as well and never reached the
/// tray's click event (`show_menu_on_left_click(false)` notwithstanding). The
/// item is therefore built without one and is lent it only here: attached,
/// opened, detached — how AppKit apps give a status item a left-click action
/// and a right-click menu. A chosen item still arrives through
/// `on_menu_event`, which listens to every menu, attached or not.
#[cfg(target_os = "macos")]
fn pop_up_menu(tray: &tauri::tray::TrayIcon<tauri::Wry>) {
    let Some(state) = tray.app_handle().try_state::<AppTray>() else {
        return;
    };
    if let Err(error) = tray.set_menu(Some(state.menu.clone())) {
        eprintln!("无法打开菜单栏菜单：{error}");
        return;
    }
    if let Err(error) = tray.with_inner_tray_icon(|inner| inner.show_menu()) {
        eprintln!("无法打开菜单栏菜单：{error}");
    }
    if let Err(error) = tray.set_menu(None::<Menu<tauri::Wry>>) {
        eprintln!("无法收起菜单栏菜单，左键将改为打开菜单：{error}");
    }
}

/// The tray's picture. A Mac menu bar extra is a template image — the eared
/// mark, black on clear, which the menu bar tints for a light, dark or
/// highlighted bar (rendered by build.rs) — where the coloured application
/// icon would stand out as a tile among the system's glyphs.
#[cfg(target_os = "macos")]
fn tray_icon(_app: &AppHandle) -> Result<Image<'static>, String> {
    decode_menu_bar_template()
}

#[cfg(not(target_os = "macos"))]
fn tray_icon(app: &AppHandle) -> Result<Image<'static>, String> {
    app.default_window_icon()
        .cloned()
        .ok_or_else(|| "应用没有可用于托盘的默认图标".to_owned())
}

#[cfg(target_os = "macos")]
fn decode_menu_bar_template() -> Result<Image<'static>, String> {
    const TEMPLATE_PNG: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/tray-template.png"));
    let failed = |error: png::DecodingError| format!("无法解码菜单栏图标: {error}");
    let mut reader = png::Decoder::new(TEMPLATE_PNG).read_info().map_err(failed)?;
    let mut rgba = vec![0; reader.output_buffer_size()];
    let frame = reader.next_frame(&mut rgba).map_err(failed)?;
    if frame.color_type != png::ColorType::Rgba || frame.bit_depth != png::BitDepth::Eight {
        return Err(format!(
            "菜单栏图标不是 8 位 RGBA：{:?} {:?}",
            frame.color_type, frame.bit_depth
        ));
    }
    rgba.truncate(frame.buffer_size());
    Ok(Image::new_owned(rgba, frame.width, frame.height))
}

pub(crate) fn is_installed(app: &AppHandle) -> bool {
    app.try_state::<AppTray>().is_some()
}

/// Whether the shell currently offers a notification area to register with.
///
/// `tray-icon` treats a failed `Shell_NotifyIconW(NIM_ADD)` as success and only
/// retries when the taskbar is (re)created, so without this check a launch
/// while Explorer is down would report a tray that nobody can see — and the
/// window close would hide the app with no visible way to quit. Skipping the
/// tray instead keeps closing the window as the way out.
#[cfg(windows)]
fn notification_area_available() -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::FindWindowW;
    let class = "Shell_TrayWnd"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect::<Vec<u16>>();
    !unsafe { FindWindowW(class.as_ptr(), std::ptr::null()) }.is_null()
}

#[cfg(not(windows))]
fn notification_area_available() -> bool {
    true
}

/// Reveals the main window wherever it is: hidden in the tray, minimized, or
/// behind other windows. Safe to call from any thread and when no window exists.
///
/// This must resolve the `Window`, not the `WebviewWindow`: once the built-in
/// browser attaches a page webview to the main window, Tauri's
/// `get_webview_window` no longer considers it a webview window and returns
/// `None`, which would silently turn both helpers into no-ops.
pub(crate) fn show_main_window(app: &AppHandle, main_window_label: &str) {
    let Some(window) = app.get_window(main_window_label) else {
        return;
    };
    // Queued on the event loop ahead of the show, so the window returns to an
    // app that is back in the Dock and can take the focus.
    #[cfg(target_os = "macos")]
    set_in_dock(app, true);
    if let Err(error) = window.show() {
        eprintln!("无法显示主窗口：{error}");
    }
    if let Err(error) = window.unminimize() {
        eprintln!("无法还原主窗口：{error}");
    }
    if let Err(error) = window.set_focus() {
        eprintln!("无法聚焦主窗口：{error}");
    }
}

pub(crate) fn hide_main_window(app: &AppHandle, main_window_label: &str) {
    let Some(window) = app.get_window(main_window_label) else {
        return;
    };
    if let Err(error) = window.hide() {
        eprintln!("无法隐藏主窗口：{error}");
        return;
    }
    #[cfg(target_os = "macos")]
    set_in_dock(app, false);
}

/// Puts the app in the Dock and the app switcher, or keeps it to the menu bar.
///
/// This is the activation policy, not `set_dock_visibility`: tao's dock toggle
/// goes through `TransformProcessType`, ignores a hide within a second of a
/// show (a window closed right after it was opened would leave the Dock icon
/// behind), and marks every window `canHide = NO`, which would break Cmd+H.
#[cfg(target_os = "macos")]
fn set_in_dock(app: &AppHandle, in_dock: bool) {
    let policy = if in_dock {
        tauri::ActivationPolicy::Regular
    } else {
        tauri::ActivationPolicy::Accessory
    };
    if let Err(error) = app.set_activation_policy(policy) {
        eprintln!("无法切换程序坞图标：{error}");
        return;
    }
    #[cfg(dev)]
    if in_dock {
        restore_development_dock_icon(app);
    }
}

/// Gives the Dock the development build's icon again once the app is back in it.
///
/// A development build is a bare executable with no bundle icon, so Tauri hands
/// the Dock its icon at start-up through `setApplicationIconImage`. Leaving the
/// Dock removes that tile, and the one macOS creates on the way back is drawn
/// from the bundle — the generic executable icon — while AppKit keeps the image
/// without sending it again. The Dock has the new tile by the time the policy
/// change returns, so setting the image once more, queued behind it, lands on
/// that tile. A bundled app's tile comes from its icon file and needs none of this.
#[cfg(all(target_os = "macos", dev))]
fn restore_development_dock_icon(app: &AppHandle) {
    let posted = app.run_on_main_thread(|| {
        let Some(mtm) = objc2::MainThreadMarker::new() else {
            return;
        };
        let application = objc2_app_kit::NSApplication::sharedApplication(mtm);
        if let Some(icon) = application.applicationIconImage() {
            unsafe { application.setApplicationIconImage(Some(&icon)) };
        }
    });
    if let Err(error) = posted {
        eprintln!("无法恢复程序坞图标：{error}");
    }
}

/// Relabels the tray menu for `language`. A no-op without a tray. The text
/// change is posted to the main thread rather than awaited, so a document save
/// on a worker never blocks on the event loop.
pub(crate) fn apply_language(app: &AppHandle, language: ResolvedLanguage) {
    let Some(tray) = app.try_state::<AppTray>() else {
        return;
    };
    let open_window = tray.open_window.clone();
    let quit = tray.quit.clone();
    let labels = tray_labels(language);
    let posted = app.run_on_main_thread(move || {
        if let Err(error) = open_window.set_text(labels.open_window) {
            eprintln!("无法更新托盘菜单文字：{error}");
        }
        if let Err(error) = quit.set_text(labels.quit) {
            eprintln!("无法更新托盘菜单文字：{error}");
        }
    });
    if let Err(error) = posted {
        eprintln!("无法调度托盘菜单文字更新：{error}");
    }
}

#[cfg(test)]
mod tests {
    use super::{tray_labels, ResolvedLanguage};

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn labels_follow_the_resolved_application_language() {
        let chinese = tray_labels(ResolvedLanguage::ZhCn);
        assert_eq!(chinese.open_window, "打开 Mework 窗口");
        assert_eq!(chinese.quit, "关闭 Mework");
        let english = tray_labels(ResolvedLanguage::EnUs);
        assert_eq!(english.open_window, "Open Mework window");
        assert_eq!(english.quit, "Quit Mework");
        assert_ne!(chinese, english);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn menu_bar_labels_follow_the_resolved_application_language() {
        let chinese = tray_labels(ResolvedLanguage::ZhCn);
        assert_eq!(chinese.open_window, "打开 Mework");
        assert_eq!(chinese.quit, "退出 Mework");
        let english = tray_labels(ResolvedLanguage::EnUs);
        assert_eq!(english.open_window, "Open Mework");
        assert_eq!(english.quit, "Quit Mework");
        assert_ne!(chinese, english);
    }

    /// A template image is read for its alpha alone, so anything but black
    /// would mean the coloured icon slipped in; the mark must also be all
    /// there, opaque at its heart and clear around it.
    #[cfg(target_os = "macos")]
    #[test]
    fn menu_bar_icon_is_the_mark_as_a_template_at_2x() {
        let icon = super::decode_menu_bar_template().expect("decode the menu bar icon");
        assert_eq!((icon.width(), icon.height()), (18, 36));
        let pixels = icon.rgba().chunks_exact(4).collect::<Vec<_>>();
        assert!(pixels.iter().all(|pixel| pixel[..3] == [0, 0, 0]));
        let alpha = |x: u32, y: u32| pixels[(y * icon.width() + x) as usize][3];
        assert_eq!(alpha(9, 25), 255);
        assert_eq!(alpha(9, 1), 0);
        assert_eq!(alpha(9, 34), 0);
        // The notch between the ears.
        assert_eq!(alpha(9, 6), 0);
    }

    #[test]
    fn menu_ids_are_distinct_and_stable() {
        assert_ne!(super::OPEN_WINDOW_ID, super::QUIT_ID);
        assert_eq!(super::TRAY_ID, "main");
    }
}
