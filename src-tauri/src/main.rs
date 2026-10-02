#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // A development build has no helper bundle, so Chromium relaunches this executable for its
    // renderer, GPU and utility processes; they must never reach the application.
    #[cfg(target_os = "macos")]
    if let Some(code) = mework_lib::cef_subprocess_main() {
        std::process::exit(code);
    }
    #[cfg(target_os = "macos")]
    mework_lib::cef_preload_framework();
    mework_lib::run();
}
