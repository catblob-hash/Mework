//! One built-in-browser page on macOS: a CEF browser whose view is a child of the host window's
//! content view, beside the trusted React WKWebView, exactly where Tauri would have put a child
//! webview.
//!
//! `browser.rs` drives pages through a small slice of Tauri's `Webview` API plus the DevTools
//! protocol. [`CefWebview`] offers that slice under the same names, so the page lifecycle,
//! policy and tool code stays one implementation for both engines, and [`CefPageBuilder`]
//! takes the same builder calls as `WebviewBuilder`. What WebView2 answers through its own
//! COM events — held script dialogs, renderer crashes, the navigation veto — CEF answers
//! through its handler interfaces, which are adapted here.
//!
//! Threading: CEF runs every handler, and accepts every browser call, on the main thread. CEF
//! objects never leave it: they live in the thread-local [`NATIVE`] registry keyed by page
//! label, and everything else reaches them by posting to the main thread. The only state
//! shared across threads is [`PageShared`], which holds no CEF object.

use std::{
    cell::RefCell,
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicI32, Ordering},
        mpsc, Arc, Mutex, MutexGuard,
    },
    time::Duration,
};

use cef::*;
use objc2::{msg_send, runtime::AnyClass};
use objc2_app_kit::{NSAutoresizingMaskOptions, NSView, NSWindowOrderingMode};
use objc2_foundation::{NSObjectProtocol, NSPoint, NSRect, NSSize};
use serde_json::{json, Value};
use tauri::{
    webview::{NewWindowResponse, PageLoadEvent},
    AppHandle, LogicalPosition, LogicalSize, Manager, PhysicalPosition, PhysicalSize, Position,
    Size, WebviewUrl, Window, Wry,
};
use url::Url;
use zeroize::Zeroizing;

use super::{is_main_thread, on_main};

/// Every page starts on this document; its real destination is loaded once the page's
/// document-start scripts are registered, so no page ever runs a byte of a remote document
/// before Mework's bootstrap.
const BLANK_URL: &str = "about:blank";
const SCRIPT_REGISTRATION_TIMEOUT: Duration = Duration::from_secs(15);
/// A tab's profile is a new Chromium profile, which initializes asynchronously before its first
/// browser can exist.
const CREATION_TIMEOUT: Duration = Duration::from_secs(20);
const CLOSE_ON_MAIN_TIMEOUT: Duration = Duration::from_secs(2);

// ----- page registry ----------------------------------------------------------------------------

/// Live pages by label. A page leaves when CEF reports it closed, so a label that resolves here
/// is a browser that still exists — the question `app.get_webview(label)` answers for WebView2.
static PAGES: Mutex<Option<HashMap<String, Arc<PageShared>>>> = Mutex::new(None);

fn pages() -> MutexGuard<'static, Option<HashMap<String, Arc<PageShared>>>> {
    PAGES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Main-thread half of a page: the CEF objects themselves.
struct NativePage {
    browser: Browser,
    _registration: Option<Registration>,
    held_dialogs: HashMap<u64, JsdialogCallback>,
}

thread_local! {
    static NATIVE: RefCell<HashMap<String, NativePage>> = RefCell::new(HashMap::new());
}

/// A clone of the page's browser. Never hold the registry borrow across a CEF call: closing a
/// browser can re-enter the handlers below synchronously.
fn native_browser(label: &str) -> Option<Browser> {
    NATIVE.with(|native| native.borrow().get(label).map(|page| page.browser.clone()))
}

/// The page with this label, if its browser is alive.
pub(crate) fn get(label: &str) -> Option<CefWebview> {
    pages()
        .as_ref()
        .and_then(|pages| pages.get(label).cloned())
        .map(|shared| CefWebview { shared })
}

pub(super) fn live_count() -> usize {
    pages().as_ref().map_or(0, HashMap::len)
}

/// Force-closes every page. Main thread only; used when the application exits, whose caller
/// then turns CEF's message loop until the pages report closed.
pub(super) fn close_all_for_shutdown() {
    let labels = NATIVE.with(|native| native.borrow().keys().cloned().collect::<Vec<_>>());
    for label in labels {
        if let Some(host) = native_browser(&label).and_then(|browser| browser.host()) {
            host.close_browser(1);
        }
    }
}

// ----- callbacks the builder collects ------------------------------------------------------------

/// Stand-in for Tauri's `NewWindowFeatures`: `browser.rs` never reads the features.
pub(crate) struct NewWindowFeatures;

/// Stand-in for Tauri's `DownloadEvent`: every download is refused before it starts.
pub(crate) struct DownloadEvent {
    #[allow(dead_code)]
    pub(crate) url: String,
}

/// The payload of a main-frame load, with the accessors Tauri's `PageLoadPayload` has.
pub(crate) struct PageLoadPayload {
    url: Url,
    event: PageLoadEvent,
}

impl PageLoadPayload {
    pub(crate) fn url(&self) -> &Url {
        &self.url
    }

    pub(crate) fn event(&self) -> PageLoadEvent {
        self.event
    }
}

type NavigationCallback = Box<dyn Fn(&Url) -> bool + Send>;
type NewWindowCallback = Box<dyn Fn(Url, NewWindowFeatures) -> NewWindowResponse<Wry> + Send>;
type DownloadCallback = Box<dyn Fn(CefWebview, DownloadEvent) -> bool + Send>;
type PageLoadCallback = Box<dyn Fn(CefWebview, PageLoadPayload) + Send>;
type TitleCallback = Box<dyn Fn(CefWebview, String) + Send>;

/// A value only ever used on the main thread, where CEF calls every handler. The builder's
/// callbacks are `Send` but, as with Tauri, not required to be `Sync`.
struct MainOnly<T>(T);

// SAFETY: the only access is `get`, which refuses every thread but the main one, so no two
// threads can ever hold a reference at once; moving the value (Send) is all that crosses threads.
unsafe impl<T: Send> Sync for MainOnly<T> {}

impl<T> MainOnly<T> {
    fn get(&self) -> Option<&T> {
        is_main_thread().then_some(&self.0)
    }
}

#[derive(Default)]
struct PageCallbacks {
    navigation: Option<NavigationCallback>,
    new_window: Option<NewWindowCallback>,
    download: Option<DownloadCallback>,
    page_load: Option<PageLoadCallback>,
    title: Option<TitleCallback>,
}

// ----- dialogs and crashes ----------------------------------------------------------------------

/// A script dialog the page opened.
pub(crate) struct PageDialog {
    /// `alert`, `confirm`, `prompt` or `beforeunload`.
    pub(crate) kind: &'static str,
    pub(crate) message: String,
    pub(crate) default_text: String,
    pub(crate) url: String,
}

/// What to do with a dialog as it opens.
pub(crate) enum DialogDecision {
    /// Keep it open until [`CefWebview::answer_dialog`] is called with this id. The page blocks
    /// in the dialog exactly as it would in a browser.
    Hold(u64),
    /// Answer immediately.
    Answer { accept: bool, text: Option<String> },
}

