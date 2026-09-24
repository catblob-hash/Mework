//! Chromium for the built-in browser on macOS.
//!
//! The browser pane needs a Chromium page it can drive over the DevTools protocol: screenshots,
//! trusted input, network and console events, held script dialogs, per-tab cookie stores. On
//! Windows WebView2 is that page. On macOS Tauri's webview is WKWebView, which has none of it,
//! cannot be stacked beneath the trusted renderer, and shares one cookie store across tabs.
//! Claude desktop solves the same problem by not using the system webview at all: its pane is
//! an Electron `WebContentsView` — Chromium — added as a child view of the main window and
//! driven through `webContents.debugger`. This module is that arrangement for Mework: the
//! Chromium Embedded Framework, loaded into this process, each page a child view of the main
//! window beside the React WKWebView, DevTools messages sent in-process.
//!
//! The engine starts lazily, on the main thread, the first time a page is needed, so a
//! session that never opens the browser pays nothing. CEF runs with an external message pump
//! ([`pump`]) on the AppKit run loop tao already turns, and the application object tao created
//! is taught Chromium's protocols at run time ([`app_protocol`]).
//!
//! Helper processes: a bundled build ships `Mework Helper.app` (and its GPU/Renderer/Plugin/
//! Alerts variants) in `Contents/Frameworks`, next to the framework, and runs them sandboxed. A
//! development build has no bundle, so the application executable doubles as its own helper:
//! `main` hands any process launched with `--type=` to [`subprocess_main`] before Tauri starts.

mod app_protocol;
pub(crate) mod page;
mod pump;

use std::{
    ffi::CString,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Mutex, MutexGuard,
    },
    time::{Duration, Instant},
};

use cef::*;
use tauri::{AppHandle, Manager};

const FRAMEWORK_DIRECTORY: &str = "Chromium Embedded Framework.framework";
const FRAMEWORK_BINARY: &str = "Chromium Embedded Framework";
const HELPER_NAME: &str = "Mework Helper";
/// Switch the browser process adds to every helper's command line naming the framework it
/// loaded, so a helper never has to rediscover it (or find a different one).
const FRAMEWORK_SWITCH: &str = "mework-cef-framework";
/// The browser-pane profiles live here, one hash-named directory per tab (see `browser.rs`);
/// CEF requires every profile to sit under its root cache path, and keeps its installation-wide
/// state (`Local State`) beside them, where the tab-profile sweep never looks.
const PROFILE_ROOT: &str = "browser-tab-profiles";
const MAIN_THREAD_TIMEOUT: Duration = Duration::from_secs(15);
const CONTEXT_READY_TIMEOUT: Duration = Duration::from_secs(20);
const SHUTDOWN_CLOSE_TIMEOUT: Duration = Duration::from_secs(3);

/// Switches for the browser process. The first three are the ones WebView2 pages get: a page
/// the Agent drives while the user looks elsewhere must not run its timers at 1 Hz or stop
/// painting. The mock keychain keeps Chromium from asking for the login keychain to encrypt
/// cookies of profiles that are single-use and deleted with their tab anyway.
const BROWSER_SWITCHES: &[&str] = &[
    "disable-background-timer-throttling",
    "disable-renderer-backgrounding",
    "disable-backgrounding-occluded-windows",
    "use-mock-keychain",
    "no-first-run",
    "no-default-browser-check",
    "disable-component-update",
];

