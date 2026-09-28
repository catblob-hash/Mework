fn main() {
    // Chromium relaunches this executable for its subprocesses too, exactly as it does `mework`.
    #[cfg(target_os = "macos")]
    if let Some(code) = mework_lib::cef_subprocess_main() {
        std::process::exit(code);
    }
    std::process::exit(mework_lib::run_browser_dev());
}