type DialogHandlerFn = Arc<dyn Fn(PageDialog) -> DialogDecision + Send + Sync>;
type CrashHandlerFn = Arc<dyn Fn(&'static str) + Send + Sync>;
type EventListener = Arc<dyn Fn(&str, &str) + Send + Sync>;
type CdpCompletion = Box<dyn FnOnce(Result<String, String>) + Send>;

// ----- shared page state ------------------------------------------------------------------------

/// Logical-pixel frame of the page within its window, top-left origin.
#[derive(Clone, Copy, Debug)]
struct LogicalFrame {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

struct PageShared {
    label: String,
    app: AppHandle,
    window: Window,
    callbacks: MainOnly<PageCallbacks>,
    frame: Mutex<LogicalFrame>,
    closed: AtomicBool,
    pending: Mutex<HashMap<i32, CdpCompletion>>,
    listeners: Mutex<Vec<EventListener>>,
    dialog_handler: Mutex<Option<DialogHandlerFn>>,
    crash_handler: Mutex<Option<CrashHandlerFn>>,
    /// Signalled by `on_after_created` once the page is registered.
    created: Mutex<Option<mpsc::SyncSender<()>>>,
    /// Set when [`add_child`] gave up waiting; a browser created after that is closed at once.
    abandoned: AtomicBool,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// DevTools message ids are per browser in CEF, but one process-wide counter keeps every id
/// unique even across the pages' shared observer bookkeeping.
static NEXT_DEVTOOLS_ID: AtomicI32 = AtomicI32::new(1);

fn next_devtools_id() -> i32 {
    NEXT_DEVTOOLS_ID
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| {
            Some(if id == i32::MAX { 1 } else { id + 1 })
        })
        .unwrap_or(1)
}

impl PageShared {
    fn complete(&self, id: i32, result: Result<String, String>) {
        let completion = lock(&self.pending).remove(&id);
        if let Some(completion) = completion {
            completion(result);
        }
    }

    fn fail_all_pending(&self, reason: &str) {
        let pending = std::mem::take(&mut *lock(&self.pending));
        for (_, completion) in pending {
            completion(Err(reason.to_owned()));
        }
    }

    fn dispatch_event(&self, method: &str, params: &str) {
        let listeners = lock(&self.listeners).clone();
        for listener in listeners {
            listener(method, params);
        }
    }

    fn handle(self: &Arc<Self>) -> CefWebview {
        CefWebview {
            shared: self.clone(),
        }
    }

    fn page_load(self: &Arc<Self>, url: &str, event: PageLoadEvent) {
        let Ok(url) = Url::parse(url) else {
            return;
        };
        if let Some(handler) = self
            .callbacks
            .get()
            .and_then(|callbacks| callbacks.page_load.as_ref())
        {
            handler(self.handle(), PageLoadPayload { url, event });
        }
    }

    /// Consults the navigation policy. A page without one allows everything, as Tauri does.
    fn allows_navigation(&self, url: &str) -> bool {
        let Ok(url) = Url::parse(url) else {
            return false;
        };
        self.callbacks
            .get()
            .and_then(|callbacks| callbacks.navigation.as_ref())
            .map_or(true, |handler| handler(&url))
    }

    /// Reports a window the page tried to open. The page is single-view: whatever the handler
    /// answers, no second browser is ever created.
    fn report_new_window(&self, url: &str) {
        let Ok(url) = Url::parse(url) else {
            return;
        };
        if let Some(handler) = self
            .callbacks
            .get()
            .and_then(|callbacks| callbacks.new_window.as_ref())
        {
            let _ = handler(url, NewWindowFeatures);
        }
    }

    fn open_dialog(&self, dialog: PageDialog, callback: JsdialogCallback) {
        let handler = lock(&self.dialog_handler).clone();
        let decision = match handler {
            Some(handler) => handler(dialog),
            // Nobody asked to see it: answer as a dismissed dialog, never as a native one.
            None => DialogDecision::Answer {
                accept: false,
                text: None,
            },
        };
        match decision {
            DialogDecision::Hold(id) => NATIVE.with(|native| {
                if let Some(page) = native.borrow_mut().get_mut(&self.label) {
                    page.held_dialogs.insert(id, callback);
                }
            }),
            DialogDecision::Answer { accept, text } => {
                let text = text.map(|text| CefString::from(text.as_str()));
                callback.cont(accept as i32, text.as_ref());
            }
        }
    }
}

// ----- the page handle --------------------------------------------------------------------------

/// A live (or closing) CEF page, addressed the way `browser.rs` addresses a Tauri webview.
#[derive(Clone)]
pub(crate) struct CefWebview {
    shared: Arc<PageShared>,
}

impl CefWebview {
    #[allow(dead_code)]
    pub(crate) fn label(&self) -> &str {
        &self.shared.label
    }

    pub(crate) fn window(&self) -> Window {
        self.shared.window.clone()
    }

    pub(crate) fn run_on_main_thread<F: FnOnce() + Send + 'static>(
        &self,
        task: F,
    ) -> tauri::Result<()> {
        self.shared.app.run_on_main_thread(task)
    }

    /// Runs `operation` on the main thread with the page's browser, host and view.
    fn with_native<T: Send + 'static>(
        &self,
        stage: &'static str,
        operation: impl FnOnce(&Browser, &BrowserHost, &NSView) -> Result<T, String> + Send + 'static,
    ) -> Result<T, String> {
        let label = self.shared.label.clone();
        on_main(&self.shared.app, stage, move || {
            let browser = native_browser(&label).ok_or_else(|| format!("{stage}: 页面已关闭"))?;
            let host = browser
                .host()
                .ok_or_else(|| format!("{stage}: 页面没有 BrowserHost"))?;
            let view = host_view(&host).ok_or_else(|| format!("{stage}: 页面没有原生视图"))?;
            operation(&browser, &host, view)
        })?
    }

    pub(crate) fn hide(&self) -> Result<(), String> {
        self.with_native("隐藏浏览器页面", |_, _, view| {
            view.setHidden(true);
            Ok(())
        })
    }

    pub(crate) fn show(&self) -> Result<(), String> {
        self.with_native("显示浏览器页面", |_, _, view| {
            view.setHidden(false);
            Ok(())
        })
    }

    pub(crate) fn set_focus(&self) -> Result<(), String> {
        self.with_native("聚焦浏览器页面", |_, host, _| {
            host.set_focus(1);
            Ok(())
        })
    }

    /// Chromium zoom levels are logarithmic with base 1.2; Tauri's factor is linear.
    pub(crate) fn set_zoom(&self, factor: f64) -> Result<(), String> {
        if !factor.is_finite() || factor <= 0.0 {
            return Err("浏览器缩放比例无效".into());
        }
        self.with_native("设置浏览器页面缩放", move |_, host, _| {
            host.set_zoom_level(factor.ln() / 1.2_f64.ln());
            Ok(())
        })
    }

    pub(crate) fn navigate(&self, url: Url) -> Result<(), String> {
        self.with_native("导航浏览器页面", move |browser, _, _| {
            let frame = browser
                .main_frame()
                .ok_or_else(|| "浏览器页面没有主框架".to_owned())?;
            frame.load_url(Some(&CefString::from(url.as_str())));
            Ok(())
        })
    }

    pub(crate) fn reload(&self) -> Result<(), String> {
        self.with_native("重新加载浏览器页面", |browser, _, _| {
            browser.reload();
            Ok(())
        })
    }

    pub(crate) fn set_position(&self, position: impl Into<Position>) -> Result<(), String> {
        let scale = self.scale_factor();
        let position = position.into().to_logical::<f64>(scale);
        {
            let mut frame = lock(&self.shared.frame);
            frame.x = position.x;
            frame.y = position.y;
        }
        self.apply_frame()
    }

    pub(crate) fn set_size(&self, size: impl Into<Size>) -> Result<(), String> {
        let scale = self.scale_factor();
        let size = size.into().to_logical::<f64>(scale);
        {
            let mut frame = lock(&self.shared.frame);
            frame.width = size.width;
            frame.height = size.height;
        }
        self.apply_frame()
    }

    fn scale_factor(&self) -> f64 {
        self.shared
            .window
            .scale_factor()
            .unwrap_or(1.0)
            .max(f64::EPSILON)
    }

    fn apply_frame(&self) -> Result<(), String> {
        let frame = *lock(&self.shared.frame);
        self.with_native("设置浏览器页面布局", move |_, _, view| {
            set_view_frame(view, frame);
            Ok(())
        })
    }

    pub(crate) fn position(&self) -> Result<PhysicalPosition<i32>, String> {
        let frame = *lock(&self.shared.frame);
        Ok(LogicalPosition::new(frame.x, frame.y).to_physical(self.scale_factor()))
    }

    pub(crate) fn size(&self) -> Result<PhysicalSize<u32>, String> {
        let frame = *lock(&self.shared.frame);
        Ok(
            LogicalSize::new(frame.width.max(0.0), frame.height.max(0.0))
                .to_physical(self.scale_factor()),
        )
    }

    pub(crate) fn url(&self) -> Result<Url, String> {
        let url = self.with_native("读取浏览器页面地址", |browser, _, _| {
            let frame = browser
                .main_frame()
                .ok_or_else(|| "浏览器页面没有主框架".to_owned())?;
            Ok(CefString::from(&frame.url()).to_string())
        })?;
        Url::parse(&url).map_err(|error| format!("浏览器页面地址无效: {error}"))
    }

    /// Evaluates `script` in the main frame and hands `callback` the JSON of its value, as
    /// WebView2's `ExecuteScript` does: `null` for `undefined`, a thrown exception, or a value
    /// that cannot be serialized. Safe to call on the main thread; the callback runs there.
    pub(crate) fn eval_with_callback(
        &self,
        script: impl Into<String>,
        callback: impl FnOnce(String) + Send + 'static,
    ) -> Result<(), String> {
        let params = json!({
            "expression": script.into(),
            "returnByValue": true,
            "awaitPromise": false,
        })
        .to_string();
        self.send_devtools_message(
            "Runtime.evaluate",
            &params,
            Box::new(move |result| {
                let value = result
                    .ok()
                    .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
                    .filter(|response| response.get("exceptionDetails").is_none())
                    .and_then(|response| response.pointer("/result/value").cloned())
                    .unwrap_or(Value::Null);
                callback(value.to_string());
            }),
        );
        Ok(())
    }

    pub(crate) fn open_devtools(&self) {
        let _ = self.with_native("打开浏览器 DevTools", |_, host, _| {
            let info = WindowInfo {
                runtime_style: RuntimeStyle::ALLOY,
                ..Default::default()
            };
            host.show_dev_tools(Some(&info), None, Some(&BrowserSettings::default()), None);
            Ok(())
        });
    }

    pub(crate) fn close_devtools(&self) {
        let _ = self.with_native("关闭浏览器 DevTools", |_, host, _| {
            host.close_dev_tools();
            Ok(())
        });
    }

    /// Closes the page. It stays resolvable by label until CEF reports it closed.
    pub(crate) fn close(&self) -> Result<(), String> {
        self.with_native("关闭浏览器页面", |_, host, _| {
            host.close_browser(1);
            Ok(())
        })?;
        // CEF finishes a close on its own message loop (see `do_close`). A caller on the main
        // thread would wait for it while nothing turns that loop — the application's teardown
        // does exactly this — so it is turned here, briefly, until the page is gone.
        if is_main_thread() {
            let deadline = std::time::Instant::now() + CLOSE_ON_MAIN_TIMEOUT;
            while !self.shared.closed.load(Ordering::SeqCst) && std::time::Instant::now() < deadline
            {
                do_message_loop_work();
                std::thread::sleep(Duration::from_millis(2));
            }
        }
        Ok(())
    }

    /// Moves the page beneath (`parked`) or above every sibling view, without touching its
    /// geometry or visibility. The trusted React WKWebView is a sibling covering the whole
    /// window, so a page at the bottom is entirely covered and takes no pointer input, while
    /// it keeps compositing because its window is still on screen.
    pub(crate) fn set_stacking(&self, parked: bool) -> Result<(), String> {
        self.with_native("调整浏览器页面层级", move |_, _, view| {
            let Some(parent) = (unsafe { view.superview() }) else {
                return Err("浏览器页面不在任何窗口中".into());
            };
            let ordering = if parked {
                NSWindowOrderingMode::Below
            } else {
                NSWindowOrderingMode::Above
            };
            unsafe {
                let _: () = msg_send![
                    &*parent,
                    addSubview: view,
                    positioned: ordering,
                    relativeTo: std::ptr::null::<NSView>()
                ];
            }
            Ok(())
        })
    }

    /// Tells Chromium the page is (not) being shown, which throttles its renderer the way a
    /// background tab is: the closest CEF has to WebView2's sleeping tabs.
    pub(crate) fn set_sleeping(&self, sleeping: bool) -> Result<(), String> {
        self.with_native("切换浏览器页面休眠", move |_, host, _| {
            host.was_hidden(sleeping as i32);
            Ok(())
        })
    }

    /// Points every connection the page makes at `proxy` — a SOCKS endpoint that opens it from
    /// another machine — or back at this computer's own network. Loopback goes through the proxy
    /// too: for a page of a remote workspace, `localhost` is that machine's. Connections already
    /// open are left to finish; new ones follow the setting at once.
    pub(crate) fn set_network_proxy(&self, proxy: Option<String>) -> Result<(), String> {
        self.with_native("设置浏览器页面网络", move |_, host, _| {
            let context = host
                .request_context()
                .ok_or_else(|| "浏览器页面没有请求上下文".to_owned())?;
            let settings = dictionary_value_create()
                .ok_or_else(|| "CEF 未能创建代理设置".to_owned())?;
            let set = |key: &str, value: &str| {
                settings.set_string(Some(&CefString::from(key)), Some(&CefString::from(value)))
            };
            match proxy.as_deref() {
                Some(server) => {
                    set("mode", "fixed_servers");
                    set("server", server);
                    set("bypass_list", "<-loopback>");
                }
                None => {
                    set("mode", "direct");
                }
            }
            let mut value = cef::value_create().ok_or_else(|| "CEF 未能创建代理设置".to_owned())?;
            let mut settings = settings;
            value.set_dictionary(Some(&mut settings));
            let mut error = CefString::default();
            let applied = context.set_preference(
                Some(&CefString::from("proxy")),
                Some(&mut value),
                Some(&mut error),
            );
            if applied == 1 {
                Ok(())
            } else {
                Err(format!("CEF 拒绝了浏览器页面的代理设置：{error}"))
            }
        })
    }

    /// The directory CEF actually keeps this page's profile in.
    pub(crate) fn cache_path(&self) -> Result<PathBuf, String> {
        self.with_native("读取浏览器页面配置目录", |_, host, _| {
            let context = host
                .request_context()
                .ok_or_else(|| "浏览器页面没有请求上下文".to_owned())?;
            let path = CefString::from(&context.cache_path()).to_string();
            if path.is_empty() {
                return Err("浏览器页面使用的是内存配置，没有专属目录".into());
            }
            Ok(PathBuf::from(path))
        })
    }

    /// Serves `folder` at `https://<host>/` for this page's profile only.
    pub(crate) fn map_virtual_host_folder(&self, host: &str, folder: &Path) -> Result<(), String> {
        let host_name = host.to_owned();
        let folder = folder.to_path_buf();
        self.with_native(
            "设置本地文件预览映射",
            move |_, browser_host, _| {
                let context = browser_host
                    .request_context()
                    .ok_or_else(|| "浏览器页面没有请求上下文".to_owned())?;
                let mut factory = FolderSchemeHandlerFactory::new(Arc::new(folder));
                let registered = context.register_scheme_handler_factory(
                    Some(&CefString::from("https")),
                    Some(&CefString::from(host_name.as_str())),
                    Some(&mut factory),
                );
                (registered == 1)
                    .then_some(())
                    .ok_or_else(|| "CEF 拒绝了本地文件预览映射".to_owned())
            },
        )
    }

    pub(crate) fn clear_virtual_host_folder(&self, host: &str) -> Result<(), String> {
        let host_name = host.to_owned();
        self.with_native(
            "清除本地文件预览映射",
            move |_, browser_host, _| {
                let context = browser_host
                    .request_context()
                    .ok_or_else(|| "浏览器页面没有请求上下文".to_owned())?;
                context.register_scheme_handler_factory(
                    Some(&CefString::from("https")),
                    Some(&CefString::from(host_name.as_str())),
                    None,
                );
                Ok(())
            },
        )
    }

    // ----- DevTools protocol ------------------------------------------------------------------

    /// Sends one DevTools method call; `completion` receives the JSON of its `result` or the
    /// text of its error, on the main thread. Any thread.
    pub(crate) fn send_devtools_message(
        &self,
        method: &str,
        params: &str,
        completion: CdpCompletion,
    ) {
        let id = next_devtools_id();
        let method_json = match serde_json::to_string(method) {
            Ok(method) => method,
            Err(error) => return completion(Err(error.to_string())),
        };
        // Some requests and results carry HttpOnly cookie values.
        let message = Zeroizing::new(format!(
            r#"{{"id":{id},"method":{method_json},"params":{params}}}"#
        ));
        lock(&self.shared.pending).insert(id, completion);
        let shared = self.shared.clone();
        let send = move || {
            let submitted = native_browser(&shared.label)
                .and_then(|browser| browser.host())
                .map(|host| host.send_dev_tools_message(Some(message.as_bytes())) == 1)
                .unwrap_or(false);
            if !submitted {
                shared.complete(id, Err("CEF 拒绝了 DevTools 消息".into()));
            }
        };
        if is_main_thread() {
            send();
        } else if let Err(error) = self.shared.app.run_on_main_thread(send) {
            self.shared
                .complete(id, Err(format!("无法调度 DevTools 消息: {error}")));
        }
    }

    /// Registers a listener for every DevTools event the page emits. Listeners run on the main
    /// thread and must not wait for a DevTools result themselves.
    pub(crate) fn set_event_listener(&self, listener: Option<EventListener>) {
        let mut listeners = lock(&self.shared.listeners);
        listeners.clear();
        listeners.extend(listener);
    }

    pub(crate) fn set_dialog_handler(&self, handler: Option<DialogHandlerFn>) {
        *lock(&self.shared.dialog_handler) = handler;
    }

    pub(crate) fn set_crash_handler(&self, handler: Option<CrashHandlerFn>) {
        *lock(&self.shared.crash_handler) = handler;
    }

    /// Answers a dialog [`DialogDecision::Hold`] kept open.
    pub(crate) fn answer_dialog(
        &self,
        id: u64,
        accept: bool,
        text: Option<String>,
    ) -> Result<(), String> {
        let label = self.shared.label.clone();
        on_main(&self.shared.app, "回应页面对话框", move || {
            let callback = NATIVE.with(|native| {
                native
                    .borrow_mut()
                    .get_mut(&label)
                    .and_then(|page| page.held_dialogs.remove(&id))
            });
            let callback = callback.ok_or_else(|| "the dialog is no longer open".to_owned())?;
            let text = text.map(|text| CefString::from(text.as_str()));
            callback.cont(accept as i32, text.as_ref());
            Ok(())
        })?
    }
}