enum Engine {
    Idle,
    /// Holds the pump: its timers reach it only weakly, so dropping it would stop CEF.
    Running(#[allow(dead_code)] pump::ExternalPump),
    Failed(String),
    Stopped,
}

static ENGINE: Mutex<Engine> = Mutex::new(Engine::Idle);
static CONTEXT_READY: AtomicBool = AtomicBool::new(false);

fn engine() -> MutexGuard<'static, Engine> {
    ENGINE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub(crate) fn is_main_thread() -> bool {
    objc2::MainThreadMarker::new().is_some()
}

/// Runs `operation` on the main thread and waits for it; inline when already there.
pub(crate) fn on_main<T: Send + 'static>(
    app: &AppHandle,
    stage: &str,
    operation: impl FnOnce() -> T + Send + 'static,
) -> Result<T, String> {
    if is_main_thread() {
        return Ok(operation());
    }
    let (sender, receiver) = mpsc::sync_channel(1);
    app.run_on_main_thread(move || {
        let _ = sender.send(operation());
    })
    .map_err(|error| format!("无法调度{stage}: {error}"))?;
    receiver
        .recv_timeout(MAIN_THREAD_TIMEOUT)
        .map_err(|error| match error {
            mpsc::RecvTimeoutError::Timeout => format!("等待{stage}超时"),
            mpsc::RecvTimeoutError::Disconnected => format!("{stage}没有返回结果"),
        })
}

// ----- helper processes -------------------------------------------------------------------------

/// When this process is one of CEF's helpers (it was launched with `--type=`), runs it and
/// returns its exit code; `None` for the application itself. Call first thing in `main`.
pub fn subprocess_main() -> Option<i32> {
    let arguments = std::env::args().collect::<Vec<_>>();
    if !arguments
        .iter()
        .skip(1)
        .any(|argument| argument.starts_with("--type="))
    {
        return None;
    }
    Some(run_helper(&arguments))
}

/// `Mework Helper`: a bundled helper enters Chromium's sandbox before it loads anything, unless
/// the browser process launched it without one.
pub fn helper_main() -> i32 {
    let arguments = std::env::args().collect::<Vec<_>>();
    let _sandbox = (!arguments.iter().any(|argument| argument == "--no-sandbox")).then(|| {
        let args = args::Args::new();
        let mut sandbox = cef::sandbox::Sandbox::new();
        sandbox.initialize(args.as_main_args());
        sandbox
    });
    run_helper(&arguments)
}

/// The helper process body, shared by the development executable and `Mework Helper`.
pub fn run_helper(arguments: &[String]) -> i32 {
    let prefix = format!("--{FRAMEWORK_SWITCH}=");
    let framework = arguments
        .iter()
        .find_map(|argument| argument.strip_prefix(&prefix))
        .map(PathBuf::from)
        .or_else(|| locate_framework().ok());
    let Some(framework) = framework else {
        eprintln!("Mework helper: Chromium Embedded Framework not found");
        return 1;
    };
    if let Err(error) = load_framework(&framework) {
        eprintln!("Mework helper: {error}");
        return 1;
    }
    let _ = api_hash(sys::CEF_API_VERSION_LAST, 0);
    let args = args::Args::new();
    execute_process(
        Some(args.as_main_args()),
        None::<&mut App>,
        std::ptr::null_mut(),
    )
    .max(0)
}

// ----- locating the distribution ----------------------------------------------------------------

/// The framework this process should load: the bundle's own when running from `Mework.app` (or
/// one of its helpers), otherwise the distribution this build was compiled against.
fn locate_framework() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("MEWORK_CEF_FRAMEWORK").map(PathBuf::from) {
        return Ok(path);
    }
    let executable =
        std::env::current_exe().map_err(|error| format!("无法确定当前可执行文件位置: {error}"))?;
    // Mework.app/Contents/MacOS/<exe> and
    // Mework.app/Contents/Frameworks/Mework Helper.app/Contents/MacOS/<helper>.
    let candidates = [
        executable
            .ancestors()
            .nth(2)
            .map(|contents| contents.join("Frameworks")),
        executable.ancestors().nth(4).map(Path::to_path_buf),
        option_env!("MEWORK_CEF_DIR").map(PathBuf::from),
    ];
    candidates
        .into_iter()
        .flatten()
        .map(|directory| directory.join(FRAMEWORK_DIRECTORY))
        .find(|framework| framework.join(FRAMEWORK_BINARY).is_file())
        .ok_or_else(|| {
            "找不到 Chromium Embedded Framework：内置浏览器在 macOS 上需要随应用打包的 Chromium（开发构建由 cef-dll-sys 下载）"
                .to_owned()
        })
}

