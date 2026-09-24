//! The macOS application menu, with a Quit that runs the exit barrier.
//!
//! Tauri's default menu quits through `-[NSApplication terminate:]`, and tao
//! never asks the application first: there is no `applicationShouldTerminate:`,
//! so `applicationWillTerminate:` arrives as `RunEvent::Exit` and the process
//! ends whatever the handler does. Cmd+Q therefore skipped the flushes and the
//! "save failed, keep the application open" answer that the tray's Quit and a
//! window close get from `request_deferred_exit_with_barrier`. This is Tauri's
//! default menu with only that one item replaced by an ordinary item that asks
//! the barrier instead. Quits that bypass every menu (Dock → Quit, logout) still
//! arrive as a bare `RunEvent::Exit`; `finalize_app_shutdown` covers those.

use tauri::{
    menu::{Menu, MenuItem, MenuItemKind},
    AppHandle, Manager,
};

use crate::model::ResolvedLanguage;

const QUIT_ID: &str = "app-menu-quit";

/// The replaced item, kept so a language change can relabel it in place.
pub(crate) struct AppMenu {
    quit: MenuItem<tauri::Wry>,
}

fn quit_label(language: ResolvedLanguage) -> &'static str {
    match language {
        ResolvedLanguage::ZhCn => "退出 Mework",
        ResolvedLanguage::EnUs => "Quit Mework",
    }
}

/// Installs the menu. Must run on the main thread (Tauri's `setup` does).
pub(crate) fn install(
    app: &AppHandle,
    language: ResolvedLanguage,
    on_quit: impl Fn(&AppHandle) + Send + Sync + 'static,
) -> Result<(), String> {
    let failed = |error: tauri::Error| format!("无法创建应用菜单: {error}");
    let menu = Menu::default(app).map_err(failed)?;
    // On macOS the first submenu is the application's own, and Tauri ends it
    // with the predefined Quit.
    let Some(MenuItemKind::Submenu(application)) = menu.items().map_err(failed)?.into_iter().next()
    else {
        return Err("默认应用菜单缺少应用子菜单".into());
    };
    let items = application.items().map_err(failed)?;
    match items.last() {
        Some(last @ MenuItemKind::Predefined(_)) => application.remove(last).map_err(failed)?,
        _ => return Err("默认应用菜单的最后一项不是「退出」".into()),
    }
    let quit = MenuItem::with_id(app, QUIT_ID, quit_label(language), true, Some("CmdOrCtrl+Q"))
        .map_err(failed)?;
    application.append(&quit).map_err(failed)?;
    app.set_menu(menu).map_err(failed)?;
    app.on_menu_event(move |app, event| {
        if event.id().as_ref() == QUIT_ID {
            on_quit(app);
        }
    });
    app.manage(AppMenu { quit });
    Ok(())
}

/// Relabels Quit for `language`. A no-op until `install` succeeded. Posted to
/// the main thread rather than awaited, like the tray's relabel.
pub(crate) fn apply_language(app: &AppHandle, language: ResolvedLanguage) {
    let Some(menu) = app.try_state::<AppMenu>() else {
        return;
    };
    let quit = menu.quit.clone();
    let posted = app.run_on_main_thread(move || {
        if let Err(error) = quit.set_text(quit_label(language)) {
            eprintln!("无法更新应用菜单文字：{error}");
        }
    });
    if let Err(error) = posted {
        eprintln!("无法调度应用菜单文字更新：{error}");
    }
}

#[cfg(test)]
mod tests {
    use super::{quit_label, ResolvedLanguage};

    #[test]
    fn quit_follows_the_resolved_application_language() {
        assert_eq!(quit_label(ResolvedLanguage::ZhCn), "退出 Mework");
        assert_eq!(quit_label(ResolvedLanguage::EnUs), "Quit Mework");
    }
}