fn host_view(host: &BrowserHost) -> Option<&'static NSView> {
    let view = host.window_handle();
    // SAFETY: a windowed CEF browser's handle is its NSView, retained by its superview for as
    // long as the browser exists; callers only use it on the main thread within this call.
    (!view.is_null()).then(|| unsafe { &*(view as *const NSView) })
}

/// Places `view` at `frame`, which is in the trusted renderer's coordinates: CSS pixels from the
/// top-left of the React WKWebView's viewport, as the pane measures its page box. That viewport
/// is not the webview's frame. Under a standard title bar the content view — and the webview
/// filling it — spans the whole window, and WebKit insets its viewport by the title bar's safe
/// area; the page box is therefore resolved against the webview's safe-area rect, converted to
/// the coordinates of the superview the page shares with it.
fn set_view_frame(view: &NSView, frame: LogicalFrame) {
    let Some(parent) = (unsafe { view.superview() }) else {
        return;
    };
    let viewport = renderer_viewport(&parent).unwrap_or_else(|| parent.bounds());
    let x = viewport.origin.x + frame.x;
    let y = if parent.isFlipped() {
        viewport.origin.y + frame.y
    } else {
        viewport.origin.y + viewport.size.height - frame.y - frame.height
    };
    view.setFrame(NSRect::new(
        NSPoint::new(x, y),
        NSSize::new(frame.width.max(0.0), frame.height.max(0.0)),
    ));
}