/// `Mework Helper` inside the bundle, or this executable when there is no bundle.
fn helper_executable() -> Result<(PathBuf, bool), String> {
    let executable =
        std::env::current_exe().map_err(|error| format!("无法确定当前可执行文件位置: {error}"))?;
    let bundled = executable.ancestors().nth(2).map(|contents| {
        contents
            .join("Frameworks")
            .join(format!("{HELPER_NAME}.app"))
            .join("Contents")
            .join("MacOS")
            .join(HELPER_NAME)
    });
    Ok(match bundled {
        Some(helper) if helper.is_file() => (helper, true),
        _ => (executable, false),
    })
}

fn load_framework(framework: &Path) -> Result<(), String> {
    let binary = CString::new(framework.join(FRAMEWORK_BINARY).as_os_str().as_bytes())
        .map_err(|_| "Chromium Embedded Framework 路径无效".to_owned())?;
    // SAFETY: a NUL-terminated path that outlives the call.
    if load_library(Some(unsafe { &*binary.as_ptr() })) != 1 {
        return Err(format!(
            "无法加载 Chromium Embedded Framework: {}",
            framework.display()
        ));
    }
    Ok(())
}

// ----- the browser process ----------------------------------------------------------------------

wrap_app! {
    struct MeworkCefApp {
        pump: pump::ExternalPump,
        framework: String,
    }

    impl App {
        fn on_before_command_line_processing(
            &self,
            process_type: Option<&CefString>,
            command_line: Option<&mut CommandLine>,
        ) {
            let browser_process = process_type.map_or(true, |kind| kind.to_string().is_empty());
            let Some(command_line) = command_line.filter(|_| browser_process) else {
                return;
            };
            for switch in BROWSER_SWITCHES {
                command_line.append_switch(Some(&CefString::from(*switch)));
            }
        }

        fn browser_process_handler(&self) -> Option<BrowserProcessHandler> {
            Some(MeworkBrowserProcess::new(self.pump.clone(), self.framework.clone()))
        }
    }
}

wrap_browser_process_handler! {
    struct MeworkBrowserProcess {
        pump: pump::ExternalPump,
        framework: String,
    }

    impl BrowserProcessHandler {
        fn on_context_initialized(&self) {
            CONTEXT_READY.store(true, Ordering::SeqCst);
        }

        fn on_before_child_process_launch(&self, command_line: Option<&mut CommandLine>) {
            if let Some(command_line) = command_line {
                command_line.append_switch_with_value(
                    Some(&CefString::from(FRAMEWORK_SWITCH)),
                    Some(&CefString::from(self.framework.as_str())),
                );
            }
        }

        fn on_schedule_message_pump_work(&self, delay_ms: i64) {
            self.pump.schedule(delay_ms);
        }

        /// Mework keeps one instance itself; Chromium must never open a window for a relaunch.
        fn on_already_running_app_relaunch(
            &self,
            _command_line: Option<&mut CommandLine>,
            _current_directory: Option<&CefString>,
        ) -> ::std::os::raw::c_int {
            1
        }
    }
}

/// Starts the engine if it is not running and waits until it can create pages. Any thread.
pub(crate) fn ensure_started(app: &AppHandle) -> Result<(), String> {
    match &*engine() {
        Engine::Running(_) => return wait_for_context(),
        Engine::Failed(error) => return Err(error.clone()),
        Engine::Stopped => return Err("内置浏览器引擎已关闭".into()),
        Engine::Idle => {}
    }
    let root = app
        .path()
        .app_local_data_dir()
        .map_err(|error| format!("无法解析内置浏览器数据目录: {error}"))?
        .join(PROFILE_ROOT);
    on_main(app, "启动内置浏览器引擎", move || {
        start_on_main(&root)
    })??;
    wait_for_context()
}

