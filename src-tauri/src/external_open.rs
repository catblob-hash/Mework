//! Open external UI links in the operating system's default browser.
//!
//! The application WebView never navigates across origins, so links require this
//! explicit route. Validation reuses [`crate::browser::is_navigation_allowed`]
//! and additionally rejects `about:blank`, which is only valid as an internal
//! initial page.

use url::Url;

/// Maximum URL length passed to the operating system.
const MAX_EXTERNAL_URL_LENGTH: usize = 8192;

/// Parse and validate an external URL for the default browser.
///
/// Callers must pass the normalized returned URL to the system rather than the
/// raw input, which may differ after percent encoding and IDNA normalization.
pub fn parse_external_url(raw: &str) -> Result<Url, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("外部地址为空".to_owned());
    }
    if trimmed.len() > MAX_EXTERNAL_URL_LENGTH {
        return Err(format!(
            "外部地址超过 {MAX_EXTERNAL_URL_LENGTH} 字节上限，拒绝交给系统打开"
        ));
    }
    // Reject controls before parsing because parsers may silently remove some
    // while preserving others.
    if trimmed.chars().any(char::is_control) {
        return Err("外部地址包含控制字符，拒绝交给系统打开".to_owned());
    }

    let url = Url::parse(trimmed).map_err(|error| format!("外部地址无法解析: {error}"))?;

    // `is_navigation_allowed` permits the embedded browser's `about:blank` page;
    // external opening must accept only HTTP(S).
    if !matches!(url.scheme(), "http" | "https") {
        return Err(format!(
            "只能用默认浏览器打开 http(s) 地址，收到的是 {}:",
            url.scheme()
        ));
    }
    if !crate::browser::is_navigation_allowed(&url) {
        return Err(format!(
            "拒绝用默认浏览器打开这个地址: {}",
            blocked_summary(&url)
        ));
    }
    Ok(url)
}

/// Summarize only an origin for user-facing validation errors.
fn blocked_summary(url: &Url) -> String {
    let origin = url.origin().ascii_serialization();
    if origin == "null" {
        format!("{}:", url.scheme())
    } else {
        origin
    }
}

/// Validate a URL and pass it to the default browser.
///
/// Success means the operating system accepted the launch request, not that a
/// browser rendered the page.
pub fn open_in_default_browser(raw: &str) -> Result<(), String> {
    let url = parse_external_url(raw)?;
    launch(url.as_str())
}