/// The trusted renderer's CSS viewport — the safe-area rect of the WKWebView sibling every page
/// shares its superview with — in that superview's coordinates.
fn renderer_viewport(parent: &NSView) -> Option<NSRect> {
    let webview_class = AnyClass::get(c"WKWebView")?;
    let subviews = parent.subviews();
    let webview = subviews
        .iter()
        .find(|subview| subview.isKindOfClass(webview_class))?;
    Some(parent.convertRect_fromView(webview.safeAreaRect(), Some(&webview)))
}

fn remove_view_now(browser: &Browser) {
    if let Some(view) = browser.host().as_ref().and_then(host_view) {
        view.removeFromSuperview();
    }
}

// ----- the builder ------------------------------------------------------------------------------

/// `WebviewBuilder`'s surface as `browser.rs` uses it.
pub(crate) struct CefPageBuilder {
    label: String,
    url: Url,
    data_directory: Option<PathBuf>,
    initialization_scripts: Vec<String>,
    callbacks: PageCallbacks,
    network_proxy: Option<String>,
}

impl CefPageBuilder {
    pub(crate) fn new(label: impl Into<String>, url: WebviewUrl) -> Self {
        let url = match url {
            WebviewUrl::External(url) => url,
            _ => Url::parse(BLANK_URL).expect("about:blank parses"),
        };
        Self {
            label: label.into(),
            url,
            data_directory: None,
            initialization_scripts: Vec::new(),
            callbacks: PageCallbacks::default(),
            network_proxy: None,
        }
    }