fn start_on_main(root: &Path) -> Result<(), String> {
    let mut engine = engine();
    match &*engine {
        Engine::Running(_) => return Ok(()),
        Engine::Failed(error) => return Err(error.clone()),
        Engine::Stopped => return Err("内置浏览器引擎已关闭".into()),
        Engine::Idle => {}
    }
    match start_engine(root) {
        Ok(pump) => {
            *engine = Engine::Running(pump);
            Ok(())
        }
        Err(error) => {
            // CEF cannot be initialized twice in one process; a failure is final until restart.
            *engine = Engine::Failed(error.clone());
            Err(error)
        }
    }
}

fn start_engine(root: &Path) -> Result<pump::ExternalPump, String> {
    let framework = locate_framework()?;
    load_framework(&framework)?;
    let _ = api_hash(sys::CEF_API_VERSION_LAST, 0);
    app_protocol::install()?;
    std::fs::create_dir_all(root)
        .map_err(|error| format!("无法创建内置浏览器数据目录: {error}"))?;
    let (helper, bundled_helper) = helper_executable()?;
    let text = |path: &Path| -> Result<String, String> {
        path.to_str()
            .map(str::to_owned)
            .ok_or_else(|| format!("路径不是有效的 Unicode: {}", path.display()))
    };
    let framework_text = text(&framework)?;
    let settings = Settings {
        // The sandbox needs the signed helper bundle, which initializes it before loading the
        // framework; the development executable acting as its own helper cannot.
        no_sandbox: (!bundled_helper) as i32,
        external_message_pump: 1,
        // Chromium switches on Mework's own command line would let whoever launches it turn on
        // remote debugging or turn off web security. Only the switches above reach it.
        command_line_args_disabled: 1,
        browser_subprocess_path: CefString::from(text(&helper)?.as_str()),
        framework_dir_path: CefString::from(framework_text.as_str()),
        root_cache_path: CefString::from(text(root)?.as_str()),
        // The global context is in memory; every page gets its own on-disk tab profile.
        persist_session_cookies: 0,
        log_file: CefString::from(text(&root.join("cef.log"))?.as_str()),
        log_severity: LogSeverity::WARNING,
        ..Default::default()
    };
    let pump = pump::ExternalPump::new();
    let mut app = MeworkCefApp::new(pump.clone(), framework_text);
    let args = args::Args::new();
    if initialize(
        Some(args.as_main_args()),
        Some(&settings),
        Some(&mut app),
        std::ptr::null_mut(),
    ) != 1
    {
        return Err("Chromium Embedded Framework 初始化失败".into());
    }
    pump.do_work();
    Ok(pump)
}

fn wait_for_context() -> Result<(), String> {
    let deadline = Instant::now() + CONTEXT_READY_TIMEOUT;
    while !CONTEXT_READY.load(Ordering::SeqCst) {
        if Instant::now() >= deadline {
            return Err("内置浏览器引擎启动超时".into());
        }
        if is_main_thread() {
            // Nothing else turns the run loop while the main thread waits here.
            do_message_loop_work();
            std::thread::sleep(Duration::from_millis(5));
        } else {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    Ok(())
}

/// Closes every page and shuts the engine down. Main thread, at application exit; a no-op when
/// the engine never started.
pub(crate) fn shutdown() {
    if !is_main_thread() {
        return;
    }
    let running = {
        let mut engine = engine();
        matches!(
            std::mem::replace(&mut *engine, Engine::Stopped),
            Engine::Running(_)
        )
    };
    if !running {
        return;
    }
    page::close_all_for_shutdown();
    let deadline = Instant::now() + SHUTDOWN_CLOSE_TIMEOUT;
    while page::live_count() > 0 && Instant::now() < deadline {
        do_message_loop_work();
        std::thread::sleep(Duration::from_millis(5));
    }
    pump::stop();
    // Chromium refuses to shut down under a live browser; leaving it to process exit is safer
    // than tripping that check.
    if page::live_count() == 0 {
        cef::shutdown();
    }
}
