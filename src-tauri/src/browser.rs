//! Native, conversation-scoped browser pages used by both Tauri commands and agent tools.
//!
//! Security invariant: `BROWSER_PAGE_LABEL` is the untrusted, remote webview. Never add that
//! label to an application capability. Browser chrome lives in the trusted main React WebView;
//! the desktop build only adds the remote page as a permissionless child WebView.
// The DevTools-protocol half of this module (network log, cookies, screenshots,
// dialogs, element picking) drives WebView2 and exists only on Windows.
#![cfg_attr(not(windows), allow(dead_code))]

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc, Arc, Mutex, MutexGuard, OnceLock, TryLockError,
    },
    time::{Duration, Instant},
};

use base64::Engine as _;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use tauri::{
    webview::{NewWindowResponse, PageLoadEvent, WebviewBuilder},
    window::WindowBuilder,
    AppHandle, LogicalPosition, LogicalSize, Manager, PhysicalPosition, PhysicalSize, Position,
    Webview, WebviewUrl, Window, WindowEvent,
};
use url::{Host, Url};
use zeroize::{Zeroize, Zeroizing};

use crate::host_platform::host_platform;
use crate::{
    browser_file_preview, browser_profile_data, browser_webview_lifecycle,
    browser_window_region::set_page_stacking,
    chromium_capability::{
        CapabilityError, WebView2Control, WebView2ControllerIssuer, WebView2Permit,
        WebView2Profile, WebView2RuntimeLease, WebView2TeardownPermit,
    },
    model::SecurityLevel,
};

#[cfg(all(windows, feature = "browser-dev"))]
use crate::chromium_capability::WebView2ReleaseObserverPermit;

pub const BROWSER_WINDOW_LABEL: &str = "browser";
pub const BROWSER_PAGE_LABEL: &str = "browser-page";
/// Root under which every ordinary tab gets its own single-use WebView2 user-data folder.
///
/// Mework never reads Chrome's or Edge's profile, and the built-in browser keeps no persistent
/// profile at all: a tab's profile is created empty when the tab is created, holds only what the
/// user (or the Agent) did inside that one tab, survives cold suspend/resume, and is deleted when
/// the tab closes or the app exits. Two tabs never share a cookie store — the boundary is
/// Chromium's (two user-data folders are two cookie stores in two browser processes), not a
/// filter Mework applies. A startup sweep removes directories a crash left behind; the profile
/// name mixes a per-session random nonce, so a leftover directory can never be re-adopted.
const TAB_BROWSER_PROFILE_ROOT: &str = "browser-tab-profiles";
// WebView2 removes the controller label before its browser process has necessarily released every
// file in the user-data directory. Keep the retry window longer than that native tail; cleanup is
// still bounded and startup cleanup applies its independent, shorter global budget below.
const RESEARCH_PROFILE_DELETE_ATTEMPTS: usize = 101;
const RESEARCH_PROFILE_DELETE_DELAY: Duration = Duration::from_millis(50);
const RESEARCH_PROFILE_STARTUP_MAX_DIRECTORIES: usize = 16;
const RESEARCH_PROFILE_STARTUP_BUDGET: Duration = Duration::from_secs(2);
pub const BROWSER_TOOLBAR_HEIGHT: f64 = 80.0;
pub const BROWSER_PANEL_WIDTH: f64 = 560.0;
const MAX_BROWSER_PANEL_VALUE: f64 = 100_000.0;
const MAX_AWAKE_BROWSER_PAGES: usize = 3;
/// Sleeping WebViews preserve exact page state, but each isolated task Profile can still retain
/// native controller/process resources. Older sleeping tasks are cold-closed beyond this bound.
const MAX_RETAINED_BROWSER_PAGES: usize = 8;
/// Tab id of a conversation's own page. Extra tabs are addressed by the `#<token>` suffix that
/// already distinguishes their session, and renderer-minted tokens are `tab_<uuid>`, so this
/// sentinel cannot collide with a real token.
const PRIMARY_TAB_ID: &str = "main";

const DEFAULT_URL: &str = "about:blank";
/// Chromium switches for every page's own browser process. Each tab has its own user-data folder
/// and therefore its own WebView2 environment, so these never reach the trusted main window.
/// `CalculateNativeWinOcclusion` is what marks a parked (off-screen) page as hidden; disabling
/// it keeps its compositor producing frames, which is what input acknowledgements wait on.
const BROWSER_PAGE_BROWSER_ARGS: &str = "--disable-background-timer-throttling --disable-renderer-backgrounding --disable-backgrounding-occluded-windows --disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection,CalculateNativeWinOcclusion";
const MAIN_WINDOW_LABEL: &str = "main";
const DEFAULT_WIDTH: f64 = 1200.0;
const DEFAULT_HEIGHT: f64 = 800.0;
/// A child WebView at this negative client coordinate has no intersection with a visible parent,
/// independent of later host growth. Tauri does not expose Wry's creation-time visibility flag.
const PRE_ATTESTATION_CHILD_OFFSET: f64 = -32_768.0;
const EVAL_TIMEOUT: Duration = Duration::from_secs(15);
const SCREENSHOT_TIMEOUT: Duration = Duration::from_secs(45);
const BROWSING_DATA_CLEAR_TIMEOUT: Duration = Duration::from_secs(45);
const BROWSER_SLEEP_TRANSITION_TIMEOUT: Duration = Duration::from_secs(10);
const BROWSER_DESTROY_TIMEOUT: Duration = Duration::from_secs(2);
const BROWSER_DESTROY_POLL: Duration = Duration::from_millis(10);
/// A cold-close snapshot is deliberately bounded even though Chromium also enforces per-cookie
/// limits. Values stay in process memory only and are never persisted by Mework.
const MAX_COLD_CLOSE_COOKIE_COUNT: usize = 4_096;
const MAX_COLD_CLOSE_COOKIE_BYTES: usize = 8 * 1024 * 1024;

const MAX_SELECTOR_CHARS: usize = 2_048;
const MAX_TEXT_INPUT_CHARS: usize = 1_000_000;
const MAX_EVALUATE_CHARS: usize = 1_000_000;
const MAX_URL_CHARS: usize = 8_192;
const MAX_SCREENSHOT_BYTES: usize = 64 * 1024 * 1024;
/// A capture returned inline crosses the IPC boundary base64-encoded, which inflates it by 4/3, so
/// it gets a quarter of the budget a capture written to disk does. A viewport-sized PNG lands far
/// below this even on an 8K panel; `full_page` captures never take this path.
const MAX_INLINE_CAPTURE_BYTES: usize = MAX_SCREENSHOT_BYTES / 4;
const MAX_EVAL_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const MAX_UI_PREFERENCE_GENERATION: u64 = 9_007_199_254_740_991;
const MAX_BROWSER_LIFECYCLE_EPOCH: u64 = 9_007_199_254_740_991;
const STALE_BROWSER_LIFECYCLE_INTENT_ERROR: &str =
    "浏览器生命周期请求已过期；已拒绝影响较新的页面状态";
const COLLIDING_BROWSER_LIFECYCLE_INTENT_ERROR: &str =
    "浏览器生命周期 epoch 已用于相反意图；已拒绝冲突请求";
const FUTURE_BROWSER_PANEL_BOUNDS_ERROR: &str = "浏览器布局 epoch 尚未被对应的生命周期意图接受";
const MISMATCHED_BROWSER_PANEL_VISIBILITY_ERROR: &str = "浏览器布局可见性与当前生命周期意图不一致";
const CLOSE_INVALID_REQUEST_MESSAGE: &str = "浏览器关闭请求无效，未更改当前页面状态";
const CLOSE_STALE_INTENT_MESSAGE: &str = "浏览器关闭请求已过期，未更改较新的页面状态";
const CLOSE_INTENT_COLLISION_MESSAGE: &str = "浏览器关闭请求与当前生命周期状态冲突，未更改页面状态";
const CLOSE_LIFECYCLE_UNAVAILABLE_MESSAGE: &str = "浏览器生命周期当前不可用于关闭，未更改页面状态";
const CLOSE_LIFECYCLE_SUPERSEDED_MESSAGE: &str =
    "浏览器关闭已被较新的生命周期请求取代，未操作较新的页面";
const CLOSE_NATIVE_CLEANUP_MESSAGE: &str =
    "浏览器已标记关闭并隐藏，但原生资源尚未完全释放；请稍后重试清理";
const CLOSE_NATIVE_CLEANUP_HIDE_FAILED_MESSAGE: &str =
    "浏览器已标记关闭，但原生资源未完全释放且页面未能确认隐藏；请立即重试";
const CLOSE_INTERNAL_FAILURE_MESSAGE: &str =
    "浏览器关闭结果无法确认；已保守保留关闭状态，请重试清理";
/// Identity of the development server that serves the trusted frontend,
/// installed once at startup from the runtime configuration.
///
/// It is deliberately not a constant. The development server asks the operating
/// system for a port and only prefers the framework default, so the value is
/// knowable at runtime and nowhere else. A release build serves the frontend
/// from the `tauri://` custom protocol and installs nothing, which is what keeps
/// loopback HTTP an ordinary user address in the shipped binary.
static APP_DEV_SERVER: OnceLock<AppDevServer> = OnceLock::new();

struct AppDevServer {
    /// The exact origin the frontend is served from, e.g. `http://127.0.0.1:1420`.
    origin: String,
    /// Its port. The page WebView's reservation is deliberately broader than the
    /// origin: the server answers on every loopback spelling of this port, and a
    /// reservation that refuses more than it must costs nothing.
    port: u16,
}
/// After the document is loaded enough to act on, a bounded best-effort wait for `load` so the
/// first snapshot sees late-arriving resources. Reaching the bound is not an error.
const NAVIGATION_LOAD_GRACE: Duration = Duration::from_secs(5);
/// The action-completion policy of `@playwright/mcp` (`waitForCompletion`): after an interaction,
/// let the page settle for `POST_ACTION_SETTLE`; if the interaction started a main-frame
/// navigation, wait up to `POST_ACTION_NAVIGATION_LOAD` for that document's `load`; otherwise wait
/// up to `POST_ACTION_NETWORK_QUIET` for the requests the interaction started to finish, then
/// settle once more. Reaching either bound is not an error.
const POST_ACTION_SETTLE: Duration = Duration::from_millis(500);
const POST_ACTION_NAVIGATION_LOAD: Duration = Duration::from_secs(10);
const POST_ACTION_NETWORK_QUIET: Duration = Duration::from_secs(5);
const POST_ACTION_POLL: Duration = Duration::from_millis(25);
/// Requests whose response body must have finished before an interaction counts as settled.
/// Other resource kinds only need their response headers, exactly as `@playwright/mcp` decides.
const SETTLE_BODY_RESOURCE_TYPES: [&str; 5] = ["document", "stylesheet", "script", "xhr", "fetch"];
/// Snapshot size included with an interaction's own result. The explicit `snapshot` action still
/// returns up to `max_chars`; this smaller inline copy exists so an interaction that changed the
/// page does not cost a second round trip before the model can see the change.
const ACTION_SNAPSHOT_CHARS: usize = 8_000;
/// Console entries recorded during one action that are echoed back with its result.
const MAX_ACTION_CONSOLE_ENTRIES: usize = 10;
const MAX_ACTION_CONSOLE_CHARS: usize = 400;
/// Observed-request bookkeeping is bounded so a chatty page cannot grow the map without limit.
const MAX_OBSERVED_REQUESTS: usize = 2_000;
const MAX_DIALOG_RECORDS: usize = 50;
/// Error a page-side wait ends with when a dialog or file chooser opened meanwhile. The
/// dispatcher turns it into a successful result that carries the modal state, because the action
/// did happen; it is the page that cannot continue until the state is cleared.
const MODAL_STATE_INTERRUPTED: &str = "the page opened a modal state before the action completed";
/// Prefix of the default text an in-page alert()/confirm() shim passes to the native prompt it is
/// routed through, followed by the real dialog kind. Must match the initialization script.
const DIALOG_KIND_MARK: &str = "⁣mework-dialog:";
const AGENT_POINTER_CDP_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_UPLOAD_FILES: usize = 10;
/// `preview_inspect.styles` and `preview_dialog.prompt_text` bounds, matching the declared schema.
const MAX_PREVIEW_STYLES: usize = 64;
const MAX_DIALOG_PROMPT_CHARS: usize = 4_096;
const MAX_LOG_LIMIT: u64 = 200;

/// One request the DevTools `Network` domain reported for the current page generation.
#[derive(Debug, Clone)]
struct ObservedRequest {
    /// Position in the page's request stream; an action's watermark selects the requests it
    /// started.
    sequence: u64,
    /// Lower-cased DevTools resource type (`document`, `xhr`, `image`, ...).
    resource_type: String,
    /// A main-frame document request, i.e. the interaction navigated the page.
    main_frame_navigation: bool,
    finished: bool,
}

/// One row of the `preview_network` listing. This is a second, deliberately separate record of the
/// same `Network` events `ObservedRequest` watches: that one answers "did my click start anything",
/// this one carries what the model reads (method, url, status) and the DevTools request id that
/// `Network.getResponseBody` accepts. The in-page `networkEntries` buffer can serve neither — it
/// only sees `fetch`/XHR and mints ids of its own.
#[derive(Debug, Clone)]
struct NetworkLogEntry {
    request_id: String,
    url: String,
    method: String,
    status: Option<i64>,
    status_text: String,
    failed: bool,
    error_text: String,
}

/// A JavaScript dialog the page opened and that is being held open until `preview_dialog`
/// answers it. Held dialogs block the page's JavaScript exactly like a real browser dialog would,
/// which is why every other action is refused while one is pending.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PendingDialog {
    id: u64,
    /// `alert`, `confirm`, `prompt` or `beforeunload`.
    kind: String,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    default_value: Option<String>,
    url: String,
    opened_at_ms: i64,
}

/// A file chooser the page opened (through a click on an `input[type=file]` or a scripted
/// `showPicker`) and that is being held until `preview_upload_image` sets its
/// files. The chooser never reaches the operating system.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PendingFileChooser {
    /// `selectSingle` or `selectMultiple`.
    mode: String,
    #[serde(skip)]
    backend_node_id: Option<u64>,
    opened_at_ms: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DialogRecord {
    timestamp: i64,
    kind: String,
    message: String,
    /// `true` when accepted, `false` when dismissed, absent while still open.
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<bool>,
}

/// Everything the host learned about the page from outside its JavaScript: the DevTools
/// `Network`/`Page` event streams and the native dialog, file-chooser and process-failure
/// callbacks. It is reset with every new native page generation.
#[derive(Debug, Default)]
struct PageActivity {
    /// Native page generation these observations belong to; stale callbacks are dropped.
    generation: u64,
    /// DevTools `Network`/`Page` events are flowing for this generation. Without them the page
    /// `loading` flag is the only load signal and request bookkeeping stays empty.
    events_enabled: bool,
    request_sequence: u64,
    requests: HashMap<String, ObservedRequest>,
    /// DevTools id of the main frame, learned from the frame tree and every main-frame navigation.
    main_frame_id: Option<String>,
    /// Main-frame navigations committed (`Page.frameNavigated` without a parent).
    main_frame_navigations: u64,
    /// Count of the main frame's `load` events, so a wait can tell a new document's from the
    /// previous document's.
    load_events: u64,
    pending_dialog: Option<PendingDialog>,
    dialog_records: Vec<DialogRecord>,
    next_dialog_id: u64,
    pending_file_chooser: Option<PendingFileChooser>,
    /// FIFO `preview_network` ledger, capped like Claude Code's at
    /// [`PREVIEW_MAX_NETWORK_ENTRIES`].
    network_log: Vec<NetworkLogEntry>,
    /// Counter behind the `uid`s in a `preview_snapshot` listing. It runs for the life of the page
    /// generation so a uid the model read earlier is never silently reused by a later snapshot.
    snapshot_uid: u64,
    /// Set when WebView2 reported that the page's browser or renderer process died. The next
    /// action resets the page to a blank document and says so, instead of driving a dead page.
    crash: Option<String>,
}

impl PageActivity {
    fn reset_for_generation(&mut self, generation: u64) {
        *self = PageActivity {
            generation,
            dialog_records: std::mem::take(&mut self.dialog_records),
            next_dialog_id: self.next_dialog_id,
            ..PageActivity::default()
        };
    }

    fn record_request(
        &mut self,
        request_id: String,
        resource_type: String,
        main_frame_navigation: bool,
    ) {
        self.request_sequence = self.request_sequence.wrapping_add(1);
        if self.requests.len() >= MAX_OBSERVED_REQUESTS {
            let mut stale: Vec<(String, u64)> = self
                .requests
                .iter()
                .filter(|(_, request)| request.finished)
                .map(|(id, request)| (id.clone(), request.sequence))
                .collect();
            stale.sort_by_key(|(_, sequence)| *sequence);
            for (id, _) in stale.into_iter().take(MAX_OBSERVED_REQUESTS / 2) {
                self.requests.remove(&id);
            }
            if self.requests.len() >= MAX_OBSERVED_REQUESTS {
                self.requests.clear();
            }
        }
        self.requests.insert(
            request_id,
            ObservedRequest {
                sequence: self.request_sequence,
                resource_type,
                main_frame_navigation,
                finished: false,
            },
        );
    }

    fn finish_request(&mut self, request_id: &str) {
        if let Some(request) = self.requests.get_mut(request_id) {
            request.finished = true;
        }
    }

    /// A redirect re-sends the same DevTools request id, so the row is replaced rather than
    /// appended and its earlier status is dropped with it.
    fn record_network_log(&mut self, request_id: &str, url: String, method: String) {
        if let Some(entry) = self.network_entry_mut(request_id) {
            *entry = NetworkLogEntry {
                request_id: request_id.to_owned(),
                url,
                method,
                status: None,
                status_text: String::new(),
                failed: false,
                error_text: String::new(),
            };
            return;
        }
        self.network_log.push(NetworkLogEntry {
            request_id: request_id.to_owned(),
            url,
            method,
            status: None,
            status_text: String::new(),
            failed: false,
            error_text: String::new(),
        });
        if self.network_log.len() > PREVIEW_MAX_NETWORK_ENTRIES {
            self.network_log.remove(0);
        }
    }

    fn network_entry_mut(&mut self, request_id: &str) -> Option<&mut NetworkLogEntry> {
        self.network_log
            .iter_mut()
            .find(|entry| entry.request_id == request_id)
    }

    /// Requests started after `watermark`, in start order, with their DevTools request ids.
    fn requests_since(&self, watermark: u64) -> Vec<(String, ObservedRequest)> {
        let mut requests: Vec<(String, ObservedRequest)> = self
            .requests
            .iter()
            .filter(|(_, request)| request.sequence > watermark)
            .map(|(id, request)| (id.clone(), request.clone()))
            .collect();
        requests.sort_by_key(|(_, request)| request.sequence);
        requests
    }

    fn modal_states(&self) -> Vec<ModalState> {
        let mut states = Vec::new();
        if let Some(dialog) = &self.pending_dialog {
            states.push(ModalState::Dialog(dialog.clone()));
        }
        if let Some(chooser) = &self.pending_file_chooser {
            states.push(ModalState::FileChooser(chooser.clone()));
        }
        states
    }

    fn record_dialog(&mut self, kind: &str, message: &str, result: Option<bool>) {
        self.dialog_records.push(DialogRecord {
            timestamp: Utc::now().timestamp_millis(),
            kind: kind.to_owned(),
            message: message.chars().take(4_000).collect(),
            result,
        });
        if self.dialog_records.len() > MAX_DIALOG_RECORDS {
            let excess = self.dialog_records.len() - MAX_DIALOG_RECORDS;
            self.dialog_records.drain(..excess);
        }
    }
}

/// A state of the page that only one specific action can clear, mirroring the modal states of
/// `@playwright/mcp`: while one is present every other page action is refused and told which
/// action clears it.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum ModalState {
    Dialog(PendingDialog),
    FileChooser(PendingFileChooser),
}

impl ModalState {
    fn description(&self) -> String {
        match self {
            ModalState::Dialog(dialog) => {
                format!(
                    "\"{}\" dialog with message \"{}\"",
                    dialog.kind, dialog.message
                )
            }
            ModalState::FileChooser(_) => "File chooser".to_owned(),
        }
    }

    fn cleared_by(&self) -> &'static str {
        match self {
            ModalState::Dialog(_) => "preview_dialog",
            ModalState::FileChooser(_) => "preview_upload_image",
        }
    }

    fn is_cleared_by(&self, tool: PreviewTool) -> bool {
        match self {
            ModalState::Dialog(_) => tool == PreviewTool::Dialog,
            ModalState::FileChooser(_) => tool == PreviewTool::UploadImage,
        }
    }
}

fn render_modal_states(states: &[ModalState]) -> Vec<String> {
    states
        .iter()
        .map(|state| {
            format!(
                "- [{}]: can be handled by {}",
                state.description(),
                state.cleared_by()
            )
        })
        .collect()
}

/// Watermarks taken before an interaction so its settle step only reasons about what the
/// interaction itself caused.
#[derive(Debug, Clone, Copy)]
struct ActionWatch {
    blocked_navigations: u64,
    /// Page generation the watermarks belong to; a recreated page restarts every counter.
    generation: u64,
    request_watermark: u64,
    main_frame_navigations: u64,
    load_events: u64,
    started: Instant,
}

impl ActionWatch {
    fn take(state: &RuntimeState) -> Self {
        ActionWatch {
            blocked_navigations: state.blocked_navigations,
            generation: state.activity.generation,
            request_watermark: state.activity.request_sequence,
            main_frame_navigations: state.activity.main_frame_navigations,
            load_events: state.activity.load_events,
            started: Instant::now(),
        }
    }

    /// Whether the main frame committed a navigation since the watch was taken.
    fn navigated_since(&self, activity: &PageActivity) -> bool {
        if activity.generation != self.generation {
            activity.main_frame_navigations > 0
        } else {
            activity.main_frame_navigations > self.main_frame_navigations
        }
    }

    /// Whether the main frame fired `load` since the watch was taken.
    fn loaded_since(&self, activity: &PageActivity) -> bool {
        if activity.generation != self.generation {
            activity.load_events > 0
        } else {
            activity.load_events > self.load_events
        }
    }

    /// Requests the interaction started, with their DevTools request ids.
    fn requests_since(&self, activity: &PageActivity) -> Vec<(String, ObservedRequest)> {
        let watermark = if activity.generation != self.generation {
            0
        } else {
            self.request_watermark
        };
        activity.requests_since(watermark)
    }
}

/// What the settle step observed after an interaction.
#[derive(Debug, Default)]
struct ActionAftermath {
    /// The interaction started a main-frame navigation.
    navigated: bool,
    /// The navigation policy refused a navigation the interaction started.
    blocked: Option<String>,
    /// The bounded wait for `load` / network quiet ran out; the page may still be busy.
    timed_out: bool,
    /// A dialog or file chooser opened during the interaction and is now held.
    modal_states: Vec<ModalState>,
}

/// One of the fifteen preview tools that act on a page.
///
/// `preview_start`, `preview_stop`, `preview_list` and `preview_logs` never reach a page, so they
/// are not here. Security policy, executor dispatch and page implementation all match on this
/// enum, so adding a page tool is a compile error everywhere it has to be decided instead of a
/// silent fallthrough.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum PreviewTool {
    ConsoleLogs,
    Screenshot,
    Snapshot,
    Inspect,
    Click,
    Fill,
    Eval,
    Network,
    Resize,
    UploadImage,
    Dialog,
    FindElement,
    ClickByDescription,
    FillByDescription,
    InspectByDescription,
}

impl PreviewTool {
    /// Declaration order is the order the catalog lists the page tools in.
    pub(crate) const ALL: [Self; 15] = [
        Self::ConsoleLogs,
        Self::Screenshot,
        Self::Snapshot,
        Self::Inspect,
        Self::Click,
        Self::Fill,
        Self::Eval,
        Self::Network,
        Self::Resize,
        Self::UploadImage,
        Self::Dialog,
        Self::FindElement,
        Self::ClickByDescription,
        Self::FillByDescription,
        Self::InspectByDescription,
    ];

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::ConsoleLogs => "preview_console_logs",
            Self::Screenshot => "preview_screenshot",
            Self::Snapshot => "preview_snapshot",
            Self::Inspect => "preview_inspect",
            Self::Click => "preview_click",
            Self::Fill => "preview_fill",
            Self::Eval => "preview_eval",
            Self::Network => "preview_network",
            Self::Resize => "preview_resize",
            Self::UploadImage => "preview_upload_image",
            Self::Dialog => "preview_dialog",
            Self::FindElement => "preview_find_element",
            Self::ClickByDescription => "preview_click_by_description",
            Self::FillByDescription => "preview_fill_by_description",
            Self::InspectByDescription => "preview_inspect_by_description",
        }
    }

    /// `None` for the four host-side preview tools and for anything that is not a preview tool
    /// at all; both are the caller's business, not this page surface's.
    pub(crate) fn from_tool_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|tool| tool.as_str() == name)
    }

    /// Whether the tool answers with the page's state after it. Only Mework's own two tools do:
    /// the thirteen ported ones answer with the source's exact text and nothing else.
    pub(crate) fn reports_page_after(self) -> bool {
        matches!(self, Self::UploadImage | Self::Dialog)
    }

    /// Whether the tool's consequences are awaited before it answers. Same two, for the same
    /// reason — a page block that does not describe the settled page would be worse than none.
    pub(crate) fn waits_for_completion(self) -> bool {
        matches!(self, Self::UploadImage | Self::Dialog)
    }

    /// Whether the tool's contract only means something to a model that can see images:
    /// `preview_screenshot` hands back pixels, and `preview_upload_image` sends an image the
    /// conversation could only be carrying for such a model.
    pub(crate) fn requires_image_capability(self) -> bool {
        matches!(self, Self::Screenshot | Self::UploadImage)
    }
}

impl std::fmt::Display for PreviewTool {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// What one preview page tool answers with.
#[derive(Debug)]
pub(crate) enum PreviewToolOutput {
    /// Already worded the way the tool words it; the executor passes it through unchanged.
    Text(String),
    /// `preview_screenshot` alone: base64 JPEG pixels, with no workspace path behind them.
    Image(PreviewScreenshot),
}

/// Filesystem authority for the one preview tool that touches host paths. Every path in here must
/// already have passed the caller's path guard; `BrowserRuntime` never resolves renderer-provided
/// paths.
#[derive(Default, Clone)]
pub struct BrowserToolGrants {
    pub upload_paths: Option<Vec<PathBuf>>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BrowserViewport {
    pub width: u32,
    pub height: u32,
}

/// A trusted-UI supplied rectangle for the browser page inside the main window.
///
/// Coordinates and dimensions are logical CSS pixels relative to the main window's content area.
/// `occluded_top` reserves trusted React chrome inside the rectangle (for example a toolbar), so
/// the untrusted remote page starts at `y + occluded_top`. Detached browser windows intentionally
/// ignore this geometry while still honoring `visible`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BrowserPanelBounds {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub visible: bool,
    #[serde(default)]
    pub occluded_top: Option<f64>,
}

/// Exact, reversible CDP fields retained only while a task has no live WebView.
///
/// This type intentionally has no `Debug` or `Serialize` implementation. Cookie values are
/// zeroized when the snapshot is replaced, restored, or the application exits.
struct ColdCloseCookie {
    name: String,
    value: Zeroizing<String>,
    domain: String,
    path: String,
    secure: bool,
    http_only: bool,
    expires: Option<f64>,
    same_site: Option<CookieSameSite>,
    priority: CookiePriority,
    source_scheme: CookieSourceScheme,
    source_port: i32,
    partition_key: Option<CookiePartitionKey>,
}

struct ColdCloseCookieSnapshot {
    cookies: Vec<ColdCloseCookie>,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
enum CookieSameSite {
    Strict,
    Lax,
    None,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
enum CookiePriority {
    Low,
    Medium,
    High,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
enum CookieSourceScheme {
    Unset,
    NonSecure,
    Secure,
}

#[derive(Hash, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct CookiePartitionKey {
    top_level_site: String,
    has_cross_site_ancestor: bool,
}

impl Default for BrowserViewport {
    fn default() -> Self {
        Self {
            width: DEFAULT_WIDTH as u32,
            height: (DEFAULT_HEIGHT - BROWSER_TOOLBAR_HEIGHT) as u32,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BrowserStatus {
    /// Whether the conversation owns a live page WebView, even when that page is hidden.
    pub has_page: bool,
    /// Whether the page is currently visible to the user.
    pub open: bool,
    pub url: String,
    pub title: Option<String>,
    pub loading: bool,
    pub can_go_back: bool,
    pub can_go_forward: bool,
    pub zoom: f64,
    pub viewport: BrowserViewport,
    pub error: Option<String>,
    pub screenshot_path: Option<String>,
    /// The task is not consuming an awake-page slot. Automatic LRU suspension retains the native
    /// WebView in WebView2's sleeping state; explicit user suspension may cold-close it while
    /// preserving the task Profile, resumable URL, and an in-memory Cookie handoff.
    #[serde(default)]
    pub suspended: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suspended_at_ms: Option<i64>,
    /// Trusted browser chrome can surface recent model-driven control without inspecting page
    /// content. The marker is deliberately metadata-only: it never includes typed text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_activity: Option<BrowserAgentActivity>,
    /// Explicit ownership of the shared page. Trusted UI and agent tools coordinate through this
    /// state instead of relying on short-lived activity indicators.
    #[serde(default)]
    pub control: BrowserControlStatus,
    /// Element-picker arm state, and whether a click is waiting to be drained by
    /// `browser_take_selected_element`. Two independent 700 ms pollers read this status, so the
    /// picked element itself deliberately never rides here — only these two booleans do.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub element_picker: Option<BrowserElementPicker>,
    /// Whether a trusted surface is drawn over the page.
    ///
    /// Sleeping, suspending, hiding and closing all restack the page without the renderer asking.
    /// The pane polls this so it can tell that the host no longer believes the page is covered and
    /// stop painting the still frame it captured — the counterpart of the frame going stale.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub occluded: bool,
    /// Whether the pane is painting a still frame of the page in the page's own place.
    ///
    /// This is the resting state of the preview pane: the page stays sunk under the renderer and
    /// the user looks at a projection of it, until the pointer or the keyboard says they want the
    /// live page. It is deliberately *not* `occluded`: nothing is covering the page, the user can
    /// see all of it, and the Agent goes on driving it exactly as it would in front.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub projected: bool,
}

/// The only part of a pick that is cheap enough to poll.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BrowserElementPicker {
    pub armed: bool,
    pub pending_pick: bool,
}

/// A close result deliberately contains no page URL, title, native error text, or imported data.
/// The renderer can make lifecycle decisions from these finite fields without treating an
/// ordinary cleanup failure as proof that the previously accepted Closed intent was rolled back.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum BrowserCloseStatus {
    Closed,
    CleanupPending,
    Rejected,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum BrowserCloseErrorCode {
    InvalidRequest,
    StaleIntent,
    IntentCollision,
    LifecycleUnavailable,
    LifecycleSuperseded,
    NativeCleanupFailed,
    NativeCleanupSurfaceHideFailed,
    InternalFailure,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BrowserCloseDisposition {
    pub status: BrowserCloseStatus,
    pub intent_accepted: bool,
    pub cleanup_complete: bool,
    pub surface_hidden: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<BrowserCloseErrorCode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl BrowserCloseDisposition {
    pub(crate) fn closed() -> Self {
        Self {
            status: BrowserCloseStatus::Closed,
            intent_accepted: true,
            cleanup_complete: true,
            surface_hidden: true,
            error_code: None,
            message: None,
        }
    }

    pub(crate) fn rejected_lifecycle(error: &str) -> Self {
        let (error_code, message) = if error == STALE_BROWSER_LIFECYCLE_INTENT_ERROR {
            (
                BrowserCloseErrorCode::StaleIntent,
                CLOSE_STALE_INTENT_MESSAGE,
            )
        } else if error == COLLIDING_BROWSER_LIFECYCLE_INTENT_ERROR {
            (
                BrowserCloseErrorCode::IntentCollision,
                CLOSE_INTENT_COLLISION_MESSAGE,
            )
        } else if error.starts_with("浏览器会话") || error.contains("安全整数") {
            (
                BrowserCloseErrorCode::InvalidRequest,
                CLOSE_INVALID_REQUEST_MESSAGE,
            )
        } else {
            (
                BrowserCloseErrorCode::LifecycleUnavailable,
                CLOSE_LIFECYCLE_UNAVAILABLE_MESSAGE,
            )
        };
        Self::rejected(error_code, message)
    }

    fn rejected(error_code: BrowserCloseErrorCode, message: &'static str) -> Self {
        Self {
            status: BrowserCloseStatus::Rejected,
            intent_accepted: false,
            cleanup_complete: false,
            surface_hidden: false,
            error_code: Some(error_code),
            message: Some(message.to_owned()),
        }
    }

    fn native_cleanup_failed(surface_hidden: bool) -> Self {
        let (error_code, message) = if surface_hidden {
            (
                BrowserCloseErrorCode::NativeCleanupFailed,
                CLOSE_NATIVE_CLEANUP_MESSAGE,
            )
        } else {
            (
                BrowserCloseErrorCode::NativeCleanupSurfaceHideFailed,
                CLOSE_NATIVE_CLEANUP_HIDE_FAILED_MESSAGE,
            )
        };
        Self::cleanup_pending(surface_hidden, error_code, message)
    }

    fn lifecycle_superseded() -> Self {
        Self::cleanup_pending(
            false,
            BrowserCloseErrorCode::LifecycleSuperseded,
            CLOSE_LIFECYCLE_SUPERSEDED_MESSAGE,
        )
    }

    pub(crate) fn internal_failure() -> Self {
        Self::cleanup_pending(
            false,
            BrowserCloseErrorCode::InternalFailure,
            CLOSE_INTERNAL_FAILURE_MESSAGE,
        )
    }

    fn cleanup_pending(
        surface_hidden: bool,
        error_code: BrowserCloseErrorCode,
        message: &'static str,
    ) -> Self {
        Self {
            status: BrowserCloseStatus::CleanupPending,
            intent_accepted: true,
            cleanup_complete: false,
            surface_hidden,
            error_code: Some(error_code),
            message: Some(message.to_owned()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BrowserAgentActivity {
    pub tool: String,
    pub source: String,
    pub active: bool,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum BrowserControlOwner {
    #[default]
    Available,
    User,
    Agent,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BrowserControlStatus {
    pub owner: BrowserControlOwner,
    pub handoff_requested: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_tool: Option<String>,
    pub updated_at_ms: i64,
}

impl Default for BrowserStatus {
    fn default() -> Self {
        Self {
            has_page: false,
            open: false,
            url: String::new(),
            title: None,
            loading: false,
            can_go_back: false,
            can_go_forward: false,
            zoom: 1.0,
            viewport: BrowserViewport::default(),
            error: None,
            screenshot_path: None,
            suspended: false,
            suspended_at_ms: None,
            agent_activity: None,
            control: BrowserControlStatus::default(),
            element_picker: None,
            occluded: false,
            projected: false,
        }
    }
}

/// One element the user picked out of the page, drained by `browser_take_selected_element`.
///
/// Every string here is page-controlled input on its way into a prompt, so each cap below is
/// applied by the host. The renderer re-asserts them for its own rendering, but this is the gate.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SelectedElement {
    /// Monotonic per session, so a replayed drain is recognizable as one.
    pub sequence: u64,
    pub tag_name: String,
    pub id: Option<String>,
    pub classes: Vec<String>,
    pub attributes: BTreeMap<String, String>,
    pub computed_styles: BTreeMap<String, String>,
    /// Page CSS pixels, document origin — the same space CDP `Page.captureScreenshot` clips in.
    pub bounding_box: SelectedElementBox,
    /// Base64 PNG, empty when the crop failed. A pick is still worth delivering without one.
    pub screenshot_base64: String,
    pub inner_text: Option<String>,
    pub parent_path: Option<String>,
    pub react_component: Option<String>,
    pub react_props: Option<Map<String, Value>>,
    pub source_file: Option<String>,
    pub outer_html: Option<String>,
    pub sibling_html: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SelectedElementBox {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// The only attributes a pick carries. Everything else on the element is page-chosen naming that
/// buys the model nothing and widens the injection surface.
const PICKER_ATTRIBUTE_ALLOWLIST: [&str; 13] = [
    "data-testid",
    "data-test-id",
    "aria-label",
    "name",
    "type",
    "href",
    "role",
    "placeholder",
    "title",
    "alt",
    "onclick",
    "value",
    "src",
];

/// Kebab-case on purpose: `CSS.getComputedStyleForNode` reports property names that way, and the
/// upstream camelCase list silently resolves fewer than half of its own entries.
const PICKER_STYLE_PROPS: [&str; 15] = [
    "display",
    "position",
    "flex-direction",
    "justify-content",
    "align-items",
    "width",
    "height",
    "padding",
    "margin",
    "background-color",
    "color",
    "font-size",
    "font-weight",
    "border-radius",
    "border",
];

const PICKER_MAX_TAG_CHARS: usize = 32;
const PICKER_MAX_ID_CHARS: usize = 256;
const PICKER_MAX_CLASSES: usize = 20;
const PICKER_MAX_CLASS_CHARS: usize = 128;
const PICKER_MAX_ATTRIBUTE_CHARS: usize = 256;
const PICKER_MAX_STYLE_PROPS: usize = 50;
const PICKER_MAX_STYLE_VALUE_CHARS: usize = 256;
const PICKER_MAX_INNER_TEXT_CHARS: usize = 200;
const PICKER_MAX_PARENT_PATH_CHARS: usize = 512;
const PICKER_MAX_HTML_CHARS: usize = 2_000;
const PICKER_MAX_REACT_COMPONENT_CHARS: usize = 256;
const PICKER_MAX_REACT_PROPS: usize = 50;
const PICKER_MAX_REACT_PROP_CHARS: usize = 256;
const PICKER_MAX_SOURCE_FILE_CHARS: usize = 512;
/// Padding around the element in the crop, in page CSS pixels.
const PICKER_SCREENSHOT_PADDING: f64 = 80.0;
/// Enforced through the clip's own `scale`, so nothing is resampled host-side.
const PICKER_SCREENSHOT_MAX_DIMENSION: f64 = 1_200.0;
const PICKER_MAX_SCREENSHOT_BASE64: usize = 2 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BrowserScreenshot {
    pub path: String,
    pub bytes: u64,
    pub width: u32,
    pub height: u32,
    pub full_page: bool,
}

pub(crate) struct BrowserPngCapture {
    pub(crate) bytes: Vec<u8>,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) full_page: bool,
}

/// The visible page as a base64 PNG returned by value, at whatever resolution Chromium is
/// compositing it — the annotate surface is drawn on top of these exact pixels.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserPageCapture {
    pub data: String,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TargetSpec {
    selector: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingNavigation {
    New,
    Back,
    Forward,
    Reload,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NavigationResumePlan {
    Continue,
    ResumeNativeThenContinue,
    ResumeColdCompletesReload,
    ColdHistoryUnavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BrowserHost {
    MainPanel,
    DetachedWindow,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct BrowserPageLayout {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

impl BrowserPageLayout {
    fn viewport(self) -> BrowserViewport {
        BrowserViewport {
            width: self.width.round().clamp(0.0, u32::MAX as f64) as u32,
            height: self.height.round().clamp(0.0, u32::MAX as f64) as u32,
        }
    }
}

#[derive(Default)]
struct RuntimeState {
    app: Option<AppHandle>,
    terminated: bool,
    /// Exclusive claim on this tab's canonical WebView2 user-data folder. The lease survives a
    /// cold close/resume and is released only when the tab itself closes.
    webview2_lease: Option<WebView2RuntimeLease>,
    /// Adapter-only issuer retained across cold close/resume. Operation controls cannot mint a
    /// replacement generation, so a stale clone can never reactivate itself.
    webview2_controller_issuer: Option<WebView2ControllerIssuer>,
    /// Revocable authority for exactly one native controller generation. It becomes usable only
    /// after that controller attests its actual `UserDataFolder`.
    webview2_control: Option<WebView2Control>,
    /// Release-only authority retained across an asynchronous close and label-release wait. A
    /// failed close keeps this exact token so an idempotent retry never needs to revive operation
    /// authority merely to finish destroying the old controller.
    webview2_teardown: Option<WebView2TeardownPermit>,
    #[cfg(test)]
    shutdown_failure: Option<String>,
    #[cfg(test)]
    hide_attempts: u64,
    #[cfg(test)]
    synthetic_surface: bool,
    #[cfg(test)]
    hide_failure: Option<String>,
    status: BrowserStatus,
    host: Option<BrowserHost>,
    /// Page ownership as it stood before a trusted surface covered the page, restored when the
    /// surface goes away. Covering the page takes it out of agent automation for as long as the
    /// cover is up, and the user must get back exactly the owner they had before, not a default.
    menu_control_before_open: Option<BrowserControlStatus>,
    /// Whether the renderer currently has a trusted surface drawn over the page.
    ///
    /// The page is a native child window that paints above every HTML layer, so a covered page is
    /// stacked beneath the trusted WebView and the renderer paints a still frame of it in the
    /// pane instead. This flag is the whole of what the host is told: not where the surface is,
    /// only that there is one. It is presentation state, not lifecycle state — a page can be
    /// occluded while open, and re-presenting an occluded page must not bring it back on top.
    occluded: bool,
    /// Whether the renderer is standing a still frame in for the page while nobody is using it.
    ///
    /// Same stacking consequence as `occluded` and no other consequence at all. The two are kept
    /// apart because covering means the user *cannot see* the page — which is why it takes the
    /// page out of agent automation and forces user control — whereas projecting means the user
    /// sees a picture of it that is one refresh old. Folding projection into `occluded` would hand
    /// control back to the user every time they moved the pointer off the pane, which is to say
    /// permanently, and the Agent would never be able to drive a preview again.
    projected: bool,
    panel_bounds: Option<BrowserPanelBounds>,
    /// Renderer generation that last made this MainPanel surface visible.
    ///
    /// This is native-only presentation ownership. It is deliberately
    /// independent from the conversation lifecycle intent so a renderer
    /// reload can hide an orphaned WebView without closing its profile,
    /// cancelling imports, or changing Open/Hidden/Closed authority.
    renderer_presentation_generation: Option<u64>,
    layout_generation: u64,
    /// Invalidates all callbacks owned by a WebView after it is suspended or a partial creation
    /// fails. Layout has a separate generation because it is also replaced on host changes.
    page_generation: u64,
    history: Vec<String>,
    history_index: Option<usize>,
    pending_navigation: Option<PendingNavigation>,
    pending_previous_url: Option<String>,
    /// Suppresses callbacks from the provisional about:blank used to restore an in-memory cookie
    /// handoff before the first real cold-resume navigation.
    cold_resume_target: Option<String>,
    /// Values exist only between a successful cold close and its matching resume.
    cold_close_cookies: Option<ColdCloseCookieSnapshot>,
    /// Exact origin the user authorized the Agent to drive on this user-opened tab.
    ///
    /// The user is never asked to justify their own browsing: a tab the user opened always uses
    /// whatever sign-in material the conversation profile holds. This grant only covers the Agent
    /// taking that surface over, and it is deliberately narrow — it is bound to one session and one
    /// origin, and it is dropped as soon as the committed document leaves that origin or the page
    /// suspends or closes, so a later navigation cannot inherit it.
    credential_takeover_grant: Option<String>,
    /// Virtual-host mapping and staging directory backing the currently displayed local file.
    ///
    /// Released as soon as a document from any other origin commits, so the mapped copy is
    /// reachable only while it is the page the user asked for.
    file_preview: Option<FilePreviewGrant>,
    /// Effective security level of the conversation driving this session.
    ///
    /// Pushed by the model run loop before each browser tool call rather than
    /// read on demand, because the navigation callbacks fire later and from the
    /// WebView's own thread: a page-initiated navigation has no caller to ask.
    /// The default is the most restrictive level, so a session that has never
    /// been claimed by a conversation never widens anything.
    security_level: SecurityLevel,
    /// Counts navigations the policy refused. An action reports a refusal only
    /// when this moved while it ran, which is what separates "my click was
    /// blocked" from an older block still sitting in `status.error`.
    blocked_navigations: u64,
    /// Latched the first time the user takes control of this page from trusted chrome, never
    /// cleared for the session's lifetime. Each tab runs on a single-use profile, so until this
    /// point every cookie the profile holds was acquired under Agent-driven browsing — the
    /// Agent's own doing, not the user's sign-in material.
    user_has_controlled: bool,
    /// Trusted application preferences are retained outside page JavaScript so every later
    /// about:blank document can be initialized consistently without touching remote documents.
    ui_theme: Option<String>,
    ui_language: Option<String>,
    /// Monotonically orders asynchronous page-eval replays without blocking the WebView UI
    /// thread. Each document ignores a preference payload older than the last one it applied.
    ui_preferences_generation: u64,
    /// What the host observed about the current native page from outside its JavaScript.
    activity: PageActivity,
    /// Arm/pick state of the trusted-chrome element picker.
    element_picker: ElementPickerState,
}

/// One user-driven element pick, from arming the Chrome inspector overlay to draining the payload.
///
/// The arm is bound to a page generation instead of being reset at each of the sites that bump
/// one: a navigation, suspend, or failed creation leaves the page without inspect mode, and this
/// reads as disarmed from that moment without any of them having to know about the picker.
#[derive(Debug, Default)]
struct ElementPickerState {
    armed_generation: Option<u64>,
    /// `backendNodeId` plus the generation it was observed under, written by the WebView2 UI
    /// thread and drained by the command.
    pending: Option<(u64, i64)>,
    sequence: u64,
}

impl ElementPickerState {
    fn armed(&self, page_generation: u64) -> bool {
        self.armed_generation == Some(page_generation)
    }

    fn arm(&mut self, page_generation: u64) {
        self.armed_generation = Some(page_generation);
        self.pending = None;
    }

    fn disarm(&mut self) {
        self.armed_generation = None;
        self.pending = None;
    }

    /// Escape pressed inside the page. The arm ends, but an already-observed click is still worth
    /// delivering, so the pending node survives.
    fn cancel(&mut self) {
        self.armed_generation = None;
    }

    /// `false` when the write was refused: not armed, or a pick is already waiting. The latch is
    /// what keeps a double click from queueing two captures.
    fn note_inspect_node(&mut self, page_generation: u64, backend_node_id: i64) -> bool {
        if !self.armed(page_generation) || self.pending.is_some() {
            return false;
        }
        self.pending = Some((page_generation, backend_node_id));
        true
    }

    /// Drains the pending node, dropping one recorded before the page changed underneath it.
    fn take_pending(&mut self, page_generation: u64) -> Option<i64> {
        let (generation, backend_node_id) = self.pending.take()?;
        (generation == page_generation).then_some(backend_node_id)
    }

    fn next_sequence(&mut self) -> u64 {
        self.sequence = self.sequence.wrapping_add(1);
        self.sequence
    }

    fn snapshot(&self, page_generation: u64) -> Option<BrowserElementPicker> {
        let armed = self.armed(page_generation);
        let pending_pick =
            matches!(self.pending, Some((generation, _)) if generation == page_generation);
        (armed || pending_pick).then_some(BrowserElementPicker {
            armed,
            pending_pick,
        })
    }
}

/// One staged local-file preview: the directory WebView2 maps and the address it is served at.
#[derive(Clone, Debug, Eq, PartialEq)]
struct FilePreviewGrant {
    directory: PathBuf,
    url: String,
}

/// A native page coupled to an owned controller-generation permit.
///
/// There is deliberately no `Deref<Target = Webview>` implementation. Tauri's ordinary WebView
/// mutations return after an off-main-thread message is queued, so letting callers invoke them
/// directly would release the caller's permit before the UI thread executes the operation. Every
/// mutation below moves a cloned permit into the main-thread task. Asynchronous JavaScript/native
/// completions retain only a revocable callback token after registration, then reacquire a short
/// permit if the callback is actually delivered.
struct AttestedPage {
    page: Webview,
    _permit: WebView2Permit,
}

impl AttestedPage {
    /// Extends this logical operation into a queued UI-thread closure. Native callbacks derive a
    /// non-blocking token from this permit before registration instead of retaining the clone
    /// indefinitely.
    fn tail_permit(&self) -> WebView2Permit {
        self._permit.clone()
    }

    /// Runs a normal Tauri WebView mutation on the UI thread while this controller generation is
    /// still in flight. `run_on_main_thread` executes inline when already on the UI thread and
    /// queues otherwise, so callbacks cannot deadlock while off-thread callers still receive the
    /// operation result. If the caller times out, the queued closure continues to own the permit.
    fn dispatch_mutation<T>(
        &self,
        stage: &'static str,
        operation: impl FnOnce(&Webview) -> Result<T, String> + Send + 'static,
    ) -> Result<T, String>
    where
        T: Send + 'static,
    {
        let page = self.page.clone();
        let tail_permit = self.tail_permit();
        let (sender, receiver) = mpsc::sync_channel(1);
        self.page
            .run_on_main_thread(move || {
                run_checked_webview_task(tail_permit, || operation(&page), sender);
            })
            .map_err(|error| format!("无法调度{stage}: {error}"))?;
        receiver
            .recv_timeout(EVAL_TIMEOUT)
            .map_err(|error| match error {
                mpsc::RecvTimeoutError::Timeout => {
                    format!("等待{stage}超时（{} ms）", EVAL_TIMEOUT.as_millis())
                }
                mpsc::RecvTimeoutError::Disconnected => format!("{stage}结果通道已关闭"),
            })?
    }

    fn hide(&self) -> Result<(), String> {
        self.dispatch_mutation("隐藏 Chromium 页面", |page| {
            page.hide().map_err(|error| error.to_string())
        })
    }

    fn show(&self) -> Result<(), String> {
        self.dispatch_mutation("显示 Chromium 页面", |page| {
            page.show().map_err(|error| error.to_string())
        })
    }

    fn set_focus(&self) -> Result<(), String> {
        self.dispatch_mutation("聚焦 Chromium 页面", |page| {
            page.set_focus().map_err(|error| error.to_string())
        })
    }

    fn set_zoom(&self, factor: f64) -> Result<(), String> {
        self.dispatch_mutation("设置 Chromium 页面缩放", move |page| {
            page.set_zoom(factor).map_err(|error| error.to_string())
        })
    }

    fn navigate(&self, url: Url) -> Result<(), String> {
        self.dispatch_mutation("导航 Chromium 页面", move |page| {
            page.navigate(url).map_err(|error| error.to_string())
        })
    }

    fn reload(&self) -> Result<(), String> {
        self.dispatch_mutation("重新加载 Chromium 页面", |page| {
            page.reload().map_err(|error| error.to_string())
        })
    }

    fn set_position(&self, position: impl Into<Position>) -> Result<(), String> {
        let position = position.into();
        self.dispatch_mutation("设置 Chromium 页面位置", move |page| {
            page.set_position(position)
                .map_err(|error| error.to_string())
        })
    }

    fn set_layout(&self, layout: BrowserPageLayout) -> Result<(), String> {
        self.dispatch_mutation("设置 Chromium 页面布局", move |page| {
            page.set_position(LogicalPosition::new(layout.x, layout.y))
                .and_then(|_| page.set_size(LogicalSize::new(layout.width, layout.height)))
                .map_err(|error| error.to_string())
        })
    }

    /// Keeps a page the user is not looking at natively visible, at its on-screen layout, but
    /// stacked beneath the trusted React WebView that covers the whole window. A WebView2
    /// controller that is hidden or moved off-screen stops compositing, and with it the renderer
    /// stops acknowledging input events and painting frames: a click then waits on a 15 s timeout
    /// and the page runs its timers at 1 Hz, which is exactly the latency a background
    /// automation must not pay. Parked, the page is fully covered (nothing shows, no pointer
    /// input reaches it) while Chromium still sees an on-screen window.
    fn park(&self, layout: BrowserPageLayout) -> Result<(), String> {
        self.dispatch_mutation("停放 Chromium 页面", move |page| {
            page.set_position(LogicalPosition::new(layout.x, layout.y))
                .and_then(|_| page.set_size(LogicalSize::new(layout.width, layout.height)))
                .and_then(|_| page.show())
                .map_err(|error| error.to_string())
        })?;
        self.with_native_tail(|native, permit| set_page_stacking(native, permit, true))
    }

    /// Stacks the page beneath the trusted WebView (`parked`) or back above it, without touching
    /// its geometry or visibility. The counterpart of `park`, and the only way back out of it.
    ///
    /// This is the whole of what covering a page costs: the page keeps its size, its position and
    /// its compositing, so the Agent's automation runs at foreground speed behind a dialog exactly
    /// as it does in front of one, and nothing about the page's own layout changes when the
    /// renderer draws over it.
    fn set_stacking(&self, parked: bool) -> Result<(), String> {
        self.with_native_tail(move |native, permit| set_page_stacking(native, permit, parked))
    }

    fn eval_with_callback(
        &self,
        script: impl Into<String>,
        callback: impl Fn(String) + Send + 'static,
    ) -> Result<(), String> {
        let callback_token = self._permit.callback_token();
        let script = script.into();
        self.dispatch_mutation("执行 Chromium 脚本", move |page| {
            page.eval_with_callback(script, move |result| {
                // A page script may deliberately await forever. The dormant callback therefore
                // retains only revocable authority; it fences this body if and when WebView2
                // invokes it, and becomes a no-op after controller invalidation.
                let Ok(_callback_permit) = callback_token.permit() else {
                    return;
                };
                callback(result)
            })
            .map_err(|error| error.to_string())
        })
    }

    fn eval(&self, script: impl Into<String>) -> Result<(), String> {
        self.eval_with_callback(script, |_| {})
    }

    fn open_devtools(&self) -> Result<(), String> {
        self.dispatch_mutation("打开 Chromium DevTools", |page| {
            page.open_devtools();
            Ok(())
        })
    }

    fn close_devtools(&self) -> Result<(), String> {
        self.dispatch_mutation("关闭 Chromium DevTools", |page| {
            page.close_devtools();
            Ok(())
        })
    }

    /// Native getters synchronously wait for Tauri's dispatcher, so the wrapper's original permit
    /// remains live until each observation completes.
    fn url(&self) -> tauri::Result<Url> {
        self.page.url()
    }

    fn size(&self) -> tauri::Result<PhysicalSize<u32>> {
        self.page.size()
    }

    fn position(&self) -> tauri::Result<PhysicalPosition<i32>> {
        self.page.position()
    }

    fn window(&self) -> Window {
        self.page.window()
    }

    /// Supplies the raw handle only inside a closure that also receives an owned async-tail
    /// permit. This is the boundary for helpers that already retain that permit through native
    /// completion callbacks (CDP, suspend/resume, browsing-data clear, and window regions).
    fn with_native_tail<T>(&self, operation: impl FnOnce(&Webview, WebView2Permit) -> T) -> T {
        operation(&self.page, self.tail_permit())
    }

    /// Controller close is authorized by a release-only teardown token, not a normal page permit.
    /// Consume the wrapper before invalidation so no general operation authority survives into the
    /// close/destroy phase.
    fn into_native_for_teardown(self) -> Webview {
        let Self { page, _permit } = self;
        drop(_permit);
        page
    }
}

/// The small, testable core used by every ordinary WebView mutation. Keeping the permit in this
/// function's frame proves that an operation cannot leave the controller-generation drain until
/// the queued main-thread task has actually run (or Tauri drops the task without running it).
fn run_checked_webview_task<T>(
    _tail_permit: WebView2Permit,
    operation: impl FnOnce() -> Result<T, String>,
    sender: mpsc::SyncSender<Result<T, String>>,
) {
    let _ = sender.try_send(operation());
}

#[derive(Debug)]
struct BrowserLabels {
    window: String,
    page: String,
    profile_root: &'static str,
    profile: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum BrowserNavigationPolicy {
    Ordinary,
}

impl BrowserNavigationPolicy {
    fn allows(&self, url: &Url, level: SecurityLevel) -> bool {
        match self {
            Self::Ordinary => is_navigation_allowed_at(url, level),
        }
    }

    fn may_retarget_new_window_to_current_page(&self) -> bool {
        matches!(self, Self::Ordinary)
    }
}

fn blocked_navigation_summary(url: &Url) -> String {
    let origin = url.origin().ascii_serialization();
    if origin == "null" {
        format!("{}:", url.scheme())
    } else {
        origin
    }
}

/// One conversation-owned remote browser page hosted beside the trusted React sidebar chrome.
#[derive(Clone)]
pub(crate) struct BrowserSession {
    state: Arc<Mutex<RuntimeState>>,
    /// Serializes creation and teardown independently from the state mutex. Tauri may synchronously
    /// invoke WebView callbacks while these operations run, so lifecycle code must never hold the
    /// state mutex while calling into Tauri.
    lifecycle: Arc<Mutex<()>>,
    /// Prevents two agents from interleaving actionability checks and input events on one page.
    automation: Arc<Mutex<()>>,
    /// Serializes generation checks with CDP overlay updates. This keeps a stale cleanup timer
    /// from hiding a newer action between its generation check and `hideHighlight`.
    agent_pointer_overlay: Arc<Mutex<()>>,
    /// Invalidates delayed CDP-overlay cleanup without exposing any marker state to the page.
    agent_pointer_generation: Arc<AtomicU64>,
    labels: Arc<BrowserLabels>,
    session_id: Arc<str>,
    navigation_policy: Arc<BrowserNavigationPolicy>,
}

/// Restores the shared page to a stable owner even when an agent tool exits early or unwinds.
/// The automation mutex outlives this guard at each call site, so cleanup cannot race trusted UI.
struct AgentControlGuard {
    state: Arc<Mutex<RuntimeState>>,
    tool_name: String,
}

impl Drop for AgentControlGuard {
    fn drop(&mut self) {
        let mut state = lock_unpoison(&self.state);
        if state.status.control.owner == BrowserControlOwner::Agent {
            state.status.control = BrowserControlStatus {
                owner: BrowserControlOwner::Available,
                updated_at_ms: Utc::now().timestamp_millis(),
                ..BrowserControlStatus::default()
            };
        }
        if let Some(activity) = state.status.agent_activity.as_mut() {
            if activity.tool == self.tool_name {
                activity.active = false;
                activity.updated_at_ms = Utc::now().timestamp_millis();
            }
        }
    }
}

#[derive(Default)]
struct BrowserManagerState {
    app: Option<AppHandle>,
    sessions: HashMap<String, BrowserSession>,
    active_session_id: Option<String>,
    live_reservations: HashSet<String>,
    last_used: HashMap<String, u64>,
    /// A trusted tab close is an explicit lifecycle fence, not merely a best-effort WebView
    /// teardown. Late renderer callbacks and Agent commands must not recreate the conversation
    /// session until the trusted UI explicitly opens it again.
    closed_session_ids: HashSet<String>,
    /// Geometry may arrive before the matching explicit open. Keep it manager-side so publishing
    /// layout never has to allocate a BrowserSession and therefore cannot cross a close fence.
    pending_panel_bounds: HashMap<String, PendingBrowserPanelBounds>,
    /// Renderer reloads may replay an old open/close completion after a newer UI intent. Track the
    /// exact conversation's monotonic lifecycle generation in the native authority boundary so a
    /// stale renderer can neither recreate nor destroy a newer page.
    lifecycle_intents: HashMap<String, BrowserSessionLifecycleIntent>,
    access_sequence: u64,
    shutting_down: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BrowserSessionLifecycleDesired {
    Open,
    Hidden,
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BrowserSessionLifecycleIntent {
    epoch: u64,
    desired: BrowserSessionLifecycleDesired,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct PendingBrowserPanelBounds {
    epoch: u64,
    bounds: BrowserPanelBounds,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct BrowserCloseIntentRollback {
    lifecycle_intent: Option<BrowserSessionLifecycleIntent>,
    closed_tombstone: bool,
    pending_bounds: Option<PendingBrowserPanelBounds>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClosedSurfaceHideOutcome {
    Hidden,
    Superseded,
    Failed,
}

/// Conversation-scoped browser pages. Each session uses a separate single-use Chromium
/// user-data folder so remote sites never share storage with the trusted application WebView.
#[derive(Clone, Default)]
pub struct BrowserRuntime {
    state: Arc<Mutex<BrowserManagerState>>,
    lifecycle: Arc<Mutex<()>>,
}

struct LivePageReservation {
    state: Arc<Mutex<BrowserManagerState>>,
    session_id: String,
}

impl Drop for LivePageReservation {
    fn drop(&mut self) {
        lock_unpoison(&self.state)
            .live_reservations
            .remove(&self.session_id);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CapacitySnapshot {
    session_id: String,
    last_used: u64,
    has_page: bool,
    retained: bool,
    suspended: bool,
    open: bool,
    loading: bool,
    pending_navigation: bool,
    occluded: bool,
    owner: BrowserControlOwner,
    active: bool,
    reserved: bool,
}

impl BrowserRuntime {
    pub fn attach_app(&self, app: AppHandle) -> Result<(), String> {
        let _manager_lifecycle = lock_unpoison(&self.lifecycle);
        let sessions = {
            let mut state = lock_unpoison(&self.state);
            state.app = Some(app.clone());
            state.sessions.values().cloned().collect::<Vec<_>>()
        };
        for session in sessions {
            session.attach_app(app.clone())?;
        }
        let runtime = self.clone();
        if let Err(error) = std::thread::Builder::new()
            .name("mework-browser-tab-profile-cleanup".into())
            .spawn(move || {
                // Tab profiles are single-use and never survive a run: every profile name mixes a
                // per-session random nonce, so at startup nothing under the tab root can belong to
                // a live session and everything hash-named is a crash leftover.
                let _manager_lifecycle = lock_unpoison(&runtime.lifecycle);
                let active_tab_profiles = {
                    let state = lock_unpoison(&runtime.state);
                    state
                        .sessions
                        .values()
                        .map(|session| session.labels.profile.clone())
                        .collect::<HashSet<_>>()
                };
                match tab_profile_root(&app) {
                    Ok(root) => {
                        if let Err(error) =
                            cleanup_stale_profiles_in_root(&root, &active_tab_profiles)
                        {
                            eprintln!("标签页浏览器配置的后台启动清理未完成: {error}");
                        }
                    }
                    Err(error) => eprintln!("标签页浏览器配置的后台启动清理未完成: {error}"),
                }
            })
        {
            eprintln!("无法启动标签页浏览器配置清理线程: {error}");
        }
        Ok(())
    }

    #[allow(dead_code)]
    pub fn is_attached(&self) -> bool {
        lock_unpoison(&self.state).app.is_some()
    }

    pub(crate) fn session(&self, session_id: &str) -> Result<BrowserSession, String> {
        let session_id = validate_session_id(session_id)?;
        let app = {
            let state = lock_unpoison(&self.state);
            if state.shutting_down {
                return Err(
                    "the application is shutting down and cannot create a browser page".into(),
                );
            }
            if state.closed_session_ids.contains(session_id) {
                return Err(
                    "the embedded browser tab is closed; reopen it from the trusted UI first"
                        .into(),
                );
            }
            if let Some(session) = state.sessions.get(session_id) {
                return Ok(session.clone());
            }
            state.app.clone()
        };
        // The single-use profile is minted here, once, and never changes for the life of the
        // session: a WebView2 user-data folder is chosen when the controller is created, so the
        // only way out of a profile is closing the tab.
        let session = BrowserSession::new(session_id);
        if let Some(app) = app {
            session.attach_app(app)?;
        }
        let mut state = lock_unpoison(&self.state);
        if state.shutting_down {
            return Err("the application is shutting down and cannot create a browser page".into());
        }
        if state.closed_session_ids.contains(session_id) {
            return Err(
                "the embedded browser tab is closed; reopen it from the trusted UI first".into(),
            );
        }
        Ok(state
            .sessions
            .entry(session_id.to_owned())
            .or_insert(session)
            .clone())
    }

    /// Returns an already-open session without creating one.
    ///
    /// Tests use this as the "lookup never allocates" oracle: a stale or forged
    /// session ID must never allocate a new profile boundary merely because it
    /// was looked up.
    #[cfg(test)]
    pub(crate) fn existing_session(&self, session_id: &str) -> Result<BrowserSession, String> {
        let session_id = validate_session_id(session_id)?;
        let state = lock_unpoison(&self.state);
        if state.shutting_down {
            return Err("应用正在退出，不能再访问浏览器页面".into());
        }
        if state.closed_session_ids.contains(session_id) {
            return Err("内置浏览器标签页正在关闭或已经关闭；请先从可信界面重新打开".into());
        }
        state
            .sessions
            .get(session_id)
            .cloned()
            .ok_or_else(|| "内置浏览器会话不存在；请先打开当前任务的浏览器".to_owned())
    }

    /// Executes one preview page tool while retaining a capacity reservation across first-page
    /// creation. The reservation is deliberately manager-owned: returning only a `BrowserSession`
    /// from `session_for_tool` cannot prevent two concurrent conversations from both observing a
    /// free slot before either WebView exists.
    pub(crate) fn execute_tool_blocking(
        &self,
        session_id: &str,
        tool: PreviewTool,
        input: &Map<String, Value>,
        grants: &BrowserToolGrants,
    ) -> Result<PreviewToolOutput, String> {
        let session_id = validate_session_id(session_id)?.to_owned();
        self.reopen_closed_session_for_agent(&session_id)?;
        let session = self.session(&session_id)?;
        let _reservation = if browser_tool_needs_live_slot(&session.status()) {
            let _lifecycle = lock_unpoison(&self.lifecycle);
            self.reserve_live_slot_locked(&session_id, &session)?
        } else {
            None
        };
        self.touch_session(&session_id);
        let result = session.execute_tool_blocking(tool, input, grants);
        self.touch_session(&session_id);
        result
    }

    /// The page's accessibility snapshot as lines, for `preview_find_element` and the
    /// `preview_*_by_description` variants. Same reservation, touch and page-preparation contract
    /// as [`Self::execute_tool_blocking`]; the caller supplies the wall clock, because the
    /// decision-model round trips that follow this read are not the page's to wait for.
    pub(crate) fn element_lines_blocking(
        &self,
        session_id: &str,
    ) -> Result<(Vec<AxLine>, bool), String> {
        self.read_page_blocking(session_id, PreviewTool::FindElement, |session| {
            session.preview_element_lines()
        })
    }

    /// A CSS selector for one element of the last `element_lines_blocking` read, or `None` when
    /// the page no longer offers one.
    pub(crate) fn selector_for_blocking(
        &self,
        session_id: &str,
        backend_node_id: i64,
    ) -> Result<Option<String>, String> {
        self.read_page_blocking(session_id, PreviewTool::FindElement, |session| {
            session.unique_selector_for(backend_node_id)
        })
    }

    /// Every console entry of the page as `[level] text`, for `preview_find_logs`.
    pub(crate) fn console_log_lines_blocking(
        &self,
        session_id: &str,
    ) -> Result<Vec<String>, String> {
        self.read_page_blocking(session_id, PreviewTool::ConsoleLogs, |session| {
            session.console_log_lines()
        })
    }

    fn read_page_blocking<T>(
        &self,
        session_id: &str,
        tool: PreviewTool,
        read: impl FnOnce(&BrowserSession) -> Result<T, String>,
    ) -> Result<T, String> {
        let session_id = validate_session_id(session_id)?.to_owned();
        self.reopen_closed_session_for_agent(&session_id)?;
        let session = self.session(&session_id)?;
        let _reservation = if browser_tool_needs_live_slot(&session.status()) {
            let _lifecycle = lock_unpoison(&self.lifecycle);
            self.reserve_live_slot_locked(&session_id, &session)?
        } else {
            None
        };
        self.touch_session(&session_id);
        let result = session.read_page_blocking(tool, read);
        self.touch_session(&session_id);
        result
    }

    /// Points the conversation's page at a dev server `preview_start` just started.
    ///
    /// A started server whose page still shows about:blank is a preview in name only, and the
    /// model has no navigation tool of its own to fix that with.
    pub(crate) fn open_preview_at(&self, session_id: &str, url: &str) -> Result<(), String> {
        let session_id = validate_session_id(session_id)?.to_owned();
        self.reopen_closed_session_for_agent(&session_id)?;
        let session = self.session(&session_id)?;
        let parsed = parse_browser_url(url, session.security_level())?;
        let _reservation = if browser_tool_needs_live_slot(&session.status()) {
            let _lifecycle = lock_unpoison(&self.lifecycle);
            self.reserve_live_slot_locked(&session_id, &session)?
        } else {
            None
        };
        self.touch_session(&session_id);
        let result = if session.page().is_err() {
            session.prepare(Some(parsed.as_str())).map(|_| ())
        } else {
            session.navigate_parsed(parsed)
        };
        self.touch_session(&session_id);
        result
    }

    /// Origin at which a preview tool would take over a page the *user* opened while that page
    /// carries the user's sign-in material.
    ///
    /// `Ok(None)` means no authorization is needed: either the page is not carrying credentials
    /// for its committed origin, or the user already approved this exact origin on this exact tab.
    pub(crate) fn pending_credential_takeover(
        &self,
        session_id: &str,
    ) -> Result<Option<String>, String> {
        let session = {
            let state = lock_unpoison(&self.state);
            if state.closed_session_ids.contains(session_id) {
                return Ok(None);
            }
            state.sessions.get(session_id).cloned()
        };
        // No live session yet means there is no user page to take over. The tool that creates one
        // will be acting on a surface the Agent itself caused to exist.
        let Some(session) = session else {
            return Ok(None);
        };
        // Same principle after creation: until the user has actually driven this page from
        // trusted chrome, every cookie in the tab's single-use profile came from Agent-driven
        // browsing. Prompting would ask the user to authorize the Agent against the Agent's own
        // session state — and would fire on every ordinary site that sets a cookie on load.
        if !session.user_has_ever_controlled() {
            return Ok(None);
        }
        let Some(origin) = session.credentialed_origin()? else {
            return Ok(None);
        };
        if session.credential_takeover_granted(&origin) {
            return Ok(None);
        }
        Ok(Some(origin))
    }

    /// Records the user's approval for the Agent to drive one user-opened page at one origin.
    pub(crate) fn grant_credential_takeover(
        &self,
        session_id: &str,
        origin: &str,
    ) -> Result<(), String> {
        let session = {
            let state = lock_unpoison(&self.state);
            state.sessions.get(session_id).cloned()
        };
        let session = session.ok_or_else(|| {
            "the authorized browser tab no longer exists; start the operation again".to_owned()
        })?;
        // Re-read the committed origin under the session's own lock. An approval must not land on a
        // page that navigated somewhere else while the dialog was open.
        match session.credentialed_origin()? {
            Some(current) if current == origin => {
                session.grant_credential_takeover(origin);
                Ok(())
            }
            _ => Err(format!(
                "this tab has left {origin}, so this authorization is void; authorize the current page again"
            )),
        }
    }

    /// Records the conversation's effective security level on the session its
    /// browser tools currently act on, so navigation admission — including the
    /// page-initiated navigations that arrive after the tool returns — reflects
    /// the level the user chose for this conversation.
    ///
    /// The session is created if it does not exist yet: the first `navigate` of a
    /// conversation would otherwise be admitted against the default level, which
    /// is the one call most likely to be opening a local development server.
    /// A session that cannot be created at all is left to fail in the tool call
    /// itself, where the reason can be reported.
    pub(crate) fn set_session_security_level(&self, session_id: &str, level: SecurityLevel) {
        if let Ok(session) = self.session(session_id) {
            session.set_security_level(level);
        }
    }

    /// A preview tool acting on a page that was closed — by the user from the trusted chrome, or
    /// by a shutdown — starts over on a fresh page instead of failing, the way `@playwright/mcp`
    /// opens a new tab when none is left. Only the tombstone that keeps late renderer callbacks
    /// from re-minting the page is cleared here; the page itself is created by the tool that
    /// needed it.
    fn reopen_closed_session_for_agent(&self, session_id: &str) -> Result<(), String> {
        let _lifecycle = lock_unpoison(&self.lifecycle);
        let cleanup_candidate = {
            let state = lock_unpoison(&self.state);
            if state.shutting_down {
                return Err(
                    "the application is shutting down and cannot create a browser page".into(),
                );
            }
            if !state.closed_session_ids.contains(session_id) {
                return Ok(());
            }
            state.sessions.get(session_id).cloned()
        };
        // A failed close keeps its terminated handle until the native labels are reusable; retry
        // that cleanup first, exactly as a trusted reopen does.
        if let Some(candidate) = cleanup_candidate.filter(|session| session.lock_state().terminated)
        {
            candidate.shutdown()?;
            let mut state = lock_unpoison(&self.state);
            match state.sessions.get(session_id) {
                Some(current) if Arc::ptr_eq(&current.state, &candidate.state) => {
                    state.sessions.remove(session_id);
                    remove_conversation_session_metadata(&mut state, session_id);
                }
                Some(_) => {
                    return Err(
                        "the browser page was replaced while it was being reopened; try again"
                            .into(),
                    );
                }
                None => {}
            }
        }
        lock_unpoison(&self.state)
            .closed_session_ids
            .remove(session_id);
        Ok(())
    }

    pub(crate) fn navigate_history_as_user(
        &self,
        session_id: &str,
        action: &str,
    ) -> Result<BrowserStatus, String> {
        let session_id = validate_session_id(session_id)?.to_owned();
        let navigation = match action {
            "back" => PendingNavigation::Back,
            "forward" => PendingNavigation::Forward,
            "reload" => PendingNavigation::Reload,
            _ => return Err(format!("未知浏览器历史操作: {action}")),
        };
        let session = self.session(&session_id)?;
        session.with_user_control(|| {
            // Preserve the existing trusted-UI ownership semantics even when validation or
            // capacity admission fails, but avoid evicting an unrelated page for an invalid
            // Back/Forward request.
            validate_history_navigation_before_capacity(&session.status(), navigation)?;
            let _manager_lifecycle = lock_unpoison(&self.lifecycle);
            let _reservation = self.reserve_live_slot_locked(&session_id, &session)?;
            self.touch_session(&session_id);
            let result = session.navigate_history_with_resume(navigation);
            self.touch_session(&session_id);
            result
        })
    }

    #[cfg(all(feature = "browser-dev", not(test)))]
    pub(crate) async fn execute_tool(
        &self,
        session_id: String,
        tool: PreviewTool,
        input: Map<String, Value>,
        grants: BrowserToolGrants,
    ) -> Result<Value, String> {
        let runtime = self.clone();
        let output = tauri::async_runtime::spawn_blocking(move || {
            runtime.execute_tool_blocking(&session_id, tool, &input, &grants)
        })
        .await
        .map_err(|error| format!("browser tool background task failed: {error}"))??;
        Ok(match output {
            PreviewToolOutput::Text(text) => Value::String(text),
            PreviewToolOutput::Image(capture) => serde_json::to_value(capture)
                .map_err(|error| format!("browser tool image could not be encoded: {error}"))?,
        })
    }

    /// Legacy trusted-open wrapper that mints its own epoch. Renderer IPC opens through the
    /// mount-bound intent path, so this stays as the lifecycle tests' plain entry point.
    #[cfg(test)]
    pub fn show(&self, session_id: &str, url: Option<&str>) -> Result<BrowserStatus, String> {
        let session_id = validate_session_id(session_id)?.to_owned();
        let _lifecycle = lock_unpoison(&self.lifecycle);
        let epoch = {
            let state = lock_unpoison(&self.state);
            next_browser_lifecycle_epoch(&state, &session_id)?
        };
        self.show_with_intent_locked(&session_id, url, epoch)
    }

    /// Opens one exact conversation only if `epoch` is not stale relative to native lifecycle
    /// authority. Epochs are positive JavaScript-safe integers and are scoped per exact session ID.
    #[cfg(test)]
    pub fn show_with_intent(
        &self,
        session_id: &str,
        url: Option<&str>,
        epoch: u64,
    ) -> Result<BrowserStatus, String> {
        let session_id = validate_session_id(session_id)?.to_owned();
        validate_browser_lifecycle_epoch(epoch)?;
        let _lifecycle = lock_unpoison(&self.lifecycle);
        self.show_with_intent_locked(&session_id, url, epoch)
    }

    /// Renderer-origin variant that atomically binds a newly visible MainPanel
    /// surface to the exact renderer mount generation.
    ///
    /// The manager lifecycle lock spans both the native show and ownership
    /// publication. A delayed orphan cleanup therefore either hides the old
    /// surface before this call, or observes this newer generation and skips
    /// it; there is no unowned visible interval between the two.
    pub(crate) fn show_with_renderer_mount_intent(
        &self,
        session_id: &str,
        url: Option<&str>,
        epoch: u64,
        renderer_mount_generation: u64,
    ) -> Result<BrowserStatus, String> {
        let session_id = validate_session_id(session_id)?.to_owned();
        validate_browser_lifecycle_epoch(epoch)?;
        validate_browser_lifecycle_epoch(renderer_mount_generation)?;
        let _lifecycle = lock_unpoison(&self.lifecycle);
        let status = self.show_with_intent_locked(&session_id, url, epoch)?;
        if status.open {
            self.bind_renderer_presentation_locked(&session_id, renderer_mount_generation);
        }
        Ok(status)
    }

    /// The manager lifecycle mutex must remain held for this complete method. This makes an older
    /// open unable to publish after a newer close has already observed the session as absent.
    fn show_with_intent_locked(
        &self,
        session_id: &str,
        url: Option<&str>,
        epoch: u64,
    ) -> Result<BrowserStatus, String> {
        let desired = BrowserSessionLifecycleDesired::Open;
        let (publish, cleanup_candidate) = {
            let state = lock_unpoison(&self.state);
            if state.shutting_down {
                return Err("应用正在退出，不能再创建浏览器页面".into());
            }
            let publish = classify_browser_lifecycle_intent(
                state.lifecycle_intents.get(session_id).copied(),
                BrowserSessionLifecycleIntent { epoch, desired },
            )?;
            let cleanup_candidate = publish
                .then(|| state.sessions.get(session_id).cloned())
                .flatten();
            (publish, cleanup_candidate)
        };

        if !publish {
            // Duplicate delivery of an already-accepted Open epoch is observational only. In
            // particular, it cannot navigate to a different URL, allocate a missing session, or
            // consume geometry; a caller retrying a failed native open must issue a newer intent.
            let current = lock_unpoison(&self.state).sessions.get(session_id).cloned();
            return Ok(current.map(|session| session.status()).unwrap_or_default());
        }

        // A failed close deliberately keeps the terminated handle until native labels are known to
        // be reusable. A newer open retries that exact cleanup before publishing Open; on failure
        // the old Closed intent and tombstone remain authoritative and the caller may retry.
        if let Some(cleanup_candidate) =
            cleanup_candidate.filter(|session| session.lock_state().terminated)
        {
            cleanup_candidate.shutdown()?;
            let mut state = lock_unpoison(&self.state);
            match state.sessions.get(session_id) {
                Some(current) if Arc::ptr_eq(&current.state, &cleanup_candidate.state) => {
                    state.sessions.remove(session_id);
                    remove_conversation_session_metadata(&mut state, session_id);
                }
                Some(_) => {
                    return Err("重新打开内置浏览器时会话已被更新；拒绝替换不匹配页面".into())
                }
                None => {}
            }
        }

        let pending_bounds = {
            let mut state = lock_unpoison(&self.state);
            state.lifecycle_intents.insert(
                session_id.to_owned(),
                BrowserSessionLifecycleIntent { epoch, desired },
            );
            state.closed_session_ids.remove(session_id);
            state
                .pending_panel_bounds
                .remove(session_id)
                .filter(|pending| pending.epoch == epoch)
                .map(|pending| pending.bounds)
        };
        let session = self.session(session_id)?;
        if let Some(bounds) = pending_bounds {
            if let Err(error) = session.set_panel_bounds(bounds) {
                lock_unpoison(&self.state).pending_panel_bounds.insert(
                    session_id.to_owned(),
                    PendingBrowserPanelBounds { epoch, bounds },
                );
                return Err(error);
            }
        }
        let _reservation = self.reserve_live_slot_locked(session_id, &session)?;
        let (previous_id, previous) = {
            let state = lock_unpoison(&self.state);
            let previous_id = state.active_session_id.clone();
            let previous = previous_id
                .as_ref()
                .and_then(|id| state.sessions.get(id))
                .cloned();
            (previous_id, previous)
        };
        let previous = previous.filter(|previous| previous.session_id.as_ref() != session_id);
        let previous_was_open = previous
            .as_ref()
            .is_some_and(|previous| previous.status().open);
        if previous_was_open {
            if let Some(previous) = previous.as_ref() {
                previous.hide()?;
            }
        }
        match session.open(url) {
            Ok(status) => {
                let mut state = lock_unpoison(&self.state);
                state.active_session_id = status.open.then(|| session_id.to_owned());
                touch_manager_state(&mut state, session_id);
                Ok(status)
            }
            Err(error) => {
                let _ = session.hide();
                let restore_error = if previous_was_open {
                    previous
                        .as_ref()
                        .and_then(|previous| previous.open(None).err())
                } else {
                    None
                };
                let mut state = lock_unpoison(&self.state);
                state.active_session_id = if previous_was_open && restore_error.is_none() {
                    previous_id
                } else {
                    None
                };
                if let Some(restore_error) = restore_error {
                    Err(format!(
                        "{error}；同时无法恢复先前的浏览器页面: {restore_error}"
                    ))
                } else {
                    Err(error)
                }
            }
        }
    }

    /// Legacy trusted-hide wrapper. Renderer IPC supplies its own monotonic epoch through
    /// [`Self::hide_with_intent`]; this epoch-minting shape is used by the lifecycle tests.
    #[cfg(test)]
    pub fn hide(&self, session_id: &str) -> Result<BrowserStatus, String> {
        let session_id = validate_session_id(session_id)?.to_owned();
        let _lifecycle = lock_unpoison(&self.lifecycle);
        let epoch = {
            let state = lock_unpoison(&self.state);
            next_browser_lifecycle_epoch(&state, &session_id)?
        };
        self.hide_with_intent_locked(&session_id, epoch)
    }

    /// Hides one exact conversation without revoking its browser/tool authority. Unlike Closed,
    /// Hidden retains the session and profile, but an older renderer can no longer show it again.
    pub fn hide_with_intent(&self, session_id: &str, epoch: u64) -> Result<BrowserStatus, String> {
        let session_id = validate_session_id(session_id)?.to_owned();
        validate_browser_lifecycle_epoch(epoch)?;
        let _lifecycle = lock_unpoison(&self.lifecycle);
        self.hide_with_intent_locked(&session_id, epoch)
    }

    fn hide_with_intent_locked(
        &self,
        session_id: &str,
        epoch: u64,
    ) -> Result<BrowserStatus, String> {
        let desired = BrowserSessionLifecycleDesired::Hidden;
        let (publish, session) = {
            let state = lock_unpoison(&self.state);
            if state.shutting_down {
                return Err("应用正在退出，不能再隐藏浏览器页面".into());
            }
            let current = state.lifecycle_intents.get(session_id).copied();
            let publish = classify_browser_lifecycle_intent(
                current,
                BrowserSessionLifecycleIntent { epoch, desired },
            )?;
            if state.closed_session_ids.contains(session_id)
                || (publish
                    && current.is_some_and(|intent| {
                        intent.desired == BrowserSessionLifecycleDesired::Closed
                    }))
            {
                return Err("内置浏览器标签页已关闭；必须先显式打开，不能直接切换为隐藏".into());
            }
            (publish, state.sessions.get(session_id).cloned())
        };

        if publish {
            let mut state = lock_unpoison(&self.state);
            state.lifecycle_intents.insert(
                session_id.to_owned(),
                BrowserSessionLifecycleIntent { epoch, desired },
            );
            state.pending_panel_bounds.remove(session_id);
        }

        let Some(session) = session else {
            return Ok(BrowserStatus::default());
        };
        // Repeating the exact Hidden epoch deliberately reaches the native page again. It is the
        // compensation path after a transient hide failure, not a navigation-capable replay.
        let status = session.hide()?;
        let mut state = lock_unpoison(&self.state);
        if state.active_session_id.as_deref() == Some(session_id) {
            state.active_session_id = None;
        }
        touch_manager_state(&mut state, session_id);
        Ok(status)
    }

    /// Permanently closes one conversation tab. Unlike `hide` or `suspend`, this removes the
    /// in-memory session and destroys its WebView; reopening the same conversation creates a fresh
    /// tab while continuing to use the conversation's stable on-disk Chromium profile.
    /// Legacy trusted-close wrapper. Renderer IPC closes through
    /// [`Self::close_after_accepted_intent`]; this shape is used by the lifecycle tests.
    #[cfg(test)]
    pub fn close(&self, session_id: &str) -> Result<BrowserStatus, String> {
        let session_id = validate_session_id(session_id)?.to_owned();
        let (epoch, session) = {
            let _lifecycle = lock_unpoison(&self.lifecycle);
            let epoch = {
                let state = lock_unpoison(&self.state);
                next_browser_lifecycle_epoch(&state, &session_id)?
            };
            let session = self.begin_close_with_intent_locked(&session_id, epoch)?;
            (epoch, session)
        };
        self.finish_close_with_intent(&session_id, epoch, session)
    }

    /// Atomically publishes Closed and acquires a caller-owned secondary fence.
    ///
    /// Browser import cancellation/revocation has its own registry lock. Running its guard
    /// acquisition inside this manager lifecycle critical section gives both authorities one
    /// total order: an older Close rejected after a newer Open has no import side effects, while a
    /// valid Close publishes its tombstone before a newer Open may proceed. The returned value may
    /// outlive this mutex and be used by the caller while waiting for in-flight work to drain.
    pub(crate) fn with_close_intent_fence<T>(
        &self,
        session_id: &str,
        epoch: u64,
        fence: impl FnOnce() -> Result<T, String>,
    ) -> Result<T, String> {
        let session_id = validate_session_id(session_id)?.to_owned();
        validate_browser_lifecycle_epoch(epoch)?;
        let _lifecycle = lock_unpoison(&self.lifecycle);
        let rollback = {
            let state = lock_unpoison(&self.state);
            BrowserCloseIntentRollback {
                lifecycle_intent: state.lifecycle_intents.get(&session_id).copied(),
                closed_tombstone: state.closed_session_ids.contains(&session_id),
                pending_bounds: state.pending_panel_bounds.get(&session_id).copied(),
            }
        };
        let _ = self.begin_close_with_intent_locked(&session_id, epoch)?;
        match fence() {
            Ok(value) => Ok(value),
            Err(error) => {
                let expected = BrowserSessionLifecycleIntent {
                    epoch,
                    desired: BrowserSessionLifecycleDesired::Closed,
                };
                let mut state = lock_unpoison(&self.state);
                // The manager lifecycle lock currently makes replacement impossible here. Keep
                // the identity check anyway: a future secondary fence may deliberately hand off
                // lifecycle ownership, and rollback must never overwrite its newer winner.
                if state.lifecycle_intents.get(&session_id).copied() == Some(expected) {
                    match rollback.lifecycle_intent {
                        Some(intent) => {
                            state.lifecycle_intents.insert(session_id.clone(), intent);
                        }
                        None => {
                            state.lifecycle_intents.remove(&session_id);
                        }
                    }
                    if rollback.closed_tombstone {
                        state.closed_session_ids.insert(session_id.clone());
                    } else {
                        state.closed_session_ids.remove(&session_id);
                    }
                    match rollback.pending_bounds {
                        Some(bounds) => {
                            state.pending_panel_bounds.insert(session_id, bounds);
                        }
                        None => {
                            state.pending_panel_bounds.remove(&session_id);
                        }
                    }
                }
                Err(error)
            }
        }
    }

    /// Publishes a native close fence for one exact conversation generation. Repeating the same
    /// Closed epoch is an idempotent cleanup retry; older epochs and same-epoch Open/Closed
    /// collisions are rejected without touching the page or tombstone.
    pub fn close_with_intent(&self, session_id: &str, epoch: u64) -> Result<BrowserStatus, String> {
        let session_id = validate_session_id(session_id)?.to_owned();
        validate_browser_lifecycle_epoch(epoch)?;
        let session = {
            let _lifecycle = lock_unpoison(&self.lifecycle);
            self.begin_close_with_intent_locked(&session_id, epoch)?
        };
        self.finish_close_with_intent(&session_id, epoch, session)
    }

    /// Completes native teardown after the caller has already accepted and fenced this exact
    /// Closed intent. Native error text stays backend-only; a failed destroy is followed by an
    /// exact-generation hide compensation and returned as a finite, renderer-safe disposition.
    pub(crate) fn close_after_accepted_intent(
        &self,
        session_id: &str,
        epoch: u64,
    ) -> BrowserCloseDisposition {
        match self.close_with_intent(session_id, epoch) {
            Ok(_) if self.close_intent_cleanup_complete(session_id, epoch) => {
                BrowserCloseDisposition::closed()
            }
            Ok(_) => BrowserCloseDisposition::lifecycle_superseded(),
            Err(_) => match self.best_effort_hide_closed_surface(session_id, epoch) {
                ClosedSurfaceHideOutcome::Hidden => {
                    BrowserCloseDisposition::native_cleanup_failed(true)
                }
                ClosedSurfaceHideOutcome::Failed => {
                    BrowserCloseDisposition::native_cleanup_failed(false)
                }
                ClosedSurfaceHideOutcome::Superseded => {
                    BrowserCloseDisposition::lifecycle_superseded()
                }
            },
        }
    }

    fn close_intent_cleanup_complete(&self, session_id: &str, epoch: u64) -> bool {
        let expected = BrowserSessionLifecycleIntent {
            epoch,
            desired: BrowserSessionLifecycleDesired::Closed,
        };
        let state = lock_unpoison(&self.state);
        state.lifecycle_intents.get(session_id).copied() == Some(expected)
            && state.closed_session_ids.contains(session_id)
            && !state.sessions.contains_key(session_id)
    }

    /// Hides only the retained native surface governed by this exact Closed generation. Holding
    /// the manager lifecycle lock across the native call prevents a newer Open from being hidden
    /// by a late cleanup compensation.
    fn best_effort_hide_closed_surface(
        &self,
        session_id: &str,
        epoch: u64,
    ) -> ClosedSurfaceHideOutcome {
        let Ok(session_id) = validate_session_id(session_id) else {
            return ClosedSurfaceHideOutcome::Failed;
        };
        if validate_browser_lifecycle_epoch(epoch).is_err() {
            return ClosedSurfaceHideOutcome::Failed;
        }
        let _lifecycle = lock_unpoison(&self.lifecycle);
        let expected = BrowserSessionLifecycleIntent {
            epoch,
            desired: BrowserSessionLifecycleDesired::Closed,
        };
        let session = {
            let state = lock_unpoison(&self.state);
            if state.lifecycle_intents.get(session_id).copied() != Some(expected)
                || !state.closed_session_ids.contains(session_id)
            {
                return ClosedSurfaceHideOutcome::Superseded;
            }
            state.sessions.get(session_id).cloned()
        };
        let Some(session) = session else {
            return ClosedSurfaceHideOutcome::Hidden;
        };
        if session.hide().is_err() {
            return ClosedSurfaceHideOutcome::Failed;
        }
        let mut state = lock_unpoison(&self.state);
        if state.lifecycle_intents.get(session_id).copied() != Some(expected) {
            return ClosedSurfaceHideOutcome::Superseded;
        }
        if state.active_session_id.as_deref() == Some(session_id) {
            state.active_session_id = None;
        }
        ClosedSurfaceHideOutcome::Hidden
    }

    /// The caller holds the manager lifecycle mutex. Publish Closed before looking up the exact
    /// native handle so even a close of an absent session fences a late older open.
    fn begin_close_with_intent_locked(
        &self,
        session_id: &str,
        epoch: u64,
    ) -> Result<Option<BrowserSession>, String> {
        let desired = BrowserSessionLifecycleDesired::Closed;
        let mut state = lock_unpoison(&self.state);
        if state.shutting_down {
            return Err("应用正在退出，不能再关闭浏览器页面".into());
        }
        let publish = classify_browser_lifecycle_intent(
            state.lifecycle_intents.get(session_id).copied(),
            BrowserSessionLifecycleIntent { epoch, desired },
        )?;
        if publish {
            state.lifecycle_intents.insert(
                session_id.to_owned(),
                BrowserSessionLifecycleIntent { epoch, desired },
            );
        }
        state.closed_session_ids.insert(session_id.to_owned());
        state.pending_panel_bounds.remove(session_id);
        Ok(state.sessions.get(session_id).cloned())
    }

    fn finish_close_with_intent(
        &self,
        session_id: &str,
        epoch: u64,
        session: Option<BrowserSession>,
    ) -> Result<BrowserStatus, String> {
        let Some(session) = session else {
            return Ok(BrowserStatus::default());
        };

        // Match trusted user operations: let an atomic Agent action finish, then prevent any
        // further work on this exact session before its native page is destroyed.
        let _automation = lock_unpoison(&session.automation);
        let _lifecycle = lock_unpoison(&self.lifecycle);
        let expected_intent = BrowserSessionLifecycleIntent {
            epoch,
            desired: BrowserSessionLifecycleDesired::Closed,
        };
        {
            let state = lock_unpoison(&self.state);
            if state.lifecycle_intents.get(session_id).copied() != Some(expected_intent) {
                // A newer Open or Close won while this request waited for an atomic Agent action.
                // That newer owner alone may create or destroy the current exact session.
                return Ok(BrowserStatus::default());
            }
            let Some(current) = state.sessions.get(session_id) else {
                return Ok(BrowserStatus::default());
            };
            if !Arc::ptr_eq(&current.state, &session.state) {
                return Err("关闭内置浏览器时会话已被更新；拒绝销毁不匹配页面".into());
            }
        }
        // Keep the exact manager entry until native destruction and label release are confirmed.
        // If shutdown fails, a later close can retry the same handle instead of orphaning it.
        session.shutdown()?;
        {
            let mut state = lock_unpoison(&self.state);
            if state.lifecycle_intents.get(session_id).copied() != Some(expected_intent) {
                return Err("关闭内置浏览器后生命周期意图发生替换；拒绝移除新页面".into());
            }
            match state.sessions.get(session_id) {
                Some(current) if Arc::ptr_eq(&current.state, &session.state) => {
                    state.sessions.remove(session_id);
                }
                Some(_) => {
                    return Err("关闭内置浏览器后会话发生替换；拒绝移除新页面".into());
                }
                None => return Ok(BrowserStatus::default()),
            }
            remove_conversation_session_metadata(&mut state, session_id);
        }
        // Closing the tab is what destroys its sign-in state: the single-use profile dies here.
        // A Windows file-lock tail that outlasts the bounded retries is written to diagnostics and
        // finished by the next startup sweep — the per-session nonce guarantees no later session
        // can ever adopt the leftover directory in the meantime.
        self.remove_tab_profile_best_effort(&session);
        Ok(BrowserStatus::default())
    }

    /// Deletes one closed ordinary session's single-use profile directory, reporting—not
    /// propagating—failure.
    fn remove_tab_profile_best_effort(&self, session: &BrowserSession) {
        if session.labels.profile_root != TAB_BROWSER_PROFILE_ROOT {
            return;
        }
        let app = { lock_unpoison(&self.state).app.clone() };
        let Some(app) = app else {
            return;
        };
        if let Err(error) = tab_profile_root(&app)
            .and_then(|root| remove_profile_with_retries(&root, &session.labels.profile))
        {
            eprintln!("清理标签页浏览器配置失败，将在下次启动重试: {error}");
        }
    }

    /// Explicit trusted-user release. Unlike automatic LRU eviction this may suspend a
    /// user-controlled or credential-protected page, but it first waits for the current atomic
    /// Agent action to finish. Closing the WebView discards all live password fields.
    pub fn suspend(&self, session_id: &str) -> Result<BrowserStatus, String> {
        let session_id = validate_session_id(session_id)?.to_owned();
        let session = self.session(&session_id)?;
        let _automation = lock_unpoison(&session.automation);
        let _manager_lifecycle = lock_unpoison(&self.lifecycle);
        let _session_lifecycle = session.lock_lifecycle();
        let status = session.suspend_page_locked()?;
        let mut state = lock_unpoison(&self.state);
        if state.active_session_id.as_deref() == Some(session_id.as_str()) {
            state.active_session_id = None;
        }
        touch_manager_state(&mut state, &session_id);
        Ok(status)
    }

    /// Synchronizes a conversation's native page with the trusted React sidebar container.
    /// This never creates a remote page: callers may safely publish layout before `browser_open`.
    #[cfg(test)]
    pub fn set_panel_bounds(
        &self,
        session_id: &str,
        bounds: BrowserPanelBounds,
    ) -> Result<BrowserStatus, String> {
        let bounds = validate_browser_panel_bounds(bounds)?;
        let session_id = validate_session_id(session_id)?.to_owned();
        let _lifecycle = lock_unpoison(&self.lifecycle);
        let epoch = {
            let state = lock_unpoison(&self.state);
            state
                .lifecycle_intents
                .get(&session_id)
                .map(|intent| intent.epoch)
                .or_else(|| {
                    state
                        .pending_panel_bounds
                        .get(&session_id)
                        .map(|pending| pending.epoch)
                })
                .unwrap_or(1)
        };
        self.set_panel_bounds_with_intent_locked(&session_id, bounds, epoch)
    }

    /// Publishes geometry only for the exact accepted Open/Hidden generation. Layout is never
    /// allowed to advance lifecycle authority or use `visible` as a side-channel around hide/close.
    #[cfg(test)]
    pub fn set_panel_bounds_with_intent(
        &self,
        session_id: &str,
        bounds: BrowserPanelBounds,
        epoch: u64,
    ) -> Result<BrowserStatus, String> {
        let bounds = validate_browser_panel_bounds(bounds)?;
        let session_id = validate_session_id(session_id)?.to_owned();
        validate_browser_lifecycle_epoch(epoch)?;
        let _lifecycle = lock_unpoison(&self.lifecycle);
        self.set_panel_bounds_with_intent_locked(&session_id, bounds, epoch)
    }

    /// Renderer-origin layout publication with the same atomic presentation
    /// ownership rule as [`Self::show_with_renderer_mount_intent`].
    pub(crate) fn set_panel_bounds_with_renderer_mount_intent(
        &self,
        session_id: &str,
        bounds: BrowserPanelBounds,
        epoch: u64,
        renderer_mount_generation: u64,
    ) -> Result<BrowserStatus, String> {
        let bounds = validate_browser_panel_bounds(bounds)?;
        let session_id = validate_session_id(session_id)?.to_owned();
        validate_browser_lifecycle_epoch(epoch)?;
        validate_browser_lifecycle_epoch(renderer_mount_generation)?;
        let _lifecycle = lock_unpoison(&self.lifecycle);
        let status = self.set_panel_bounds_with_intent_locked(&session_id, bounds, epoch)?;
        if status.open {
            self.bind_renderer_presentation_locked(&session_id, renderer_mount_generation);
        }
        Ok(status)
    }

    fn set_panel_bounds_with_intent_locked(
        &self,
        session_id: &str,
        bounds: BrowserPanelBounds,
        epoch: u64,
    ) -> Result<BrowserStatus, String> {
        let session = {
            let mut state = lock_unpoison(&self.state);
            if state.shutting_down {
                return Err("应用正在退出，不能再同步浏览器布局".into());
            }
            let current = state.lifecycle_intents.get(session_id).copied();
            let Some(current) = current else {
                if state.closed_session_ids.contains(session_id) {
                    return Ok(BrowserStatus::default());
                }
                if !bounds.visible {
                    return Err(MISMATCHED_BROWSER_PANEL_VISIBILITY_ERROR.to_owned());
                }
                if let Some(pending) = state.pending_panel_bounds.get(session_id) {
                    if epoch < pending.epoch {
                        return Err(STALE_BROWSER_LIFECYCLE_INTENT_ERROR.to_owned());
                    }
                }
                // React layout can beat its matching explicit Open IPC. Keep only a generation-
                // bound candidate; it has no authority to allocate or show a native page.
                state.pending_panel_bounds.insert(
                    session_id.to_owned(),
                    PendingBrowserPanelBounds { epoch, bounds },
                );
                return Ok(BrowserStatus::default());
            };

            if epoch < current.epoch {
                return Err(STALE_BROWSER_LIFECYCLE_INTENT_ERROR.to_owned());
            }
            if epoch > current.epoch {
                if current.desired == BrowserSessionLifecycleDesired::Hidden
                    && bounds.visible
                    && !state.closed_session_ids.contains(session_id)
                {
                    if let Some(pending) = state.pending_panel_bounds.get(session_id) {
                        if epoch < pending.epoch {
                            return Err(STALE_BROWSER_LIFECYCLE_INTENT_ERROR.to_owned());
                        }
                    }
                    // A restored renderer's child layout effect may beat its parent Open IPC.
                    // Retain exact geometry only; Hidden remains authoritative and the native page
                    // is untouched until Open accepts this same epoch.
                    state.pending_panel_bounds.insert(
                        session_id.to_owned(),
                        PendingBrowserPanelBounds { epoch, bounds },
                    );
                    return Ok(BrowserStatus::default());
                }
                return Err(FUTURE_BROWSER_PANEL_BOUNDS_ERROR.to_owned());
            }
            match current.desired {
                BrowserSessionLifecycleDesired::Open if !bounds.visible => {
                    return Err(MISMATCHED_BROWSER_PANEL_VISIBILITY_ERROR.to_owned())
                }
                BrowserSessionLifecycleDesired::Hidden if bounds.visible => {
                    return Err(MISMATCHED_BROWSER_PANEL_VISIBILITY_ERROR.to_owned())
                }
                BrowserSessionLifecycleDesired::Closed => return Ok(BrowserStatus::default()),
                BrowserSessionLifecycleDesired::Open | BrowserSessionLifecycleDesired::Hidden => {}
            }
            if state.closed_session_ids.contains(session_id) {
                return Ok(BrowserStatus::default());
            }
            match state.sessions.get(session_id).cloned() {
                Some(session) => session,
                None => {
                    state.pending_panel_bounds.insert(
                        session_id.to_owned(),
                        PendingBrowserPanelBounds { epoch, bounds },
                    );
                    return Ok(BrowserStatus::default());
                }
            }
        };

        let target_status = session.status();
        if bounds.visible && !target_status.has_page {
            // Layout publication must remain side-effect free for never-opened and suspended
            // sessions. `browser_open` owns restoration and capacity admission.
            return session.set_panel_bounds(bounds);
        }

        let (previous_id, previous) = {
            let state = lock_unpoison(&self.state);
            let previous_id = state.active_session_id.clone();
            let previous = previous_id
                .as_ref()
                .and_then(|id| state.sessions.get(id))
                .cloned();
            (previous_id, previous)
        };
        let previous = previous.filter(|previous| previous.session_id.as_ref() != session_id);
        let previous_was_open = bounds.visible
            && previous
                .as_ref()
                .is_some_and(|previous| previous.status().open);
        if previous_was_open {
            if let Some(previous) = previous.as_ref() {
                previous.hide()?;
            }
        }

        match session.set_panel_bounds(bounds) {
            Ok(status) => {
                let mut state = lock_unpoison(&self.state);
                if status.open {
                    state.active_session_id = Some(session_id.to_owned());
                } else if state.active_session_id.as_deref() == Some(session_id) {
                    state.active_session_id = None;
                }
                touch_manager_state(&mut state, session_id);
                Ok(status)
            }
            Err(error) => {
                let _ = session.hide();
                let restore_error = if previous_was_open {
                    previous
                        .as_ref()
                        .and_then(|previous| previous.open(None).err())
                } else {
                    None
                };
                let mut state = lock_unpoison(&self.state);
                state.active_session_id = if previous_was_open && restore_error.is_none() {
                    previous_id
                } else if previous_id.as_deref() != Some(session_id) {
                    previous_id
                } else {
                    None
                };
                if let Some(restore_error) = restore_error {
                    Err(format!(
                        "{error}；同时无法恢复先前的浏览器页面: {restore_error}"
                    ))
                } else {
                    Err(error)
                }
            }
        }
    }

    /// Records presentation ownership while the caller holds the manager
    /// lifecycle mutex.
    fn bind_renderer_presentation_locked(&self, session_id: &str, renderer_mount_generation: u64) {
        let session = lock_unpoison(&self.state).sessions.get(session_id).cloned();
        let Some(session) = session else {
            return;
        };
        let mut state = session.lock_state();
        if state.host == Some(BrowserHost::MainPanel) && state.status.open {
            state.renderer_presentation_generation = Some(renderer_mount_generation);
        }
    }

    /// Presentation-only fail-safe used when a trusted renderer document is
    /// replaced or misses its heartbeat.
    ///
    /// It hides every actually visible conversation MainPanel WebView owned by
    /// an invalidated renderer at or below `hide_through_generation`. It never
    /// mutates the conversation's Open/Hidden/Closed intent, close tombstone,
    /// profile, pending layout, or import authority. Holding the manager
    /// lifecycle mutex across selection and native hide makes a delayed old
    /// cleanup unable to hide a surface already claimed by a newer renderer.
    pub(crate) fn fail_safe_hide_renderer_presentations_through(
        &self,
        hide_through_generation: u64,
    ) -> Result<usize, String> {
        if hide_through_generation > MAX_BROWSER_LIFECYCLE_EPOCH {
            return Err("浏览器 renderer presentation generation 无效".to_owned());
        }
        let _lifecycle = lock_unpoison(&self.lifecycle);
        self.fail_safe_hide_locked(hide_through_generation)
    }

    /// Main-thread-safe variant that never waits for the lifecycle mutex.
    ///
    /// A native browser mutation holds that mutex while it dispatches Win32 and
    /// WebView work to the main thread and waits for the answer. Blocking the
    /// main thread here would deadlock both sides permanently, because the
    /// Tauri getters used by those mutations wait without a timeout. `Ok(None)`
    /// means the caller must have the fail-safe retried off the main thread.
    pub(crate) fn try_fail_safe_hide_renderer_presentations_through(
        &self,
        hide_through_generation: u64,
    ) -> Result<Option<usize>, String> {
        if hide_through_generation > MAX_BROWSER_LIFECYCLE_EPOCH {
            return Err("浏览器 renderer presentation generation 无效".to_owned());
        }
        let Some(_lifecycle) = try_lock_unpoison(&self.lifecycle) else {
            return Ok(None);
        };
        self.fail_safe_hide_locked(hide_through_generation)
            .map(Some)
    }

    fn fail_safe_hide_locked(&self, hide_through_generation: u64) -> Result<usize, String> {
        let sessions = lock_unpoison(&self.state)
            .sessions
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let mut hidden = 0usize;
        let mut hidden_session_ids = Vec::new();
        let mut failures = 0usize;

        for session in sessions {
            let should_hide = {
                let state = session.lock_state();
                state.host == Some(BrowserHost::MainPanel)
                    && state.status.open
                    && state
                        .renderer_presentation_generation
                        .is_none_or(|generation| generation <= hide_through_generation)
            };
            if !should_hide {
                continue;
            }
            match session.hide() {
                Ok(_) => {
                    hidden = hidden.saturating_add(1);
                    hidden_session_ids.push(session.session_id.to_string());
                }
                Err(_) => failures = failures.saturating_add(1),
            }
        }

        let mut state = lock_unpoison(&self.state);
        if state
            .active_session_id
            .as_ref()
            .is_some_and(|session_id| hidden_session_ids.contains(session_id))
        {
            state.active_session_id = None;
        }
        drop(state);

        if failures == 0 {
            Ok(hidden)
        } else {
            Err(format!(
                "有 {failures} 个内置浏览器原生页面未能完成 renderer presentation fail-safe"
            ))
        }
    }

    pub fn status(&self, session_id: &str) -> BrowserStatus {
        let session = lock_unpoison(&self.state).sessions.get(session_id).cloned();
        session.map(|session| session.status()).unwrap_or_default()
    }

    /// Releases every conversation WebView before the main React WebView exits. The shared engine
    /// then has no remaining application-owned pages and is terminated by Tauri with the process.
    pub fn shutdown_all(&self) {
        let _lifecycle = lock_unpoison(&self.lifecycle);
        if let Err(error) = self.shutdown_all_locked() {
            eprintln!("退出时未能完整释放浏览器资源: {error}");
        }
    }

    fn shutdown_all_locked(&self) -> Result<(), String> {
        let (sessions, app) = {
            let mut state = lock_unpoison(&self.state);
            state.shutting_down = true;
            state.active_session_id = None;
            state.live_reservations.clear();
            state.last_used.clear();
            state.closed_session_ids.clear();
            state.pending_panel_bounds.clear();
            state.lifecycle_intents.clear();
            let sessions = state
                .sessions
                .drain()
                .map(|(_, session)| session)
                .collect::<Vec<_>>();
            (sessions, state.app.clone())
        };
        let mut errors = Vec::new();
        for session in sessions {
            if let Err(error) = session.shutdown() {
                errors.push(format!("关闭内置浏览器失败: {error}"));
                continue;
            }
            // App exit destroys every tab's sign-in state, exactly like closing the tab would.
            // A directory Windows still holds is finished by the next startup sweep.
            if session.labels.profile_root != TAB_BROWSER_PROFILE_ROOT {
                continue;
            }
            let Some(app) = app.as_ref() else {
                continue;
            };
            if let Err(error) = tab_profile_root(app)
                .and_then(|root| remove_profile_with_retries(&root, &session.labels.profile))
            {
                errors.push(format!(
                    "清理标签页浏览器配置失败，将在下次启动重试: {error}"
                ));
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("；"))
        }
    }

    /// Registers one Environment5 exit handler per real Chromium browser process, closes every
    /// conversation WebView, and waits until each handler observes a normal process exit.
    ///
    /// This is intentionally browser-dev-only. The caller runs on the import-drain worker while
    /// Tauri's main message pump remains alive; every COM interface is acquired, used, and released
    /// inside `with_webview` or the native event callback and never crosses a Rust thread boundary.
    #[cfg(all(windows, feature = "browser-dev"))]
    pub(crate) fn shutdown_all_with_webview2_release_barrier(
        &self,
        timeout: Duration,
        require_browser_process: bool,
    ) -> Result<usize, String> {
        let _lifecycle = lock_unpoison(&self.lifecycle);
        let targets = {
            let state = lock_unpoison(&self.state);
            let app = state
                .app
                .clone()
                .ok_or_else(|| "浏览器运行时尚未连接应用，无法建立 WebView2 释放屏障".to_owned())?;
            let mut sessions = state.sessions.values().cloned().collect::<Vec<_>>();
            sessions.sort_unstable_by(|left, right| left.labels.page.cmp(&right.labels.page));
            sessions.dedup_by(|left, right| left.labels.page == right.labels.page);
            sessions
                .into_iter()
                .filter_map(|session| {
                    app.get_webview(&session.labels.page)
                        .map(|webview| (session, webview))
                })
                .collect::<Vec<_>>()
        };

        let seen_processes = Arc::new(Mutex::new(HashSet::new()));
        let (exit_sender, exit_receiver) = mpsc::channel::<BrowserProcessExitObservation>();
        let mut expected_processes = HashSet::new();
        let mut errors = Vec::new();
        // Keep every Webview handle alive until the Environment5 exit notifications settle.
        // Consuming this vector here drops the last host-side environment handles before
        // `shutdown_all_locked` can deliver `BrowserProcessExited`; the callback senders then
        // disappear and the release barrier observes a disconnected channel instead of an exit.
        for (session, _) in &targets {
            match register_browser_process_exit_handler(
                session,
                seen_processes.clone(),
                exit_sender.clone(),
            ) {
                Ok(Some(process_id)) => {
                    expected_processes.insert(process_id);
                }
                Ok(None) => {}
                Err(error) => errors.push(error),
            }
        }
        drop(exit_sender);

        let process_count = expected_processes.len();
        if require_browser_process && process_count == 0 {
            errors.push("图片输入 E2E WebView2 释放屏障未发现任何实际浏览器进程".to_owned());
        }

        let shutdown_started = Instant::now();
        if let Err(error) = self.shutdown_all_locked() {
            errors.push(error);
        }
        let deadline = shutdown_started + timeout;
        if let Err(error) =
            wait_for_browser_process_exits(exit_receiver, expected_processes, deadline)
        {
            errors.push(error);
        }
        drop(targets);

        if errors.is_empty() {
            Ok(process_count)
        } else {
            Err(errors.join("；"))
        }
    }

    fn touch_session(&self, session_id: &str) {
        touch_manager_state(&mut lock_unpoison(&self.state), session_id);
    }

    fn reserve_live_slot_locked(
        &self,
        session_id: &str,
        session: &BrowserSession,
    ) -> Result<Option<LivePageReservation>, String> {
        if session.status().has_page {
            return Ok(None);
        }
        let target_already_retained = session.has_retained_page();
        {
            let state = lock_unpoison(&self.state);
            if state.live_reservations.contains(session_id) {
                return Err(
                    "this browser task page is being created or restored; try again shortly".into(),
                );
            }
        }

        self.make_awake_slot_locked()?;

        // Waking a native sleeping page does not grow the retained-controller set. A never-opened
        // or cold-suspended task does, so evict the oldest safe sleeping controller first.
        if !target_already_retained {
            let mut cold_close_errors = Vec::new();
            let mut attempted_sleepers = HashSet::new();
            loop {
                let snapshots = self.capacity_snapshots();
                let retained_count = snapshots
                    .iter()
                    .filter(|snapshot| snapshot.retained || snapshot.reserved)
                    .map(|snapshot| snapshot.session_id.as_str())
                    .collect::<HashSet<_>>()
                    .len();
                if retained_count < MAX_RETAINED_BROWSER_PAGES {
                    break;
                }
                let remaining = snapshots
                    .into_iter()
                    .filter(|snapshot| !attempted_sleepers.contains(&snapshot.session_id))
                    .collect::<Vec<_>>();
                let Some(candidate_id) = select_lru_retained_sleeper(&remaining) else {
                    let detail = cold_close_errors
                        .first()
                        .map(|error| format!(" Most recent cold close failed: {error}"))
                        .unwrap_or_default();
                    return Err(format!(
                        "At most {MAX_RETAINED_BROWSER_PAGES} native browser pages may be retained; the existing sleeping pages cannot be safely cold-closed yet.{detail}"
                    ));
                };
                let candidate = {
                    lock_unpoison(&self.state)
                        .sessions
                        .get(&candidate_id)
                        .cloned()
                };
                let Some(candidate) = candidate else {
                    attempted_sleepers.insert(candidate_id);
                    continue;
                };
                match candidate.try_cold_close_sleeping_for_capacity() {
                    Ok(true) => continue,
                    Ok(false) => {}
                    Err(error) => cold_close_errors.push(error),
                }
                attempted_sleepers.insert(candidate_id);
                // A failed cold close may have resumed this controller. Restore an empty awake
                // slot before trying another sleeper; otherwise resuming the next candidate could
                // transiently create a fourth awake page even though no target was admitted yet.
                self.make_awake_slot_locked()?;
            }
        }

        // A retained-page eviction resumes a native sleeper before capturing its Cookie handoff.
        // If capture/close fails but a later sleeper is evicted successfully, that first candidate
        // remains awake. Revalidate after the retained phase so adding the target reservation can
        // never push the manager above the hard awake-page limit.
        self.make_awake_slot_locked()?;

        let mut state = lock_unpoison(&self.state);
        if state.live_reservations.contains(session_id) {
            return Err(
                "this browser task page is being created or restored; try again shortly".into(),
            );
        }
        state.live_reservations.insert(session_id.to_owned());
        Ok(Some(LivePageReservation {
            state: self.state.clone(),
            session_id: session_id.to_owned(),
        }))
    }

    fn make_awake_slot_locked(&self) -> Result<(), String> {
        let mut suspend_errors = Vec::new();
        let mut attempted_candidates = HashSet::new();
        loop {
            // An existing reservation may complete or fail without taking the manager lifecycle.
            // Recompute the union every iteration so a failed creation never causes an unnecessary
            // extra eviction, while a successful one remains represented by the same session id.
            let snapshots = self.capacity_snapshots();
            if awake_or_reserved_count(&snapshots) < MAX_AWAKE_BROWSER_PAGES {
                return Ok(());
            }
            let remaining = snapshots
                .into_iter()
                .filter(|snapshot| !attempted_candidates.contains(&snapshot.session_id))
                .collect::<Vec<_>>();
            let Some(candidate_id) = select_lru_candidate(&remaining) else {
                let detail = suspend_errors
                    .first()
                    .map(|error| format!(" Most recent release failed: {error}"))
                    .unwrap_or_default();
                return Err(format!(
                    "At most {MAX_AWAKE_BROWSER_PAGES} browser pages may be awake at once; every current page is visible, loading, controlled by the user or Agent, or credential-protected. Suspend a task page first, then try again.{detail}"
                ));
            };
            let candidate = {
                lock_unpoison(&self.state)
                    .sessions
                    .get(&candidate_id)
                    .cloned()
            };
            let Some(candidate) = candidate else {
                attempted_candidates.insert(candidate_id);
                continue;
            };
            match candidate.try_suspend_for_capacity() {
                Ok(true) => continue,
                Ok(false) => {}
                Err(error) => {
                    suspend_errors.push(error);
                }
            }
            attempted_candidates.insert(candidate_id);
        }
    }

    fn capacity_snapshots(&self) -> Vec<CapacitySnapshot> {
        let (sessions, active, reservations, last_used) = {
            let state = lock_unpoison(&self.state);
            (
                state
                    .sessions
                    .iter()
                    .map(|(id, session)| (id.clone(), session.clone()))
                    .collect::<Vec<_>>(),
                state.active_session_id.clone(),
                state.live_reservations.clone(),
                state.last_used.clone(),
            )
        };
        sessions
            .into_iter()
            .map(|(id, session)| {
                session.capacity_snapshot(
                    id,
                    last_used
                        .get(session.session_id.as_ref())
                        .copied()
                        .unwrap_or_default(),
                    active.as_deref() == Some(session.session_id.as_ref()),
                    reservations.contains(session.session_id.as_ref()),
                )
            })
            .collect()
    }

    #[cfg(any(test, feature = "browser-dev"))]
    pub(crate) fn live_page_count(&self) -> usize {
        self.capacity_snapshots()
            .into_iter()
            .filter(|snapshot| snapshot.has_page || snapshot.reserved)
            .map(|snapshot| snapshot.session_id)
            .collect::<HashSet<_>>()
            .len()
    }
}

fn touch_manager_state(state: &mut BrowserManagerState, session_id: &str) {
    state.access_sequence = state.access_sequence.saturating_add(1);
    state
        .last_used
        .insert(session_id.to_owned(), state.access_sequence);
}

fn remove_conversation_session_metadata(state: &mut BrowserManagerState, session_id: &str) {
    state.live_reservations.remove(session_id);
    state.last_used.remove(session_id);
    if state.active_session_id.as_deref() == Some(session_id) {
        state.active_session_id = None;
    }
}

fn validate_browser_lifecycle_epoch(epoch: u64) -> Result<u64, String> {
    if !(1..=MAX_BROWSER_LIFECYCLE_EPOCH).contains(&epoch) {
        return Err(format!(
            "浏览器生命周期 epoch 必须是 1 到 {MAX_BROWSER_LIFECYCLE_EPOCH} 之间的安全整数"
        ));
    }
    Ok(epoch)
}

#[cfg(test)]
/// Mints the next lifecycle epoch for one exact session.
///
/// Renderer-origin lifecycle requests carry the epoch the trusted UI issued, so this is only for
/// callers that are themselves the origin of the intent: the Agent's own tab close, and tests. A
/// minted epoch is a proposal, not authority — two callers that mint the same value still meet
/// `classify_browser_lifecycle_intent`, which rejects the colliding second intent.
fn next_browser_lifecycle_epoch(
    state: &BrowserManagerState,
    session_id: &str,
) -> Result<u64, String> {
    let current = state
        .lifecycle_intents
        .get(session_id)
        .map(|intent| intent.epoch)
        .unwrap_or(0);
    let next = current
        .checked_add(1)
        .filter(|epoch| *epoch <= MAX_BROWSER_LIFECYCLE_EPOCH)
        .ok_or_else(|| "浏览器生命周期 epoch 已耗尽；请重新启动应用".to_owned())?;
    validate_browser_lifecycle_epoch(next)
}

/// Returns `true` only when the incoming intent must be published. Repeating the exact same
/// desired state is idempotent; an older epoch or a same-epoch opposite state has no side effect.
fn classify_browser_lifecycle_intent(
    current: Option<BrowserSessionLifecycleIntent>,
    incoming: BrowserSessionLifecycleIntent,
) -> Result<bool, String> {
    validate_browser_lifecycle_epoch(incoming.epoch)?;
    let Some(current) = current else {
        return Ok(true);
    };
    if incoming.epoch < current.epoch {
        return Err(STALE_BROWSER_LIFECYCLE_INTENT_ERROR.to_owned());
    }
    if incoming.epoch == current.epoch {
        if incoming.desired == current.desired {
            return Ok(false);
        }
        return Err(COLLIDING_BROWSER_LIFECYCLE_INTENT_ERROR.to_owned());
    }
    Ok(true)
}

fn awake_or_reserved_count(snapshots: &[CapacitySnapshot]) -> usize {
    snapshots
        .iter()
        .filter(|snapshot| snapshot.has_page || snapshot.reserved)
        .map(|snapshot| snapshot.session_id.as_str())
        .collect::<HashSet<_>>()
        .len()
}

fn navigation_resume_plan(
    status: &BrowserStatus,
    retained: bool,
    navigation: PendingNavigation,
) -> NavigationResumePlan {
    if !status.suspended {
        NavigationResumePlan::Continue
    } else if retained {
        NavigationResumePlan::ResumeNativeThenContinue
    } else if navigation == PendingNavigation::Reload {
        NavigationResumePlan::ResumeColdCompletesReload
    } else {
        NavigationResumePlan::ColdHistoryUnavailable
    }
}

fn select_lru_candidate(snapshots: &[CapacitySnapshot]) -> Option<String> {
    snapshots
        .iter()
        .filter(|snapshot| {
            snapshot.has_page
                && !snapshot.open
                && !snapshot.loading
                && !snapshot.pending_navigation
                && !snapshot.occluded
                && snapshot.owner == BrowserControlOwner::Available
                && !snapshot.active
                && !snapshot.reserved
        })
        .min_by(|left, right| {
            (left.last_used, left.session_id.as_str())
                .cmp(&(right.last_used, right.session_id.as_str()))
        })
        .map(|snapshot| snapshot.session_id.clone())
}

fn select_lru_retained_sleeper(snapshots: &[CapacitySnapshot]) -> Option<String> {
    snapshots
        .iter()
        .filter(|snapshot| {
            snapshot.retained
                && snapshot.suspended
                && !snapshot.open
                && !snapshot.loading
                && !snapshot.pending_navigation
                && !snapshot.occluded
                && snapshot.owner == BrowserControlOwner::Available
                && !snapshot.active
                && !snapshot.reserved
        })
        .min_by(|left, right| {
            (left.last_used, left.session_id.as_str())
                .cmp(&(right.last_used, right.session_id.as_str()))
        })
        .map(|snapshot| snapshot.session_id.clone())
}

fn validate_session_id(session_id: &str) -> Result<&str, String> {
    if session_id.trim().is_empty() {
        return Err("a browser session must belong to a conversation".into());
    }
    if session_id.trim() != session_id
        || session_id.len() > 256
        || session_id.chars().any(char::is_control)
    {
        return Err("browser session identifier is invalid".into());
    }
    // `#` separates a conversation from one of its extra tabs. Keeping the suffix single and
    // non-empty means `browser_conversation_owner` always recovers exactly one owning
    // conversation, so no tab session id can be crafted into another conversation's tab roster.
    let mut parts = session_id.split('#');
    parts.next();
    if let Some(tab) = parts.next() {
        if parts.next().is_some() {
            return Err("a browser tab identifier may contain at most one # separator".into());
        }
        if tab.is_empty()
            || !tab
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err("a browser tab identifier may contain only letters, digits, hyphens, and underscores".into());
        }
        if browser_conversation_owner(session_id).is_empty() {
            return Err("a browser tab must belong to a conversation".into());
        }
    }
    Ok(session_id)
}

/// Conversation a session belongs to. Extra tabs carry a `#<token>` suffix; everything before it
/// is the conversation whose tab roster they appear in.
fn browser_conversation_owner(session_id: &str) -> &str {
    session_id
        .split_once('#')
        .map(|(owner, _)| owner)
        .unwrap_or(session_id)
}

/// Tab id the Agent addresses a session by, within its own conversation.
fn browser_tab_id(session_id: &str) -> &str {
    session_id
        .split_once('#')
        .map(|(_, tab)| tab)
        .unwrap_or(PRIMARY_TAB_ID)
}

/// The tab id a user-facing prompt should name, matching what the trusted chrome shows.
pub(crate) fn browser_tab_label(session_id: &str) -> &str {
    browser_tab_id(session_id)
}

/// Page the conversation's preview tools act on.
///
/// One conversation, one page. All this decides is that the caller handed over a conversation
/// rather than one of the trusted UI's extra tab sessions: those are addressed by appending
/// `#<tab>`, and a conversation id already carrying one would silently move the tools onto a
/// surface the tool executor never authorized.
pub(crate) fn preview_page_session_id(conversation_id: &str) -> Result<String, String> {
    let conversation_id = validate_session_id(conversation_id)?;
    if conversation_id.contains('#') {
        return Err(
            "preview tools must be called by the conversation itself, not a tab session".into(),
        );
    }
    Ok(conversation_id.to_owned())
}

fn browser_space_digest(value: &str) -> String {
    Sha256::digest([b"mework-browser-space-v1\0".as_slice(), value.as_bytes()].concat())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn tab_profile_root(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(app
        .path()
        .app_local_data_dir()
        .map_err(|error| format!("无法解析标签页浏览器配置目录: {error}"))?
        .join(TAB_BROWSER_PROFILE_ROOT))
}

fn is_profile_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

#[cfg(windows)]
fn metadata_is_link_or_reparse(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    metadata.file_type().is_symlink()
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn metadata_is_link_or_reparse(metadata: &std::fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

fn verify_profile_root(root: &Path) -> Result<(), String> {
    let metadata = std::fs::symlink_metadata(root)
        .map_err(|error| format!("无法检查联网搜索浏览器配置根目录: {error}"))?;
    if metadata_is_link_or_reparse(&metadata) || !metadata.is_dir() {
        return Err("联网搜索浏览器配置根目录不是安全的真实目录".into());
    }
    Ok(())
}

fn ensure_profile_root(root: &Path) -> Result<(), String> {
    std::fs::create_dir_all(root)
        .map_err(|error| format!("无法创建联网搜索浏览器配置根目录: {error}"))?;
    verify_profile_root(root)
}

fn remove_profile_with_retries(root: &Path, profile: &str) -> Result<(), String> {
    remove_profile_with_retries_before(root, profile, None)
}

fn remove_profile_with_retries_before(
    root: &Path,
    profile: &str,
    deadline: Option<Instant>,
) -> Result<(), String> {
    if !is_profile_hash(profile) {
        return Err("拒绝清理非哈希命名的联网搜索浏览器配置目录".into());
    }
    match std::fs::symlink_metadata(root) {
        Ok(_) => verify_profile_root(root)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("无法检查联网搜索浏览器配置根目录: {error}")),
    }
    let target = root.join(profile);
    if target.parent() != Some(root) {
        return Err("联网搜索浏览器配置清理目标越过了专用根目录".into());
    }

    let mut last_error = None;
    for attempt in 0..RESEARCH_PROFILE_DELETE_ATTEMPTS {
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Err("联网搜索浏览器启动清理已达到重试等待预算，将在下次启动继续".into());
        }
        match std::fs::symlink_metadata(&target) {
            Ok(metadata) => {
                if metadata_is_link_or_reparse(&metadata) || !metadata.is_dir() {
                    return Err("拒绝清理不是安全真实目录的联网搜索浏览器配置目标".into());
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(format!("无法检查联网搜索浏览器配置目录: {error}")),
        }
        match std::fs::remove_dir_all(&target) {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => last_error = Some(error),
        }
        if attempt + 1 < RESEARCH_PROFILE_DELETE_ATTEMPTS {
            let delay = deadline
                .map(|deadline| {
                    deadline
                        .saturating_duration_since(Instant::now())
                        .min(RESEARCH_PROFILE_DELETE_DELAY)
                })
                .unwrap_or(RESEARCH_PROFILE_DELETE_DELAY);
            if delay.is_zero() {
                return Err("联网搜索浏览器启动清理已达到重试等待预算，将在下次启动继续".into());
            }
            std::thread::sleep(delay);
        }
    }
    Err(format!(
        "无法删除联网搜索浏览器临时配置目录（已重试 {RESEARCH_PROFILE_DELETE_ATTEMPTS} 次）: {}",
        last_error
            .map(|error| error.to_string())
            .unwrap_or_else(|| "未知错误".to_owned())
    ))
}

fn cleanup_stale_profiles_in_root(
    root: &Path,
    active_profiles: &HashSet<String>,
) -> Result<(), String> {
    match std::fs::symlink_metadata(root) {
        Ok(_) => verify_profile_root(root)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("无法检查联网搜索浏览器配置根目录: {error}")),
    }

    let mut errors = Vec::new();
    let deadline = Instant::now() + RESEARCH_PROFILE_STARTUP_BUDGET;
    let mut attempted = 0usize;
    let mut deferred = false;
    let entries = std::fs::read_dir(root)
        .map_err(|error| format!("无法枚举联网搜索浏览器配置根目录: {error}"))?;
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                errors.push(format!("无法读取配置目录项: {error}"));
                continue;
            }
        };
        let name = entry.file_name();
        let Some(profile) = name.to_str() else {
            continue;
        };
        if !is_profile_hash(profile) || active_profiles.contains(profile) {
            continue;
        }
        if attempted >= RESEARCH_PROFILE_STARTUP_MAX_DIRECTORIES || Instant::now() >= deadline {
            deferred = true;
            break;
        }
        attempted += 1;
        if let Err(error) = remove_profile_with_retries_before(root, profile, Some(deadline)) {
            errors.push(error);
        }
    }
    if deferred {
        errors.push(format!(
            "联网搜索浏览器启动清理最多处理 {RESEARCH_PROFILE_STARTUP_MAX_DIRECTORIES} 个目录且重试等待软预算为 {} ms；剩余目录将在下次启动继续",
            RESEARCH_PROFILE_STARTUP_BUDGET.as_millis()
        ));
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("；"))
    }
}

/// Every preview tool gets a page (`ensureTab`), so any call on a page-less or suspended session
/// takes an awake slot before it runs.
fn browser_tool_needs_live_slot(status: &BrowserStatus) -> bool {
    !status.has_page
}

/// The argument checks each preview tool performs before touching the page, run up front so a
/// malformed call is refused before a page is created for it. Each check calls the same helper
/// the tool itself uses; the tool repeats it, which keeps the two from drifting apart.
fn validate_preview_input(
    tool: PreviewTool,
    input: &Map<String, Value>,
    grants: &BrowserToolGrants,
) -> Result<(), String> {
    match tool {
        PreviewTool::ConsoleLogs => {
            validate_preview_level(input, &["all", "error", "warn"])?;
            optional_input_u64(input, "lines")?;
        }
        PreviewTool::Screenshot => {
            validate_preview_scale(optional_input_f64(input, "scale")?)?;
        }
        PreviewTool::Snapshot => {}
        PreviewTool::FindElement => {
            required_input_string(input, "query", 2_000, false)?;
        }
        PreviewTool::ClickByDescription => {
            required_input_string(input, "description", 2_000, false)?;
            optional_input_bool(input, "doubleClick", false)?;
        }
        PreviewTool::FillByDescription => {
            required_input_string(input, "description", 2_000, false)?;
            required_input_string(input, "value", MAX_TEXT_INPUT_CHARS, true)?;
        }
        PreviewTool::InspectByDescription => {
            required_input_string(input, "description", 2_000, false)?;
            if let Some(styles) = parse_preview_styles(input)? {
                if styles.len() > MAX_PREVIEW_STYLES {
                    return Err(format!(
                        "preview_inspect_by_description styles accepts at most {MAX_PREVIEW_STYLES} properties"
                    ));
                }
            }
        }
        PreviewTool::Inspect => {
            validate_selector(&required_input_string(
                input,
                "selector",
                MAX_SELECTOR_CHARS,
                false,
            )?)?;
            if let Some(styles) = parse_preview_styles(input)? {
                if styles.len() > MAX_PREVIEW_STYLES {
                    return Err(format!(
                        "preview_inspect styles accepts at most {MAX_PREVIEW_STYLES} properties"
                    ));
                }
            }
        }
        PreviewTool::Click => {
            validate_selector(&required_input_string(
                input,
                "selector",
                MAX_SELECTOR_CHARS,
                false,
            )?)?;
            optional_input_bool(input, "doubleClick", false)?;
        }
        PreviewTool::Fill => {
            validate_selector(&required_input_string(
                input,
                "selector",
                MAX_SELECTOR_CHARS,
                false,
            )?)?;
            // The empty string is how a field is cleared, so it is a value like any other.
            required_input_string(input, "value", MAX_TEXT_INPUT_CHARS, true)?;
        }
        PreviewTool::Eval => {
            let expression = required_input_string(input, "expression", MAX_EVALUATE_CHARS, false)?;
            if expression.trim().is_empty() {
                return Err("preview_eval expression must not be empty".into());
            }
        }
        PreviewTool::Network => {
            validate_preview_level(input, &["all", "failed"])?;
            optional_input_string(input, "requestId", 256)?;
        }
        PreviewTool::Resize => {
            for key in ["width", "height"] {
                if let Some(value) = optional_input_f64(input, key)? {
                    if preview_viewport_dimension(Some(value)).is_none() {
                        return Err(format!(
                            "preview_resize {key} must be a number from 1 to {PREVIEW_VIEWPORT_MAX}"
                        ));
                    }
                }
            }
        }
        PreviewTool::UploadImage => {
            if let Some(selector) = optional_input_string(input, "selector", MAX_SELECTOR_CHARS)? {
                validate_selector(&selector)?;
            }
            if grants.upload_paths.is_none() {
                return Err(
                    "preview_upload_image is missing a host-materialized image file".into(),
                );
            }
        }
        PreviewTool::Dialog => {
            match input.get("accept") {
                None | Some(Value::Null) => {}
                Some(value) => {
                    value
                        .as_bool()
                        .ok_or_else(|| "parameter accept must be a boolean".to_owned())?;
                }
            }
            if optional_input_string(input, "prompt_text", MAX_DIALOG_PROMPT_CHARS + 1)?
                .is_some_and(|text| text.chars().count() > MAX_DIALOG_PROMPT_CHARS)
            {
                return Err(format!(
                    "preview_dialog prompt_text exceeds the {MAX_DIALOG_PROMPT_CHARS}-character limit"
                ));
            }
        }
    }
    Ok(())
}

/// Both enum-valued `level`-shaped parameters (`preview_console_logs.level` and
/// `preview_network.filter`) refuse an unknown member rather than silently reading it as `all`.
fn validate_preview_level(input: &Map<String, Value>, allowed: &[&str]) -> Result<(), String> {
    let key = if allowed.contains(&"warn") {
        "level"
    } else {
        "filter"
    };
    let Some(value) = optional_input_string(input, key, 32)? else {
        return Ok(());
    };
    if allowed.contains(&value.as_str()) {
        return Ok(());
    }
    Err(format!(
        "{key} must be one of {}",
        allowed
            .iter()
            .map(|member| format!("\"{member}\""))
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

/// `preview_inspect.styles`. `None` is the source's ten defaults; an empty array is an explicit
/// request for no computed styles at all, which the source honours.
fn parse_preview_styles(input: &Map<String, Value>) -> Result<Option<Vec<String>>, String> {
    match input.get("styles") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| {
                item.as_str()
                    .filter(|property| !property.trim().is_empty() && property.len() <= 128)
                    .map(str::to_owned)
                    .ok_or_else(|| {
                        "styles may contain only non-empty CSS property names".to_owned()
                    })
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Some),
        Some(_) => Err("styles must be an array of strings".into()),
    }
}

fn validate_history_navigation_before_capacity(
    status: &BrowserStatus,
    navigation: PendingNavigation,
) -> Result<(), String> {
    match navigation {
        PendingNavigation::Back if !status.can_go_back => Err("browser has no back history".into()),
        PendingNavigation::Forward if !status.can_go_forward => {
            Err("browser has no forward history".into())
        }
        PendingNavigation::Reload if !status.has_page && !status.suspended => {
            Err("the embedded browser is not open".into())
        }
        _ => Ok(()),
    }
}

fn validate_browser_panel_bounds(
    mut bounds: BrowserPanelBounds,
) -> Result<BrowserPanelBounds, String> {
    let values = [bounds.x, bounds.y, bounds.width, bounds.height];
    if values.into_iter().any(|value| !value.is_finite())
        || bounds.occluded_top.is_some_and(|value| !value.is_finite())
    {
        return Err("浏览器侧边栏 bounds 必须是有限数字".into());
    }
    if bounds.width < 0.0 || bounds.height < 0.0 {
        return Err("浏览器侧边栏宽高不能为负数".into());
    }
    if bounds.visible && (bounds.width < 1.0 || bounds.height < 1.0) {
        return Err("可见的浏览器侧边栏宽高必须至少为 1 像素".into());
    }

    // Bound even hidden/pre-layout values before retaining them in the long-lived session state.
    // The effective rectangle is clamped again to the current host window at application time.
    bounds.x = bounds
        .x
        .clamp(-MAX_BROWSER_PANEL_VALUE, MAX_BROWSER_PANEL_VALUE);
    bounds.y = bounds
        .y
        .clamp(-MAX_BROWSER_PANEL_VALUE, MAX_BROWSER_PANEL_VALUE);
    bounds.width = bounds.width.min(MAX_BROWSER_PANEL_VALUE);
    bounds.height = bounds.height.min(MAX_BROWSER_PANEL_VALUE);
    bounds.occluded_top = bounds
        .occluded_top
        .map(|value| value.clamp(0.0, MAX_BROWSER_PANEL_VALUE));
    Ok(bounds)
}

impl Default for BrowserSession {
    fn default() -> Self {
        Self::new("default")
    }
}

impl BrowserSession {
    /// Creates an ordinary session in its own single-use WebView2 profile.
    ///
    /// Every tab is a fresh user-data folder: sign-in state created inside the tab stays inside
    /// the tab, survives cold suspend/resume (the folder outlives the native WebView), and dies
    /// with the tab. The folder is chosen when the controller is created and never changes for the
    /// life of the session. Its name mixes a per-session random nonce, so a directory left behind
    /// by a crash can never be re-adopted by a later session with the same id — the startup sweep
    /// deletes such leftovers instead.
    pub(crate) fn new(session_id: &str) -> Self {
        // Labels are native object identity only; the collision-resistant digest keeps two live
        // pages from ever claiming the same Tauri label.
        let session_digest = browser_space_digest(session_id);
        let suffix = &session_digest[..16];
        let profile =
            browser_space_digest(&format!("{session_id}\0{}", uuid::Uuid::new_v4().simple()));
        Self::new_with_boundary(
            session_id,
            BrowserLabels {
                window: format!("{BROWSER_WINDOW_LABEL}-{suffix}"),
                page: format!("{BROWSER_PAGE_LABEL}-{suffix}"),
                profile_root: TAB_BROWSER_PROFILE_ROOT,
                profile,
            },
            BrowserNavigationPolicy::Ordinary,
        )
    }

    fn new_with_boundary(
        session_id: &str,
        labels: BrowserLabels,
        navigation_policy: BrowserNavigationPolicy,
    ) -> Self {
        Self {
            state: Arc::new(Mutex::new(RuntimeState::default())),
            lifecycle: Arc::new(Mutex::new(())),
            automation: Arc::new(Mutex::new(())),
            agent_pointer_overlay: Arc::new(Mutex::new(())),
            agent_pointer_generation: Arc::new(AtomicU64::new(0)),
            labels: Arc::new(labels),
            session_id: Arc::from(session_id),
            navigation_policy: Arc::new(navigation_policy),
        }
    }

    fn capacity_snapshot(
        &self,
        session_id: String,
        last_used: u64,
        active: bool,
        reserved: bool,
    ) -> CapacitySnapshot {
        let status = self.status();
        let retained = self.has_retained_page();
        let state = self.lock_state();
        CapacitySnapshot {
            session_id,
            last_used,
            has_page: status.has_page,
            retained,
            suspended: status.suspended,
            open: status.open,
            loading: status.loading,
            pending_navigation: state.pending_navigation.is_some(),
            occluded: state.occluded,
            owner: state.status.control.owner,
            active,
            reserved,
        }
    }

    fn has_retained_page(&self) -> bool {
        self.lock_state()
            .app
            .as_ref()
            .is_some_and(|app| app.get_webview(&self.labels.page).is_some())
    }

    fn try_suspend_for_capacity(&self) -> Result<bool, String> {
        let Some(_automation) = try_lock_unpoison(&self.automation) else {
            return Ok(false);
        };
        let Some(_lifecycle) = try_lock_unpoison(&self.lifecycle) else {
            return Ok(false);
        };
        {
            let status = self.status();
            let state = self.lock_state();
            if !status.has_page
                || status.open
                || status.loading
                || state.pending_navigation.is_some()
                || state.occluded
                || state.status.control.owner != BrowserControlOwner::Available
            {
                return Ok(false);
            }
        }
        self.sleep_page_locked()
    }

    fn try_cold_close_sleeping_for_capacity(&self) -> Result<bool, String> {
        let Some(_automation) = try_lock_unpoison(&self.automation) else {
            return Ok(false);
        };
        let Some(_lifecycle) = try_lock_unpoison(&self.lifecycle) else {
            return Ok(false);
        };
        {
            let state = self.lock_state();
            if !state.status.suspended
                || state.status.open
                || state.status.loading
                || state.pending_navigation.is_some()
                || state.occluded
                || state.status.control.owner != BrowserControlOwner::Available
            {
                return Ok(false);
            }
        }
        if !self.has_retained_page() {
            return Ok(false);
        }
        self.suspend_page_locked().map(|_| true)
    }

    /// Moves an eligible hidden page into WebView2's native sleeping-tab state. Unlike a cold
    /// close, this retains session cookies, sessionStorage, form fields, history, and in-memory JS
    /// state. The caller owns both `automation` and `lifecycle`.
    fn sleep_page_locked(&self) -> Result<bool, String> {
        let page = self.page()?;

        // WebView2 requires an invisible controller before TrySuspend. LRU candidates have already
        // been proven hidden; repeat the native operation defensively so status and controller
        // visibility cannot drift after a host/layout transition.
        page.hide()
            .map_err(|error| format!("准备浏览器睡眠失败: {error}"))?;
        if !page.with_native_tail(|native, permit| {
            browser_webview_lifecycle::try_suspend(native, permit, BROWSER_SLEEP_TRANSITION_TIMEOUT)
        })? {
            return Ok(false);
        }

        self.agent_pointer_generation.fetch_add(1, Ordering::SeqCst);
        let now = Utc::now().timestamp_millis();
        let mut state = self.lock_state();
        state.status.has_page = false;
        state.status.open = false;
        state.status.loading = false;
        state.status.suspended = true;
        state.status.suspended_at_ms = Some(now);
        state.status.error = None;
        state.status.agent_activity = None;
        state.status.control = BrowserControlStatus {
            updated_at_ms: now,
            ..BrowserControlStatus::default()
        };
        restore_control_after_menu(&mut state);
        // A sleeping page has nothing left to cover. Saying so is what lets the renderer drop the
        // still frame it captured instead of leaving the user looking at a picture of a page that
        // no longer exists.
        clear_page_cover(&mut state);
        state.cold_close_cookies = None;
        state.credential_takeover_grant = None;
        discard_file_preview_staging(&mut state);
        Ok(true)
    }

    /// Explicitly releases the live WebView. The stable profile directory and enough trusted
    /// metadata to recreate the current page remain owned by this conversation.
    ///
    /// The caller owns both `automation` and `lifecycle`.
    fn suspend_page_locked(&self) -> Result<BrowserStatus, String> {
        let mut status = self.status();
        let app = self.app_handle()?;
        if !status.has_page {
            if status.suspended {
                // Automatic LRU suspension retains the controller. A later explicit user suspend
                // is a stronger privacy/resource action: wake it only long enough to snapshot
                // cookies and cold-close the page.
                let page = match self.attested_page(true) {
                    Ok(page) => page,
                    Err(_) if app.get_webview(&self.labels.page).is_none() => return Ok(status),
                    Err(error) => return Err(error),
                };
                if let Err(error) = page.with_native_tail(|native, permit| {
                    browser_webview_lifecycle::resume(
                        native,
                        permit,
                        BROWSER_SLEEP_TRANSITION_TIMEOUT,
                    )
                }) {
                    // A timeout cannot prove that the UI-thread Resume did not run late. Count the
                    // controller as awake so capacity accounting never underestimates resources.
                    let mut state = self.lock_state();
                    state.status.has_page = true;
                    state.status.suspended = false;
                    state.status.suspended_at_ms = None;
                    state.status.open = false;
                    state.status.loading = false;
                    state.status.error = Some(error.clone());
                    return Err(error);
                }
                {
                    let mut state = self.lock_state();
                    state.status.has_page = true;
                    state.status.suspended = false;
                    state.status.suspended_at_ms = None;
                    state.status.error = None;
                }
                status = self.status();
                if !status.has_page {
                    return Err("恢复睡眠页面以执行冷挂起时页面已丢失".into());
                }
            } else {
                return Err("内置浏览器尚未打开".into());
            }
        }
        let host = self.lock_state().host;
        let page = self.attested_page(true)?;
        // Destroying the last WebView for a data directory ends Chromium's in-memory session
        // cookie lifetime. Capture an exact, bounded CDP handoff first; if Chromium exposes a
        // cookie shape that cannot be recreated without widening its scope, refuse the cold close.
        let chromium_control = self.webview2_control()?;
        let cookie_snapshot = capture_cookies_for_cold_close(&chromium_control, &page)?;
        let native_page = page.into_native_for_teardown();
        // Invalidate before requesting asynchronous close. The generation transition refuses new
        // operations and waits for every already-issued page/CDP permit to finish.
        let teardown = self
            .invalidate_webview2_controller()?
            .ok_or_else(|| "挂起内置浏览器页面时缺少 controller 销毁能力".to_owned())?;
        teardown
            .ensure_active()
            .map_err(|error| format!("Chromium controller 销毁能力不可用: {error}"))?;
        if let Err(error) = native_page.close() {
            return Err(format!("挂起内置浏览器页面失败: {error}"));
        }
        self.agent_pointer_generation.fetch_add(1, Ordering::SeqCst);

        let now = Utc::now().timestamp_millis();
        {
            // Persist the cold-resume state and invalidate every callback before destroying a
            // detached host. Window destruction can synchronously dispatch `Destroyed`.
            let mut state = self.lock_state();
            state.page_generation = state.page_generation.wrapping_add(1);
            state.layout_generation = state.layout_generation.wrapping_add(1);
            let resume_url = cold_resume_url(&state);
            // `ensure_page` resumes from `status.url` rather than from the history entry, so the
            // address this session reports has to forget the preview too — otherwise the toolbar
            // keeps printing one nothing can reach and the resume follows it.
            state.status.url = resume_url.clone();
            state.status.has_page = false;
            state.status.open = false;
            state.status.loading = false;
            state.status.suspended = true;
            state.status.suspended_at_ms = Some(now);
            state.status.error = None;
            state.status.agent_activity = None;
            state.status.control = BrowserControlStatus {
                updated_at_ms: now,
                ..BrowserControlStatus::default()
            };
            state.host = None;
            // The control status above is already reset, so the pre-cover owner is dropped rather
            // than restored; what still has to be published is that nothing is covering a page
            // this conversation no longer has.
            clear_page_cover(&mut state);
            state.menu_control_before_open = None;
            // Native WebView history cannot be injected into a replacement controller. Keep only
            // the resumable URL rather than advertising back/forward actions that cannot work.
            state.history.clear();
            state.history.push(resume_url);
            state.history_index = Some(0);
            state.pending_navigation = None;
            state.pending_previous_url = None;
            state.cold_resume_target = None;
            state.cold_close_cookies = Some(cookie_snapshot);
            state.credential_takeover_grant = None;
            discard_file_preview_staging(&mut state);
            sync_history_flags(&mut state);
        }

        if let Err(error) = self.retire_native_surface_and_wait(&app, host, false, Some(&teardown))
        {
            self.lock_state().status.error = Some(error.clone());
            return Err(error);
        }
        Ok(self.lock_state().status.clone())
    }

    /// Injects the application handle. This is cheap and may be called more than once during setup
    /// or in tests, but replacing it while a browser page exists is rejected.
    pub fn attach_app(&self, app: AppHandle) -> Result<(), String> {
        let _lifecycle = self.lock_lifecycle();
        let mut state = self.lock_state();
        if (state.status.has_page || state.status.suspended || state.webview2_lease.is_some())
            && state.app.is_some()
        {
            return Err("内置浏览器打开时不能替换 AppHandle".into());
        }
        state.app = Some(app);
        Ok(())
    }

    #[allow(dead_code)] // Useful for embedding applications and setup diagnostics.
    pub fn is_attached(&self) -> bool {
        self.lock_state().app.is_some()
    }

    /// Opens (or focuses) the single native browser surface.
    ///
    /// Window creation must not run on Tauri's UI thread on Windows. Tauri `async` commands and
    /// [`execute_tool`](Self::execute_tool) satisfy that requirement.
    pub fn open(&self, url: Option<&str>) -> Result<BrowserStatus, String> {
        self.ensure_page(url, true)
    }

    /// Creates or navigates the page without making a hidden page visible.
    fn prepare(&self, url: Option<&str>) -> Result<BrowserStatus, String> {
        self.ensure_page(url, false)
    }

    fn ensure_page(&self, url: Option<&str>, make_visible: bool) -> Result<BrowserStatus, String> {
        // Keep this guard for the complete inspect/cleanup/create-or-focus sequence. In particular,
        // partial WebView cleanup must not race a concurrent close or a second open.
        let _lifecycle = self.lock_lifecycle();
        if self.lock_state().terminated {
            return Err(
                "the application is shutting down and the browser page has been released".into(),
            );
        }
        let resume_url = {
            let state = self.lock_state();
            if url.is_none() && state.status.suspended && !state.status.url.trim().is_empty() {
                state.status.url.clone()
            } else {
                DEFAULT_URL.to_owned()
            }
        };
        let parsed = parse_browser_url(url.unwrap_or(&resume_url), self.security_level())?;
        let app = self.app_handle()?;

        // A native label left behind by failed attestation or asynchronous cold-close teardown is
        // never a reusable controller. The control is reset before every controller creation and
        // immediately after cold close, so only an independently attested live page reaches the
        // fast path below.
        let native_page_exists = app.get_webview(&self.labels.page).is_some();
        let detached_window_exists = app.get_window(&self.labels.window).is_some();
        if (native_page_exists && self.attested_page(true).is_err())
            || (!native_page_exists && detached_window_exists)
        {
            self.discard_unattested_native_surface(&app)?;
        }

        let page = if app.get_webview(&self.labels.page).is_some() {
            Some(self.attested_page(true)?)
        } else {
            None
        };
        if let Some(page) = page.as_ref() {
            let resuming_native_sleep = self.lock_state().status.suspended;
            if resuming_native_sleep {
                if let Err(error) = page.with_native_tail(|native, permit| {
                    browser_webview_lifecycle::resume(
                        native,
                        permit,
                        BROWSER_SLEEP_TRANSITION_TIMEOUT,
                    )
                }) {
                    // Resume scheduling has a deadline, but conservatively treat an uncertain
                    // controller as awake. Showing/navigating it later is WebView2's documented
                    // secondary resume path.
                    let mut state = self.lock_state();
                    state.status.has_page = true;
                    state.status.suspended = false;
                    state.status.suspended_at_ms = None;
                    state.status.open = false;
                    state.status.loading = false;
                    state.status.error = Some(error.clone());
                    return Err(error);
                }
            }
            {
                let mut state = self.lock_state();
                state.status.has_page = true;
                state.status.suspended = false;
                state.status.suspended_at_ms = None;
                if make_visible {
                    state.status.open = true;
                }
                state.status.error = None;
                restore_control_after_menu(&mut state);
                // Whatever was covering this page belonged to the pane as it stood before the
                // page was retained; the renderer republishes its surfaces against the page it
                // is being handed now, and until it does the host must not claim one is up.
                clear_page_cover(&mut state);
            }
            if resuming_native_sleep {
                self.apply_preferences_to_current_start_page()?;
            }
            if make_visible {
                let host = self.lock_state().host;
                if host == Some(BrowserHost::DetachedWindow) {
                    let window = app
                        .get_window(&self.labels.window)
                        .ok_or_else(|| "内置浏览器独立窗口已丢失".to_owned())?;
                    window
                        .show()
                        .map_err(|error| format!("显示内置浏览器失败: {error}"))?;
                    window
                        .set_focus()
                        .map_err(|error| format!("聚焦内置浏览器失败: {error}"))?;
                } else if let Some(window) = app.get_window(MAIN_WINDOW_LABEL) {
                    let panel_bounds = self.lock_state().panel_bounds;
                    let viewport = resize_page(
                        BrowserHost::MainPanel,
                        &window,
                        page,
                        window.inner_size().ok(),
                        panel_bounds,
                    );
                    self.lock_state().status.viewport = viewport;
                }
                page.show()
                    .map_err(|error| format!("显示内置浏览器页面失败: {error}"))?;
                // A page presented while it is already sunk — a trusted surface is over it, or the
                // pane is painting a still of it — goes straight back under the renderer. Both
                // raising it and giving it focus would put it in front of what the user is
                // actually looking at.
                let parked = page_parked(&self.lock_state());
                page.set_stacking(parked)?;
                if !parked {
                    page.set_focus()
                        .map_err(|error| format!("聚焦浏览器页面失败: {error}"))?;
                }
            }
            if !make_visible {
                // Resumed from sleep or still parked: keep it live for the Agent either way.
                self.park_attested_page(&app)?;
            }
            if url.is_some() {
                self.navigate_parsed(parsed)?;
            }
            return Ok(self.status());
        }

        let (previous_status, previous_history, previous_history_index, cold_close_cookies) = {
            let mut state = self.lock_state();
            let cold_close_cookies = if state.status.suspended {
                state.cold_close_cookies.take()
            } else {
                None
            };
            (
                state.status.clone(),
                state.history.clone(),
                state.history_index,
                cold_close_cookies,
            )
        };
        let restoring_suspended = previous_status.suspended;
        if !restoring_suspended {
            reset_closed_state(&mut self.lock_state());
        }
        self.lock_state().status.error = None;

        // A cold-close snapshot must be restored on a network-inert page. Starting Chromium at the
        // destination URL would let the first request race ahead without the session cookie.
        let cold_resume = restoring_suspended && cold_close_cookies.is_some();
        // An ordinary page defers its first navigation for the mirror-image reason: nothing may
        // reach the network until WebView2 itself has confirmed which profile it opened. Every tab
        // is promised its own single-use profile; if an override had quietly pointed this
        // controller at another user-data folder, a request sent first and checked second would
        // already have carried that folder's cookies. Every session is an ordinary tab now that the
        // research browser is gone, so this always attests.
        let defer_first_navigation = cold_resume || parsed.as_str() != DEFAULT_URL;
        let initial_url = if defer_first_navigation {
            Url::parse(DEFAULT_URL).expect("about:blank is a valid URL")
        } else {
            parsed.clone()
        };
        let mut result = self.create_window(
            &app,
            initial_url,
            restoring_suspended && url.is_none(),
            defer_first_navigation.then_some(&parsed),
        );
        let created_page = result.is_ok();
        if result.is_ok() && defer_first_navigation {
            result = (|| {
                let page = self.page()?;
                if let Some(snapshot) = cold_close_cookies.as_ref() {
                    let now = Utc::now().timestamp_millis() as f64 / 1_000.0;
                    let chromium_control = self.webview2_control()?;
                    restore_cookies_after_cold_close(&chromium_control, &page, snapshot, now)?;
                }
                self.navigate_after_cold_resume(&page, &parsed)
            })();
        }
        if result.is_ok() {
            result = if make_visible {
                self.show_attested_page(&app)
            } else {
                self.park_attested_page(&app)
            };
        }
        result = match result {
            Err(primary) if created_page => match self.discard_failed_cold_resume(&app) {
                Ok(()) => Err(primary),
                Err(cleanup) => Err(format!("{primary}；{cleanup}")),
            },
            other => other,
        };
        let mut state = self.lock_state();
        match result {
            Ok(()) => {
                state.status.suspended = false;
                state.status.suspended_at_ms = None;
                Ok(state.status.clone())
            }
            Err(error) => {
                if restoring_suspended {
                    state.status = previous_status;
                    state.status.error = Some(error.clone());
                    state.status.has_page = false;
                    state.status.open = false;
                    state.status.loading = false;
                    state.history = previous_history;
                    state.history_index = previous_history_index;
                    state.host = None;
                    clear_page_cover(&mut state);
                    state.menu_control_before_open = None;
                    state.pending_navigation = None;
                    state.pending_previous_url = None;
                    state.cold_resume_target = None;
                    state.cold_close_cookies = cold_close_cookies;
                    state.credential_takeover_grant = None;
                    discard_file_preview_staging(&mut state);
                    sync_history_flags(&mut state);
                } else {
                    reset_closed_state(&mut state);
                    state.status.error = Some(error.clone());
                }
                Err(error)
            }
        }
    }

    fn profile_directory(&self, app: &AppHandle) -> Result<PathBuf, String> {
        let app_data = app
            .path()
            .app_local_data_dir()
            .map_err(|error| format!("failed to resolve the browser profile directory: {error}"))?;
        // Every tab profile is a single-use, hash-named directory under its own app-owned
        // root; the root may not be a link and the name may not be anything but the exact
        // 64-hex digest we minted.
        if !is_profile_hash(&self.labels.profile) {
            return Err("browser profile directory identifier is invalid".into());
        }
        let root = app_data.join(self.labels.profile_root);
        ensure_profile_root(&root)?;
        Ok(root.join(&self.labels.profile))
    }

    /// Creates and exclusively claims this tab's profile before any native controller can use it.
    /// A retained claim is reused across cold close/resume; a different runtime cannot claim the
    /// same path (or an ancestor/descendant) while this tab remains alive.
    fn ensure_webview2_profile_claim(&self, app: &AppHandle) -> Result<PathBuf, String> {
        {
            let state = self.lock_state();
            match (
                &state.webview2_lease,
                &state.webview2_controller_issuer,
                &state.webview2_control,
                &state.webview2_teardown,
            ) {
                (Some(lease), Some(_), Some(_), None)
                | (Some(lease), Some(_), None, None)
                | (Some(lease), Some(_), None, Some(_)) => {
                    lease.ensure_active().map_err(|error| {
                        format!("embedded browser Chromium profile lease is invalid: {error}")
                    })?;
                    return Ok(lease.directory().to_path_buf());
                }
                (None, None, None, None) => {}
                _ => {
                    return Err(
                        "embedded browser Chromium profile capability state is incomplete".into(),
                    )
                }
            }
        }

        let requested = self.profile_directory(app)?;
        std::fs::create_dir_all(&requested)
            .map_err(|error| format!("failed to create the browser profile directory: {error}"))?;
        let root = tab_profile_root(app)?;
        let profile = WebView2Profile::within_root(&requested, &root).map_err(|error| {
            format!("failed to acquire the embedded browser Chromium profile lease: {error}")
        })?;
        let lease = WebView2RuntimeLease::claim(profile).map_err(|error| {
            format!("failed to acquire the embedded browser Chromium profile lease: {error}")
        })?;
        let directory = lease.directory().to_path_buf();
        let issuer = lease.controller_issuer();

        let mut state = self.lock_state();
        if state.terminated {
            return Err(
                "the application is shutting down and the browser page has been released".into(),
            );
        }
        if state.webview2_lease.is_some()
            || state.webview2_controller_issuer.is_some()
            || state.webview2_control.is_some()
            || state.webview2_teardown.is_some()
        {
            return Err(
                "the embedded browser Chromium profile lease was concurrently replaced".into(),
            );
        }
        state.webview2_lease = Some(lease);
        state.webview2_controller_issuer = Some(issuer);
        Ok(directory)
    }

    fn begin_webview2_controller(&self) -> Result<WebView2Control, String> {
        let issuer = {
            let state = self.lock_state();
            if state.webview2_teardown.is_some() {
                return Err("the previous Chromium controller has not finished closing".into());
            }
            state.webview2_controller_issuer.clone().ok_or_else(|| {
                "the embedded browser has not acquired Chromium controller issuance capability"
                    .to_owned()
            })?
        };
        // Mint outside RuntimeState: replacing a controller waits for all old-generation permits,
        // and those operations may legitimately touch RuntimeState before they finish.
        let control = issuer.begin_controller().map_err(|error| {
            format!("failed to begin the WebView2 controller startup generation: {error}")
        })?;
        let mut state = self.lock_state();
        if state.terminated || state.webview2_lease.is_none() {
            drop(state);
            let _ = control.invalidate();
            return Err(
                "the application is shutting down and the browser page has been released".into(),
            );
        }
        state.webview2_control = Some(control.clone());
        Ok(control)
    }

    fn webview2_control(&self) -> Result<WebView2Control, String> {
        self.lock_state().webview2_control.clone().ok_or_else(|| {
            "the embedded browser has not acquired Chromium native control capability".to_owned()
        })
    }

    fn invalidate_webview2_controller(&self) -> Result<Option<WebView2TeardownPermit>, String> {
        let (control, existing) = {
            let mut state = self.lock_state();
            (
                state.webview2_control.take(),
                state.webview2_teardown.clone(),
            )
        };
        if let Some(teardown) = existing {
            return Ok(Some(teardown));
        }
        let Some(control) = control else {
            return Ok(None);
        };
        let teardown = control
            .invalidate()
            .map_err(|error| format!("无法撤销 WebView2 controller 启动代: {error}"))?;
        self.lock_state().webview2_teardown = Some(teardown.clone());
        Ok(Some(teardown))
    }

    fn clear_webview2_teardown(&self) {
        self.lock_state().webview2_teardown = None;
    }

    fn revoke_webview2_profile_claim(&self) {
        let (mut lease, issuer, control, teardown) = {
            let mut state = self.lock_state();
            (
                state.webview2_lease.take(),
                state.webview2_controller_issuer.take(),
                state.webview2_control.take(),
                state.webview2_teardown.take(),
            )
        };
        if let Some(lease) = lease.as_mut() {
            lease.revoke();
        }
        drop(control);
        drop(teardown);
        drop(issuer);
        drop(lease);
    }

    /// Makes WebView2 itself state which user-data folder this page opened, and rejects the page
    /// unless it is the single-use tab profile the host asked for.
    ///
    /// The isolation this feature promises is Chromium's: two user-data folders are two cookie
    /// stores. That promise is only as good as the folder the controller actually got, and a
    /// process environment variable, a Loader Override registry policy, or a WebView2 host bug
    /// could all redirect it. Rather than enumerate those mechanisms, ask the component that
    /// resolved them — `ICoreWebView2Environment7::UserDataFolder` is the folder in force.
    #[cfg(windows)]
    fn attest_tab_profile_directory(&self, app: &AppHandle) -> Result<(), String> {
        use webview2_com::take_pwstr;
        use webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Environment7;
        use windows_core::{Interface, PWSTR};

        let expected = std::fs::canonicalize(self.profile_directory(app)?)
            .map_err(|error| format!("无法确认标签页专属浏览器配置目录的真实路径: {error}"))?;
        let chromium_control = self.webview2_control()?;
        let attestation_permit = chromium_control
            .begin_attestation()
            .map_err(|error| format!("无法建立 WebView2 Profile 验证屏障: {error}"))?;
        // This is the one native operation allowed before the new controller is attested. Normal
        // callers go through `page()`, which rejects an unattested controller. The dedicated
        // permit moves into the queued closure so a caller timeout cannot race failed-creation
        // teardown with late Environment7 access.
        let page = app
            .get_webview(&self.labels.page)
            .ok_or_else(|| "无法验证标签页专属浏览器配置目录: 页面已丢失".to_owned())?;
        let queued_page = page.clone();
        let (sender, receiver) = mpsc::sync_channel::<Result<(), String>>(1);
        page.with_webview(move |platform| {
            let result = (|| -> Result<(), String> {
                // This is the only ordinary WebView mutation before attestation. The closure is
                // already on Tauri's UI thread and owns the dedicated attestation permit, so hide
                // cannot escape the generation drain even when the caller times out.
                queued_page
                    .hide()
                    .map_err(|error| format!("隐藏待验证的 Chromium 页面失败: {error}"))?;
                let environment = platform
                    .environment()
                    .cast::<ICoreWebView2Environment7>()
                    .map_err(|error| format!("当前 WebView2 无法验证实际配置目录: {error}"))?;
                let mut actual = PWSTR::null();
                unsafe {
                    environment
                        .UserDataFolder(&mut actual)
                        .map_err(|error| format!("读取 WebView2 实际配置目录失败: {error}"))?;
                }
                let actual =
                    std::fs::canonicalize(PathBuf::from(take_pwstr(actual))).map_err(|error| {
                        format!("无法确认 WebView2 实际配置目录的真实路径: {error}")
                    })?;
                if !actual
                    .to_string_lossy()
                    .eq_ignore_ascii_case(&expected.to_string_lossy())
                {
                    return Err(
                        "WebView2 实际使用的配置目录不是这个标签页的专属 Profile，已拒绝打开该标签页"
                            .into(),
                    );
                }
                attestation_permit
                    .verify_attested_user_data_folder(&actual)
                    .map_err(|error| format!("WebView2 Profile 能力验证失败: {error}"))?;
                Ok(())
            })();
            let _ = sender.send(result);
        })
        .map_err(|error| format!("无法调度标签页专属浏览器配置目录验证: {error}"))?;
        receiver
            .recv_timeout(EVAL_TIMEOUT)
            .map_err(|_| "验证标签页专属浏览器配置目录超时".to_owned())?
    }

    #[cfg(not(windows))]
    fn attest_tab_profile_directory(&self, app: &AppHandle) -> Result<(), String> {
        // Off Windows there is no WebView2 environment to interrogate. The separate data directory
        // is still passed to the platform webview. Bind the requested canonical directory so
        // development builds retain the same control lifecycle, but do not claim native
        // UserDataFolder attestation on those platforms.
        let expected = self.profile_directory(app)?;
        let attestation_permit = self
            .webview2_control()?
            .begin_attestation()
            .map_err(|error| format!("无法建立开发平台 Chromium Profile 验证屏障: {error}"))?;
        let page = app
            .get_webview(&self.labels.page)
            .ok_or_else(|| "无法绑定开发平台 Chromium Profile 能力: 页面已丢失".to_owned())?;
        let queued_page = page.clone();
        let (sender, receiver) = mpsc::sync_channel::<Result<(), String>>(1);
        page.with_webview(move |_| {
            let result = queued_page
                .hide()
                .map_err(|error| format!("隐藏待验证的 Chromium 页面失败: {error}"))
                .and_then(|_| {
                    attestation_permit
                        .verify_attested_user_data_folder(&expected)
                        .map_err(|error| format!("无法绑定开发平台 Chromium Profile 能力: {error}"))
                });
            let _ = sender.try_send(result);
        })
        .map_err(|error| format!("无法调度开发平台 Chromium Profile 验证: {error}"))?;
        receiver
            .recv_timeout(EVAL_TIMEOUT)
            .map_err(|_| "绑定开发平台 Chromium Profile 能力超时".to_owned())?
    }

    /// Publishes a newly created controller only after its profile generation is attested.
    /// Leaves a background page natively visible but off-screen (see `AttestedPage::park`). A
    /// detached-window host is hidden as a whole instead; it only exists without a main window.
    fn park_attested_page(&self, app: &AppHandle) -> Result<(), String> {
        let page = self.page()?;
        let host = self.lock_state().host;
        match (host, app.get_window(MAIN_WINDOW_LABEL)) {
            (Some(BrowserHost::MainPanel), Some(window)) => {
                let panel_bounds = self.lock_state().panel_bounds;
                let layout = browser_host_layout(&window, BrowserHost::MainPanel, panel_bounds);
                page.park(layout)
                    .map_err(|error| format!("failed to park the browser page: {error}"))?;
                self.lock_state().status.viewport = layout.viewport();
            }
            (Some(BrowserHost::DetachedWindow), _) => {
                // The detached host window stays hidden; the controller inside it must still be
                // visible to Chromium for the page to keep compositing.
                page.show()
                    .map_err(|error| format!("failed to park the browser page: {error}"))?;
            }
            _ => {}
        }
        Ok(())
    }

    fn show_attested_page(&self, app: &AppHandle) -> Result<(), String> {
        let page = self.page()?;
        let host = self
            .lock_state()
            .host
            .ok_or_else(|| "内置浏览器宿主尚未建立".to_owned())?;
        if host == BrowserHost::DetachedWindow {
            let window = app
                .get_window(&self.labels.window)
                .ok_or_else(|| "内置浏览器独立窗口已丢失".to_owned())?;
            window
                .show()
                .map_err(|error| format!("显示内置浏览器失败: {error}"))?;
            window
                .set_focus()
                .map_err(|error| format!("聚焦内置浏览器失败: {error}"))?;
        } else if let Some(window) = app.get_window(MAIN_WINDOW_LABEL) {
            let panel_bounds = self.lock_state().panel_bounds;
            let viewport = resize_page(
                BrowserHost::MainPanel,
                &window,
                &page,
                window.inner_size().ok(),
                panel_bounds,
            );
            self.lock_state().status.viewport = viewport;
        }
        page.show()
            .map_err(|error| format!("显示内置浏览器页面失败: {error}"))?;
        // See the same sequence in `ensure_page`: a page that is already sunk is presented under
        // the renderer and is not focused.
        let parked = page_parked(&self.lock_state());
        page.set_stacking(parked)?;
        if !parked {
            page.set_focus()
                .map_err(|error| format!("聚焦浏览器页面失败: {error}"))?;
        }
        self.lock_state().status.open = true;
        Ok(())
    }

    fn create_window(
        &self,
        app: &AppHandle,
        initial_url: Url,
        preserve_title: bool,
        cold_resume_target: Option<&Url>,
    ) -> Result<(), String> {
        // A replacement page is born uncovered, and the surfaces that were over the old one went
        // away with it. Say so before anything native exists, so the renderer republishes against
        // the page it is about to get rather than against the one it lost.
        {
            let mut state = self.lock_state();
            clear_page_cover(&mut state);
        }
        // Resolve and exclusively claim storage before creating any native host so a path error or
        // cross-runtime collision cannot leave a detached window or provisional page behind.
        let profile_directory = self.ensure_webview2_profile_claim(app)?;
        let chromium_control = self.begin_webview2_controller()?;

        let (window, host) = if let Some(main) = app.get_window(MAIN_WINDOW_LABEL) {
            (main, BrowserHost::MainPanel)
        } else {
            let detached = WindowBuilder::new(app, &self.labels.window)
                .title("Mework Browser")
                .inner_size(DEFAULT_WIDTH, DEFAULT_HEIGHT)
                .min_inner_size(480.0, 320.0)
                .visible(false)
                .build()
                .map_err(|error| format!("创建内置浏览器窗口失败: {error}"))?;
            (detached, BrowserHost::DetachedWindow)
        };

        let (layout_generation, page_generation, saved_zoom) = {
            let mut state = self.lock_state();
            let status_url = cold_resume_target.unwrap_or(&initial_url).to_string();
            state.host = Some(host);
            state.layout_generation = state.layout_generation.wrapping_add(1);
            state.page_generation = state.page_generation.wrapping_add(1);
            let page_generation = state.page_generation;
            state.activity.reset_for_generation(page_generation);
            state.status.has_page = true;
            state.status.open = false;
            state.status.suspended = false;
            state.status.suspended_at_ms = None;
            state.status.url = status_url.clone();
            state.status.loading = true;
            if !preserve_title {
                state.status.title = None;
            }
            state.status.error = None;
            state.history.clear();
            state.history.push(status_url);
            state.history_index = Some(0);
            state.pending_navigation = None;
            state.pending_previous_url = None;
            state.cold_resume_target = cold_resume_target.map(Url::to_string);
            sync_history_flags(&mut state);
            (
                state.layout_generation,
                state.page_generation,
                state.status.zoom,
            )
        };
        let result = (|| {
            let navigation_state = self.state.clone();
            let navigation_policy = self.navigation_policy.clone();
            let navigation_control = chromium_control.clone();
            let load_state = self.state.clone();
            let load_control = chromium_control.clone();
            let title_state = self.state.clone();
            let title_control = chromium_control.clone();
            let new_window_state = self.state.clone();
            let new_window_policy = self.navigation_policy.clone();
            let new_window_control = chromium_control.clone();
            let download_control = chromium_control.clone();
            let updates_window_title = host == BrowserHost::DetachedWindow;
            let page_builder =
                WebviewBuilder::new(&self.labels.page, WebviewUrl::External(initial_url.clone()))
                    .data_directory(profile_directory);
            let page_builder = page_builder
                .initialization_script(browser_initialization_script())
                // Pages are driven in the background, where Chromium would otherwise run timers
                // at 1 Hz and pause animation frames; a page that reacts a second late to every
                // input is the latency the model would then be waiting out. The first three
                // switches keep a hidden page running like a visible tab; the feature list is
                // wry's own default, which setting any argument replaces.
                .additional_browser_args(BROWSER_PAGE_BROWSER_ARGS)
                .zoom_hotkeys_enabled(true)
                .devtools(true)
                .general_autofill_enabled(true)
                .on_navigation(move |url| {
                    let _permit = match navigation_control.permit() {
                        Ok(permit) => permit,
                        // Controller creation itself may report its fixed, network-inert initial
                        // about:blank before UserDataFolder attestation. Never authorize another
                        // destination or mutate browser state in that interval, and never extend
                        // this exception to a stale or revoked controller.
                        Err(CapabilityError::Unattested) => return url.as_str() == DEFAULT_URL,
                        Err(_) => return false,
                    };
                    // Read under the same lock that records the block: the level is
                    // pushed by the run loop, and this callback fires from the
                    // WebView's own thread with no caller to ask.
                    let mut state = lock_unpoison(&navigation_state);
                    let allowed = navigation_policy.allows(url, state.security_level);
                    if !allowed {
                        state.blocked_navigations = state.blocked_navigations.saturating_add(1);
                        if state.page_generation == page_generation && state.status.has_page {
                            rollback_navigation_state(
                                &mut state,
                                format!(
                                    "unsafe browser navigation was blocked: {}",
                                    blocked_navigation_summary(url)
                                ),
                            );
                        }
                    }
                    allowed
                })
                .on_new_window({
                    // Single-view browser: instead of silently dropping window.open/target=_blank,
                    // load policy-approved URLs in the current view. The navigation runs off-thread
                    // because this callback can fire re-entrantly on the WebView UI thread.
                    let runtime = self.clone();
                    move |url, _features| {
                        let Ok(_permit) = new_window_control.permit() else {
                            return NewWindowResponse::Deny;
                        };
                        let mut state = lock_unpoison(&new_window_state);
                        let current =
                            state.page_generation == page_generation && state.status.has_page;
                        let allowed = new_window_policy.allows(&url, state.security_level);
                        if current && new_window_policy.may_retarget_new_window_to_current_page() {
                            if allowed {
                                drop(state);
                                let runtime = runtime.clone();
                                std::thread::spawn(move || {
                                    let _ = runtime.navigate_parsed(url);
                                });
                            } else {
                                // A refused popup used to vanish without a trace: nothing
                                // opened, no state changed, and the tool that caused it
                                // still reported plain success.
                                state.blocked_navigations =
                                    state.blocked_navigations.saturating_add(1);
                                state.status.error = Some(format!(
                                    "unsafe browser navigation was blocked: {}",
                                    blocked_navigation_summary(&url)
                                ));
                            }
                        }
                        NewWindowResponse::Deny
                    }
                })
                .on_download(move |_, _| {
                    // Downloads are denied for every page. Still require the exact controller
                    // generation so even this release/deny callback has an explicit authority.
                    let Ok(_permit) = download_control.permit() else {
                        return false;
                    };
                    false
                })
                .on_page_load(move |webview, payload| {
                    let Ok(permit) = load_control.permit() else {
                        return;
                    };
                    let page = AttestedPage {
                        page: webview.clone(),
                        _permit: permit,
                    };
                    let mut released_file_preview = None;
                    let sync_start_page_preferences = {
                        let mut state = lock_unpoison(&load_state);
                        if state.page_generation != page_generation || !state.status.has_page {
                            return;
                        }
                        let url = payload.url().as_str().to_owned();
                        if url == DEFAULT_URL
                            && state
                                .cold_resume_target
                                .as_deref()
                                .is_some_and(|target| target != DEFAULT_URL)
                        {
                            // This is the network-inert bootstrap document. It must never
                            // overwrite the trusted target or consume its pending navigation.
                            return;
                        }
                        state.status.url = url.clone();
                        match payload.event() {
                            // WebView2 may omit the matching Finished callback for its
                            // synthetic initial about:blank document. The start page is
                            // already usable as soon as the initialization script runs, so it
                            // must not leave the browser chrome stuck loading.
                            PageLoadEvent::Started => {
                                state.status.loading = url != DEFAULT_URL;
                                false
                            }
                            PageLoadEvent::Finished => {
                                // The old document can remain live after Started and even
                                // after navigate() returns. Credential taint is therefore
                                // released only for a committed Finished document.
                                clear_credential_takeover_after_committed_url(&mut state, &url);
                                released_file_preview =
                                    take_file_preview_after_committed_url(&mut state, &url);
                                state.status.loading = false;
                                // Counted next to the DevTools load event so a wait has a
                                // host-side signal even where DevTools events are unavailable;
                                // a double count only means "loaded since", which is the
                                // question every wait asks.
                                state.activity.load_events =
                                    state.activity.load_events.wrapping_add(1);
                                update_history_after_load(&mut state, url.clone());
                                state.cold_resume_target = None;
                                url == DEFAULT_URL
                            }
                        }
                    };
                    if let Some(grant) = released_file_preview {
                        release_file_preview(&page, grant);
                    }
                    if sync_start_page_preferences {
                        // Initialization scripts are document-scoped. Replay Mework's
                        // trusted preferences for every committed start-page document only;
                        // remote sites must retain their own theme, lang, and color-scheme.
                        let preferences = {
                            let state = lock_unpoison(&load_state);
                            (state.page_generation == page_generation && state.status.has_page)
                                .then(|| {
                                    (
                                        state.ui_theme.clone(),
                                        state.ui_language.clone(),
                                        state.ui_preferences_generation,
                                    )
                                })
                        };
                        if let Some((theme, language, generation)) = preferences {
                            let _ = page.eval(&start_page_preferences_script(
                                theme.as_deref(),
                                language.as_deref(),
                                generation,
                            ));
                        }
                    }
                })
                .on_document_title_changed(move |webview, title| {
                    let Ok(_permit) = title_control.permit() else {
                        return;
                    };
                    let title = sanitize_title(&title);
                    {
                        let mut state = lock_unpoison(&title_state);
                        if state.page_generation != page_generation || !state.status.has_page {
                            return;
                        }
                        state.status.title = (!title.is_empty()).then(|| title.clone());
                    }
                    if updates_window_title {
                        let window_title = if title.is_empty() {
                            "Mework Browser".to_owned()
                        } else {
                            format!("{title} - Mework Browser")
                        };
                        let _ = webview.window().set_title(&window_title);
                    }
                });

            let panel_bounds = self.lock_state().panel_bounds;
            let layout = browser_host_layout(&window, host, panel_bounds);
            let (initial_position, initial_size) = if host == BrowserHost::MainPanel {
                (
                    LogicalPosition::new(
                        PRE_ATTESTATION_CHILD_OFFSET,
                        PRE_ATTESTATION_CHILD_OFFSET,
                    ),
                    LogicalSize::new(1.0, 1.0),
                )
            } else {
                (
                    LogicalPosition::new(layout.x, layout.y),
                    LogicalSize::new(layout.width, layout.height),
                )
            };
            // Browser chrome stays in the main React WebView. This is the only native child: an
            // untrusted remote page with no application IPC capability.
            let page = window
                .add_child(page_builder, initial_position, initial_size)
                .map_err(|error| format!("创建浏览器页面失败: {error}"))?;
            // The controller is clipped outside the visible parent (or its parent is hidden) and
            // its only document is network-inert. The builder's fixed document-created bootstrap
            // may run on that about:blank, but cannot navigate or issue a request. Interrogate the
            // native environment before any general page operation and bind this exact controller
            // generation to the claimed UserDataFolder.
            self.attest_tab_profile_directory(app)?;
            let initialization_permit = chromium_control
                .permit()
                .map_err(|error| format!("Chromium 页面初始化能力不可用: {error}"))?;
            let page = AttestedPage {
                page,
                _permit: initialization_permit,
            };
            page.set_zoom(saved_zoom)
                .map_err(|error| format!("恢复浏览器缩放失败: {error}"))?;
            if initial_url.as_str() == DEFAULT_URL {
                // WebView2 does not consistently run document-created scripts for the synthetic
                // first about:blank when a controller is created visible. Queue the same guarded
                // bootstrap explicitly, then verify the trusted start page before reporting the
                // page ready. The registered initialization script remains authoritative for all
                // later navigations.
                page.eval(&browser_initialization_script())
                    .map_err(|error| format!("初始化浏览器开始页失败: {error}"))?;
                let (theme, language, generation) = {
                    let state = self.lock_state();
                    (
                        state.ui_theme.clone(),
                        state.ui_language.clone(),
                        state.ui_preferences_generation,
                    )
                };
                page.eval(&start_page_preferences_script(
                    theme.as_deref(),
                    language.as_deref(),
                    generation,
                ))
                .map_err(|error| format!("同步浏览器开始页偏好失败: {error}"))?;
                Self::verify_network_inert_start_page(&page)?;
            }

            install_layout_handler(
                self.state.clone(),
                chromium_control.clone(),
                host,
                layout_generation,
                &window,
                &page,
            );
            if let Err(error) =
                self.install_page_activity_observers(&chromium_control, &page, page_generation)
            {
                // Observation is a quality-of-service layer over a page that already works;
                // losing it degrades waits to the loading flag rather than failing the page.
                eprintln!(
                    "browser page observers unavailable for {}: {error}",
                    self.session_id
                );
            }
            // Keep the final layout unpublished until `show_attested_page` acquires this exact
            // controller generation's permit.

            let mut state = self.lock_state();
            state.status.has_page = true;
            state.status.open = false;
            if initial_url.as_str() == DEFAULT_URL {
                // Some WebView2 builds emit neither page-load callback for their synthetic first
                // about:blank document. The initialized start page is already ready here.
                state.status.loading = false;
            }
            state.status.viewport = layout.viewport();
            sync_history_flags(&mut state);
            Ok(())
        })();

        if let Err(primary) = result {
            self.agent_pointer_generation.fetch_add(1, Ordering::SeqCst);
            {
                let mut state = self.lock_state();
                state.page_generation = state.page_generation.wrapping_add(1);
                state.layout_generation = state.layout_generation.wrapping_add(1);
                state.host = None;
            }
            let mut cleanup_errors = Vec::new();
            let teardown = match self.invalidate_webview2_controller() {
                Ok(teardown) => teardown,
                Err(error) => {
                    cleanup_errors.push(error);
                    None
                }
            };
            if let Err(error) =
                self.retire_native_surface_and_wait(app, Some(host), true, teardown.as_ref())
            {
                cleanup_errors.push(error);
            }
            return if cleanup_errors.is_empty() {
                Err(primary)
            } else {
                Err(format!("{primary}；{}", cleanup_errors.join("；")))
            };
        }
        Ok(())
    }

    /// Completes a cold resume after the cookie handoff has been restored on about:blank.
    ///
    /// Native WebView history cannot be reconstructed, so the destination replaces the
    /// provisional entry and remains the only advertised history item.
    fn navigate_after_cold_resume(&self, page: &AttestedPage, target: &Url) -> Result<(), String> {
        let target = target.to_string();
        {
            let mut state = self.lock_state();
            state.status.url = target.clone();
            state.history.clear();
            state.history.push(target.clone());
            state.history_index = Some(0);
            state.pending_navigation = None;
            state.pending_previous_url = None;
            sync_history_flags(&mut state);
            if target == DEFAULT_URL {
                state.status.loading = false;
                state.cold_resume_target = None;
                return Ok(());
            }
            // Treat the first real navigation like a reload of the single retained entry. This
            // prevents the provisional about:blank from becoming a synthetic Back destination.
            begin_navigation(&mut state, PendingNavigation::Reload, Some(&target));
        }
        if let Err(error) = page
            .navigate(Url::parse(&target).map_err(|error| format!("冷恢复目标 URL 无效: {error}"))?)
        {
            let message = format!("恢复浏览器页面失败: {error}");
            rollback_navigation_state(&mut self.lock_state(), message.clone());
            return Err(message);
        }
        Ok(())
    }

    /// Removes a provisional about:blank page after cookie restoration or navigation fails.
    /// The caller restores the prior suspended metadata and in-memory snapshot afterwards.
    fn discard_failed_cold_resume(&self, app: &AppHandle) -> Result<(), String> {
        let invalidation = self.invalidate_webview2_controller();
        self.agent_pointer_generation.fetch_add(1, Ordering::SeqCst);
        let host = {
            let mut state = self.lock_state();
            state.page_generation = state.page_generation.wrapping_add(1);
            state.layout_generation = state.layout_generation.wrapping_add(1);
            // The provisional page is going away before it ever reached the pane, so nothing the
            // renderer drew over it applies to whatever is restored in its place.
            clear_page_cover(&mut state);
            let host = state.host;
            state.host = None;
            host
        }
        .or_else(|| {
            app.get_window(&self.labels.window)
                .map(|_| BrowserHost::DetachedWindow)
        });
        let retirement = self.retire_native_surface_and_wait(
            app,
            host,
            true,
            invalidation.as_ref().ok().and_then(Option::as_ref),
        );
        combine_cleanup_results(invalidation.map(|_| ()), retirement)
    }

    fn discard_unattested_native_surface(&self, app: &AppHandle) -> Result<(), String> {
        let invalidation = self.invalidate_webview2_controller();
        self.agent_pointer_generation.fetch_add(1, Ordering::SeqCst);
        let host = {
            let mut state = self.lock_state();
            state.page_generation = state.page_generation.wrapping_add(1);
            state.layout_generation = state.layout_generation.wrapping_add(1);
            state.status.has_page = false;
            state.status.open = false;
            state.status.loading = false;
            // A surface that never passed attestation is not a page anything can be covering.
            clear_page_cover(&mut state);
            let host = state.host;
            state.host = None;
            host
        }
        .or_else(|| {
            app.get_window(&self.labels.window)
                .map(|_| BrowserHost::DetachedWindow)
        });
        let retirement = self.retire_native_surface_and_wait(
            app,
            host,
            true,
            invalidation.as_ref().ok().and_then(Option::as_ref),
        );
        combine_cleanup_results(invalidation.map(|_| ()), retirement)
    }

    /// Retires one controller and waits until Tauri no longer resolves its native labels. This is
    /// the fence that separates a failed/closed controller generation from a later retry.
    fn retire_native_surface_and_wait(
        &self,
        app: &AppHandle,
        host: Option<BrowserHost>,
        request_page_close: bool,
        teardown: Option<&WebView2TeardownPermit>,
    ) -> Result<(), String> {
        let mut errors = Vec::new();
        let surface_exists = app.get_webview(&self.labels.page).is_some()
            || (host == Some(BrowserHost::DetachedWindow)
                && app.get_window(&self.labels.window).is_some());
        if surface_exists {
            match teardown {
                Some(teardown) => {
                    if let Err(error) = teardown.ensure_active() {
                        errors.push(format!("Chromium controller 销毁能力不可用: {error}"));
                    }
                }
                None => errors.push("Chromium 原生 surface 存在但缺少销毁能力".to_owned()),
            }
        }
        let teardown_authorized = !surface_exists || errors.is_empty();
        if !teardown_authorized {
            return Err(errors.join("；"));
        }
        if request_page_close {
            if let Some(page) = app.get_webview(&self.labels.page) {
                if let Err(error) = page.close() {
                    errors.push(format!("关闭 Chromium 页面失败: {error}"));
                }
            }
        }
        if host == Some(BrowserHost::DetachedWindow) {
            if let Some(window) = app.get_window(&self.labels.window) {
                if let Err(error) = window.destroy() {
                    errors.push(format!("销毁 Chromium 窗口失败: {error}"));
                }
            }
        }

        let deadline = Instant::now() + BROWSER_DESTROY_TIMEOUT;
        loop {
            let page_exists = app.get_webview(&self.labels.page).is_some();
            let window_exists = host == Some(BrowserHost::DetachedWindow)
                && app.get_window(&self.labels.window).is_some();
            if !page_exists && !window_exists {
                break;
            }
            if Instant::now() >= deadline {
                errors.push("Chromium 控制器销毁超时，拒绝复用旧原生标签".to_owned());
                break;
            }
            std::thread::sleep(BROWSER_DESTROY_POLL);
        }

        if errors.is_empty() {
            self.clear_webview2_teardown();
            Ok(())
        } else {
            Err(errors.join("；"))
        }
    }

    pub fn status(&self) -> BrowserStatus {
        let (app, mut status, pending_navigation) = {
            let state = self.lock_state();
            let mut status = state.status.clone();
            status.element_picker = state.element_picker.snapshot(state.page_generation);
            #[cfg(test)]
            if state.synthetic_surface {
                // A synthetic surface has no native page to observe, so the
                // recorded state is authoritative. `hide` already honours this;
                // reading a status must agree with it.
                return status;
            }
            (state.app.clone(), status, state.pending_navigation)
        };
        let Some(app) = app else {
            status.has_page = false;
            status.open = false;
            status.loading = false;
            return status;
        };
        if status.agent_activity.as_ref().is_some_and(|activity| {
            !activity.active
                && Utc::now()
                    .timestamp_millis()
                    .saturating_sub(activity.updated_at_ms)
                    > 1_800
        }) {
            status.agent_activity = None;
        }
        if status.suspended {
            status.has_page = false;
            status.open = false;
            status.loading = false;
            return status;
        }
        // Manager lookup alone does not touch the controller. URL/size observation is a native
        // operation and therefore goes through the same generation permit as every page command;
        // a concurrently creating, unattested surface remains completely unobserved here.
        status.has_page = app.get_webview(&self.labels.page).is_some();
        status.open = status.open && status.has_page;
        if !status.has_page {
            status.loading = false;
        } else if let Ok(page) = self.attested_page(true) {
            if let Ok(url) = page.url() {
                merge_observed_url(&mut status, pending_navigation, url.as_str());
            }
            if let Ok(size) = page.size() {
                let scale = page
                    .window()
                    .scale_factor()
                    .unwrap_or(1.0)
                    .max(f64::EPSILON);
                let logical: LogicalSize<f64> = size.to_logical(scale);
                status.viewport = BrowserViewport {
                    width: logical.width.round().clamp(0.0, u32::MAX as f64) as u32,
                    height: logical.height.round().clamp(0.0, u32::MAX as f64) as u32,
                };
            }
        }
        status
    }

    /// Address-bar navigation validates the target before claiming the shared page for the user.
    pub fn navigate_as_user(&self, url: &str) -> Result<BrowserStatus, String> {
        let parsed = parse_browser_url(url, self.security_level())?;
        self.with_user_control(|| {
            let _lifecycle = self.lock_lifecycle();
            self.navigate_parsed(parsed)?;
            Ok(self.status())
        })
    }

    /// Displays a user-picked local file by mapping a throwaway staging directory onto a virtual
    /// host name, so the address the page commits is an ordinary https URL.
    ///
    /// `with_user_control` is what keeps this a user-owned page, exactly like address-bar
    /// navigation; nothing here widens what [`is_navigation_allowed`] admits.
    pub(crate) fn preview_local_file(
        &self,
        app_data: &Path,
        picked: &Path,
    ) -> Result<BrowserStatus, String> {
        let staged = browser_file_preview::stage(app_data, picked)?;
        self.with_user_control(|| {
            let _lifecycle = self.lock_lifecycle();
            let page = match self.page() {
                Ok(page) => page,
                Err(error) => {
                    browser_file_preview::discard(&staged.directory);
                    return Err(error);
                }
            };
            if let Err(error) = page.with_native_tail(|native, permit| {
                browser_profile_data::map_virtual_host_folder(
                    native,
                    permit,
                    browser_file_preview::PREVIEW_VIRTUAL_HOST,
                    &staged.directory,
                )
            }) {
                browser_file_preview::discard(&staged.directory);
                return Err(error);
            }
            let replaced = self.lock_state().file_preview.replace(FilePreviewGrant {
                directory: staged.directory.clone(),
                url: staged.url.to_string(),
            });
            if let Some(replaced) = replaced {
                browser_file_preview::discard(&replaced.directory);
            }
            if let Err(error) = self.navigate_parsed(staged.url.clone()) {
                let abandoned = self.lock_state().file_preview.take();
                if let Some(abandoned) = abandoned {
                    release_file_preview(&page, abandoned);
                }
                return Err(error);
            }
            Ok(self.status())
        })
    }

    fn navigate_parsed(&self, url: Url) -> Result<(), String> {
        if !self.navigation_policy.allows(&url, self.security_level()) {
            return Err(match self.navigation_policy.as_ref() {
                BrowserNavigationPolicy::Ordinary => {
                    format!("unsafe browser navigation was blocked: {url}")
                }
            });
        }
        let page = self.page()?;
        self.hide_agent_pointer();
        {
            let mut state = self.lock_state();
            begin_navigation(&mut state, PendingNavigation::New, Some(url.as_str()));
        }
        if let Err(error) = page.navigate(url) {
            let message = format!("browser navigation failed: {error}");
            rollback_navigation_state(&mut self.lock_state(), message.clone());
            return Err(message);
        }
        Ok(())
    }

    fn resume_for_navigation_action(&self, navigation: PendingNavigation) -> Result<bool, String> {
        let status = self.status();
        match navigation_resume_plan(&status, self.has_retained_page(), navigation) {
            NavigationResumePlan::Continue => Ok(false),
            NavigationResumePlan::ResumeNativeThenContinue => {
                // Native sleep preserves the controller, history, sessionStorage, form state, and
                // JS heap. Resume it in place, then let the requested history operation run.
                self.prepare(None)?;
                Ok(false)
            }
            NavigationResumePlan::ResumeColdCompletesReload => {
                // Cold close cannot retain native history. Recreating the saved URL after restoring
                // the Cookie handoff is itself the reload; issuing page.reload() afterwards would
                // make an unnecessary second request.
                self.prepare(None)?;
                Ok(true)
            }
            NavigationResumePlan::ColdHistoryUnavailable => {
                Err("cold-suspended pages do not retain native back/forward history".into())
            }
        }
    }

    fn navigate_history_with_resume(
        &self,
        navigation: PendingNavigation,
    ) -> Result<BrowserStatus, String> {
        if self.resume_for_navigation_action(navigation)? {
            return Ok(self.status());
        }
        match navigation {
            PendingNavigation::Back => self.back(),
            PendingNavigation::Forward => self.forward(),
            PendingNavigation::Reload => self.reload(),
            PendingNavigation::New => {
                Err("new-page navigation cannot be executed as a history operation".into())
            }
        }
    }

    pub fn back(&self) -> Result<BrowserStatus, String> {
        self.hide_agent_pointer();
        {
            let mut state = self.lock_state();
            if !state.status.can_go_back {
                return Err("browser has no back history".into());
            }
            begin_navigation(&mut state, PendingNavigation::Back, None);
        }
        if let Err(error) = self.eval_value("history.back(); return true;", EVAL_TIMEOUT) {
            rollback_navigation_state(&mut self.lock_state(), error.clone());
            return Err(error);
        }
        Ok(self.status())
    }

    pub fn forward(&self) -> Result<BrowserStatus, String> {
        self.hide_agent_pointer();
        {
            let mut state = self.lock_state();
            if !state.status.can_go_forward {
                return Err("browser has no forward history".into());
            }
            begin_navigation(&mut state, PendingNavigation::Forward, None);
        }
        if let Err(error) = self.eval_value("history.forward(); return true;", EVAL_TIMEOUT) {
            rollback_navigation_state(&mut self.lock_state(), error.clone());
            return Err(error);
        }
        Ok(self.status())
    }

    pub fn reload(&self) -> Result<BrowserStatus, String> {
        self.hide_agent_pointer();
        {
            let mut state = self.lock_state();
            begin_navigation(&mut state, PendingNavigation::Reload, None);
        }
        let page = match self.page() {
            Ok(page) => page,
            Err(error) => {
                rollback_navigation_state(&mut self.lock_state(), error.clone());
                return Err(error);
            }
        };
        if let Err(error) = page.reload() {
            let message = format!("failed to reload the browser page: {error}");
            rollback_navigation_state(&mut self.lock_state(), message.clone());
            return Err(message);
        }
        Ok(self.status())
    }

    pub fn stop(&self) -> Result<BrowserStatus, String> {
        self.eval_value("window.stop(); return true;", EVAL_TIMEOUT)?;
        let mut state = self.lock_state();
        state.status.loading = false;
        state.pending_navigation = None;
        state.pending_previous_url = None;
        Ok(state.status.clone())
    }

    /// Hides the user-facing browser surface while preserving its page and history.
    /// Conversation WebViews are released only during application shutdown.
    ///
    /// The page is parked as the first thing that touches it. It used to slide out of the panel
    /// first, which meant a live remote page kept painting above the renderer for the length of
    /// that animation — and the pane it belonged to was already gone from the React layout by
    /// then, because the renderer removes it in the same tick that it sends this command. Nothing
    /// in the renderer animates on the way out, so there was never a box for the page to slide
    /// out of.
    pub fn hide(&self) -> Result<BrowserStatus, String> {
        let _lifecycle = self.lock_lifecycle();
        // A renderer teardown must restore both trusted overlays before the
        // remote surface disappears. Delayed pointer timers are invalidated by
        // the generation bump inside this helper.
        self.hide_agent_pointer();
        #[cfg(test)]
        {
            let mut state = self.lock_state();
            state.hide_attempts = state.hide_attempts.saturating_add(1);
            if let Some(error) = state.hide_failure.clone() {
                return Err(error);
            }
            if state.synthetic_surface {
                restore_control_after_menu(&mut state);
                clear_page_cover(&mut state);
                state.renderer_presentation_generation = None;
                state.status.open = false;
                state.status.loading = false;
                return Ok(state.status.clone());
            }
        }
        let app = self.app_handle()?;
        let host = self.lock_state().host;
        let page = if app.get_webview(&self.labels.page).is_some() {
            Some(self.attested_page(true)?)
        } else {
            None
        };
        // Sink the page before anything else touches it. A page hidden from the user stays live
        // for the Agent (see `AttestedPage::park`); only a sleeping page is truly hidden, by the
        // sleep transition itself. Every native round trip made before it sinks is one more frame
        // of live remote content painted over a renderer layout that has already let the pane go.
        let parked = page.as_ref().map(|page| {
            match (host, app.get_window(MAIN_WINDOW_LABEL)) {
                (Some(BrowserHost::MainPanel), Some(window)) => {
                    let panel_bounds = self.lock_state().panel_bounds;
                    let layout = browser_host_layout(&window, BrowserHost::MainPanel, panel_bounds);
                    page.park(layout)
                }
                // A detached host is hidden as a whole below; its controller stays visible so
                // the page keeps compositing.
                (Some(BrowserHost::DetachedWindow), _) => Ok(()),
                _ => page.hide(),
            }
        });
        // The pane the surfaces were drawn into is already gone, so the claim goes whether or not
        // the page sank: an occlusion left standing on a page that failed to park would keep the
        // next presentation of it stacked underneath a still frame nobody is painting any more.
        {
            let mut state = self.lock_state();
            clear_page_cover(&mut state);
            restore_control_after_menu(&mut state);
        }
        if let Some(Err(error)) = parked {
            return Err(format!("收起内置浏览器页面失败: {error}"));
        }
        if host == Some(BrowserHost::DetachedWindow) {
            let window = app
                .get_window(&self.labels.window)
                .ok_or_else(|| "内置浏览器独立窗口已丢失".to_owned())?;
            window
                .hide()
                .map_err(|error| format!("收起内置浏览器失败: {error}"))?;
        }

        let mut state = self.lock_state();
        restore_control_after_menu(&mut state);
        state.renderer_presentation_generation = None;
        state.status.has_page = page.is_some() && !state.status.suspended;
        state.status.open = false;
        if !state.status.has_page {
            state.status.loading = false;
        }
        Ok(state.status.clone())
    }

    fn set_panel_bounds(&self, bounds: BrowserPanelBounds) -> Result<BrowserStatus, String> {
        let _lifecycle = self.lock_lifecycle();
        let (app, host, has_page) = {
            let mut state = self.lock_state();
            state.panel_bounds = Some(bounds);
            (state.app.clone(), state.host, state.status.has_page)
        };

        // Publishing geometry is allowed before a page exists. It is retained and applied by the
        // next explicit browser open without creating remote content as a side effect.
        if !has_page {
            let mut state = self.lock_state();
            state.status.open = false;
            state.renderer_presentation_generation = None;
            return Ok(state.status.clone());
        }
        let app =
            app.ok_or_else(|| "BrowserRuntime 尚未在 Tauri setup 中注入 AppHandle".to_owned())?;
        let host = host.ok_or_else(|| "内置浏览器尚未打开".to_owned())?;
        let page = if app.get_webview(&self.labels.page).is_some() {
            self.attested_page(true)?
        } else {
            let mut state = self.lock_state();
            state.status.has_page = false;
            state.status.open = false;
            state.status.loading = false;
            state.renderer_presentation_generation = None;
            return Ok(state.status.clone());
        };

        if host == BrowserHost::MainPanel && bounds.width >= 1.0 && bounds.height >= 1.0 {
            let window = app
                .get_window(MAIN_WINDOW_LABEL)
                .ok_or_else(|| "the embedded browser host window was lost".to_owned())?;
            let viewport = resize_page_placed(
                host,
                &window,
                &page,
                window.inner_size().ok(),
                Some(bounds),
                !bounds.visible,
            );
            self.lock_state().status.viewport = viewport;
            if !bounds.visible {
                // The pane whose surfaces were over this page has let it go, so nothing is left
                // to keep it stacked under: the next presentation must be free to raise it.
                let mut state = self.lock_state();
                clear_page_cover(&mut state);
                restore_control_after_menu(&mut state);
            }
        }

        if bounds.visible {
            page.show()
                .map_err(|error| format!("显示内置浏览器页面失败: {error}"))?;
            // Geometry changes do not raise a sunk page: a pane that resizes while a dialog is
            // open must not put the page over the dialog on its way back to visible, and one that
            // resizes while the pane is painting a still must not swap that still for the live
            // page mid-drag.
            //
            // The flags are read into a binding first because a `self.lock_state()` temporary in
            // argument position stays alive for the whole statement, and restacking blocks on the
            // WebView2 UI thread. That thread runs this session's own layout handlers, which take
            // this same lock, so holding it across the round trip makes the restack wait out its
            // two-second deadline and fail — and freezes the window for as long as it waits.
            let parked = page_parked(&self.lock_state());
            page.set_stacking(parked)?;
            if host == BrowserHost::DetachedWindow {
                app.get_window(&self.labels.window)
                    .ok_or_else(|| "内置浏览器独立窗口已丢失".to_owned())?
                    .show()
                    .map_err(|error| format!("显示内置浏览器失败: {error}"))?;
            }
        } else if host == BrowserHost::MainPanel {
            let window = app
                .get_window(MAIN_WINDOW_LABEL)
                .ok_or_else(|| "the embedded browser host window was lost".to_owned())?;
            let layout = browser_host_layout(&window, host, Some(bounds));
            page.park(layout)
                .map_err(|error| format!("收起内置浏览器页面失败: {error}"))?;
        } else if host == BrowserHost::DetachedWindow {
            // Hide the host window only; see `park_attested_page`.
            app.get_window(&self.labels.window)
                .ok_or_else(|| "内置浏览器独立窗口已丢失".to_owned())?
                .hide()
                .map_err(|error| format!("收起内置浏览器失败: {error}"))?;
        } else {
            page.hide()
                .map_err(|error| format!("收起内置浏览器页面失败: {error}"))?;
        }

        let mut state = self.lock_state();
        state.status.has_page = true;
        state.status.open = bounds.visible;
        if !bounds.visible {
            state.renderer_presentation_generation = None;
        }
        Ok(state.status.clone())
    }

    fn shutdown(&self) -> Result<(), String> {
        let _lifecycle = self.lock_lifecycle();
        let (app, host, injected_failure) = {
            let mut state = self.lock_state();
            state.terminated = true;
            (state.app.clone(), state.host, {
                #[cfg(test)]
                {
                    state.shutdown_failure.clone()
                }
                #[cfg(not(test))]
                {
                    None::<String>
                }
            })
        };
        if let Some(error) = injected_failure {
            let mut state = self.lock_state();
            state.status.open = false;
            state.status.loading = false;
            state.status.error = Some(error.clone());
            return Err(error);
        }
        let mut errors = Vec::new();
        // Refuse new controller operations and wait for every existing page/CDP permit, then
        // retain the resulting release-only token through close/destroy and native label drain.
        let teardown = match self.invalidate_webview2_controller() {
            Ok(teardown) => teardown,
            Err(error) => {
                errors.push(error);
                None
            }
        };
        if let Some(app) = app {
            if let Err(error) =
                self.retire_native_surface_and_wait(&app, host, true, teardown.as_ref())
            {
                errors.push(error);
            }
        }
        if errors.is_empty() {
            // Native labels are gone, so no further controller callback may legitimately use the
            // tab. Revoke every cloned Chromium control before the profile directory is removed by
            // the manager-level close path.
            self.revoke_webview2_profile_claim();
            reset_closed_state(&mut self.lock_state());
            Ok(())
        } else {
            let error = errors.join("；");
            let mut state = self.lock_state();
            state.status.open = false;
            state.status.loading = false;
            state.status.error = Some(error.clone());
            // Preserve the native host/page state for a later idempotent retry. In particular, a
            // failed detached-window destroy must not lose the fact that the old label is owned.
            Err(error)
        }
    }

    pub fn set_zoom(&self, factor: f64) -> Result<BrowserStatus, String> {
        if !factor.is_finite() || !(0.25..=5.0).contains(&factor) {
            return Err("浏览器缩放比例必须在 0.25 到 5.0 之间".into());
        }
        self.page()?
            .set_zoom(factor)
            .map_err(|error| format!("设置浏览器缩放失败: {error}"))?;
        let mut state = self.lock_state();
        state.status.zoom = factor;
        Ok(state.status.clone())
    }

    /// Trusted-chrome viewport emulation, driven by the preview pane's viewport menu.
    ///
    /// `None` is the menu's "Responsive" entry. The source offers no desktop preset: clearing the
    /// device-metrics override *is* the reset, and it also drops the mobile user agent and touch
    /// emulation. A size stays on the tab across reloads until another call replaces or clears it.
    pub fn set_emulated_viewport(
        &self,
        size: Option<BrowserViewport>,
    ) -> Result<BrowserStatus, String> {
        match size {
            Some(size) => {
                if !browser_viewport_in_range(size) {
                    return Err(browser_viewport_action_error());
                }
                self.preview_set_viewport(size.width, size.height)
            }
            None => self.preview_clear_viewport(),
        }
        .map_err(|error| format!("设置浏览器视口失败: {error}"))?;
        Ok(self.status())
    }

    /// Keeps Mework's native about:blank page in sync with the trusted application theme.
    pub fn set_ui_theme(&self, theme: &str) -> Result<BrowserStatus, String> {
        if !matches!(theme, "day" | "night") {
            return Err("浏览器主题必须是 day 或 night".into());
        }
        {
            let mut state = self.lock_state();
            let generation = state
                .ui_preferences_generation
                .checked_add(1)
                .filter(|generation| *generation <= MAX_UI_PREFERENCE_GENERATION)
                .ok_or_else(|| "浏览器界面偏好 generation 已耗尽".to_owned())?;
            state.ui_theme = Some(theme.to_owned());
            state.ui_preferences_generation = generation;
        }
        self.apply_preferences_to_current_start_page()?;
        Ok(self.status())
    }

    /// Keeps the native start page aligned with Mework's explicit UI language instead of the
    /// operating-system language exposed by Chromium.
    pub fn set_ui_language(&self, language: &str) -> Result<BrowserStatus, String> {
        let language = match language.trim().to_ascii_lowercase().as_str() {
            value if value.starts_with("zh") => "zh-CN",
            value if value.starts_with("en") => "en-US",
            _ => return Err("浏览器界面语言必须是 zh 或 en".into()),
        };
        {
            let mut state = self.lock_state();
            let generation = state
                .ui_preferences_generation
                .checked_add(1)
                .filter(|generation| *generation <= MAX_UI_PREFERENCE_GENERATION)
                .ok_or_else(|| "浏览器界面偏好 generation 已耗尽".to_owned())?;
            state.ui_language = Some(language.to_owned());
            state.ui_preferences_generation = generation;
        }
        self.apply_preferences_to_current_start_page()?;
        Ok(self.status())
    }

    /// Replays trusted preferences only into the Mework-owned about:blank document. The values
    /// are still persisted when the page is absent, suspended, or navigating; the Finished
    /// callback applies them when the next start page commits.
    fn apply_preferences_to_current_start_page(&self) -> Result<(), String> {
        let (app, suspended, theme, language, generation) = {
            let state = self.lock_state();
            (
                state.app.clone(),
                state.status.suspended,
                state.ui_theme.clone(),
                state.ui_language.clone(),
                state.ui_preferences_generation,
            )
        };
        if suspended {
            return Ok(());
        }
        let Some(app) = app else {
            return Ok(());
        };
        if app.get_webview(&self.labels.page).is_none() {
            return Ok(());
        }
        let page = self.page()?;
        if page.url().ok().as_ref().map(Url::as_str) != Some(DEFAULT_URL) {
            return Ok(());
        }
        page.eval(&start_page_preferences_script(
            theme.as_deref(),
            language.as_deref(),
            generation,
        ))
        .map_err(|error| format!("同步浏览器开始页偏好失败: {error}"))
    }

    pub fn devtools(&self, open: bool) -> Result<Value, String> {
        let page = self.page()?;
        if open {
            page.open_devtools()?;
        } else {
            page.close_devtools()?;
        }
        Ok(json!({"open": open}))
    }

    pub fn clear_data(&self) -> Result<Value, String> {
        let page = self.page()?;
        page.with_native_tail(|native, permit| {
            browser_profile_data::clear_all_browsing_data(
                native,
                permit,
                BROWSING_DATA_CLEAR_TIMEOUT,
            )
        })?;
        // Clearing profile storage does not clear the current document's live password input.
        // Keep the Rust-side taint until trusted UI releases it or navigation leaves the origin.
        Ok(json!({"completed": true}))
    }

    /// Runs a trusted browser-chrome operation while persistently assigning the page to the user.
    ///
    /// Callers must not invoke an operation that acquires `automation` again from inside this
    /// closure. Credential fill and data import own that lock directly and call
    /// [`mark_user_control`](Self::mark_user_control) after acquiring it.
    pub(crate) fn with_user_control<T>(
        &self,
        operation: impl FnOnce() -> Result<T, String>,
    ) -> Result<T, String> {
        let _automation = lock_unpoison(&self.automation);
        self.mark_user_control();
        operation()
    }

    /// Explicit trusted-UI takeover. If an agent tool is running, this waits for its atomic
    /// automation section to finish before changing ownership.
    pub fn take_user_control(&self) -> BrowserStatus {
        let _automation = lock_unpoison(&self.automation);
        self.mark_user_control();
        self.status()
    }

    /// Explicit trusted handoff. Credential protection is released in the same atomic transition,
    /// so there is no interval where an agent can observe a password-filled page prematurely.
    ///
    /// This is a user action from trusted chrome, so it also stands in for the takeover prompt on
    /// the committed origin. Asking again immediately afterwards would be asking the same person
    /// the same question twice.
    pub fn handoff_to_agent(&self) -> BrowserStatus {
        let _automation = lock_unpoison(&self.automation);
        {
            let mut state = self.lock_state();
            state.credential_takeover_grant = browser_url_origin(&state.status.url);
            let released = BrowserControlStatus {
                owner: BrowserControlOwner::Available,
                updated_at_ms: Utc::now().timestamp_millis(),
                ..BrowserControlStatus::default()
            };
            if state.occluded {
                state.menu_control_before_open = Some(released);
                state.status.control = BrowserControlStatus {
                    owner: BrowserControlOwner::User,
                    updated_at_ms: Utc::now().timestamp_millis(),
                    ..BrowserControlStatus::default()
                };
            } else {
                state.status.control = released;
            }
        }
        self.status()
    }

    fn mark_user_control(&self) {
        self.hide_agent_pointer();
        let control = BrowserControlStatus {
            owner: BrowserControlOwner::User,
            updated_at_ms: Utc::now().timestamp_millis(),
            ..BrowserControlStatus::default()
        };
        let mut state = self.lock_state();
        state.user_has_controlled = true;
        if state.occluded {
            state.menu_control_before_open = Some(control.clone());
        }
        state.status.control = control;
    }

    /// Origin at which this page currently carries the user's sign-in material, if any.
    ///
    /// The page is credential-bearing when the profile actually holds cookies for the committed
    /// URL — that is what "already signed in" means for the built-in browser's per-tab profile,
    /// and those cookies are exactly what a takeover would let the model act with.
    ///
    /// Errors are returned rather than swallowed: a caller deciding whether to prompt must not read
    /// a failed cookie query as "no credentials here".
    pub(crate) fn credentialed_origin(&self) -> Result<Option<String>, String> {
        let committed = self.lock_state().status.url.clone();
        let Some(origin) = browser_url_origin(&committed) else {
            return Ok(None);
        };
        let url = Url::parse(&committed).map_err(|_| {
            "could not parse the current page address to determine sign-in state".to_owned()
        })?;
        if self.page_holds_cookies_for(&url)? {
            return Ok(Some(origin));
        }
        Ok(None)
    }

    /// Whether Chromium has at least one cookie it would send to `url`.
    ///
    /// This deliberately reads only the count. Cookie names and values never leave the host here;
    /// the decision this feeds is "ask the user or not", which needs no cookie content.
    fn page_holds_cookies_for(&self, url: &Url) -> Result<bool, String> {
        #[cfg(windows)]
        {
            let _automation = lock_unpoison(&self.automation);
            let page = match self.page() {
                Ok(page) => page,
                // A suspended or not-yet-created page holds no live document to take over. The
                // takeover prompt belongs to the navigation that follows, not to this empty state.
                Err(_) => return Ok(false),
            };
            let params = serde_json::to_string(&json!({ "urls": [url.as_str()] }))
                .map_err(|_| "could not encode the current page cookie query".to_owned())?;
            let control = self.webview2_control()?;
            let response = call_devtools_protocol(
                &control,
                &page,
                "Network.getCookies",
                &params,
                EVAL_TIMEOUT,
                &|| false,
            )
            .map_err(|_| {
                "could not confirm whether the current page carries sign-in cookies".to_owned()
            })?;
            Ok(response
                .get("cookies")
                .and_then(Value::as_array)
                .is_some_and(|cookies| !cookies.is_empty()))
        }
        #[cfg(not(windows))]
        {
            let _ = url;
            Ok(false)
        }
    }

    /// Records the user's approval for the Agent to drive this page at `origin`.
    pub(crate) fn grant_credential_takeover(&self, origin: &str) {
        let mut state = self.lock_state();
        state.credential_takeover_grant = Some(origin.to_owned());
        state.status.control = BrowserControlStatus {
            owner: BrowserControlOwner::Agent,
            updated_at_ms: Utc::now().timestamp_millis(),
            ..BrowserControlStatus::default()
        };
    }

    /// Records the effective security level of the conversation driving this
    /// session. Written before a browser tool runs so the asynchronous
    /// navigation callbacks can read it later.
    pub(crate) fn set_security_level(&self, level: SecurityLevel) {
        self.lock_state().security_level = level;
    }

    fn security_level(&self) -> SecurityLevel {
        self.lock_state().security_level
    }

    /// Whether the user has ever driven this page from trusted chrome.
    pub(crate) fn user_has_ever_controlled(&self) -> bool {
        self.lock_state().user_has_controlled
    }

    /// Whether the user has already approved an Agent takeover of this page at `origin`.
    pub(crate) fn credential_takeover_granted(&self, origin: &str) -> bool {
        self.lock_state().credential_takeover_grant.as_deref() == Some(origin)
    }

    fn begin_agent_control(&self, tool: PreviewTool) -> Result<AgentControlGuard, String> {
        let tool_name = tool.as_str().to_owned();
        {
            let mut state = self.lock_state();
            if state.occluded {
                state.status.control = BrowserControlStatus {
                    owner: BrowserControlOwner::User,
                    handoff_requested: true,
                    requested_tool: Some(tool_name.clone()),
                    updated_at_ms: Utc::now().timestamp_millis(),
                };
                if let Some(previous) = state.menu_control_before_open.as_mut() {
                    if previous.owner == BrowserControlOwner::User {
                        previous.handoff_requested = true;
                        previous.requested_tool = Some(tool_name.clone());
                        previous.updated_at_ms = Utc::now().timestamp_millis();
                    }
                }
                return Err(format!(
                    "{tool_name} is temporarily blocked because trusted UI is covering the browser page; wait for the user to dismiss it"
                ));
            }
            if state.status.control.owner == BrowserControlOwner::User {
                state.status.control.handoff_requested = true;
                state.status.control.requested_tool = Some(tool_name.clone());
                state.status.control.updated_at_ms = Utc::now().timestamp_millis();
                return Err(format!(
                    "{tool_name} is waiting for the user's permission to continue; ask the user explicitly in the conversation"
                ));
            }
            state.status.control = BrowserControlStatus {
                owner: BrowserControlOwner::Agent,
                updated_at_ms: Utc::now().timestamp_millis(),
                ..BrowserControlStatus::default()
            };
            state.status.agent_activity = Some(BrowserAgentActivity {
                tool: tool_name.to_owned(),
                source: "mework-cdp".to_owned(),
                active: true,
                updated_at_ms: Utc::now().timestamp_millis(),
            });
        }
        Ok(AgentControlGuard {
            state: Arc::clone(&self.state),
            tool_name: tool_name.to_owned(),
        })
    }

    pub fn find_text(&self, query: &str) -> Result<Value, String> {
        let query = query.trim();
        if query.is_empty() {
            return Err("查找内容不能为空".into());
        }
        if query.chars().count() > 2_048 {
            return Err("查找内容不能超过 2048 个字符".into());
        }
        let literal = js_string_literal(query)?;
        self.eval_value(
            &format!("return window.find({literal}, false, false, true, false, false, false);"),
            EVAL_TIMEOUT,
        )
    }

    pub fn print_page(&self) -> Result<Value, String> {
        self.eval_value("window.print(); return true;", EVAL_TIMEOUT)
    }

    /// `browser_action` with `occlude`: the renderer reports whether any trusted surface is drawn
    /// over the page, and the page is stacked to match.
    ///
    /// The two directions do not fail the same way, so they are not written the same way. A page
    /// left on top of a dialog is unreadable and unreachable and the user cannot dismiss what they
    /// cannot see; a page left underneath one shows a frozen still that the next uncover repairs.
    /// The flag is therefore committed before the restack is attempted and kept even when the
    /// restack fails, so that whatever presents this page next reads it and stacks correctly
    /// rather than inheriting a page that believes nothing is covering it.
    pub fn set_occluded(&self, occluded: bool) -> Result<BrowserStatus, String> {
        let _automation = lock_unpoison(&self.automation);
        // Every `self.status()` below is deliberately outside the state lock. `status()` locks the
        // state itself, and this is a plain non-reentrant `Mutex`: reading a status while still
        // holding the guard deadlocks the calling thread — and because that thread is holding the
        // automation lock too, every later browser command queues behind it and the app stops
        // responding. Nearly every method on this type locks the state somewhere, so the usable
        // rule is to call nothing at all on `self` while a guard is alive; the presented check
        // below reads into a binding for that reason, not because the `if` itself would hold one.
        let changed = {
            let mut state = self.lock_state();
            if state.occluded == occluded {
                false
            } else {
                state.occluded = occluded;
                state.status.occluded = occluded;
                if occluded {
                    // Trusted chrome over the page takes it out of agent automation for as long as
                    // it is up: the user is looking at a still frame, and an Agent click landing on
                    // the live page behind it would be a click nobody could see.
                    let previous = state.status.control.clone();
                    state.menu_control_before_open = Some(previous.clone());
                    state.status.control = BrowserControlStatus {
                        owner: BrowserControlOwner::User,
                        updated_at_ms: Utc::now().timestamp_millis(),
                        ..previous
                    };
                } else {
                    restore_control_after_menu(&mut state);
                }
                true
            }
        };
        if !changed {
            return Ok(self.status());
        }
        if occluded {
            self.hide_agent_pointer();
        }
        self.restack_presented_page()
    }

    /// Records whether the pane is painting a still frame of the page in the page's own place.
    ///
    /// Stacking and nothing else. Projection is the preview pane's resting state — the pointer is
    /// somewhere else in the app, so the user is shown a picture rather than a native window that
    /// would paint over every menu and rounded corner around it — and a resting state must not
    /// cost the page anything. So unlike `set_occluded` this leaves page ownership alone, does not
    /// hide the agent pointer, and keeps the page in agent automation: the Agent goes on driving a
    /// projected page at foreground speed, and each frame the pane captures is what the user sees
    /// it doing.
    pub fn set_projected(&self, projected: bool) -> Result<BrowserStatus, String> {
        let _automation = lock_unpoison(&self.automation);
        // Same deadlock rule as `set_occluded`: nothing is called on `self` while a guard is alive.
        let changed = {
            let mut state = self.lock_state();
            if state.projected == projected {
                false
            } else {
                state.projected = projected;
                state.status.projected = projected;
                true
            }
        };
        if !changed {
            return Ok(self.status());
        }
        self.restack_presented_page()
    }

    /// Applies the current sink decision to the live page, if there is one on screen to apply it to.
    fn restack_presented_page(&self) -> Result<BrowserStatus, String> {
        // A page that is not presented is already stacked at the bottom and stays there; the flags
        // alone are what the next presentation reads.
        let Ok(page) = self.attested_page(true) else {
            return Ok(self.status());
        };
        let presented = self.lock_state().status.open;
        if !presented {
            return Ok(self.status());
        }
        let parked = page_parked(&self.lock_state());
        page.set_stacking(parked)?;
        Ok(self.status())
    }

    fn app_handle(&self) -> Result<AppHandle, String> {
        self.lock_state()
            .app
            .clone()
            .ok_or_else(|| "BrowserRuntime has no AppHandle injected during Tauri setup".to_owned())
    }

    fn page(&self) -> Result<AttestedPage, String> {
        self.attested_page(false)
    }

    fn attested_page(&self, allow_suspended: bool) -> Result<AttestedPage, String> {
        if !allow_suspended && self.lock_state().status.suspended {
            return Err("this browser task is suspended; call preview_start or restore it from the task card".into());
        }
        let permit = self.webview2_control()?.permit().map_err(|error| {
            format!("Chromium native control capability is unavailable: {error}")
        })?;
        let page = self
            .app_handle()?
            .get_webview(&self.labels.page)
            .ok_or_else(|| "the embedded browser is not open".to_owned())?;
        Ok(AttestedPage {
            page,
            _permit: permit,
        })
    }

    fn lock_state(&self) -> MutexGuard<'_, RuntimeState> {
        lock_unpoison(&self.state)
    }

    fn lock_lifecycle(&self) -> MutexGuard<'_, ()> {
        lock_unpoison(&self.lifecycle)
    }
}

fn install_layout_handler(
    state: Arc<Mutex<RuntimeState>>,
    chromium_control: WebView2Control,
    host: BrowserHost,
    generation: u64,
    window: &Window,
    page: &AttestedPage,
) {
    let window = window.clone();
    let page = page.page.clone();
    window.clone().on_window_event(move |event| match event {
        WindowEvent::Resized(size) => {
            let Ok(permit) = chromium_control.permit() else {
                return;
            };
            let page = AttestedPage {
                page: page.clone(),
                _permit: permit,
            };
            if !layout_handler_is_current(&state, host, generation) {
                return;
            }
            let (panel_bounds, parked) = {
                let state = lock_unpoison(&state);
                (state.panel_bounds, !state.status.open)
            };
            let viewport =
                resize_page_placed(host, &window, &page, Some(*size), panel_bounds, parked);
            lock_unpoison(&state).status.viewport = viewport;
        }
        WindowEvent::ScaleFactorChanged { new_inner_size, .. } => {
            let Ok(permit) = chromium_control.permit() else {
                return;
            };
            let page = AttestedPage {
                page: page.clone(),
                _permit: permit,
            };
            if !layout_handler_is_current(&state, host, generation) {
                return;
            }
            let (panel_bounds, parked) = {
                let state = lock_unpoison(&state);
                (state.panel_bounds, !state.status.open)
            };
            let viewport = resize_page_placed(
                host,
                &window,
                &page,
                Some(*new_inner_size),
                panel_bounds,
                parked,
            );
            lock_unpoison(&state).status.viewport = viewport;
        }
        WindowEvent::Destroyed => {
            let Ok(_permit) = chromium_control.permit() else {
                return;
            };
            let mut state = lock_unpoison(&state);
            // The host window took the page with it, so no surface can still be drawn over one.
            // This runs even for a stale generation: the flag is presentation state that the
            // renderer republishes, and leaving it set would strand the next page underneath.
            clear_page_cover(&mut state);
            if state.host == Some(host) && state.layout_generation == generation {
                reset_closed_state(&mut state);
            }
        }
        _ => {}
    });
}

fn layout_handler_is_current(
    state: &Arc<Mutex<RuntimeState>>,
    host: BrowserHost,
    generation: u64,
) -> bool {
    let state = lock_unpoison(state);
    state.host == Some(host) && state.layout_generation == generation && state.status.has_page
}

fn resize_page(
    host: BrowserHost,
    window: &Window,
    page: &AttestedPage,
    physical_size: Option<PhysicalSize<u32>>,
    panel_bounds: Option<BrowserPanelBounds>,
) -> BrowserViewport {
    resize_page_placed(host, window, page, physical_size, panel_bounds, false)
}

/// `parked` keeps the page at its parked position (see `AttestedPage::park`) while still
/// giving it the size it would have on screen, so a window resize can never drag a page the user
/// closed back into view.
fn resize_page_placed(
    host: BrowserHost,
    window: &Window,
    page: &AttestedPage,
    physical_size: Option<PhysicalSize<u32>>,
    panel_bounds: Option<BrowserPanelBounds>,
    parked: bool,
) -> BrowserViewport {
    let size = physical_size
        .or_else(|| window.inner_size().ok())
        .unwrap_or_else(|| PhysicalSize::new(DEFAULT_WIDTH as u32, DEFAULT_HEIGHT as u32));
    let scale = window.scale_factor().unwrap_or(1.0).max(f64::EPSILON);
    let logical: LogicalSize<f64> = size.to_logical(scale);
    let layout = browser_layout_for_size(logical, host, panel_bounds);
    apply_page_layout(page, layout);
    // A parked page keeps its on-screen layout; what hides it is its stacking beneath the
    // trusted WebView, which a resize never changes.
    let _ = parked;
    layout.viewport()
}

fn browser_host_layout(
    window: &Window,
    host: BrowserHost,
    panel_bounds: Option<BrowserPanelBounds>,
) -> BrowserPageLayout {
    let scale = window.scale_factor().unwrap_or(1.0).max(f64::EPSILON);
    let logical = window
        .inner_size()
        .map(|size| size.to_logical(scale))
        .unwrap_or(LogicalSize::new(DEFAULT_WIDTH, DEFAULT_HEIGHT));
    browser_layout_for_size(logical, host, panel_bounds)
}

/// Covering the page never changes its geometry: the trusted surface is composited over a page
/// that keeps its exact size and position, so CDP/Playwright viewport coordinates stay stable
/// while it is up.
fn browser_layout_for_size(
    logical: LogicalSize<f64>,
    host: BrowserHost,
    panel_bounds: Option<BrowserPanelBounds>,
) -> BrowserPageLayout {
    let host_width = logical.width.max(1.0);
    let host_height = logical.height.max(1.0);
    if host == BrowserHost::DetachedWindow {
        return BrowserPageLayout {
            x: 0.0,
            y: 0.0,
            width: host_width,
            height: host_height,
        };
    }

    if let Some(bounds) = panel_bounds.filter(|bounds| bounds.width >= 1.0 && bounds.height >= 1.0)
    {
        // A native child WebView must never extend beyond its trusted host, even briefly while a
        // ResizeObserver update is in flight after the main window changes size.
        let x = bounds.x.clamp(0.0, (host_width - 1.0).max(0.0));
        let y = bounds.y.clamp(0.0, (host_height - 1.0).max(0.0));
        let width = bounds.width.clamp(1.0, (host_width - x).max(1.0));
        let total_height = bounds.height.clamp(1.0, (host_height - y).max(1.0));
        let mut occluded_top = bounds.occluded_top.unwrap_or(0.0);
        occluded_top = occluded_top.clamp(0.0, (total_height - 1.0).max(0.0));
        return BrowserPageLayout {
            x,
            y: y + occluded_top,
            width,
            height: (total_height - occluded_top).max(1.0),
        };
    }

    // Compatibility layout until React publishes the measured sidebar rectangle.
    let width = BROWSER_PANEL_WIDTH.min(host_width).max(1.0);
    let top = BROWSER_TOOLBAR_HEIGHT.clamp(0.0, (host_height - 1.0).max(0.0));
    BrowserPageLayout {
        x: (host_width - width).max(0.0),
        y: top,
        width,
        height: (host_height - top).max(1.0),
    }
}

fn apply_page_layout(page: &AttestedPage, layout: BrowserPageLayout) {
    let _ = page.set_layout(layout);
}

/// Hands the page back to whoever owned it before a trusted surface covered it.
///
/// The saved owner is only ever recorded while the page is covered, so restoring unconditionally
/// is also the right no-op for a page nothing ever covered. Clearing the occlusion flag is left
/// to the caller: a lifecycle transition that drops the cover has its own reasons for saying so,
/// and this helper is called on paths that only ever change ownership.
fn restore_control_after_menu(state: &mut RuntimeState) {
    if let Some(previous) = state.menu_control_before_open.take() {
        state.status.control = previous;
    }
}

/// Whether the page's child window belongs at the bottom of the z-order.
///
/// The two reasons are independent and either one is sufficient: a trusted surface is drawn over
/// the page, or the pane is standing a still frame in for it. Only the union reaches the native
/// restack; everything else about the two states differs, which is why they are separate flags.
fn page_parked(state: &RuntimeState) -> bool {
    state.occluded || state.projected
}

/// Drops both reasons the page might be sunk, in state and in the polled status together.
///
/// Every path that takes the page away from the pane — sleeping, suspending, hiding, closing,
/// replacing — has to clear both. A page that goes away still carrying either flag comes back
/// stacked under a renderer that is no longer painting anything in its place, which is a page
/// that is simply invisible with nothing left to raise it.
fn clear_page_cover(state: &mut RuntimeState) {
    state.occluded = false;
    state.status.occluded = false;
    state.projected = false;
    state.status.projected = false;
}

fn reset_closed_state(state: &mut RuntimeState) {
    let zoom = state.status.zoom;
    let viewport = state.status.viewport;
    state.status = BrowserStatus {
        zoom,
        viewport,
        ..BrowserStatus::default()
    };
    state.history.clear();
    state.host = None;
    state.occluded = false;
    state.projected = false;
    state.menu_control_before_open = None;
    state.renderer_presentation_generation = None;
    state.history_index = None;
    state.pending_navigation = None;
    state.pending_previous_url = None;
    state.cold_resume_target = None;
    state.cold_close_cookies = None;
    state.credential_takeover_grant = None;
    state.element_picker = ElementPickerState::default();
    discard_file_preview_staging(state);
}

#[cfg(all(windows, feature = "browser-dev"))]
struct BrowserProcessExitObservation {
    expected_process_id: u32,
    result: Result<(), String>,
}

#[cfg(all(windows, feature = "browser-dev"))]
fn register_browser_process_exit_handler(
    session: &BrowserSession,
    seen_processes: Arc<Mutex<HashSet<u32>>>,
    exit_sender: mpsc::Sender<BrowserProcessExitObservation>,
) -> Result<Option<u32>, String> {
    use std::sync::atomic::AtomicBool;

    use webview2_com::BrowserProcessExitedEventHandler;
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        ICoreWebView2BrowserProcessExitedEventHandler, ICoreWebView2Environment5,
        COREWEBVIEW2_BROWSER_PROCESS_EXIT_KIND_NORMAL,
    };
    use windows_core::Interface;

    const INSTALL_TIMEOUT: Duration = Duration::from_secs(5);

    let page = session.attested_page(true)?;
    let control = session.webview2_control()?;
    let completion_permit = control
        .permit()
        .map_err(|error| format!("WebView2 释放处理器安装能力不可用: {error}"))?;
    let release_observer = control
        .release_observer()
        .map_err(|error| format!("WebView2 退出观察能力不可用: {error}"))?;
    let (registration_sender, registration_receiver) =
        mpsc::sync_channel::<Result<Option<u32>, String>>(1);
    let cancelled = Arc::new(AtomicBool::new(false));
    let callback_cancelled = cancelled.clone();
    let native_page = &page.page;
    let scheduling = native_page.with_webview(move |platform| {
        // If installation dispatch outlives the caller's timeout, controller teardown must still
        // wait for this queued native operation to finish or be dropped.
        let _completion_permit = completion_permit;
        let result = (|| -> Result<Option<u32>, String> {
            if callback_cancelled.load(Ordering::Acquire) {
                return Err("WebView2 释放处理器安装已取消".to_owned());
            }
            let controller = platform.controller();
            let core = unsafe { controller.CoreWebView2() }
                .map_err(|error| format!("取得 WebView2 核心失败: {error}"))?;
            let mut process_id = 0u32;
            unsafe {
                core.BrowserProcessId(&mut process_id)
                    .map_err(|error| format!("读取 WebView2 浏览器进程标识失败: {error}"))?;
            }
            if process_id == 0 {
                return Err("WebView2 未报告有效的浏览器进程标识".to_owned());
            }
            let environment = platform
                .environment()
                .cast::<ICoreWebView2Environment5>()
                .map_err(|error| format!("当前 WebView2 不支持浏览器进程退出通知: {error}"))?;

            {
                let mut seen = lock_unpoison(&seen_processes);
                if !seen.insert(process_id) {
                    return Ok(None);
                }
            }
            if callback_cancelled.load(Ordering::Acquire) {
                lock_unpoison(&seen_processes).remove(&process_id);
                return Err("WebView2 释放处理器安装已取消".to_owned());
            }

            let event_token = Arc::new(Mutex::new(None::<i64>));
            let handler_token = event_token.clone();
            // WebView2 does not guarantee that the environment keeps the Rust event-handler
            // wrapper alive after registration. Retain one reference on the callback's own
            // main-thread lifetime island and break the cycle after the exit event unregisters
            // itself. Otherwise all Sender clones can disappear as soon as the registration
            // closure returns, making the release barrier report a disconnected channel.
            let retained_handler = Arc::new(Mutex::new(
                None::<ICoreWebView2BrowserProcessExitedEventHandler>,
            ));
            let callback_retained_handler = retained_handler.clone();
            // The event belongs to Environment5, whose final ordinary reference disappears when
            // the last controller closes. Keep that COM interface on the same callback lifetime
            // island as the handler; otherwise the browser process can exit after the environment
            // is released and WebView2 has nowhere left to deliver the registered notification.
            let retained_environment = Arc::new(Mutex::new(None::<ICoreWebView2Environment5>));
            let callback_retained_environment = retained_environment.clone();
            let release_observer = Arc::new(Mutex::new(Some(release_observer)));
            let callback_release_observer = release_observer.clone();
            let handler_sender = exit_sender.clone();
            let handler = BrowserProcessExitedEventHandler::create(Box::new(
                move |event_environment, event_args| {
                    let Some(_release_observer): Option<WebView2ReleaseObserverPermit> =
                        lock_unpoison(&callback_release_observer).take()
                    else {
                        lock_unpoison(&callback_retained_handler).take();
                        lock_unpoison(&callback_retained_environment).take();
                        let _ = handler_sender.send(BrowserProcessExitObservation {
                            expected_process_id: process_id,
                            result: Err(
                                "WebView2 退出通知缺少对应 controller 的观察能力".to_owned()
                            ),
                        });
                        return Ok(());
                    };
                    let event_result = (|| -> Result<(), String> {
                        let args = event_args
                            .as_ref()
                            .ok_or_else(|| "WebView2 退出通知缺少事件参数".to_owned())?;
                        let mut actual_process_id = 0u32;
                        let mut exit_kind = Default::default();
                        unsafe {
                            args.BrowserProcessId(&mut actual_process_id)
                                .map_err(|error| {
                                    format!("读取 WebView2 退出事件进程标识失败: {error}")
                                })?;
                            args.BrowserProcessExitKind(&mut exit_kind)
                                .map_err(|error| {
                                    format!("读取 WebView2 浏览器进程退出类型失败: {error}")
                                })?;
                        }
                        validate_browser_process_exit_values(
                            process_id,
                            actual_process_id,
                            exit_kind == COREWEBVIEW2_BROWSER_PROCESS_EXIT_KIND_NORMAL,
                        )
                    })();
                    let unregister_result = (|| -> Result<(), String> {
                        let environment = event_environment
                            .as_ref()
                            .ok_or_else(|| "WebView2 退出通知缺少 Environment".to_owned())?
                            .cast::<ICoreWebView2Environment5>()
                            .map_err(|error| {
                                format!("WebView2 退出通知无法取得 Environment5: {error}")
                            })?;
                        let token = lock_unpoison(&handler_token)
                            .take()
                            .ok_or_else(|| "WebView2 退出处理器缺少注销 token".to_owned())?;
                        unsafe {
                            environment
                                .remove_BrowserProcessExited(token)
                                .map_err(|error| format!("WebView2 退出处理器自注销失败: {error}"))
                        }
                    })();
                    let result = match (event_result, unregister_result) {
                        (Ok(()), Ok(())) => Ok(()),
                        (Err(event_error), Ok(())) => Err(event_error),
                        (Ok(()), Err(unregister_error)) => Err(unregister_error),
                        (Err(event_error), Err(unregister_error)) => {
                            Err(format!("{event_error}；{unregister_error}"))
                        }
                    };
                    lock_unpoison(&callback_retained_handler).take();
                    lock_unpoison(&callback_retained_environment).take();
                    let _ = handler_sender.send(BrowserProcessExitObservation {
                        expected_process_id: process_id,
                        result,
                    });
                    Ok(())
                },
            ));
            *lock_unpoison(&retained_handler) = Some(handler.clone());
            *lock_unpoison(&retained_environment) = Some(environment.clone());
            let mut token = 0i64;
            if let Err(error) =
                unsafe { environment.add_BrowserProcessExited(&handler, &mut token) }
            {
                lock_unpoison(&retained_handler).take();
                lock_unpoison(&retained_environment).take();
                lock_unpoison(&seen_processes).remove(&process_id);
                return Err(format!("安装 WebView2 浏览器进程退出处理器失败: {error}"));
            }
            *lock_unpoison(&event_token) = Some(token);
            Ok(Some(process_id))
        })();
        let _ = registration_sender.try_send(result);
    });
    scheduling.map_err(|error| format!("调度 WebView2 浏览器进程退出处理器失败: {error}"))?;

    match registration_receiver.recv_timeout(INSTALL_TIMEOUT) {
        Ok(result) => result,
        Err(mpsc::RecvTimeoutError::Timeout) => {
            cancelled.store(true, Ordering::Release);
            Err("等待 WebView2 浏览器进程退出处理器安装超时".to_owned())
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            Err("WebView2 浏览器进程退出处理器安装通道意外关闭".to_owned())
        }
    }
}

#[cfg(all(windows, feature = "browser-dev"))]
fn wait_for_browser_process_exits(
    receiver: mpsc::Receiver<BrowserProcessExitObservation>,
    mut expected_processes: HashSet<u32>,
    deadline: Instant,
) -> Result<(), String> {
    let mut errors = Vec::new();
    while !expected_processes.is_empty() {
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            errors.push(format!(
                "等待 {} 个 WebView2 浏览器进程退出超时",
                expected_processes.len()
            ));
            break;
        };
        match receiver.recv_timeout(remaining) {
            Ok(observation) => {
                if !expected_processes.remove(&observation.expected_process_id) {
                    errors.push(format!(
                        "收到未登记或重复的 WebView2 浏览器进程退出通知: {}",
                        observation.expected_process_id
                    ));
                }
                if let Err(error) = observation.result {
                    errors.push(error);
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                errors.push(format!(
                    "等待 {} 个 WebView2 浏览器进程退出超时",
                    expected_processes.len()
                ));
                break;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                errors.push(format!(
                    "仍有 {} 个 WebView2 浏览器进程未确认退出，通知通道已关闭",
                    expected_processes.len()
                ));
                break;
            }
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("；"))
    }
}

#[cfg(any(test, all(windows, feature = "browser-dev")))]
fn validate_browser_process_exit_values(
    expected_process_id: u32,
    actual_process_id: u32,
    exited_normally: bool,
) -> Result<(), String> {
    if actual_process_id != expected_process_id {
        return Err(format!(
            "WebView2 退出通知进程标识不匹配: 预期 {expected_process_id}，实际 {actual_process_id}"
        ));
    }
    if !exited_normally {
        return Err(format!(
            "WebView2 浏览器进程 {expected_process_id} 未以 NORMAL 类型退出"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod browser_process_exit_barrier_tests {
    use super::validate_browser_process_exit_values;

    #[test]
    fn release_barrier_accepts_only_matching_normal_exit() {
        assert!(validate_browser_process_exit_values(41, 41, true).is_ok());
        assert!(validate_browser_process_exit_values(41, 42, true)
            .unwrap_err()
            .contains("进程标识不匹配"));
        assert!(validate_browser_process_exit_values(41, 41, false)
            .unwrap_err()
            .contains("NORMAL"));
    }
}

fn page_load_is_pending(status: &BrowserStatus) -> bool {
    status.has_page && status.loading
}

/// A newly accepted navigation is asynchronous. Until its page-load callback settles, the live
/// WebView URL can still be the previous page and must not overwrite the accepted target reported
/// by `preview_start` or a trusted-UI open. Other navigation kinds intentionally keep
/// reporting the observed URL while their history movement settles.
fn merge_observed_url(
    status: &mut BrowserStatus,
    pending_navigation: Option<PendingNavigation>,
    observed_url: &str,
) {
    if pending_navigation != Some(PendingNavigation::New) {
        status.url = observed_url.to_owned();
    }
}

fn begin_navigation(
    state: &mut RuntimeState,
    navigation: PendingNavigation,
    target_url: Option<&str>,
) {
    state.pending_previous_url = Some(state.status.url.clone());
    state.pending_navigation = Some(navigation);
    state.status.loading = true;
    state.status.error = None;
    if let Some(target_url) = target_url {
        state.status.url = target_url.to_owned();
    }
}

fn rollback_navigation_state(state: &mut RuntimeState, error: String) {
    if let Some(previous_url) = state.pending_previous_url.take() {
        state.status.url = previous_url;
    }
    state.pending_navigation = None;
    state.status.loading = false;
    state.status.error = Some(error);
}

fn update_history_after_load(state: &mut RuntimeState, url: String) {
    state.pending_previous_url = None;
    match state.pending_navigation.take() {
        Some(PendingNavigation::Back) => {
            if let Some(index) = state.history_index.as_mut() {
                *index = index.saturating_sub(1);
                if *index < state.history.len() {
                    state.history[*index] = url;
                }
            }
        }
        Some(PendingNavigation::Forward) => {
            if let Some(index) = state.history_index.as_mut() {
                if *index + 1 < state.history.len() {
                    *index += 1;
                    state.history[*index] = url;
                }
            }
        }
        Some(PendingNavigation::Reload) => {
            if let Some(index) = state.history_index {
                if index < state.history.len() {
                    state.history[index] = url;
                }
            }
        }
        Some(PendingNavigation::New) | None => {
            let current_is_same = state
                .history_index
                .and_then(|index| state.history.get(index))
                .is_some_and(|current| current == &url);
            if !current_is_same {
                let next = state.history_index.map_or(0, |index| index + 1);
                state.history.truncate(next);
                state.history.push(url);
                state.history_index = Some(state.history.len() - 1);
            }
        }
    }
    sync_history_flags(state);
}

fn sync_history_flags(state: &mut RuntimeState) {
    state.status.can_go_back = state.history_index.is_some_and(|index| index > 0);
    state.status.can_go_forward = state
        .history_index
        .is_some_and(|index| index + 1 < state.history.len());
}

fn sanitize_title(title: &str) -> String {
    title
        .chars()
        .filter(|character| !character.is_control())
        .take(256)
        .collect::<String>()
        .trim()
        .to_owned()
}

fn combine_cleanup_results(
    first: Result<(), String>,
    second: Result<(), String>,
) -> Result<(), String> {
    match (first, second) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(first), Err(second)) => Err(format!("{first}；{second}")),
    }
}

fn lock_unpoison<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn try_lock_unpoison<T>(mutex: &Mutex<T>) -> Option<MutexGuard<'_, T>> {
    match mutex.try_lock() {
        Ok(guard) => Some(guard),
        Err(TryLockError::Poisoned(poisoned)) => Some(poisoned.into_inner()),
        Err(TryLockError::WouldBlock) => None,
    }
}

impl BrowserSession {
    // ----- actionability -----------------------------------------------------------------

    /// Waits (bounded) for the page to stop loading. Reaching the deadline is not an error; the
    /// caller simply observes `loading: true` in the returned status.
    fn wait_for_load(&self, timeout: Duration) -> BrowserStatus {
        let deadline = Instant::now() + timeout;
        loop {
            let status = self.status();
            if !page_load_is_pending(&status) || Instant::now() >= deadline {
                return status;
            }
            if self.has_modal_state() {
                return status;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Whether a held dialog or file chooser is pending on this page.
    fn has_modal_state(&self) -> bool {
        let state = self.lock_state();
        state.activity.pending_dialog.is_some() || state.activity.pending_file_chooser.is_some()
    }

    fn modal_states(&self) -> Vec<ModalState> {
        self.lock_state().activity.modal_states()
    }

    /// Watermarks taken before an interaction so the settle step can tell what the interaction
    /// caused from what had already happened.
    fn action_watch(&self) -> ActionWatch {
        ActionWatch::take(&self.lock_state())
    }

    /// The action-completion wait of `@playwright/mcp`, applied after an interaction:
    ///
    /// 1. let the page settle for `POST_ACTION_SETTLE`;
    /// 2. if the interaction started a main-frame navigation, wait for that document's `load`
    ///    (bounded by `POST_ACTION_NAVIGATION_LOAD`);
    /// 3. otherwise wait for the requests the interaction started to finish (bounded by
    ///    `POST_ACTION_NETWORK_QUIET`) and, if there were any, settle once more.
    ///
    /// A navigation the policy refused is reported instead of waited for, and a dialog or file
    /// chooser that opens meanwhile ends the wait at once: the page's JavaScript is blocked by it
    /// and only the action that clears it can make progress. Reaching a bound is never an error.
    fn settle_after_action(&self, watch: &ActionWatch) -> ActionAftermath {
        let mut aftermath = ActionAftermath::default();
        let loading_seen = std::cell::Cell::new(false);
        // Returns true when the wait must end now (refused navigation or modal state).
        let observe = |aftermath: &mut ActionAftermath| -> bool {
            let state = self.lock_state();
            if state.blocked_navigations != watch.blocked_navigations {
                // The action did cause a navigation; the policy refused it. Say so, rather
                // than returning a bare success the model would read as "the click did
                // nothing".
                aftermath.blocked = Some(
                    state
                        .status
                        .error
                        .clone()
                        .unwrap_or_else(|| "unsafe browser navigation was blocked".to_owned()),
                );
                return true;
            }
            let modal = state.activity.modal_states();
            if !modal.is_empty() {
                aftermath.modal_states = modal;
                return true;
            }
            if state.status.loading {
                loading_seen.set(true);
            }
            false
        };
        let settle_until = Instant::now() + POST_ACTION_SETTLE;
        while Instant::now() < settle_until {
            if observe(&mut aftermath) {
                return aftermath;
            }
            std::thread::sleep(POST_ACTION_POLL);
        }
        if observe(&mut aftermath) {
            return aftermath;
        }
        let (requests, committed) = {
            let state = self.lock_state();
            (
                watch.requests_since(&state.activity),
                watch.navigated_since(&state.activity),
            )
        };
        let navigated = loading_seen.get()
            || committed
            || requests
                .iter()
                .any(|(_, request)| request.main_frame_navigation);
        if navigated {
            aftermath.navigated = true;
            let deadline = Instant::now() + POST_ACTION_NAVIGATION_LOAD;
            loop {
                if observe(&mut aftermath) {
                    return aftermath;
                }
                // `loading` only turns on once the new document starts arriving (ContentLoading),
                // so its absence proves nothing until it has been seen; the load-event counter
                // is the authoritative signal.
                let loaded = {
                    let state = self.lock_state();
                    watch.loaded_since(&state.activity)
                        || (loading_seen.get() && !page_load_is_pending(&state.status))
                };
                if loaded {
                    return aftermath;
                }
                if Instant::now() >= deadline {
                    aftermath.timed_out = true;
                    return aftermath;
                }
                std::thread::sleep(POST_ACTION_POLL);
            }
        }
        if requests.is_empty() {
            return aftermath;
        }
        let deadline = Instant::now() + POST_ACTION_NETWORK_QUIET;
        loop {
            if observe(&mut aftermath) {
                return aftermath;
            }
            let pending = {
                let state = self.lock_state();
                requests.iter().any(|(id, request)| {
                    // Only the kinds whose body the page is likely waiting on hold the settle;
                    // an image or font that is still streaming does not.
                    SETTLE_BODY_RESOURCE_TYPES.contains(&request.resource_type.as_str())
                        && state
                            .activity
                            .requests
                            .get(id)
                            .is_some_and(|current| !current.finished)
                })
            };
            if !pending {
                break;
            }
            if Instant::now() >= deadline {
                aftermath.timed_out = true;
                break;
            }
            std::thread::sleep(POST_ACTION_POLL);
        }
        let settle_until = Instant::now() + POST_ACTION_SETTLE;
        while Instant::now() < settle_until {
            if observe(&mut aftermath) {
                return aftermath;
            }
            std::thread::sleep(POST_ACTION_POLL);
        }
        aftermath
    }

    fn describe_target(&self, target: &TargetSpec) -> Result<Value, String> {
        self.eval_value(
            &format!(
                "return __state.describeTarget({}, null);",
                js_optional_literal(target.selector.as_deref())?
            ),
            EVAL_TIMEOUT,
        )
    }

    fn next_agent_pointer_generation(&self) -> u64 {
        self.agent_pointer_generation
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1)
    }

    /// Invalidates every pending cleanup timer before removing the current trusted overlay.
    fn hide_agent_pointer(&self) {
        let _overlay = lock_unpoison(&self.agent_pointer_overlay);
        self.next_agent_pointer_generation();
        let _ = self.hide_agent_pointer_overlay();
    }

    fn hide_agent_pointer_overlay(&self) -> Result<(), String> {
        self.cdp_call(
            "Overlay.hideHighlight",
            &json!({}),
            AGENT_POINTER_CDP_TIMEOUT,
        )
        .map(|_| ())
    }

    // ----- dialogs -----------------------------------------------------------------------

    /// `preview_dialog` answers the dialog the page is currently blocked on. Dialogs are held
    /// natively (the page's JavaScript stays blocked in `alert`/`confirm`/`prompt`, exactly as
    /// in a real browser) until this action accepts or dismisses them; while one is held every
    /// other page action is refused. Without a held dialog the action is an error, as in
    /// `@playwright/mcp`, and the recent dialog records are returned with it.
    pub fn dialog_tool(
        &self,
        accept: Option<bool>,
        prompt_text: Option<&str>,
    ) -> Result<Value, String> {
        if prompt_text.is_some_and(|text| text.chars().count() > MAX_DIALOG_PROMPT_CHARS) {
            return Err(format!(
                "preview_dialog prompt_text exceeds the {MAX_DIALOG_PROMPT_CHARS}-character limit"
            ));
        }
        if !host_platform().is_windows() {
            // Without native holding the page intercepts its own dialogs; this arms the answer
            // for the next confirm/prompt and reads the records, the pre-WebView2 contract.
            let arm = accept.is_some() || prompt_text.is_some();
            let accept_literal = match accept {
                Some(true) => "true",
                Some(false) => "false",
                None => "null",
            };
            return self.eval_value(
                &format!(
                    "return __state.dialogControl({arm}, {accept_literal}, {});",
                    js_optional_literal(prompt_text)?
                ),
                EVAL_TIMEOUT,
            );
        }
        let (pending, records) = {
            let state = self.lock_state();
            (
                state.activity.pending_dialog.clone(),
                state.activity.dialog_records.clone(),
            )
        };
        let Some(pending) = pending else {
            let recent = serde_json::to_string(&records.iter().rev().take(5).collect::<Vec<_>>())
                .unwrap_or_else(|_| "[]".to_owned());
            return Err(format!(
                "preview_dialog can only be used while the page has a dialog open; there is none right now. Recent dialogs: {recent}"
            ));
        };
        let accept = accept.unwrap_or(true);
        self.answer_pending_dialog(pending.id, accept, prompt_text)?;
        {
            let mut state = self.lock_state();
            if state
                .activity
                .pending_dialog
                .as_ref()
                .is_some_and(|dialog| dialog.id == pending.id)
            {
                state.activity.pending_dialog = None;
            }
            state
                .activity
                .record_dialog(&pending.kind, &pending.message, Some(accept));
        }
        Ok(json!({
            "dialog": pending,
            "accepted": accept,
            "promptText": prompt_text,
        }))
    }

    // ----- file upload -------------------------------------------------------------------

    /// `preview_upload_image` sets an already-guard-approved host file onto an `input[type=file]`.
    /// File inputs are frequently hidden behind styled buttons, so visibility is not required.
    ///
    /// When the page opened a file chooser (a click on the input, or a scripted picker) the
    /// chooser is being held and the files are set on the element it belongs to; a selector is
    /// then optional and, when given, must be that same element.
    pub fn file_upload(
        &self,
        selector: Option<&str>,
        validated_paths: &[PathBuf],
    ) -> Result<Value, String> {
        let target = validate_target_input(selector, true)?;
        if validated_paths.is_empty() || validated_paths.len() > MAX_UPLOAD_FILES {
            return Err(format!(
                "preview_upload_image requires between 1 and {MAX_UPLOAD_FILES} files"
            ));
        }
        let files: Vec<String> = validated_paths
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect();
        let chooser = self.lock_state().activity.pending_file_chooser.clone();
        let has_target = target.selector.is_some();
        let params = match (&chooser, has_target) {
            (Some(chooser), _) if chooser.backend_node_id.is_some() => {
                if chooser.mode == "selectSingle" && files.len() > 1 {
                    return Err(
                        "the open file chooser accepts a single file; pass exactly one path".into(),
                    );
                }
                json!({ "backendNodeId": chooser.backend_node_id, "files": files })
            }
            (_, true) => {
                let object_id = self.resolve_object_id(&target)?;
                json!({ "objectId": object_id, "files": files })
            }
            (_, false) => {
                return Err(
                    "preview_upload_image needs a selector for the file input unless the page has a file chooser open".into(),
                );
            }
        };
        self.cdp_call("DOM.setFileInputFiles", &params, EVAL_TIMEOUT)?;
        if let Some(chooser) = chooser {
            let mut state = self.lock_state();
            if state
                .activity
                .pending_file_chooser
                .as_ref()
                .is_some_and(|pending| pending.opened_at_ms == chooser.opened_at_ms)
            {
                state.activity.pending_file_chooser = None;
            }
        }
        let element = if has_target {
            self.describe_target(&target).unwrap_or(Value::Null)
        } else {
            Value::Null
        };
        Ok(json!({ "element": element, "files": files.len() }))
    }

    /// Synchronous agent-executor adapter. Call this only from an existing worker/blocking thread.
    ///
    /// The lifecycle around a preview tool follows `@playwright/mcp`: a page is created (or a
    /// suspended one resumed, or a crashed one reset to a blank document) before any tool needs
    /// it, and a held dialog or file chooser refuses every tool but the one that clears it. Only
    /// Mework's own two tools then wait for their consequences and answer with the page's state;
    /// the thirteen ported ones answer with the source's exact text, so anything appended to it
    /// would be a divergence.
    pub fn execute_tool_blocking(
        &self,
        tool: PreviewTool,
        input: &Map<String, Value>,
        grants: &BrowserToolGrants,
    ) -> Result<PreviewToolOutput, String> {
        let _automation = lock_unpoison(&self.automation);
        // Arguments are refused before a page is created for them: a malformed call must not
        // cost a WebView, and its error must be the argument's, not the page's.
        validate_preview_input(tool, input, grants)?;
        let mut notices: Vec<String> = Vec::new();
        let status = self.status();
        let crashed = self.lock_state().activity.crash.clone();
        if let Some(crash) = crashed {
            // Playwright closes a crashed page and opens a fresh one; the equivalent here is
            // retiring the dead native surface and starting over on the blank start page.
            self.reset_after_crash()?;
            notices.push(format!(
                "Page crashed and was reset to about:blank ({crash})."
            ));
        } else if status.suspended {
            // Automatic LRU sleep is transparent to the next observation or interaction. Resume
            // the retained controller in place (or recreate a prior explicit cold suspension)
            // only after BrowserRuntime has reserved an awake slot for this tool call.
            self.prepare(None)?;
            self.wait_for_load(NAVIGATION_LOAD_GRACE);
        } else if !status.has_page {
            // `ensureTab`: a conversation that has no page yet gets one, blank and in the
            // background, instead of an error telling the model to open one first.
            self.prepare(None)?;
        }
        let _control = self.begin_agent_control(tool)?;
        let modal_states = self.modal_states();
        if modal_states.iter().any(|state| !state.is_cleared_by(tool)) {
            let mut lines = vec![
                format!("Tool \"{tool}\" does not handle the modal state."),
                "Modal state:".to_owned(),
            ];
            lines.extend(render_modal_states(&modal_states));
            return Err(lines.join("\n"));
        }
        let watch = self.action_watch();
        let result = (|| {
            let selector = || required_input_string(input, "selector", MAX_SELECTOR_CHARS, false);
            match tool {
                PreviewTool::ConsoleLogs => self
                    .preview_console_logs(
                        optional_input_string(input, "level", 32)?.as_deref(),
                        optional_input_u64(input, "lines")?,
                    )
                    .map(PreviewToolOutput::Text),
                PreviewTool::Screenshot => self
                    .preview_screenshot(optional_input_f64(input, "scale")?)
                    .map(PreviewToolOutput::Image),
                PreviewTool::Snapshot => self.preview_snapshot().map(PreviewToolOutput::Text),
                // The decision-model page tools are dispatched host-side by
                // `decision_tools::preview`, which reads this page's elements and then hands
                // the chosen action to Click/Fill/Inspect above; their network calls must not
                // run under this page's automation lock. The variants exist so the name is a
                // page tool everywhere a page tool is treated specially.
                PreviewTool::FindElement
                | PreviewTool::ClickByDescription
                | PreviewTool::FillByDescription
                | PreviewTool::InspectByDescription => Err(format!(
                    "{tool} is dispatched host-side by decision_tools::preview"
                )),
                PreviewTool::Inspect => {
                    let selector = selector()?;
                    let styles = parse_preview_styles(input)?;
                    let inspected = self.preview_inspect(&selector, styles.as_deref())?;
                    Ok(PreviewToolOutput::Text(match inspected {
                        Some(element) => serde_json::to_string(&element).map_err(|error| {
                            format!("Failed to encode inspected element: {error}")
                        })?,
                        None => format!("Element not found: {selector}"),
                    }))
                }
                PreviewTool::Click => {
                    let selector = selector()?;
                    let double = optional_input_bool(input, "doubleClick", false)?;
                    if !self.preview_click(&selector, double)? {
                        return Err(format!("Failed to click element: {selector}"));
                    }
                    Ok(PreviewToolOutput::Text(format!(
                        "Successfully {}clicked: {selector}",
                        if double { "double-" } else { "" }
                    )))
                }
                PreviewTool::Fill => {
                    let selector = selector()?;
                    let value = required_input_string(input, "value", MAX_TEXT_INPUT_CHARS, true)?;
                    if !self.preview_fill(&selector, &value)? {
                        return Err(format!("Failed to fill element: {selector}"));
                    }
                    Ok(PreviewToolOutput::Text(format!(
                        "Successfully filled: {selector}"
                    )))
                }
                PreviewTool::Eval => {
                    let expression =
                        required_input_string(input, "expression", MAX_EVALUATE_CHARS, false)?;
                    let value = self.preview_eval_value(&expression)?;
                    Ok(PreviewToolOutput::Text(match value {
                        None => "undefined".to_owned(),
                        Some(value) => serde_json::to_string_pretty(&value).map_err(|error| {
                            format!("Failed to encode evaluation result: {error}")
                        })?,
                    }))
                }
                PreviewTool::Network => self
                    .preview_network(
                        optional_input_string(input, "filter", 32)?.as_deref(),
                        optional_input_string(input, "requestId", 256)?.as_deref(),
                    )
                    .map(PreviewToolOutput::Text),
                PreviewTool::Resize => self
                    .preview_resize(
                        optional_input_string(input, "preset", 32)?.as_deref(),
                        optional_input_f64(input, "width")?,
                        optional_input_f64(input, "height")?,
                        optional_input_string(input, "colorScheme", 32)?.as_deref(),
                    )
                    .map(PreviewToolOutput::Text),
                // The same `DOM.setFileInputFiles` machinery Claude Code has no tool for: the file
                // is a host-materialized transcript attachment, the input carries no path at all,
                // and the grant is the only source of bytes.
                PreviewTool::UploadImage => {
                    let paths = grants.upload_paths.as_deref().ok_or_else(|| {
                        "preview_upload_image is missing a host-materialized image file".to_owned()
                    })?;
                    let uploaded = self.file_upload(
                        optional_input_string(input, "selector", MAX_SELECTOR_CHARS)?.as_deref(),
                        paths,
                    )?;
                    Ok(PreviewToolOutput::Text(uploaded.to_string()))
                }
                PreviewTool::Dialog => {
                    let accept = match input.get("accept") {
                        None | Some(Value::Null) => None,
                        Some(value) => Some(
                            value
                                .as_bool()
                                .ok_or_else(|| "parameter accept must be a boolean".to_owned())?,
                        ),
                    };
                    let prompt_text =
                        optional_input_string(input, "prompt_text", MAX_DIALOG_PROMPT_CHARS)?;
                    let answered = self.dialog_tool(accept, prompt_text.as_deref())?;
                    Ok(PreviewToolOutput::Text(answered.to_string()))
                }
            }
        })();
        let mut result = match result {
            Ok(output) => output,
            Err(error) if error == MODAL_STATE_INTERRUPTED && self.has_modal_state() => {
                PreviewToolOutput::Text(json!({ "interrupted": "modal-state" }).to_string())
            }
            Err(error) => return Err(error),
        };
        if !tool.reports_page_after() {
            // The thirteen ported tools answer with the source's exact text and nothing else, so
            // the only thing allowed to precede it is a diagnosis of a page that is no longer the
            // one the model asked about.
            if let (false, PreviewToolOutput::Text(text)) = (notices.is_empty(), &mut result) {
                *text = format!(
                    "{}

{text}",
                    notices.join(" ")
                );
            }
            return Ok(result);
        }
        let aftermath = if tool.waits_for_completion() {
            self.settle_after_action(&watch)
        } else {
            ActionAftermath::default()
        };
        // Only Mework's own two tools reach here, and both answer with a JSON object, so the page
        // block has somewhere to attach.
        if let PreviewToolOutput::Text(text) = &mut result {
            let mut object = serde_json::from_str::<Value>(text)
                .ok()
                .filter(Value::is_object)
                .unwrap_or_else(|| json!({ "value": text }));
            self.report_page_after_action(&mut object, &watch, &aftermath, notices);
            *text = serde_json::to_string_pretty(&object)
                .map_err(|error| format!("Failed to encode preview tool result: {error}"))?;
        }
        Ok(result)
    }

    /// The same envelope as [`Self::execute_tool_blocking`] around a read that answers in the
    /// caller's own type: the automation lock, page preparation, agent control and the modal-state
    /// gate, and nothing after it — a read has no consequences to settle and no page block to
    /// append.
    ///
    /// The decision-model preview tools use this for their two page round trips, because their
    /// scoring runs between them and must not hold this lock while it does.
    fn read_page_blocking<T>(
        &self,
        tool: PreviewTool,
        read: impl FnOnce(&Self) -> Result<T, String>,
    ) -> Result<T, String> {
        let _automation = lock_unpoison(&self.automation);
        let status = self.status();
        if self.lock_state().activity.crash.is_some() {
            self.reset_after_crash()?;
        } else if status.suspended {
            self.prepare(None)?;
            self.wait_for_load(NAVIGATION_LOAD_GRACE);
        } else if !status.has_page {
            self.prepare(None)?;
        }
        let _control = self.begin_agent_control(tool)?;
        let modal_states = self.modal_states();
        if modal_states.iter().any(|state| !state.is_cleared_by(tool)) {
            let mut lines = vec![
                format!("Tool \"{tool}\" does not handle the modal state."),
                "Modal state:".to_owned(),
            ];
            lines.extend(render_modal_states(&modal_states));
            return Err(lines.join("\n"));
        }
        read(self)
    }

    /// Attaches the page's state after an interaction to its result, the way `@playwright/mcp`
    /// appends `### Page` / `### Modal state` / `### Snapshot` sections: the page header (URL,
    /// title, whether it is still loading, console counts since navigation), what the settle step
    /// observed, any held dialog or file chooser, new console errors, and a bounded copy of the
    /// accessibility tree so the model can act on the change without another round trip.
    fn report_page_after_action(
        &self,
        result: &mut Value,
        watch: &ActionWatch,
        aftermath: &ActionAftermath,
        mut notices: Vec<String>,
    ) {
        let Value::Object(object) = result else {
            return;
        };
        if let Some(blocked) = &aftermath.blocked {
            object.insert("blocked".to_owned(), json!(blocked));
        }
        if aftermath.navigated {
            object.insert("navigated".to_owned(), json!(true));
        }
        if aftermath.timed_out {
            notices.push(format!(
                "The page was still busy after the {}s completion wait; its state below may be incomplete.",
                if aftermath.navigated {
                    POST_ACTION_NAVIGATION_LOAD.as_secs()
                } else {
                    POST_ACTION_NETWORK_QUIET.as_secs()
                }
            ));
        }
        let modal_states = if aftermath.modal_states.is_empty() {
            self.modal_states()
        } else {
            aftermath.modal_states.clone()
        };
        let status = self.status();
        let mut page = json!({
            "url": status.url,
            "title": status.title,
            "loading": status.loading,
        });
        if modal_states.is_empty() {
            // Page JavaScript is reachable only while no dialog holds it.
            if let Ok(summary) = self.page_summary_since_action(watch) {
                if let Some(console) = summary.get("console") {
                    page["console"] = console.clone();
                }
                if let Some(entries) = summary.get("newConsoleErrors").filter(|entries| {
                    entries
                        .as_array()
                        .is_some_and(|entries| !entries.is_empty())
                }) {
                    object.insert("newConsoleErrors".to_owned(), entries.clone());
                }
                if let Some(tree) = summary.get("tree") {
                    object.insert(
                        "snapshot".to_owned(),
                        json!({
                            "tree": tree,
                            "truncated": summary.get("truncated").cloned().unwrap_or(Value::Bool(false)),
                        }),
                    );
                }
            }
        } else {
            object.insert(
                "modalState".to_owned(),
                json!({
                    "states": modal_states,
                    "description": render_modal_states(&modal_states),
                }),
            );
            notices.push(format!(
                "The page opened a modal state that blocks it: {}. Only the named action can continue.",
                render_modal_states(&modal_states).join("; ")
            ));
        }
        object.insert("page".to_owned(), page);
        if !notices.is_empty() {
            object.insert("notices".to_owned(), json!(notices));
        }
    }

    /// Console counts since the current document loaded, the error-level entries recorded during
    /// the action, and a bounded accessibility tree, read in one page round trip.
    fn page_summary_since_action(&self, watch: &ActionWatch) -> Result<Value, String> {
        let since_ms = Utc::now().timestamp_millis()
            - i64::try_from(watch.started.elapsed().as_millis()).unwrap_or(i64::MAX);
        self.eval_value(
            &format!(
                r#"
const entries = __state.consoleEntries;
let errors = 0, warnings = 0;
for (const entry of entries) {{
  if (entry.level === "error") errors += 1;
  else if (entry.level === "warn") warnings += 1;
}}
const newConsoleErrors = entries
  .filter(entry => entry.level === "error" && entry.timestamp >= {since_ms})
  .slice(-{MAX_ACTION_CONSOLE_ENTRIES})
  .map(entry => String(entry.message ?? "").slice(0, {MAX_ACTION_CONSOLE_CHARS}));
const tree = __state.snapshotTree({ACTION_SNAPSHOT_CHARS});
return {{
  console: {{ total: entries.length, errors, warnings }},
  newConsoleErrors,
  tree: tree.text,
  truncated: tree.truncated
}};
"#
            ),
            EVAL_TIMEOUT,
        )
    }

    /// Waits for a page-side completion while watching for a dialog or file chooser to open. A
    /// dialog blocks the page's JavaScript, so a script or input event that triggered one never
    /// completes; `@playwright/mcp` races its actions against modal states for the same reason.
    /// The abandoned completion is harmless: its sender finds the receiver gone.
    fn recv_racing_modal_states<T>(
        &self,
        receiver: &mpsc::Receiver<T>,
        timeout: Duration,
    ) -> Result<T, mpsc::RecvTimeoutError> {
        let deadline = Instant::now() + timeout;
        loop {
            let slice = POST_ACTION_POLL.min(deadline.saturating_duration_since(Instant::now()));
            match receiver.recv_timeout(slice) {
                Ok(value) => return Ok(value),
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(mpsc::RecvTimeoutError::Disconnected)
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            if self.has_modal_state() || Instant::now() >= deadline {
                return Err(mpsc::RecvTimeoutError::Timeout);
            }
        }
    }

    fn eval_value(&self, body: &str, timeout: Duration) -> Result<Value, String> {
        let page = self.page()?;
        let script = automation_script(body);
        let (sender, receiver) = mpsc::sync_channel(1);
        page.eval_with_callback(script, move |result| {
            let _ = sender.send(result);
        })
        .map_err(|error| format!("failed to run script in browser: {error}"))?;
        let raw =
            self.recv_racing_modal_states(&receiver, timeout)
                .map_err(|error| match error {
                    mpsc::RecvTimeoutError::Timeout if self.has_modal_state() => {
                        MODAL_STATE_INTERRUPTED.to_owned()
                    }
                    mpsc::RecvTimeoutError::Timeout => {
                        format!(
                            "timed out waiting for browser script result ({} ms)",
                            timeout.as_millis()
                        )
                    }
                    mpsc::RecvTimeoutError::Disconnected => {
                        "browser script result channel closed".into()
                    }
                })?;
        if raw.len() > MAX_EVAL_RESPONSE_BYTES {
            return Err(format!(
                "browser script result exceeds the {} MiB safety limit",
                MAX_EVAL_RESPONSE_BYTES / (1024 * 1024)
            ));
        }
        decode_eval_response(&raw)
    }

    /// Verifies the fixed, network-inert `about:blank` start page after this controller
    /// generation has attested its actual UserDataFolder. The script cannot navigate or touch
    /// remote content; all general evaluation continues through `eval_value`, whose `page()`
    /// lookup holds an attested controller permit for the complete operation.
    fn verify_network_inert_start_page(page: &AttestedPage) -> Result<(), String> {
        let script = automation_script(
            r#"
__state.mountStartPage();
return {
  runtime: window.__MEWORK_BROWSER_RUNTIME__ === __state,
  startPage: location.href === "about:blank" && !!document.querySelector(".mework-start")
};
"#,
        );
        let (sender, receiver) = mpsc::sync_channel(1);
        page.eval_with_callback(script, move |result| {
            let _ = sender.send(result);
        })
        .map_err(|error| {
            format!("failed to verify the network-inert Chromium start page: {error}")
        })?;
        let raw = receiver
            .recv_timeout(EVAL_TIMEOUT)
            .map_err(|error| match error {
                mpsc::RecvTimeoutError::Timeout => {
                    format!(
                        "timed out waiting for network-inert Chromium start-page verification ({} ms)",
                        EVAL_TIMEOUT.as_millis()
                    )
                }
                mpsc::RecvTimeoutError::Disconnected => {
                    "network-inert Chromium start-page verification channel closed".into()
                }
            })?;
        if raw.len() > MAX_EVAL_RESPONSE_BYTES {
            return Err("Chromium start-page verification result exceeds the safety limit".into());
        }
        let ready = decode_eval_response(&raw)?;
        if ready.get("runtime").and_then(Value::as_bool) != Some(true)
            || ready.get("startPage").and_then(Value::as_bool) != Some(true)
        {
            return Err("Chromium could not verify the embedded browser start page".into());
        }
        Ok(())
    }

    /// Captures and saves a PNG to a path already authorized by the caller's filesystem guard.
    /// Agent tools use [`capture_screenshot_png`](Self::capture_screenshot_png) so their path guard
    /// can run after the comparatively slow CDP capture and immediately before the write.
    pub fn screenshot(
        &self,
        validated_path: &Path,
        full_page: bool,
    ) -> Result<BrowserScreenshot, String> {
        validate_screenshot_path(validated_path)?;
        let capture = self.capture_screenshot_png(full_page)?;
        std::fs::write(validated_path, &capture.bytes)
            .map_err(|error| format!("failed to write browser screenshot: {error}"))?;
        Ok(self.note_screenshot_saved(validated_path, &capture))
    }

    fn capture_screenshot_png(&self, full_page: bool) -> Result<BrowserPngCapture, String> {
        self.capture_png_clip(full_page, None)
    }

    /// `browser_capture_page`: the visible page as a base64 PNG, by value.
    ///
    /// `Ok(None)` is "there is nothing to capture" — no live page, a suspended one, or a platform
    /// without WebView2 — which the pane treats as the Annotate button simply having nothing to
    /// offer. A capture that was attempted and failed is still an error.
    pub(crate) fn capture_page(&self) -> Result<Option<BrowserPageCapture>, String> {
        {
            let state = self.lock_state();
            if !state.status.has_page || state.status.suspended {
                return Ok(None);
            }
        }
        #[cfg(windows)]
        {
            let capture = self.capture_screenshot_png(false)?;
            if capture.bytes.len() > MAX_INLINE_CAPTURE_BYTES {
                return Err(format!(
                    "browser page capture exceeds the {MAX_INLINE_CAPTURE_BYTES}-byte inline limit"
                ));
            }
            Ok(Some(BrowserPageCapture {
                data: base64::engine::general_purpose::STANDARD.encode(&capture.bytes),
                width: capture.width,
                height: capture.height,
            }))
        }
        #[cfg(not(windows))]
        {
            Ok(None)
        }
    }

    fn capture_png_clip(
        &self,
        full_page: bool,
        clip: Option<Value>,
    ) -> Result<BrowserPngCapture, String> {
        // The trusted collaboration marker is for the human observer, not page evidence consumed
        // by the model or saved screenshots.
        self.hide_agent_pointer();
        #[cfg(windows)]
        {
            self.with_composited_surface(|page| {
                let mut params = json!({
                    "format": "png",
                    "fromSurface": true,
                    "captureBeyondViewport": full_page || clip.is_some(),
                    "optimizeForSpeed": true,
                });
                if let Some(clip) = clip {
                    params["clip"] = clip;
                }
                let control = self.webview2_control()?;
                let response = call_devtools_protocol(
                    &control,
                    page,
                    "Page.captureScreenshot",
                    &params.to_string(),
                    SCREENSHOT_TIMEOUT,
                    &|| self.has_modal_state(),
                )?;
                let encoded = response
                    .get("data")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "WebView2 screenshot response is missing data".to_owned())?;
                let bytes = decode_base64(encoded)?;
                let (width, height) = png_dimensions(&bytes)?;
                Ok(BrowserPngCapture {
                    bytes,
                    width,
                    height,
                    full_page,
                })
            })
        }

        #[cfg(not(windows))]
        {
            let _ = (full_page, clip);
            Err(
                "preview_screenshot is currently supported only by Windows WebView2 (CDP Page.captureScreenshot)"
                    .into(),
            )
        }
    }

    /// Runs `capture` with the page guaranteed to be compositing frames. WebView2 can indefinitely
    /// defer `Page.captureScreenshot` while its controller or parent window is invisible, so a
    /// task-space page the user never opened is rendered offscreen without being focused, then
    /// restored to its exact hidden geometry.
    #[cfg(windows)]
    fn with_composited_surface<T>(
        &self,
        capture: impl FnOnce(&AttestedPage) -> Result<T, String>,
    ) -> Result<T, String> {
        let page = self.page()?;
        let (was_open, host) = {
            let state = self.lock_state();
            (state.status.open, state.host)
        };
        let original_page_position = (!was_open).then(|| page.position().ok()).flatten();
        let window = page.window();
        let original_window_position = (!was_open && host == Some(BrowserHost::DetachedWindow))
            .then(|| window.outer_position().ok())
            .flatten();
        let restore_hidden_surface = || {
            let _ = page.hide();
            if let Some(position) = original_page_position {
                let _ = page.set_position(position);
            }
            if host == Some(BrowserHost::DetachedWindow) {
                let _ = window.hide();
                if let Some(position) = original_window_position {
                    let _ = window.set_position(position);
                }
            }
        };

        if !was_open {
            let prepared = (|| -> Result<(), String> {
                page.set_position(PhysicalPosition::new(-32_000, -32_000))
                    .map_err(|error| {
                        format!("failed to prepare the offscreen browser screenshot: {error}")
                    })?;
                if host == Some(BrowserHost::DetachedWindow) {
                    window
                        .set_position(LogicalPosition::new(-32_000.0, -32_000.0))
                        .map_err(|error| {
                            format!("failed to prepare the offscreen browser window: {error}")
                        })?;
                    window.show().map_err(|error| {
                        format!("failed to show the offscreen browser window: {error}")
                    })?;
                }
                page.show().map_err(|error| {
                    format!("failed to show the offscreen browser page: {error}")
                })?;
                Ok(())
            })();
            if let Err(error) = prepared {
                restore_hidden_surface();
                return Err(error);
            }
            let _ = self.cdp_call("Page.bringToFront", &json!({}), EVAL_TIMEOUT);
            std::thread::sleep(Duration::from_millis(80));
        }

        let captured = capture(&page);

        if !was_open {
            restore_hidden_surface();
        }
        captured
    }

    pub(crate) fn note_screenshot_saved(
        &self,
        path: &Path,
        capture: &BrowserPngCapture,
    ) -> BrowserScreenshot {
        self.note_screenshot_saved_as(&path.to_string_lossy(), capture)
    }

    /// Commits only the provider/UI-safe receipt spelling after the caller has
    /// completed its handle-bound filesystem installation.
    pub(crate) fn note_screenshot_saved_as(
        &self,
        receipt_path: &str,
        capture: &BrowserPngCapture,
    ) -> BrowserScreenshot {
        let path = receipt_path.to_owned();
        self.lock_state().status.screenshot_path = Some(path.clone());
        BrowserScreenshot {
            path,
            bytes: capture.bytes.len() as u64,
            width: capture.width,
            height: capture.height,
            full_page: capture.full_page,
        }
    }
}

fn validate_screenshot_path(path: &Path) -> Result<(), String> {
    if !path.is_absolute() {
        return Err("browser screenshot accepts only validated absolute paths".into());
    }
    if path
        .extension()
        .and_then(|extension| extension.to_str())
        .map_or(true, |extension| !extension.eq_ignore_ascii_case("png"))
    {
        return Err("browser screenshot path must end in .png".into());
    }
    let parent = path
        .parent()
        .ok_or_else(|| "browser screenshot path has no parent directory".to_owned())?;
    if !parent.is_dir() {
        return Err("browser screenshot parent directory must already exist".into());
    }
    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ColdCloseCookieParam<'a> {
    name: &'a str,
    value: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    domain: Option<&'a str>,
    path: &'a str,
    secure: bool,
    http_only: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    same_site: Option<CookieSameSite>,
    #[serde(skip_serializing_if = "Option::is_none")]
    expires: Option<f64>,
    priority: CookiePriority,
    source_scheme: CookieSourceScheme,
    source_port: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    partition_key: Option<&'a CookiePartitionKey>,
}

#[derive(Serialize)]
struct ColdCloseSetCookies<'a> {
    cookies: Vec<ColdCloseCookieParam<'a>>,
}

/// Reads every cookie in the conversation's isolated Chromium profile. This is intentionally a
/// profile-wide handoff: restoring only the current URL could silently lose an authentication
/// cookie scoped to a redirect, identity provider, or partitioned third-party resource.
fn capture_cookies_for_cold_close(
    control: &WebView2Control,
    page: &AttestedPage,
) -> Result<ColdCloseCookieSnapshot, String> {
    #[cfg(windows)]
    {
        let response =
            call_devtools_protocol(control, page, "Storage.getCookies", "{}", EVAL_TIMEOUT, &|| false)
                .or_else(|_| {
                    // Older WebView2 runtimes may predate the preferred Storage endpoint while still
                    // implementing the equivalent deprecated Network endpoint.
                    call_devtools_protocol(
                        control,
                        page,
                        "Network.getAllCookies",
                        "{}",
                        EVAL_TIMEOUT,
                        &|| false,
                    )
                })
                .map_err(|_| {
                    "Chromium did not provide a verifiable cookie snapshot; cold suspension was refused to avoid losing sign-in state"
                        .to_owned()
                })?;
        parse_cold_close_cookie_snapshot(response).map_err(|error| {
            format!("Cookie data cannot be preserved losslessly; cold suspension was refused to avoid losing sign-in state: {error}")
        })
    }
    #[cfg(not(windows))]
    {
        let _ = (control, page);
        Err("this platform cannot safely preserve Chromium session cookies, so cold suspension was refused".into())
    }
}

fn restore_cookies_after_cold_close(
    control: &WebView2Control,
    page: &AttestedPage,
    snapshot: &ColdCloseCookieSnapshot,
    now: f64,
) -> Result<ColdCloseCookieSnapshot, String> {
    #[cfg(windows)]
    {
        let count = write_cookie_subset(control, page, snapshot.cookies.iter(), now)?;

        let restored = capture_cookies_for_cold_close(control, page).map_err(|_| {
            "could not verify cold-suspension cookie restoration; the page remains suspended"
                .to_owned()
        })?;
        if count == 0 {
            return Ok(restored);
        }
        verify_cookie_snapshot_contains(
            snapshot,
            &restored,
            now,
            "Chromium could not verify every cold-suspension cookie; the page remains suspended to avoid state degradation",
        )?;
        Ok(restored)
    }
    #[cfg(not(windows))]
    {
        let _ = (control, page, snapshot, now);
        Err("this platform cannot safely restore Chromium session cookies".into())
    }
}

fn verify_cookie_snapshot_contains(
    expected_snapshot: &ColdCloseCookieSnapshot,
    actual_snapshot: &ColdCloseCookieSnapshot,
    now: f64,
    error: &str,
) -> Result<(), String> {
    let mut matched = vec![false; actual_snapshot.cookies.len()];
    for expected in expected_snapshot
        .cookies
        .iter()
        .filter(|cookie| cookie.expires.is_none_or(|expires| expires > now))
    {
        let Some((index, _)) =
            actual_snapshot
                .cookies
                .iter()
                .enumerate()
                .find(|(index, actual)| {
                    !matched[*index] && cold_close_cookie_matches(expected, actual)
                })
        else {
            return Err(error.to_owned());
        };
        matched[index] = true;
    }
    Ok(())
}

#[cfg(windows)]
fn write_cookie_subset<'a>(
    control: &WebView2Control,
    page: &AttestedPage,
    source: impl IntoIterator<Item = &'a ColdCloseCookie>,
    now: f64,
) -> Result<usize, String> {
    let mut cookies = Vec::new();
    for cookie in source {
        if let Some(param) = cold_close_cookie_param(cookie, now)? {
            cookies.push(param);
        }
    }
    let count = cookies.len();
    if count == 0 {
        return Ok(0);
    }

    // Serialize borrowed values straight into a zeroizing buffer instead of cloning secrets
    // through serde_json::Value.
    let request = ColdCloseSetCookies { cookies };
    let encoded = Zeroizing::new(
        serde_json::to_string(&request)
            .map_err(|_| "could not encode the Chromium cookie restoration request".to_owned())?,
    );
    call_devtools_protocol(
        control,
        page,
        "Storage.setCookies",
        encoded.as_str(),
        EVAL_TIMEOUT,
        &|| false,
    )
    .or_else(|_| {
        call_devtools_protocol(
            control,
            page,
            "Network.setCookies",
            encoded.as_str(),
            EVAL_TIMEOUT,
            &|| false,
        )
    })
    .map_err(|_| "Chromium refused to write cookies".to_owned())?;
    Ok(count)
}

fn cold_close_cookie_param<'a>(
    cookie: &'a ColdCloseCookie,
    now: f64,
) -> Result<Option<ColdCloseCookieParam<'a>>, String> {
    if cookie.expires.is_some_and(|expires| expires <= now) {
        return Ok(None);
    }
    let (url, domain) = if cookie.domain.starts_with('.') {
        (None, Some(cookie.domain.as_str()))
    } else {
        (Some(host_only_cookie_url(cookie)?), None)
    };
    Ok(Some(ColdCloseCookieParam {
        name: &cookie.name,
        value: cookie.value.as_str(),
        url,
        domain,
        path: &cookie.path,
        secure: cookie.secure,
        http_only: cookie.http_only,
        same_site: cookie.same_site,
        expires: cookie.expires,
        priority: cookie.priority,
        source_scheme: cookie.source_scheme,
        source_port: cookie.source_port,
        partition_key: cookie.partition_key.as_ref(),
    }))
}

/// Host-only cookies must be recreated with `url` and without `domain`; supplying even an
/// un-dotted domain through CookieParam would turn the write into an ambiguous domain cookie.
fn host_only_cookie_url(cookie: &ColdCloseCookie) -> Result<String, String> {
    if cookie.domain.is_empty() || cookie.domain.starts_with('.') {
        return Err("host-only cookie host is invalid".into());
    }
    let raw_host = cookie
        .domain
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(cookie.domain.as_str());
    let host = if raw_host.parse::<std::net::Ipv6Addr>().is_ok() {
        format!("[{raw_host}]")
    } else {
        raw_host.to_owned()
    };
    let scheme = match cookie.source_scheme {
        CookieSourceScheme::Secure => "https",
        CookieSourceScheme::NonSecure => "http",
        CookieSourceScheme::Unset if cookie.secure => "https",
        CookieSourceScheme::Unset => "http",
    };
    let port = if cookie.source_port == -1 {
        String::new()
    } else {
        format!(":{}", cookie.source_port)
    };
    let candidate = format!("{scheme}://{host}{port}/");
    let parsed = Url::parse(&candidate)
        .ok()
        .filter(|url| {
            url.host_str()
                .is_some_and(|parsed_host| parsed_host.eq_ignore_ascii_case(raw_host))
        })
        .ok_or_else(|| {
            "could not construct an exact source URL for the host-only cookie host".to_owned()
        })?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err("host-only cookie source scheme is invalid".into());
    }
    Ok(candidate)
}

fn cold_close_cookie_matches(expected: &ColdCloseCookie, actual: &ColdCloseCookie) -> bool {
    expected.name == actual.name
        && expected.value.as_str() == actual.value.as_str()
        && expected.domain == actual.domain
        && expected.path == actual.path
        && expected.secure == actual.secure
        && expected.http_only == actual.http_only
        && match (expected.expires, actual.expires) {
            (None, None) => true,
            (Some(left), Some(right)) => (left - right).abs() <= 1.0,
            _ => false,
        }
        && expected.same_site == actual.same_site
        && expected.priority == actual.priority
        && expected.source_scheme == actual.source_scheme
        && expected.source_port == actual.source_port
        && expected.partition_key == actual.partition_key
}

struct SecretJson(Value);

impl Drop for SecretJson {
    fn drop(&mut self) {
        zeroize_json_strings(&mut self.0);
    }
}

fn zeroize_json_strings(value: &mut Value) {
    match value {
        Value::String(value) => value.zeroize(),
        Value::Array(values) => values.iter_mut().for_each(zeroize_json_strings),
        Value::Object(values) => values.values_mut().for_each(zeroize_json_strings),
        _ => {}
    }
}

fn json_payload_bytes(value: &Value) -> usize {
    match value {
        Value::Null => 4,
        Value::Bool(_) => 5,
        Value::Number(_) => 24,
        Value::String(value) => value.len(),
        Value::Array(values) => values.iter().fold(0_usize, |total, value| {
            total.saturating_add(json_payload_bytes(value))
        }),
        Value::Object(values) => values.iter().fold(0_usize, |total, (key, value)| {
            total
                .saturating_add(key.len())
                .saturating_add(json_payload_bytes(value))
        }),
    }
}

fn parse_cold_close_cookie_snapshot(response: Value) -> Result<ColdCloseCookieSnapshot, String> {
    let mut response = SecretJson(response);
    if json_payload_bytes(&response.0) > MAX_COLD_CLOSE_COOKIE_BYTES {
        return Err(format!(
            "Cookie snapshot exceeds the {}-byte limit",
            MAX_COLD_CLOSE_COOKIE_BYTES
        ));
    }
    let cookies = response
        .0
        .as_object_mut()
        .and_then(|object| object.remove("cookies"))
        .ok_or_else(|| "Chromium cookie snapshot is missing the cookies array".to_owned())?;
    let mut cookies = SecretJson(cookies);
    let values = cookies.0.as_array_mut().ok_or_else(|| {
        "the cookies field in the Chromium cookie snapshot is not an array".to_owned()
    })?;
    if values.len() > MAX_COLD_CLOSE_COOKIE_COUNT {
        return Err(format!(
            "Cookie snapshot exceeds the {}-item limit",
            MAX_COLD_CLOSE_COOKIE_COUNT
        ));
    }

    let mut parsed = Vec::with_capacity(values.len());
    for (index, value) in values.iter_mut().enumerate() {
        let object = value
            .as_object_mut()
            .ok_or_else(|| format!("cookie item {} is not an object", index + 1))?;
        let name = take_cookie_string(object, "name", index)?;
        let secret_value = Zeroizing::new(take_cookie_string(object, "value", index)?);
        let domain = take_cookie_string(object, "domain", index)?;
        let path = take_cookie_string(object, "path", index)?;
        let expires_raw = take_cookie_number_or_null(object, "expires", index)?;
        let size = take_cookie_integer(object, "size", index)?;
        let http_only = take_cookie_bool(object, "httpOnly", index)?;
        let secure = take_cookie_bool(object, "secure", index)?;
        let session = take_cookie_bool(object, "session", index)?;
        let same_site = take_optional_cookie_string(object, "sameSite", index)?
            .map(|value| parse_cookie_same_site(&value, index))
            .transpose()?;
        let priority =
            parse_cookie_priority(&take_cookie_string(object, "priority", index)?, index)?;
        let source_scheme =
            parse_cookie_source_scheme(&take_cookie_string(object, "sourceScheme", index)?, index)?;
        let source_port = take_cookie_integer(object, "sourcePort", index)?;
        let partition_key = parse_cookie_partition_key(object.remove("partitionKey"), index)?;
        let partition_key_opaque =
            take_optional_cookie_bool(object, "partitionKeyOpaque", index)?.unwrap_or(false);

        if !object.is_empty() {
            return Err(format!(
                "cookie item {} contains fields this version cannot restore losslessly",
                index + 1
            ));
        }
        if partition_key_opaque {
            return Err(format!(
                "cookie item {} uses an irreversible opaque partition key",
                index + 1
            ));
        }
        if size < 0 {
            return Err(format!("cookie item {} has an invalid size", index + 1));
        }
        if domain.is_empty()
            || domain.chars().any(char::is_control)
            || path.is_empty()
            || !path.starts_with('/')
            || path.chars().any(char::is_control)
        {
            return Err(format!("cookie item {} has an invalid scope", index + 1));
        }
        if domain.starts_with("..") {
            return Err(format!("cookie item {} has an invalid domain", index + 1));
        }
        let bare_domain = domain.trim_start_matches('.');
        if Host::parse(bare_domain).is_err() {
            return Err(format!("cookie item {} has an invalid domain", index + 1));
        }
        if source_port != -1 && !(1..=65_535).contains(&source_port) {
            return Err(format!(
                "cookie item {} has an invalid sourcePort",
                index + 1
            ));
        }
        let expires = if session {
            if expires_raw.is_some_and(|expires| expires > 0.0) {
                return Err(format!(
                    "cookie item {} has conflicting session and expires fields",
                    index + 1
                ));
            }
            None
        } else {
            Some(
                expires_raw
                    .filter(|expires| expires.is_finite() && *expires > 0.0)
                    .ok_or_else(|| {
                        format!("cookie item {} has an invalid expires field", index + 1)
                    })?,
            )
        };

        let cookie = ColdCloseCookie {
            name,
            value: secret_value,
            domain,
            path,
            secure,
            http_only,
            expires,
            same_site,
            priority,
            source_scheme,
            source_port: source_port as i32,
            partition_key,
        };
        if !cookie.domain.starts_with('.') {
            host_only_cookie_url(&cookie)
                .map_err(|_| format!("host-only cookie item {} has an invalid scope", index + 1))?;
        }
        parsed.push(cookie);
    }
    Ok(ColdCloseCookieSnapshot { cookies: parsed })
}

/// Runs a candidate snapshot through the exact destination validator and reports
/// how many cookies survived.
///
/// The elevated import broker builds this snapshot from Chromium's own cookie
/// store. Without checking it against the real parser, that mapping could drift
/// until an import failed wholesale in production, so its tests validate here
/// rather than against a hand-copied expectation.
#[cfg(test)]
pub(crate) fn validate_chromium_cookie_snapshot(response: Value) -> Result<usize, String> {
    parse_cold_close_cookie_snapshot(response).map(|snapshot| snapshot.cookies.len())
}

fn take_cookie_string(
    object: &mut Map<String, Value>,
    field: &str,
    index: usize,
) -> Result<String, String> {
    match object.remove(field) {
        Some(Value::String(value)) => Ok(value),
        _ => Err(format!(
            "cookie item {} has an invalid {field} field",
            index + 1
        )),
    }
}

fn take_optional_cookie_string(
    object: &mut Map<String, Value>,
    field: &str,
    index: usize,
) -> Result<Option<String>, String> {
    match object.remove(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value)),
        Some(_) => Err(format!(
            "cookie item {} has an invalid {field} field",
            index + 1
        )),
    }
}

fn take_cookie_bool(
    object: &mut Map<String, Value>,
    field: &str,
    index: usize,
) -> Result<bool, String> {
    object
        .remove(field)
        .and_then(|value| value.as_bool())
        .ok_or_else(|| format!("cookie item {} has an invalid {field} field", index + 1))
}

fn take_optional_cookie_bool(
    object: &mut Map<String, Value>,
    field: &str,
    index: usize,
) -> Result<Option<bool>, String> {
    match object.remove(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(value)),
        Some(_) => Err(format!(
            "cookie item {} has an invalid {field} field",
            index + 1
        )),
    }
}

fn take_cookie_integer(
    object: &mut Map<String, Value>,
    field: &str,
    index: usize,
) -> Result<i64, String> {
    object
        .remove(field)
        .and_then(|value| value.as_i64())
        .ok_or_else(|| format!("cookie item {} has an invalid {field} field", index + 1))
}

fn take_cookie_number_or_null(
    object: &mut Map<String, Value>,
    field: &str,
    index: usize,
) -> Result<Option<f64>, String> {
    match object.remove(field) {
        Some(Value::Null) => Ok(None),
        Some(Value::Number(value)) => value
            .as_f64()
            .filter(|number| number.is_finite())
            .map(Some)
            .ok_or_else(|| format!("cookie item {} has an invalid {field} field", index + 1)),
        _ => Err(format!(
            "cookie item {} has an invalid {field} field",
            index + 1
        )),
    }
}

fn parse_cookie_same_site(value: &str, index: usize) -> Result<CookieSameSite, String> {
    match value {
        "Strict" => Ok(CookieSameSite::Strict),
        "Lax" => Ok(CookieSameSite::Lax),
        "None" => Ok(CookieSameSite::None),
        _ => Err(format!(
            "cookie item {} has an invalid sameSite field",
            index + 1
        )),
    }
}

fn parse_cookie_priority(value: &str, index: usize) -> Result<CookiePriority, String> {
    match value {
        "Low" => Ok(CookiePriority::Low),
        "Medium" => Ok(CookiePriority::Medium),
        "High" => Ok(CookiePriority::High),
        _ => Err(format!(
            "cookie item {} has an invalid priority field",
            index + 1
        )),
    }
}

fn parse_cookie_source_scheme(value: &str, index: usize) -> Result<CookieSourceScheme, String> {
    match value {
        "Unset" => Ok(CookieSourceScheme::Unset),
        "NonSecure" => Ok(CookieSourceScheme::NonSecure),
        "Secure" => Ok(CookieSourceScheme::Secure),
        _ => Err(format!(
            "cookie item {} has an invalid sourceScheme field",
            index + 1
        )),
    }
}

fn parse_cookie_partition_key(
    value: Option<Value>,
    index: usize,
) -> Result<Option<CookiePartitionKey>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let mut value = SecretJson(value);
    let object = value
        .0
        .as_object_mut()
        .ok_or_else(|| format!("cookie item {} has an invalid partitionKey", index + 1))?;
    let top_level_site = take_cookie_string(object, "topLevelSite", index)?;
    let has_cross_site_ancestor = take_cookie_bool(object, "hasCrossSiteAncestor", index)?;
    if !object.is_empty() {
        return Err(format!(
            "cookie item {} partitionKey contains unknown fields",
            index + 1
        ));
    }
    let parsed = Url::parse(&top_level_site)
        .ok()
        .filter(|url| {
            matches!(url.scheme(), "http" | "https")
                && url.host().is_some()
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none()
                && url.port().is_none()
                && url.path() == "/"
        })
        .ok_or_else(|| format!("cookie item {} has an invalid topLevelSite", index + 1))?;
    if parsed.origin().ascii_serialization() != top_level_site.trim_end_matches('/') {
        return Err(format!(
            "cookie item {} topLevelSite is not canonical",
            index + 1
        ));
    }
    Ok(Some(CookiePartitionKey {
        top_level_site,
        has_cross_site_ancestor,
    }))
}

// ----- CDP input pipeline --------------------------------------------------------------------
//
// On Windows the pointer/keyboard tools drive the WebView2 DevTools `Input` domain so events carry
// `isTrusted: true` and run the browser's native default actions. If the Input domain is
// unavailable, each dispatcher returns `Ok(false)` so the caller can fall back to synthetic JS
// events. Non-Windows builds always return `Ok(false)` and use the synthetic path.

impl BrowserSession {
    fn cdp_call(&self, method: &str, params: &Value, timeout: Duration) -> Result<Value, String> {
        #[cfg(windows)]
        {
            let control = self.webview2_control()?;
            let page = self.page()?;
            call_devtools_protocol(
                &control,
                &page,
                method,
                &params.to_string(),
                timeout,
                &|| self.has_modal_state(),
            )
        }
        #[cfg(not(windows))]
        {
            let _ = (method, params, timeout);
            Err("the embedded browser on this platform does not support WebView2 CDP".into())
        }
    }

    /// Resolves a target to a CDP remote object id for `DOM.setFileInputFiles`.
    fn resolve_object_id(&self, target: &TargetSpec) -> Result<String, String> {
        let expression = format!(
            "window.__MEWORK_BROWSER_RUNTIME__.resolve({}, null)",
            js_optional_literal(target.selector.as_deref())?
        );
        let response = self.cdp_call(
            "Runtime.evaluate",
            &json!({"expression": expression, "returnByValue": false}),
            EVAL_TIMEOUT,
        )?;
        if let Some(details) = response.get("exceptionDetails") {
            let description = details
                .pointer("/exception/description")
                .and_then(Value::as_str)
                .or_else(|| details.get("text").and_then(Value::as_str))
                .unwrap_or("target resolution failed");
            return Err(format!(
                "preview_upload_image could not locate the input: {description}"
            ));
        }
        response
            .pointer("/result/objectId")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| {
                "preview_upload_image could not obtain a target element reference".to_owned()
            })
    }
}

/// Clamps a log `limit` argument into `1..=MAX_LOG_LIMIT`, applying a default when absent.
fn clamp_log_limit(limit: Option<u64>, default: u64) -> u64 {
    limit.unwrap_or(default).clamp(1, MAX_LOG_LIMIT)
}

fn optional_input_string(
    input: &Map<String, Value>,
    key: &str,
    max_chars: usize,
) -> Result<Option<String>, String> {
    match input.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(_) => required_input_string(input, key, max_chars, false).map(Some),
    }
}

fn required_input_string(
    input: &Map<String, Value>,
    key: &str,
    max_chars: usize,
    allow_empty: bool,
) -> Result<String, String> {
    let value = input
        .get(key)
        .ok_or_else(|| format!("missing required parameter {key}"))?
        .as_str()
        .ok_or_else(|| format!("parameter {key} must be a string"))?;
    if !allow_empty && value.trim().is_empty() {
        return Err(format!("parameter {key} must not be empty"));
    }
    if value.chars().count() > max_chars {
        return Err(format!(
            "parameter {key} exceeds the {max_chars}-character limit"
        ));
    }
    Ok(value.to_owned())
}

fn optional_input_bool(
    input: &Map<String, Value>,
    key: &str,
    default: bool,
) -> Result<bool, String> {
    match input.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(value) => value
            .as_bool()
            .ok_or_else(|| format!("parameter {key} must be a boolean")),
    }
}

fn optional_input_u64(input: &Map<String, Value>, key: &str) -> Result<Option<u64>, String> {
    match input.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .map(Some)
            .ok_or_else(|| format!("parameter {key} must be a non-negative integer")),
    }
}

fn optional_input_f64(input: &Map<String, Value>, key: &str) -> Result<Option<f64>, String> {
    match input.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_f64()
            .filter(|number| number.is_finite())
            .map(Some)
            .ok_or_else(|| format!("parameter {key} must be a finite number")),
    }
}

#[cfg(windows)]
/// `interrupted` is polled while waiting; when it reports true the wait ends with
/// `MODAL_STATE_INTERRUPTED` (a dialog opened and the page cannot answer).
fn call_devtools_protocol(
    control: &WebView2Control,
    page: &AttestedPage,
    method: &str,
    parameters: &str,
    timeout: Duration,
    interrupted: &dyn Fn() -> bool,
) -> Result<Value, String> {
    use webview2_com::{CallDevToolsProtocolMethodCompletedHandler, CoTaskMemPWSTR};

    let dispatch_permit = control
        .permit()
        .map_err(|error| format!("Chromium native control rejected the CDP call: {error}"))?;
    let callback_token = dispatch_permit.callback_token();
    let method = method.to_owned();
    // Some CDP requests and responses contain HttpOnly cookie values. Keep the transport buffers
    // zeroizing even though most browser tools only carry non-secret JSON.
    let parameters = Zeroizing::new(parameters.to_owned());
    let (sender, receiver) = mpsc::sync_channel::<Result<Zeroizing<String>, String>>(1);
    let schedule_sender = sender.clone();
    let native_page = &page.page;
    let scheduling = native_page.with_webview(move |platform| {
        // Fence only Tauri's queued dispatch and native callback registration. The callback owns
        // a non-blocking generation token so a CDP method that never completes cannot deadlock
        // controller teardown.
        let _dispatch_permit = dispatch_permit;
        let callback_sender = sender.clone();
        let scheduled = (|| -> Result<(), String> {
            let controller = platform.controller();
            let core = unsafe { controller.CoreWebView2() }
                .map_err(|error| format!("failed to obtain WebView2 CoreWebView2: {error}"))?;
            let method_wide = CoTaskMemPWSTR::from(method.as_str());
            let parameters_wide = CoTaskMemPWSTR::from(parameters.as_str());
            let callback = CallDevToolsProtocolMethodCompletedHandler::create(Box::new(
                move |status, response| {
                    let Ok(_callback_permit) = callback_token.permit() else {
                        return Ok(());
                    };
                    let result = status
                        .map(|_| Zeroizing::new(response))
                        .map_err(|error| format!("WebView2 CDP call failed: {error}"));
                    let _ = callback_sender.try_send(result);
                    Ok(())
                },
            ));
            unsafe {
                core.CallDevToolsProtocolMethod(
                    *method_wide.as_ref().as_pcwstr(),
                    *parameters_wide.as_ref().as_pcwstr(),
                    &callback,
                )
            }
            .map_err(|error| format!("启动 WebView2 CDP call failed: {error}"))?;
            Ok(())
        })();
        if let Err(error) = scheduled {
            let _ = schedule_sender.try_send(Err(error));
        }
    });
    scheduling.map_err(|error| format!("failed to schedule the WebView2 CDP call: {error}"))?;

    let deadline = Instant::now() + timeout;
    let raw = loop {
        let slice = POST_ACTION_POLL.min(deadline.saturating_duration_since(Instant::now()));
        match receiver.recv_timeout(slice) {
            Ok(result) => break result?,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err("WebView2 CDP result channel closed".into());
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if interrupted() {
            return Err(MODAL_STATE_INTERRUPTED.into());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "timed out waiting for WebView2 CDP result ({} ms)",
                timeout.as_millis()
            ));
        }
    };
    serde_json::from_str(raw.as_str())
        .map_err(|error| format!("WebView2 CDP returned invalid JSON: {error}"))
}

// ----- page activity observers ---------------------------------------------------------------
//
// Everything below feeds `RuntimeState::activity`: the DevTools `Network`/`Page` event streams
// that let an interaction wait for what it started, the native dialog and file-chooser holds
// that become modal states, and the process-failure notice that turns into a page reset. All of
// it is registered on the WebView2 UI thread when a page generation is created and ignored once
// that generation is retired.

/// A dialog WebView2 is holding open on the host's behalf. The event args and the deferral are
/// bound to the WebView UI thread and cannot be sent across it, so they live in this thread-local
/// registry keyed by the id the host state carries in `PendingDialog`.
#[cfg(windows)]
struct HeldDialog {
    args: webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2ScriptDialogOpeningEventArgs,
    deferral: webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Deferral,
}

#[cfg(windows)]
thread_local! {
    static HELD_DIALOGS: std::cell::RefCell<HashMap<u64, HeldDialog>> =
        std::cell::RefCell::new(HashMap::new());
    /// Event receivers and handlers registered for a page, retained until the page's session
    /// installs a newer generation. WebView2 keeps its own reference while they are registered;
    /// this copy only guards against a receiver being collected before its events are.
    static PAGE_OBSERVERS: std::cell::RefCell<HashMap<String, (u64, Vec<Box<dyn std::any::Any>>)>> =
        std::cell::RefCell::new(HashMap::new());
}

/// DevTools resource types whose name WebView2 reports in `Network.requestWillBeSent`.
fn normalize_resource_type(value: Option<&str>) -> String {
    value.unwrap_or("other").to_ascii_lowercase()
}

impl BrowserSession {
    /// Registers the native and DevTools observers for the page generation that was just created.
    /// Failing to observe is not fatal: the page still works, only with the coarser `loading`
    /// signal and without held dialogs, exactly like the non-Windows build.
    #[cfg(windows)]
    fn install_page_activity_observers(
        &self,
        control: &WebView2Control,
        page: &AttestedPage,
        page_generation: u64,
    ) -> Result<(), String> {
        use webview2_com::Microsoft::Web::WebView2::Win32::{
            COREWEBVIEW2_PROCESS_FAILED_KIND_BROWSER_PROCESS_EXITED,
            COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_EXITED,
            COREWEBVIEW2_SCRIPT_DIALOG_KIND_ALERT, COREWEBVIEW2_SCRIPT_DIALOG_KIND_BEFOREUNLOAD,
            COREWEBVIEW2_SCRIPT_DIALOG_KIND_CONFIRM, COREWEBVIEW2_SCRIPT_DIALOG_KIND_PROMPT,
        };
        use webview2_com::{
            take_pwstr, CallDevToolsProtocolMethodCompletedHandler, CoTaskMemPWSTR,
            DevToolsProtocolEventReceivedEventHandler, ProcessFailedEventHandler,
            ScriptDialogOpeningEventHandler,
        };
        use windows_core::PWSTR;

        let dispatch_permit = control
            .permit()
            .map_err(|error| format!("Chromium native control rejected observer setup: {error}"))?;
        let callback_token = dispatch_permit.callback_token();
        let state = self.state.clone();
        let session_id = self.session_id.to_string();
        let (sender, receiver) = mpsc::sync_channel::<Result<(), String>>(1);
        let scheduling = page.page.with_webview(move |platform| {
            let _dispatch_permit = dispatch_permit;
            let result = (|| -> Result<(), String> {
                let controller = platform.controller();
                let core = unsafe { controller.CoreWebView2() }
                    .map_err(|error| format!("failed to obtain WebView2 CoreWebView2: {error}"))?;
                let mut retained: Vec<Box<dyn std::any::Any>> = Vec::new();

                // Native dialogs are held rather than shown or auto-answered: the page blocks in
                // alert()/confirm()/prompt() exactly as it would in a real browser until
                // `preview_dialog` answers.
                let settings = unsafe { core.Settings() }
                    .map_err(|error| format!("failed to read WebView2 settings: {error}"))?;
                unsafe { settings.SetAreDefaultScriptDialogsEnabled(false) }
                    .map_err(|error| format!("failed to take over WebView2 dialogs: {error}"))?;
                let dialog_state = state.clone();
                let dialog_token = callback_token.clone();
                let dialog_handler =
                    ScriptDialogOpeningEventHandler::create(Box::new(move |_, args| {
                        let Ok(_permit) = dialog_token.permit() else {
                            return Ok(());
                        };
                        let Some(args) = args else {
                            return Ok(());
                        };
                        let mut kind = Default::default();
                        let mut message = PWSTR::null();
                        let mut default_text = PWSTR::null();
                        let mut uri = PWSTR::null();
                        unsafe {
                            let _ = args.Kind(&mut kind);
                            let _ = args.Message(&mut message);
                            let _ = args.DefaultText(&mut default_text);
                            let _ = args.Uri(&mut uri);
                        }
                        let kind = if kind == COREWEBVIEW2_SCRIPT_DIALOG_KIND_ALERT {
                            "alert"
                        } else if kind == COREWEBVIEW2_SCRIPT_DIALOG_KIND_CONFIRM {
                            "confirm"
                        } else if kind == COREWEBVIEW2_SCRIPT_DIALOG_KIND_PROMPT {
                            "prompt"
                        } else if kind == COREWEBVIEW2_SCRIPT_DIALOG_KIND_BEFOREUNLOAD {
                            "beforeunload"
                        } else {
                            "dialog"
                        };
                        let message = take_pwstr(message);
                        let default_text = take_pwstr(default_text);
                        let uri = take_pwstr(uri);
                        // alert()/confirm() reach the host as prompts carrying the real kind in
                        // the default text (see the initialization script).
                        let (kind, default_text) = match default_text.strip_prefix(DIALOG_KIND_MARK)
                        {
                            Some(real_kind) if kind == "prompt" => {
                                (real_kind.to_owned(), String::new())
                            }
                            _ => (kind.to_owned(), default_text),
                        };
                        let Ok(deferral) = (unsafe { args.GetDeferral() }) else {
                            // Without a deferral the dialog is answered as the page's default
                            // (cancel) when this callback returns; nothing to hold.
                            return Ok(());
                        };
                        let mut state = lock_unpoison(&dialog_state);
                        if state.page_generation != page_generation || !state.status.has_page {
                            let _ = unsafe { deferral.Complete() };
                            return Ok(());
                        }
                        state.activity.next_dialog_id =
                            state.activity.next_dialog_id.wrapping_add(1);
                        let id = state.activity.next_dialog_id;
                        state.activity.pending_dialog = Some(PendingDialog {
                            id,
                            default_value: (kind == "prompt").then_some(default_text),
                            kind,
                            message: message.chars().take(4_000).collect(),
                            url: uri,
                            opened_at_ms: Utc::now().timestamp_millis(),
                        });
                        drop(state);
                        HELD_DIALOGS.with(|held| {
                            held.borrow_mut().insert(id, HeldDialog { args, deferral });
                        });
                        Ok(())
                    }));
                let mut token = 0i64;
                unsafe { core.add_ScriptDialogOpening(&dialog_handler, &mut token) }
                    .map_err(|error| format!("failed to observe WebView2 dialogs: {error}"))?;
                retained.push(Box::new(dialog_handler));

                let failure_state = state.clone();
                let failure_token = callback_token.clone();
                let failure_handler =
                    ProcessFailedEventHandler::create(Box::new(move |_, args| {
                        let Ok(_permit) = failure_token.permit() else {
                            return Ok(());
                        };
                        let Some(args) = args else {
                            return Ok(());
                        };
                        let mut kind = Default::default();
                        unsafe {
                            let _ = args.ProcessFailedKind(&mut kind);
                        }
                        let description = if kind
                            == COREWEBVIEW2_PROCESS_FAILED_KIND_BROWSER_PROCESS_EXITED
                        {
                            "the browser process exited"
                        } else if kind == COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_EXITED {
                            "the renderer process exited"
                        } else {
                            // GPU/utility/plugin process failures and an unresponsive renderer are
                            // survivable; Chromium recovers them on its own.
                            return Ok(());
                        };
                        let mut state = lock_unpoison(&failure_state);
                        if state.page_generation != page_generation || !state.status.has_page {
                            return Ok(());
                        }
                        state.activity.crash = Some(description.to_owned());
                        state.status.loading = false;
                        state.status.error = Some(format!("browser page crashed: {description}"));
                        Ok(())
                    }));
                unsafe { core.add_ProcessFailed(&failure_handler, &mut token) }.map_err(
                    |error| format!("failed to observe WebView2 process failures: {error}"),
                )?;
                retained.push(Box::new(failure_handler));

                // The completion of an enable call carries nothing the host needs; the events
                // themselves are the signal.
                for (method, parameters) in [
                    ("Network.enable", "{}"),
                    ("Page.enable", "{}"),
                    ("Page.setInterceptFileChooserDialog", r#"{"enabled":true}"#),
                ] {
                    let method_wide = CoTaskMemPWSTR::from(method);
                    let parameters_wide = CoTaskMemPWSTR::from(parameters);
                    let completion =
                        CallDevToolsProtocolMethodCompletedHandler::create(Box::new(|_, _| Ok(())));
                    unsafe {
                        core.CallDevToolsProtocolMethod(
                            *method_wide.as_ref().as_pcwstr(),
                            *parameters_wide.as_ref().as_pcwstr(),
                            &completion,
                        )
                    }
                    .map_err(|error| format!("failed to enable DevTools {method}: {error}"))?;
                }
                // The main frame id separates a navigation from a subframe's document load.
                {
                    let frame_state = state.clone();
                    let frame_token = callback_token.clone();
                    let method_wide = CoTaskMemPWSTR::from("Page.getFrameTree");
                    let parameters_wide = CoTaskMemPWSTR::from("{}");
                    let completion = CallDevToolsProtocolMethodCompletedHandler::create(Box::new(
                        move |status, response| {
                            let Ok(_permit) = frame_token.permit() else {
                                return Ok(());
                            };
                            if status.is_err() {
                                return Ok(());
                            }
                            let Ok(payload) = serde_json::from_str::<Value>(&response) else {
                                return Ok(());
                            };
                            if let Some(id) = payload
                                .pointer("/frameTree/frame/id")
                                .and_then(Value::as_str)
                            {
                                let mut state = lock_unpoison(&frame_state);
                                if state.page_generation == page_generation {
                                    state.activity.main_frame_id = Some(id.to_owned());
                                }
                            }
                            Ok(())
                        },
                    ));
                    unsafe {
                        core.CallDevToolsProtocolMethod(
                            *method_wide.as_ref().as_pcwstr(),
                            *parameters_wide.as_ref().as_pcwstr(),
                            &completion,
                        )
                    }
                    .map_err(|error| format!("failed to read the DevTools frame tree: {error}"))?;
                }

                for event in [
                    "Network.requestWillBeSent",
                    "Network.responseReceived",
                    "Network.loadingFinished",
                    "Network.loadingFailed",
                    "Page.domContentEventFired",
                    "Page.loadEventFired",
                    "Page.frameNavigated",
                    "Page.fileChooserOpened",
                    "Overlay.inspectNodeRequested",
                    "Overlay.inspectModeCanceled",
                ] {
                    let event_wide = CoTaskMemPWSTR::from(event);
                    let receiver = unsafe {
                        core.GetDevToolsProtocolEventReceiver(*event_wide.as_ref().as_pcwstr())
                    }
                    .map_err(|error| format!("failed to subscribe to DevTools {event}: {error}"))?;
                    let event_state = state.clone();
                    let event_token = callback_token.clone();
                    let handler = DevToolsProtocolEventReceivedEventHandler::create(Box::new(
                        move |_, args| {
                            let Ok(_permit) = event_token.permit() else {
                                return Ok(());
                            };
                            let Some(args) = args else {
                                return Ok(());
                            };
                            let mut raw = PWSTR::null();
                            unsafe {
                                let _ = args.ParameterObjectAsJson(&mut raw);
                            }
                            let Ok(parameters) = serde_json::from_str::<Value>(&take_pwstr(raw))
                            else {
                                return Ok(());
                            };
                            let mut state = lock_unpoison(&event_state);
                            if state.page_generation != page_generation || !state.status.has_page {
                                return Ok(());
                            }
                            record_element_picker_event(
                                &mut state.element_picker,
                                page_generation,
                                event,
                                &parameters,
                            );
                            record_devtools_event(&mut state.activity, event, &parameters);
                            Ok(())
                        },
                    ));
                    unsafe { receiver.add_DevToolsProtocolEventReceived(&handler, &mut token) }
                        .map_err(|error| format!("failed to observe DevTools {event}: {error}"))?;
                    retained.push(Box::new(handler));
                    retained.push(Box::new(receiver));
                }

                PAGE_OBSERVERS.with(|observers| {
                    observers
                        .borrow_mut()
                        .insert(session_id.clone(), (page_generation, retained));
                });
                let mut state = lock_unpoison(&state);
                if state.page_generation == page_generation {
                    state.activity.events_enabled = true;
                }
                Ok(())
            })();
            let _ = sender.send(result);
        });
        scheduling
            .map_err(|error| format!("failed to schedule WebView2 observer setup: {error}"))?;
        receiver
            .recv_timeout(EVAL_TIMEOUT)
            .map_err(|_| "timed out installing WebView2 page observers".to_owned())?
    }

    #[cfg(not(windows))]
    fn install_page_activity_observers(
        &self,
        _control: &WebView2Control,
        _page: &AttestedPage,
        _page_generation: u64,
    ) -> Result<(), String> {
        Ok(())
    }

    /// Answers the dialog `preview_dialog` named, on the WebView UI thread that owns it.
    #[cfg(windows)]
    fn answer_pending_dialog(
        &self,
        dialog_id: u64,
        accept: bool,
        prompt_text: Option<&str>,
    ) -> Result<(), String> {
        use webview2_com::CoTaskMemPWSTR;

        let page = self.page()?;
        let prompt_text = prompt_text.map(str::to_owned);
        let (sender, receiver) = mpsc::sync_channel::<Result<(), String>>(1);
        let scheduling = page.page.with_webview(move |_| {
            let result = (|| -> Result<(), String> {
                let held = HELD_DIALOGS.with(|held| held.borrow_mut().remove(&dialog_id));
                let Some(HeldDialog { args, deferral }) = held else {
                    return Err("the dialog is no longer open".into());
                };
                if accept {
                    if let Some(text) = prompt_text {
                        let text_wide = CoTaskMemPWSTR::from(text.as_str());
                        unsafe { args.SetResultText(*text_wide.as_ref().as_pcwstr()) }
                            .map_err(|error| format!("failed to set the prompt answer: {error}"))?;
                    }
                    unsafe { args.Accept() }
                        .map_err(|error| format!("failed to accept the dialog: {error}"))?;
                }
                unsafe { deferral.Complete() }
                    .map_err(|error| format!("failed to release the dialog: {error}"))
            })();
            let _ = sender.send(result);
        });
        scheduling.map_err(|error| format!("failed to schedule the dialog answer: {error}"))?;
        receiver
            .recv_timeout(EVAL_TIMEOUT)
            .map_err(|_| "timed out answering the dialog".to_owned())?
    }

    #[cfg(not(windows))]
    fn answer_pending_dialog(
        &self,
        _dialog_id: u64,
        _accept: bool,
        _prompt_text: Option<&str>,
    ) -> Result<(), String> {
        Err("page dialogs are held only by the Windows WebView2 browser".into())
    }

    /// Retires the native page whose process died and starts over on the blank start page, kept
    /// visible if it was: the equivalent of Playwright closing a crashed page and opening a new one.
    fn reset_after_crash(&self) -> Result<(), String> {
        let app = self.app_handle()?;
        let was_open = self.status().open;
        self.discard_unattested_native_surface(&app)?;
        {
            let mut state = self.lock_state();
            state.activity.crash = None;
            state.status.error = None;
        }
        self.ensure_page(None, was_open).map(|_| ())
    }
}

/// Folds one DevTools event into the page's activity record.
/// Records an `Overlay` picker event on the WebView2 UI thread.
///
/// WHY the body only writes a field: this runs on the WebView2 UI thread, and `cdp_call`
/// dispatches to that same thread and then blocks waiting for the completion handler — calling it
/// from here deadlocks the pane until the CDP timeout. Every CDP call the pick needs happens later,
/// in `BrowserSession::take_selected_element`, off this thread.
fn record_element_picker_event(
    picker: &mut ElementPickerState,
    page_generation: u64,
    event: &str,
    parameters: &Value,
) {
    match event {
        "Overlay.inspectNodeRequested" => {
            if let Some(backend_node_id) = parameters.get("backendNodeId").and_then(Value::as_i64) {
                picker.note_inspect_node(page_generation, backend_node_id);
            }
        }
        "Overlay.inspectModeCanceled" => picker.cancel(),
        _ => {}
    }
}

fn record_devtools_event(activity: &mut PageActivity, event: &str, parameters: &Value) {
    let string = |pointer: &str| parameters.pointer(pointer).and_then(Value::as_str);
    match event {
        "Network.requestWillBeSent" => {
            let Some(request_id) = string("/requestId") else {
                return;
            };
            activity.record_network_log(
                request_id,
                truncate_network_url(
                    string("/request/url").unwrap_or_default(),
                    PREVIEW_MAX_LOG_TEXT_CHARS,
                ),
                truncate_with_ellipsis(
                    string("/request/method").unwrap_or_default(),
                    PREVIEW_MAX_METHOD_CHARS,
                ),
            );
            let resource_type = normalize_resource_type(string("/type"));
            let frame_id = string("/frameId");
            let main_frame_navigation = resource_type == "document"
                && match (&activity.main_frame_id, frame_id) {
                    (Some(main), Some(frame)) => main == frame,
                    // Until the frame tree answered, a document request is assumed to be the
                    // page's own; subframe loads are the rarer case and only cost a longer wait.
                    _ => true,
                };
            // A redirect re-sends the same request id; keep its first sequence number.
            if activity.requests.contains_key(request_id) {
                return;
            }
            activity.record_request(request_id.to_owned(), resource_type, main_frame_navigation);
        }
        "Network.responseReceived" => {
            let Some(request_id) = string("/requestId") else {
                return;
            };
            let status = parameters
                .pointer("/response/status")
                .and_then(Value::as_i64);
            let status_text = truncate_with_ellipsis(
                string("/response/statusText").unwrap_or_default(),
                PREVIEW_MAX_LOG_TEXT_CHARS,
            );
            if let Some(entry) = activity.network_entry_mut(request_id) {
                entry.status = status;
                entry.status_text = status_text;
            }
        }
        "Network.loadingFinished" => {
            if let Some(request_id) = string("/requestId") {
                activity.finish_request(request_id);
            }
        }
        "Network.loadingFailed" => {
            let Some(request_id) = string("/requestId") else {
                return;
            };
            activity.finish_request(request_id);
            let error_text = truncate_with_ellipsis(
                string("/errorText").unwrap_or_default(),
                PREVIEW_MAX_LOG_TEXT_CHARS,
            );
            if let Some(entry) = activity.network_entry_mut(request_id) {
                entry.failed = true;
                entry.error_text = error_text;
            }
        }
        "Page.loadEventFired" => {
            activity.load_events = activity.load_events.wrapping_add(1);
        }
        "Page.frameNavigated" => {
            if parameters.pointer("/frame/parentId").is_none() {
                if let Some(id) = string("/frame/id") {
                    activity.main_frame_id = Some(id.to_owned());
                }
                activity.main_frame_navigations = activity.main_frame_navigations.wrapping_add(1);
                // A new document: whatever chooser the old one opened is gone with it.
                activity.pending_file_chooser = None;
            }
        }
        "Page.fileChooserOpened" => {
            activity.pending_file_chooser = Some(PendingFileChooser {
                mode: string("/mode").unwrap_or("selectSingle").to_owned(),
                backend_node_id: parameters.pointer("/backendNodeId").and_then(Value::as_u64),
                opened_at_ms: Utc::now().timestamp_millis(),
            });
        }
        _ => {}
    }
}

fn decode_base64(input: &str) -> Result<Vec<u8>, String> {
    if input.len() > (MAX_SCREENSHOT_BYTES / 3 + 1) * 4 + 16 {
        return Err(format!(
            "browser screenshot exceeds the {MAX_SCREENSHOT_BYTES}-byte limit"
        ));
    }
    let mut output = Vec::with_capacity(input.len() / 4 * 3);
    let mut quartet = [0_u8; 4];
    let mut count = 0;
    let mut padding = 0;

    for byte in input.bytes() {
        if byte.is_ascii_whitespace() {
            continue;
        }
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => {
                padding += 1;
                0
            }
            _ => return Err("WebView2 screenshot contains invalid base64 characters".into()),
        };
        if padding > 0 && byte != b'=' {
            return Err("WebView2 screenshot base64 padding is invalid".into());
        }
        quartet[count] = value;
        count += 1;
        if count == 4 {
            if padding > 2 {
                return Err("WebView2 screenshot base64 padding is invalid".into());
            }
            output.push((quartet[0] << 2) | (quartet[1] >> 4));
            if padding < 2 {
                output.push((quartet[1] << 4) | (quartet[2] >> 2));
            }
            if padding == 0 {
                output.push((quartet[2] << 6) | quartet[3]);
            }
            count = 0;
            quartet = [0; 4];
        }
    }
    if count != 0 {
        if padding != 0 || count == 1 {
            return Err("WebView2 screenshot base64 length is invalid".into());
        }
        output.push((quartet[0] << 2) | (quartet[1] >> 4));
        if count == 3 {
            output.push((quartet[1] << 4) | (quartet[2] >> 2));
        }
    }
    if output.len() > MAX_SCREENSHOT_BYTES {
        return Err(format!(
            "browser screenshot exceeds the {MAX_SCREENSHOT_BYTES}-byte limit"
        ));
    }
    Ok(output)
}

fn png_dimensions(bytes: &[u8]) -> Result<(u32, u32), String> {
    const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
    if bytes.len() < 24 || &bytes[..8] != PNG_SIGNATURE || &bytes[12..16] != b"IHDR" {
        return Err("WebView2 screenshot is not a valid PNG".into());
    }
    let width = u32::from_be_bytes(bytes[16..20].try_into().unwrap());
    let height = u32::from_be_bytes(bytes[20..24].try_into().unwrap());
    if width == 0 || height == 0 {
        return Err("WebView2 screenshot PNG dimensions are invalid".into());
    }
    Ok((width, height))
}

fn automation_script(body: &str) -> String {
    format!(
        r#"
(function() {{
  try {{
    const __state = window.__MEWORK_BROWSER_RUNTIME__;
    if (!__state) throw new Error("browser automation initialization script is not ready");
    const __value = (() => {{ {body} }})();
    return {{ok:true, value:__state.serialize(__value)}};
  }} catch (error) {{
    return {{ok:false, error:{{name:String(error?.name || "Error"), message:String(error?.message || error), stack:String(error?.stack || "").slice(0,8192)}}}};
  }}
}})()
"#
    )
}

fn decode_eval_response(raw: &str) -> Result<Value, String> {
    let mut value: Value = serde_json::from_str(raw).map_err(|error| {
        format!(
            "browser returned invalid JSON: {error}; raw={}",
            truncate(raw, 512)
        )
    })?;
    // Some WebKit bindings wrap an already-serialized callback value in a JSON string.
    if let Value::String(inner) = &value {
        if let Ok(decoded) = serde_json::from_str::<Value>(inner) {
            value = decoded;
        }
    }
    let object = value.as_object().ok_or_else(|| {
        format!(
            "browser script did not return a result object: {}",
            truncate(raw, 512)
        )
    })?;
    if object.get("ok").and_then(Value::as_bool) == Some(true) {
        return Ok(object.get("value").cloned().unwrap_or(Value::Null));
    }
    let error = object.get("error").cloned().unwrap_or(Value::Null);
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("unknown JavaScript error");
    let name = error.get("name").and_then(Value::as_str).unwrap_or("Error");
    Err(format!("browser script failed: {name}: {message}"))
}

fn truncate(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

fn js_string_literal(value: &str) -> Result<String, String> {
    serde_json::to_string(value)
        .map_err(|error| format!("failed to create JavaScript string: {error}"))
}

fn js_optional_literal(value: Option<&str>) -> Result<String, String> {
    value
        .map(js_string_literal)
        .unwrap_or_else(|| Ok("null".into()))
}

fn validate_target_input(selector: Option<&str>, allow_empty: bool) -> Result<TargetSpec, String> {
    if !allow_empty && selector.is_none() {
        return Err("selector is required".into());
    }
    Ok(TargetSpec {
        selector: selector.map(validate_selector).transpose()?,
    })
}

fn validate_selector(selector: &str) -> Result<String, String> {
    if selector.trim().is_empty() {
        return Err("selector must not be empty".into());
    }
    if selector.chars().count() > MAX_SELECTOR_CHARS {
        return Err(format!(
            "selector exceeds the {MAX_SELECTOR_CHARS}-character limit"
        ));
    }
    if selector.contains('\0') {
        return Err("selector must not contain NUL".into());
    }
    Ok(selector.to_owned())
}

/// Parses toolbar/tool input into the only URL classes allowed in the untrusted page webview.
///
/// `level` is the effective security level of the conversation that owns this
/// session; it decides only whether the development-server origin is reachable.
pub fn parse_browser_url(input: &str, level: SecurityLevel) -> Result<Url, String> {
    let input = input.trim();
    if input.is_empty() {
        return Err("browser URL must not be empty".into());
    }
    if input.chars().count() > MAX_URL_CHARS {
        return Err(format!(
            "browser URL exceeds the {MAX_URL_CHARS}-character limit"
        ));
    }
    if input.chars().any(char::is_control) {
        return Err("browser URL must not contain control characters".into());
    }

    let normalized = if input.eq_ignore_ascii_case(DEFAULT_URL) {
        DEFAULT_URL.to_owned()
    } else if !input.contains("://") {
        // A schemeless target has to be guessed, and https is the expensive guess
        // for a local address: development servers overwhelmingly speak plain
        // HTTP, so upgrading turns "open my dev server" into a TLS handshake
        // error against a port that was never listening for one. Deciding by
        // address rather than by prefix also covers `myapp.localhost:3000` and
        // LAN literals, which the earlier prefix test silently sent to https.
        let local = Url::parse(&format!("http://{input}"))
            .is_ok_and(|url| crate::http_util::is_local_network_url(&url));
        format!("{}://{input}", if local { "http" } else { "https" })
    } else {
        input.to_owned()
    };
    if raw_authority_has_userinfo(&normalized) {
        return Err("browser URL must not contain a username, password, or userinfo".into());
    }
    let url = Url::parse(&normalized).map_err(|error| format!("invalid browser URL: {error}"))?;
    if !is_navigation_allowed_at(&url, level) {
        return Err(
            "browser allows only http://, https://, and about:blank URLs without userinfo and cannot open the application own trusted origins"
                .into(),
        );
    }
    Ok(url)
}

fn browser_url_origin(input: &str) -> Option<String> {
    Url::parse(input)
        .ok()
        .filter(|url| matches!(url.scheme(), "http" | "https"))
        .map(|url| url.origin().ascii_serialization())
}

/// A takeover the user approved for one origin must not follow the page to the next one. The
/// committed-load callback owns this release because `status.url` reports an accepted target
/// before the old document is actually gone.
fn clear_credential_takeover_after_committed_url(state: &mut RuntimeState, url: &str) {
    let committed = browser_url_origin(url);
    if state
        .credential_takeover_grant
        .as_ref()
        .is_some_and(|origin| committed.as_deref() != Some(origin))
    {
        state.credential_takeover_grant = None;
    }
}

/// The address a cold-closed session comes back to.
///
/// A staged file preview does not survive the controller: cold close discards the lone copy, and
/// the virtual-host mapping dies with the WebView2 instance that holds it. The address it
/// committed is a well-formed https URL, so nothing downstream refuses it — the page would simply
/// return pointed at a host that no longer resolves, with nothing on screen to explain why.
fn cold_resume_url(state: &RuntimeState) -> String {
    if state.status.url.trim().is_empty() || state.file_preview.is_some() {
        DEFAULT_URL.to_owned()
    } else {
        state.status.url.clone()
    }
}

/// The mapped copy stops being reachable the moment a document from any other origin commits.
/// Only the state half runs here; the native release needs a page and must happen outside the
/// state lock.
fn take_file_preview_after_committed_url(
    state: &mut RuntimeState,
    url: &str,
) -> Option<FilePreviewGrant> {
    let committed = browser_url_origin(url);
    let staged = state
        .file_preview
        .as_ref()
        .and_then(|grant| browser_url_origin(&grant.url))?;
    if committed.as_deref() == Some(staged.as_str()) {
        return None;
    }
    state.file_preview.take()
}

fn release_file_preview(page: &AttestedPage, grant: FilePreviewGrant) {
    let _ = page.with_native_tail(|native, permit| {
        browser_profile_data::clear_virtual_host_folder(
            native,
            permit,
            browser_file_preview::PREVIEW_VIRTUAL_HOST,
        )
    });
    browser_file_preview::discard(&grant.directory);
}

/// Teardown variant: the controller is going away, and a mapping does not outlive the WebView2
/// instance that holds it, so only the staged copy still needs removing.
fn discard_file_preview_staging(state: &mut RuntimeState) {
    if let Some(grant) = state.file_preview.take() {
        browser_file_preview::discard(&grant.directory);
    }
}

fn start_page_preferences_script(
    theme: Option<&str>,
    language: Option<&str>,
    generation: u64,
) -> String {
    debug_assert!(generation <= MAX_UI_PREFERENCE_GENERATION);
    // Values enter RuntimeState only through the strict day/night and zh/en setters. JSON quoting
    // remains the final boundary so this helper is safe even if those enums grow in the future.
    let theme = serde_json::to_string(&theme).expect("serializing an optional string cannot fail");
    let language =
        serde_json::to_string(&language).expect("serializing an optional string cannot fail");
    format!(
        r#"(() => {{
  if (location.href !== "about:blank") return false;
  const state = window.__MEWORK_BROWSER_RUNTIME__;
  if (!state) return false;
  const theme = {theme};
  const language = {language};
  return state.setUiPreferences(theme, language, {generation});
}})();"#
    )
}

/// Admission for anything the untrusted page WebView may load, and for handing a
/// URL to the system browser. The application's own private origins are refused
/// at every security level: they are the trusted surfaces the page must never be
/// able to reach.
pub fn is_navigation_allowed(url: &Url) -> bool {
    if url.as_str() == DEFAULT_URL {
        return true;
    }
    matches!(url.scheme(), "http" | "https")
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && !raw_authority_has_userinfo(url.as_str())
        && !is_reserved_app_origin(url)
}

/// The same admission plus the development-server reservation, which only the
/// embedded page WebView needs.
///
/// Full access lifts the reservation. A release build serves the frontend from
/// the custom protocol and installs no development port at all, so there the
/// reservation is empty and loopback HTTP names whatever server the user happens
/// to be running — exactly the page they were trying to open. A conversation at
/// full access has already been granted every other unbounded browser action, so
/// refusing this one origin only ever cost the user their own dev server.
fn is_navigation_allowed_at(url: &Url, level: SecurityLevel) -> bool {
    is_navigation_allowed_with(url, level, app_dev_server_port())
}

/// Admission against an explicit development-server port so the reservation can
/// be exercised in both of its states without touching process-global state.
fn is_navigation_allowed_with(url: &Url, level: SecurityLevel, dev_port: Option<u16>) -> bool {
    is_navigation_allowed(url)
        && (level == SecurityLevel::FullAccess || !is_app_dev_server_origin(url, dev_port))
}

/// Records the development server that serves the trusted frontend.
///
/// Called once during application setup, before any WebView exists, and only in
/// a development build. Later calls are ignored: what a navigation callback
/// admits must not change underneath a page that is already loaded.
pub(crate) fn install_app_dev_server(url: &Url) {
    let Some(port) = url.port() else {
        return;
    };
    let _ = APP_DEV_SERVER.set(AppDevServer {
        origin: url.origin().ascii_serialization(),
        port,
    });
}

fn app_dev_server_port() -> Option<u16> {
    APP_DEV_SERVER.get().map(|server| server.port)
}

/// The exact origin the trusted frontend is served from, if this build serves it
/// over HTTP at all.
///
/// The main window compares its own navigations against this to recognize its
/// own page. That test is exact on purpose, unlike the reservation below: the
/// window is the trusted surface, so admitting a *different* server that merely
/// shares the port would replace the application's own UI with someone else's.
pub(crate) fn app_dev_server_origin() -> Option<&'static str> {
    APP_DEV_SERVER.get().map(|server| server.origin.as_str())
}

fn is_reserved_app_origin(url: &Url) -> bool {
    matches!(
        url.host_str(),
        Some(host)
            if host.eq_ignore_ascii_case("tauri.localhost")
                || host.eq_ignore_ascii_case("asset.localhost")
                || host.eq_ignore_ascii_case("ipc.localhost")
                || host.to_ascii_lowercase().ends_with(".tauri.localhost")
    )
}

/// Loopback on the port the development server uses. In a development build the
/// trusted application frontend is served there, so the untrusted page must not
/// load it below full access. Every loopback spelling counts: the configuration
/// names one host, and the server answers to all of them on that port. The
/// scheme does not: the development server speaks plain HTTP, so an HTTPS URL on
/// the same port is a different origin that was never this frontend.
fn is_app_dev_server_origin(url: &Url, dev_port: Option<u16>) -> bool {
    let Some(dev_port) = dev_port else {
        return false;
    };
    if url.scheme() != "http" || url.port() != Some(dev_port) {
        return false;
    }
    match url.host() {
        Some(Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
        Some(Host::Ipv4(host)) => host.is_loopback(),
        Some(Host::Ipv6(host)) => host.is_loopback(),
        None => false,
    }
}

fn raw_authority_has_userinfo(input: &str) -> bool {
    let Some(scheme_end) = input.find("://") else {
        return false;
    };
    let authority = &input[scheme_end + 3..];
    let end = authority.find(['/', '?', '#']).unwrap_or(authority.len());
    authority[..end].contains('@')
}

const BROWSER_INITIALIZATION_SCRIPT: &str = r#"
(() => {
  "use strict";
  if (window.top !== window || Object.prototype.hasOwnProperty.call(window, "__MEWORK_BROWSER_RUNTIME__")) return;

  let uiTheme = window.matchMedia?.("(prefers-color-scheme: dark)")?.matches ? "night" : "day";
  let uiLanguage = String(navigator.language || "").toLowerCase().startsWith("zh") ? "zh-CN" : "en-US";
  let uiPreferenceGeneration = 0;
  const applyUiTheme = theme => {
    uiTheme = theme === "night" ? "night" : "day";
    if (location.href !== "about:blank") return uiTheme;
    document.documentElement.dataset.theme = uiTheme;
    document.documentElement.style.colorScheme = uiTheme === "night" ? "dark" : "light";
    return uiTheme;
  };
  const setUiTheme = theme => {
    const applied = applyUiTheme(theme);
    return {theme: applied, startPage: location.href === "about:blank"};
  };
  const startPageTitle = () => uiLanguage === "zh-CN" ? "新标签页" : "New tab";
  const applyStartPageCopy = () => {
    if (location.href !== "about:blank") return false;
    document.documentElement.lang = uiLanguage;
    document.title = startPageTitle();
    return true;
  };
  const setUiLanguage = language => {
    uiLanguage = String(language || "").toLowerCase().startsWith("zh") ? "zh-CN" : "en-US";
    return {language: uiLanguage, startPage: applyStartPageCopy()};
  };
  // The standby surface the user actually reads is the pane's own React body card, drawn in the
  // main window over a fully withdrawn page region. This document only has to be a themed,
  // network-inert backdrop carrying the `.mework-start` attestation anchor — any copy here would
  // be a second, untranslated source of truth for a page nobody sees.
  const mountStartPage = () => {
    if (location.href !== "about:blank") return false;
    if (!document.body) {
      document.addEventListener("DOMContentLoaded", mountStartPage, {once:true});
      return false;
    }
    applyUiTheme(uiTheme);
    applyStartPageCopy();
    if (document.querySelector(".mework-start")) {
      return true;
    }
    const style = document.createElement("style");
    style.textContent = `
      :root{--mework-start-bg:#f7f7f4;background:var(--mework-start-bg)}
      :root[data-theme="night"]{--mework-start-bg:#171817}
      *{box-sizing:border-box}html,body{width:100%;height:100%;margin:0}body{background:var(--mework-start-bg)}
      .mework-start{width:100%;height:100%;user-select:none}
    `;
    document.head.appendChild(style);
    document.body.innerHTML = `<main class="mework-start"></main>`;
    return true;
  };
  const setUiPreferences = (theme, language, generation) => {
    const nextGeneration = Number(generation);
    if (!Number.isSafeInteger(nextGeneration) || nextGeneration < 0) {
      return {applied:false, generation:uiPreferenceGeneration, startPage:location.href === "about:blank"};
    }
    if (nextGeneration < uiPreferenceGeneration) {
      return {applied:false, generation:uiPreferenceGeneration, startPage:location.href === "about:blank"};
    }
    uiPreferenceGeneration = nextGeneration;
    if (theme !== null) uiTheme = theme === "night" ? "night" : "day";
    if (language !== null) {
      uiLanguage = String(language || "").toLowerCase().startsWith("zh") ? "zh-CN" : "en-US";
    }
    const startPage = location.href === "about:blank";
    const mounted = startPage ? mountStartPage() : false;
    return {applied:true, generation:uiPreferenceGeneration, startPage, mounted};
  };
  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", mountStartPage, {once:true});
  } else {
    mountStartPage();
  }

  const consoleEntries = [];
  const networkEntries = [];
  const refs = new WeakMap();
  const elements = new Map();
  let nextRef = 1;
  const cap = (array, value, maximum = 1000) => {
    array.push(value);
    if (array.length > maximum) array.splice(0, array.length - maximum);
    return value;
  };
  const elementRef = element => {
    let value = refs.get(element);
    if (!value) {
      value = `e${nextRef++}`;
      refs.set(element, value);
      elements.set(value, element);
    }
    return value;
  };
  const accessibleName = element => {
    const labelledBy = element.getAttribute?.("aria-labelledby");
    if (labelledBy) {
      const text = labelledBy.split(/\s+/).map(id => document.getElementById(id)?.textContent || "").join(" ").trim();
      if (text) return text;
    }
    return String(
      element.getAttribute?.("aria-label") || element.getAttribute?.("alt") ||
      element.getAttribute?.("title") || (element instanceof HTMLInputElement ? element.placeholder : "") ||
      element.innerText || element.textContent || ""
    ).replace(/\s+/g, " ").trim().slice(0, 1000);
  };
  const roleOf = element => element.getAttribute?.("role") || ({
    A: "link", BUTTON: "button", INPUT: element.type === "checkbox" ? "checkbox" : element.type === "radio" ? "radio" : "textbox",
    TEXTAREA: "textbox", SELECT: "combobox", OPTION: "option", IMG: "img", SUMMARY: "button"
  }[element.tagName] || null);
  const describe = element => {
    if (!(element instanceof Element)) return null;
    const rect = element.getBoundingClientRect();
    const output = {
      ref: elementRef(element), tag: element.tagName.toLowerCase(), role: roleOf(element), name: accessibleName(element),
      disabled: Boolean(element.disabled || element.getAttribute("aria-disabled") === "true"),
      hidden: rect.width <= 0 || rect.height <= 0 || getComputedStyle(element).visibility === "hidden",
      rect: {x: Math.round(rect.x), y: Math.round(rect.y), width: Math.round(rect.width), height: Math.round(rect.height)}
    };
    if (element instanceof HTMLInputElement) {
      output.type = element.type;
      output.value = element.type === "password" || element.hasAttribute("data-mework-protected-password") ? "••••••" : String(element.value).slice(0, 2000);
      if (["checkbox", "radio"].includes(element.type)) output.checked = element.checked;
    } else if (element instanceof HTMLTextAreaElement || element instanceof HTMLSelectElement) {
      output.value = String(element.value).slice(0, 2000);
    }
    if (element instanceof HTMLAnchorElement) output.href = element.href;
    if (element instanceof HTMLSelectElement) output.values = [...element.selectedOptions].map(option => option.value);
    return output;
  };
  const takeString = (value, budget, limit = 200000) => {
    const text = String(value);
    const remaining = Math.max(0, Math.min(limit, budget.maxChars - budget.chars));
    if (remaining === 0) return "[BudgetExceeded]";
    const output = text.slice(0, remaining);
    budget.chars += output.length;
    return output;
  };
  const serialize = (value, depth = 0, seen = new WeakSet(), budget = {nodes:0, chars:0, maxNodes:10000, maxChars:2000000}) => {
    budget.nodes += 1;
    if (budget.nodes > budget.maxNodes || budget.chars >= budget.maxChars) return "[BudgetExceeded]";
    if (value === null || value === undefined || typeof value === "boolean" || typeof value === "number") return value ?? null;
    if (typeof value === "string") return takeString(value, budget);
    if (typeof value === "bigint") return takeString(`${value}n`, budget);
    if (typeof value === "symbol" || typeof value === "function") return takeString(value, budget);
    if (value instanceof Element) return serialize(describe(value), depth + 1, seen, budget);
    if (value instanceof Error) return {name:takeString(value.name, budget, 256), message:takeString(value.message, budget, 16000), stack:takeString(value.stack || "", budget, 8192)};
    if (value instanceof Date) return takeString(value.toISOString(), budget, 64);
    if (depth >= 8) return "[MaxDepth]";
    if (seen.has(value)) return "[Circular]";
    seen.add(value);
    if (Array.isArray(value)) return value.slice(0,2000).map(item => serialize(item, depth + 1, seen, budget));
    const output = {};
    for (const rawKey of Object.keys(value).slice(0,500)) {
      if (budget.nodes >= budget.maxNodes || budget.chars >= budget.maxChars) break;
      const key = takeString(rawKey, budget, 512);
      try { output[key] = serialize(value[rawKey], depth + 1, seen, budget); } catch (error) { output[key] = takeString(`[Unreadable: ${error}]`, budget, 1024); }
    }
    return output;
  };
  const INTERACTIVE = "a[href],button,input,textarea,select,summary,[role],[contenteditable='true'],[tabindex]:not([tabindex='-1'])";
  const isInteractive = element => { try { return element.matches(INTERACTIVE); } catch (_) { return false; } };
  const isHidden = element => {
    const rect = element.getBoundingClientRect();
    if (rect.width <= 0 && rect.height <= 0) return true;
    const style = getComputedStyle(element);
    return style.visibility === "hidden" || style.display === "none";
  };
  const dialogState = {records: [], armed: false, accept: null, promptText: null};
  let trustedPointerSerial = 0;
  let trustedPointerTarget = null;
  let trustedPointerButton = null;
  for (const [eventType, button] of [["click", "left"], ["auxclick", "middle"], ["contextmenu", "right"]]) {
    addEventListener(eventType, event => {
      if (!event.isTrusted) return;
      trustedPointerSerial += 1;
      trustedPointerTarget = event.target;
      trustedPointerButton = button;
    }, true);
  }

  const resolve = (selector, ref, optional = false) => {
    if (selector !== null && ref !== null) throw new Error("provide either selector or ref, not both");
    let element = null;
    if (ref !== null) {
      element = elements.get(ref) || null;
      if (element && !element.isConnected) { elements.delete(ref); element = null; }
    } else if (selector !== null) {
      element = document.querySelector(selector);
    }
    if (!element && !optional) throw new Error(ref !== null ? `Ref ${ref} not found in the current page snapshot. Try capturing new snapshot.` : `"${selector}" does not match any elements.`);
    return element;
  };
  const clickCheckpoint = () => trustedPointerSerial;
  const trustedClickObserved = (selector, ref, checkpoint, button) => {
    const observed = trustedPointerSerial > Number(checkpoint) && trustedPointerButton === button;
    // The click may already have replaced or removed its own target (a link that navigated, a
    // button that re-rendered its form); a trusted click since the checkpoint is then the only
    // evidence left, and it is enough.
    const element = resolve(selector, ref, true);
    if (!element) return observed;
    const target = trustedPointerTarget;
    return observed && target instanceof Node && (target === element || element.contains(target));
  };
  const syntheticClick = (selector, ref) => {
    const element = resolve(selector, ref);
    if (typeof element.click !== "function") throw new Error("target element is not clickable");
    element.click();
    return true;
  };
  const evalElement = ref => {
    const element = elements.get(ref);
    if (!element || !element.isConnected) throw new Error(`Ref ${ref} not found in the current page snapshot. Try capturing new snapshot.`);
    return element;
  };
  const describeTarget = (selector, ref) => describe(resolve(selector, ref));
  const classifyControl = (selector, ref) => {
    const element = resolve(selector, ref);
    if (element instanceof HTMLSelectElement) return "select";
    if (element instanceof HTMLInputElement && ["checkbox", "radio"].includes(element.type)) return "toggle";
    if (element instanceof HTMLInputElement && element.type === "file") return "file";
    return "other";
  };

  // Playwright-style actionability: exists, visible, size-settled between polls, enabled, and its
  // interaction point is the top element there. Returns a viewport-relative CSS point for CDP input.
  const stability = new WeakMap();
  const actionable = (selector, ref, expectEnabled) => {
    const element = resolve(selector, ref, true);
    // A ref names an element the model saw in a snapshot. If it is gone, no amount of waiting
    // brings it back — the page navigated or re-rendered — so say so now rather than spending the
    // whole actionability budget and then reporting the generic "never became interactive".
    if (!element && ref !== null) return {status: "fatal", reason: `Ref ${ref} not found in the current page snapshot. Try capturing new snapshot.`};
    if (!element) return {status: "retry", reason: "element has not appeared yet"};
    if (!element.isConnected) return {status: "retry", reason: "element was removed from the document"};
    const rect = element.getBoundingClientRect();
    if (rect.width <= 0 || rect.height <= 0) return {status: "retry", reason: "element is not visible (zero dimensions)"};
    const style = getComputedStyle(element);
    if (style.visibility === "hidden" || style.display === "none") return {status: "retry", reason: "element is hidden by CSS"};
    const centerX = rect.left + rect.width / 2;
    const centerY = rect.top + rect.height / 2;
    if (centerX < 1 || centerX > innerWidth - 1 || centerY < 1 || centerY > innerHeight - 1) {
      element.scrollIntoView({block:"center", inline:"center", behavior:"instant"});
      stability.delete(element);
      return {status:"retry", reason:"scrolling element into an actionable area"};
    }
    const box = {x: rect.x, y: rect.y, width: rect.width, height: rect.height};
    const previous = stability.get(element);
    stability.set(element, box);
    const settled = previous && Math.abs(previous.x - box.x) < 1 && Math.abs(previous.y - box.y) < 1 && Math.abs(previous.width - box.width) < 1 && Math.abs(previous.height - box.height) < 1;
    if (!settled) return {status: "retry", reason: "element is still moving or animating"};
    if (expectEnabled && (element.disabled || element.getAttribute("aria-disabled") === "true")) return {status: "retry", reason: "element is disabled"};
    const px = Math.min(Math.max(centerX, 1), Math.max(1, innerWidth - 1));
    const py = Math.min(Math.max(centerY, 1), Math.max(1, innerHeight - 1));
    const topElement = document.elementFromPoint(px, py);
    const reachable = !topElement || topElement === element || element.contains(topElement) || topElement.contains(element);
    if (!reachable) return {status: "retry", reason: "interaction point is covered by another element"};
    return {status: "ok", x: px, y: py, element: describe(element)};
  };

  const elementClip = (selector, ref) => {
    const element = resolve(selector, ref);
    element.scrollIntoView({block: "center", inline: "center", behavior: "instant"});
    const rect = element.getBoundingClientRect();
    return {x: rect.left + scrollX, y: rect.top + scrollY, width: rect.width, height: rect.height};
  };
  const scrollTo = (selector, ref) => {
    const element = resolve(selector, ref);
    element.scrollIntoView({block: "center", inline: "nearest", behavior: "instant"});
    return {target: describe(element), scrollX, scrollY};
  };
  const scrollReport = () => ({scrollX, scrollY, maxX: Math.max(0, document.documentElement.scrollWidth - innerWidth), maxY: Math.max(0, document.documentElement.scrollHeight - innerHeight)});
  const scrollBy = (selector, ref, deltaX, deltaY) => {
    const element = resolve(selector, ref, true);
    if (element && typeof element.scrollBy === "function") element.scrollBy({left: deltaX, top: deltaY, behavior: "instant"});
    else window.scrollBy({left: deltaX, top: deltaY, behavior: "instant"});
    return scrollReport();
  };
  const syntheticHover = (selector, ref) => {
    const element = resolve(selector, ref);
    element.scrollIntoView({block: "center", inline: "center", behavior: "instant"});
    const rect = element.getBoundingClientRect();
    const options = {bubbles: true, cancelable: true, clientX: rect.left + rect.width / 2, clientY: rect.top + rect.height / 2, view: window};
    for (const type of ["mouseover", "mouseenter", "mousemove"]) element.dispatchEvent(new MouseEvent(type, options));
    return describe(element);
  };
  const syntheticKey = specification => {
    const pieces = specification.split("+").map(piece => piece.trim()).filter(Boolean);
    const rawKey = pieces.pop() || specification;
    const aliases = {Esc: "Escape", Return: "Enter", Space: " ", Del: "Delete", Cmd: "Meta", Ctrl: "Control"};
    const key = aliases[rawKey] || rawKey;
    const modifiers = new Set(pieces.map(piece => aliases[piece] || piece));
    const target = document.activeElement || document.body;
    const options = {key, code: key.length === 1 ? (/^[a-z]$/i.test(key) ? "Key" + key.toUpperCase() : key) : key, bubbles: true, cancelable: true, altKey: modifiers.has("Alt"), ctrlKey: modifiers.has("Control"), metaKey: modifiers.has("Meta"), shiftKey: modifiers.has("Shift")};
    const accepted = target.dispatchEvent(new KeyboardEvent("keydown", options));
    if (accepted && key === "Enter") {
      if (target instanceof HTMLButtonElement || (target instanceof HTMLInputElement && ["button", "submit"].includes(target.type))) target.click();
      else if (target.form && target.form.requestSubmit) target.form.requestSubmit();
    }
    if (accepted && key === "Escape" && typeof target.blur === "function") target.blur();
    if (accepted && key === "Tab") {
      const focusable = [...document.querySelectorAll("a[href],button,input,select,textarea,[tabindex]:not([tabindex='-1'])")].filter(node => !node.disabled && node.getClientRects().length > 0);
      const index = focusable.indexOf(target);
      const direction = options.shiftKey ? -1 : 1;
      const next = focusable[(index + direction + focusable.length) % focusable.length];
      if (next) next.focus();
    }
    target.dispatchEvent(new KeyboardEvent("keyup", options));
    return {key, accepted, target: describe(document.activeElement || target)};
  };
  const prepareFill = (selector, ref, clear) => {
    const element = resolve(selector, ref);
    element.scrollIntoView({block: "center", inline: "nearest", behavior: "instant"});
    if (typeof element.focus === "function") element.focus({preventScroll: true});
    if (element instanceof HTMLInputElement || element instanceof HTMLTextAreaElement) {
      if (element.disabled || element.readOnly) throw new Error("target input is not editable");
      if (clear) { try { element.select(); } catch (_) { element.setSelectionRange && element.setSelectionRange(0, element.value.length); } }
      else { const end = element.value.length; try { element.setSelectionRange(end, end); } catch (_) {} }
    } else if (element.isContentEditable) {
      const range = document.createRange();
      range.selectNodeContents(element);
      if (!clear) range.collapse(false);
      const selection = getSelection();
      selection.removeAllRanges();
      selection.addRange(range);
    } else {
      throw new Error("target element is not an editable input");
    }
    return {tag: element.tagName.toLowerCase()};
  };
  const setValue = (selector, ref, text, clear) => {
    const element = resolve(selector, ref);
    if (typeof element.focus === "function") element.focus({preventScroll: true});
    if (element instanceof HTMLInputElement || element instanceof HTMLTextAreaElement) {
      if (element.disabled || element.readOnly) throw new Error("target input is not editable");
      const prototype = element instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
      const setter = Object.getOwnPropertyDescriptor(prototype, "value")?.set;
      const next = clear ? text : String(element.value ?? "") + text;
      if (setter) setter.call(element, next); else element.value = next;
      element.dispatchEvent(new InputEvent("input", {bubbles: true, inputType: "insertText", data: text}));
      element.dispatchEvent(new Event("change", {bubbles: true}));
    } else if (element.isContentEditable) {
      if (clear) element.textContent = "";
      element.textContent = String(element.textContent ?? "") + text;
      element.dispatchEvent(new InputEvent("input", {bubbles: true, inputType: "insertText", data: text}));
    } else {
      throw new Error("target element is not an editable input");
    }
    return describe(element);
  };
  const dialogControl = (arm, accept, promptText) => {
    if (arm) { dialogState.armed = true; dialogState.accept = accept; dialogState.promptText = promptText; }
    return {records: dialogState.records.slice(), count: dialogState.records.length, armed: dialogState.armed, pendingAccept: dialogState.accept, pendingPromptText: dialogState.promptText};
  };

  const TREE_TAGS = new Set(["a", "button", "input", "textarea", "select", "summary", "label", "nav", "main", "header", "footer", "form", "table", "tr", "th", "td", "ul", "ol", "li", "option", "dialog", "details", "h1", "h2", "h3", "h4", "h5", "h6", "img", "p", "section", "article"]);
  const buildTree = limit => {
    const lines = [];
    const walk = (element, depth) => {
      if (lines.length >= 1000 || !(element instanceof Element) || isHidden(element)) return;
      const tag = element.tagName.toLowerCase();
      if (tag === "script" || tag === "style" || tag === "noscript") return;
      const role = roleOf(element);
      let childDepth = depth;
      if (role || TREE_TAGS.has(tag)) {
        const info = describe(element);
        let line = "  ".repeat(Math.min(depth, 24)) + "- " + (role || tag);
        if (info.name) line += ' "' + info.name.slice(0, 120) + '"';
        const states = [];
        if (info.disabled) states.push("disabled");
        if (info.checked === true) states.push("checked");
        if (typeof info.value === "string" && info.value) states.push("value=" + JSON.stringify(info.value.slice(0, 40)));
        if (states.length) line += " [" + states.join(", ") + "]";
        if (isInteractive(element)) line += " {ref=" + info.ref + "}";
        lines.push(line);
        childDepth = depth + 1;
      }
      for (const child of element.children) walk(child, childDepth);
    };
    walk(document.body || document.documentElement, 0);
    let tree = lines.join("\n");
    if (tree.length > limit) tree = tree.slice(0, limit) + "\n… [truncated]";
    return tree;
  };
  const snapshotTree = maxChars => {
    const limit = Math.max(200, Number(maxChars) || 0);
    const text = buildTree(limit);
    return {text, truncated: text.endsWith("[truncated]")};
  };
  const snapshot = maxChars => {
    const bodyText = String(document.body?.innerText || document.documentElement?.innerText || "").replace(/\r/g, "");
    const candidates = [...document.querySelectorAll(INTERACTIVE)];
    const interactive = [];
    for (const element of candidates) {
      if (interactive.length >= 400) break;
      const description = describe(element);
      if (!description.hidden) interactive.push(description);
    }
    return {
      url: location.href, title: document.title, readyState: document.readyState,
      viewport: {width: innerWidth, height: innerHeight, scrollX, scrollY, documentWidth: document.documentElement.scrollWidth, documentHeight: document.documentElement.scrollHeight},
      tree: buildTree(maxChars),
      text: bodyText.slice(0, maxChars), truncated: bodyText.length > maxChars,
      elements: interactive,
      dialogs: dialogState.records.slice(-5)
    };
  };

  const state = {
    consoleEntries, networkEntries, serialize, resolve, describe, snapshot, snapshotTree,
    actionable, elementClip, scrollTo, scrollReport, scrollBy, syntheticClick, syntheticHover, syntheticKey,
    clickCheckpoint, trustedClickObserved,
    prepareFill, setValue, describeTarget, classifyControl, evalElement, dialogControl,
    setUiTheme, setUiLanguage, setUiPreferences, mountStartPage
  };
  Object.freeze(state);
  Object.defineProperty(window, "__MEWORK_BROWSER_RUNTIME__", {value:state, writable:false, configurable:false, enumerable:false});

  for (const level of ["debug", "log", "info", "warn", "error"]) {
    try {
      const original = console[level]?.bind(console);
      if (!original) continue;
      console[level] = (...args) => {
        const serializedArgs = serialize(args, 0, new WeakSet(), {nodes:0, chars:0, maxNodes:500, maxChars:32000});
        let message;
        try { message = JSON.stringify(serializedArgs).slice(0,16000); } catch (_) { message = "[Unserializable console arguments]"; }
        cap(consoleEntries, {timestamp:Date.now(), level, message, args:serializedArgs}, 200);
        return original(...args);
      };
    } catch (_) {}
  }
  addEventListener("error", event => cap(consoleEntries, {timestamp:Date.now(), level:"error", message:String(event.message || event.error || "Script error").slice(0,16000), source:String(event.filename || "").slice(0,8192) || null, line:event.lineno || null, column:event.colno || null}, 200));
  addEventListener("unhandledrejection", event => cap(consoleEntries, {timestamp:Date.now(), level:"error", kind:"unhandledrejection", message:String(event.reason?.message || event.reason).slice(0,16000), reason:serialize(event.reason, 0, new WeakSet(), {nodes:0, chars:0, maxNodes:500, maxChars:32000})}, 200));

  try {
    const originalFetch = window.fetch.bind(window);
    window.fetch = (...args) => {
      let request;
      try { request = new Request(args[0], args[1]); } catch (_) { request = null; }
      const entry = cap(networkEntries, {id:`f${Date.now()}-${Math.random().toString(36).slice(2)}`, kind:"fetch", url:String(request?.url || args[0]).slice(0,8192), method:String(request?.method || args[1]?.method || "GET").slice(0,32), startedAt:Date.now(), pending:true});
      return originalFetch(...args).then(response => {
        Object.assign(entry, {pending:false, endedAt:Date.now(), durationMs:Date.now()-entry.startedAt, status:response.status, statusText:String(response.statusText).slice(0,1024), ok:response.ok, redirected:response.redirected, responseUrl:String(response.url).slice(0,8192)});
        try {
          const type = response.headers.get("content-type") || "";
          if (/json|text|xml|javascript|html|urlencoded/i.test(type)) {
            response.clone().text().then(body => { entry.bodyPreview = String(body).slice(0, 2048); }).catch(() => {});
          }
        } catch (_) {}
        return response;
      }, error => { Object.assign(entry, {pending:false, endedAt:Date.now(), durationMs:Date.now()-entry.startedAt, error:String(error)}); throw error; });
    };
  } catch (_) {}

  try {
    const originalOpen = XMLHttpRequest.prototype.open;
    const originalSend = XMLHttpRequest.prototype.send;
    const metadata = new WeakMap();
    XMLHttpRequest.prototype.open = function(method, url, ...rest) {
      metadata.set(this, {method:String(method || "GET").toUpperCase().slice(0,32), url:new URL(String(url), location.href).href.slice(0,8192)});
      return originalOpen.call(this, method, url, ...rest);
    };
    XMLHttpRequest.prototype.send = function(...args) {
      const info = metadata.get(this) || {method:"GET", url:""};
      const entry = cap(networkEntries, {id:`x${Date.now()}-${Math.random().toString(36).slice(2)}`, kind:"xhr", ...info, startedAt:Date.now(), pending:true});
      const finish = () => {
        Object.assign(entry, {pending:false, endedAt:Date.now(), durationMs:Date.now()-entry.startedAt, status:this.status, statusText:String(this.statusText).slice(0,1024), responseUrl:String(this.responseURL).slice(0,8192)});
        try { if (this.responseType === "" || this.responseType === "text") entry.bodyPreview = String(this.responseText || "").slice(0, 2048); } catch (_) {}
      };
      this.addEventListener("loadend", finish, {once:true});
      this.addEventListener("error", () => { entry.error="Network error"; }, {once:true});
      this.addEventListener("abort", () => { entry.error="Aborted"; }, {once:true});
      this.addEventListener("timeout", () => { entry.error="Timeout"; }, {once:true});
      return originalSend.apply(this, args);
    };
  } catch (_) {}

  // Where the host cannot hold native dialogs (every platform but Windows WebView2), intercept
  // them in-page so a stray alert()/confirm()/prompt() can never freeze the WebView event loop.
  // On Windows the native dialog is held open by the host instead and answered by
  // preview_dialog, so the page must reach the real window.alert/confirm/prompt.
  const NATIVE_DIALOGS = "__MEWORK_NATIVE_DIALOGS__" === "true";
  if (NATIVE_DIALOGS) try {
    // Tauri's dialog plugin replaces window.alert/confirm in every WebView it creates with
    // invoke()-backed versions, which this permissionless page cannot use, so a page calling
    // confirm() would get a rejected Promise instead of a dialog; there is no realm left in which
    // the originals survive (initialization scripts reach every frame on Windows). window.prompt
    // is untouched and blocks the page exactly like the others, so alert/confirm are routed
    // through it with the real kind encoded in the default text; the host reads that marker
    // from the held dialog, and since every dialog is held rather than shown, nobody ever sees a
    // prompt where a confirm was asked.
    const nativePrompt = window.prompt;
    const MARK = "⁣mework-dialog:";
    Object.defineProperty(window, "alert", {configurable: true, writable: true, value: function(message) {
      nativePrompt.call(window, String(message ?? ""), MARK + "alert");
    }});
    Object.defineProperty(window, "confirm", {configurable: true, writable: true, value: function(message) {
      return nativePrompt.call(window, String(message ?? ""), MARK + "confirm") !== null;
    }});
  } catch (_) {}
  if (!NATIVE_DIALOGS) try {
    const recordDialog = (type, message, result) => cap(dialogState.records, {timestamp: Date.now(), type, message: String(message ?? "").slice(0, 4000), result}, 50);
    window.alert = message => { recordDialog("alert", message, true); };
    window.confirm = message => {
      const accept = dialogState.armed ? dialogState.accept === true : false;
      recordDialog("confirm", message, accept);
      dialogState.armed = false; dialogState.accept = null; dialogState.promptText = null;
      return accept;
    };
    window.prompt = (message, fallback) => {
      let result = null;
      if (dialogState.armed) {
        result = dialogState.accept === false ? null : (dialogState.promptText ?? (fallback ?? ""));
      }
      recordDialog("prompt", message, result);
      dialogState.armed = false; dialogState.accept = null; dialogState.promptText = null;
      return result;
    };
  } catch (_) {}
})();
"#;

/// The initialization script with its platform switches filled in. Native dialog holding exists
/// only on Windows WebView2; elsewhere the in-page interception stays active.
fn browser_initialization_script() -> String {
    BROWSER_INITIALIZATION_SCRIPT.replace(
        "\"__MEWORK_NATIVE_DIALOGS__\" === \"true\"",
        if host_platform().is_windows() { "true" } else { "false" },
    )
}

// ----- Claude Code preview primitives ----------------------------------------------------------
//
// The page half of the `preview_*` tool surface, ported from the Claude Code desktop app's
// `CDPTools` class and its tool dispatcher. Everything the model reads — snapshot lines, console
// and network listings, resize confirmations, error sentences — is that app's text verbatim.
//
// These are bare page primitives, like `snapshot`/`click_tool`/`console` above: the caller supplies
// the envelope (`automation` lock, agent control, page preparation, modal-state gate, post-action
// settle) exactly as `execute_tool_blocking` does. Every CDP call goes through `cdp_call`, so a
// dialog opening mid-call ends the wait with `MODAL_STATE_INTERRUPTED` rather than hanging on it.

/// Whole-snapshot character cap, and the per-name / per-value cap inside one line.
const PREVIEW_SNAPSHOT_MAX_CHARS: usize = 12_000;
const PREVIEW_SNAPSHOT_TEXT_CHARS: usize = 200;
/// Below this depth a subtree collapses into a `... (N descendants)` tail.
const PREVIEW_SNAPSHOT_MAX_DEPTH: usize = 8;
/// Guards against a cyclic or pathologically deep `childIds` graph; the upstream builder has no
/// such bound and would recurse until the stack ran out.
const PREVIEW_AX_MAX_TREE_DEPTH: usize = 500;
const PREVIEW_EMPTY_SNAPSHOT: &str = "No accessible content found.";
/// Element lines one `preview_element_lines` read hands to the decision-model tools. The
/// whole-snapshot character cap does not apply there — a scored line is addressed by its uid,
/// not by its position in a text — so the bound is on lines, and the caller reports when it bit.
pub(crate) const PREVIEW_ELEMENT_LINES_CAP: usize = 2_000;
const PREVIEW_MAX_NETWORK_ENTRIES: usize = 500;
const PREVIEW_MAX_LOG_TEXT_CHARS: usize = 8_000;
const PREVIEW_MAX_METHOD_CHARS: usize = 64;
const PREVIEW_DEFAULT_LOG_LINES: u64 = 50;
const PREVIEW_RESPONSE_BODY_CHARS: usize = 10_000;
const PREVIEW_SCREENSHOT_MAX_WIDTH: f64 = 800.0;
const PREVIEW_SCREENSHOT_QUALITY: u32 = 75;
const PREVIEW_SCALE_MIN: f64 = 0.1;
const PREVIEW_VIEWPORT_MAX: u32 = 9_999;
const PREVIEW_MOBILE_MAX_WIDTH: u32 = 768;
const PREVIEW_MOBILE_TOUCH_POINTS: u32 = 5;

/// Roles that make a subtree worth keeping even when the wrapper above it carries no name.
const PREVIEW_AX_INTERESTING_ROLES: [&str; 10] = [
    "button", "link", "textbox", "checkbox", "radio", "combobox", "menuitem", "tab", "heading",
    "img",
];
/// Roles that contribute nothing of their own and are flattened away.
const PREVIEW_AX_GENERIC_ROLES: [&str; 3] = ["none", "presentation", "generic"];

/// What `preview_inspect` reports when the call names no properties.
const PREVIEW_DEFAULT_INSPECT_STYLES: [&str; 10] = [
    "color",
    "background-color",
    "font-size",
    "font-weight",
    "padding",
    "margin",
    "width",
    "height",
    "display",
    "visibility",
];

const PREVIEW_CLICK_RECT_SCRIPT: &str = r#"
        (function() {
          const el = document.querySelector(__MEWORK_SELECTOR__);
          if (!el) return null;
          el.scrollIntoView({ block: 'center', behavior: 'instant' });
          const r = el.getBoundingClientRect();
          return { x: r.x + r.width / 2, y: r.y + r.height / 2 };
        })()
      "#;

const PREVIEW_FILL_SCRIPT: &str = r#"
        (function() {
          const el = document.querySelector(__MEWORK_SELECTOR__);
          if (!el) return { success: false, error: 'Element not found' };

          // Focus the element
          el.focus();

          // Handle different input types
          const tagName = el.tagName.toLowerCase();

          if (tagName === 'select') {
            // For select elements, find and select the option
            const option = Array.from(el.options).find(o =>
              o.value === __MEWORK_VALUE__ || o.text === __MEWORK_VALUE__
            );
            if (option) {
              el.value = option.value;
            } else {
              return { success: false, error: 'Option not found' };
            }
          } else if (tagName === 'input' || tagName === 'textarea') {
            // Use the native setter to trigger React's synthetic event system.
            // Setting .value directly bypasses React's controlled component handling.
            const proto = tagName === 'textarea'
              ? window.HTMLTextAreaElement.prototype
              : window.HTMLInputElement.prototype;
            const nativeSetter = Object.getOwnPropertyDescriptor(proto, 'value')?.set;
            if (nativeSetter) {
              nativeSetter.call(el, __MEWORK_VALUE__);
            } else {
              el.value = __MEWORK_VALUE__;
            }
          } else if (el.isContentEditable) {
            // For contenteditable elements
            el.textContent = __MEWORK_VALUE__;
          } else {
            return { success: false, error: 'Element is not fillable' };
          }

          // Dispatch events to trigger any listeners
          el.dispatchEvent(new Event('input', { bubbles: true }));
          el.dispatchEvent(new Event('change', { bubbles: true }));

          return { success: true };
        })()
      "#;

const PREVIEW_INNER_TEXT_SCRIPT: &str =
    r#"document.querySelector(__MEWORK_SELECTOR__)?.innerText?.substring(0, 500) || """#;

/// `Runtime.callFunctionOn` body that turns the element it is called on into a CSS selector.
///
/// It takes no arguments and reads no page state beyond `this` and `document`: an `#id` when the
/// document holds exactly one element with it, otherwise `tag:nth-of-type(n)` segments up to
/// `html`. The path is returned only after the page itself agrees it selects this very node, so a
/// node in a shadow root or one detached from the document answers `null` rather than a selector
/// that would silently act on something else.
const PREVIEW_UNIQUE_SELECTOR_FUNCTION: &str = r#"function () {
  const node = this;
  if (!node || node.nodeType !== 1 || !node.ownerDocument) return null;
  const doc = node.ownerDocument;
  const selects = path => {
    try { return doc.querySelector(path) === node ? path : null; } catch (_) { return null; }
  };
  if (node.id && doc.querySelectorAll('#' + CSS.escape(node.id)).length === 1) {
    const byId = selects('#' + CSS.escape(node.id));
    if (byId) return byId;
  }
  const segments = [];
  let current = node;
  while (current && current.nodeType === 1) {
    const tag = current.localName;
    if (!tag) return null;
    if (tag === 'html') { segments.unshift('html'); break; }
    const parent = current.parentElement;
    if (!parent) { segments.unshift(tag); break; }
    let index = 0;
    for (const sibling of parent.children) {
      if (sibling.localName === tag) {
        index += 1;
        if (sibling === current) break;
      }
    }
    segments.unshift(tag + ':nth-of-type(' + index + ')');
    current = parent;
  }
  return segments.length ? selects(segments.join(' > ')) : null;
}"#;

const PREVIEW_REACT_FIBER_SCRIPT: &str = r#"(function() {
            var el = document.querySelector(__MEWORK_SELECTOR__);
            if (!el) return null;
            var key = Object.keys(el).find(function(k) {
              return k.startsWith('__reactFiber$') || k.startsWith('__reactInternalInstance$');
            });
            if (!key) return null;
            var fiber = el[key];
            while (fiber && typeof fiber.type === 'string') fiber = fiber.return;
            if (!fiber || !fiber.type) return null;
            var name = fiber.type.displayName || fiber.type.name || null;
            if (!name) return null;
            var props = {};
            try {
              var mp = fiber.memoizedProps || {};
              Object.keys(mp).forEach(function(k) {
                if (k === 'children') return;
                var v = mp[k];
                var t = typeof v;
                if (t === 'string' || t === 'number' || t === 'boolean' || v === null) {
                  props[k] = v;
                } else if (t === 'function') {
                  props[k] = '[function]';
                } else if (Array.isArray(v)) {
                  props[k] = '[array(' + v.length + ')]';
                } else if (t === 'object') {
                  props[k] = '[object]';
                }
              });
            } catch(e) {}
            return { name: name, props: props };
          })()"#;

/// `this`-bound fork of [`PREVIEW_REACT_FIBER_SCRIPT`]. A pick is identified by `backendNodeId`,
/// not by a selector, so the picker's scripts run through `Runtime.callFunctionOn`. This one also
/// collects the named ancestors and `_debugSource` that produce the chip label and `<source>` line.
const PICKER_REACT_FIBER_FUNCTION: &str = r#"function() {
  var el = this;
  var key = Object.keys(el).find(function(k) {
    return k.startsWith('__reactFiber$') || k.startsWith('__reactInternalInstance$');
  });
  if (!key) return null;
  var fiber = el[key];
  while (fiber && typeof fiber.type === 'string') fiber = fiber.return;
  if (!fiber || !fiber.type) return null;
  var name = fiber.type.displayName || fiber.type.name || null;
  if (!name) return null;
  var ancestors = [];
  var walker = fiber.return;
  while (walker && ancestors.length < 4) {
    if (walker.type && typeof walker.type !== 'string') {
      var label = walker.type.displayName || walker.type.name || null;
      if (label && label.length > 1 && label !== 'Anonymous') ancestors.push(label);
    }
    walker = walker.return;
  }
  var source = null;
  try {
    var debugSource = fiber._debugSource;
    if (debugSource && debugSource.fileName) {
      source = debugSource.fileName + (debugSource.lineNumber ? ':' + debugSource.lineNumber : '');
    }
  } catch (e) {}
  var props = {};
  try {
    var mp = fiber.memoizedProps || {};
    Object.keys(mp).forEach(function(k) {
      if (k === 'children') return;
      var v = mp[k];
      var t = typeof v;
      if (t === 'string' || t === 'number' || t === 'boolean' || v === null) {
        props[k] = v;
      } else if (t === 'function') {
        props[k] = '[function]';
      } else if (Array.isArray(v)) {
        props[k] = '[array(' + v.length + ')]';
      } else if (t === 'object') {
        props[k] = '[object]';
      }
    });
  } catch (e) {}
  return { name: name, ancestors: ancestors, source: source, props: props };
}"#;

/// The element's text plus its rect in **page** coordinates.
///
/// `visualViewport.pageLeft`/`pageTop` is what turns the viewport-relative
/// `getBoundingClientRect()` into the absolute document space `Page.captureScreenshot`'s `clip`
/// expects. Asking the page itself sidesteps `DOM.getBoxModel`'s coordinate-space ambiguity, and it
/// is the opposite of subtracting the scroll offset, which is what a window-coordinate capture API
/// would need.
const PICKER_TEXT_AND_RECT_FUNCTION: &str = r#"function() {
  var r = this.getBoundingClientRect();
  var v = window.visualViewport;
  return {
    text: (this.innerText || '').substring(0, 200),
    x: r.x + (v ? v.pageLeft : window.scrollX),
    y: r.y + (v ? v.pageTop : window.scrollY),
    width: r.width,
    height: r.height
  };
}"#;

const PICKER_PARENT_PATH_FUNCTION: &str = r#"function() {
  var parts = [];
  var node = this.parentElement;
  var hops = 0;
  while (node && hops < 4 && node !== document.body) {
    var tag = (node.tagName || '').toLowerCase();
    var label = tag;
    if (node.id) {
      label = tag + '#' + node.id;
    } else if (node.classList && node.classList.length > 0) {
      label = tag + '.' + node.classList[0];
    }
    parts.unshift(label);
    node = node.parentElement;
    hops++;
  }
  return parts.join(' > ');
}"#;

const PICKER_HTML_FUNCTION: &str = r#"function() {
  var self = this;
  var siblings = '';
  try {
    var parent = self.parentElement;
    if (parent) {
      siblings = Array.prototype.map.call(parent.children, function(child) {
        if (child === self) return '<!-- SELECTED -->' + (child.outerHTML || '');
        var tag = (child.tagName || '').toLowerCase();
        var id = child.id ? ' id="' + child.id + '"' : '';
        var cls = '';
        if (child.classList && child.classList.length > 0) {
          cls = ' class="' + Array.prototype.slice.call(child.classList, 0, 3).join(' ') + '"';
        }
        return '<' + tag + id + cls + ' />';
      }).join('\n');
    }
  } catch (e) {}
  return { outerHTML: (self.outerHTML || '').substring(0, 4000), siblingHTML: siblings.substring(0, 4000) };
}"#;

fn picker_tag_name(node: &Value) -> String {
    truncate(
        &node
            .get("nodeName")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_lowercase(),
        PICKER_MAX_TAG_CHARS,
    )
}

fn picker_inner_text(text_and_rect: &Value) -> Option<String> {
    text_and_rect
        .get("text")
        .and_then(Value::as_str)
        .map(|text| truncate(text, PICKER_MAX_INNER_TEXT_CHARS))
        .filter(|text| !text.is_empty())
}

fn picker_parent_path(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(|path| truncate(path, PICKER_MAX_PARENT_PATH_CHARS))
        .filter(|path| !path.is_empty())
}

fn picker_source_file(react: &Value) -> Option<String> {
    react
        .get("source")
        .and_then(Value::as_str)
        .map(|source| truncate(source, PICKER_MAX_SOURCE_FILE_CHARS))
        .filter(|source| !source.is_empty())
}

/// Splits `DOM.describeNode`'s flat `[name, value, name, value, …]` array into the element's id,
/// its classes, and the allowlisted attributes. Everything a page could name freely is dropped.
fn picker_attributes(
    attributes: Option<&Value>,
) -> (Option<String>, Vec<String>, BTreeMap<String, String>) {
    let mut id = None;
    let mut classes = Vec::new();
    let mut allowed = BTreeMap::new();
    let pairs = attributes
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for pair in pairs.chunks(2) {
        let (Some(name), Some(text)) = (
            pair.first().and_then(Value::as_str),
            pair.get(1).and_then(Value::as_str),
        ) else {
            continue;
        };
        match name {
            "id" => id = Some(truncate(text, PICKER_MAX_ID_CHARS)).filter(|id| !id.is_empty()),
            "class" => {
                classes = text
                    .split_whitespace()
                    .take(PICKER_MAX_CLASSES)
                    .map(|class| truncate(class, PICKER_MAX_CLASS_CHARS))
                    .collect();
            }
            _ => {
                if PICKER_ATTRIBUTE_ALLOWLIST.contains(&name) {
                    allowed.insert(name.to_owned(), truncate(text, PICKER_MAX_ATTRIBUTE_CHARS));
                }
            }
        }
    }
    (id, classes, allowed)
}

fn picker_computed_styles(computed_style: Option<&Value>) -> BTreeMap<String, String> {
    let declarations = computed_style
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut resolved = BTreeMap::new();
    for property in PICKER_STYLE_PROPS {
        if resolved.len() >= PICKER_MAX_STYLE_PROPS {
            break;
        }
        let found = declarations
            .iter()
            .find(|declaration| declaration.get("name").and_then(Value::as_str) == Some(property));
        if let Some(text) = found.and_then(|declaration| declaration.get("value")) {
            let text = text.as_str().unwrap_or_default();
            if text.is_empty() {
                continue;
            }
            resolved.insert(
                property.to_owned(),
                truncate(text, PICKER_MAX_STYLE_VALUE_CHARS),
            );
        }
    }
    resolved
}

/// Page CSS pixels. A rect the page could not report becomes the degenerate box, which the crop
/// answers with a plain viewport capture.
fn picker_bounding_box(text_and_rect: &Value) -> SelectedElementBox {
    let number = |field: &str| {
        text_and_rect
            .get(field)
            .and_then(Value::as_f64)
            .filter(|value| value.is_finite())
            .unwrap_or(0.0)
    };
    SelectedElementBox {
        x: number("x"),
        y: number("y"),
        width: number("width"),
        height: number("height"),
    }
}

fn picker_html_field(html: &Value, field: &str) -> Option<String> {
    html.get(field)
        .and_then(Value::as_str)
        .map(|text| truncate(text, PICKER_MAX_HTML_CHARS))
        .filter(|text| !text.is_empty())
}

/// `Name` on its own, or `Name (in Parent > Grandparent)` when the fiber walk found ancestors.
fn picker_react_component(react: &Value) -> Option<String> {
    let name = react.get("name").and_then(Value::as_str)?;
    if name.is_empty() {
        return None;
    }
    let ancestors: Vec<&str> = react
        .get("ancestors")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(Value::as_str)
                .filter(|label| !label.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let component = if ancestors.is_empty() {
        name.to_owned()
    } else {
        format!("{name} (in {})", ancestors.join(" > "))
    };
    Some(truncate(&component, PICKER_MAX_REACT_COMPONENT_CHARS))
}

fn picker_react_props(props: Option<&Value>) -> Option<Map<String, Value>> {
    let props = props?.as_object()?;
    let mut capped = Map::new();
    for (name, value) in props {
        if capped.len() >= PICKER_MAX_REACT_PROPS {
            break;
        }
        let value = match value {
            Value::String(text) => Value::String(truncate(text, PICKER_MAX_REACT_PROP_CHARS)),
            Value::Number(_) | Value::Bool(_) | Value::Null => value.clone(),
            // The page script only ever emits scalars; anything else is a page that replaced it.
            _ => continue,
        };
        capped.insert(truncate(name, PICKER_MAX_REACT_PROP_CHARS), value);
    }
    (!capped.is_empty()).then_some(capped)
}

/// The `Page.captureScreenshot` clip for one element, in absolute page coordinates.
///
/// CDP resolves the scroll offset itself, so there is no scroll term here; `scale` both caps the
/// longest edge at [`PICKER_SCREENSHOT_MAX_DIMENSION`] and spares the host a resampler it does not
/// have. `None` is a degenerate box, which the caller answers with a viewport capture.
fn picker_clip_rect(bounding_box: &SelectedElementBox) -> Option<Value> {
    if !(bounding_box.width > 0.0 && bounding_box.height > 0.0) {
        return None;
    }
    if !bounding_box.x.is_finite() || !bounding_box.y.is_finite() {
        return None;
    }
    let x = (bounding_box.x - PICKER_SCREENSHOT_PADDING).max(0.0);
    let y = (bounding_box.y - PICKER_SCREENSHOT_PADDING).max(0.0);
    let width = bounding_box.width + PICKER_SCREENSHOT_PADDING * 2.0;
    let height = bounding_box.height + PICKER_SCREENSHOT_PADDING * 2.0;
    let scale = (PICKER_SCREENSHOT_MAX_DIMENSION / width.max(height)).min(1.0);
    Some(json!({
        "x": x,
        "y": y,
        "width": width,
        "height": height,
        "scale": scale,
    }))
}

fn picker_screenshot_base64(bytes: &[u8]) -> Option<String> {
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    (encoded.len() <= PICKER_MAX_SCREENSHOT_BASE64).then_some(encoded)
}

#[cfg(test)]
mod element_picker_tests {
    use super::*;

    fn attribute_pairs(pairs: &[(&str, &str)]) -> Value {
        Value::Array(
            pairs
                .iter()
                .flat_map(|(name, value)| [json!(name), json!(value)])
                .collect(),
        )
    }

    fn clip_number(clip: &Value, field: &str) -> f64 {
        clip.get(field)
            .and_then(Value::as_f64)
            .unwrap_or_else(|| panic!("clip is missing {field}"))
    }

    #[test]
    fn attributes_keep_only_the_allowlist() {
        let (id, classes, attributes) = picker_attributes(Some(&attribute_pairs(&[
            ("id", "save"),
            ("class", "btn btn-primary"),
            ("aria-label", "Save"),
            ("data-testid", "save-button"),
            ("href", "https://example.test/"),
            ("onclick", "doThing()"),
            ("data-secret-token", "leak-me"),
            ("style", "position:absolute"),
            ("srcset", "leak-me-too"),
        ])));
        assert_eq!(id.as_deref(), Some("save"));
        assert_eq!(classes, vec!["btn".to_owned(), "btn-primary".to_owned()]);
        let kept: Vec<&str> = attributes.keys().map(String::as_str).collect();
        assert_eq!(
            kept,
            vec!["aria-label", "data-testid", "href", "onclick"],
            "only allowlisted attributes survive"
        );
        assert!(!attributes.contains_key("data-secret-token"));
        assert!(!attributes.contains_key("style"));
        // `srcset` starts with an allowlisted name but is not one of them.
        assert!(!attributes.contains_key("srcset"));
    }

    #[test]
    fn attributes_cap_class_count_and_class_length() {
        let many = (0..40)
            .map(|index| format!("c{index}"))
            .collect::<Vec<_>>()
            .join(" ");
        let (_, classes, _) = picker_attributes(Some(&attribute_pairs(&[("class", &many)])));
        assert_eq!(classes.len(), PICKER_MAX_CLASSES);
        assert_eq!(classes[0], "c0");

        let long = "x".repeat(400);
        let (_, classes, _) = picker_attributes(Some(&attribute_pairs(&[("class", &long)])));
        assert_eq!(classes[0].chars().count(), PICKER_MAX_CLASS_CHARS);
    }

    #[test]
    fn attribute_values_and_id_are_capped() {
        let long = "y".repeat(4_000);
        let (id, _, attributes) =
            picker_attributes(Some(&attribute_pairs(&[("id", &long), ("title", &long)])));
        assert_eq!(
            id.expect("id survives").chars().count(),
            PICKER_MAX_ID_CHARS
        );
        assert_eq!(
            attributes["title"].chars().count(),
            PICKER_MAX_ATTRIBUTE_CHARS
        );
    }

    #[test]
    fn tag_name_is_lowercased_and_capped() {
        assert_eq!(picker_tag_name(&json!({"nodeName": "BUTTON"})), "button");
        let long = "A".repeat(200);
        assert_eq!(
            picker_tag_name(&json!({"nodeName": long})).chars().count(),
            PICKER_MAX_TAG_CHARS
        );
        assert_eq!(picker_tag_name(&json!({})), "");
    }

    #[test]
    fn computed_styles_resolve_kebab_case_names() {
        let computed = json!([
            {"name": "display", "value": "inline-flex"},
            {"name": "flex-direction", "value": "row"},
            {"name": "background-color", "value": "rgb(0, 0, 0)"},
            {"name": "font-size", "value": "14px"},
            {"name": "flexDirection", "value": "must-not-resolve"},
            {"name": "z-index", "value": "9999"},
        ]);
        let styles = picker_computed_styles(Some(&computed));
        // The camelCase spelling upstream uses resolves nothing; the kebab-case one does.
        assert_eq!(
            styles.get("flex-direction").map(String::as_str),
            Some("row")
        );
        assert_eq!(
            styles.get("background-color").map(String::as_str),
            Some("rgb(0, 0, 0)")
        );
        assert_eq!(styles.get("font-size").map(String::as_str), Some("14px"));
        assert!(
            !styles.contains_key("z-index"),
            "unlisted props are dropped"
        );
        assert!(styles.len() <= PICKER_MAX_STYLE_PROPS);
        assert!(styles.len() <= PICKER_STYLE_PROPS.len());
    }

    #[test]
    fn computed_style_values_are_capped() {
        let computed = json!([{"name": "border", "value": "z".repeat(4_000)}]);
        let styles = picker_computed_styles(Some(&computed));
        assert_eq!(
            styles["border"].chars().count(),
            PICKER_MAX_STYLE_VALUE_CHARS
        );
    }

    #[test]
    fn html_and_inner_text_are_truncated() {
        let html = json!({
            "outerHTML": "<b>".repeat(2_000),
            "siblingHTML": "<i>".repeat(2_000),
        });
        assert_eq!(
            picker_html_field(&html, "outerHTML")
                .expect("outerHTML")
                .chars()
                .count(),
            PICKER_MAX_HTML_CHARS
        );
        assert_eq!(
            picker_html_field(&html, "siblingHTML")
                .expect("siblingHTML")
                .chars()
                .count(),
            PICKER_MAX_HTML_CHARS
        );
        assert_eq!(
            picker_html_field(&json!({"outerHTML": ""}), "outerHTML"),
            None
        );

        let text = json!({"text": "\u{6c49}".repeat(500)});
        assert_eq!(
            picker_inner_text(&text).expect("text").chars().count(),
            PICKER_MAX_INNER_TEXT_CHARS,
            "the picker's own 200-char cap, not the 500 preview_inspect uses"
        );
        assert_eq!(picker_inner_text(&json!({"text": ""})), None);
    }

    #[test]
    fn parent_path_and_source_file_are_capped() {
        let long = json!("p > ".repeat(400));
        assert_eq!(
            picker_parent_path(Some(&long))
                .expect("path")
                .chars()
                .count(),
            PICKER_MAX_PARENT_PATH_CHARS
        );
        assert_eq!(picker_parent_path(None), None);
        assert_eq!(picker_parent_path(Some(&json!(""))), None);

        let react = json!({"source": "/".repeat(4_000)});
        assert_eq!(
            picker_source_file(&react).expect("source").chars().count(),
            PICKER_MAX_SOURCE_FILE_CHARS
        );
        assert_eq!(picker_source_file(&json!({})), None);
    }

    #[test]
    fn react_component_folds_in_its_ancestors() {
        assert_eq!(
            picker_react_component(&json!({"name": "SubmitButton", "ancestors": []})).as_deref(),
            Some("SubmitButton")
        );
        assert_eq!(
            picker_react_component(
                &json!({"name": "SubmitButton", "ancestors": ["SettingsForm", "SettingsPage"]})
            )
            .as_deref(),
            Some("SubmitButton (in SettingsForm > SettingsPage)")
        );
        assert_eq!(picker_react_component(&Value::Null), None);
    }

    #[test]
    fn react_props_are_capped_and_scalar_only() {
        let mut props = Map::new();
        for index in 0..80 {
            props.insert(format!("p{index:03}"), json!(index));
        }
        props.insert("long".to_owned(), json!("v".repeat(4_000)));
        props.insert("nested".to_owned(), json!({"still": "an object"}));
        let capped = picker_react_props(Some(&Value::Object(props))).expect("props");
        assert_eq!(capped.len(), PICKER_MAX_REACT_PROPS);
        assert!(capped.values().all(|value| !value.is_object()));
        if let Some(Value::String(long)) = capped.get("long") {
            assert_eq!(long.chars().count(), PICKER_MAX_REACT_PROP_CHARS);
        }
        assert_eq!(picker_react_props(Some(&json!({}))), None);
    }

    #[test]
    fn clip_pads_the_page_rect_by_eighty_pixels() {
        let clip = picker_clip_rect(&SelectedElementBox {
            x: 300.0,
            y: 200.0,
            width: 120.0,
            height: 40.0,
        })
        .expect("a real box clips");
        assert_eq!(clip_number(&clip, "x"), 220.0);
        assert_eq!(clip_number(&clip, "y"), 120.0);
        assert_eq!(clip_number(&clip, "width"), 280.0);
        assert_eq!(clip_number(&clip, "height"), 200.0);
        assert_eq!(clip_number(&clip, "scale"), 1.0);
    }

    #[test]
    fn clip_of_a_scrolled_element_stays_in_page_coordinates() {
        // The rect already comes from the page as `getBoundingClientRect()` plus
        // `visualViewport.pageLeft/pageTop`, and CDP resolves the scroll itself. Subtracting the
        // scroll offset here — which a window-coordinate capture API would need — would move the
        // crop up the page by exactly the distance the user scrolled.
        let clip = picker_clip_rect(&SelectedElementBox {
            x: 40.0,
            y: 4_800.0,
            width: 200.0,
            height: 60.0,
        })
        .expect("a scrolled box clips");
        assert_eq!(clip_number(&clip, "y"), 4_720.0);
        // Padding never pushes the clip off the left edge of the document.
        assert_eq!(clip_number(&clip, "x"), 0.0);
        assert_eq!(clip_number(&clip, "width"), 360.0);
    }

    #[test]
    fn clip_scales_a_large_element_under_the_dimension_cap() {
        let clip = picker_clip_rect(&SelectedElementBox {
            x: 0.0,
            y: 0.0,
            width: 2_000.0,
            height: 400.0,
        })
        .expect("a large box clips");
        let scale = clip_number(&clip, "scale");
        let longest = clip_number(&clip, "width").max(clip_number(&clip, "height"));
        assert!((longest * scale - PICKER_SCREENSHOT_MAX_DIMENSION).abs() < 0.001);
        assert!(scale < 1.0);
    }

    #[test]
    fn a_degenerate_box_has_no_clip() {
        for box_ in [
            SelectedElementBox {
                x: 10.0,
                y: 10.0,
                width: 0.0,
                height: 20.0,
            },
            SelectedElementBox {
                x: 10.0,
                y: 10.0,
                width: 20.0,
                height: 0.0,
            },
            SelectedElementBox {
                x: 10.0,
                y: 10.0,
                width: -5.0,
                height: 20.0,
            },
            SelectedElementBox {
                x: f64::NAN,
                y: 10.0,
                width: 20.0,
                height: 20.0,
            },
            SelectedElementBox {
                x: 10.0,
                y: f64::INFINITY,
                width: 20.0,
                height: 20.0,
            },
        ] {
            assert!(
                picker_clip_rect(&box_).is_none(),
                "a degenerate box falls back to a viewport capture"
            );
        }
    }

    #[test]
    fn an_oversize_screenshot_is_refused() {
        // base64 is 4 bytes per 3 input bytes, so the cap lands exactly at 1.5 MiB of PNG.
        let at_cap = vec![0u8; 1_572_864];
        assert_eq!(
            picker_screenshot_base64(&at_cap)
                .expect("a capture at the cap is kept")
                .len(),
            PICKER_MAX_SCREENSHOT_BASE64
        );
        let over_cap = vec![0u8; 1_572_867];
        assert_eq!(picker_screenshot_base64(&over_cap), None);
    }

    #[test]
    fn the_sequence_counter_is_monotonic() {
        let mut picker = ElementPickerState::default();
        assert_eq!(picker.next_sequence(), 1);
        assert_eq!(picker.next_sequence(), 2);
        picker.disarm();
        assert_eq!(
            picker.next_sequence(),
            3,
            "disarming does not rewind the sequence a replay is recognized by"
        );
    }

    #[test]
    fn a_pick_from_a_previous_page_generation_is_dropped() {
        let mut picker = ElementPickerState::default();
        picker.arm(7);
        assert!(picker.note_inspect_node(7, 42));
        assert_eq!(
            picker.take_pending(8),
            None,
            "the page navigated out from under the pick"
        );
        assert_eq!(
            picker.take_pending(7),
            None,
            "and the stale pick is consumed, not left to be drained later"
        );
    }

    #[test]
    fn a_second_click_is_latched_out_until_the_pick_is_drained() {
        let mut picker = ElementPickerState::default();
        picker.arm(3);
        assert!(picker.note_inspect_node(3, 11));
        assert!(
            !picker.note_inspect_node(3, 22),
            "a capture is already queued"
        );
        assert_eq!(picker.take_pending(3), Some(11));
        assert_eq!(picker.take_pending(3), None);
    }

    #[test]
    fn a_pick_is_ignored_while_disarmed() {
        let mut picker = ElementPickerState::default();
        assert!(!picker.note_inspect_node(1, 5));
        picker.arm(1);
        assert!(
            !picker.note_inspect_node(2, 5),
            "an arm belongs to exactly one page generation"
        );
        assert!(picker.note_inspect_node(1, 5));
    }

    #[test]
    fn the_arm_expires_with_its_page_generation() {
        let mut picker = ElementPickerState::default();
        picker.arm(4);
        assert!(picker.armed(4));
        assert!(
            !picker.armed(5),
            "a navigation leaves the page without inspect mode"
        );
        assert_eq!(picker.snapshot(5), None);
    }

    #[test]
    fn inspect_mode_canceled_disarms_but_keeps_a_pending_pick() {
        let mut picker = ElementPickerState::default();
        picker.arm(2);
        record_element_picker_event(
            &mut picker,
            2,
            "Overlay.inspectNodeRequested",
            &json!({"backendNodeId": 99}),
        );
        record_element_picker_event(&mut picker, 2, "Overlay.inspectModeCanceled", &json!({}));
        assert!(!picker.armed(2), "Escape inside the page ends the arm");
        assert_eq!(
            picker.take_pending(2),
            Some(99),
            "a click already observed is still worth delivering"
        );
    }

    #[test]
    fn the_event_handler_ignores_a_malformed_node_id() {
        let mut picker = ElementPickerState::default();
        picker.arm(1);
        record_element_picker_event(
            &mut picker,
            1,
            "Overlay.inspectNodeRequested",
            &json!({"backendNodeId": "not a number"}),
        );
        record_element_picker_event(&mut picker, 1, "Page.loadEventFired", &json!({}));
        assert_eq!(picker.take_pending(1), None);
        assert!(picker.armed(1));
    }

    #[test]
    fn the_status_snapshot_is_absent_while_idle() {
        let mut picker = ElementPickerState::default();
        assert_eq!(
            picker.snapshot(1),
            None,
            "an idle picker adds nothing to the poll"
        );
        picker.arm(1);
        assert_eq!(
            picker.snapshot(1),
            Some(BrowserElementPicker {
                armed: true,
                pending_pick: false
            })
        );
        picker.note_inspect_node(1, 8);
        assert_eq!(
            picker.snapshot(1),
            Some(BrowserElementPicker {
                armed: true,
                pending_pick: true
            })
        );
        picker.cancel();
        assert_eq!(
            picker.snapshot(1),
            Some(BrowserElementPicker {
                armed: false,
                pending_pick: true
            }),
            "a drained-but-unarmed picker still tells the pane to come and take it"
        );
    }

    #[test]
    fn the_status_field_is_omitted_from_an_idle_status() {
        let serialized = serde_json::to_value(BrowserStatus::default()).expect("status serializes");
        assert!(
            serialized.get("elementPicker").is_none(),
            "the poll payload is unchanged until the picker is used"
        );
        let armed = BrowserStatus {
            element_picker: Some(BrowserElementPicker {
                armed: true,
                pending_pick: false,
            }),
            ..BrowserStatus::default()
        };
        assert_eq!(
            serde_json::to_value(&armed).expect("status serializes")["elementPicker"],
            json!({"armed": true, "pendingPick": false})
        );
    }

    #[test]
    fn an_existing_status_fixture_still_deserializes() {
        let legacy = json!({
            "hasPage": true,
            "open": true,
            "url": "https://example.test/",
            "title": null,
            "loading": false,
            "canGoBack": false,
            "canGoForward": false,
            "zoom": 1.0,
            "viewport": {"width": 800, "height": 600},
            "error": null,
            "screenshotPath": null,
        });
        let status: BrowserStatus =
            serde_json::from_value(legacy).expect("a status without elementPicker still loads");
        assert_eq!(status.element_picker, None);
    }

    #[test]
    fn the_selected_element_payload_is_camel_case() {
        let element = SelectedElement {
            sequence: 3,
            tag_name: "button".to_owned(),
            id: Some("save".to_owned()),
            classes: vec!["btn".to_owned()],
            attributes: BTreeMap::from([("aria-label".to_owned(), "Save".to_owned())]),
            computed_styles: BTreeMap::from([("display".to_owned(), "block".to_owned())]),
            bounding_box: SelectedElementBox {
                x: 1.0,
                y: 2.0,
                width: 3.0,
                height: 4.0,
            },
            screenshot_base64: "AAAA".to_owned(),
            inner_text: Some("Save".to_owned()),
            parent_path: None,
            react_component: None,
            react_props: None,
            source_file: None,
            outer_html: None,
            sibling_html: None,
        };
        let serialized = serde_json::to_value(&element).expect("payload serializes");
        assert_eq!(
            serialized,
            json!({
                "sequence": 3,
                "tagName": "button",
                "id": "save",
                "classes": ["btn"],
                "attributes": {"aria-label": "Save"},
                "computedStyles": {"display": "block"},
                "boundingBox": {"x": 1.0, "y": 2.0, "width": 3.0, "height": 4.0},
                "screenshotBase64": "AAAA",
                "innerText": "Save",
                "parentPath": null,
                "reactComponent": null,
                "reactProps": null,
                "sourceFile": null,
                "outerHtml": null,
                "siblingHtml": null,
            })
        );
    }
}

/// One rebuilt accessibility node. `uid` is what `preview_click`/`preview_fill` calls refer back
/// to, and it is minted per page generation rather than per snapshot.
#[derive(Debug, Clone, Serialize)]
struct AxNode {
    uid: u64,
    role: String,
    name: String,
    value: Option<String>,
    description: Option<String>,
    /// The DOM node behind this accessibility node, when the tree names one. It is what
    /// `unique_selector_for` resolves into a CSS selector; a node without one (a synthesized
    /// accessibility node) cannot be addressed by selector at all.
    backend_node_id: Option<i64>,
    children: Vec<AxNode>,
}

/// One line of a rendered accessibility snapshot, before it becomes text.
///
/// `format_ax_snapshot` is a renderer over these, so a decision-model tool can score, choose and
/// address individual elements without re-parsing the text the model would have read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AxLine {
    pub(crate) indent: usize,
    pub(crate) uid: u64,
    pub(crate) role: String,
    pub(crate) name: String,
    /// Empty when the node carries no value; the renderer omits the `(value: …)` part then.
    pub(crate) value: String,
    pub(crate) backend_node_id: Option<i64>,
    /// `Some(n)` on a depth-capped line, which prints `... (n descendants)` instead of them.
    pub(crate) descendants_note: Option<usize>,
}

/// Base64 JPEG returned inline as an image content block, not written to a workspace path.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PreviewScreenshot {
    pub(crate) data: String,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

/// The page's CSS viewport plus the device-pixel ratio Chromium is actually compositing at.
#[derive(Debug, Clone, Copy)]
struct PreviewViewport {
    width: f64,
    height: f64,
    device_ratio: f64,
}

impl BrowserSession {
    // ----- accessibility snapshot ---------------------------------------------------------

    /// `preview_snapshot`: the accessibility tree as indented `[uid] role: "name"` lines.
    pub(crate) fn preview_snapshot(&self) -> Result<String, String> {
        // Unlike Network/Page, the Accessibility domain is not enabled when the page is created.
        // Enabling and releasing it around the read keeps Chromium from maintaining a full
        // accessibility tree for the rest of the page's life just because one snapshot was taken.
        let _ = self.cdp_call("Accessibility.enable", &json!({}), EVAL_TIMEOUT);
        let tree = self.cdp_call("Accessibility.getFullAXTree", &json!({}), EVAL_TIMEOUT);
        let _ = self.cdp_call("Accessibility.disable", &json!({}), EVAL_TIMEOUT);
        let response = tree?;
        let nodes = response
            .get("nodes")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut uid = self.lock_state().activity.snapshot_uid;
        let tree = build_ax_tree(&nodes, &mut uid);
        self.lock_state().activity.snapshot_uid = uid;
        let Some(tree) = tree else {
            return Ok(PREVIEW_EMPTY_SNAPSHOT.to_owned());
        };
        let text = truncate_preview_snapshot(&format_ax_snapshot(&tree, 0, 0));
        Ok(if text.is_empty() {
            PREVIEW_EMPTY_SNAPSHOT.to_owned()
        } else {
            text
        })
    }

    // ----- element inspection -------------------------------------------------------------

    /// The same accessibility read `preview_snapshot` performs, handed back as lines instead of
    /// text: the decision-model preview tools score, choose among and address these individually.
    ///
    /// The uid minting is shared with `preview_snapshot` — both advance the page generation's
    /// counter — so a uid the model saw in a snapshot never means a different element here.
    /// The second half of the answer is whether the [`PREVIEW_ELEMENT_LINES_CAP`] bit.
    pub(crate) fn preview_element_lines(&self) -> Result<(Vec<AxLine>, bool), String> {
        let _ = self.cdp_call("Accessibility.enable", &json!({}), EVAL_TIMEOUT);
        let tree = self.cdp_call("Accessibility.getFullAXTree", &json!({}), EVAL_TIMEOUT);
        let _ = self.cdp_call("Accessibility.disable", &json!({}), EVAL_TIMEOUT);
        let response = tree?;
        let nodes = response
            .get("nodes")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut uid = self.lock_state().activity.snapshot_uid;
        let tree = build_ax_tree(&nodes, &mut uid);
        self.lock_state().activity.snapshot_uid = uid;
        let Some(tree) = tree else {
            return Ok((Vec::new(), false));
        };
        let mut lines = Vec::new();
        ax_snapshot_lines(&tree, 0, 0, &mut lines);
        let truncated = lines.len() > PREVIEW_ELEMENT_LINES_CAP;
        lines.truncate(PREVIEW_ELEMENT_LINES_CAP);
        Ok((lines, truncated))
    }

    /// A CSS selector that addresses exactly the DOM node behind `backend_node_id`, or `None`
    /// when no selector can: the node was removed, it lives inside a shadow root, or the path
    /// this builds does not select it back.
    ///
    /// The selector is what the decision-model variants hand to `preview_click`/`preview_fill`/
    /// `preview_inspect`, so "the page agreed this path selects this node" is the whole contract.
    pub(crate) fn unique_selector_for(
        &self,
        backend_node_id: i64,
    ) -> Result<Option<String>, String> {
        self.cdp_call("DOM.enable", &json!({}), EVAL_TIMEOUT)?;
        let selector = self.resolve_unique_selector(backend_node_id);
        let _ = self.cdp_call("DOM.disable", &json!({}), EVAL_TIMEOUT);
        selector
    }

    fn resolve_unique_selector(&self, backend_node_id: i64) -> Result<Option<String>, String> {
        // A node the tree named a moment ago and the DOM no longer holds is not a transport
        // fault; it is the ordinary "the page moved on" the caller reports as unavailable.
        let Ok(resolved) = self.cdp_call(
            "DOM.resolveNode",
            &json!({ "backendNodeId": backend_node_id }),
            EVAL_TIMEOUT,
        ) else {
            return Ok(None);
        };
        let Some(object_id) = resolved
            .pointer("/object/objectId")
            .and_then(Value::as_str)
            .map(str::to_owned)
        else {
            return Ok(None);
        };
        let called = self.cdp_call(
            "Runtime.callFunctionOn",
            &json!({
                "objectId": object_id,
                "functionDeclaration": PREVIEW_UNIQUE_SELECTOR_FUNCTION,
                "returnByValue": true,
            }),
            EVAL_TIMEOUT,
        );
        // The handle is the page's, not ours: it must be released whether the call answered.
        let _ = self.cdp_call(
            "Runtime.releaseObject",
            &json!({ "objectId": object_id }),
            EVAL_TIMEOUT,
        );
        let called = called?;
        if called.get("exceptionDetails").is_some() {
            return Ok(None);
        }
        Ok(called
            .pointer("/result/value")
            .and_then(Value::as_str)
            .map(str::to_owned))
    }

    /// Every console entry as `[level] text`, for scoring. Unlike `preview_console_logs` there is
    /// no level filter, no tail slice and no footer: the decision model chunks the whole buffer
    /// and only the pieces that clear the threshold reach the conversation anyway.
    pub(crate) fn console_log_lines(&self) -> Result<Vec<String>, String> {
        let payload = self.eval_value(
            r#"
const render = value => typeof value === "string" ? value : (() => { try { return JSON.stringify(value); } catch (_) { return String(value); } })();
return __state.consoleEntries.map(entry => ({
  level: String(entry.level || "log"),
  text: Array.isArray(entry.args) ? entry.args.map(render).join(" ") : String(entry.message || "")
}));
"#,
            EVAL_TIMEOUT,
        )?;
        Ok(payload
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .map(|entry| {
                format!(
                    "[{}] {}",
                    entry.get("level").and_then(Value::as_str).unwrap_or("log"),
                    truncate_with_ellipsis(
                        entry
                            .get("text")
                            .and_then(Value::as_str)
                            .unwrap_or_default(),
                        PREVIEW_MAX_LOG_TEXT_CHARS
                    )
                )
            })
            .collect())
    }

    /// `preview_inspect`. `Ok(None)` is the source's "element not found", which the dispatcher
    /// renders as the ordinary (non-error) text `Element not found: {selector}`; it also covers the
    /// DOM/CSS lookups the source folds into that same answer.
    pub(crate) fn preview_inspect(
        &self,
        selector: &str,
        styles: Option<&[String]>,
    ) -> Result<Option<Value>, String> {
        let selector = validate_selector(selector)?;
        self.cdp_call("DOM.enable", &json!({}), EVAL_TIMEOUT)?;
        self.cdp_call("CSS.enable", &json!({}), EVAL_TIMEOUT)?;
        let inspected = self.inspect_element(&selector, styles);
        // Both domains are released on every exit path, exactly as upstream does.
        let _ = self.cdp_call("CSS.disable", &json!({}), EVAL_TIMEOUT);
        let _ = self.cdp_call("DOM.disable", &json!({}), EVAL_TIMEOUT);
        inspected
    }

    fn inspect_element(
        &self,
        selector: &str,
        styles: Option<&[String]>,
    ) -> Result<Option<Value>, String> {
        let literal = js_string_literal(selector)?;
        let Ok(document) = self.cdp_call("DOM.getDocument", &json!({}), EVAL_TIMEOUT) else {
            return Ok(None);
        };
        let Some(root) = document.pointer("/root/nodeId").and_then(Value::as_i64) else {
            return Ok(None);
        };
        let Ok(found) = self.cdp_call(
            "DOM.querySelector",
            &json!({"nodeId": root, "selector": selector}),
            EVAL_TIMEOUT,
        ) else {
            return Ok(None);
        };
        let node_id = found.get("nodeId").and_then(Value::as_i64).unwrap_or(0);
        if node_id == 0 {
            return Ok(None);
        }
        let Ok(described) = self.cdp_call(
            "DOM.describeNode",
            &json!({ "nodeId": node_id }),
            EVAL_TIMEOUT,
        ) else {
            return Ok(None);
        };
        let Some(node) = described.get("node") else {
            return Ok(None);
        };

        let mut class_name = String::new();
        let mut id = String::new();
        let mut value = String::new();
        let attributes = node
            .get("attributes")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for pair in attributes.chunks(2) {
            let (Some(name), Some(text)) = (
                pair.first().and_then(Value::as_str),
                pair.get(1).and_then(Value::as_str),
            ) else {
                continue;
            };
            match name {
                "class" => class_name = truncate(text, 200),
                "id" => id = text.to_owned(),
                "value" => value = text.to_owned(),
                _ => {}
            }
        }

        let requested: Vec<String> = match styles {
            Some(list) => list.to_vec(),
            None => PREVIEW_DEFAULT_INSPECT_STYLES
                .iter()
                .map(|name| (*name).to_owned())
                .collect(),
        };
        let Ok(computed) = self.cdp_call(
            "CSS.getComputedStyleForNode",
            &json!({ "nodeId": node_id }),
            EVAL_TIMEOUT,
        ) else {
            return Ok(None);
        };
        let declarations = computed
            .get("computedStyle")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut resolved = Map::new();
        for property in &requested {
            let found = declarations.iter().find(|declaration| {
                declaration.get("name").and_then(Value::as_str) == Some(property.as_str())
            });
            if let Some(text) = found.and_then(|declaration| declaration.get("value")) {
                resolved.insert(property.clone(), text.clone());
            }
        }

        let bounding_box = self
            .cdp_call(
                "DOM.getBoxModel",
                &json!({ "nodeId": node_id }),
                EVAL_TIMEOUT,
            )
            .ok()
            .and_then(|model| {
                let content = model.pointer("/model/content")?.as_array()?.clone();
                if content.len() < 6 {
                    return None;
                }
                let corner = |index: usize| content.get(index).and_then(Value::as_f64);
                Some(json!({
                    "x": corner(0)?,
                    "y": corner(1)?,
                    "width": corner(2)? - corner(0)?,
                    "height": corner(5)? - corner(1)?,
                }))
            });

        let text = self
            .preview_eval_value(&render_page_script(
                PREVIEW_INNER_TEXT_SCRIPT,
                &[("__MEWORK_SELECTOR__", &literal)],
            ))
            .ok()
            .flatten()
            .and_then(|value| value.as_str().map(str::to_owned))
            .unwrap_or_default();

        let react = self
            .preview_eval_value(&render_page_script(
                PREVIEW_REACT_FIBER_SCRIPT,
                &[("__MEWORK_SELECTOR__", &literal)],
            ))
            .ok()
            .flatten()
            .filter(|value| value.is_object());

        let mut result = Map::new();
        result.insert(
            "tagName".to_owned(),
            json!(node
                .get("nodeName")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_lowercase()),
        );
        result.insert("text".to_owned(), json!(truncate(&text, 500)));
        result.insert("className".to_owned(), json!(class_name));
        result.insert("id".to_owned(), json!(id));
        if !value.is_empty() {
            result.insert("value".to_owned(), json!(value));
        }
        result.insert("styles".to_owned(), Value::Object(resolved));
        if let Some(bounding_box) = bounding_box {
            result.insert("boundingBox".to_owned(), bounding_box);
        }
        if let Some(react) = react {
            if let Some(name) = react.get("name").and_then(Value::as_str) {
                result.insert("reactComponent".to_owned(), json!(name));
            }
            if let Some(props) = react
                .get("props")
                .and_then(Value::as_object)
                .filter(|props| !props.is_empty())
            {
                result.insert("reactProps".to_owned(), Value::Object(props.clone()));
            }
        }
        Ok(Some(Value::Object(result)))
    }

    // ----- element picker -------------------------------------------------------------------

    /// Arms or disarms Chrome's own inspector overlay for one user-driven pick.
    ///
    /// This is the whole CDP surface of arming: if WebView2 turns out not to serve the `Overlay`
    /// domain, replacing this one function with an injected in-page picker is the entire change on
    /// the arm side. The error is deliberately returned rather than swallowed, so an unavailable
    /// domain reaches the pane's error strip carrying WebView2's own message.
    pub(crate) fn arm_element_picker(&self, armed: bool) -> Result<(), String> {
        if !armed {
            self.disarm_element_picker();
            return Ok(());
        }
        {
            let mut state = self.lock_state();
            let generation = state.page_generation;
            state.element_picker.arm(generation);
        }
        let armed = (|| -> Result<(), String> {
            self.cdp_call("DOM.enable", &json!({}), EVAL_TIMEOUT)?;
            self.cdp_call("Overlay.enable", &json!({}), EVAL_TIMEOUT)?;
            self.cdp_call(
                "Overlay.setInspectMode",
                &json!({
                    "mode": "searchForNode",
                    "highlightConfig": {
                        "showInfo": true,
                        "contentColor": {"r": 111, "g": 168, "b": 220, "a": 0.66},
                        "paddingColor": {"r": 147, "g": 196, "b": 125, "a": 0.55},
                        "borderColor": {"r": 255, "g": 229, "b": 153, "a": 0.66},
                        "marginColor": {"r": 246, "g": 178, "b": 107, "a": 0.66},
                    },
                }),
                EVAL_TIMEOUT,
            )?;
            Ok(())
        })();
        if armed.is_err() {
            self.lock_state().element_picker.disarm();
        }
        armed
    }

    /// Leaves inspect mode and releases the domains, mirroring upstream's best-effort teardown.
    fn disarm_element_picker(&self) {
        self.lock_state().element_picker.disarm();
        let _ = self.cdp_call(
            "Overlay.setInspectMode",
            &json!({"mode": "none", "highlightConfig": {}}),
            EVAL_TIMEOUT,
        );
        let _ = self.cdp_call("Overlay.hideHighlight", &json!({}), EVAL_TIMEOUT);
        let _ = self.cdp_call("Overlay.disable", &json!({}), EVAL_TIMEOUT);
        let _ = self.cdp_call("DOM.disable", &json!({}), EVAL_TIMEOUT);
    }

    /// `browser_take_selected_element`. `Ok(None)` is "no click is waiting", which is what the
    /// poll sees almost every time it asks.
    ///
    /// Arming is single-shot on this side: the host disarms itself here, and the renderer only
    /// reflects `status.elementPicker.armed`. Re-arming in the host while the renderer disarms is
    /// a race with no upside — picking several elements is one more click either way.
    pub(crate) fn take_selected_element(&self) -> Result<Option<SelectedElement>, String> {
        let backend_node_id = {
            let mut state = self.lock_state();
            let generation = state.page_generation;
            match state.element_picker.take_pending(generation) {
                Some(backend_node_id) => backend_node_id,
                None => return Ok(None),
            }
        };
        // Before the capture, not after: `capture_png_clip` hides the agent pointer, which only
        // fires `Overlay.hideHighlight` and leaves inspect mode running. Captured while still
        // armed, the inspector's box and tag tooltip composite into the element's own thumbnail.
        self.disarm_element_picker();
        self.capture_element_context(backend_node_id).map(Some)
    }

    fn capture_element_context(&self, backend_node_id: i64) -> Result<SelectedElement, String> {
        self.cdp_call("DOM.enable", &json!({}), EVAL_TIMEOUT)?;
        self.cdp_call("CSS.enable", &json!({}), EVAL_TIMEOUT)?;
        let captured = self.capture_element_context_inner(backend_node_id);
        let _ = self.cdp_call("CSS.disable", &json!({}), EVAL_TIMEOUT);
        let _ = self.cdp_call("DOM.disable", &json!({}), EVAL_TIMEOUT);
        captured
    }

    fn capture_element_context_inner(
        &self,
        backend_node_id: i64,
    ) -> Result<SelectedElement, String> {
        // `CSS.getComputedStyleForNode` takes a frontend `nodeId`, and a node that was never
        // pushed to the frontend describes itself with `nodeId: 0`.
        self.cdp_call("DOM.getDocument", &json!({"depth": 0}), EVAL_TIMEOUT)?;
        let pushed = self.cdp_call(
            "DOM.pushNodesByBackendIdsToFrontend",
            &json!({"backendNodeIds": [backend_node_id]}),
            EVAL_TIMEOUT,
        )?;
        let node_id = pushed
            .pointer("/nodeIds/0")
            .and_then(Value::as_i64)
            .filter(|node_id| *node_id != 0);

        let described = self.cdp_call(
            "DOM.describeNode",
            &json!({"backendNodeId": backend_node_id, "depth": 0}),
            EVAL_TIMEOUT,
        )?;
        let node = described
            .get("node")
            .ok_or_else(|| "所选元素已不在页面中".to_owned())?;
        let tag_name = picker_tag_name(node);
        let (id, classes, attributes) = picker_attributes(node.get("attributes"));

        let computed_styles = node_id
            .and_then(|node_id| {
                self.cdp_call(
                    "CSS.getComputedStyleForNode",
                    &json!({"nodeId": node_id}),
                    EVAL_TIMEOUT,
                )
                .ok()
            })
            .map(|computed| picker_computed_styles(computed.get("computedStyle")))
            .unwrap_or_default();

        let object_id = self
            .cdp_call(
                "DOM.resolveNode",
                &json!({"backendNodeId": backend_node_id}),
                EVAL_TIMEOUT,
            )
            .ok()
            .and_then(|resolved| {
                resolved
                    .pointer("/object/objectId")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            });

        let call = |function: &str| -> Option<Value> {
            let object_id = object_id.as_deref()?;
            self.call_function_on(object_id, function).ok().flatten()
        };

        let text_and_rect = call(PICKER_TEXT_AND_RECT_FUNCTION).unwrap_or(Value::Null);
        let inner_text = picker_inner_text(&text_and_rect);
        let bounding_box = picker_bounding_box(&text_and_rect);

        let parent_path = picker_parent_path(call(PICKER_PARENT_PATH_FUNCTION).as_ref());

        let html = call(PICKER_HTML_FUNCTION).unwrap_or(Value::Null);
        let outer_html = picker_html_field(&html, "outerHTML");
        let sibling_html = picker_html_field(&html, "siblingHTML");

        let react = call(PICKER_REACT_FIBER_FUNCTION).unwrap_or(Value::Null);
        let react_component = picker_react_component(&react);
        let react_props = picker_react_props(react.get("props"));
        let source_file = picker_source_file(&react);

        let screenshot_base64 = self.capture_picker_screenshot(&bounding_box);

        if let Some(object_id) = object_id.as_deref() {
            let _ = self.cdp_call(
                "Runtime.releaseObject",
                &json!({"objectId": object_id}),
                EVAL_TIMEOUT,
            );
        }

        let sequence = self.lock_state().element_picker.next_sequence();
        Ok(SelectedElement {
            sequence,
            tag_name,
            id,
            classes,
            attributes,
            computed_styles,
            bounding_box,
            screenshot_base64,
            inner_text,
            parent_path,
            react_component,
            react_props,
            source_file,
            outer_html,
            sibling_html,
        })
    }

    /// A crop is a nicety, not the pick. Any failure degrades to an empty string rather than
    /// losing the element the user chose.
    fn capture_picker_screenshot(&self, bounding_box: &SelectedElementBox) -> String {
        let clip = picker_clip_rect(bounding_box);
        let captured = match clip {
            Some(clip) => self.capture_png_clip(false, Some(clip)),
            None => self.capture_screenshot_png(false),
        };
        captured
            .ok()
            .and_then(|capture| picker_screenshot_base64(&capture.bytes))
            .unwrap_or_default()
    }

    /// `Runtime.callFunctionOn` against a resolved element, so a picker script can use `this`
    /// where `preview_eval_value`'s scripts use a selector.
    fn call_function_on(
        &self,
        object_id: &str,
        function_declaration: &str,
    ) -> Result<Option<Value>, String> {
        let response = self.cdp_call(
            "Runtime.callFunctionOn",
            &json!({
                "objectId": object_id,
                "functionDeclaration": function_declaration,
                "returnByValue": true,
                "awaitPromise": true,
            }),
            EVAL_TIMEOUT,
        )?;
        if let Some(details) = response.get("exceptionDetails") {
            let description = details
                .pointer("/exception/description")
                .and_then(Value::as_str)
                .or_else(|| details.get("text").and_then(Value::as_str))
                .unwrap_or("page evaluation failed");
            return Err(description.to_owned());
        }
        Ok(response.pointer("/result/value").cloned())
    }

    // ----- interaction --------------------------------------------------------------------

    /// `preview_click`. `Ok(false)` is the source's "element not found or has no rect", which the
    /// dispatcher renders as `Failed to click element: {selector}`.
    pub(crate) fn preview_click(&self, selector: &str, double: bool) -> Result<bool, String> {
        let selector = validate_selector(selector)?;
        let literal = js_string_literal(&selector)?;
        let exists = self.preview_eval_value(&format!("!!document.querySelector({literal})"))?;
        if exists.as_ref().and_then(Value::as_bool) != Some(true) {
            return Ok(false);
        }
        let rect = self
            .preview_eval_value(&render_page_script(
                PREVIEW_CLICK_RECT_SCRIPT,
                &[("__MEWORK_SELECTOR__", &literal)],
            ))?
            .unwrap_or(Value::Null);
        let (Some(x), Some(y)) = (
            rect.get("x").and_then(Value::as_f64),
            rect.get("y").and_then(Value::as_f64),
        ) else {
            return Ok(false);
        };
        let (x, y) = (x.round(), y.round());
        for _ in 0..if double { 2 } else { 1 } {
            self.cdp_call(
                "Input.dispatchMouseEvent",
                &json!({"type":"mousePressed","x":x,"y":y,"button":"left","clickCount":1}),
                EVAL_TIMEOUT,
            )?;
            self.cdp_call(
                "Input.dispatchMouseEvent",
                &json!({"type":"mouseReleased","x":x,"y":y,"button":"left","clickCount":1}),
                EVAL_TIMEOUT,
            )?;
        }
        Ok(true)
    }

    /// `preview_fill`. One page-side script that focuses the element and writes through the native
    /// `value` setter, so a React controlled component observes the change instead of reverting it.
    pub(crate) fn preview_fill(&self, selector: &str, value: &str) -> Result<bool, String> {
        let selector = validate_selector(selector)?;
        if value.chars().count() > MAX_TEXT_INPUT_CHARS {
            return Err(format!(
                "preview_fill value exceeds the {MAX_TEXT_INPUT_CHARS}-character limit"
            ));
        }
        let selector_literal = js_string_literal(&selector)?;
        let value_literal = js_string_literal(value)?;
        let outcome = self.preview_eval_value(&render_page_script(
            PREVIEW_FILL_SCRIPT,
            &[
                ("__MEWORK_SELECTOR__", &selector_literal),
                ("__MEWORK_VALUE__", &value_literal),
            ],
        ))?;
        Ok(outcome
            .as_ref()
            .and_then(|outcome| outcome.get("success"))
            .and_then(Value::as_bool)
            == Some(true))
    }

    /// `CDPTools.evaluate`: one `Runtime.evaluate` whose completion value comes back by value.
    /// `None` is JavaScript `undefined` — CDP omits `result.value` for it but sends an explicit
    /// null for `null`, and the source distinguishes the two (`i?.value`, then `r === void 0`).
    fn preview_eval_value(&self, expression: &str) -> Result<Option<Value>, String> {
        let response = self.cdp_call(
            "Runtime.evaluate",
            &json!({
                "expression": expression,
                "returnByValue": true,
                "awaitPromise": true,
            }),
            EVAL_TIMEOUT,
        )?;
        if let Some(details) = response.get("exceptionDetails") {
            let description = details
                .pointer("/exception/description")
                .and_then(Value::as_str)
                .or_else(|| details.get("text").and_then(Value::as_str))
                .unwrap_or("page evaluation failed");
            return Err(description.to_owned());
        }
        Ok(response.pointer("/result/value").cloned())
    }

    // ----- screenshot ---------------------------------------------------------------------

    /// `preview_screenshot`: a JPEG clipped to at most 800 device pixels wide and returned base64
    /// inline. Mework's other screenshot path writes a PNG to a workspace path instead.
    #[cfg_attr(not(windows), allow(unused_variables))]
    pub(crate) fn preview_screenshot(
        &self,
        scale: Option<f64>,
    ) -> Result<PreviewScreenshot, String> {
        let scale = validate_preview_scale(scale)?;
        #[cfg(windows)]
        {
            self.hide_agent_pointer();
            self.with_composited_surface(|page| {
                let mut viewport = self.read_visual_viewport()?;
                // A page that has never been laid out reports a zero viewport and would capture
                // nothing; give it a desktop-sized one for the capture and take it back after.
                let seeded = viewport.width <= 0.0 || viewport.height <= 0.0;
                if seeded {
                    self.preview_set_viewport(1280, 720)?;
                    viewport = self.read_visual_viewport()?;
                    if viewport.width <= 0.0 {
                        viewport.width = 1280.0;
                    }
                    if viewport.height <= 0.0 {
                        viewport.height = 720.0;
                    }
                }
                let captured = self.capture_preview_jpeg(page, viewport, scale);
                if seeded {
                    let _ = self.preview_clear_viewport();
                }
                captured
            })
        }
        #[cfg(not(windows))]
        {
            Err(
                "preview_screenshot is currently supported only by Windows WebView2 (CDP Page.captureScreenshot)"
                    .into(),
            )
        }
    }

    #[cfg(windows)]
    fn capture_preview_jpeg(
        &self,
        page: &AttestedPage,
        viewport: PreviewViewport,
        scale: Option<f64>,
    ) -> Result<PreviewScreenshot, String> {
        let device_width = viewport.width * viewport.device_ratio;
        let fit = if device_width > PREVIEW_SCREENSHOT_MAX_WIDTH {
            PREVIEW_SCREENSHOT_MAX_WIDTH / device_width
        } else {
            1.0
        };
        let requested = fit * scale.unwrap_or(1.0);
        let mut params = json!({"format": "jpeg", "quality": PREVIEW_SCREENSHOT_QUALITY});
        let applied = if requested < 1.0 {
            params["clip"] = json!({
                "x": 0,
                "y": 0,
                "width": viewport.width,
                "height": viewport.height,
                "scale": requested,
            });
            requested
        } else {
            1.0
        };
        let control = self.webview2_control()?;
        let response = call_devtools_protocol(
            &control,
            page,
            "Page.captureScreenshot",
            &params.to_string(),
            SCREENSHOT_TIMEOUT,
            &|| self.has_modal_state(),
        )?;
        let data = response
            .get("data")
            .and_then(Value::as_str)
            .ok_or_else(|| "WebView2 screenshot response is missing data".to_owned())?;
        Ok(PreviewScreenshot {
            data: data.to_owned(),
            width: preview_capture_extent(viewport.width, applied, viewport.device_ratio),
            height: preview_capture_extent(viewport.height, applied, viewport.device_ratio),
        })
    }

    fn read_visual_viewport(&self) -> Result<PreviewViewport, String> {
        let metrics = self.cdp_call("Page.getLayoutMetrics", &json!({}), EVAL_TIMEOUT)?;
        let read = |pointer: &str| {
            metrics
                .pointer(pointer)
                .and_then(Value::as_f64)
                .unwrap_or(0.0)
        };
        let width = read("/cssVisualViewport/clientWidth");
        let height = read("/cssVisualViewport/clientHeight");
        let device_width = read("/visualViewport/clientWidth");
        let ratio = if width > 0.0 && device_width > 0.0 {
            device_width / width
        } else {
            1.0
        };
        Ok(PreviewViewport {
            width,
            height,
            device_ratio: ratio.clamp(0.25, 4.0),
        })
    }

    // ----- viewport emulation -------------------------------------------------------------

    /// `preview_resize`. Each branch's sentence is Claude Code's verbatim, joined with `. ` and
    /// closed with a period.
    pub(crate) fn preview_resize(
        &self,
        preset: Option<&str>,
        width: Option<f64>,
        height: Option<f64>,
        color_scheme: Option<&str>,
    ) -> Result<String, String> {
        if let Some(scheme) = color_scheme {
            if !matches!(scheme, "light" | "dark") {
                return Err(format!(
                    "Unknown colorScheme \"{}\". Use light or dark.",
                    truncate(scheme, 40)
                ));
            }
        }
        let mut parts: Vec<String> = Vec::new();
        if preset == Some("desktop") {
            self.preview_clear_viewport()
                .map_err(|error| format!("Resize failed: {error}"))?;
            parts.push(
                "Viewport emulation cleared; the tab is back to the pane's responsive size (desktop)"
                    .to_owned(),
            );
        } else if preset.is_some() || width.is_some() || height.is_some() {
            let (viewport_width, viewport_height) = match preset {
                Some(preset) => preview_viewport_preset(preset).ok_or_else(|| {
                    format!(
                        "Unknown preset \"{}\". Use mobile, tablet, or desktop.",
                        truncate(preset, 40)
                    )
                })?,
                None => match (
                    preview_viewport_dimension(width),
                    preview_viewport_dimension(height),
                ) {
                    (Some(width), Some(height)) => (width, height),
                    _ => return Err(format!(
                        "A custom viewport needs both width and height, each a number from 1 to {PREVIEW_VIEWPORT_MAX}. Use preset \"mobile\" or \"tablet\" for a device size, or preset \"desktop\" to clear the emulation and return to the pane's responsive size."
                    )),
                },
            };
            self.preview_set_viewport(viewport_width, viewport_height)
                .map_err(|error| format!("Resize failed: {error}"))?;
            let label = preset
                .map(|preset| format!(" ({preset})"))
                .unwrap_or_default();
            parts.push(format!(
                "Viewport set to {viewport_width}x{viewport_height}{label} on this tab. It stays (scaled down to fit if larger than the pane) until you call this tool with preset \"desktop\", so reset it when you finish testing"
            ));
        }
        if let Some(scheme) = color_scheme {
            self.preview_set_color_scheme(scheme)
                .map_err(|error| format!("Resize failed: {error}"))?;
            parts.push(format!(
                "Color scheme emulation set to {scheme} on this tab; it survives reloads until you set the other value or the pane re-syncs the tab to the app theme"
            ));
        }
        if parts.is_empty() {
            return Err(
                "Provide a preset (mobile/tablet/desktop), width/height, or colorScheme."
                    .to_owned(),
            );
        }
        Ok(format!("{}.", parts.join(". ")))
    }

    fn preview_set_viewport(&self, width: u32, height: u32) -> Result<(), String> {
        let mobile = width < PREVIEW_MOBILE_MAX_WIDTH;
        self.cdp_call(
            "Emulation.setDeviceMetricsOverride",
            &json!({
                "width": width,
                "height": height,
                "deviceScaleFactor": if mobile { 2 } else { 0 },
                "mobile": mobile,
            }),
            EVAL_TIMEOUT,
        )?;
        self.preview_set_mobile_overrides(mobile)
    }

    fn preview_clear_viewport(&self) -> Result<(), String> {
        self.cdp_call(
            "Emulation.clearDeviceMetricsOverride",
            &json!({}),
            EVAL_TIMEOUT,
        )?;
        self.preview_set_mobile_overrides(false)
    }

    fn preview_set_color_scheme(&self, scheme: &str) -> Result<(), String> {
        self.cdp_call(
            "Emulation.setEmulatedMedia",
            &json!({"features": [{"name": "prefers-color-scheme", "value": scheme}]}),
            EVAL_TIMEOUT,
        )
        .map(|_| ())
    }

    /// Android Chrome user agent, five touch points and mouse-to-touch translation, so a page's
    /// load-time device gates see a phone rather than a narrow desktop window. Upstream skips this
    /// when the flag already matches; here the calls are synchronous and idempotent, so the state
    /// is simply re-asserted rather than tracked.
    fn preview_set_mobile_overrides(&self, enabled: bool) -> Result<(), String> {
        let user_agent = if enabled {
            let version = self.chromium_major_version();
            json!({
                "userAgent": preview_mobile_user_agent(&version),
                "userAgentMetadata": preview_mobile_user_agent_metadata(&version),
            })
        } else {
            json!({ "userAgent": "" })
        };
        self.cdp_call("Emulation.setUserAgentOverride", &user_agent, EVAL_TIMEOUT)?;
        let mut touch = json!({ "enabled": enabled });
        if enabled {
            touch["maxTouchPoints"] = json!(PREVIEW_MOBILE_TOUCH_POINTS);
        }
        self.cdp_call("Emulation.setTouchEmulationEnabled", &touch, EVAL_TIMEOUT)?;
        let mut emit = json!({ "enabled": enabled });
        if enabled {
            emit["configuration"] = json!("mobile");
        }
        self.cdp_call("Emulation.setEmitTouchEventsForMouse", &emit, EVAL_TIMEOUT)?;
        Ok(())
    }

    /// Claude Code builds the emulated user agent from Electron's own `process.versions.chrome`
    /// and falls back to `0`; the equivalent here is the WebView2 runtime the page already runs on.
    fn chromium_major_version(&self) -> String {
        self.cdp_call("Browser.getVersion", &json!({}), EVAL_TIMEOUT)
            .ok()
            .and_then(|response| {
                response
                    .get("product")
                    .and_then(Value::as_str)
                    .and_then(chromium_major_version)
            })
            .unwrap_or_else(|| "0".to_owned())
    }

    // ----- console and network listings ---------------------------------------------------

    /// `preview_console_logs`, rendered from the in-page console buffer.
    pub(crate) fn preview_console_logs(
        &self,
        level: Option<&str>,
        lines: Option<u64>,
    ) -> Result<String, String> {
        let limit = clamp_log_limit(lines, PREVIEW_DEFAULT_LOG_LINES);
        let level = js_string_literal(level.unwrap_or("all"))?;
        let payload = self.eval_value(
            &format!(
                r#"
const level = {level};
const render = value => typeof value === "string" ? value : (() => {{ try {{ return JSON.stringify(value); }} catch (_) {{ return String(value); }} }})();
let entries = __state.consoleEntries.map(entry => ({{
  level: String(entry.level || "log"),
  text: Array.isArray(entry.args) ? entry.args.map(render).join(" ") : String(entry.message || "")
}}));
if (level === "error") entries = entries.filter(entry => entry.level === "error");
else if (level === "warn") entries = entries.filter(entry => entry.level === "warn" || entry.level === "error");
const total = entries.length;
if (entries.length > {limit}) entries = entries.slice(entries.length - {limit});
return {{entries, total}};
"#
            ),
            EVAL_TIMEOUT,
        )?;
        let total = payload.get("total").and_then(Value::as_u64).unwrap_or(0);
        let entries = payload
            .get("entries")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if entries.is_empty() {
            return Ok("No console logs.".to_owned());
        }
        let rendered = entries
            .iter()
            .map(|entry| {
                format!(
                    "[{}] {}",
                    entry.get("level").and_then(Value::as_str).unwrap_or("log"),
                    truncate_with_ellipsis(
                        entry
                            .get("text")
                            .and_then(Value::as_str)
                            .unwrap_or_default(),
                        PREVIEW_MAX_LOG_TEXT_CHARS
                    )
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        Ok(format!(
            "{rendered}{}",
            console_log_footer(entries.len() as u64, total)
        ))
    }

    /// `preview_network`. Without `request_id` this lists the DevTools ledger; with one it reads
    /// that response's body back out of the browser cache.
    pub(crate) fn preview_network(
        &self,
        filter: Option<&str>,
        request_id: Option<&str>,
    ) -> Result<String, String> {
        if let Some(request_id) = request_id {
            // Upstream swallows the CDP failure here: an id the cache no longer holds and a
            // transport error are the same answer to the model.
            let Some(body) = self
                .cdp_call(
                    "Network.getResponseBody",
                    &json!({ "requestId": request_id }),
                    EVAL_TIMEOUT,
                )
                .ok()
            else {
                return Err(format!(
                    "Response body not available for request {request_id}. It may have been evicted from the browser cache."
                ));
            };
            let text = body.get("body").and_then(Value::as_str).unwrap_or_default();
            if body.get("base64Encoded").and_then(Value::as_bool) == Some(true) {
                return Ok(format!(
                    "Response is binary (base64-encoded, {} chars). Not displayed.",
                    text.chars().count()
                ));
            }
            return Ok(render_preview_response_body(text));
        }
        let failed_only = filter == Some("failed");
        let entries: Vec<NetworkLogEntry> = {
            let state = self.lock_state();
            state
                .activity
                .network_log
                .iter()
                .filter(|entry| {
                    !failed_only || entry.failed || entry.status.is_some_and(|status| status >= 400)
                })
                .cloned()
                .collect()
        };
        if entries.is_empty() {
            return Ok(if failed_only {
                "No failed requests.".to_owned()
            } else {
                "No network requests recorded.".to_owned()
            });
        }
        Ok(entries
            .iter()
            .map(format_network_entry)
            .collect::<Vec<_>>()
            .join("\n"))
    }
}

/// Substitutes `__MEWORK_*__` markers in one left-to-right pass, so an inserted JavaScript string
/// literal is never rescanned and a selector that happens to contain a marker cannot expand.
fn render_page_script(template: &str, values: &[(&str, &str)]) -> String {
    let mut rendered = String::with_capacity(template.len() + 64);
    let mut rest = template;
    loop {
        let next = values
            .iter()
            .filter_map(|(marker, value)| rest.find(marker).map(|at| (at, *marker, *value)))
            .min_by_key(|(at, _, _)| *at);
        let Some((at, marker, value)) = next else {
            rendered.push_str(rest);
            return rendered;
        };
        rendered.push_str(&rest[..at]);
        rendered.push_str(value);
        rest = &rest[at + marker.len()..];
    }
}

/// Rebuilds the flat `Accessibility.getFullAXTree` node list into a tree rooted at its first entry.
fn build_ax_tree(nodes: &[Value], uid: &mut u64) -> Option<AxNode> {
    let root = nodes.first()?;
    let mut by_id: HashMap<&str, &Value> = HashMap::with_capacity(nodes.len());
    for node in nodes {
        if let Some(id) = node.get("nodeId").and_then(Value::as_str) {
            by_id.insert(id, node);
        }
    }
    Some(build_ax_node(&by_id, root, uid, 0))
}

fn build_ax_node(
    by_id: &HashMap<&str, &Value>,
    node: &Value,
    uid: &mut u64,
    depth: usize,
) -> AxNode {
    *uid += 1;
    let own = *uid;
    let mut children = Vec::new();
    if depth < PREVIEW_AX_MAX_TREE_DEPTH {
        if let Some(ids) = node.get("childIds").and_then(Value::as_array) {
            for id in ids.iter().filter_map(Value::as_str) {
                if let Some(child) = by_id.get(id) {
                    children.push(build_ax_node(by_id, child, uid, depth + 1));
                }
            }
        }
    }
    AxNode {
        uid: own,
        role: node
            .pointer("/role/value")
            .and_then(Value::as_str)
            .filter(|role| !role.is_empty())
            .unwrap_or("unknown")
            .to_owned(),
        name: node
            .pointer("/name/value")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        value: ax_property_string(node.pointer("/value/value")),
        description: node
            .pointer("/description/value")
            .and_then(Value::as_str)
            .map(str::to_owned),
        backend_node_id: node.get("backendDOMNodeId").and_then(Value::as_i64),
        children,
    }
}

/// `String(node.value.value)` for anything but `null`/absent.
fn ax_property_string(value: Option<&Value>) -> Option<String> {
    match value {
        None | Some(Value::Null) => None,
        Some(Value::String(text)) => Some(text.clone()),
        Some(other) => Some(other.to_string()),
    }
}

fn ax_role_is_generic(role: &str) -> bool {
    PREVIEW_AX_GENERIC_ROLES.contains(&role)
}

fn ax_value_is_set(node: &AxNode) -> bool {
    node.value.as_deref().is_some_and(|value| !value.is_empty())
}

/// Whether anything below this node carries meaning of its own. An unnamed `img` does not count.
fn ax_has_interesting_descendant(node: &AxNode) -> bool {
    node.children.iter().any(|child| {
        (PREVIEW_AX_INTERESTING_ROLES.contains(&child.role.as_str())
            && !(child.role == "img" && child.name.is_empty()))
            || ax_has_interesting_descendant(child)
    })
}

/// A nameless generic wrapper around nothing interesting: its children are printed in its place.
fn ax_is_transparent(node: &AxNode) -> bool {
    if !ax_role_is_generic(&node.role) || !node.name.is_empty() || ax_value_is_set(node) {
        return false;
    }
    !ax_has_interesting_descendant(node)
}

fn ax_descendant_count(node: &AxNode) -> usize {
    node.children.len() + node.children.iter().map(ax_descendant_count).sum::<usize>()
}

/// Walks a rebuilt tree into exactly the lines [`format_ax_snapshot`] prints, in print order.
///
/// The transparency, redundancy, opacity and depth rules all live here; the renderer below only
/// turns one line into text. A transparent wrapper contributes no line of its own, a redundant
/// one is replaced by its single child, and a node past the depth cap contributes one line whose
/// descendants are summarised instead of walked.
fn ax_snapshot_lines(node: &AxNode, indent: usize, depth: usize, out: &mut Vec<AxLine>) {
    let line = |descendants_note| AxLine {
        indent,
        uid: node.uid,
        role: node.role.clone(),
        name: node.name.clone(),
        value: node.value.clone().unwrap_or_default(),
        backend_node_id: node.backend_node_id,
        descendants_note,
    };
    if depth > PREVIEW_SNAPSHOT_MAX_DEPTH {
        let descendants = ax_descendant_count(node);
        out.push(line((descendants > 0).then_some(descendants)));
        return;
    }
    // An SVG's internals, and a decorative image's, are noise the model cannot act on.
    let opaque = node.role == "SvgRoot"
        || (node.role == "img"
            && !node.children.is_empty()
            && !ax_has_interesting_descendant(node));
    let children: &[AxNode] = if opaque { &[] } else { &node.children };
    if ax_is_transparent(node) {
        for child in children {
            ax_snapshot_lines(child, indent, depth, out);
        }
        return;
    }
    // A generic wrapper whose only content is one child adds an indent level and nothing else.
    let redundant = node.name.is_empty()
        && !ax_value_is_set(node)
        && children.len() == 1
        && ax_role_is_generic(&node.role);
    if redundant {
        ax_snapshot_lines(&children[0], indent, depth, out);
        return;
    }
    out.push(line(None));
    for child in children {
        ax_snapshot_lines(child, indent + 1, depth + 1, out);
    }
}

/// One snapshot line as text: `[uid] role: "name" (value: "…")`, names and values capped.
/// `with_indent` is false where the line stands on its own — a hit, a choice option — and the
/// tree position it came from would only be noise.
pub(crate) fn render_ax_line(line: &AxLine, with_indent: bool) -> String {
    let name = truncate_with_ellipsis(&line.name, PREVIEW_SNAPSHOT_TEXT_CHARS);
    let value = truncate_with_ellipsis(&line.value, PREVIEW_SNAPSHOT_TEXT_CHARS);
    let indent = if with_indent { "  ".repeat(line.indent) } else { String::new() };
    let mut text = format!("{indent}[{}] {}", line.uid, line.role);
    if !name.is_empty() {
        text.push_str(&format!(": \"{name}\""));
    }
    if !value.is_empty() {
        text.push_str(&format!(" (value: \"{value}\")"));
    }
    if let Some(descendants) = line.descendants_note {
        text.push_str(&format!(" ... ({descendants} descendants)"));
    }
    text
}

fn format_ax_snapshot(node: &AxNode, indent: usize, depth: usize) -> String {
    let mut lines = Vec::new();
    ax_snapshot_lines(node, indent, depth, &mut lines);
    lines
        .iter()
        .map(|line| render_ax_line(line, true))
        .collect::<Vec<_>>()
        .join("\n")
}

fn truncate_preview_snapshot(text: &str) -> String {
    let total = text.chars().count();
    if total <= PREVIEW_SNAPSHOT_MAX_CHARS {
        return text.to_owned();
    }
    format!(
        "{}\n\n... (truncated \u{2014} {total} total chars, showing first {PREVIEW_SNAPSHOT_MAX_CHARS})",
        truncate(text, PREVIEW_SNAPSHOT_MAX_CHARS)
    )
}

/// A hard character cap with a trailing `...`, applied to every name, value and log line the
/// preview surface renders.
fn truncate_with_ellipsis(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_owned();
    }
    format!("{}...", truncate(value, max_chars))
}

/// Keeps a `data:` URL's media type and drops its payload, which is otherwise megabytes of base64.
fn truncate_network_url(url: &str, max_chars: usize) -> String {
    if url.starts_with("data:") {
        if let Some(comma) = url.find(',') {
            return format!(
                "{},<{} chars omitted>",
                truncate_with_ellipsis(&url[..comma], 128),
                url[comma + 1..].chars().count()
            );
        }
    }
    truncate_with_ellipsis(url, max_chars)
}

fn format_network_entry(entry: &NetworkLogEntry) -> String {
    let mut line = format!("[{}] {} {}", entry.request_id, entry.method, entry.url);
    if let Some(status) = entry.status {
        line.push_str(&format!(" \u{2192} {status} {}", entry.status_text));
    }
    if entry.failed {
        line.push_str(&format!(" [FAILED: {}]", entry.error_text));
    }
    line
}

fn console_log_footer(shown: u64, total: u64) -> String {
    if total <= shown {
        return String::new();
    }
    let hint = if shown < MAX_LOG_LIMIT {
        " Use 'lines' parameter (max 200) to see more."
    } else {
        ""
    };
    format!("\n\n(Showing last {shown} of {total} entries.{hint})")
}

/// Pretty-prints a parseable JSON body before applying the cap. The reported total is the raw
/// body's length, not the pretty-printed one's.
fn render_preview_response_body(body: &str) -> String {
    let mut text = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|value| serde_json::to_string_pretty(&value).ok())
        .unwrap_or_else(|| body.to_owned());
    if text.chars().count() > PREVIEW_RESPONSE_BODY_CHARS {
        text = format!(
            "{}\n... (truncated, {} total chars)",
            truncate(&text, PREVIEW_RESPONSE_BODY_CHARS),
            body.chars().count()
        );
    }
    text
}

fn preview_viewport_preset(preset: &str) -> Option<(u32, u32)> {
    match preset {
        "mobile" => Some((375, 812)),
        "tablet" => Some((768, 1024)),
        _ => None,
    }
}

fn preview_viewport_dimension(value: Option<f64>) -> Option<u32> {
    let value = value?;
    if !value.is_finite() || value < 1.0 || value > f64::from(PREVIEW_VIEWPORT_MAX) {
        return None;
    }
    Some(value.round() as u32)
}

fn browser_viewport_in_range(size: BrowserViewport) -> bool {
    (1..=PREVIEW_VIEWPORT_MAX).contains(&size.width)
        && (1..=PREVIEW_VIEWPORT_MAX).contains(&size.height)
}

fn browser_viewport_action_error() -> String {
    format!(
        "browser_action viewport 需要 null（响应式）或含 1 到 {PREVIEW_VIEWPORT_MAX} 之间 width 与 height 的对象 value"
    )
}

/// Decodes the trusted pane's `browser_action viewport` value.
///
/// An absent or null value is the pane's "Responsive" entry, which clears the emulation; anything
/// else must be a `{width, height}` object inside the same bounds `preview_resize` enforces.
pub(crate) fn parse_browser_viewport_action(
    value: Option<&Value>,
) -> Result<Option<BrowserViewport>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let object = value
        .as_object()
        .ok_or_else(browser_viewport_action_error)?;
    let dimension = |key: &str| preview_viewport_dimension(object.get(key).and_then(Value::as_f64));
    match (dimension("width"), dimension("height")) {
        (Some(width), Some(height)) => Ok(Some(BrowserViewport { width, height })),
        _ => Err(browser_viewport_action_error()),
    }
}

fn validate_preview_scale(scale: Option<f64>) -> Result<Option<f64>, String> {
    let Some(scale) = scale else {
        return Ok(None);
    };
    if !scale.is_finite() || scale < PREVIEW_SCALE_MIN || scale > 1.0 {
        return Err(format!(
            "scale must be a number in [{PREVIEW_SCALE_MIN}, 1] \u{2014} e.g. 0.5 for a half-size image"
        ));
    }
    Ok(Some(scale))
}

/// `Math.max(1, Math.round(Math.floor(extent) * scale * ratio))`.
fn preview_capture_extent(extent: f64, scale: f64, ratio: f64) -> u32 {
    let value = (extent.floor() * scale * ratio).round();
    if !value.is_finite() || value < 1.0 {
        return 1;
    }
    value.min(f64::from(u32::MAX)) as u32
}

/// `Chrome/141.0.7390.55` → `141`.
fn chromium_major_version(product: &str) -> Option<String> {
    let major = product.rsplit('/').next()?.split('.').next()?;
    (!major.is_empty() && major.bytes().all(|byte| byte.is_ascii_digit())).then(|| major.to_owned())
}

fn preview_mobile_user_agent(major: &str) -> String {
    format!("Mozilla/5.0 (Linux; Android 14; Pixel 8) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/{major}.0.0.0 Mobile Safari/537.36")
}

fn preview_mobile_user_agent_metadata(major: &str) -> Value {
    let full = format!("{major}.0.0.0");
    json!({
        "brands": [
            {"brand": "Chromium", "version": major},
            {"brand": "Google Chrome", "version": major},
            {"brand": "Not=A?Brand", "version": "24"},
        ],
        "fullVersion": full,
        "fullVersionList": [
            {"brand": "Chromium", "version": full},
            {"brand": "Google Chrome", "version": full},
            {"brand": "Not=A?Brand", "version": "24.0.0.0"},
        ],
        "platform": "Android",
        "platformVersion": "14.0.0",
        "architecture": "",
        "model": "Pixel 8",
        "mobile": true,
    })
}

#[cfg(test)]
mod preview_primitive_tests {
    use super::*;

    fn ax(id: &str, role: &str, name: &str, children: &[&str]) -> Value {
        json!({
            "nodeId": id,
            "role": {"value": role},
            "name": {"value": name},
            "childIds": children,
        })
    }

    fn render(nodes: &[Value]) -> String {
        let mut uid = 0;
        let tree = build_ax_tree(nodes, &mut uid).expect("tree");
        format_ax_snapshot(&tree, 0, 0)
    }

    #[test]
    fn snapshot_renders_indented_uid_role_name_lines() {
        let nodes = vec![
            ax("1", "RootWebArea", "Example", &["2", "3"]),
            ax("2", "heading", "Title", &[]),
            json!({
                "nodeId": "3",
                "role": {"value": "textbox"},
                "name": {"value": "Search"},
                "value": {"value": "hello"},
                "childIds": [],
            }),
        ];
        assert_eq!(
            render(&nodes),
            "[1] RootWebArea: \"Example\"\n  [2] heading: \"Title\"\n  [3] textbox: \"Search\" (value: \"hello\")"
        );
    }

    #[test]
    fn snapshot_flattens_generic_wrappers_around_nothing_interesting() {
        // A nameless `generic` with no actionable descendant prints its children in its place.
        let nodes = vec![
            ax("1", "RootWebArea", "Page", &["2"]),
            ax("2", "generic", "", &["3", "4"]),
            ax("3", "StaticText", "one", &[]),
            ax("4", "StaticText", "two", &[]),
        ];
        assert_eq!(
            render(&nodes),
            "[1] RootWebArea: \"Page\"\n  [3] StaticText: \"one\"\n  [4] StaticText: \"two\""
        );
    }

    #[test]
    fn snapshot_collapses_a_single_child_generic_wrapper() {
        let nodes = vec![
            ax("1", "generic", "", &["2"]),
            ax("2", "button", "Send", &[]),
        ];
        assert_eq!(render(&nodes), "[2] button: \"Send\"");
    }

    #[test]
    fn snapshot_hides_svg_internals() {
        let nodes = vec![
            ax("1", "RootWebArea", "Page", &["2"]),
            ax("2", "SvgRoot", "Logo", &["3"]),
            ax("3", "graphics-symbol", "path", &[]),
        ];
        assert_eq!(
            render(&nodes),
            "[1] RootWebArea: \"Page\"\n  [2] SvgRoot: \"Logo\""
        );
    }

    #[test]
    fn snapshot_caps_depth_with_a_descendant_tail() {
        let mut nodes = vec![ax("0", "RootWebArea", "Deep", &["1"])];
        for level in 1..=11 {
            let child = (level + 1).to_string();
            nodes.push(ax(
                &level.to_string(),
                "listitem",
                &format!("level {level}"),
                &[child.as_str()],
            ));
        }
        nodes.push(ax("12", "listitem", "level 12", &[]));
        let rendered = render(&nodes);
        // Depth 0..=8 print in full; the ninth level prints once with the rest summarised.
        assert!(rendered.contains("[9] listitem: \"level 8\""));
        assert!(rendered.ends_with("[10] listitem: \"level 9\" ... (3 descendants)"));
        assert!(!rendered.contains("level 10"));
    }

    #[test]
    fn snapshot_truncates_names_and_values_at_two_hundred_chars() {
        let long = "n".repeat(250);
        let nodes = vec![json!({
            "nodeId": "1",
            "role": {"value": "textbox"},
            "name": {"value": long},
            "value": {"value": "v".repeat(250)},
            "childIds": [],
        })];
        let rendered = render(&nodes);
        assert!(rendered.contains(&format!(": \"{}...\"", "n".repeat(200))));
        assert!(rendered.contains(&format!("(value: \"{}...\")", "v".repeat(200))));
    }

    #[test]
    fn snapshot_uids_continue_across_calls() {
        let nodes = vec![ax("1", "button", "Go", &[])];
        let mut uid = 7;
        let tree = build_ax_tree(&nodes, &mut uid).expect("tree");
        assert_eq!(uid, 8);
        assert_eq!(format_ax_snapshot(&tree, 0, 0), "[8] button: \"Go\"");
    }

    /// The line walk and the text renderer are one traversal: the lines carry exactly what the
    /// snapshot prints, plus the DOM identity a decision-model tool needs to act on one of them.
    #[test]
    fn element_lines_are_the_snapshot_lines_with_dom_identity() {
        let nodes = vec![
            json!({
                "nodeId": "1",
                "backendDOMNodeId": 11,
                "role": {"value": "RootWebArea"},
                "name": {"value": "Example"},
                "childIds": ["2", "3"],
            }),
            json!({
                "nodeId": "2",
                "backendDOMNodeId": 22,
                "role": {"value": "heading"},
                "name": {"value": "Title"},
                "childIds": [],
            }),
            json!({
                "nodeId": "3",
                "role": {"value": "textbox"},
                "name": {"value": "Search"},
                "value": {"value": "hello"},
                "childIds": [],
            }),
        ];
        let mut uid = 0;
        let tree = build_ax_tree(&nodes, &mut uid).expect("tree");
        let mut lines = Vec::new();
        ax_snapshot_lines(&tree, 0, 0, &mut lines);
        assert_eq!(
            lines
                .iter()
                .map(|line| render_ax_line(line, true))
                .collect::<Vec<_>>()
                .join("\n"),
            format_ax_snapshot(&tree, 0, 0)
        );
        assert_eq!(
            lines
                .iter()
                .map(|line| (line.indent, line.uid, line.backend_node_id))
                .collect::<Vec<_>>(),
            vec![(0, 1, Some(11)), (1, 2, Some(22)), (1, 3, None)]
        );
        // Standing on its own, a line drops the tree indentation and keeps everything else.
        assert_eq!(
            render_ax_line(&lines[2], false),
            "[3] textbox: \"Search\" (value: \"hello\")"
        );
        assert_eq!(render_ax_line(&lines[1], true), "  [2] heading: \"Title\"");
    }

    /// Flattened wrappers contribute no line of their own, and a depth-capped node carries its
    /// descendant count on the line instead of walking into them.
    #[test]
    fn element_lines_follow_the_flattening_and_depth_rules() {
        let nodes = vec![
            ax("1", "RootWebArea", "Page", &["2"]),
            ax("2", "generic", "", &["3", "4"]),
            ax("3", "StaticText", "one", &[]),
            ax("4", "StaticText", "two", &[]),
        ];
        let mut uid = 0;
        let tree = build_ax_tree(&nodes, &mut uid).expect("tree");
        let mut lines = Vec::new();
        ax_snapshot_lines(&tree, 0, 0, &mut lines);
        assert_eq!(
            lines.iter().map(|line| line.uid).collect::<Vec<_>>(),
            vec![1, 3, 4],
            "the nameless generic wrapper prints its children in its place"
        );

        let mut deep = vec![ax("0", "RootWebArea", "Deep", &["1"])];
        for level in 1..=11 {
            let child = (level + 1).to_string();
            deep.push(ax(
                &level.to_string(),
                "listitem",
                &format!("level {level}"),
                &[child.as_str()],
            ));
        }
        deep.push(ax("12", "listitem", "level 12", &[]));
        let mut uid = 0;
        let tree = build_ax_tree(&deep, &mut uid).expect("tree");
        let mut lines = Vec::new();
        ax_snapshot_lines(&tree, 0, 0, &mut lines);
        let last = lines.last().expect("a capped line");
        assert_eq!(last.descendants_note, Some(3));
        assert_eq!(
            render_ax_line(last, false),
            "[10] listitem: \"level 9\" ... (3 descendants)"
        );
    }

    #[test]
    fn snapshot_bounds_a_cyclic_child_graph() {
        let nodes = vec![
            ax("1", "generic", "a", &["2"]),
            ax("2", "generic", "b", &["1"]),
        ];
        let mut uid = 0;
        build_ax_tree(&nodes, &mut uid).expect("tree");
        assert_eq!(uid as usize, PREVIEW_AX_MAX_TREE_DEPTH as usize + 1);
    }

    #[test]
    fn whole_snapshot_truncation_reports_the_total() {
        let text = "x".repeat(PREVIEW_SNAPSHOT_MAX_CHARS);
        assert_eq!(truncate_preview_snapshot(&text), text);
        let longer = "y".repeat(PREVIEW_SNAPSHOT_MAX_CHARS + 34);
        let truncated = truncate_preview_snapshot(&longer);
        assert!(truncated
            .ends_with("\n\n... (truncated \u{2014} 12034 total chars, showing first 12000)"));
        assert!(truncated.starts_with(&"y".repeat(PREVIEW_SNAPSHOT_MAX_CHARS)));
    }

    #[test]
    fn empty_tree_has_no_root() {
        let mut uid = 0;
        assert!(build_ax_tree(&[], &mut uid).is_none());
        assert_eq!(uid, 0);
    }

    fn entry(request_id: &str) -> NetworkLogEntry {
        NetworkLogEntry {
            request_id: request_id.to_owned(),
            url: "https://example.test/a".to_owned(),
            method: "GET".to_owned(),
            status: None,
            status_text: String::new(),
            failed: false,
            error_text: String::new(),
        }
    }

    #[test]
    fn network_lines_carry_status_and_failure() {
        let mut plain = entry("1");
        assert_eq!(
            format_network_entry(&plain),
            "[1] GET https://example.test/a"
        );
        plain.status = Some(404);
        plain.status_text = "Not Found".to_owned();
        assert_eq!(
            format_network_entry(&plain),
            "[1] GET https://example.test/a \u{2192} 404 Not Found"
        );
        plain.failed = true;
        plain.error_text = "net::ERR_ABORTED".to_owned();
        assert_eq!(
            format_network_entry(&plain),
            "[1] GET https://example.test/a \u{2192} 404 Not Found [FAILED: net::ERR_ABORTED]"
        );
    }

    #[test]
    fn network_ledger_is_capped_and_redirects_replace_the_row() {
        let mut activity = PageActivity::default();
        for index in 0..PREVIEW_MAX_NETWORK_ENTRIES + 10 {
            activity.record_network_log(
                &index.to_string(),
                format!("https://example.test/{index}"),
                "GET".to_owned(),
            );
        }
        assert_eq!(activity.network_log.len(), PREVIEW_MAX_NETWORK_ENTRIES);
        assert_eq!(activity.network_log[0].request_id, "10");

        activity.network_entry_mut("11").expect("row").status = Some(302);
        activity.record_network_log(
            "11",
            "https://example.test/moved".to_owned(),
            "GET".to_owned(),
        );
        let redirected = activity.network_entry_mut("11").expect("row");
        assert_eq!(redirected.url, "https://example.test/moved");
        assert_eq!(redirected.status, None);
        assert_eq!(activity.network_log.len(), PREVIEW_MAX_NETWORK_ENTRIES);
    }

    #[test]
    fn devtools_events_fill_the_network_ledger() {
        let mut activity = PageActivity::default();
        record_devtools_event(
            &mut activity,
            "Network.requestWillBeSent",
            &json!({
                "requestId": "7",
                "type": "XHR",
                "request": {"url": "https://example.test/api", "method": "POST"},
            }),
        );
        record_devtools_event(
            &mut activity,
            "Network.responseReceived",
            &json!({"requestId": "7", "response": {"status": 500, "statusText": "Server Error"}}),
        );
        record_devtools_event(
            &mut activity,
            "Network.loadingFailed",
            &json!({"requestId": "7", "errorText": "net::ERR_FAILED"}),
        );
        assert_eq!(
            format_network_entry(&activity.network_log[0]),
            "[7] POST https://example.test/api \u{2192} 500 Server Error [FAILED: net::ERR_FAILED]"
        );
        assert!(activity
            .requests
            .get("7")
            .is_some_and(|request| request.finished));
    }

    #[test]
    fn data_urls_lose_their_payload_in_the_listing() {
        let url = format!("data:image/png;base64,{}", "A".repeat(4_096));
        assert_eq!(
            truncate_network_url(&url, PREVIEW_MAX_LOG_TEXT_CHARS),
            "data:image/png;base64,<4096 chars omitted>"
        );
        assert_eq!(
            truncate_network_url("https://example.test/a", PREVIEW_MAX_LOG_TEXT_CHARS),
            "https://example.test/a"
        );
    }

    #[test]
    fn console_footer_appears_only_when_entries_were_dropped() {
        assert_eq!(console_log_footer(50, 50), "");
        assert_eq!(
            console_log_footer(50, 120),
            "\n\n(Showing last 50 of 120 entries. Use 'lines' parameter (max 200) to see more.)"
        );
        assert_eq!(
            console_log_footer(200, 900),
            "\n\n(Showing last 200 of 900 entries.)"
        );
    }

    #[test]
    fn response_bodies_are_pretty_printed_then_capped() {
        assert_eq!(
            render_preview_response_body(r#"{"a":1}"#),
            "{\n  \"a\": 1\n}"
        );
        let body = "z".repeat(PREVIEW_RESPONSE_BODY_CHARS + 5);
        let rendered = render_preview_response_body(&body);
        assert!(rendered.ends_with("\n... (truncated, 10005 total chars)"));
        assert!(rendered.starts_with(&"z".repeat(PREVIEW_RESPONSE_BODY_CHARS)));
    }

    #[test]
    fn viewport_presets_and_custom_bounds() {
        assert_eq!(preview_viewport_preset("mobile"), Some((375, 812)));
        assert_eq!(preview_viewport_preset("tablet"), Some((768, 1024)));
        assert_eq!(preview_viewport_preset("desktop"), None);
        assert_eq!(preview_viewport_preset("watch"), None);

        assert_eq!(preview_viewport_dimension(Some(1.0)), Some(1));
        assert_eq!(preview_viewport_dimension(Some(9_999.0)), Some(9_999));
        assert_eq!(preview_viewport_dimension(Some(1_280.4)), Some(1_280));
        assert_eq!(preview_viewport_dimension(Some(0.9)), None);
        assert_eq!(preview_viewport_dimension(Some(10_000.0)), None);
        assert_eq!(preview_viewport_dimension(Some(f64::NAN)), None);
        assert_eq!(preview_viewport_dimension(None), None);
    }

    #[test]
    fn trusted_viewport_action_treats_null_as_the_responsive_reset() {
        assert_eq!(parse_browser_viewport_action(None), Ok(None));
        assert_eq!(parse_browser_viewport_action(Some(&Value::Null)), Ok(None));
        assert_eq!(
            parse_browser_viewport_action(Some(&json!({"width": 375, "height": 812}))),
            Ok(Some(BrowserViewport {
                width: 375,
                height: 812
            }))
        );
        assert_eq!(
            parse_browser_viewport_action(Some(&json!({"width": 768, "height": 1024}))),
            Ok(Some(BrowserViewport {
                width: 768,
                height: 1024
            }))
        );
    }

    #[test]
    fn trusted_viewport_action_rejects_shapes_the_pane_never_sends() {
        for value in [
            json!("mobile"),
            json!(375),
            json!([375, 812]),
            json!({"width": 375}),
            json!({"height": 812}),
            json!({"width": 0, "height": 812}),
            json!({"width": 375, "height": 10_000}),
            json!({"width": "375", "height": "812"}),
        ] {
            let rejected = parse_browser_viewport_action(Some(&value));
            assert_eq!(rejected, Err(browser_viewport_action_error()), "{value}");
        }
        assert!(browser_viewport_action_error().contains("browser_action viewport"));
    }

    #[test]
    fn trusted_viewport_action_agrees_with_the_mobile_emulation_threshold() {
        // The pane's mobile preset must land under the width that also switches the user agent,
        // touch points and mouse-to-touch translation; its tablet preset must not.
        assert!(375 < PREVIEW_MOBILE_MAX_WIDTH);
        assert!(768 >= PREVIEW_MOBILE_MAX_WIDTH);
        assert_eq!(preview_viewport_preset("mobile"), Some((375, 812)));
        assert_eq!(preview_viewport_preset("tablet"), Some((768, 1024)));
    }

    #[test]
    fn scale_is_bounded_to_the_documented_range() {
        assert_eq!(validate_preview_scale(None), Ok(None));
        assert_eq!(validate_preview_scale(Some(0.5)), Ok(Some(0.5)));
        assert_eq!(validate_preview_scale(Some(1.0)), Ok(Some(1.0)));
        assert_eq!(
            validate_preview_scale(Some(0.05)),
            Err(
                "scale must be a number in [0.1, 1] \u{2014} e.g. 0.5 for a half-size image".into()
            )
        );
        assert!(validate_preview_scale(Some(1.5)).is_err());
        assert!(validate_preview_scale(Some(f64::INFINITY)).is_err());
    }

    #[test]
    fn capture_extent_never_reports_a_zero_dimension() {
        assert_eq!(preview_capture_extent(1_280.0, 1.0, 1.0), 1_280);
        assert_eq!(preview_capture_extent(1_280.0, 0.625, 1.0), 800);
        assert_eq!(preview_capture_extent(1_280.0, 0.5, 2.0), 1_280);
        assert_eq!(preview_capture_extent(0.5, 1.0, 1.0), 1);
    }

    #[test]
    fn mobile_user_agent_tracks_the_runtime_major_version() {
        assert_eq!(
            chromium_major_version("Chrome/141.0.7390.55"),
            Some("141".to_owned())
        );
        assert_eq!(chromium_major_version("HeadlessChrome/"), None);
        assert_eq!(
            preview_mobile_user_agent("141"),
            "Mozilla/5.0 (Linux; Android 14; Pixel 8) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Mobile Safari/537.36"
        );
        let metadata = preview_mobile_user_agent_metadata("141");
        assert_eq!(metadata["fullVersion"], json!("141.0.0.0"));
        assert_eq!(metadata["mobile"], json!(true));
        assert_eq!(metadata["brands"][0]["version"], json!("141"));
    }

    #[test]
    fn page_script_markers_are_substituted_exactly_once() {
        let selector = js_string_literal("#__MEWORK_VALUE__").expect("literal");
        let value = js_string_literal("kept").expect("literal");
        let rendered = render_page_script(
            PREVIEW_FILL_SCRIPT,
            &[
                ("__MEWORK_SELECTOR__", &selector),
                ("__MEWORK_VALUE__", &value),
            ],
        );
        // The marker inside the selector literal is data, not another substitution site.
        assert!(rendered.contains("document.querySelector(\"#__MEWORK_VALUE__\")"));
        assert!(!rendered.contains("__MEWORK_SELECTOR__"));
        assert_eq!(rendered.matches("\"kept\"").count(), 5);
        assert!(rendered.contains("Object.getOwnPropertyDescriptor(proto, 'value')?.set"));
        assert!(rendered.contains("error: 'Element is not fillable'"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn link_test_directory(target: &Path, link: &Path) -> std::io::Result<()> {
        std::os::unix::fs::symlink(target, link)
    }

    #[cfg(windows)]
    fn link_test_directory(target: &Path, link: &Path) -> std::io::Result<()> {
        std::os::windows::fs::symlink_dir(target, link)
    }

    fn object(value: Value) -> Map<String, Value> {
        value
            .as_object()
            .expect("test input must be an object")
            .clone()
    }

    fn valid_dispatch_input(tool: PreviewTool) -> Map<String, Value> {
        object(match tool {
            PreviewTool::ConsoleLogs => json!({"level":"error", "lines":10}),
            PreviewTool::Screenshot => json!({"scale":0.5}),
            PreviewTool::Snapshot => json!({}),
            PreviewTool::FindElement => json!({"query":"button"}),
            PreviewTool::ClickByDescription => json!({"description":"the save button", "doubleClick":true}),
            PreviewTool::FillByDescription => json!({"description":"the email field", "value":"hello"}),
            PreviewTool::InspectByDescription => json!({"description":"the save button", "styles":["color"]}),
            PreviewTool::Inspect => json!({"selector":"button", "styles":["color"]}),
            PreviewTool::Click => json!({"selector":"button", "doubleClick":true}),
            PreviewTool::Fill => json!({"selector":"input", "value":"hello"}),
            PreviewTool::Eval => json!({"expression":"document.title"}),
            PreviewTool::Network => json!({"filter":"failed"}),
            PreviewTool::Resize => json!({"preset":"mobile"}),
            PreviewTool::UploadImage => json!({"selector":"input"}),
            PreviewTool::Dialog => json!({"accept":true}),
        })
    }

    #[test]
    fn every_preview_page_tool_has_a_runtime_dispatch_arm() {
        let session = BrowserSession::default();

        assert_eq!(PreviewTool::ALL.len(), 15);
        let catalog = crate::catalog::tool_catalog()
            .into_iter()
            .map(|tool| tool.name)
            .collect::<Vec<_>>();
        assert!(
            !catalog.iter().any(|name| name == "playwright"),
            "the multiplexed browser tool is retired"
        );
        for tool in PreviewTool::ALL {
            assert!(
                catalog.iter().any(|name| name == tool.as_str()),
                "{tool} must be a catalog tool"
            );
            assert_eq!(PreviewTool::from_tool_name(tool.as_str()), Some(tool));
            // `preview_upload_image` is the one page tool the executor never reaches without a
            // host-materialized file, so it is given one here rather than being skipped.
            let grants = if tool == PreviewTool::UploadImage {
                BrowserToolGrants {
                    upload_paths: Some(vec![PathBuf::from("image.png")]),
                }
            } else {
                BrowserToolGrants::default()
            };
            if let Err(error) =
                session.execute_tool_blocking(tool, &valid_dispatch_input(tool), &grants)
            {
                assert!(
                    !error.contains("does not support")
                        && !error.contains("is executed by the browser manager"),
                    "{tool} is not dispatched: {error}"
                );
            }
        }
        assert_eq!(PreviewTool::from_tool_name("preview_start"), None);
        assert_eq!(PreviewTool::from_tool_name("playwright"), None);
    }

    #[test]
    fn browser_dispatch_rejects_invalid_arguments_before_webview_access() {
        let session = BrowserSession::default();
        let invalid = [
            (
                PreviewTool::Click,
                json!({}),
                "missing required parameter selector",
            ),
            (
                PreviewTool::Click,
                json!({"selector":"  "}),
                "must not be empty",
            ),
            (
                PreviewTool::Fill,
                json!({"selector":"input"}),
                "missing required parameter value",
            ),
            (
                PreviewTool::Inspect,
                json!({"selector":"button", "styles":"color"}),
                "styles must be an array of strings",
            ),
            (
                PreviewTool::Eval,
                json!({"expression":"  "}),
                "expression must not be empty",
            ),
            (
                PreviewTool::Screenshot,
                json!({"scale":2}),
                "scale must be a number in",
            ),
            (
                PreviewTool::ConsoleLogs,
                json!({"level":"verbose"}),
                "level must be one of",
            ),
            (
                PreviewTool::Network,
                json!({"filter":"slow"}),
                "filter must be one of",
            ),
            (
                PreviewTool::Resize,
                json!({"width":0, "height":720}),
                "preview_resize width must be a number from 1 to",
            ),
            (
                PreviewTool::Dialog,
                json!({"accept":1}),
                "accept must be a boolean",
            ),
            (
                PreviewTool::UploadImage,
                json!({"selector":"input"}),
                "missing a host-materialized image file",
            ),
        ];

        let grants = BrowserToolGrants::default();
        for (tool, input, expected) in invalid {
            let error = session
                .execute_tool_blocking(tool, &object(input), &grants)
                .expect_err("a malformed call must be refused before a page is created");
            assert!(
                error.contains(expected),
                "{tool} returned an unexpected error: {error}"
            );
        }
    }

    /// The dialog and the file chooser each name the one tool that clears them, and every other
    /// preview tool is refused while one is held.
    #[test]
    fn a_held_modal_state_names_the_only_tool_that_clears_it() {
        let dialog = ModalState::Dialog(PendingDialog {
            id: 1,
            kind: "confirm".into(),
            message: "leave?".into(),
            default_value: None,
            url: "https://example.com/".into(),
            opened_at_ms: 0,
        });
        assert_eq!(dialog.cleared_by(), "preview_dialog");
        assert!(dialog.is_cleared_by(PreviewTool::Dialog));
        assert!(!dialog.is_cleared_by(PreviewTool::UploadImage));

        let chooser = ModalState::FileChooser(PendingFileChooser {
            mode: "selectSingle".into(),
            backend_node_id: None,
            opened_at_ms: 0,
        });
        assert_eq!(chooser.cleared_by(), "preview_upload_image");
        assert!(chooser.is_cleared_by(PreviewTool::UploadImage));
        for tool in PreviewTool::ALL {
            if tool != PreviewTool::UploadImage {
                assert!(!chooser.is_cleared_by(tool), "{tool}");
            }
        }
    }

    #[test]
    fn browser_url_accepts_only_safe_navigation_classes() {
        let level = SecurityLevel::RequestApproval;
        assert_eq!(
            parse_browser_url("about:blank", level).unwrap().as_str(),
            "about:blank"
        );
        assert_eq!(
            parse_browser_url("example.com/path?q=1", level)
                .unwrap()
                .as_str(),
            "https://example.com/path?q=1"
        );
        assert!(parse_browser_url("https://example.com/a@b", level).is_ok());
        assert!(parse_browser_url("http://127.0.0.1:8080/", level).is_ok());
        assert_eq!(
            parse_browser_url("localhost:3000/app", level)
                .unwrap()
                .as_str(),
            "http://localhost:3000/app"
        );
        assert_eq!(
            parse_browser_url("example.com:8443/app", level)
                .unwrap()
                .as_str(),
            "https://example.com:8443/app"
        );

        for invalid in [
            "javascript:alert(1)",
            "data:text/html,hi",
            "file:///tmp/a",
            "about:config",
            "https://user@example.com",
            "https://:secret@example.com",
            "https://@example.com",
            "https://",
            "https://exa\0mple.com",
        ] {
            assert!(
                parse_browser_url(invalid, level).is_err(),
                "unexpectedly accepted {invalid:?}"
            );
        }
    }

    /// The local-file preview exists precisely because `file:` and `data:` above stay refused: its
    /// virtual host has to be an ordinary admitted https origin, and it must not be mistaken for
    /// one of the application's own trusted origins, which are refused at every security level.
    #[test]
    fn the_local_file_preview_origin_is_admitted_and_is_not_a_reserved_app_origin() {
        let url = Url::parse(&format!(
            "https://{}/report.pdf",
            browser_file_preview::PREVIEW_VIRTUAL_HOST
        ))
        .unwrap();
        assert!(is_navigation_allowed(&url));
        assert!(!is_reserved_app_origin(&url));
        assert_eq!(
            parse_browser_url(url.as_str(), SecurityLevel::RequestApproval)
                .unwrap()
                .as_str(),
            url.as_str()
        );
    }

    /// Cold close throws away the staged copy, and the virtual-host mapping goes with the WebView2
    /// instance — so the one address a suspended session must *not* come back to is the preview's
    /// own. It is admitted by every gate downstream, which is exactly why nothing else catches it:
    /// the page would resume onto a `.invalid` host serving nothing.
    #[test]
    fn a_cold_closed_file_preview_resumes_blank_rather_than_at_its_dead_virtual_host() {
        let preview_url = format!(
            "https://{}/report.html",
            browser_file_preview::PREVIEW_VIRTUAL_HOST
        );
        let mut state = RuntimeState::default();
        state.status.url = preview_url.clone();
        state.file_preview = Some(FilePreviewGrant {
            directory: PathBuf::from("staging"),
            url: preview_url.clone(),
        });
        assert_eq!(cold_resume_url(&state), DEFAULT_URL);

        // The same address with no grant behind it is somebody else's site that merely shares the
        // name, and a resume target like any other.
        state.file_preview = None;
        assert_eq!(cold_resume_url(&state), preview_url);

        // Every other page still resumes where it was, and a session that never had one comes
        // back blank instead of trying to parse an empty address.
        state.status.url = "https://example.com/app".to_owned();
        assert_eq!(cold_resume_url(&state), "https://example.com/app");
        state.status.url = "   ".to_owned();
        assert_eq!(cold_resume_url(&state), DEFAULT_URL);
    }

    /// The mapped copy is reachable only while it is the committed document; anything else
    /// commits and the grant is handed back for native release.
    #[test]
    fn a_committed_document_from_another_origin_releases_the_file_preview() {
        let staged = FilePreviewGrant {
            directory: PathBuf::from("staging"),
            url: format!(
                "https://{}/report.pdf",
                browser_file_preview::PREVIEW_VIRTUAL_HOST
            ),
        };
        let mut state = RuntimeState {
            file_preview: Some(staged.clone()),
            ..RuntimeState::default()
        };

        assert_eq!(
            take_file_preview_after_committed_url(&mut state, &staged.url),
            None
        );
        assert_eq!(
            take_file_preview_after_committed_url(
                &mut state,
                &format!(
                    "https://{}/other.png",
                    browser_file_preview::PREVIEW_VIRTUAL_HOST
                )
            ),
            None
        );
        assert_eq!(
            take_file_preview_after_committed_url(&mut state, "https://example.com/"),
            Some(staged)
        );
        assert_eq!(state.file_preview, None);
        assert_eq!(
            take_file_preview_after_committed_url(&mut state, "https://example.com/"),
            None
        );
    }

    /// A schemeless local target must not be upgraded to https: the server on the
    /// other end is a development server speaking plain HTTP, so the upgrade fails
    /// the navigation outright instead of loading the page the model asked for.
    #[test]
    fn schemeless_local_targets_keep_plain_http() {
        let level = SecurityLevel::RequestApproval;
        for (input, expected) in [
            ("localhost:3000/app", "http://localhost:3000/app"),
            ("127.0.0.1:8080/", "http://127.0.0.1:8080/"),
            ("[::1]:8080/", "http://[::1]:8080/"),
            ("myapp.localhost:3000/", "http://myapp.localhost:3000/"),
            ("192.168.1.10:8080/", "http://192.168.1.10:8080/"),
            ("10.0.0.5:8080/", "http://10.0.0.5:8080/"),
            ("dev.local:5173/", "http://dev.local:5173/"),
        ] {
            assert_eq!(
                parse_browser_url(input, level).unwrap().as_str(),
                expected,
                "{input} must stay on plain http"
            );
        }
        // A public host still gets the safe guess.
        assert_eq!(
            parse_browser_url("example.com:8443/app", level)
                .unwrap()
                .as_str(),
            "https://example.com:8443/app"
        );
    }

    #[test]
    fn navigation_callback_rejects_userinfo_and_non_web_schemes() {
        assert!(is_navigation_allowed(
            &Url::parse("https://example.com/").unwrap()
        ));
        assert!(!is_navigation_allowed(
            &Url::parse("https://user:pw@example.com/").unwrap()
        ));
        assert!(!is_navigation_allowed(
            &Url::parse("ftp://example.com/").unwrap()
        ));
        assert!(!is_navigation_allowed(&Url::parse("about:srcdoc").unwrap()));
    }

    #[test]
    fn navigation_callback_rejects_application_origins_at_every_level() {
        for blocked in [
            "http://tauri.localhost/",
            "https://tauri.localhost/settings",
            "http://asset.localhost/file",
            "http://ipc.localhost/",
            "http://inner.tauri.localhost/",
        ] {
            for level in [
                SecurityLevel::Plan,
                SecurityLevel::RequestApproval,
                SecurityLevel::AllowEdits,
                SecurityLevel::FullAccess,
            ] {
                assert!(
                    !is_navigation_allowed_at(&Url::parse(blocked).unwrap(), level),
                    "unexpectedly accepted reserved origin {blocked} at {level:?}"
                );
            }
        }
    }

    /// The development-server reservation is the only part of admission the level
    /// moves. While a development server is running its port stays reserved below
    /// full access, and at full access it is just the user's own server.
    #[test]
    fn dev_server_origin_opens_only_under_full_access() {
        let dev_port = Some(1420);
        for origin in [
            "http://localhost:1420/",
            "http://127.0.0.1:1420/",
            "http://[::1]:1420/",
        ] {
            let url = Url::parse(origin).unwrap();
            assert!(
                !is_navigation_allowed_with(&url, SecurityLevel::Plan, dev_port),
                "{origin} must stay reserved below full access"
            );
            assert!(
                !is_navigation_allowed_with(&url, SecurityLevel::RequestApproval, dev_port),
                "{origin} must stay reserved below full access"
            );
            assert!(
                !is_navigation_allowed_with(&url, SecurityLevel::AllowEdits, dev_port),
                "{origin} must stay reserved below full access"
            );
            assert!(
                is_navigation_allowed_with(&url, SecurityLevel::FullAccess, dev_port),
                "{origin} must open under full access"
            );
        }
        // Any other loopback port was never reserved at any level.
        for level in [
            SecurityLevel::Plan,
            SecurityLevel::RequestApproval,
            SecurityLevel::FullAccess,
        ] {
            assert!(is_navigation_allowed_with(
                &Url::parse("http://localhost:3000/").unwrap(),
                level,
                dev_port
            ));
        }
        // Opening a link in the user's own browser is outside the page WebView, so
        // the reservation never applied there.
        assert!(is_navigation_allowed(
            &Url::parse("http://localhost:1420/").unwrap()
        ));
    }

    /// The reserved port follows the development server rather than a constant,
    /// so a run that had to take a different port still protects the frontend and
    /// still leaves the framework's usual port an ordinary user address.
    #[test]
    fn the_reservation_follows_the_port_the_development_server_actually_took() {
        let dev_port = Some(53_117);
        for level in [SecurityLevel::RequestApproval, SecurityLevel::AllowEdits] {
            assert!(
                !is_navigation_allowed_with(
                    &Url::parse("http://127.0.0.1:53117/").unwrap(),
                    level,
                    dev_port
                ),
                "the port in use must be reserved at {level:?}"
            );
            assert!(
                is_navigation_allowed_with(
                    &Url::parse("http://127.0.0.1:1420/").unwrap(),
                    level,
                    dev_port
                ),
                "a port this run never took is the user's own server at {level:?}"
            );
        }
    }

    /// A release build serves the frontend from the custom protocol and installs
    /// no development port, so no loopback address is reserved from the page.
    #[test]
    fn a_build_without_a_development_server_reserves_no_loopback_port() {
        for origin in [
            "http://localhost:1420/",
            "http://127.0.0.1:1420/",
            "http://[::1]:1420/",
        ] {
            let url = Url::parse(origin).unwrap();
            assert!(!is_app_dev_server_origin(&url, None));
            for level in [
                SecurityLevel::Plan,
                SecurityLevel::RequestApproval,
                SecurityLevel::AllowEdits,
                SecurityLevel::FullAccess,
            ] {
                assert!(
                    is_navigation_allowed_with(&url, level, None),
                    "{origin} is an ordinary address without a development server at {level:?}"
                );
            }
        }
    }

    /// The main window recognizes its own frontend under every loopback spelling.
    /// It has to: the configuration names one host, the server answers to all of
    /// them, and a spelling the window fails to recognize is handed to the system
    /// browser instead of being rendered.
    #[test]
    fn the_frontend_origin_is_recognized_under_every_loopback_spelling() {
        let dev_port = Some(1420);
        for origin in [
            "http://localhost:1420/",
            "http://LOCALHOST:1420/",
            "http://127.0.0.1:1420/",
            "http://127.0.0.2:1420/",
            "http://[::1]:1420/",
        ] {
            assert!(
                is_app_dev_server_origin(&Url::parse(origin).unwrap(), dev_port),
                "{origin} names this application's own frontend"
            );
        }
        for other in [
            "http://localhost:1421/",
            "http://example.com:1420/",
            "https://localhost:1420/",
        ] {
            assert!(
                !is_app_dev_server_origin(&Url::parse(other).unwrap(), dev_port),
                "{other} does not name this application's own frontend"
            );
        }
    }

    #[test]
    fn selectors_are_strictly_validated() {
        assert!(validate_target_input(Some("button[data-x='a']"), false).is_ok());
        assert!(validate_target_input(None, true).is_ok());
        assert!(validate_target_input(None, false).is_err());
        assert!(validate_selector("\0").is_err());
        assert!(validate_selector("   ").is_err());
        assert!(validate_selector(&"a".repeat(MAX_SELECTOR_CHARS + 1)).is_err());
    }

    #[test]
    fn javascript_arguments_are_json_escaped_not_interpolated() {
        let hostile = "'); window.pwned=true; //\n\"\\\u{2028}</script>";
        let literal = js_string_literal(hostile).unwrap();
        assert_eq!(serde_json::from_str::<String>(&literal).unwrap(), hostile);
        assert!(!literal.contains("window.pwned=true; //\n"));

        let script = automation_script(&format!("return {};", literal));
        assert!(script.contains("const __state"));
        assert!(script.contains("return {ok:true"));
    }

    #[test]
    fn callback_decoder_handles_direct_and_double_encoded_values() {
        assert_eq!(
            decode_eval_response(r#"{"ok":true,"value":{"a":1}}"#).unwrap(),
            json!({"a":1})
        );
        let double = serde_json::to_string(r#"{"ok":true,"value":"x"}"#).unwrap();
        assert_eq!(decode_eval_response(&double).unwrap(), json!("x"));
        assert!(decode_eval_response(
            r#"{"ok":false,"error":{"name":"TypeError","message":"bad"}}"#
        )
        .unwrap_err()
        .contains("TypeError: bad"));
    }

    #[test]
    fn base64_decoder_and_png_header_validation_are_bounded() {
        assert_eq!(decode_base64("aGVsbG8=").unwrap(), b"hello");
        assert_eq!(decode_base64("aGVsbG8").unwrap(), b"hello");
        assert!(decode_base64("a===").is_err());
        assert!(decode_base64("%%%%").is_err());

        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        png.extend_from_slice(&320_u32.to_be_bytes());
        png.extend_from_slice(&240_u32.to_be_bytes());
        assert_eq!(png_dimensions(&png).unwrap(), (320, 240));
        assert!(png_dimensions(b"not png").is_err());
    }

    #[test]
    fn storage_scope_and_status_follow_toolbar_contract() {
        let status = BrowserStatus::default();
        let value = serde_json::to_value(status).unwrap();
        assert_eq!(value["hasPage"], false);
        assert_eq!(value["url"], "");
        assert!(value.get("error").is_some());
        assert!(value.get("screenshotPath").is_some());
        assert!(value.get("agentActivity").is_none());
        assert!(value.get("credentialProtected").is_none());
        assert_eq!(value["control"]["owner"], "available");
        assert_eq!(value["control"]["handoffRequested"], false);
        assert!(value["control"].get("requestedTool").is_none());
        assert!(value.get("lastError").is_none());
    }

    #[test]
    fn initialization_script_contains_start_page_without_page_owned_agent_marker() {
        for expected in [
            ":root[data-theme=\"night\"]",
            "New tab",
            "新标签页",
            "setUiLanguage",
            "setUiPreferences",
            "nextGeneration < uiPreferenceGeneration",
            "mountStartPage",
            "if (location.href !== \"about:blank\") return uiTheme;",
            "if (location.href !== \"about:blank\") return false;",
            "data-mework-protected-password",
            "Object.freeze(state)",
        ] {
            assert!(
                BROWSER_INITIALIZATION_SCRIPT.contains(expected),
                "initialization script lost required hook: {expected}"
            );
        }
        assert!(!BROWSER_INITIALIZATION_SCRIPT.contains("password-value-must-stay-secret"));
        assert!(!BROWSER_INITIALIZATION_SCRIPT.contains("fillCredential"));
        assert!(!BROWSER_INITIALIZATION_SCRIPT.contains("agentPointer"));
        assert!(!BROWSER_INITIALIZATION_SCRIPT.contains("hideAgentPointer"));
        assert!(!BROWSER_INITIALIZATION_SCRIPT.contains("data-mework-agent-overlay"));
    }

    /// A ref names an element from a snapshot the model already took. Once the page navigates or
    /// re-renders, no amount of polling brings it back, so the probe has to say `fatal` — retrying
    /// spends the whole actionability budget and then blames the element for "never becoming
    /// interactive", which sends the model looking for the wrong problem.
    ///
    /// This branch is injected page JS with no host-side seam, so it is guarded by its text. The
    /// host half — `fatal` short-circuiting the retry loop — lives in `wait_actionable`.
    #[test]
    fn a_stale_snapshot_ref_is_a_fatal_probe_rather_than_a_retry() {
        let fatal_branch = BROWSER_INITIALIZATION_SCRIPT
            .find("if (!element && ref !== null) return {status: \"fatal\"")
            .expect("actionable() must fail fast on a ref whose element is gone");
        let retry_branch = BROWSER_INITIALIZATION_SCRIPT
            .find("if (!element) return {status: \"retry\"")
            .expect("a selector that has not appeared yet is still worth polling for");
        // Order matters: the generic retry would otherwise swallow the stale ref.
        assert!(fatal_branch < retry_branch);
        assert!(BROWSER_INITIALIZATION_SCRIPT
            .contains("not found in the current page snapshot. Try capturing new snapshot."));
    }

    #[test]
    fn start_page_preferences_are_guarded_and_survive_runtime_reset() {
        let script = start_page_preferences_script(Some("night"), Some("zh-CN"), 42);
        assert!(script.contains("if (location.href !== \"about:blank\") return false;"));
        assert!(script.contains("const theme = \"night\";"));
        assert!(script.contains("const language = \"zh-CN\";"));
        assert!(script.contains("state.setUiPreferences(theme, language, 42)"));

        let mut state = RuntimeState {
            ui_theme: Some("night".into()),
            ui_language: Some("zh-CN".into()),
            ui_preferences_generation: 42,
            ..RuntimeState::default()
        };
        reset_closed_state(&mut state);
        assert_eq!(state.ui_theme.as_deref(), Some("night"));
        assert_eq!(state.ui_language.as_deref(), Some("zh-CN"));
        assert_eq!(state.ui_preferences_generation, 42);
    }

    #[test]
    fn panel_bounds_validate_clamp_and_serialize_at_the_trusted_ui_boundary() {
        let hidden = validate_browser_panel_bounds(BrowserPanelBounds {
            x: -200_000.0,
            y: 200_000.0,
            width: 0.0,
            height: 0.0,
            visible: false,
            occluded_top: Some(-20.0),
        })
        .unwrap();
        assert_eq!(hidden.x, -MAX_BROWSER_PANEL_VALUE);
        assert_eq!(hidden.y, MAX_BROWSER_PANEL_VALUE);
        assert_eq!(hidden.occluded_top, Some(0.0));

        let serialized = serde_json::to_value(hidden).unwrap();
        assert_eq!(serialized["visible"], false);
        assert_eq!(serialized["occludedTop"], 0.0);
        assert!(serialized.get("occluded_top").is_none());

        assert!(validate_browser_panel_bounds(BrowserPanelBounds {
            visible: true,
            ..hidden
        })
        .unwrap_err()
        .contains("至少为 1 像素"));
        assert!(validate_browser_panel_bounds(BrowserPanelBounds {
            x: f64::NAN,
            width: 100.0,
            height: 100.0,
            visible: true,
            ..hidden
        })
        .unwrap_err()
        .contains("有限数字"));
    }

    #[test]
    fn main_panel_layout_tracks_measured_bounds_and_never_escapes_the_host() {
        let bounds = BrowserPanelBounds {
            x: -20.0,
            y: 50.0,
            width: 800.0,
            height: 700.0,
            visible: true,
            occluded_top: Some(90.0),
        };
        let layout = browser_layout_for_size(
            LogicalSize::new(600.0, 500.0),
            BrowserHost::MainPanel,
            Some(bounds),
        );
        assert_eq!(
            layout,
            BrowserPageLayout {
                x: 0.0,
                y: 140.0,
                width: 600.0,
                height: 360.0,
            }
        );

        let right_edge = browser_layout_for_size(
            LogicalSize::new(600.0, 500.0),
            BrowserHost::MainPanel,
            Some(BrowserPanelBounds {
                x: 580.0,
                y: 490.0,
                width: 100.0,
                height: 100.0,
                visible: true,
                occluded_top: None,
            }),
        );
        assert_eq!(right_edge.width, 20.0);
        assert_eq!(right_edge.height, 10.0);
    }

    fn cdp_cold_close_cookie(domain: &str, session: bool) -> Value {
        json!({
            "name": if session { "__Host-session" } else { "shared" },
            "value": "test-secret-never-log",
            "domain": domain,
            "path": if session { "/" } else { "/account" },
            "expires": if session { -1.0 } else { 4_102_444_800.0 },
            "size": 32,
            "httpOnly": true,
            "secure": true,
            "session": session,
            "sameSite": "Strict",
            "priority": "High",
            "sourceScheme": "Secure",
            "sourcePort": 443,
            "partitionKeyOpaque": false
        })
    }

    #[test]
    fn cold_close_cookie_handoff_preserves_host_only_and_domain_attributes() {
        let mut domain_cookie = cdp_cold_close_cookie(".example.com", false);
        domain_cookie.as_object_mut().unwrap().insert(
            "partitionKey".into(),
            json!({
                "topLevelSite": "https://top.example",
                "hasCrossSiteAncestor": true
            }),
        );
        let snapshot = parse_cold_close_cookie_snapshot(json!({
            "cookies": [
                cdp_cold_close_cookie("login.example.com", true),
                domain_cookie
            ]
        }))
        .unwrap();
        assert_eq!(snapshot.cookies.len(), 2);

        let host = &snapshot.cookies[0];
        let host_param = cold_close_cookie_param(host, 1_800_000_000.0)
            .unwrap()
            .unwrap();
        assert_eq!(
            host_param.url.as_deref(),
            Some("https://login.example.com:443/")
        );
        assert!(host_param.domain.is_none());
        assert!(host_param.expires.is_none());
        assert!(host_param.http_only);
        assert!(host_param.secure);
        assert!(matches!(host_param.same_site, Some(CookieSameSite::Strict)));
        assert!(matches!(host_param.priority, CookiePriority::High));
        assert!(matches!(
            host_param.source_scheme,
            CookieSourceScheme::Secure
        ));
        assert_eq!(host_param.source_port, 443);
        // Avoid ever including the test secret itself in an assertion failure.
        assert_eq!(host_param.value.len(), 21);

        let domain = &snapshot.cookies[1];
        let domain_param = cold_close_cookie_param(domain, 1_800_000_000.0)
            .unwrap()
            .unwrap();
        assert!(domain_param.url.is_none());
        assert_eq!(domain_param.domain, Some(".example.com"));
        assert_eq!(domain_param.path, "/account");
        assert_eq!(domain_param.expires, Some(4_102_444_800.0));
        let partition = domain_param.partition_key.unwrap();
        assert_eq!(partition.top_level_site, "https://top.example");
        assert!(partition.has_cross_site_ancestor);
    }

    #[test]
    fn cold_close_cookie_handoff_rejects_opaque_or_unknown_semantics() {
        let mut opaque = cdp_cold_close_cookie("login.example.com", true);
        opaque
            .as_object_mut()
            .unwrap()
            .insert("partitionKeyOpaque".into(), Value::Bool(true));
        let error = parse_cold_close_cookie_snapshot(json!({"cookies":[opaque]}))
            .err()
            .unwrap();
        assert!(error.contains("opaque partition key"));

        let mut future = cdp_cold_close_cookie("login.example.com", true);
        future
            .as_object_mut()
            .unwrap()
            .insert("futureSecurityScope".into(), json!("narrow"));
        let error = parse_cold_close_cookie_snapshot(json!({"cookies":[future]}))
            .err()
            .unwrap();
        assert!(error.contains("cannot restore losslessly"));
    }

    #[test]
    fn cold_close_cookie_handoff_is_bounded_and_drops_expired_persistent_entries() {
        let too_many = vec![Value::Null; MAX_COLD_CLOSE_COOKIE_COUNT + 1];
        let error = parse_cold_close_cookie_snapshot(json!({"cookies":too_many}))
            .err()
            .unwrap();
        assert!(error.contains("item limit"));

        let snapshot = parse_cold_close_cookie_snapshot(json!({
            "cookies":[cdp_cold_close_cookie(".example.com", false)]
        }))
        .unwrap();
        assert!(
            cold_close_cookie_param(&snapshot.cookies[0], 4_102_444_801.0)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn cold_close_cookie_match_requires_exact_partition_and_source_scope() {
        let first = parse_cold_close_cookie_snapshot(json!({
            "cookies":[cdp_cold_close_cookie("login.example.com", true)]
        }))
        .unwrap();
        let mut changed = cdp_cold_close_cookie("login.example.com", true);
        changed
            .as_object_mut()
            .unwrap()
            .insert("sourcePort".into(), json!(-1));
        let second = parse_cold_close_cookie_snapshot(json!({"cookies":[changed]})).unwrap();
        assert!(!cold_close_cookie_matches(
            &first.cookies[0],
            &second.cookies[0]
        ));
    }

    #[test]
    fn detached_layout_ignores_sidebar_geometry() {
        let layout = browser_layout_for_size(
            LogicalSize::new(900.0, 640.0),
            BrowserHost::DetachedWindow,
            Some(BrowserPanelBounds {
                x: 700.0,
                y: 100.0,
                width: 120.0,
                height: 200.0,
                visible: false,
                occluded_top: Some(80.0),
            }),
        );
        assert_eq!(
            layout,
            BrowserPageLayout {
                x: 0.0,
                y: 0.0,
                width: 900.0,
                height: 640.0,
            }
        );
    }

    #[test]
    fn tool_session_lookup_never_attempts_to_show_a_page() {
        let runtime = BrowserRuntime::default();
        let session = runtime
            .session("conversation-one")
            .expect("tool lookup should not require an attached UI runtime");

        let status = session.lock_state().status.clone();
        assert!(!status.has_page);
        assert!(!status.open);
    }

    #[test]
    fn closing_a_tab_removes_manager_state_and_reopens_a_fresh_session() {
        let runtime = BrowserRuntime::default();
        let session = runtime
            .session("user-closed-tab")
            .expect("test session should be created");
        {
            let mut state = session.lock_state();
            state.status.has_page = true;
            state.status.open = true;
            state.status.url = "https://example.com/previous".into();
            state.status.title = Some("Previous page".into());
            state.history = vec!["https://example.com/previous".into()];
            state.history_index = Some(0);
            state.pending_navigation = Some(PendingNavigation::Reload);
            state.credential_takeover_grant = Some("https://example.com".into());
        }
        {
            let mut state = lock_unpoison(&runtime.state);
            state.active_session_id = Some("user-closed-tab".into());
            state.live_reservations.insert("user-closed-tab".into());
            touch_manager_state(&mut state, "user-closed-tab");
        }

        let closed = runtime
            .close("user-closed-tab")
            .expect("closing a tab without an attached app should succeed");
        assert_eq!(closed, BrowserStatus::default());
        assert_eq!(runtime.status("user-closed-tab"), BrowserStatus::default());
        {
            let state = lock_unpoison(&runtime.state);
            assert!(!state.sessions.contains_key("user-closed-tab"));
            assert!(state.active_session_id.is_none());
            assert!(!state.live_reservations.contains("user-closed-tab"));
            assert!(!state.last_used.contains_key("user-closed-tab"));
            assert!(state.closed_session_ids.contains("user-closed-tab"));
        }
        {
            let state = session.lock_state();
            assert!(state.terminated);
            assert_eq!(state.status, BrowserStatus::default());
            assert!(state.history.is_empty());
            assert!(state.history_index.is_none());
            assert!(state.pending_navigation.is_none());
            assert!(state.credential_takeover_grant.is_none());
        }

        assert!(
            runtime.session("user-closed-tab").is_err(),
            "late actions must not cross an explicit close fence"
        );
        let open_error = runtime
            .show("user-closed-tab", None)
            .expect_err("the unattached test runtime cannot create a native page");
        assert!(open_error.contains("AppHandle"));
        let reopened = runtime
            .existing_session("user-closed-tab")
            .expect("an explicit trusted open may create a fresh session");
        assert!(!Arc::ptr_eq(&session.state, &reopened.state));
        assert!(!reopened.lock_state().terminated);
        assert_eq!(reopened.status(), BrowserStatus::default());
        let state = lock_unpoison(&runtime.state);
        assert!(state.sessions.contains_key("user-closed-tab"));
        assert!(!state.closed_session_ids.contains("user-closed-tab"));
        assert!(state.active_session_id.is_none());
        assert!(!state.live_reservations.contains("user-closed-tab"));
    }

    #[test]
    fn closing_an_absent_tab_does_not_create_a_session() {
        let runtime = BrowserRuntime::default();

        let closed = runtime
            .close("never-opened-tab")
            .expect("closing an absent tab should be idempotent");

        assert_eq!(closed, BrowserStatus::default());
        assert_eq!(runtime.status("never-opened-tab"), BrowserStatus::default());
        let state = lock_unpoison(&runtime.state);
        assert!(state.sessions.is_empty());
        assert!(state.active_session_id.is_none());
        assert!(state.live_reservations.is_empty());
        assert!(state.last_used.is_empty());
        assert!(state.closed_session_ids.contains("never-opened-tab"));
        drop(state);
        assert!(runtime.session("never-opened-tab").is_err());
    }

    #[test]
    fn lifecycle_epochs_are_exact_positive_javascript_safe_integers() {
        assert_eq!(validate_browser_lifecycle_epoch(1).unwrap(), 1);
        assert_eq!(
            validate_browser_lifecycle_epoch(MAX_BROWSER_LIFECYCLE_EPOCH).unwrap(),
            MAX_BROWSER_LIFECYCLE_EPOCH
        );
        assert!(validate_browser_lifecycle_epoch(0).is_err());
        assert!(validate_browser_lifecycle_epoch(MAX_BROWSER_LIFECYCLE_EPOCH + 1).is_err());
    }

    #[test]
    fn absent_newer_close_fences_a_late_older_open_without_allocating() {
        let runtime = BrowserRuntime::default();
        runtime
            .close_with_intent("closed-before-stale-open", 2)
            .expect("a newer close of an absent session should publish its fence");

        let error = runtime
            .show_with_intent("closed-before-stale-open", None, 1)
            .expect_err("an older open must not cross the newer close");
        assert_eq!(error, STALE_BROWSER_LIFECYCLE_INTENT_ERROR);

        let state = lock_unpoison(&runtime.state);
        assert!(state.sessions.is_empty());
        assert!(state
            .closed_session_ids
            .contains("closed-before-stale-open"));
        assert_eq!(
            state
                .lifecycle_intents
                .get("closed-before-stale-open")
                .copied(),
            Some(BrowserSessionLifecycleIntent {
                epoch: 2,
                desired: BrowserSessionLifecycleDesired::Closed,
            })
        );
    }

    #[test]
    fn shutdown_clears_session_intents_and_rejects_later_lifecycle_requests() {
        let runtime = BrowserRuntime::default();
        runtime
            .close_with_intent("shutdown-intent", 4)
            .expect("the pre-shutdown close should publish an intent");
        assert!(lock_unpoison(&runtime.state)
            .lifecycle_intents
            .contains_key("shutdown-intent"));

        runtime.shutdown_all();

        let state = lock_unpoison(&runtime.state);
        assert!(state.shutting_down);
        assert!(state.lifecycle_intents.is_empty());
        assert!(state.closed_session_ids.is_empty());
        drop(state);
        assert!(runtime
            .show_with_intent("shutdown-intent", None, 5)
            .is_err());
        assert!(runtime.close_with_intent("shutdown-intent", 5).is_err());
    }

    #[test]
    fn close_intent_fence_calls_secondary_authority_only_after_acceptance() {
        let runtime = BrowserRuntime::default();
        let accepted_calls = std::cell::Cell::new(0usize);
        let first = runtime
            .with_close_intent_fence("fenced-close", 8, || {
                assert!(matches!(
                    runtime.lifecycle.try_lock(),
                    Err(TryLockError::WouldBlock)
                ));
                accepted_calls.set(accepted_calls.get() + 1);
                Ok("first-guard")
            })
            .expect("a current Close should acquire the secondary fence");
        assert_eq!(first, "first-guard");
        assert_eq!(accepted_calls.get(), 1);
        let repeated = runtime
            .with_close_intent_fence("fenced-close", 8, || {
                accepted_calls.set(accepted_calls.get() + 1);
                Ok("retry-guard")
            })
            .expect("the same Closed epoch should reacquire its secondary fence");
        assert_eq!(repeated, "retry-guard");
        assert_eq!(accepted_calls.get(), 2);

        let newer_open = BrowserRuntime::default();
        let open_error = newer_open
            .show_with_intent("reject-fenced-close", None, 12)
            .expect_err("an unattached test runtime cannot create a native page");
        assert!(open_error.contains("AppHandle"));
        let rejected_calls = std::cell::Cell::new(0usize);
        let stale = newer_open
            .with_close_intent_fence("reject-fenced-close", 11, || {
                rejected_calls.set(rejected_calls.get() + 1);
                Ok(())
            })
            .expect_err("an older Close must be rejected before touching secondary authority");
        assert_eq!(stale, STALE_BROWSER_LIFECYCLE_INTENT_ERROR);
        let collision = newer_open
            .with_close_intent_fence("reject-fenced-close", 12, || {
                rejected_calls.set(rejected_calls.get() + 1);
                Ok(())
            })
            .expect_err("the same Open epoch cannot be reused for a Close fence");
        assert_eq!(collision, COLLIDING_BROWSER_LIFECYCLE_INTENT_ERROR);
        assert_eq!(rejected_calls.get(), 0);
        let state = lock_unpoison(&newer_open.state);
        assert!(!state.closed_session_ids.contains("reject-fenced-close"));
        assert_eq!(
            state.lifecycle_intents.get("reject-fenced-close").copied(),
            Some(BrowserSessionLifecycleIntent {
                epoch: 12,
                desired: BrowserSessionLifecycleDesired::Open,
            })
        );
    }

    #[test]
    fn failed_secondary_fence_restores_pending_layout_and_absent_intent_exactly() {
        let runtime = BrowserRuntime::default();
        let pending = BrowserPanelBounds {
            x: 12.0,
            y: 34.0,
            width: 640.0,
            height: 480.0,
            visible: true,
            occluded_top: Some(44.0),
        };
        runtime
            .set_panel_bounds("secondary-fence-rollback", pending)
            .expect("pre-open geometry should stay manager-side");

        let error = runtime
            .with_close_intent_fence("secondary-fence-rollback", 9, || -> Result<(), String> {
                Err("synthetic secondary registry conflict".into())
            })
            .expect_err("secondary fence acquisition should fail");
        assert_eq!(error, "synthetic secondary registry conflict");

        let state = lock_unpoison(&runtime.state);
        assert!(!state
            .lifecycle_intents
            .contains_key("secondary-fence-rollback"));
        assert!(!state
            .closed_session_ids
            .contains("secondary-fence-rollback"));
        assert_eq!(
            state
                .pending_panel_bounds
                .get("secondary-fence-rollback")
                .copied(),
            Some(PendingBrowserPanelBounds {
                epoch: 1,
                bounds: pending,
            })
        );
        assert!(state.sessions.is_empty());
    }

    #[test]
    fn failed_secondary_registry_fence_rolls_back_before_a_newer_close_can_publish() {
        let runtime = BrowserRuntime::default();
        let open_error = runtime
            .show_with_intent("two-registry-close", None, 20)
            .expect_err("an unattached test runtime cannot create a native page");
        assert!(open_error.contains("AppHandle"));
        let session = runtime
            .existing_session("two-registry-close")
            .expect("the accepted Open should own one exact session");
        let secondary_registry = Arc::new(Mutex::new(Some(19u64)));
        let (first_entered_tx, first_entered_rx) = mpsc::channel();
        let (release_first_tx, release_first_rx) = mpsc::channel();
        let first_runtime = runtime.clone();
        let first_secondary = secondary_registry.clone();
        let first = std::thread::spawn(move || {
            first_runtime.with_close_intent_fence(
                "two-registry-close",
                21,
                || -> Result<(), String> {
                    assert_eq!(*lock_unpoison(&first_secondary), Some(19));
                    first_entered_tx.send(()).unwrap();
                    release_first_rx.recv().unwrap();
                    Err("SessionAlreadyClosing".into())
                },
            )
        });
        first_entered_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("first Close did not enter its secondary registry fence");
        {
            let state = lock_unpoison(&runtime.state);
            assert_eq!(
                state.lifecycle_intents.get("two-registry-close").copied(),
                Some(BrowserSessionLifecycleIntent {
                    epoch: 21,
                    desired: BrowserSessionLifecycleDesired::Closed,
                })
            );
            assert!(state.closed_session_ids.contains("two-registry-close"));
        }

        *lock_unpoison(&secondary_registry) = None;
        let (second_attempted_tx, second_attempted_rx) = mpsc::channel();
        let (second_entered_tx, second_entered_rx) = mpsc::channel();
        let second_runtime = runtime.clone();
        let second_secondary = secondary_registry.clone();
        let second = std::thread::spawn(move || {
            second_attempted_tx.send(()).unwrap();
            second_runtime.with_close_intent_fence(
                "two-registry-close",
                22,
                || -> Result<(), String> {
                    let mut registry = lock_unpoison(&second_secondary);
                    assert!(registry.is_none());
                    *registry = Some(22);
                    second_entered_tx.send(()).unwrap();
                    Ok(())
                },
            )
        });
        second_attempted_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("newer Close did not attempt the browser lifecycle fence");
        assert!(
            second_entered_rx
                .recv_timeout(Duration::from_millis(75))
                .is_err(),
            "newer Close reached secondary authority before the failed older fence rolled back"
        );

        release_first_tx.send(()).unwrap();
        assert_eq!(
            first.join().expect("first close worker should not panic"),
            Err("SessionAlreadyClosing".into())
        );
        second_entered_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("newer Close did not proceed after rollback");
        second
            .join()
            .expect("newer close worker should not panic")
            .expect("newer Close should acquire both lifecycle authorities");

        let state = lock_unpoison(&runtime.state);
        assert_eq!(
            state.lifecycle_intents.get("two-registry-close").copied(),
            Some(BrowserSessionLifecycleIntent {
                epoch: 22,
                desired: BrowserSessionLifecycleDesired::Closed,
            })
        );
        assert!(state.closed_session_ids.contains("two-registry-close"));
        let current = state
            .sessions
            .get("two-registry-close")
            .expect("fencing alone must not destroy the native session");
        assert!(Arc::ptr_eq(&current.state, &session.state));
        drop(state);

        runtime
            .close_with_intent("two-registry-close", 22)
            .expect("the newest accepted Close should finish exact cleanup");
        assert!(session.lock_state().terminated);
        assert!(!lock_unpoison(&runtime.state)
            .sessions
            .contains_key("two-registry-close"));
    }

    #[test]
    fn same_epoch_is_idempotent_only_for_the_same_desired_state() {
        let runtime = BrowserRuntime::default();
        runtime
            .close_with_intent("same-close", 7)
            .expect("first close should publish");
        runtime
            .close_with_intent("same-close", 7)
            .expect("same Closed epoch should be an idempotent cleanup retry");
        assert_eq!(
            runtime
                .show_with_intent("same-close", None, 7)
                .expect_err("the same epoch cannot mean Open and Closed"),
            COLLIDING_BROWSER_LIFECYCLE_INTENT_ERROR
        );

        let open_runtime = BrowserRuntime::default();
        let first_error = open_runtime
            .show_with_intent("same-open", None, 11)
            .expect_err("an unattached test runtime cannot create a native page");
        assert!(first_error.contains("AppHandle"));
        let first = open_runtime
            .existing_session("same-open")
            .expect("the accepted Open intent should own one manager session");
        let expected_status = first.status();
        let retry_status = open_runtime
            .show_with_intent("same-open", None, 11)
            .expect("duplicate delivery of the same Open intent should be observational");
        assert_eq!(retry_status, expected_status);
        let retry = open_runtime
            .existing_session("same-open")
            .expect("same Open epoch must reuse the exact session");
        assert!(Arc::ptr_eq(&first.state, &retry.state));
        assert_eq!(
            open_runtime
                .close_with_intent("same-open", 11)
                .expect_err("the same epoch cannot be reused for Closed"),
            COLLIDING_BROWSER_LIFECYCLE_INTENT_ERROR
        );
        assert!(!first.lock_state().terminated);
    }

    #[test]
    fn same_hidden_epoch_retries_native_hide_as_a_compensation() {
        let runtime = BrowserRuntime::default();
        let open_error = runtime
            .show_with_intent("same-hidden", None, 1)
            .expect_err("an unattached test runtime cannot create a native page");
        assert!(open_error.contains("AppHandle"));
        let session = runtime
            .existing_session("same-hidden")
            .expect("the accepted Open should retain its manager session");
        let attempts_before_hidden = session.lock_state().hide_attempts;

        let first_hide = runtime
            .hide_with_intent("same-hidden", 2)
            .expect_err("the test session has no AppHandle");
        assert!(first_hide.contains("AppHandle"));
        let retry_hide = runtime
            .hide_with_intent("same-hidden", 2)
            .expect_err("same Hidden must retry the native compensation");
        assert!(retry_hide.contains("AppHandle"));

        assert_eq!(
            session.lock_state().hide_attempts,
            attempts_before_hidden + 2
        );
        let state = lock_unpoison(&runtime.state);
        assert_eq!(
            state.lifecycle_intents.get("same-hidden").copied(),
            Some(BrowserSessionLifecycleIntent {
                epoch: 2,
                desired: BrowserSessionLifecycleDesired::Hidden,
            })
        );
        assert!(!state.closed_session_ids.contains("same-hidden"));
    }

    /// The renderer paints its captured still frame for exactly as long as the host says the page
    /// is covered, so a park that quietly sank the page without clearing the flag would leave the
    /// pane showing a photograph of a page that is no longer there.
    #[test]
    fn hiding_a_surface_reports_that_the_page_is_no_longer_covered() {
        let runtime = BrowserRuntime::default();
        let session = runtime.session("occlusion-record").unwrap();
        {
            let mut state = session.lock_state();
            state.synthetic_surface = true;
            state.host = Some(BrowserHost::MainPanel);
            state.status.has_page = true;
            state.status.open = true;
        }
        assert!(session
            .set_occluded(true)
            .expect("a synthetic surface records occlusion without a native page")
            .occluded);

        let status = session.hide().expect("a synthetic surface parks in place");

        assert!(!status.occluded);
        assert!(!session.lock_state().occluded);
    }

    #[test]
    fn panel_visibility_must_match_the_exact_open_or_hidden_intent() {
        let runtime = BrowserRuntime::default();
        let open_error = runtime
            .show_with_intent("layout-visibility", None, 10)
            .expect_err("an unattached test runtime cannot create a native page");
        assert!(open_error.contains("AppHandle"));
        let session = runtime
            .existing_session("layout-visibility")
            .expect("the Open intent should retain its exact session");
        let hidden_bounds = BrowserPanelBounds {
            x: 10.0,
            y: 20.0,
            width: 640.0,
            height: 480.0,
            visible: false,
            occluded_top: Some(44.0),
        };

        assert_eq!(
            runtime
                .set_panel_bounds_with_intent("layout-visibility", hidden_bounds, 10)
                .expect_err("visible=false cannot bypass an Open lifecycle intent"),
            MISMATCHED_BROWSER_PANEL_VISIBILITY_ERROR
        );
        assert!(session.lock_state().panel_bounds.is_none());

        let hide_error = runtime
            .hide_with_intent("layout-visibility", 11)
            .expect_err("the test session has no AppHandle");
        assert!(hide_error.contains("AppHandle"));
        let status = runtime
            .set_panel_bounds_with_intent("layout-visibility", hidden_bounds, 11)
            .expect("visible=false should be accepted for exact Hidden");
        assert!(!status.open);
        assert_eq!(session.lock_state().panel_bounds, Some(hidden_bounds));

        let visible_bounds = BrowserPanelBounds {
            visible: true,
            ..hidden_bounds
        };
        assert_eq!(
            runtime
                .set_panel_bounds_with_intent("layout-visibility", visible_bounds, 11)
                .expect_err("visible=true cannot bypass exact Hidden"),
            MISMATCHED_BROWSER_PANEL_VISIBILITY_ERROR
        );
        assert_eq!(session.lock_state().panel_bounds, Some(hidden_bounds));
    }

    #[test]
    fn stale_future_and_closed_layout_generations_fail_closed() {
        let runtime = BrowserRuntime::default();
        let open_error = runtime
            .show_with_intent("layout-generation", None, 20)
            .expect_err("an unattached test runtime cannot create a native page");
        assert!(open_error.contains("AppHandle"));
        let bounds = BrowserPanelBounds {
            x: 1.0,
            y: 2.0,
            width: 600.0,
            height: 400.0,
            visible: true,
            occluded_top: Some(44.0),
        };

        assert_eq!(
            runtime
                .set_panel_bounds_with_intent("layout-generation", bounds, 19)
                .expect_err("an older layout must be rejected"),
            STALE_BROWSER_LIFECYCLE_INTENT_ERROR
        );
        assert_eq!(
            runtime
                .set_panel_bounds_with_intent("layout-generation", bounds, 21)
                .expect_err("future layout cannot advance an Open lifecycle"),
            FUTURE_BROWSER_PANEL_BOUNDS_ERROR
        );
        runtime
            .close_with_intent("layout-generation", 22)
            .expect("newer Close should remove the session");
        assert_eq!(
            runtime
                .set_panel_bounds_with_intent("layout-generation", bounds, 23)
                .expect_err("future layout cannot cross a Closed lifecycle"),
            FUTURE_BROWSER_PANEL_BOUNDS_ERROR
        );
        let state = lock_unpoison(&runtime.state);
        assert!(state.sessions.is_empty());
        assert!(state.pending_panel_bounds.is_empty());
        assert!(state.closed_session_ids.contains("layout-generation"));
    }

    #[test]
    fn preopen_panel_bounds_are_consumed_only_by_the_exact_open_epoch() {
        let exact = BrowserRuntime::default();
        let bounds = BrowserPanelBounds {
            x: 12.0,
            y: 34.0,
            width: 700.0,
            height: 500.0,
            visible: true,
            occluded_top: Some(48.0),
        };
        exact
            .set_panel_bounds_with_intent("exact-pending-layout", bounds, 30)
            .expect("layout may arrive before its matching Open");
        assert_eq!(
            lock_unpoison(&exact.state)
                .pending_panel_bounds
                .get("exact-pending-layout")
                .copied(),
            Some(PendingBrowserPanelBounds { epoch: 30, bounds })
        );
        let exact_open = exact
            .show_with_intent("exact-pending-layout", None, 30)
            .expect_err("the test runtime has no AppHandle");
        assert!(exact_open.contains("AppHandle"));
        let exact_session = exact
            .existing_session("exact-pending-layout")
            .expect("matching Open should allocate its exact session");
        assert_eq!(exact_session.lock_state().panel_bounds, Some(bounds));
        assert!(lock_unpoison(&exact.state).pending_panel_bounds.is_empty());

        let unmatched = BrowserRuntime::default();
        unmatched
            .set_panel_bounds_with_intent("unmatched-pending-layout", bounds, 40)
            .expect("future pre-open geometry may be retained without authority");
        let unmatched_open = unmatched
            .show_with_intent("unmatched-pending-layout", None, 39)
            .expect_err("the test runtime has no AppHandle");
        assert!(unmatched_open.contains("AppHandle"));
        let unmatched_session = unmatched
            .existing_session("unmatched-pending-layout")
            .expect("Open should still allocate its own session");
        assert!(unmatched_session.lock_state().panel_bounds.is_none());
        assert!(lock_unpoison(&unmatched.state)
            .pending_panel_bounds
            .is_empty());
    }

    #[test]
    fn future_visible_layout_waits_for_restore_open_and_survives_failed_close_fence() {
        let runtime = BrowserRuntime::default();
        let open_error = runtime
            .show_with_intent("hidden-restore-layout", None, 50)
            .expect_err("an unattached test runtime cannot create a native page");
        assert!(open_error.contains("AppHandle"));
        let session = runtime
            .existing_session("hidden-restore-layout")
            .expect("the Open intent should retain its exact session");
        let hide_error = runtime
            .hide_with_intent("hidden-restore-layout", 51)
            .expect_err("the test session has no AppHandle");
        assert!(hide_error.contains("AppHandle"));
        let future_bounds = BrowserPanelBounds {
            x: 22.0,
            y: 33.0,
            width: 720.0,
            height: 520.0,
            visible: true,
            occluded_top: Some(44.0),
        };

        let status = runtime
            .set_panel_bounds_with_intent("hidden-restore-layout", future_bounds, 52)
            .expect("child layout may beat its matching restore Open");
        assert!(!status.open);
        assert!(session.lock_state().panel_bounds.is_none());
        {
            let state = lock_unpoison(&runtime.state);
            assert_eq!(
                state
                    .lifecycle_intents
                    .get("hidden-restore-layout")
                    .copied(),
                Some(BrowserSessionLifecycleIntent {
                    epoch: 51,
                    desired: BrowserSessionLifecycleDesired::Hidden,
                })
            );
            assert_eq!(
                state
                    .pending_panel_bounds
                    .get("hidden-restore-layout")
                    .copied(),
                Some(PendingBrowserPanelBounds {
                    epoch: 52,
                    bounds: future_bounds,
                })
            );
        }

        let close_error = runtime
            .with_close_intent_fence("hidden-restore-layout", 53, || -> Result<(), String> {
                Err("secondary close registry unavailable".into())
            })
            .expect_err("failed secondary fence should roll back the browser lifecycle");
        assert_eq!(close_error, "secondary close registry unavailable");
        {
            let state = lock_unpoison(&runtime.state);
            assert_eq!(
                state
                    .lifecycle_intents
                    .get("hidden-restore-layout")
                    .copied(),
                Some(BrowserSessionLifecycleIntent {
                    epoch: 51,
                    desired: BrowserSessionLifecycleDesired::Hidden,
                })
            );
            assert!(!state.closed_session_ids.contains("hidden-restore-layout"));
            assert_eq!(
                state
                    .pending_panel_bounds
                    .get("hidden-restore-layout")
                    .copied(),
                Some(PendingBrowserPanelBounds {
                    epoch: 52,
                    bounds: future_bounds,
                })
            );
        }

        let restore_error = runtime
            .show_with_intent("hidden-restore-layout", None, 52)
            .expect_err("geometry is applied before the test runtime reports missing AppHandle");
        assert!(restore_error.contains("AppHandle"));
        assert_eq!(session.lock_state().panel_bounds, Some(future_bounds));
        let state = lock_unpoison(&runtime.state);
        assert_eq!(
            state
                .lifecycle_intents
                .get("hidden-restore-layout")
                .copied(),
            Some(BrowserSessionLifecycleIntent {
                epoch: 52,
                desired: BrowserSessionLifecycleDesired::Open,
            })
        );
        assert!(state.pending_panel_bounds.is_empty());
    }

    #[test]
    fn stale_close_never_tombstones_or_destroys_a_newer_open_session() {
        let runtime = BrowserRuntime::default();
        let open_error = runtime
            .show_with_intent("newer-open", None, 2)
            .expect_err("an unattached test runtime cannot create a native page");
        assert!(open_error.contains("AppHandle"));
        let session = runtime
            .existing_session("newer-open")
            .expect("the newer Open intent should retain its exact manager session");

        let close_error = runtime
            .close_with_intent("newer-open", 1)
            .expect_err("an older close must be rejected before publishing a tombstone");
        assert_eq!(close_error, STALE_BROWSER_LIFECYCLE_INTENT_ERROR);

        let state = lock_unpoison(&runtime.state);
        let retained = state
            .sessions
            .get("newer-open")
            .expect("the newer session must remain");
        assert!(Arc::ptr_eq(&retained.state, &session.state));
        assert!(!state.closed_session_ids.contains("newer-open"));
        assert_eq!(
            state.lifecycle_intents.get("newer-open").copied(),
            Some(BrowserSessionLifecycleIntent {
                epoch: 2,
                desired: BrowserSessionLifecycleDesired::Open,
            })
        );
        drop(state);
        assert!(!session.lock_state().terminated);
    }

    #[test]
    fn a_newer_close_after_an_accepted_open_leaves_the_native_authority_closed() {
        let runtime = BrowserRuntime::default();
        let open_error = runtime
            .show_with_intent("open-then-close", None, 1)
            .expect_err("an unattached test runtime cannot create a native page");
        assert!(open_error.contains("AppHandle"));
        let opened = runtime
            .existing_session("open-then-close")
            .expect("the Open intent should allocate its manager handle");

        runtime
            .close_with_intent("open-then-close", 2)
            .expect("the newer close should destroy the previously accepted session");

        let state = lock_unpoison(&runtime.state);
        assert!(!state.sessions.contains_key("open-then-close"));
        assert!(state.closed_session_ids.contains("open-then-close"));
        assert_eq!(
            state.lifecycle_intents.get("open-then-close").copied(),
            Some(BrowserSessionLifecycleIntent {
                epoch: 2,
                desired: BrowserSessionLifecycleDesired::Closed,
            })
        );
        drop(state);
        assert!(opened.lock_state().terminated);
    }

    #[test]
    fn newer_open_supersedes_a_close_waiting_for_an_atomic_agent_action() {
        let runtime = BrowserRuntime::default();
        let session = runtime
            .session("close-waits-for-agent")
            .expect("test session should be created");
        let automation = lock_unpoison(&session.automation);
        let closer_runtime = runtime.clone();
        let closer = std::thread::spawn(move || {
            closer_runtime.close_with_intent("close-waits-for-agent", 2)
        });

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let published = lock_unpoison(&runtime.state)
                .lifecycle_intents
                .get("close-waits-for-agent")
                .copied()
                == Some(BrowserSessionLifecycleIntent {
                    epoch: 2,
                    desired: BrowserSessionLifecycleDesired::Closed,
                });
            if published {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "close did not publish its fence before waiting for automation"
            );
            std::thread::yield_now();
        }

        let open_error = runtime
            .show_with_intent("close-waits-for-agent", None, 3)
            .expect_err("the test runtime still has no AppHandle");
        assert!(open_error.contains("AppHandle"));
        drop(automation);
        closer
            .join()
            .expect("close worker should not panic")
            .expect("the superseded close should become a harmless no-op");

        let state = lock_unpoison(&runtime.state);
        let current = state
            .sessions
            .get("close-waits-for-agent")
            .expect("the newer Open must retain the exact session");
        assert!(Arc::ptr_eq(&current.state, &session.state));
        assert!(!state.closed_session_ids.contains("close-waits-for-agent"));
        assert_eq!(
            state
                .lifecycle_intents
                .get("close-waits-for-agent")
                .copied(),
            Some(BrowserSessionLifecycleIntent {
                epoch: 3,
                desired: BrowserSessionLifecycleDesired::Open,
            })
        );
        drop(state);
        assert!(!session.lock_state().terminated);
    }

    #[test]
    fn late_panel_bounds_after_close_do_not_recreate_a_session() {
        let runtime = BrowserRuntime::default();
        runtime
            .close("closed-before-layout")
            .expect("an absent close should publish its fence");

        let status = runtime
            .set_panel_bounds(
                "closed-before-layout",
                BrowserPanelBounds {
                    x: 10.0,
                    y: 20.0,
                    width: 640.0,
                    height: 480.0,
                    visible: true,
                    occluded_top: Some(44.0),
                },
            )
            .expect("a stale layout is an idempotent no-op");

        assert_eq!(status, BrowserStatus::default());
        let state = lock_unpoison(&runtime.state);
        assert!(state.sessions.is_empty());
        assert!(state.pending_panel_bounds.is_empty());
        assert!(state.closed_session_ids.contains("closed-before-layout"));
    }

    #[test]
    fn panel_bounds_wait_manager_side_for_the_first_explicit_open() {
        let runtime = BrowserRuntime::default();
        let bounds = BrowserPanelBounds {
            x: 12.0,
            y: 34.0,
            width: 700.0,
            height: 500.0,
            visible: true,
            occluded_top: Some(48.0),
        };
        let status = runtime
            .set_panel_bounds("layout-before-open", bounds)
            .expect("pre-open geometry should be accepted");
        assert_eq!(status, BrowserStatus::default());
        assert!(lock_unpoison(&runtime.state).sessions.is_empty());

        let error = runtime
            .show("layout-before-open", None)
            .expect_err("the unattached test runtime cannot create a native page");
        assert!(error.contains("AppHandle"));
        let session = runtime
            .existing_session("layout-before-open")
            .expect("the explicit open should allocate exactly one session");
        assert_eq!(session.lock_state().panel_bounds, Some(bounds));
        assert!(lock_unpoison(&runtime.state)
            .pending_panel_bounds
            .is_empty());
    }

    #[test]
    fn renderer_presentation_fail_safe_hides_all_eligible_main_panel_surfaces_only() {
        let runtime = BrowserRuntime::default();
        let unowned = runtime.session("renderer-unowned").unwrap();
        let owned = runtime.session("renderer-owned").unwrap();
        let newer = runtime.session("renderer-newer").unwrap();
        let detached = runtime.session("renderer-detached").unwrap();

        for (session, host, generation) in [
            (&unowned, BrowserHost::MainPanel, None),
            (&owned, BrowserHost::MainPanel, Some(4)),
            (&newer, BrowserHost::MainPanel, Some(5)),
            (&detached, BrowserHost::DetachedWindow, Some(2)),
        ] {
            let mut state = session.lock_state();
            state.synthetic_surface = true;
            state.host = Some(host);
            state.status.has_page = true;
            state.status.open = true;
            state.renderer_presentation_generation = generation;
            state.occluded = true;
            state.status.occluded = true;
        }
        let (lifecycle_before, closed_before, pending_before) = {
            let mut state = lock_unpoison(&runtime.state);
            state.active_session_id = Some("renderer-owned".to_owned());
            state.lifecycle_intents.insert(
                "renderer-unowned".to_owned(),
                BrowserSessionLifecycleIntent {
                    epoch: 30,
                    desired: BrowserSessionLifecycleDesired::Open,
                },
            );
            state.lifecycle_intents.insert(
                "renderer-owned".to_owned(),
                BrowserSessionLifecycleIntent {
                    epoch: 31,
                    desired: BrowserSessionLifecycleDesired::Hidden,
                },
            );
            state.lifecycle_intents.insert(
                "renderer-newer".to_owned(),
                BrowserSessionLifecycleIntent {
                    epoch: 32,
                    desired: BrowserSessionLifecycleDesired::Closed,
                },
            );
            state.closed_session_ids.insert("renderer-newer".to_owned());
            state.pending_panel_bounds.insert(
                "renderer-unowned".to_owned(),
                PendingBrowserPanelBounds {
                    epoch: 33,
                    bounds: BrowserPanelBounds {
                        x: 2.0,
                        y: 3.0,
                        width: 500.0,
                        height: 400.0,
                        visible: true,
                        occluded_top: None,
                    },
                },
            );
            (
                state.lifecycle_intents.clone(),
                state.closed_session_ids.clone(),
                state.pending_panel_bounds.clone(),
            )
        };

        assert_eq!(
            runtime
                .fail_safe_hide_renderer_presentations_through(4)
                .unwrap(),
            2
        );
        assert!(!unowned.status().open);
        assert!(!owned.status().open);
        assert!(newer.status().open);
        assert!(detached.status().open);
        assert!(!unowned.lock_state().occluded);
        assert!(!unowned.status().occluded);
        assert!(unowned.agent_pointer_generation.load(Ordering::Acquire) > 0);

        let state = lock_unpoison(&runtime.state);
        assert_eq!(state.lifecycle_intents, lifecycle_before);
        assert_eq!(state.closed_session_ids, closed_before);
        assert_eq!(state.pending_panel_bounds, pending_before);
        assert!(state.active_session_id.is_none());
    }

    #[test]
    fn delayed_orphan_cleanup_cannot_hide_a_newer_renderer_presentation() {
        let runtime = BrowserRuntime::default();
        let session = runtime.session("renderer-generation-fence").unwrap();
        {
            let mut state = session.lock_state();
            state.synthetic_surface = true;
            state.host = Some(BrowserHost::MainPanel);
            state.status.has_page = true;
            state.status.open = true;
            state.renderer_presentation_generation = Some(8);
        }
        lock_unpoison(&runtime.state).active_session_id =
            Some("renderer-generation-fence".to_owned());

        assert_eq!(
            runtime
                .fail_safe_hide_renderer_presentations_through(7)
                .unwrap(),
            0
        );
        assert!(session.status().open);
        assert_eq!(session.lock_state().hide_attempts, 0);
        assert_eq!(
            lock_unpoison(&runtime.state).active_session_id.as_deref(),
            Some("renderer-generation-fence")
        );

        assert_eq!(
            runtime
                .fail_safe_hide_renderer_presentations_through(8)
                .unwrap(),
            1
        );
        assert!(!session.status().open);
        assert_eq!(session.lock_state().hide_attempts, 1);
    }

    #[test]
    fn structured_stale_close_is_rejected_before_any_fence_or_native_side_effect() {
        let runtime = BrowserRuntime::default();
        let open_error = runtime
            .show_with_intent("structured-stale-close", None, 20)
            .expect_err("the test runtime has no AppHandle");
        assert!(open_error.contains("AppHandle"));
        let session = runtime
            .existing_session("structured-stale-close")
            .expect("the newer Open should own the exact session");
        let fence_called = std::sync::atomic::AtomicBool::new(false);

        let error = runtime
            .with_close_intent_fence("structured-stale-close", 19, || {
                fence_called.store(true, Ordering::SeqCst);
                Ok::<_, String>(())
            })
            .expect_err("a stale close must fail before acquiring the secondary fence");
        let disposition = BrowserCloseDisposition::rejected_lifecycle(&error);

        assert_eq!(disposition.status, BrowserCloseStatus::Rejected);
        assert!(!disposition.intent_accepted);
        assert!(!disposition.cleanup_complete);
        assert!(!disposition.surface_hidden);
        assert_eq!(
            disposition.error_code,
            Some(BrowserCloseErrorCode::StaleIntent)
        );
        assert!(!fence_called.load(Ordering::SeqCst));
        let state = lock_unpoison(&runtime.state);
        assert!(!state.closed_session_ids.contains("structured-stale-close"));
        assert!(state
            .sessions
            .get("structured-stale-close")
            .is_some_and(|current| Arc::ptr_eq(&current.state, &session.state)));
        drop(state);
        assert!(!session.lock_state().terminated);
    }

    #[test]
    fn structured_close_successfully_completes_native_cleanup() {
        let runtime = BrowserRuntime::default();
        let session = runtime
            .session("structured-close-success")
            .expect("test session should be created");
        runtime
            .with_close_intent_fence("structured-close-success", 31, || Ok::<_, String>(()))
            .expect("close fence should be accepted");

        let disposition = runtime.close_after_accepted_intent("structured-close-success", 31);

        assert_eq!(disposition, BrowserCloseDisposition::closed());
        assert!(session.lock_state().terminated);
        let state = lock_unpoison(&runtime.state);
        assert!(!state.sessions.contains_key("structured-close-success"));
        assert!(state
            .closed_session_ids
            .contains("structured-close-success"));
    }

    #[test]
    fn same_closed_epoch_is_an_idempotent_structured_cleanup_retry() {
        let runtime = BrowserRuntime::default();
        let session = runtime
            .session("structured-close-retry")
            .expect("test session should be created");
        {
            let mut state = session.lock_state();
            state.synthetic_surface = true;
            state.status.has_page = true;
            state.status.open = true;
            state.shutdown_failure = Some("sensitive synthetic destroy detail".to_owned());
        }
        runtime
            .with_close_intent_fence("structured-close-retry", 44, || Ok::<_, String>(()))
            .expect("first close fence should be accepted");

        let first = runtime.close_after_accepted_intent("structured-close-retry", 44);
        assert!(first.intent_accepted);
        assert!(!first.cleanup_complete);
        assert!(first.surface_hidden);
        assert_eq!(
            first.error_code,
            Some(BrowserCloseErrorCode::NativeCleanupFailed)
        );
        assert!(!serde_json::to_string(&first)
            .expect("close disposition should serialize")
            .contains("sensitive synthetic"));

        session.lock_state().shutdown_failure = None;
        runtime
            .with_close_intent_fence("structured-close-retry", 44, || Ok::<_, String>(()))
            .expect("the same Closed epoch should be accepted as a cleanup retry");
        let second = runtime.close_after_accepted_intent("structured-close-retry", 44);

        assert_eq!(second, BrowserCloseDisposition::closed());
        let state = lock_unpoison(&runtime.state);
        assert!(!state.sessions.contains_key("structured-close-retry"));
        assert_eq!(
            state
                .lifecycle_intents
                .get("structured-close-retry")
                .copied(),
            Some(BrowserSessionLifecycleIntent {
                epoch: 44,
                desired: BrowserSessionLifecycleDesired::Closed,
            })
        );
    }

    #[test]
    fn close_failure_retains_the_exact_session_for_an_idempotent_retry() {
        let runtime = BrowserRuntime::default();
        let session = runtime
            .session("retryable-close")
            .expect("test session should be created");
        {
            let mut state = session.lock_state();
            state.status.has_page = true;
            state.status.open = true;
            state.shutdown_failure = Some("synthetic native destroy failure".into());
        }

        let error = runtime
            .close("retryable-close")
            .expect_err("the injected native failure must be reported");
        assert_eq!(error, "synthetic native destroy failure");
        {
            let state = lock_unpoison(&runtime.state);
            let retained = state
                .sessions
                .get("retryable-close")
                .expect("failed close must retain its exact cleanup handle");
            assert!(Arc::ptr_eq(&retained.state, &session.state));
            assert!(state.closed_session_ids.contains("retryable-close"));
        }
        assert!(
            runtime.existing_session("retryable-close").is_err(),
            "a retained cleanup handle must not remain an import/tool authority"
        );
        assert!(session.lock_state().terminated);

        session.lock_state().shutdown_failure = None;
        let closed = runtime
            .close("retryable-close")
            .expect("retrying the same retained handle should finish cleanup");
        assert_eq!(closed, BrowserStatus::default());
        assert!(!lock_unpoison(&runtime.state)
            .sessions
            .contains_key("retryable-close"));
    }

    #[test]
    fn newer_open_requires_failed_close_cleanup_before_publishing_or_creating_fresh() {
        let runtime = BrowserRuntime::default();
        let old_session = runtime
            .session("failed-close-then-open")
            .expect("test session should be created");
        old_session.lock_state().shutdown_failure =
            Some("synthetic native label release failure".into());

        let close_error = runtime
            .close_with_intent("failed-close-then-open", 2)
            .expect_err("the injected close failure must retain the old handle");
        assert_eq!(close_error, "synthetic native label release failure");

        let first_open_error = runtime
            .show_with_intent("failed-close-then-open", None, 3)
            .expect_err("a newer Open must retry and report the exact cleanup failure");
        assert_eq!(first_open_error, "synthetic native label release failure");
        {
            let state = lock_unpoison(&runtime.state);
            let retained = state
                .sessions
                .get("failed-close-then-open")
                .expect("failed cleanup must retain the old exact handle");
            assert!(Arc::ptr_eq(&retained.state, &old_session.state));
            assert!(state.closed_session_ids.contains("failed-close-then-open"));
            assert_eq!(
                state
                    .lifecycle_intents
                    .get("failed-close-then-open")
                    .copied(),
                Some(BrowserSessionLifecycleIntent {
                    epoch: 2,
                    desired: BrowserSessionLifecycleDesired::Closed,
                })
            );
        }

        old_session.lock_state().shutdown_failure = None;
        let second_open_error = runtime
            .show_with_intent("failed-close-then-open", None, 3)
            .expect_err("cleanup succeeds, then the fresh test session still lacks AppHandle");
        assert!(second_open_error.contains("AppHandle"));

        let fresh = runtime
            .existing_session("failed-close-then-open")
            .expect("successful exact cleanup should publish Open and allocate fresh");
        assert!(!Arc::ptr_eq(&fresh.state, &old_session.state));
        let state = lock_unpoison(&runtime.state);
        assert!(!state.closed_session_ids.contains("failed-close-then-open"));
        assert_eq!(
            state
                .lifecycle_intents
                .get("failed-close-then-open")
                .copied(),
            Some(BrowserSessionLifecycleIntent {
                epoch: 3,
                desired: BrowserSessionLifecycleDesired::Open,
            })
        );
    }

    #[test]
    fn existing_session_lookup_never_allocates_and_returns_the_exact_opened_session() {
        let runtime = BrowserRuntime::default();

        assert!(runtime.existing_session("never-opened-import").is_err());
        assert!(lock_unpoison(&runtime.state).sessions.is_empty());

        let opened = runtime
            .session("opened-import")
            .expect("trusted browser open creates the session");
        let existing = runtime
            .existing_session("opened-import")
            .expect("import lookup must reuse the trusted session");

        assert!(Arc::ptr_eq(&opened.state, &existing.state));
        assert_eq!(lock_unpoison(&runtime.state).sessions.len(), 1);
    }

    #[test]
    fn explicit_user_control_records_agent_handoff_requests() {
        let session = BrowserSession::new("explicit-user-control");
        let controlled = session.take_user_control();
        assert_eq!(controlled.control.owner, BrowserControlOwner::User);
        assert!(!controlled.control.handoff_requested);

        {
            let _automation = lock_unpoison(&session.automation);
            let error = match session.begin_agent_control(PreviewTool::Click) {
                Ok(_) => panic!("agent must wait while the user owns the page"),
                Err(error) => error,
            };
            assert!(error.contains("ask the user explicitly in the conversation"));
        }
        let waiting = session.status();
        assert_eq!(waiting.control.owner, BrowserControlOwner::User);
        assert!(waiting.control.handoff_requested);
        assert_eq!(
            waiting.control.requested_tool.as_deref(),
            Some("preview_click")
        );

        let released = session.handoff_to_agent();
        assert_eq!(released.control.owner, BrowserControlOwner::Available);
        assert!(!released.control.handoff_requested);
        assert!(released.control.requested_tool.is_none());
    }

    #[test]
    fn agent_control_guard_restores_available_after_early_exit() {
        let session = BrowserSession::new("agent-control-guard");
        {
            let _automation = lock_unpoison(&session.automation);
            let _control = session
                .begin_agent_control(PreviewTool::Snapshot)
                .expect("available page should allow an agent tool");
            let active = session.status();
            assert_eq!(active.control.owner, BrowserControlOwner::Agent);
            assert!(active
                .agent_activity
                .as_ref()
                .is_some_and(|activity| activity.active));
        }

        let released = session.status();
        assert_eq!(released.control.owner, BrowserControlOwner::Available);
        assert!(!released.control.handoff_requested);
        assert!(released
            .agent_activity
            .as_ref()
            .is_some_and(|activity| !activity.active));
    }

    #[test]
    fn a_covered_page_blocks_the_agent_and_restores_the_previous_owner() {
        let session = BrowserSession::new("covered-page-control");
        {
            let mut state = session.lock_state();
            let previous = state.status.control.clone();
            state.menu_control_before_open = Some(previous);
            state.occluded = true;
            state.status.occluded = true;
            state.status.control = BrowserControlStatus {
                owner: BrowserControlOwner::User,
                updated_at_ms: 1,
                ..BrowserControlStatus::default()
            };
        }

        {
            let _automation = lock_unpoison(&session.automation);
            let error = match session.begin_agent_control(PreviewTool::Screenshot) {
                Ok(_) => panic!("agent must not act on a page the user cannot see"),
                Err(error) => error,
            };
            assert!(error.contains("trusted UI is covering the browser page"));
        }
        let waiting = session.status();
        assert!(waiting.control.handoff_requested);
        assert_eq!(
            waiting.control.requested_tool.as_deref(),
            Some("preview_screenshot")
        );

        restore_control_after_menu(&mut session.lock_state());
        let restored = session.status();
        assert_eq!(restored.control.owner, BrowserControlOwner::Available);
        assert!(!restored.control.handoff_requested);
    }

    #[test]
    fn trusted_user_operation_keeps_user_control_even_when_it_fails() {
        let session = BrowserSession::new("trusted-user-operation");
        let error = session
            .with_user_control(|| Err::<(), String>("expected failure".into()))
            .expect_err("test operation should fail");
        assert_eq!(error, "expected failure");
        assert_eq!(session.status().control.owner, BrowserControlOwner::User);
    }

    #[test]
    fn an_approved_takeover_releases_the_fence_only_for_the_origin_it_was_granted_for() {
        let session = BrowserSession::new("credential-takeover");
        {
            let mut state = session.lock_state();
            state.status.url = "https://example.com/account".into();
        }

        session.grant_credential_takeover("https://example.com");
        assert!(session.credential_takeover_granted("https://example.com"));
        assert!(!session.credential_takeover_granted("https://other.example"));
        assert_eq!(session.status().control.owner, BrowserControlOwner::Agent);

        // Following the page to another origin must not carry the approval along.
        {
            let mut state = session.lock_state();
            clear_credential_takeover_after_committed_url(
                &mut state,
                "https://example.com/still-here",
            );
        }
        assert!(session.credential_takeover_granted("https://example.com"));
        {
            let mut state = session.lock_state();
            clear_credential_takeover_after_committed_url(&mut state, "https://other.example/next");
        }
        assert!(!session.credential_takeover_granted("https://example.com"));
        assert!(!session.credential_takeover_granted("https://other.example"));
    }

    #[test]
    fn suspending_a_page_drops_any_approved_takeover() {
        let session = BrowserSession::new("credential-takeover-reset");
        {
            let mut state = session.lock_state();
            state.status.url = "https://example.com/account".into();
            state.credential_takeover_grant = Some("https://example.com".into());
            reset_closed_state(&mut state);
        }
        assert!(!session.credential_takeover_granted("https://example.com"));
    }

    #[test]
    fn the_trusted_handoff_button_stands_in_for_the_takeover_prompt() {
        let session = BrowserSession::new("credential-handoff");
        {
            let mut state = session.lock_state();
            state.status.url = "https://example.com/account".into();
        }

        let released = session.handoff_to_agent();

        assert_eq!(released.control.owner, BrowserControlOwner::Available);
        // Asking again right after the user pressed the handoff button would be asking the same
        // person the same question twice.
        assert!(session.credential_takeover_granted("https://example.com"));
    }

    #[test]
    fn hidden_pages_remain_pending_while_they_load() {
        let hidden_loading = BrowserStatus {
            has_page: true,
            open: false,
            loading: true,
            ..BrowserStatus::default()
        };
        assert!(page_load_is_pending(&hidden_loading));

        assert!(!page_load_is_pending(&BrowserStatus {
            loading: true,
            ..BrowserStatus::default()
        }));
        assert!(!page_load_is_pending(&BrowserStatus {
            has_page: true,
            loading: false,
            ..BrowserStatus::default()
        }));
    }

    /// The settle window is polled rather than sampled once at its end, so a handler that defers
    /// its navigation past the first few milliseconds is still reported to the model, and the
    /// wait then continues until that document has loaded.
    #[test]
    fn a_navigation_that_starts_late_in_the_settle_window_is_still_observed() {
        let session = BrowserSession::new("settle-late-navigation");
        {
            let mut state = session.lock_state();
            state.synthetic_surface = true;
            state.status.has_page = true;
            state.status.loading = false;
            state.status.url = "https://example.com/form".into();
        }
        let deferred = session.clone();
        let navigator = std::thread::spawn(move || {
            // Well inside the polled window, but later than a single early sample would see.
            std::thread::sleep(Duration::from_millis(220));
            {
                let mut state = deferred.lock_state();
                state.status.url = "https://example.com/done".into();
                state.status.loading = true;
            }
            // Longer than the settle window itself: the wait must follow the load, not stop
            // when the window closes.
            std::thread::sleep(Duration::from_millis(700));
            deferred.lock_state().status.loading = false;
        });

        let watch = session.action_watch();
        let aftermath = session.settle_after_action(&watch);
        navigator.join().expect("navigation thread should finish");

        assert!(
            aftermath.navigated,
            "a navigation that begins inside the window must be reported"
        );
        assert!(!aftermath.timed_out);
        assert!(aftermath.blocked.is_none());
        let status = session.status();
        assert_eq!(status.url, "https://example.com/done");
        assert!(!status.loading);
    }

    /// A navigation the policy refuses used to be invisible: the page went nowhere,
    /// `loading` never turned on, and the click that caused it returned a plain
    /// success. The model would then keep clicking, or conclude the element was
    /// dead, with no way to learn that a policy had answered for it.
    #[test]
    fn an_interaction_whose_navigation_was_refused_reports_the_block() {
        let session = BrowserSession::new("settle-blocked-navigation");
        {
            let mut state = session.lock_state();
            state.synthetic_surface = true;
            state.status.has_page = true;
            state.status.loading = false;
        }
        let watch = session.action_watch();
        {
            // Exactly what the navigation callback records when it refuses a target.
            let mut state = session.lock_state();
            state.blocked_navigations += 1;
            state.status.error =
                Some("unsafe browser navigation was blocked: http://tauri.localhost".into());
        }

        let aftermath = session.settle_after_action(&watch);
        assert_eq!(
            aftermath.blocked.as_deref(),
            Some("unsafe browser navigation was blocked: http://tauri.localhost")
        );
        assert!(
            !aftermath.navigated,
            "a refused navigation has no destination to report"
        );
    }

    /// The opposite direction: an interaction that navigates nothing and starts no request pays
    /// the settle window once and reports nothing, rather than blocking on a load timeout.
    #[test]
    fn an_interaction_that_never_navigates_reports_no_settled_page() {
        let session = BrowserSession::new("settle-no-navigation");
        {
            let mut state = session.lock_state();
            state.synthetic_surface = true;
            state.status.has_page = true;
            state.status.loading = false;
        }

        let started = Instant::now();
        let aftermath = session.settle_after_action(&session.action_watch());
        assert!(!aftermath.navigated && !aftermath.timed_out && aftermath.blocked.is_none());
        assert!(aftermath.modal_states.is_empty());
        assert!(started.elapsed() < POST_ACTION_SETTLE + POST_ACTION_NETWORK_QUIET);
    }

    /// The requests an interaction started are waited for (bounded), the way `@playwright/mcp`
    /// waits for the responses of document/script/xhr/fetch requests; an image still streaming
    /// does not hold the settle.
    #[test]
    fn an_interaction_waits_for_the_requests_it_started_but_not_for_images() {
        let session = BrowserSession::new("settle-network-quiet");
        {
            let mut state = session.lock_state();
            state.synthetic_surface = true;
            state.status.has_page = true;
            state.status.loading = false;
            state.activity.reset_for_generation(7);
            state.activity.events_enabled = true;
        }
        let watch = session.action_watch();
        {
            let mut state = session.lock_state();
            state
                .activity
                .record_request("xhr-1".into(), "xhr".into(), false);
            state
                .activity
                .record_request("img-1".into(), "image".into(), false);
        }
        let finisher = session.clone();
        let finishing = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(900));
            finisher.lock_state().activity.finish_request("xhr-1");
        });

        let started = Instant::now();
        let aftermath = session.settle_after_action(&watch);
        finishing.join().unwrap();

        assert!(!aftermath.navigated && !aftermath.timed_out);
        // settle + the xhr's 900 ms + settle again, but never the 5 s network bound.
        assert!(
            started.elapsed() >= Duration::from_millis(900),
            "{:?}",
            started.elapsed()
        );
        assert!(started.elapsed() < POST_ACTION_SETTLE + POST_ACTION_NETWORK_QUIET);
    }

    /// A document request the interaction started means a navigation: the wait follows that
    /// document's `load` event rather than the request stream.
    #[test]
    fn a_document_request_started_by_an_interaction_counts_as_navigation() {
        let session = BrowserSession::new("settle-document-request");
        {
            let mut state = session.lock_state();
            state.synthetic_surface = true;
            state.status.has_page = true;
            state.status.loading = false;
            state.activity.reset_for_generation(3);
            state.activity.events_enabled = true;
            state.activity.main_frame_id = Some("main".into());
        }
        let watch = session.action_watch();
        {
            let mut state = session.lock_state();
            state.status.loading = true;
            record_devtools_event(
                &mut state.activity,
                "Network.requestWillBeSent",
                &json!({"requestId":"doc-1","type":"Document","frameId":"main"}),
            );
        }
        let loader = session.clone();
        let loading = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(800));
            let mut state = loader.lock_state();
            record_devtools_event(&mut state.activity, "Page.loadEventFired", &json!({}));
            state.status.loading = false;
        });

        let aftermath = session.settle_after_action(&watch);
        loading.join().unwrap();

        assert!(aftermath.navigated);
        assert!(!aftermath.timed_out);
    }

    /// A dialog that opens during the interaction ends the wait at once and is reported as the
    /// modal state; nothing else about the page can be observed while it is held.
    #[test]
    fn a_dialog_opening_during_an_interaction_ends_the_wait_with_the_modal_state() {
        let session = BrowserSession::new("settle-dialog");
        {
            let mut state = session.lock_state();
            state.synthetic_surface = true;
            state.status.has_page = true;
            state.status.loading = false;
        }
        let watch = session.action_watch();
        let opener = session.clone();
        let opening = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(120));
            opener.lock_state().activity.pending_dialog = Some(PendingDialog {
                id: 1,
                kind: "confirm".into(),
                message: "Leave?".into(),
                default_value: None,
                url: "https://example.com/".into(),
                opened_at_ms: 0,
            });
        });

        let started = Instant::now();
        let aftermath = session.settle_after_action(&watch);
        opening.join().unwrap();

        assert_eq!(aftermath.modal_states.len(), 1);
        assert!(started.elapsed() < POST_ACTION_SETTLE);
        assert_eq!(
            render_modal_states(&aftermath.modal_states),
            vec![
                "- [\"confirm\" dialog with message \"Leave?\"]: can be handled by preview_dialog"
            ]
        );
    }

    /// While a dialog is held, every action but `dialog` is refused with the modal state, before
    /// it touches the page.
    #[test]
    fn a_held_dialog_refuses_every_action_but_dialog() {
        let session = BrowserSession::new("modal-gate");
        {
            let mut state = session.lock_state();
            state.synthetic_surface = true;
            state.status.has_page = true;
            state.status.loading = false;
            state.activity.pending_dialog = Some(PendingDialog {
                id: 9,
                kind: "alert".into(),
                message: "Saved".into(),
                default_value: None,
                url: "https://example.com/".into(),
                opened_at_ms: 0,
            });
        }
        for tool in PreviewTool::ALL {
            if tool == PreviewTool::Dialog {
                continue;
            }
            let error = session
                .execute_tool_blocking(
                    tool,
                    &object(json!({"selector":"input","value":"x","expression":"1","query":"button","description":"the save button","doubleClick":false,"styles":["color"]})),
                    &BrowserToolGrants {
                        upload_paths: Some(vec![PathBuf::from("image.png")]),
                    },
                )
                .expect_err("a held dialog must refuse every tool but the one that clears it");
            assert!(
                error.starts_with(&format!("Tool \"{tool}\" does not handle the modal state.")),
                "{error}"
            );
            assert!(
                error.contains("can be handled by preview_dialog"),
                "{error}"
            );
        }
    }

    #[test]
    fn conversations_receive_stable_distinct_webview_labels() {
        let first = BrowserSession::new("conversation-one");
        let same = BrowserSession::new("conversation-one");
        let second = BrowserSession::new("conversation-two");

        assert_eq!(first.labels.page, same.labels.page);
        assert_ne!(first.labels.page, second.labels.page);
        assert!(first.labels.page.starts_with("browser-page-"));
        // Native labels are per session id; the single-use profile deliberately is not — two
        // sessions minted for the same id are two profiles, so a recreated tab starts clean.
        assert_ne!(first.labels.profile, same.labels.profile);
        // sha256("mework-browser-space-v1\0" + "conversation-one")[..16]
        assert_eq!(first.labels.page, "browser-page-b2d5e7ea4da98df9");
    }

    #[test]
    fn every_tab_gets_its_own_single_use_profile_and_distinct_webview_labels() {
        let primary = BrowserSession::new("conversation-one");
        let second_tab = BrowserSession::new("conversation-one#tab_2");
        let third_tab = BrowserSession::new("conversation-one#tab_3");
        let agent_tab = BrowserSession::new("conversation-one#agent-1");
        let other_conversation = BrowserSession::new("conversation-two");
        let other_conversation_tab = BrowserSession::new("conversation-two#tab_2");

        // Every tab is its own Chromium user-data folder: signing in inside one tab must not be
        // visible in any other tab, in another conversation, or in a tab the Agent minted.
        let sessions = [
            &primary,
            &second_tab,
            &third_tab,
            &agent_tab,
            &other_conversation,
            &other_conversation_tab,
        ];
        for (index, session) in sessions.iter().enumerate() {
            assert_eq!(session.labels.profile_root, TAB_BROWSER_PROFILE_ROOT);
            assert!(is_profile_hash(&session.labels.profile));
            for other in &sessions[index + 1..] {
                assert_ne!(session.labels.profile, other.labels.profile);
            }
        }

        // Native object identity stays per tab so two live pages never collide on one label.
        assert_ne!(primary.labels.page, second_tab.labels.page);
        assert_ne!(second_tab.labels.page, third_tab.labels.page);
        assert_ne!(primary.labels.window, second_tab.labels.window);
        assert_ne!(second_tab.labels.page, other_conversation_tab.labels.page);
        assert!(primary
            .labels
            .page
            .starts_with(&format!("{BROWSER_PAGE_LABEL}-")));
    }

    #[test]
    fn tab_session_ids_accept_one_safe_suffix_and_reject_ambiguous_owners() {
        assert_eq!(
            browser_conversation_owner("conversation-one"),
            "conversation-one"
        );
        assert_eq!(
            browser_conversation_owner("conversation-one#tab_2"),
            "conversation-one"
        );

        for accepted in [
            "conversation-one",
            "conversation-one#tab_2",
            "conversation-one#A-b_9",
        ] {
            assert!(
                validate_session_id(accepted).is_ok(),
                "unexpectedly rejected {accepted}"
            );
        }
        for rejected in [
            "conversation-one#",
            "#tab_2",
            "conversation-one#tab#2",
            "conversation-one#tab 2",
            "conversation-one#tab.2",
            "conversation-one#tab/2",
        ] {
            assert!(
                validate_session_id(rejected).is_err(),
                "unexpectedly accepted {rejected}"
            );
        }
    }

    #[test]
    fn profile_hash_is_exact_lowercase_ascii_hex() {
        let mixed = "abcdef".repeat(10) + "abcd";
        assert!(is_profile_hash(&"0".repeat(64)));
        assert!(is_profile_hash(&mixed));
        for invalid in [
            "a".repeat(63),
            "a".repeat(65),
            "A".repeat(64),
            "g".repeat(64),
            format!("{}.", "a".repeat(63)),
        ] {
            assert!(
                !is_profile_hash(&invalid),
                "unexpectedly accepted {invalid}"
            );
        }
    }

    #[test]
    fn checked_webview_task_keeps_queued_mutation_inside_generation_drain() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("root");
        let profile = root.join("profile");
        std::fs::create_dir_all(&profile).unwrap();
        let lease =
            WebView2RuntimeLease::claim(WebView2Profile::within_root(&profile, &root).unwrap())
                .unwrap();
        let control = lease.controller_issuer().begin_controller().unwrap();
        control.verify_attested_user_data_folder(&profile).unwrap();

        let caller_permit = control.permit().unwrap();
        let queued_permit = caller_permit.clone();
        let (operation_sender, operation_receiver) = mpsc::sync_channel(1);
        let queued_task: Box<dyn FnOnce() + Send> = Box::new(move || {
            run_checked_webview_task(queued_permit, || Ok::<_, String>("ran"), operation_sender);
        });
        drop(caller_permit);

        let (attempted_sender, attempted_receiver) = mpsc::sync_channel(1);
        let (invalidated_sender, invalidated_receiver) = mpsc::sync_channel(1);
        let invalidation_control = control.clone();
        let invalidation = std::thread::spawn(move || {
            attempted_sender.send(()).unwrap();
            let teardown = invalidation_control.invalidate().unwrap();
            invalidated_sender.send(()).unwrap();
            drop(teardown);
        });
        attempted_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("controller invalidation did not start");
        assert!(
            invalidated_receiver
                .recv_timeout(Duration::from_millis(75))
                .is_err(),
            "controller invalidation crossed a queued checked WebView task"
        );

        queued_task();
        assert_eq!(
            operation_receiver
                .recv_timeout(Duration::from_secs(1))
                .unwrap()
                .unwrap(),
            "ran"
        );
        invalidated_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("controller invalidation did not continue after the queued task ran");
        invalidation.join().unwrap();
        drop(lease);
    }

    #[test]
    fn profile_cleanup_deletes_only_stale_safe_hash_directories() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join(TAB_BROWSER_PROFILE_ROOT);
        std::fs::create_dir_all(&root).unwrap();
        let stale = "a".repeat(64);
        let active = "b".repeat(64);
        let file_named_like_profile = "c".repeat(64);
        std::fs::create_dir_all(root.join(&stale)).unwrap();
        std::fs::write(root.join(&stale).join("Cookies"), b"stale").unwrap();
        std::fs::create_dir_all(root.join(&active)).unwrap();
        std::fs::write(root.join(&active).join("Cookies"), b"active").unwrap();
        std::fs::create_dir_all(root.join("not-a-profile")).unwrap();
        std::fs::create_dir_all(root.join("D".repeat(64))).unwrap();
        std::fs::write(root.join(&file_named_like_profile), b"not a directory").unwrap();

        let result = cleanup_stale_profiles_in_root(&root, &HashSet::from([active.clone()]));
        assert!(result.is_err(), "the hash-named file must be reported");
        assert!(!root.join(stale).exists());
        assert!(root.join(active).is_dir());
        assert!(root.join("not-a-profile").is_dir());
        assert!(root.join("D".repeat(64)).is_dir());
        assert!(root.join(file_named_like_profile).is_file());
    }

    #[test]
    fn profile_cleanup_is_idempotent_and_rejects_links() {
        let temporary = tempfile::tempdir().unwrap();
        let missing_root = temporary.path().join("missing");
        let profile = "e".repeat(64);
        remove_profile_with_retries(&missing_root, &profile).unwrap();

        let real_root = temporary.path().join("real-root");
        let outside = temporary.path().join("outside");
        std::fs::create_dir_all(&real_root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("sentinel"), b"keep").unwrap();

        let linked_root = temporary.path().join("linked-root");
        if link_test_directory(&real_root, &linked_root).is_ok() {
            assert!(remove_profile_with_retries(&linked_root, &profile).is_err());
        }

        let linked_profile = real_root.join(&profile);
        if link_test_directory(&outside, &linked_profile).is_ok() {
            assert!(remove_profile_with_retries(&real_root, &profile).is_err());
            assert_eq!(std::fs::read(outside.join("sentinel")).unwrap(), b"keep");
        }
    }

    #[test]
    fn profile_startup_cleanup_limits_directory_count_and_defers_the_remainder() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join(TAB_BROWSER_PROFILE_ROOT);
        std::fs::create_dir_all(&root).unwrap();
        for index in 0..=RESEARCH_PROFILE_STARTUP_MAX_DIRECTORIES {
            let profile = format!("{index:064x}");
            std::fs::create_dir_all(root.join(profile)).unwrap();
        }

        let error = cleanup_stale_profiles_in_root(&root, &HashSet::new())
            .expect_err("one profile must be deferred by the startup count budget");
        assert!(error.contains("下次启动继续"));
        assert_eq!(
            std::fs::read_dir(&root).unwrap().count(),
            1,
            "startup cleanup must stop at its fixed directory budget"
        );
    }

    #[test]
    fn conversation_session_ids_are_exact_bounded_and_nonempty() {
        assert_eq!(
            validate_session_id("conversation-one").unwrap(),
            "conversation-one"
        );
        assert!(validate_session_id("").is_err());
        assert!(validate_session_id(" conversation-one").is_err());
        assert!(validate_session_id("conversation-one ").is_err());
        assert!(validate_session_id("conversation\ninvalid").is_err());
        assert!(validate_session_id(&"x".repeat(257)).is_err());
    }

    #[test]
    fn whitespace_variants_cannot_alias_an_existing_browser_profile() {
        let runtime = BrowserRuntime::default();
        runtime
            .session("conversation-one")
            .expect("trusted exact conversation ID creates the session");

        assert!(runtime.existing_session(" conversation-one").is_err());
        assert!(runtime.existing_session("conversation-one ").is_err());
        assert_eq!(lock_unpoison(&runtime.state).sessions.len(), 1);
    }

    fn eligible_capacity_snapshot(session_id: &str, last_used: u64) -> CapacitySnapshot {
        CapacitySnapshot {
            session_id: session_id.to_owned(),
            last_used,
            has_page: true,
            retained: true,
            suspended: false,
            open: false,
            loading: false,
            pending_navigation: false,
            occluded: false,
            owner: BrowserControlOwner::Available,
            active: false,
            reserved: false,
        }
    }

    #[test]
    fn capacity_lru_selects_the_oldest_safe_hidden_page() {
        let snapshots = vec![
            eligible_capacity_snapshot("newest", 30),
            eligible_capacity_snapshot("oldest", 10),
            eligible_capacity_snapshot("middle", 20),
        ];
        assert_eq!(select_lru_candidate(&snapshots).as_deref(), Some("oldest"));
    }

    #[test]
    fn capacity_lru_never_selects_unsafe_or_reserved_pages() {
        let mut snapshots = Vec::new();
        for (index, mutation) in [
            "active", "open", "loading", "pending", "covered", "user", "agent", "reserved",
            "missing",
        ]
        .into_iter()
        .enumerate()
        {
            let mut snapshot = eligible_capacity_snapshot(mutation, index as u64);
            match mutation {
                "active" => snapshot.active = true,
                "open" => snapshot.open = true,
                "loading" => snapshot.loading = true,
                "pending" => snapshot.pending_navigation = true,
                "covered" => snapshot.occluded = true,
                "user" => snapshot.owner = BrowserControlOwner::User,
                "agent" => snapshot.owner = BrowserControlOwner::Agent,
                "reserved" => snapshot.reserved = true,
                "missing" => snapshot.has_page = false,
                _ => unreachable!(),
            }
            snapshots.push(snapshot);
        }
        assert_eq!(select_lru_candidate(&snapshots), None);
    }

    #[test]
    fn failed_retained_eviction_is_recounted_before_target_reservation() {
        let first = eligible_capacity_snapshot("awake-first", 20);
        let second = eligible_capacity_snapshot("awake-second", 30);
        // A retained sleeper becomes an ordinary awake page when its cold-close Cookie snapshot
        // or controller close fails after Resume.
        let resumed_failed_sleeper = eligible_capacity_snapshot("resumed-failed-sleeper", 10);
        let before_reservation = vec![first, second, resumed_failed_sleeper.clone()];

        assert_eq!(
            awake_or_reserved_count(&before_reservation),
            MAX_AWAKE_BROWSER_PAGES
        );
        assert_eq!(
            select_lru_candidate(&before_reservation).as_deref(),
            Some("resumed-failed-sleeper")
        );

        let next_resumed_sleeper = eligible_capacity_snapshot("next-resumed-sleeper", 40);
        assert_eq!(
            awake_or_reserved_count(&[
                before_reservation[0].clone(),
                before_reservation[1].clone(),
                before_reservation[2].clone(),
                next_resumed_sleeper.clone(),
            ]),
            MAX_AWAKE_BROWSER_PAGES + 1
        );

        // The immediate post-failure awake pass sleeps that failed candidate before another cold
        // close is attempted. This keeps the next candidate's temporary Resume at the hard limit.
        let mut slept_again = resumed_failed_sleeper;
        slept_again.has_page = false;
        slept_again.suspended = true;
        assert_eq!(
            awake_or_reserved_count(&[
                before_reservation[0].clone(),
                before_reservation[1].clone(),
                slept_again.clone(),
                next_resumed_sleeper,
            ]),
            MAX_AWAKE_BROWSER_PAGES
        );

        // The final pass also keeps admitting the target at the limit after the retained phase.
        let mut target = eligible_capacity_snapshot("new-target", 40);
        target.has_page = false;
        target.retained = false;
        target.reserved = true;
        assert_eq!(
            awake_or_reserved_count(&[
                before_reservation[0].clone(),
                before_reservation[1].clone(),
                slept_again,
                target,
            ]),
            MAX_AWAKE_BROWSER_PAGES
        );
    }

    #[test]
    fn retained_lru_selects_only_the_oldest_native_sleeper() {
        let mut oldest = eligible_capacity_snapshot("oldest-sleeper", 10);
        oldest.has_page = false;
        oldest.suspended = true;
        let mut newer = eligible_capacity_snapshot("newer-sleeper", 20);
        newer.has_page = false;
        newer.suspended = true;
        let mut cold = eligible_capacity_snapshot("already-cold", 5);
        cold.has_page = false;
        cold.retained = false;
        cold.suspended = true;
        let mut awake = eligible_capacity_snapshot("awake", 1);
        awake.suspended = false;

        assert_eq!(
            select_lru_retained_sleeper(&[newer, cold, awake, oldest]).as_deref(),
            Some("oldest-sleeper")
        );
    }

    #[test]
    fn retained_lru_rejects_held_active_and_reserved_sleepers() {
        let mut protected = eligible_capacity_snapshot("held-by-user", 1);
        protected.has_page = false;
        protected.suspended = true;
        protected.owner = BrowserControlOwner::User;
        let mut active = eligible_capacity_snapshot("active", 2);
        active.has_page = false;
        active.suspended = true;
        active.active = true;
        let mut reserved = eligible_capacity_snapshot("reserved", 3);
        reserved.has_page = false;
        reserved.suspended = true;
        reserved.reserved = true;

        assert_eq!(
            select_lru_retained_sleeper(&[protected, active, reserved]),
            None
        );
    }

    #[test]
    fn suspended_status_round_trips_cold_resume_metadata() {
        let status = BrowserStatus {
            has_page: false,
            open: false,
            url: "https://example.com/task".into(),
            title: Some("Task".into()),
            zoom: 1.25,
            suspended: true,
            suspended_at_ms: Some(1_753_318_800_000),
            ..BrowserStatus::default()
        };
        let value = serde_json::to_value(&status).unwrap();
        assert_eq!(value["suspended"], true);
        assert_eq!(value["suspendedAtMs"], 1_753_318_800_000_i64);
        assert_eq!(value["url"], "https://example.com/task");
        assert_eq!(value["title"], "Task");
        assert_eq!(value["zoom"], 1.25);
    }

    #[test]
    fn navigation_resume_plan_distinguishes_native_sleep_from_cold_close() {
        let awake = BrowserStatus::default();
        let sleeping = BrowserStatus {
            suspended: true,
            ..BrowserStatus::default()
        };

        assert_eq!(
            navigation_resume_plan(&awake, true, PendingNavigation::Reload),
            NavigationResumePlan::Continue
        );
        for navigation in [
            PendingNavigation::Back,
            PendingNavigation::Forward,
            PendingNavigation::Reload,
        ] {
            assert_eq!(
                navigation_resume_plan(&sleeping, true, navigation),
                NavigationResumePlan::ResumeNativeThenContinue
            );
        }
        assert_eq!(
            navigation_resume_plan(&sleeping, false, PendingNavigation::Reload),
            NavigationResumePlan::ResumeColdCompletesReload
        );
        assert_eq!(
            navigation_resume_plan(&sleeping, false, PendingNavigation::Back),
            NavigationResumePlan::ColdHistoryUnavailable
        );
        assert_eq!(
            navigation_resume_plan(&sleeping, false, PendingNavigation::Forward),
            NavigationResumePlan::ColdHistoryUnavailable
        );
    }

    #[test]
    fn history_navigation_validation_admits_sleeping_pages_without_widening_history() {
        let native_sleeping = BrowserStatus {
            suspended: true,
            can_go_back: true,
            can_go_forward: true,
            ..BrowserStatus::default()
        };
        for navigation in [
            PendingNavigation::Back,
            PendingNavigation::Forward,
            PendingNavigation::Reload,
        ] {
            assert!(
                validate_history_navigation_before_capacity(&native_sleeping, navigation).is_ok()
            );
        }

        let cold_sleeping = BrowserStatus {
            suspended: true,
            ..BrowserStatus::default()
        };
        assert!(validate_history_navigation_before_capacity(
            &cold_sleeping,
            PendingNavigation::Reload
        )
        .is_ok());
        assert!(validate_history_navigation_before_capacity(
            &cold_sleeping,
            PendingNavigation::Back
        )
        .is_err());
        assert!(validate_history_navigation_before_capacity(
            &cold_sleeping,
            PendingNavigation::Forward
        )
        .is_err());
        assert!(validate_history_navigation_before_capacity(
            &BrowserStatus::default(),
            PendingNavigation::Reload
        )
        .is_err());
    }

    #[test]
    fn every_tool_reserves_capacity_when_resuming_a_sleeping_task() {
        let sleeping = BrowserStatus {
            suspended: true,
            ..BrowserStatus::default()
        };
        assert!(browser_tool_needs_live_slot(&sleeping));

        // Every preview tool creates the page it needs (ensureTab), so a never-opened session
        // takes a slot for an observation just as it does for an interaction.
        assert!(browser_tool_needs_live_slot(&BrowserStatus::default()));
        assert!(!browser_tool_needs_live_slot(&BrowserStatus {
            has_page: true,
            ..BrowserStatus::default()
        }));
    }

    #[test]
    fn trusted_history_wrapper_keeps_user_ownership_when_validation_fails() {
        let runtime = BrowserRuntime::default();
        let error = runtime
            .navigate_history_as_user("trusted-history", "back")
            .unwrap_err();

        assert!(error.contains("browser has no back history"));
        assert_eq!(
            runtime.status("trusted-history").control.owner,
            BrowserControlOwner::User
        );
        assert_eq!(runtime.live_page_count(), 0);
    }

    #[test]
    fn a_recreated_session_gets_a_fresh_profile_instead_of_the_closed_ones() {
        let runtime = BrowserRuntime::default();
        let conversation = "cookie-single-use";
        let first = runtime.session(conversation).unwrap();
        let first_profile = first.labels.profile.clone();
        lock_unpoison(&runtime.state).sessions.remove(conversation);

        // The manager entry is gone (as after a close); the next session for the same id must not
        // resurrect the old cookie store, whose directory is deleted on close.
        let second = runtime.session(conversation).unwrap();
        assert_ne!(second.labels.profile, first_profile);
    }

    #[test]
    fn credential_takeover_only_applies_after_the_user_has_driven_the_page() {
        let runtime = BrowserRuntime::default();
        let conversation = "cookie-provenance";
        let session = runtime.session(conversation).unwrap();

        // Until the user takes the page over from trusted chrome, every cookie in the tab's
        // single-use profile came from Agent-driven browsing, so the takeover question does not
        // arise — regardless of what the page's cookie jar holds.
        assert!(!session.user_has_ever_controlled());
        assert_eq!(
            runtime.pending_credential_takeover(conversation).unwrap(),
            None
        );

        // The first trusted-chrome takeover latches the page as potentially carrying the user's
        // own sign-in material from here on.
        session.take_user_control();
        assert!(session.user_has_ever_controlled());
    }

    #[test]
    fn cloned_runtimes_serialize_the_complete_browser_lifecycle() {
        let runtime = BrowserSession::default();
        let first = runtime.clone();
        let second = runtime.clone();
        let (first_entered_tx, first_entered_rx) = mpsc::channel();
        let (release_first_tx, release_first_rx) = mpsc::channel();
        let (second_attempted_tx, second_attempted_rx) = mpsc::channel();
        let (second_entered_tx, second_entered_rx) = mpsc::channel();

        let first_thread = std::thread::spawn(move || {
            let _guard = first.lock_lifecycle();
            first_entered_tx.send(()).unwrap();
            release_first_rx.recv().unwrap();
        });
        first_entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("first lifecycle operation did not start");

        let second_thread = std::thread::spawn(move || {
            second_attempted_tx.send(()).unwrap();
            let _guard = second.lock_lifecycle();
            second_entered_tx.send(()).unwrap();
        });
        second_attempted_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("second lifecycle operation did not attempt the shared gate");
        assert!(
            second_entered_rx
                .recv_timeout(Duration::from_millis(75))
                .is_err(),
            "a cloned runtime entered while another lifecycle operation was active"
        );

        release_first_tx.send(()).unwrap();
        second_entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("second lifecycle operation did not continue after release");
        first_thread.join().unwrap();
        second_thread.join().unwrap();
    }

    #[test]
    fn pending_new_navigation_reports_the_accepted_target_not_the_stale_webview_url() {
        let target = "http://127.0.0.1:1430/image-input-browser-e2e?step=two";
        let mut status = BrowserStatus {
            open: true,
            url: target.to_owned(),
            loading: true,
            ..BrowserStatus::default()
        };

        merge_observed_url(
            &mut status,
            Some(PendingNavigation::New),
            "http://127.0.0.1:1430/image-input-browser-e2e",
        );
        assert_eq!(status.url, target);

        merge_observed_url(&mut status, None, target);
        assert_eq!(status.url, target);
    }
}