    /// The proxy every connection of the page goes through; `None` is the computer's own network.
    /// Applied to the tab's own request context before the page loads anything.
    pub(crate) fn network_proxy(mut self, proxy: Option<String>) -> Self {
        self.network_proxy = proxy;
        self
    }

    pub(crate) fn data_directory(mut self, directory: PathBuf) -> Self {
        self.data_directory = Some(directory);
        self
    }

    pub(crate) fn initialization_script(mut self, script: impl Into<String>) -> Self {
        self.initialization_scripts.push(script.into());
        self
    }

    /// Chromium switches are process-wide under CEF; the page switches WebView2 takes per
    /// environment are applied to the whole engine in `cef_host`.
    pub(crate) fn additional_browser_args(self, _args: &str) -> Self {
        self
    }

    pub(crate) fn zoom_hotkeys_enabled(self, _enabled: bool) -> Self {
        self
    }

    /// DevTools are always available to the host; the page cannot open them itself.
    pub(crate) fn devtools(self, _enabled: bool) -> Self {
        self
    }

    pub(crate) fn general_autofill_enabled(self, _enabled: bool) -> Self {
        self
    }

    pub(crate) fn on_navigation(mut self, handler: impl Fn(&Url) -> bool + Send + 'static) -> Self {
        self.callbacks.navigation = Some(Box::new(handler));
        self
    }

    pub(crate) fn on_new_window(
        mut self,
        handler: impl Fn(Url, NewWindowFeatures) -> NewWindowResponse<Wry> + Send + 'static,
    ) -> Self {
        self.callbacks.new_window = Some(Box::new(handler));
        self
    }

    pub(crate) fn on_download(
        mut self,
        handler: impl Fn(CefWebview, DownloadEvent) -> bool + Send + 'static,
    ) -> Self {
        self.callbacks.download = Some(Box::new(handler));
        self
    }

    pub(crate) fn on_page_load(
        mut self,
        handler: impl Fn(CefWebview, PageLoadPayload) + Send + 'static,
    ) -> Self {
        self.callbacks.page_load = Some(Box::new(handler));
        self
    }

    pub(crate) fn on_document_title_changed(
        mut self,
        handler: impl Fn(CefWebview, String) + Send + 'static,
    ) -> Self {
        self.callbacks.title = Some(Box::new(handler));
        self
    }
}

/// Creates the page as a child of `window` at `position`/`size` and returns once its
/// document-start scripts are registered and its first navigation has been issued — the moment
/// `Window::add_child` returns a WebView2 page. Must not be called on the main thread, which
/// has to keep turning for CEF to answer.
pub(crate) fn add_child(
    window: &Window,
    builder: CefPageBuilder,
    position: impl Into<Position>,
    size: impl Into<Size>,
) -> Result<CefWebview, String> {
    if is_main_thread() {
        return Err("浏览器页面不能在主线程上创建".into());
    }
    let app = window.app_handle().clone();
    super::ensure_started(&app)?;
    let scale = window.scale_factor().unwrap_or(1.0).max(f64::EPSILON);
    let position = position.into().to_logical::<f64>(scale);
    let size = size.into().to_logical::<f64>(scale);
    let CefPageBuilder {
        label,
        url,
        data_directory,
        initialization_scripts,
        callbacks,
        network_proxy,
    } = builder;
    if get(&label).is_some() {
        return Err(format!("浏览器页面 {label} 已存在"));
    }
    let shared = Arc::new(PageShared {
        label: label.clone(),
        app: app.clone(),
        window: window.clone(),
        callbacks: MainOnly(callbacks),
        frame: Mutex::new(LogicalFrame {
            x: position.x,
            y: position.y,
            width: size.width,
            height: size.height,
        }),
        closed: AtomicBool::new(false),
        pending: Mutex::new(HashMap::new()),
        listeners: Mutex::new(Vec::new()),
        dialog_handler: Mutex::new(None),
        crash_handler: Mutex::new(None),
        created: Mutex::new(None),
        abandoned: AtomicBool::new(false),
    });
    let (created_sender, created) = mpsc::sync_channel(1);
    *lock(&shared.created) = Some(created_sender);
    {
        let window = window.clone();
        let shared = shared.clone();
        on_main(&app, "创建浏览器页面", move || {
            create_native(&window, &shared, data_directory.as_deref())
        })??;
    }
    if created.recv_timeout(CREATION_TIMEOUT).is_err() {
        shared.abandoned.store(true, Ordering::SeqCst);
        // It may have been created in the meantime; either way it is not this call's page.
        if let Some(page) = get(&label).filter(|page| Arc::ptr_eq(&page.shared, &shared)) {
            let _ = page.close();
        }
        return Err("CEF 未能在时限内创建浏览器页面".into());
    }
    let page = shared.handle();
    let result = (|| {
        // Before anything loads: a page of a remote workspace must never make its first request
        // from this computer's network.
        if network_proxy.is_some() {
            page.set_network_proxy(network_proxy.clone())?;
        }
        for source in &initialization_scripts {
            page.call_devtools_blocking(
                "Page.addScriptToEvaluateOnNewDocument",
                &json!({ "source": source }).to_string(),
                SCRIPT_REGISTRATION_TIMEOUT,
            )?;
        }
        if url.as_str() != BLANK_URL {
            page.navigate(url)?;
        }
        Ok(())
    })();
    if let Err(error) = result {
        let _ = page.close();
        return Err(error);
    }
    Ok(page)
}