#[cfg(windows)]
fn launch(url: &str) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt as _;
    use windows_sys::Win32::System::Com::{
        CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE,
    };
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    fn wide(value: &str) -> Vec<u16> {
        std::ffi::OsStr::new(value)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    // Do not use `cmd /c start` or `explorer.exe`: each adds command-line
    // parsing. ShellExecuteW passes the URL directly to its registered handler.
    let verb = wide("open");
    let target = wide(url);

    // Shell extensions can require COM. Use a short-lived thread so apartment
    // state cannot leak into a reused Tokio blocking-pool thread.
    let handle = std::thread::Builder::new()
        .name("mework-open-external".to_owned())
        .spawn(move || {
            // SAFETY: Both wide strings are NUL-terminated and live for the
            // call; null pointers request documented defaults.
            unsafe {
                let com = CoInitializeEx(
                    std::ptr::null(),
                    (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32,
                );
                let result = ShellExecuteW(
                    std::ptr::null_mut(),
                    verb.as_ptr(),
                    target.as_ptr(),
                    std::ptr::null(),
                    std::ptr::null(),
                    SW_SHOWNORMAL,
                );
                // Only successful COM initialization requires the matching call.
                if com >= 0 {
                    CoUninitialize();
                }
                // ShellExecuteW succeeds only for return values greater than 32.
                result as isize
            }
        })
        .map_err(|error| format!("无法启动打开外部链接的线程: {error}"))?;

    let result = handle
        .join()
        .map_err(|_| "打开外部链接的线程异常退出".to_owned())?;
    if result > 32 {
        Ok(())
    } else {
        Err(format!("系统拒绝打开这个链接（ShellExecute 返回 {result}）"))
    }
}

#[cfg(not(windows))]
fn launch(url: &str) -> Result<(), String> {
    // Validated HTTP(S) URLs cannot be interpreted as options by `open` or
    // `xdg-open`, which would require a leading `-`.
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    std::process::Command::new(opener)
        .arg(url)
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("无法打开外部链接: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_ordinary_remote_pages() {
        for candidate in [
            "https://example.com",
            "https://example.com/docs?a=1&b=2#frag",
            "http://example.com:8443/path",
            // User-hosted services are valid; only the application's port 1420 is reserved.
            "http://localhost:8080/search",
            "http://127.0.0.1:3000/",
        ] {
            assert!(
                parse_external_url(candidate).is_ok(),
                "{candidate} should be openable"
            );
        }
    }

    #[test]
    fn normalizes_before_handing_the_string_to_the_system() {
        let url = parse_external_url("  https://example.com/a b  ").expect("space is escapable");
        assert_eq!(url.as_str(), "https://example.com/a%20b");
    }

    #[test]
    fn rejects_every_scheme_other_than_http_and_https() {
        for candidate in [
            "file:///C:/Windows/System32/calc.exe",
            "javascript:alert(1)",
            "data:text/html,<script>alert(1)</script>",
            "vbscript:msgbox(1)",
            "ms-settings:privacy",
            "search-ms:query=x",
            "shell:startup",
            "about:blank",
            "tauri://localhost/",
            "ipc://localhost/",
        ] {
            assert!(
                parse_external_url(candidate).is_err(),
                "{candidate} must not reach the system"
            );
        }
    }

    #[test]
    fn rejects_relative_and_malformed_input() {
        for candidate in ["", "   ", "example.com", "/docs/readme", "https://", "http://"] {
            assert!(
                parse_external_url(candidate).is_err(),
                "{candidate:?} must not reach the system"
            );
        }
    }

    #[test]
    fn rejects_userinfo_spoofing() {
        for candidate in [
            "https://evil.example@good.example/",
            "https://user:pass@good.example/",
            "https://good.example%40evil.example@evil.example/",
        ] {
            assert!(
                parse_external_url(candidate).is_err(),
                "{candidate} must not reach the system"
            );
        }
    }

    #[test]
    fn rejects_the_applications_own_origins() {
        for candidate in [
            "http://tauri.localhost/index.html",
            "https://tauri.localhost/",
            "http://asset.localhost/x",
            "http://ipc.localhost/",
            "http://sub.tauri.localhost/",
        ] {
            assert!(
                parse_external_url(candidate).is_err(),
                "{candidate} must not reach the system"
            );
        }
    }

    /// The development-server reservation belongs to the embedded page WebView,
    /// which must not load the origin the trusted frontend is served from. The
    /// system browser is a different process outside the application entirely, and
    /// opening a local development server in it is the ordinary thing to want.
    #[test]
    fn allows_a_local_development_server_to_reach_the_system_browser() {
        for candidate in [
            "http://localhost:1420/",
            "http://127.0.0.1:1420/",
            "http://localhost:3000/",
        ] {
            assert!(
                parse_external_url(candidate).is_ok(),
                "{candidate} must reach the system browser"
            );
        }
    }

    #[test]
    fn rejects_control_characters_and_oversized_input() {
        assert!(parse_external_url("https://example.com/\u{0}x").is_err());
        assert!(parse_external_url("https://example.com/\nx").is_err());
        assert!(parse_external_url("https://example.com/\tx").is_err());
        let long = format!("https://example.com/{}", "a".repeat(MAX_EXTERNAL_URL_LENGTH));
        assert!(parse_external_url(&long).is_err());
    }

    #[test]
    fn error_text_never_echoes_the_whole_untrusted_address() {
        let error = parse_external_url("http://tauri.localhost/secret-token-path?q=secret")
            .expect_err("reserved origin");
        assert!(!error.contains("secret-token-path"));
        assert!(!error.contains("q=secret"));
        assert!(error.contains("http://tauri.localhost"));
    }
}
