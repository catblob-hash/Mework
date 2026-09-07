//! System tray residency: the process outlives its window.
//!
//! Closing the main window only hides it; background subagents, workflows,
//! shell tasks and the sidecar keep running. The tray menu offers exactly two
//! actions — bring the window back, or quit — and quitting is the only one
//! that runs the exit barrier. If the tray cannot be installed the window
//! close falls back to quitting, so the process can never become unreachable.

use tauri::{
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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TrayLabels {
    pub open_window: &'static str,
    pub quit: &'static str,
}

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
    let icon = app
        .default_window_icon()
        .cloned()
        .ok_or_else(|| "应用没有可用于托盘的默认图标".to_owned())?;
    let labels = tray_labels(language);
    let open_window = MenuItem::with_id(app, OPEN_WINDOW_ID, labels.open_window, true, None::<&str>)
        .map_err(|error| format!("无法创建托盘菜单项: {error}"))?;
    let quit = MenuItem::with_id(app, QUIT_ID, labels.quit, true, None::<&str>)
        .map_err(|error| format!("无法创建托盘菜单项: {error}"))?;
    let separator = PredefinedMenuItem::separator(app)
        .map_err(|error| format!("无法创建托盘菜单分隔线: {error}"))?;
    let menu = Menu::with_items(app, &[&open_window, &separator, &quit])
        .map_err(|error| format!("无法创建托盘菜单: {error}"))?;

    TrayIconBuilder::with_id(TRAY_ID)
        .icon(icon)
        .tooltip(TOOLTIP)
        .menu(&menu)
        .show_menu_on_left_click(false)
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
            _ => {}
        })
        .build(app)
        .map_err(|error| format!("无法创建系统托盘图标: {error}"))?;

    app.manage(AppTray { open_window, quit });
    Ok(())
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

    #[test]
    fn menu_ids_are_distinct_and_stable() {
        assert_ne!(super::OPEN_WINDOW_ID, super::QUIT_ID);
        assert_eq!(super::TRAY_ID, "main");
    }
}