impl CefWebview {
    /// A DevTools call that waits for its result. Off the main thread only.
    pub(crate) fn call_devtools_blocking(
        &self,
        method: &str,
        params: &str,
        timeout: Duration,
    ) -> Result<String, String> {
        let (sender, receiver) = mpsc::sync_channel(1);
        self.send_devtools_message(
            method,
            params,
            Box::new(move |result| {
                let _ = sender.try_send(result);
            }),
        );
        receiver
            .recv_timeout(timeout)
            .map_err(|_| format!("等待 DevTools {method} 超时"))?
    }
}

/// Main-thread half of [`add_child`].
fn create_native(
    window: &Window,
    shared: &Arc<PageShared>,
    data_directory: Option<&Path>,
) -> Result<(), String> {
    let parent = window
        .ns_view()
        .map_err(|error| format!("无法取得宿主窗口视图: {error}"))?;
    if parent.is_null() {
        return Err("宿主窗口没有内容视图".into());
    }
    let mut request_context = match data_directory {
        Some(directory) => {
            let path = directory
                .to_str()
                .ok_or_else(|| "浏览器配置目录不是有效的 Unicode".to_owned())?;
            let settings = RequestContextSettings {
                cache_path: CefString::from(path),
                persist_session_cookies: 0,
                ..Default::default()
            };
            Some(
                request_context_create_context(Some(&settings), None)
                    .ok_or_else(|| "CEF 未能创建标签页专属配置".to_owned())?,
            )
        }
        None => None,
    };
    let frame = *lock(&shared.frame);
    let info = WindowInfo {
        runtime_style: RuntimeStyle::ALLOY,
        ..Default::default()
    }
    .set_as_child(
        parent as _,
        &Rect {
            x: frame.x.round() as i32,
            y: frame.y.round() as i32,
            width: frame.width.round().max(1.0) as i32,
            height: frame.height.round().max(1.0) as i32,
        },
    );
    let mut client = PageClient::new(
        PageLifeSpan::new(shared.clone()),
        PageLoad::new(shared.clone()),
        PageDisplay::new(shared.clone()),
        PageRequests::new(shared.clone()),
        PageDialogs::new(shared.clone()),
        PageDownloads::new(shared.clone()),
        PageContextMenu::new(),
    );
    // Asynchronous: the tab's profile has to finish initializing first, which the synchronous
    // variant refuses to wait for. `on_after_created` completes the page.
    let started = browser_host_create_browser(
        Some(&info),
        Some(&mut client),
        Some(&CefString::from(BLANK_URL)),
        Some(&BrowserSettings::default()),
        None,
        request_context.as_mut(),
    );
    (started == 1)
        .then_some(())
        .ok_or_else(|| "CEF 未能开始创建浏览器页面".to_owned())
}

/// Main-thread completion of a page CEF has just created: place its view, attach the DevTools
/// observer and publish it by label.
fn register_created(shared: &Arc<PageShared>, browser: &Browser) {
    let Some(host) = browser.host() else {
        return;
    };
    if shared.abandoned.load(Ordering::SeqCst) {
        host.close_browser(1);
        remove_view_now(browser);
        return;
    }
    let frame = *lock(&shared.frame);
    if let Some(view) = host_view(&host) {
        // Keep the distance to the window's top edge as the window resizes; the host also
        // re-lays the page out on every resize.
        view.setAutoresizingMask(NSAutoresizingMaskOptions::ViewMinYMargin);
        set_view_frame(view, frame);
    }
    let mut observer = PageDevTools::new(shared.clone());
    let registration = host.add_dev_tools_message_observer(Some(&mut observer));
    NATIVE.with(|native| {
        native.borrow_mut().insert(
            shared.label.clone(),
            NativePage {
                browser: browser.clone(),
                _registration: registration,
                held_dialogs: HashMap::new(),
            },
        )
    });
    pages()
        .get_or_insert_with(HashMap::new)
        .insert(shared.label.clone(), shared.clone());
    if let Some(created) = lock(&shared.created).take() {
        let _ = created.try_send(());
    }
}

// ----- CEF handlers -----------------------------------------------------------------------------

wrap_client! {
    struct PageClient {
        life_span: LifeSpanHandler,
        load: LoadHandler,
        display: DisplayHandler,
        requests: RequestHandler,
        dialogs: JsdialogHandler,
        downloads: DownloadHandler,
        context_menu: ContextMenuHandler,
    }

    impl Client {
        fn life_span_handler(&self) -> Option<LifeSpanHandler> {
            Some(self.life_span.clone())
        }
        fn load_handler(&self) -> Option<LoadHandler> {
            Some(self.load.clone())
        }
        fn display_handler(&self) -> Option<DisplayHandler> {
            Some(self.display.clone())
        }
        fn request_handler(&self) -> Option<RequestHandler> {
            Some(self.requests.clone())
        }
        fn jsdialog_handler(&self) -> Option<JsdialogHandler> {
            Some(self.dialogs.clone())
        }
        fn download_handler(&self) -> Option<DownloadHandler> {
            Some(self.downloads.clone())
        }
        fn context_menu_handler(&self) -> Option<ContextMenuHandler> {
            Some(self.context_menu.clone())
        }
    }
}

wrap_life_span_handler! {
    struct PageLifeSpan {
        shared: Arc<PageShared>,
    }

    impl LifeSpanHandler {
        fn on_before_popup(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            _popup_id: ::std::os::raw::c_int,
            target_url: Option<&CefString>,
            _target_frame_name: Option<&CefString>,
            _target_disposition: WindowOpenDisposition,
            _user_gesture: ::std::os::raw::c_int,
            _popup_features: Option<&PopupFeatures>,
            _window_info: Option<&mut WindowInfo>,
            _client: Option<&mut Option<Client>>,
            _settings: Option<&mut BrowserSettings>,
            _extra_info: Option<&mut Option<DictionaryValue>>,
            _no_javascript_access: Option<&mut ::std::os::raw::c_int>,
        ) -> ::std::os::raw::c_int {
            if let Some(url) = target_url {
                self.shared.report_new_window(&url.to_string());
            }
            1
        }

        fn on_after_created(&self, browser: Option<&mut Browser>) {
            if let Some(browser) = browser {
                register_created(&self.shared, browser);
            }
        }

        /// Takes the close over so it stays this page's. Left to CEF, closing a child browser
        /// sends `performClose:` to its top-level window — Mework's main window. Instead the
        /// page's own view is released: CEF learns a windowed browser is gone when its view is
        /// deallocated, and answers with `on_before_close`. The release runs as a CEF task, on
        /// CEF's own loop, because it re-enters CEF — and it must not retain the view meanwhile,
        /// or the deallocation it exists to cause would wait for whoever held it.
        fn do_close(&self, browser: Option<&mut Browser>) -> ::std::os::raw::c_int {
            if let Some(browser) = browser {
                let mut task = ReleasePageView::new(self.shared.clone(), browser.clone());
                post_task(ThreadId::UI, Some(&mut task));
            }
            1
        }

        fn on_before_close(&self, _browser: Option<&mut Browser>) {
            let shared = &self.shared;
            shared.closed.store(true, Ordering::SeqCst);
            {
                let mut pages = pages();
                if let Some(pages) = pages.as_mut() {
                    if pages
                        .get(&shared.label)
                        .is_some_and(|current| Arc::ptr_eq(current, shared))
                    {
                        pages.remove(&shared.label);
                    }
                }
            }
            let native = NATIVE.with(|native| native.borrow_mut().remove(&shared.label));
            drop(native);
            shared.fail_all_pending("浏览器页面已关闭");
            lock(&shared.listeners).clear();
            *lock(&shared.dialog_handler) = None;
            *lock(&shared.crash_handler) = None;
        }
    }
}

wrap_task! {
    struct ReleasePageView {
        shared: Arc<PageShared>,
        browser: Browser,
    }

    impl Task {
        fn execute(&self) {
            // Once CEF reported the page closed its view may already be gone.
            if !self.shared.closed.load(Ordering::SeqCst) {
                remove_view_now(&self.browser);
            }
        }
    }
}

wrap_load_handler! {
    struct PageLoad {
        shared: Arc<PageShared>,
    }

    impl LoadHandler {
        fn on_load_start(
            &self,
            _browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            _transition_type: TransitionType,
        ) {
            if let Some(frame) = frame.filter(|frame| frame.is_main() == 1) {
                let url = CefString::from(&frame.url()).to_string();
                self.shared.page_load(&url, PageLoadEvent::Started);
            }
        }

        fn on_load_end(
            &self,
            _browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            _http_status_code: ::std::os::raw::c_int,
        ) {
            if let Some(frame) = frame.filter(|frame| frame.is_main() == 1) {
                let url = CefString::from(&frame.url()).to_string();
                self.shared.page_load(&url, PageLoadEvent::Finished);
            }
        }

        /// A failed main-frame load finishes like WebView2's does, on the address that failed.
        /// A load the navigation policy cancelled is not a load at all: the policy already
        /// rolled the page's state back.
        fn on_load_error(
            &self,
            _browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            error_code: Errorcode,
            _error_text: Option<&CefString>,
            failed_url: Option<&CefString>,
        ) {
            if error_code == Errorcode::ABORTED {
                return;
            }
            if frame.is_some_and(|frame| frame.is_main() == 1) {
                if let Some(url) = failed_url {
                    self.shared.page_load(&url.to_string(), PageLoadEvent::Finished);
                }
            }
        }
    }
}

wrap_display_handler! {
    struct PageDisplay {
        shared: Arc<PageShared>,
    }

    impl DisplayHandler {
        fn on_title_change(&self, _browser: Option<&mut Browser>, title: Option<&CefString>) {
            let title = title.map(ToString::to_string).unwrap_or_default();
            let shared = &self.shared;
            if let Some(handler) = shared.callbacks.get().and_then(|callbacks| callbacks.title.as_ref()) {
                handler(shared.handle(), title);
            }
        }

        /// The page's console reaches the host through its initialization script; Chromium's
        /// own copy would only fill the engine log.
        fn on_console_message(
            &self,
            _browser: Option<&mut Browser>,
            _level: LogSeverity,
            _message: Option<&CefString>,
            _source: Option<&CefString>,
            _line: ::std::os::raw::c_int,
        ) -> ::std::os::raw::c_int {
            1
        }
    }
}

wrap_request_handler! {
    struct PageRequests {
        shared: Arc<PageShared>,
    }

    impl RequestHandler {
        /// The synchronous navigation veto, for the main frame and its redirects — where
        /// WebView2's `NavigationStarting` fires.
        fn on_before_browse(
            &self,
            _browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            request: Option<&mut Request>,
            _user_gesture: ::std::os::raw::c_int,
            _is_redirect: ::std::os::raw::c_int,
        ) -> ::std::os::raw::c_int {
            if !frame.is_some_and(|frame| frame.is_main() == 1) {
                return 0;
            }
            let Some(request) = request else {
                return 1;
            };
            let url = CefString::from(&request.url()).to_string();
            (!self.shared.allows_navigation(&url)) as ::std::os::raw::c_int
        }

        /// Cmd-click and middle-click: a request for a new tab, handled as `window.open` is.
        fn on_open_urlfrom_tab(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            target_url: Option<&CefString>,
            _target_disposition: WindowOpenDisposition,
            _user_gesture: ::std::os::raw::c_int,
        ) -> ::std::os::raw::c_int {
            if let Some(url) = target_url {
                self.shared.report_new_window(&url.to_string());
            }
            1
        }

        fn on_render_process_terminated(
            &self,
            _browser: Option<&mut Browser>,
            status: TerminationStatus,
            _error_code: ::std::os::raw::c_int,
            _error_string: Option<&CefString>,
        ) {
            // Every termination status CEF reports here leaves the page without a renderer.
            let _ = status;
            let handler = lock(&self.shared.crash_handler).clone();
            if let Some(handler) = handler {
                handler("the renderer process exited");
            }
        }
    }
}

wrap_jsdialog_handler! {
    struct PageDialogs {
        shared: Arc<PageShared>,
    }

    impl JsdialogHandler {
        fn on_jsdialog(
            &self,
            _browser: Option<&mut Browser>,
            origin_url: Option<&CefString>,
            dialog_type: JsdialogType,
            message_text: Option<&CefString>,
            default_prompt_text: Option<&CefString>,
            callback: Option<&mut JsdialogCallback>,
            _suppress_message: Option<&mut ::std::os::raw::c_int>,
        ) -> ::std::os::raw::c_int {
            let Some(callback) = callback else {
                return 0;
            };
            let kind = if dialog_type == JsdialogType::ALERT {
                "alert"
            } else if dialog_type == JsdialogType::CONFIRM {
                "confirm"
            } else {
                "prompt"
            };
            self.shared.open_dialog(
                PageDialog {
                    kind,
                    message: message_text.map(ToString::to_string).unwrap_or_default(),
                    default_text: default_prompt_text.map(ToString::to_string).unwrap_or_default(),
                    url: origin_url.map(ToString::to_string).unwrap_or_default(),
                },
                callback.clone(),
            );
            1
        }

        fn on_before_unload_dialog(
            &self,
            browser: Option<&mut Browser>,
            message_text: Option<&CefString>,
            _is_reload: ::std::os::raw::c_int,
            callback: Option<&mut JsdialogCallback>,
        ) -> ::std::os::raw::c_int {
            let Some(callback) = callback else {
                return 0;
            };
            let url = browser
                .and_then(|browser| browser.main_frame())
                .map(|frame| CefString::from(&frame.url()).to_string())
                .unwrap_or_default();
            self.shared.open_dialog(
                PageDialog {
                    kind: "beforeunload",
                    message: message_text.map(ToString::to_string).unwrap_or_default(),
                    default_text: String::new(),
                    url,
                },
                callback.clone(),
            );
            1
        }

        /// CEF drops every open dialog when the page navigates away or is torn down.
        fn on_reset_dialog_state(&self, _browser: Option<&mut Browser>) {
            NATIVE.with(|native| {
                if let Some(page) = native.borrow_mut().get_mut(&self.shared.label) {
                    page.held_dialogs.clear();
                }
            });
        }
    }
}

wrap_download_handler! {
    struct PageDownloads {
        shared: Arc<PageShared>,
    }

    impl DownloadHandler {
        fn can_download(
            &self,
            _browser: Option<&mut Browser>,
            url: Option<&CefString>,
            _request_method: Option<&CefString>,
        ) -> ::std::os::raw::c_int {
            let shared = &self.shared;
            let allowed = shared
                .callbacks
                .get()
                .and_then(|callbacks| callbacks.download.as_ref())
                .is_some_and(|handler| {
                    handler(
                        shared.handle(),
                        DownloadEvent {
                            url: url.map(ToString::to_string).unwrap_or_default(),
                        },
                    )
                });
            allowed as ::std::os::raw::c_int
        }

        fn on_before_download(
            &self,
            _browser: Option<&mut Browser>,
            _download_item: Option<&mut DownloadItem>,
            _suggested_name: Option<&CefString>,
            _callback: Option<&mut BeforeDownloadCallback>,
        ) -> ::std::os::raw::c_int {
            0
        }
    }
}

wrap_context_menu_handler! {
    struct PageContextMenu;

    impl ContextMenuHandler {
        /// The page's own context menu keeps editing and navigation; "View Source" and "Print"
        /// would open windows outside the pane.
        fn on_before_context_menu(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            _params: Option<&mut ContextMenuParams>,
            model: Option<&mut MenuModel>,
        ) {
            if let Some(model) = model {
                model.remove(MenuId::VIEW_SOURCE.get_raw() as i32);
                model.remove(MenuId::PRINT.get_raw() as i32);
            }
        }
    }
}

wrap_dev_tools_message_observer! {
    struct PageDevTools {
        shared: Arc<PageShared>,
    }

    impl DevToolsMessageObserver {
        fn on_dev_tools_method_result(
            &self,
            _browser: Option<&mut Browser>,
            message_id: ::std::os::raw::c_int,
            success: ::std::os::raw::c_int,
            result: Option<&[u8]>,
        ) {
            let text = Zeroizing::new(
                result
                    .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
                    .unwrap_or_default(),
            );
            let result = if success == 1 {
                Ok(text.to_string())
            } else {
                Err(serde_json::from_str::<Value>(&text)
                    .ok()
                    .and_then(|error| error.get("message").and_then(Value::as_str).map(str::to_owned))
                    .unwrap_or_else(|| "DevTools call failed".to_owned()))
            };
            self.shared.complete(message_id, result);
        }

        fn on_dev_tools_event(
            &self,
            _browser: Option<&mut Browser>,
            method: Option<&CefString>,
            params: Option<&[u8]>,
        ) {
            let Some(method) = method.map(ToString::to_string) else {
                return;
            };
            let params = params
                .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
                .unwrap_or_else(|| "{}".to_owned());
            self.shared.dispatch_event(&method, &params);
        }
    }
}

// ----- local file preview -----------------------------------------------------------------------

wrap_scheme_handler_factory! {
    struct FolderSchemeHandlerFactory {
        folder: Arc<PathBuf>,
    }

    impl SchemeHandlerFactory {
        /// Serves a file below the mapped folder, or nothing. The path is resolved component
        /// by component and re-checked after canonicalisation, so neither `..` nor a symlink can
        /// reach outside the folder.
        fn create(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            _scheme_name: Option<&CefString>,
            request: Option<&mut Request>,
        ) -> Option<ResourceHandler> {
            let request = request?;
            let url = Url::parse(&CefString::from(&request.url()).to_string()).ok()?;
            let file = resolve_preview_file(&self.folder, url.path())?;
            let stream = stream_reader_create_for_file(Some(&CefString::from(file.to_str()?)))?;
            Some(wrapper::stream_resource_handler::StreamResourceHandler::new_with_stream(
                preview_mime_type(&file).to_owned(),
                stream,
            ))
        }
    }
}

fn resolve_preview_file(folder: &Path, url_path: &str) -> Option<PathBuf> {
    let mut path = folder.to_path_buf();
    for segment in url_path.split('/').filter(|segment| !segment.is_empty()) {
        let segment = percent_decode(segment)?;
        if segment == "." || segment == ".." || segment.contains('/') || segment.contains('\0') {
            return None;
        }
        path.push(segment);
    }
    let root = std::fs::canonicalize(folder).ok()?;
    let resolved = std::fs::canonicalize(&path).ok()?;
    (resolved.starts_with(&root) && resolved.is_file()).then_some(resolved)
}

fn percent_decode(segment: &str) -> Option<String> {
    let bytes = segment.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = std::str::from_utf8(bytes.get(index + 1..index + 3)?).ok()?;
            decoded.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

fn preview_mime_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("html" | "htm") => "text/html",
        Some("css") => "text/css",
        Some("js" | "mjs") => "text/javascript",
        Some("json") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("ico") => "image/x-icon",
        Some("pdf") => "application/pdf",
        Some("txt" | "md" | "log") => "text/plain",
        Some("xml") => "application/xml",
        Some("wasm") => "application/wasm",
        Some("mp4") => "video/mp4",
        Some("webm") => "video/webm",
        Some("mp3") => "audio/mpeg",
        Some("wav") => "audio/wav",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        Some("ttf") => "font/ttf",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_files_never_leave_the_mapped_folder() {
        let temporary = tempfile::tempdir().unwrap();
        let folder = temporary.path().join("site");
        std::fs::create_dir_all(folder.join("assets")).unwrap();
        std::fs::write(folder.join("index.html"), "<p>hi</p>").unwrap();
        std::fs::write(folder.join("assets/app.js"), "1").unwrap();
        std::fs::write(temporary.path().join("secret.txt"), "no").unwrap();

        assert!(resolve_preview_file(&folder, "/index.html").is_some());
        assert!(resolve_preview_file(&folder, "/assets/app.js").is_some());
        assert!(resolve_preview_file(&folder, "/assets%2Fapp.js").is_none());
        assert!(resolve_preview_file(&folder, "/../secret.txt").is_none());
        assert!(resolve_preview_file(&folder, "/%2e%2e/secret.txt").is_none());
        assert!(resolve_preview_file(&folder, "/assets").is_none());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(temporary.path().join("secret.txt"), folder.join("link"))
                .unwrap();
            assert!(resolve_preview_file(&folder, "/link").is_none());
        }
    }

    #[test]
    fn preview_mime_types_follow_the_extension() {
        assert_eq!(preview_mime_type(Path::new("a/Index.HTML")), "text/html");
        assert_eq!(
            preview_mime_type(Path::new("a.bin")),
            "application/octet-stream"
        );
    }
}
