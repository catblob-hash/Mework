//! Native, conversation-scoped browser pages used by both Tauri commands and agent tools.
//!
//! Security invariant: `BROWSER_PAGE_LABEL` is the untrusted, remote webview. Never add that
//! label to an application capability. Browser chrome lives in the trusted main React WebView;
//! the desktop build only adds the remote page as a permissionless child WebView.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc, Arc, Mutex, MutexGuard, OnceLock, TryLockError,
    },
    time::{Duration, Instant},
};

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

use crate::{
    browser_profile_data, browser_webview_lifecycle,
    browser_window_region::{
        apply_menu_hole_ordered, restore_full_region_ordered, set_page_stacking,
        LogicalRect as BrowserWindowLogicalRect, WindowRegionOrder, WindowRegionTicket,
    },
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
const BROWSER_PANEL_ANIMATION_DURATION: Duration = Duration::from_millis(240);
const BROWSER_PANEL_ANIMATION_FRAME: Duration = Duration::from_millis(16);
const MAX_AWAKE_BROWSER_PAGES: usize = 3;
/// Sleeping WebViews preserve exact page state, but each isolated task Profile can still retain
/// native controller/process resources. Older sleeping tasks are cold-closed beyond this bound.
const MAX_RETAINED_BROWSER_PAGES: usize = 8;
/// Tab id the Agent uses for a conversation's own page. Extra tabs are addressed by the `#<token>`
/// suffix that already distinguishes their session, and renderer-minted tokens are `tab_<uuid>`,
/// so this sentinel cannot collide with a real token.
const AGENT_PRIMARY_TAB_ID: &str = "main";
/// Prefix of every tab id `playwright tab_new` mints. It is the sole marker separating the model's own
/// pages from the user's, so the mint site and [`session_is_user_owned`] must keep sharing it.
const AGENT_TAB_ID_PREFIX: &str = "agent-";
/// Tabs one conversation may hold at once, counting its primary page. Live pages are already
/// bounded by the awake/retained limits; this keeps a stray loop from accumulating page-less
/// sessions the user would then have to close by hand.
const MAX_AGENT_BROWSER_TABS: usize = 8;
const MAX_AGENT_TAB_MINT_ATTEMPTS: usize = 64;

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

const DEFAULT_WAIT_TIMEOUT_MS: u64 = 5_000;
const MAX_WAIT_TIMEOUT_MS: u64 = 30_000;
const DEFAULT_SNAPSHOT_CHARS: usize = 30_000;
const MIN_SNAPSHOT_CHARS: usize = 1_000;
const MAX_SNAPSHOT_CHARS: usize = 60_000;
const MAX_SELECTOR_CHARS: usize = 2_048;
const MAX_TEXT_INPUT_CHARS: usize = 1_000_000;
const MAX_EVALUATE_CHARS: usize = 1_000_000;
const MAX_KEY_CHARS: usize = 64;
const MAX_URL_CHARS: usize = 8_192;
const MAX_SCREENSHOT_BYTES: usize = 64 * 1024 * 1024;
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
/// Interactions poll actionability (visible, stable, enabled, unobstructed) up to this long. This
/// is Playwright's default action timeout, which `@playwright/mcp` inherits unchanged.
const ACTIONABILITY_TIMEOUT: Duration = Duration::from_secs(5);
const ACTIONABILITY_POLL: Duration = Duration::from_millis(80);
/// `playwright navigate` waits this long for the new document to reach DOMContentLoaded before
/// failing the call, the same bound Playwright's `page.goto` applies (`timeouts.navigation`).
const NAVIGATION_TIMEOUT: Duration = Duration::from_secs(60);
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
const AGENT_POINTER_LIFETIME: Duration = Duration::from_millis(1_350);
const AGENT_POINTER_CDP_TIMEOUT: Duration = Duration::from_secs(2);
const AGENT_POINTER_RADIUS: f64 = 12.0;
const MAX_FILL_FORM_FIELDS: usize = 50;
const MAX_UPLOAD_FILES: usize = 10;
const MAX_KEY_REPEAT: u64 = 50;
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

/// A JavaScript dialog the page opened and that is being held open until `playwright dialog`
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
/// `showPicker`) and that is being held until `playwright file_upload` or `upload_image` sets its
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
    /// Counts of the main frame's `DOMContentLoaded` and `load` events, so a wait can tell a new
    /// document's events from the previous document's.
    dom_content_loaded_events: u64,
    load_events: u64,
    pending_dialog: Option<PendingDialog>,
    dialog_records: Vec<DialogRecord>,
    next_dialog_id: u64,
    pending_file_chooser: Option<PendingFileChooser>,
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

    fn record_request(&mut self, request_id: String, resource_type: String, main_frame_navigation: bool) {
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
                format!("\"{}\" dialog with message \"{}\"", dialog.kind, dialog.message)
            }
            ModalState::FileChooser(_) => "File chooser".to_owned(),
        }
    }

    fn cleared_by(&self) -> &'static str {
        match self {
            ModalState::Dialog(_) => "playwright dialog",
            ModalState::FileChooser(_) => "playwright file_upload (or upload_image)",
        }
    }

    fn is_cleared_by(&self, action: PlaywrightAction) -> bool {
        match self {
            ModalState::Dialog(_) => action == PlaywrightAction::Dialog,
            ModalState::FileChooser(_) => {
                matches!(action, PlaywrightAction::FileUpload | PlaywrightAction::UploadImage)
            }
        }
    }
}

fn render_modal_states(states: &[ModalState]) -> Vec<String> {
    states
        .iter()
        .map(|state| format!("- [{}]: can be handled by {}", state.description(), state.cleared_by()))
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
    dom_content_loaded_events: u64,
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
            dom_content_loaded_events: state.activity.dom_content_loaded_events,
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

    /// Whether the main frame fired `DOMContentLoaded` since the watch was taken.
    fn dom_content_loaded_since(&self, activity: &PageActivity) -> bool {
        if activity.generation != self.generation {
            activity.dom_content_loaded_events > 0
        } else {
            activity.dom_content_loaded_events > self.dom_content_loaded_events
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

/// One operation of the single public `playwright` tool.
///
/// The whole browser surface is one wire tool multiplexed by `action`, and this enum is the only
/// place the 23 operation names exist. Catalog, model-visible schema, security policy, executor
/// dispatch and page implementation all match on it, so adding an operation is a compile error
/// everywhere it has to be decided instead of a silent fallthrough.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum PlaywrightAction {
    Navigate,
    Snapshot,
    Click,
    Type,
    FillForm,
    Select,
    Hover,
    Key,
    Scroll,
    Evaluate,
    Wait,
    Screenshot,
    Console,
    Network,
    Dialog,
    FileUpload,
    UploadImage,
    Resize,
    TabNew,
    TabList,
    TabSelect,
    TabClose,
    Close,
}

impl PlaywrightAction {
    /// Declaration order is the order the catalog, the schema variants and the docs list them in.
    pub(crate) const ALL: [Self; 23] = [
        Self::Navigate,
        Self::Snapshot,
        Self::Click,
        Self::Type,
        Self::FillForm,
        Self::Select,
        Self::Hover,
        Self::Key,
        Self::Scroll,
        Self::Evaluate,
        Self::Wait,
        Self::Screenshot,
        Self::Console,
        Self::Network,
        Self::Dialog,
        Self::FileUpload,
        Self::UploadImage,
        Self::Resize,
        Self::TabNew,
        Self::TabList,
        Self::TabSelect,
        Self::TabClose,
        Self::Close,
    ];

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Navigate => "navigate",
            Self::Snapshot => "snapshot",
            Self::Click => "click",
            Self::Type => "type",
            Self::FillForm => "fill_form",
            Self::Select => "select",
            Self::Hover => "hover",
            Self::Key => "key",
            Self::Scroll => "scroll",
            Self::Evaluate => "evaluate",
            Self::Wait => "wait",
            Self::Screenshot => "screenshot",
            Self::Console => "console",
            Self::Network => "network",
            Self::Dialog => "dialog",
            Self::FileUpload => "file_upload",
            Self::UploadImage => "upload_image",
            Self::Resize => "resize",
            Self::TabNew => "tab_new",
            Self::TabList => "tab_list",
            Self::TabSelect => "tab_select",
            Self::TabClose => "tab_close",
            Self::Close => "close",
        }
    }

    pub(crate) fn from_wire(value: &str) -> Result<Self, String> {
        Self::ALL
            .into_iter()
            .find(|action| action.as_str() == value)
            .ok_or_else(|| format!("playwright does not support action {value}"))
    }

    /// Reads the action out of one `playwright` call's arguments.
    ///
    /// An absent or non-string `action` is an error, never a default: every downstream decision
    /// (security policy, capacity reservation, page dispatch) is action-specific, so guessing one
    /// would classify an interaction as an observation.
    pub(crate) fn from_input(input: &Map<String, Value>) -> Result<Self, String> {
        let value = input
            .get("action")
            .ok_or_else(|| "playwright is missing required parameter action".to_owned())?;
        let name = value
            .as_str()
            .ok_or_else(|| "playwright parameter action must be a string".to_owned())?;
        Self::from_wire(name)
    }

    /// Whether this action is dispatched by the manager rather than by one page. Tab actions and
    /// `close` create, retarget and destroy sessions, which is authority only the manager holds.
    pub(crate) fn is_tab_action(self) -> bool {
        matches!(
            self,
            Self::TabNew | Self::TabList | Self::TabSelect | Self::TabClose | Self::Close
        )
    }

    /// Whether the action changes the page and therefore answers with the page's state after it:
    /// the same actions `@playwright/mcp` answers with a fresh snapshot. Observations
    /// (`snapshot`, `console`, `network`, `screenshot`, `evaluate`) and manager actions do not.
    pub(crate) fn reports_page_after(self) -> bool {
        matches!(
            self,
            Self::Navigate
                | Self::Click
                | Self::Type
                | Self::FillForm
                | Self::Select
                | Self::Hover
                | Self::Key
                | Self::Scroll
                | Self::Wait
                | Self::Dialog
                | Self::FileUpload
                | Self::UploadImage
                | Self::Resize
        )
    }

    /// Whether the action's consequences are awaited before it answers: the interactions
    /// `@playwright/mcp` wraps in `waitForCompletion`. `hover`, `select` and `fill_form` are not,
    /// because they cannot start a navigation of their own.
    pub(crate) fn waits_for_completion(self) -> bool {
        matches!(
            self,
            Self::Click
                | Self::Type
                | Self::Key
                | Self::Scroll
                | Self::Evaluate
                | Self::Dialog
                | Self::FileUpload
                | Self::UploadImage
        )
    }

    /// Whether the action's contract only means something to a model that can
    /// see images: `screenshot` hands back pixels, and `upload_image` sends an
    /// image the conversation could only be carrying for such a model. Offering
    /// either to a text-only model advertises a result it can never receive.
    pub(crate) fn requires_image_capability(self) -> bool {
        matches!(self, Self::Screenshot | Self::UploadImage)
    }
}

impl std::fmt::Display for PlaywrightAction {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Filesystem authority for browser tools that touch host paths. Every path in here must already
/// have passed the caller's path guard; `BrowserRuntime` never resolves renderer-provided paths.
#[derive(Default, Clone)]
pub struct BrowserToolGrants {
    pub screenshot_path: Option<PathBuf>,
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

/// Geometry of trusted React browser chrome that must appear above the native Chromium child.
///
/// The rectangle is expressed relative to the remote page's top-left corner. It is removed from
/// the child HWND with `SetWindowRgn`, leaving page size and Playwright/CDP coordinates unchanged.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BrowserMenuRegionRequest {
    pub generation: u64,
    pub expanded: bool,
    #[serde(default)]
    pub rect: Option<BrowserMenuRect>,
    #[serde(default)]
    pub shadow: Option<f64>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BrowserMenuRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, Copy)]
struct BrowserMenuHole {
    rect: BrowserWindowLogicalRect,
    shadow: f64,
    order_identity: (u64, u64),
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
        }
    }
}

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

#[derive(Debug, Clone, PartialEq, Eq)]
struct TargetSpec {
    selector: Option<String>,
    element_ref: Option<String>,
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
    menu_expanded: bool,
    /// Opening trusted chrome temporarily takes the page out of agent automation. The menu is
    /// exposed through a native window-region hole, so page geometry never changes.
    menu_control_before_open: Option<BrowserControlStatus>,
    menu_hole: Option<BrowserMenuHole>,
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

    /// Brings a shown page back above the trusted WebView (the counterpart of `park`).
    fn unpark(&self) -> Result<(), String> {
        self.with_native_tail(|native, permit| set_page_stacking(native, permit, false))
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
    /// Invalidates queued or timed-out native menu-region callbacks before they reach SetWindowRgn.
    menu_window_region_order: WindowRegionOrder,
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
    /// Tab each conversation's Agent browser tools currently act on, keyed by the owning
    /// conversation. An absent entry means the conversation's own primary page. Values are always
    /// sessions of that same conversation, so the pointer can never move an Agent's tools across a
    /// profile boundary.
    agent_tabs: HashMap<String, String>,
    /// Monotonic source of Agent-minted tab tokens. It is never reset while the process lives, so
    /// a closed tab's id is not handed out again to a later tab of the same run.
    agent_tab_sequence: u64,
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
    menu_expanded: bool,
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
                return Err("the application is shutting down and cannot create a browser page".into());
            }
            if state.closed_session_ids.contains(session_id) {
                return Err("the embedded browser tab is closed; reopen it from the trusted UI first".into());
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
            return Err("the embedded browser tab is closed; reopen it from the trusted UI first".into());
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

    pub(crate) fn session_for_tool(
        &self,
        session_id: &str,
        _action: PlaywrightAction,
    ) -> Result<BrowserSession, String> {
        self.reopen_closed_session_for_agent(session_id)?;
        let session = self.session(session_id)?;
        self.touch_session(session_id);
        Ok(session)
    }

    /// Executes an Agent tool while retaining a capacity reservation across first-page creation.
    /// The reservation is deliberately manager-owned: returning only a `BrowserSession` from
    /// `session_for_tool` cannot prevent two concurrent conversations from both observing a free
    /// slot before either WebView exists.
    pub(crate) fn execute_tool_blocking(
        &self,
        session_id: &str,
        action: PlaywrightAction,
        input: &Map<String, Value>,
        grants: &BrowserToolGrants,
    ) -> Result<Value, String> {
        let session_id = validate_session_id(session_id)?.to_owned();
        self.reopen_closed_session_for_agent(&session_id)?;
        let session = self.session(&session_id)?;
        let status = session.status();
        let needs_page = browser_tool_needs_live_slot(action, &status);
        if action == PlaywrightAction::Navigate && needs_page {
            validate_navigation_tool_before_capacity(input, &session)?;
        }
        let _reservation = if needs_page {
            let _lifecycle = lock_unpoison(&self.lifecycle);
            self.reserve_live_slot_locked(&session_id, &session)?
        } else {
            None
        };
        self.touch_session(&session_id);
        let result = session.execute_tool_blocking(action, input, grants);
        self.touch_session(&session_id);
        result
    }

    /// Origin at which an Agent browser tool would take over a page the *user* opened while that
    /// page carries the user's sign-in material.
    ///
    /// `Ok(None)` means no authorization is needed: either the Agent is acting on a tab it opened
    /// itself, or the user's tab is not carrying credentials for its committed origin, or the user
    /// already approved this exact origin on this exact tab.
    pub(crate) fn pending_credential_takeover(
        &self,
        session_id: &str,
    ) -> Result<Option<String>, String> {
        if !session_is_user_owned(session_id) {
            return Ok(None);
        }
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
        let session =
            session.ok_or_else(|| "the authorized browser tab no longer exists; start the operation again".to_owned())?;
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

    /// Session the Agent's browser tools currently act on for one conversation.
    ///
    /// The pointer is convenience state, never authority. A tab the trusted UI closed, or that a
    /// shutdown removed, resolves back to the conversation's own page so the next tool still lands
    /// on a real surface instead of failing on a tab the Agent cannot see disappear.
    pub(crate) fn agent_tab_session_id(&self, conversation_id: &str) -> Result<String, String> {
        let conversation_id = validate_agent_conversation_id(conversation_id)?;
        let mut state = lock_unpoison(&self.state);
        let Some(current) = state.agent_tabs.get(conversation_id).cloned() else {
            return Ok(conversation_id.to_owned());
        };
        if state.sessions.contains_key(&current) && !state.closed_session_ids.contains(&current) {
            return Ok(current);
        }
        state.agent_tabs.remove(conversation_id);
        Ok(conversation_id.to_owned())
    }

    /// Runs one manager-level action. These are deliberately not `BrowserSession` tools: they
    /// create, retarget, and destroy sessions, which is authority only the manager holds.
    pub(crate) fn execute_agent_tab_tool(
        &self,
        conversation_id: &str,
        action: PlaywrightAction,
        input: &Map<String, Value>,
    ) -> Result<Value, String> {
        let conversation_id = validate_agent_conversation_id(conversation_id)?.to_owned();
        match action {
            PlaywrightAction::TabList => self.agent_tab_roster(&conversation_id),
            PlaywrightAction::TabNew => self.open_agent_tab(&conversation_id, input),
            PlaywrightAction::TabSelect => {
                let tab = required_input_string(input, "tab", 256, false)?;
                let session_id = self.agent_tab_target(&conversation_id, &tab)?;
                lock_unpoison(&self.state)
                    .agent_tabs
                    .insert(conversation_id.clone(), session_id);
                self.agent_tab_roster(&conversation_id)
            }
            PlaywrightAction::TabClose => self.close_agent_tab(&conversation_id, input),
            PlaywrightAction::Close => self.close_agent_browser(&conversation_id),
            other => Err(format!("embedded browser tab tools do not support action {other}")),
        }
    }

    /// An Agent action on a page that was closed — by the user from the trusted chrome, by
    /// `tab_close`, or by `close` — starts over on a fresh page instead of failing, the way
    /// `@playwright/mcp` opens a new tab when none is left. Only the tombstone that keeps late
    /// renderer callbacks from re-minting the page is cleared here; the page itself is created
    /// by the action that needed it.
    fn reopen_closed_session_for_agent(&self, session_id: &str) -> Result<(), String> {
        let _lifecycle = lock_unpoison(&self.lifecycle);
        let cleanup_candidate = {
            let state = lock_unpoison(&self.state);
            if state.shutting_down {
                return Err("the application is shutting down and cannot create a browser page".into());
            }
            if !state.closed_session_ids.contains(session_id) {
                return Ok(());
            }
            state.sessions.get(session_id).cloned()
        };
        // A failed close keeps its terminated handle until the native labels are reusable; retry
        // that cleanup first, exactly as a trusted reopen does.
        if let Some(candidate) = cleanup_candidate.filter(|session| session.lock_state().terminated) {
            candidate.shutdown()?;
            let mut state = lock_unpoison(&self.state);
            match state.sessions.get(session_id) {
                Some(current) if Arc::ptr_eq(&current.state, &candidate.state) => {
                    state.sessions.remove(session_id);
                    remove_conversation_session_metadata(&mut state, session_id);
                }
                Some(_) => {
                    return Err("the browser page was replaced while it was being reopened; try again".into());
                }
                None => {}
            }
        }
        lock_unpoison(&self.state).closed_session_ids.remove(session_id);
        Ok(())
    }

    /// `playwright close`: closes every tab of the conversation, extra tabs first and the
    /// conversation's own page last. The next action opens a fresh blank page again.
    fn close_agent_browser(&self, conversation_id: &str) -> Result<Value, String> {
        let mut session_ids: Vec<String> = {
            let state = lock_unpoison(&self.state);
            state
                .sessions
                .keys()
                .filter(|id| {
                    browser_conversation_owner(id) == conversation_id
                        && !state.closed_session_ids.contains(*id)
                })
                .cloned()
                .collect()
        };
        session_ids.sort_by(|left, right| {
            browser_tab_id(left)
                .eq(AGENT_PRIMARY_TAB_ID)
                .cmp(&browser_tab_id(right).eq(AGENT_PRIMARY_TAB_ID))
                .then_with(|| left.cmp(right))
        });
        let mut closed = Vec::new();
        let mut failures = Vec::new();
        for session_id in session_ids {
            let epoch = {
                let state = lock_unpoison(&self.state);
                next_browser_lifecycle_epoch(&state, &session_id)?
            };
            match self.close_with_intent(&session_id, epoch) {
                Ok(_) => closed.push(browser_tab_id(&session_id).to_owned()),
                Err(error) => failures.push(format!("{}: {error}", browser_tab_id(&session_id))),
            }
        }
        lock_unpoison(&self.state).agent_tabs.remove(conversation_id);
        if !failures.is_empty() {
            return Err(format!(
                "some browser tabs could not be closed: {}",
                failures.join("; ")
            ));
        }
        Ok(json!({
            "closed": closed,
            "current": AGENT_PRIMARY_TAB_ID,
            "tabs": [],
            "notices": ["No open tabs. The next action opens a fresh blank page."],
        }))
    }

    fn open_agent_tab(
        &self,
        conversation_id: &str,
        input: &Map<String, Value>,
    ) -> Result<Value, String> {
        // Validate before minting anything: a rejected URL must not leave a stray tab behind.
        let url = optional_input_string(input, "url", MAX_URL_CHARS)?;
        if let Some(url) = url.as_deref() {
            if matches!(url, "back" | "forward" | "reload") {
                return Err("a new tab has no history, so url cannot be back, forward, or reload".into());
            }
        }

        let (session_id, previous) = {
            let mut state = lock_unpoison(&self.state);
            if state.shutting_down {
                return Err("the application is shutting down and cannot create a browser tab".into());
            }
            let existing = state
                .sessions
                .keys()
                .filter(|id| browser_conversation_owner(id) == conversation_id)
                .count();
            if existing >= MAX_AGENT_BROWSER_TABS {
                return Err(format!(
                    "this conversation can have at most {MAX_AGENT_BROWSER_TABS} browser tabs open at once; use playwright tab_close to close tabs you no longer need"
                ));
            }
            let mut session_id = String::new();
            for _ in 0..MAX_AGENT_TAB_MINT_ATTEMPTS {
                state.agent_tab_sequence = state.agent_tab_sequence.saturating_add(1);
                let candidate = format!(
                    "{conversation_id}#{AGENT_TAB_ID_PREFIX}{}",
                    state.agent_tab_sequence
                );
                if !state.sessions.contains_key(&candidate)
                    && !state.closed_session_ids.contains(&candidate)
                {
                    session_id = candidate;
                    break;
                }
            }
            if session_id.is_empty() {
                return Err("could not allocate an available identifier for the new browser tab; restart the application".into());
            }
            let previous = state.agent_tabs.get(conversation_id).cloned();
            (session_id, previous)
        };

        self.session(&session_id)?;
        lock_unpoison(&self.state)
            .agent_tabs
            .insert(conversation_id.to_owned(), session_id.clone());

        if url.is_some() {
            if let Err(error) = self.execute_tool_blocking(
                &session_id,
                PlaywrightAction::Navigate,
                input,
                &BrowserToolGrants::default(),
            ) {
                self.discard_unopened_agent_tab(conversation_id, &session_id, previous);
                return Err(error);
            }
        }

        let mut roster = self.agent_tab_roster(conversation_id)?;
        if let Some(object) = roster.as_object_mut() {
            object.insert("opened".into(), json!(browser_tab_id(&session_id)));
        }
        Ok(roster)
    }

    /// Undoes a freshly minted tab whose first navigation failed. Only a page-less session is
    /// dropped: if navigation left a real WebView behind, the tab stays listed so the Agent and
    /// the user can still see and close it.
    fn discard_unopened_agent_tab(
        &self,
        conversation_id: &str,
        session_id: &str,
        previous: Option<String>,
    ) {
        // Read the page's own state before touching the manager: a session's lock must never be
        // taken under the manager lock.
        let session = lock_unpoison(&self.state).sessions.get(session_id).cloned();
        let page_less = session.is_some_and(|session| !session.status().has_page);
        let mut state = lock_unpoison(&self.state);
        match previous {
            Some(previous) => {
                state
                    .agent_tabs
                    .insert(conversation_id.to_owned(), previous);
            }
            None => {
                state.agent_tabs.remove(conversation_id);
            }
        }
        if page_less {
            state.sessions.remove(session_id);
            state.last_used.remove(session_id);
        }
    }

    fn close_agent_tab(
        &self,
        conversation_id: &str,
        input: &Map<String, Value>,
    ) -> Result<Value, String> {
        let tab = required_input_string(input, "tab", 256, false)?;
        let session_id = self.agent_tab_target(conversation_id, &tab)?;
        // Remember where the closed tab stood so the next tab in order becomes current, the
        // rule Playwright applies when the current page closes.
        let order_before = self.agent_tab_order(conversation_id);
        let was_current = self.agent_tab_session_id(conversation_id)? == session_id;
        let epoch = {
            let state = lock_unpoison(&self.state);
            next_browser_lifecycle_epoch(&state, &session_id)?
        };
        self.close_with_intent(&session_id, epoch)?;
        {
            let mut state = lock_unpoison(&self.state);
            if state.agent_tabs.get(conversation_id) == Some(&session_id) {
                state.agent_tabs.remove(conversation_id);
            }
        }
        if was_current {
            let remaining = self.agent_tab_order(conversation_id);
            let index = order_before
                .iter()
                .position(|id| *id == session_id)
                .unwrap_or(0)
                .min(remaining.len().saturating_sub(1));
            let mut state = lock_unpoison(&self.state);
            match remaining.get(index) {
                Some(next) if next != conversation_id => {
                    state
                        .agent_tabs
                        .insert(conversation_id.to_owned(), next.clone());
                }
                _ => {
                    state.agent_tabs.remove(conversation_id);
                }
            }
        }
        let mut roster = self.agent_tab_roster(conversation_id)?;
        if let Some(object) = roster.as_object_mut() {
            object.insert("closed".into(), json!(tab));
            if object
                .get("tabs")
                .and_then(Value::as_array)
                .is_some_and(Vec::is_empty)
            {
                object.insert(
                    "notices".into(),
                    json!(["No open tabs. The next action opens a fresh blank page."]),
                );
            }
        }
        Ok(roster)
    }

    /// Live tabs of one conversation in roster order: the primary page first, then lexical.
    fn agent_tab_order(&self, conversation_id: &str) -> Vec<String> {
        let state = lock_unpoison(&self.state);
        let mut ids: Vec<String> = state
            .sessions
            .keys()
            .filter(|id| {
                browser_conversation_owner(id) == conversation_id
                    && !state.closed_session_ids.contains(*id)
            })
            .cloned()
            .collect();
        ids.sort_by(|left, right| {
            browser_tab_id(left)
                .eq(AGENT_PRIMARY_TAB_ID)
                .cmp(&browser_tab_id(right).eq(AGENT_PRIMARY_TAB_ID))
                .reverse()
                .then_with(|| left.cmp(right))
        });
        ids
    }

    /// Resolves an Agent-supplied tab id inside exactly one conversation. `main` is the
    /// conversation's own page; every other id must be a live tab of that same conversation, so a
    /// crafted value can neither name another conversation's session nor create one.
    fn agent_tab_target(&self, conversation_id: &str, tab: &str) -> Result<String, String> {
        if tab == AGENT_PRIMARY_TAB_ID {
            return Ok(conversation_id.to_owned());
        }
        let session_id = format!("{conversation_id}#{tab}");
        validate_session_id(&session_id)?;
        let state = lock_unpoison(&self.state);
        if !state.sessions.contains_key(&session_id)
            || state.closed_session_ids.contains(&session_id)
        {
            return Err(format!(
                "browser tab {tab} does not exist or is already closed; use playwright tab_list to view current tabs"
            ));
        }
        Ok(session_id)
    }

    /// Every tab of one conversation, in a stable order with the primary page first.
    fn agent_tab_roster(&self, conversation_id: &str) -> Result<Value, String> {
        let current = self.agent_tab_session_id(conversation_id)?;
        let sessions = {
            let state = lock_unpoison(&self.state);
            let mut sessions = state
                .sessions
                .iter()
                .filter(|(id, _)| {
                    browser_conversation_owner(id) == conversation_id
                        && !state.closed_session_ids.contains(*id)
                })
                .map(|(id, session)| (id.clone(), session.clone()))
                .collect::<Vec<_>>();
            sessions.sort_by(|(left, _), (right, _)| {
                browser_tab_id(left)
                    .eq(AGENT_PRIMARY_TAB_ID)
                    .cmp(&browser_tab_id(right).eq(AGENT_PRIMARY_TAB_ID))
                    .reverse()
                    .then_with(|| left.cmp(right))
            });
            sessions
        };
        let tabs = sessions
            .into_iter()
            .map(|(id, session)| {
                let status = session.status();
                json!({
                    "tab": browser_tab_id(&id),
                    "url": status.url,
                    "title": status.title,
                    "current": id == current,
                    "visible": status.open,
                })
            })
            .collect::<Vec<_>>();
        Ok(json!({
            "current": browser_tab_id(&current),
            "tabs": tabs,
        }))
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
        action: PlaywrightAction,
        input: Map<String, Value>,
        grants: BrowserToolGrants,
    ) -> Result<Value, String> {
        let runtime = self.clone();
        tauri::async_runtime::spawn_blocking(move || {
            runtime.execute_tool_blocking(&session_id, action, &input, &grants)
        })
        .await
        .map_err(|error| format!("browser tool background task failed: {error}"))?
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
                previous.hide(false)?;
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
                let _ = session.hide(false);
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
    pub fn hide(&self, session_id: &str, animate: bool) -> Result<BrowserStatus, String> {
        let session_id = validate_session_id(session_id)?.to_owned();
        let _lifecycle = lock_unpoison(&self.lifecycle);
        let epoch = {
            let state = lock_unpoison(&self.state);
            next_browser_lifecycle_epoch(&state, &session_id)?
        };
        self.hide_with_intent_locked(&session_id, animate, epoch)
    }

    /// Hides one exact conversation without revoking its browser/tool authority. Unlike Closed,
    /// Hidden retains the session and profile, but an older renderer can no longer show it again.
    pub fn hide_with_intent(
        &self,
        session_id: &str,
        animate: bool,
        epoch: u64,
    ) -> Result<BrowserStatus, String> {
        let session_id = validate_session_id(session_id)?.to_owned();
        validate_browser_lifecycle_epoch(epoch)?;
        let _lifecycle = lock_unpoison(&self.lifecycle);
        self.hide_with_intent_locked(&session_id, animate, epoch)
    }

    fn hide_with_intent_locked(
        &self,
        session_id: &str,
        animate: bool,
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
        let status = session.hide(animate)?;
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
        if session.hide(false).is_err() {
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
                previous.hide(false)?;
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
                let _ = session.hide(false);
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
            match session.hide(false) {
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

    /// Every live page of one conversation as `(tab, status)`, primary first
    /// then by session id — the same order `agent_tab_roster` uses, so the task
    /// tools and `playwright tab_list` never disagree about which page is which.
    ///
    /// Unlike `agent_tab_roster` this does not resolve or create a current tab:
    /// listing tasks must not have a side effect on which page the model drives.
    pub fn conversation_task_pages(&self, conversation_id: &str) -> Vec<(String, BrowserStatus)> {
        let state = lock_unpoison(&self.state);
        let mut sessions = state
            .sessions
            .iter()
            .filter(|(id, _)| {
                browser_conversation_owner(id) == conversation_id
                    && !state.closed_session_ids.contains(*id)
            })
            .map(|(id, session)| (id.clone(), session.status()))
            .collect::<Vec<_>>();
        sessions.sort_by(|(left, _), (right, _)| {
            browser_tab_id(left)
                .eq(AGENT_PRIMARY_TAB_ID)
                .cmp(&browser_tab_id(right).eq(AGENT_PRIMARY_TAB_ID))
                .reverse()
                .then_with(|| left.cmp(right))
        });
        sessions
            .into_iter()
            .map(|(id, status)| (browser_tab_id(&id).to_owned(), status))
            .collect()
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
            state.agent_tabs.clear();
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
            let mut sessions = state
                .sessions
                .values()
                .cloned()
                .collect::<Vec<_>>();
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
                return Err("this browser task page is being created or restored; try again shortly".into());
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
            return Err("this browser task page is being created or restored; try again shortly".into());
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
                && !snapshot.menu_expanded
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
                && !snapshot.menu_expanded
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
        .unwrap_or(AGENT_PRIMARY_TAB_ID)
}

/// Whether a session is a surface the user opened rather than one the Agent minted.
///
/// `playwright tab_new` is the only way the model gets a page of its own, and it always mints
/// `<conversation>#agent-<n>`. The conversation's primary page has no `#` suffix at all, and the
/// trusted UI mints extra user tabs as `<conversation>#tab_<uuid>`. So the `agent-` prefix is the
/// exact, non-overlapping marker for pages the model owns; everything else belongs to the user and
/// gets the takeover prompt before the Agent may drive it.
pub(crate) fn session_is_user_owned(session_id: &str) -> bool {
    !browser_tab_id(session_id).starts_with(AGENT_TAB_ID_PREFIX)
}

/// The tab id a user-facing prompt should name, matching what `playwright tab_list` shows the model.
pub(crate) fn browser_tab_label(session_id: &str) -> &str {
    browser_tab_id(session_id)
}

/// Conversation an Agent tab tool may act on. Tab tools compose ids by appending `#<tab>`, so a
/// conversation id that already carried a separator would let one call address a session outside
/// the conversation the tool executor authorized.
fn validate_agent_conversation_id(conversation_id: &str) -> Result<&str, String> {
    let conversation_id = validate_session_id(conversation_id)?;
    if conversation_id.contains('#') {
        return Err("browser tab tools must be called by the conversation itself, not a tab session".into());
    }
    Ok(conversation_id)
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
        if let Err(error) =
            remove_profile_with_retries_before(root, profile, Some(deadline))
        {
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

/// Every action gets a page (`ensureTab`), so any action on a page-less or suspended session
/// takes an awake slot before it runs.
fn browser_tool_needs_live_slot(_action: PlaywrightAction, status: &BrowserStatus) -> bool {
    !status.has_page
}

/// The argument checks each action performs before touching the page, run up front so a
/// malformed call is refused before a page is created for it. Each check calls the same helper
/// the action itself uses; the action repeats it, which keeps the two from drifting apart.
fn validate_action_input(
    action: PlaywrightAction,
    input: &Map<String, Value>,
    grants: &BrowserToolGrants,
    level: SecurityLevel,
) -> Result<(), String> {
    let selector = optional_input_string(input, "selector", MAX_SELECTOR_CHARS)?;
    let element_ref = optional_input_string(input, "ref", 32)?;
    let target = |optional: bool| {
        validate_target_input(selector.as_deref(), element_ref.as_deref(), optional)
    };
    match action {
        PlaywrightAction::Navigate => {
            let url = required_input_string(input, "url", MAX_URL_CHARS, false)?;
            if !matches!(url.trim().to_ascii_lowercase().as_str(), "back" | "forward" | "reload") {
                parse_browser_url(url.trim(), level)?;
            }
        }
        PlaywrightAction::Snapshot => {
            if let Some(max_chars) = optional_input_u64(input, "max_chars")? {
                let max_chars =
                    usize::try_from(max_chars).map_err(|_| "max_chars is too large".to_owned())?;
                if !(MIN_SNAPSHOT_CHARS..=MAX_SNAPSHOT_CHARS).contains(&max_chars) {
                    return Err(format!(
                        "snapshot max_chars must be between {MIN_SNAPSHOT_CHARS} and {MAX_SNAPSHOT_CHARS}"
                    ));
                }
            }
        }
        PlaywrightAction::Click => {
            target(false)?;
            let button =
                optional_input_string(input, "button", 16)?.unwrap_or_else(|| "left".to_owned());
            if !matches!(button.as_str(), "left" | "right" | "middle") {
                return Err(format!(
                    "playwright click does not support button {button} (allowed: left, right, middle)"
                ));
            }
            optional_input_bool(input, "double", false)?;
            modifier_bits(&parse_string_array_field(input, "modifiers")?)?;
        }
        PlaywrightAction::Type => {
            target(false)?;
            let text = required_input_string(input, "text", MAX_TEXT_INPUT_CHARS, true)?;
            optional_input_bool(input, "clear", true)?;
            optional_input_bool(input, "submit", false)?;
            let slowly = optional_input_bool(input, "slowly", false)?;
            if slowly && text.chars().count() > 2_000 {
                return Err("playwright type slowly mode accepts at most 2000 characters".into());
            }
        }
        PlaywrightAction::FillForm => {
            let fields = input
                .get("fields")
                .ok_or_else(|| "missing required parameter fields".to_owned())?;
            let fields = fields
                .as_array()
                .ok_or_else(|| "playwright fill_form fields must be an array".to_owned())?;
            if fields.is_empty() || fields.len() > MAX_FILL_FORM_FIELDS {
                return Err(format!(
                    "playwright fill_form fields must contain between 1 and {MAX_FILL_FORM_FIELDS} items"
                ));
            }
        }
        PlaywrightAction::Select => {
            target(false)?;
            let values = parse_select_values(input)?;
            if values.is_empty() || values.len() > 100 {
                return Err("playwright select values must contain between 1 and 100 values".into());
            }
        }
        PlaywrightAction::Hover => {
            target(false)?;
        }
        PlaywrightAction::Key => {
            let key = required_input_string(input, "key", MAX_KEY_CHARS, false)?;
            validate_key(&key)?;
            let repeat = optional_input_u64(input, "repeat")?.unwrap_or(1);
            if !(1..=MAX_KEY_REPEAT).contains(&repeat) {
                return Err(format!(
                    "playwright key repeat must be between 1 and {MAX_KEY_REPEAT}"
                ));
            }
            parse_key_spec(&key)?;
        }
        PlaywrightAction::Scroll => {
            for delta in [
                optional_input_f64(input, "x")?,
                optional_input_f64(input, "y")?,
            ]
            .into_iter()
            .flatten()
            {
                if !delta.is_finite() || delta.abs() > 10_000_000.0 {
                    return Err("playwright scroll x/y must be finite numbers with absolute values no greater than 10000000".into());
                }
            }
            target(true)?;
        }
        PlaywrightAction::Evaluate => {
            let script = required_input_string(input, "script", MAX_EVALUATE_CHARS, false)?;
            if script.trim().is_empty() {
                return Err("playwright evaluate script must not be empty".into());
            }
            if let Some(reference) = element_ref.as_deref() {
                validate_element_ref(reference)?;
            }
        }
        PlaywrightAction::Wait => {
            if let Some(selector) = selector.as_deref() {
                validate_selector(selector)?;
            }
            for (key, label) in [("text", "text"), ("text_gone", "text_gone")] {
                if optional_input_string(input, key, 65_536 + 1)?
                    .is_some_and(|value| value.chars().count() > 65_536)
                {
                    return Err(format!("playwright wait {label} exceeds the 65536-character limit"));
                }
            }
            optional_input_bool(input, "load", false)?;
            let timeout_ms = optional_input_u64(input, "timeout_ms")?.unwrap_or(DEFAULT_WAIT_TIMEOUT_MS);
            if !(1..=MAX_WAIT_TIMEOUT_MS).contains(&timeout_ms) {
                return Err(format!(
                    "playwright wait timeout_ms must be between 1 and {MAX_WAIT_TIMEOUT_MS}"
                ));
            }
        }
        PlaywrightAction::Screenshot => {
            required_input_string(input, "path", 4_096, false)?;
            optional_input_bool(input, "full_page", false)?;
            target(true)?;
            if grants.screenshot_path.is_none() {
                return Err(
                    "playwright screenshot is missing a save path validated by the path guard".into(),
                );
            }
        }
        PlaywrightAction::Console => {
            optional_input_bool(input, "only_errors", false)?;
            optional_input_u64(input, "limit")?;
            optional_input_bool(input, "clear", false)?;
        }
        PlaywrightAction::Network => {
            optional_input_string(input, "filter", 2_048)?;
            optional_input_u64(input, "limit")?;
            optional_input_bool(input, "clear", false)?;
        }
        PlaywrightAction::Dialog => {
            match input.get("accept") {
                None | Some(Value::Null) => {}
                Some(value) => {
                    value
                        .as_bool()
                        .ok_or_else(|| "parameter accept must be a boolean".to_owned())?;
                }
            }
            if optional_input_string(input, "prompt_text", 16_384 + 1)?
                .is_some_and(|text| text.chars().count() > 16_384)
            {
                return Err("playwright dialog prompt_text exceeds the 16384-character limit".into());
            }
        }
        PlaywrightAction::FileUpload => {
            target(true)?;
            input
                .get("paths")
                .ok_or_else(|| "missing required parameter paths".to_owned())?;
            if grants.upload_paths.is_none() {
                return Err(
                    "playwright file_upload is missing files validated by the path guard".into(),
                );
            }
        }
        PlaywrightAction::UploadImage => {
            target(true)?;
            if grants.upload_paths.is_none() {
                return Err(
                    "playwright upload_image is missing a host-materialized image file".into(),
                );
            }
        }
        PlaywrightAction::Resize => {
            let width = required_input_u32(input, "width")?;
            let height = required_input_u32(input, "height")?;
            if !(320..=7680).contains(&width) || !(240..=4320).contains(&height) {
                return Err("browser viewport must be 320..7680 × 240..4320".into());
            }
        }
        PlaywrightAction::TabNew
        | PlaywrightAction::TabList
        | PlaywrightAction::TabSelect
        | PlaywrightAction::TabClose
        | PlaywrightAction::Close => {}
    }
    Ok(())
}

fn validate_navigation_tool_before_capacity(
    input: &Map<String, Value>,
    session: &BrowserSession,
) -> Result<(), String> {
    let url = required_input_string(input, "url", MAX_URL_CHARS, false)?;
    match url.trim().to_ascii_lowercase().as_str() {
        "back" => {
            validate_history_navigation_before_capacity(&session.status(), PendingNavigation::Back)?
        }
        "forward" => validate_history_navigation_before_capacity(
            &session.status(),
            PendingNavigation::Forward,
        )?,
        "reload" => validate_history_navigation_before_capacity(
            &session.status(),
            PendingNavigation::Reload,
        )?,
        _ => {
            parse_browser_url(&url, session.security_level())?;
        }
    }
    Ok(())
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
            menu_window_region_order: WindowRegionOrder::default(),
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
            menu_expanded: state.menu_expanded,
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
                || state.menu_expanded
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
                || state.menu_expanded
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

        let full_region_ticket = self.menu_window_region_order.supersede_with_full_region()?;
        page.with_native_tail(|native, permit| {
            restore_full_region_ordered(native, permit, full_region_ticket)
        })?;
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
        state.cold_close_cookies = None;
        state.credential_takeover_grant = None;
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
        let full_region_ticket = self.menu_window_region_order.supersede_with_full_region()?;
        page.with_native_tail(|native, permit| {
            restore_full_region_ordered(native, permit, full_region_ticket)
        })?;
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
            let resume_url = if state.status.url.trim().is_empty() {
                DEFAULT_URL.to_owned()
            } else {
                state.status.url.clone()
            };
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
            state.menu_expanded = false;
            state.menu_control_before_open = None;
            state.menu_hole = None;
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
            sync_history_flags(&mut state);
        }

        if let Err(error) =
            self.retire_native_surface_and_wait(&app, host, false, Some(&teardown))
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
            return Err("the application is shutting down and the browser page has been released".into());
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
            let full_region_ticket = self.menu_window_region_order.supersede_with_full_region()?;
            page.with_native_tail(|native, permit| {
                restore_full_region_ordered(native, permit, full_region_ticket)
            })?;
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
                    let (panel_bounds, menu_expanded) = {
                        let state = self.lock_state();
                        (state.panel_bounds, state.menu_expanded)
                    };
                    let viewport = resize_page(
                        BrowserHost::MainPanel,
                        &window,
                        page,
                        window.inner_size().ok(),
                        panel_bounds,
                        menu_expanded,
                    );
                    self.lock_state().status.viewport = viewport;
                }
                page.show()
                    .map_err(|error| format!("显示内置浏览器页面失败: {error}"))?;
                page.unpark()?;
                page.set_focus()
                    .map_err(|error| format!("聚焦浏览器页面失败: {error}"))?;
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
                    state.menu_expanded = false;
                    state.menu_control_before_open = None;
                    state.menu_hole = None;
                    state.pending_navigation = None;
                    state.pending_previous_url = None;
                    state.cold_resume_target = None;
                    state.cold_close_cookies = cold_close_cookies;
                    state.credential_takeover_grant = None;
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
                _ => return Err("embedded browser Chromium profile capability state is incomplete".into()),
            }
        }

        let requested = self.profile_directory(app)?;
        std::fs::create_dir_all(&requested)
            .map_err(|error| format!("failed to create the browser profile directory: {error}"))?;
        let root = tab_profile_root(app)?;
        let profile = WebView2Profile::within_root(&requested, &root)
            .map_err(|error| format!("failed to acquire the embedded browser Chromium profile lease: {error}"))?;
        let lease = WebView2RuntimeLease::claim(profile)
            .map_err(|error| format!("failed to acquire the embedded browser Chromium profile lease: {error}"))?;
        let directory = lease.directory().to_path_buf();
        let issuer = lease.controller_issuer();

        let mut state = self.lock_state();
        if state.terminated {
            return Err("the application is shutting down and the browser page has been released".into());
        }
        if state.webview2_lease.is_some()
            || state.webview2_controller_issuer.is_some()
            || state.webview2_control.is_some()
            || state.webview2_teardown.is_some()
        {
            return Err("the embedded browser Chromium profile lease was concurrently replaced".into());
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
            state
                .webview2_controller_issuer
                .clone()
                .ok_or_else(|| "the embedded browser has not acquired Chromium controller issuance capability".to_owned())?
        };
        // Mint outside RuntimeState: replacing a controller waits for all old-generation permits,
        // and those operations may legitimately touch RuntimeState before they finish.
        let control = issuer
            .begin_controller()
            .map_err(|error| format!("failed to begin the WebView2 controller startup generation: {error}"))?;
        let mut state = self.lock_state();
        if state.terminated || state.webview2_lease.is_none() {
            drop(state);
            let _ = control.invalidate();
            return Err("the application is shutting down and the browser page has been released".into());
        }
        state.webview2_control = Some(control.clone());
        Ok(control)
    }

    fn webview2_control(&self) -> Result<WebView2Control, String> {
        self.lock_state()
            .webview2_control
            .clone()
            .ok_or_else(|| "the embedded browser has not acquired Chromium native control capability".to_owned())
    }

    fn invalidate_webview2_controller(
        &self,
    ) -> Result<Option<WebView2TeardownPermit>, String> {
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
                let layout =
                    browser_host_layout(&window, BrowserHost::MainPanel, panel_bounds, false);
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
            let (panel_bounds, menu_expanded) = {
                let state = self.lock_state();
                (state.panel_bounds, state.menu_expanded)
            };
            let viewport = resize_page(
                BrowserHost::MainPanel,
                &window,
                &page,
                window.inner_size().ok(),
                panel_bounds,
                menu_expanded,
            );
            self.lock_state().status.viewport = viewport;
        }
        page.show()
            .map_err(|error| format!("显示内置浏览器页面失败: {error}"))?;
        page.unpark()?;
        page.set_focus()
            .map_err(|error| format!("聚焦浏览器页面失败: {error}"))?;
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
        // A replacement WebView always starts with a full native region. Retire callbacks owned by
        // any previous controller before the new child can reuse this conversation's label.
        self.menu_window_region_order.supersede_with_full_region()?;
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

            let (panel_bounds, menu_expanded) = {
                let state = self.lock_state();
                (state.panel_bounds, state.menu_expanded)
            };
            let layout = browser_host_layout(&window, host, panel_bounds, menu_expanded);
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
                self.menu_window_region_order.clone(),
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
                eprintln!("browser page observers unavailable for {}: {error}", self.session_id);
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
            if let Err(error) = self.retire_native_surface_and_wait(
                app,
                Some(host),
                true,
                teardown.as_ref(),
            ) {
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
        let _ = self.menu_window_region_order.supersede_with_full_region();
        let host = {
            let mut state = self.lock_state();
            state.page_generation = state.page_generation.wrapping_add(1);
            state.layout_generation = state.layout_generation.wrapping_add(1);
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
        let _ = self.menu_window_region_order.supersede_with_full_region();
        let host = {
            let mut state = self.lock_state();
            state.page_generation = state.page_generation.wrapping_add(1);
            state.layout_generation = state.layout_generation.wrapping_add(1);
            state.status.has_page = false;
            state.status.open = false;
            state.status.loading = false;
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
            let status = state.status.clone();
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
            PendingNavigation::New => Err("new-page navigation cannot be executed as a history operation".into()),
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
    pub fn hide(&self, animate: bool) -> Result<BrowserStatus, String> {
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
        if let Some(page) = page.as_ref() {
            let ticket = self.menu_window_region_order.supersede_with_full_region()?;
            page.with_native_tail(|native, permit| {
                restore_full_region_ordered(native, permit, ticket)
            })?;
        } else {
            self.menu_window_region_order.supersede_with_full_region()?;
        }
        restore_control_after_menu(&mut self.lock_state());

        if animate && host == Some(BrowserHost::MainPanel) {
            if let (Some(window), Some(page)) = (app.get_window(MAIN_WINDOW_LABEL), page.as_ref()) {
                let panel_bounds = self.lock_state().panel_bounds;
                let layout =
                    browser_host_layout(&window, BrowserHost::MainPanel, panel_bounds, false);
                animate_page_horizontal(page, layout.x, layout.x + layout.width, layout);
            }
        }
        if let Some(page) = page.as_ref() {
            // A page hidden from the user stays live for the Agent (see `AttestedPage::park`);
            // only a sleeping page is truly hidden, by the sleep transition itself.
            match (host, app.get_window(MAIN_WINDOW_LABEL)) {
                (Some(BrowserHost::MainPanel), Some(window)) => {
                    let panel_bounds = self.lock_state().panel_bounds;
                    let layout =
                        browser_host_layout(&window, BrowserHost::MainPanel, panel_bounds, false);
                    page.park(layout)
                        .map_err(|error| format!("收起内置浏览器页面失败: {error}"))?;
                }
                // A detached host is hidden as a whole below; its controller stays visible so
                // the page keeps compositing.
                (Some(BrowserHost::DetachedWindow), _) => {}
                _ => {
                    page.hide()
                        .map_err(|error| format!("收起内置浏览器页面失败: {error}"))?;
                }
            }
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
        let (app, host, has_page, menu_expanded, menu_hole) = {
            let mut state = self.lock_state();
            state.panel_bounds = Some(bounds);
            (
                state.app.clone(),
                state.host,
                state.status.has_page,
                state.menu_expanded,
                state.menu_hole,
            )
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
                menu_expanded,
                !bounds.visible,
            );
            self.lock_state().status.viewport = viewport;
            if bounds.visible {
                if let Some(ticket) = self.menu_window_region_order.current_hole_ticket() {
                    // Only reapply geometry installed by this exact ticket. A newer renderer
                    // measurement may already be published but still waiting for `automation`;
                    // replaying the old rectangle under that new ticket could win out of order.
                    if let Some(hole) = menu_hole
                        .filter(|hole| menu_expanded && hole.order_identity == ticket.identity())
                    {
                        page.with_native_tail(|native, permit| {
                            apply_menu_hole_ordered(
                                native,
                                permit,
                                hole.rect,
                                window.scale_factor().unwrap_or(1.0).max(f64::EPSILON),
                                hole.shadow,
                                ticket,
                            )
                        })?;
                    }
                } else {
                    let ticket = self.menu_window_region_order.current_ticket();
                    page.with_native_tail(|native, permit| {
                        restore_full_region_ordered(native, permit, ticket)
                    })?;
                }
            } else {
                let ticket = self.menu_window_region_order.supersede_with_full_region()?;
                page.with_native_tail(|native, permit| {
                    restore_full_region_ordered(native, permit, ticket)
                })?;
                restore_control_after_menu(&mut self.lock_state());
            }
        }

        if bounds.visible {
            page.show()
                .map_err(|error| format!("显示内置浏览器页面失败: {error}"))?;
            page.unpark()?;
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
            let layout = browser_host_layout(&window, host, Some(bounds), menu_expanded);
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

    pub fn set_viewport(&self, width: u32, height: u32) -> Result<BrowserStatus, String> {
        if !(320..=7680).contains(&width) || !(240..=4320).contains(&height) {
            return Err("browser viewport must be 320..7680 × 240..4320".into());
        }
        let app = self.app_handle()?;
        let host = self
            .lock_state()
            .host
            .ok_or_else(|| "the embedded browser is not open".to_owned())?;
        let window_label = if host == BrowserHost::MainPanel {
            MAIN_WINDOW_LABEL
        } else {
            &self.labels.window
        };
        let window = app
            .get_window(window_label)
            .ok_or_else(|| "the embedded browser host window was lost".to_owned())?;
        if host == BrowserHost::DetachedWindow {
            window
                .set_size(LogicalSize::new(width as f64, height as f64))
                .map_err(|error| format!("failed to set the browser viewport: {error}"))?;
        }
        if app.get_webview(&self.labels.page).is_some() {
            let page = self.attested_page(true)?;
            let host_size = window
                .inner_size()
                .ok()
                .map(|size| {
                    let scale = window.scale_factor().unwrap_or(1.0).max(f64::EPSILON);
                    size.to_logical::<f64>(scale)
                })
                .unwrap_or(LogicalSize::new(width as f64, height as f64));
            let panel_bounds = self.lock_state().panel_bounds;
            let layout = if host == BrowserHost::MainPanel
                && panel_bounds.is_some_and(|bounds| bounds.width >= 1.0 && bounds.height >= 1.0)
            {
                browser_layout_for_size(host_size, host, panel_bounds, false)
            } else if host == BrowserHost::MainPanel {
                let effective_width = (width as f64)
                    .min(BROWSER_PANEL_WIDTH)
                    .min(host_size.width)
                    .max(1.0);
                let effective_height = (height as f64)
                    .min((host_size.height - BROWSER_TOOLBAR_HEIGHT).max(1.0))
                    .max(1.0);
                BrowserPageLayout {
                    x: (host_size.width - effective_width).max(0.0),
                    y: BROWSER_TOOLBAR_HEIGHT.min((host_size.height - 1.0).max(0.0)),
                    width: effective_width,
                    height: effective_height,
                }
            } else {
                BrowserPageLayout {
                    x: 0.0,
                    y: 0.0,
                    width: width as f64,
                    height: height as f64,
                }
            };
            let full_region_ticket = self.menu_window_region_order.supersede_with_full_region()?;
            page.with_native_tail(|native, permit| {
                restore_full_region_ordered(native, permit, full_region_ticket)
            })?;
            restore_control_after_menu(&mut self.lock_state());
            apply_page_layout(&page, layout);
            let mut state = self.lock_state();
            state.status.viewport = layout.viewport();
            return Ok(state.status.clone());
        }
        Err("the embedded browser WebView was lost".into())
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
            if state.menu_expanded {
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
        if state.menu_expanded {
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
        let url =
            Url::parse(&committed).map_err(|_| "could not parse the current page address to determine sign-in state".to_owned())?;
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
            .map_err(|_| "could not confirm whether the current page carries sign-in cookies".to_owned())?;
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

    fn begin_agent_control(&self, action: PlaywrightAction) -> Result<AgentControlGuard, String> {
        let tool_name = format!("playwright {action}");
        {
            let mut state = self.lock_state();
            if state.menu_expanded {
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
                    "{tool_name} is temporarily blocked because the user is using the browser menu; wait for the menu to close"
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

    pub fn set_menu_region(
        &self,
        request: BrowserMenuRegionRequest,
    ) -> Result<BrowserStatus, String> {
        if !request.expanded && request.rect.is_some() {
            return Err("关闭浏览器菜单时不能提交菜单区域".into());
        }
        if request
            .shadow
            .is_some_and(|shadow| !shadow.is_finite() || !(0.0..=64.0).contains(&shadow))
        {
            return Err("浏览器菜单阴影边距必须为 0..64 的有限数值".into());
        }
        let Some(ticket) = self.menu_window_region_order.claim(
            request.generation,
            request.expanded && request.rect.is_some(),
        )?
        else {
            return Ok(self.status());
        };
        self.set_menu_region_ordered(request, ticket)
    }

    fn set_menu_region_ordered(
        &self,
        request: BrowserMenuRegionRequest,
        ticket: WindowRegionTicket,
    ) -> Result<BrowserStatus, String> {
        // Publishing a newer generation happens before this lock. Thus a close immediately
        // invalidates an older WebView UI closure even when that closure is still timing out.
        if !ticket.is_current() {
            return Ok(self.status());
        }
        let _automation = lock_unpoison(&self.automation);
        if !ticket.is_current() {
            return Ok(self.status());
        }
        if request.expanded {
            self.hide_agent_pointer();
        }
        let app = self.app_handle()?;
        let host = self
            .lock_state()
            .host
            .ok_or_else(|| "the embedded browser is not open".to_owned())?;
        let window_label = if host == BrowserHost::MainPanel {
            MAIN_WINDOW_LABEL
        } else {
            &self.labels.window
        };
        let window = app
            .get_window(window_label)
            .ok_or_else(|| "the embedded browser host window was lost".to_owned())?;
        let page = self.attested_page(true)?;

        let hole = if request.expanded {
            request.rect.map(|rect| BrowserMenuHole {
                rect: BrowserWindowLogicalRect::new(rect.x, rect.y, rect.width, rect.height),
                shadow: request.shadow.unwrap_or(0.0),
                order_identity: ticket.identity(),
            })
        } else {
            None
        };

        let region_result = page.with_native_tail(|native, permit| match hole {
            Some(hole) => apply_menu_hole_ordered(
                native,
                permit,
                hole.rect,
                window.scale_factor().unwrap_or(1.0).max(f64::EPSILON),
                hole.shadow,
                ticket.clone(),
            ),
            None => restore_full_region_ordered(native, permit, ticket.clone()),
        });

        if let Err(error) = region_result {
            let Some(recovery_ticket) = self
                .menu_window_region_order
                .supersede_failed_with_full_region(&ticket)?
            else {
                return Ok(self.status());
            };
            let recovery_error = page
                .with_native_tail(|native, permit| {
                    restore_full_region_ordered(native, permit, recovery_ticket)
                })
                .err();
            let mut state = self.lock_state();
            // A menu hidden behind the remote child would be unusable. Roll ownership back and
            // restore it from Rust too: renderer disconnect must not leave a live native hole.
            restore_control_after_menu(&mut state);
            return Err(match recovery_error {
                Some(recovery_error) => {
                    format!("{error}; 恢复 Chromium 完整窗口区域失败: {recovery_error}")
                }
                None => error,
            });
        }
        if !ticket.is_current() {
            return Ok(self.status());
        }

        let mut state = self.lock_state();
        if request.expanded && !state.menu_expanded {
            let previous = state.status.control.clone();
            state.menu_control_before_open = Some(previous.clone());
            state.status.control = BrowserControlStatus {
                owner: BrowserControlOwner::User,
                updated_at_ms: Utc::now().timestamp_millis(),
                ..previous
            };
            state.menu_expanded = true;
        } else if !request.expanded {
            restore_control_after_menu(&mut state);
        }
        state.menu_hole = hole;
        drop(state);
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
            return Err("this browser task is suspended; call playwright navigate first or restore it from the task card".into());
        }
        let permit = self
            .webview2_control()?
            .permit()
            .map_err(|error| format!("Chromium native control capability is unavailable: {error}"))?;
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
    menu_window_region_order: WindowRegionOrder,
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
            let (panel_bounds, expanded, parked) = {
                let state = lock_unpoison(&state);
                (state.panel_bounds, state.menu_expanded, !state.status.open)
            };
            let viewport = resize_page_placed(
                host,
                &window,
                &page,
                Some(*size),
                panel_bounds,
                expanded,
                parked,
            );
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
            let (panel_bounds, expanded, parked) = {
                let state = lock_unpoison(&state);
                (state.panel_bounds, state.menu_expanded, !state.status.open)
            };
            let viewport = resize_page_placed(
                host,
                &window,
                &page,
                Some(*new_inner_size),
                panel_bounds,
                expanded,
                parked,
            );
            lock_unpoison(&state).status.viewport = viewport;
        }
        WindowEvent::Destroyed => {
            let Ok(_permit) = chromium_control.permit() else {
                return;
            };
            let _ = menu_window_region_order.supersede_with_full_region();
            let mut state = lock_unpoison(&state);
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
    menu_expanded: bool,
) -> BrowserViewport {
    resize_page_placed(host, window, page, physical_size, panel_bounds, menu_expanded, false)
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
    menu_expanded: bool,
    parked: bool,
) -> BrowserViewport {
    let size = physical_size
        .or_else(|| window.inner_size().ok())
        .unwrap_or_else(|| PhysicalSize::new(DEFAULT_WIDTH as u32, DEFAULT_HEIGHT as u32));
    let scale = window.scale_factor().unwrap_or(1.0).max(f64::EPSILON);
    let logical: LogicalSize<f64> = size.to_logical(scale);
    let layout = browser_layout_for_size(logical, host, panel_bounds, menu_expanded);
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
    menu_expanded: bool,
) -> BrowserPageLayout {
    let scale = window.scale_factor().unwrap_or(1.0).max(f64::EPSILON);
    let logical = window
        .inner_size()
        .map(|size| size.to_logical(scale))
        .unwrap_or(LogicalSize::new(DEFAULT_WIDTH, DEFAULT_HEIGHT));
    browser_layout_for_size(logical, host, panel_bounds, menu_expanded)
}

fn browser_layout_for_size(
    logical: LogicalSize<f64>,
    host: BrowserHost,
    panel_bounds: Option<BrowserPanelBounds>,
    _menu_expanded: bool,
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

fn animate_page_horizontal(page: &AttestedPage, from_x: f64, to_x: f64, layout: BrowserPageLayout) {
    if (from_x - to_x).abs() < f64::EPSILON {
        return;
    }
    let started = Instant::now();
    loop {
        let progress = (started.elapsed().as_secs_f64()
            / BROWSER_PANEL_ANIMATION_DURATION.as_secs_f64())
        .clamp(0.0, 1.0);
        // Fast ease-out, close to the CSS drawer curve without blocking the UI thread.
        let eased = 1.0 - (1.0 - progress).powi(3);
        let x = from_x + (to_x - from_x) * eased;
        apply_page_layout(page, BrowserPageLayout { x, ..layout });
        if progress >= 1.0 {
            break;
        }
        std::thread::sleep(BROWSER_PANEL_ANIMATION_FRAME);
    }
}

fn restore_control_after_menu(state: &mut RuntimeState) {
    if state.menu_expanded {
        if let Some(previous) = state.menu_control_before_open.take() {
            state.status.control = previous;
        }
    } else {
        state.menu_control_before_open = None;
    }
    state.menu_expanded = false;
    state.menu_hole = None;
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
    state.menu_expanded = false;
    state.menu_control_before_open = None;
    state.menu_hole = None;
    state.renderer_presentation_generation = None;
    state.history_index = None;
    state.pending_navigation = None;
    state.pending_previous_url = None;
    state.cold_resume_target = None;
    state.cold_close_cookies = None;
    state.credential_takeover_grant = None;
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
                                "WebView2 退出通知缺少对应 controller 的观察能力".to_owned(),
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
/// by `playwright navigate` or a trusted-UI open. Other navigation kinds intentionally keep
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

fn combine_cleanup_results(first: Result<(), String>, second: Result<(), String>) -> Result<(), String> {
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

fn agent_pointer_highlight_params(x: f64, y: f64) -> Value {
    json!({
        "quad": [
            x, y - AGENT_POINTER_RADIUS,
            x + AGENT_POINTER_RADIUS, y,
            x, y + AGENT_POINTER_RADIUS,
            x - AGENT_POINTER_RADIUS, y,
        ],
        "color": {"r": 124, "g": 92, "b": 255, "a": 0.24},
        "outlineColor": {"r": 162, "g": 139, "b": 255, "a": 0.98},
    })
}

impl BrowserSession {
    pub fn snapshot(&self, max_chars: Option<usize>) -> Result<Value, String> {
        let max_chars = max_chars.unwrap_or(DEFAULT_SNAPSHOT_CHARS);
        if !(MIN_SNAPSHOT_CHARS..=MAX_SNAPSHOT_CHARS).contains(&max_chars) {
            return Err(format!(
                "snapshot max_chars must be between {MIN_SNAPSHOT_CHARS} and {MAX_SNAPSHOT_CHARS}"
            ));
        }
        self.eval_value(
            &format!("return __state.snapshot({max_chars});"),
            EVAL_TIMEOUT,
        )
    }

    // ----- actionability -----------------------------------------------------------------

    /// Polls the in-page actionability probe until the target exists, is visible, has a settled
    /// rect, is enabled (when required), and is not covered at its interaction point. Returns the
    /// probe object, which carries a viewport-relative CSS point plus an element description.
    fn wait_actionable(&self, target: &TargetSpec, expect_enabled: bool) -> Result<Value, String> {
        let body = format!(
            "return __state.actionable({}, {}, {});",
            js_optional_literal(target.selector.as_deref())?,
            js_optional_literal(target.element_ref.as_deref())?,
            expect_enabled
        );
        let started = Instant::now();
        loop {
            let probe = self.eval_value(&body, EVAL_TIMEOUT)?;
            match probe.get("status").and_then(Value::as_str) {
                Some("ok") => return Ok(probe),
                Some("fatal") => {
                    return Err(probe
                        .get("reason")
                        .and_then(Value::as_str)
                        .unwrap_or("target resolution failed")
                        .to_owned())
                }
                _ => {}
            }
            if started.elapsed() >= ACTIONABILITY_TIMEOUT {
                return Err(format!(
                    "element did not become actionable within {} ms ({})",
                    ACTIONABILITY_TIMEOUT.as_millis(),
                    probe
                        .get("reason")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown reason")
                ));
            }
            std::thread::sleep(ACTIONABILITY_POLL);
        }
    }

    fn point_from_probe(probe: &Value) -> Result<(f64, f64), String> {
        match (
            probe.get("x").and_then(Value::as_f64),
            probe.get("y").and_then(Value::as_f64),
        ) {
            (Some(x), Some(y)) => Ok((x, y)),
            _ => Err("actionability probe did not return coordinates".into()),
        }
    }

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
            || requests.iter().any(|(_, request)| request.main_frame_navigation);
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

    /// Waits for the document a navigation just started, the way `page.goto` does: the call fails
    /// if `DOMContentLoaded` has not happened within `NAVIGATION_TIMEOUT`; after that, `load` is
    /// awaited for at most `NAVIGATION_LOAD_GRACE` and its absence is reported, not failed.
    ///
    /// Without DevTools events (non-Windows, synthetic test surfaces) the page's `loading` flag
    /// is the only signal, so `loading: false` is treated as loaded.
    fn wait_for_navigation(
        &self,
        watch: &ActionWatch,
        target: &str,
    ) -> Result<BrowserStatus, String> {
        let deadline = watch.started + NAVIGATION_TIMEOUT;
        let mut loading_seen = false;
        loop {
            let (status, has_events, document_ready, navigation_requested, modal) = {
                let state = self.lock_state();
                let has_events = state.activity.events_enabled;
                (
                    state.status.clone(),
                    has_events,
                    watch.dom_content_loaded_since(&state.activity)
                        || watch.loaded_since(&state.activity),
                    watch.navigated_since(&state.activity)
                        || watch
                            .requests_since(&state.activity)
                            .iter()
                            .any(|(_, request)| request.main_frame_navigation),
                    !state.activity.modal_states().is_empty(),
                )
            };
            if modal {
                return Ok(status);
            }
            if status.loading {
                loading_seen = true;
            }
            let navigation_started = loading_seen || navigation_requested;
            let ready = if has_events {
                document_ready || (loading_seen && !status.loading)
            } else {
                !status.loading && (loading_seen || !page_load_is_pending(&status))
            };
            // A same-document navigation (a fragment change, a history move within one
            // document) never starts a load; once the settle window has passed without any
            // sign of one, the page is already where it is going to be.
            let settled_without_load =
                !navigation_started && watch.started.elapsed() >= POST_ACTION_SETTLE;
            if ready || settled_without_load {
                break;
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "Timeout {}ms exceeded while navigating to {target}: the page did not reach DOMContentLoaded",
                    NAVIGATION_TIMEOUT.as_millis()
                ));
            }
            std::thread::sleep(POST_ACTION_POLL);
        }
        Ok(self.wait_for_load(NAVIGATION_LOAD_GRACE))
    }

    // ----- navigation --------------------------------------------------------------------

    /// `playwright navigate` absorbs the retired open/back/forward/reload tools. `url` accepts the
    /// sentinels `back`/`forward`/`reload`; anything else is validated and loaded, preparing a
    /// hidden page first if needed. Blocks until the new document is loaded enough to act on.
    pub fn navigate_tool(&self, url: &str) -> Result<Value, String> {
        let trimmed = url.trim();
        let watch = self.action_watch();
        match trimmed.to_ascii_lowercase().as_str() {
            "back" => {
                self.navigate_history_with_resume(PendingNavigation::Back)?;
            }
            "forward" => {
                self.navigate_history_with_resume(PendingNavigation::Forward)?;
            }
            "reload" => {
                self.navigate_history_with_resume(PendingNavigation::Reload)?;
            }
            _ => {
                let parsed = parse_browser_url(trimmed, self.security_level())?;
                if self.page().is_err() {
                    self.prepare(Some(parsed.as_str()))?;
                } else {
                    self.navigate_parsed(parsed)?;
                }
            }
        }
        let status = self.wait_for_navigation(&watch, trimmed)?;
        if let Some(blocked) = {
            let state = self.lock_state();
            (state.blocked_navigations != watch.blocked_navigations)
                .then(|| state.status.error.clone())
                .flatten()
        } {
            return Err(blocked);
        }
        to_json_value(status)
    }

    // ----- forms -------------------------------------------------------------------------

    fn set_checked(
        &self,
        selector: Option<&str>,
        element_ref: Option<&str>,
        wanted: bool,
    ) -> Result<Value, String> {
        let target = validate_target_input(selector, element_ref, false)?;
        let probe = self.wait_actionable(&target, true)?;
        let current = probe
            .pointer("/element/checked")
            .and_then(Value::as_bool)
            .ok_or_else(|| "target element is not a checkbox or radio".to_owned())?;
        if current == wanted {
            return Ok(json!({
                "element": probe.get("element").cloned().unwrap_or(Value::Null),
                "changed": false,
            }));
        }
        self.click_tool(selector, element_ref, "left", false, &[])?;
        let element = self.describe_target(&target).unwrap_or(Value::Null);
        if element.get("checked").and_then(Value::as_bool) != Some(wanted) {
            return Err("checked state did not change after clicking (the page may have intercepted the action)".into());
        }
        Ok(json!({"element": element, "changed": true}))
    }

    /// `playwright fill_form` fills several controls in one call. Fields run in order; the first
    /// failure stops the batch so the model sees exactly how far the form got.
    pub fn fill_form(&self, fields: &Value) -> Result<Value, String> {
        let fields = fields
            .as_array()
            .ok_or_else(|| "playwright fill_form fields must be an array".to_owned())?;
        if fields.is_empty() || fields.len() > MAX_FILL_FORM_FIELDS {
            return Err(format!(
                "playwright fill_form fields must contain between 1 and {MAX_FILL_FORM_FIELDS} items"
            ));
        }
        let mut results = Vec::new();
        for (index, field) in fields.iter().enumerate() {
            let label = field
                .get("ref")
                .and_then(Value::as_str)
                .or_else(|| field.get("selector").and_then(Value::as_str))
                .unwrap_or("<unspecified>")
                .to_owned();
            match self.fill_one_field(field) {
                Ok(value) => results.push(json!({"target": label, "ok": true, "result": value})),
                Err(error) => {
                    results.push(json!({"target": label, "ok": false, "error": error}));
                    return Ok(json!({
                        "fields": results,
                        "completed": index,
                        "total": fields.len(),
                        "error": format!("field {} failed to fill; later fields were not executed", index + 1),
                    }));
                }
            }
        }
        Ok(json!({"fields": results, "completed": fields.len(), "total": fields.len()}))
    }

    fn fill_one_field(&self, field: &Value) -> Result<Value, String> {
        let object = field
            .as_object()
            .ok_or_else(|| "every fields item must be an object".to_owned())?;
        let selector = object
            .get("selector")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let element_ref = object.get("ref").and_then(Value::as_str).map(str::to_owned);
        let selector = selector.as_deref();
        let element_ref = element_ref.as_deref();
        if let Some(values) = object.get("values") {
            let values = parse_string_or_array(values, "values")?;
            return self.select(selector, element_ref, &values);
        }
        if let Some(checked) = object.get("checked") {
            let wanted = checked
                .as_bool()
                .ok_or_else(|| "checked must be a boolean".to_owned())?;
            return self.set_checked(selector, element_ref, wanted);
        }
        if let Some(value) = object.get("value") {
            let text = value
                .as_str()
                .ok_or_else(|| "value must be a string".to_owned())?;
            let kind = self
                .eval_value(
                    &format!(
                        "return __state.classifyControl({}, {});",
                        js_optional_literal(selector)?,
                        js_optional_literal(element_ref)?
                    ),
                    EVAL_TIMEOUT,
                )?
                .as_str()
                .unwrap_or("other")
                .to_owned();
            return match kind.as_str() {
                "select" => self.select(selector, element_ref, &[text.to_owned()]),
                "toggle" => Err("use checked rather than value for checkbox and radio controls".into()),
                "file" => Err("use playwright file_upload for file inputs".into()),
                _ => self.type_tool(selector, element_ref, text, true, false, false),
            };
        }
        Err("every fields item requires one of value, values, or checked".into())
    }

    fn describe_target(&self, target: &TargetSpec) -> Result<Value, String> {
        self.eval_value(
            &format!(
                "return __state.describeTarget({}, {});",
                js_optional_literal(target.selector.as_deref())?,
                js_optional_literal(target.element_ref.as_deref())?
            ),
            EVAL_TIMEOUT,
        )
    }

    /// Shows a short-lived pointer in Chromium's trusted DevTools overlay.
    ///
    /// Unlike the old page-injected DOM marker, this quad cannot be hidden, forged, observed, or
    /// restyled by remote page JavaScript. The React toolbar pill remains the textual identity
    /// anchor; this best-effort marker only points out the current action location.
    fn show_agent_pointer(&self, x: f64, y: f64) {
        if !x.is_finite() || !y.is_finite() {
            return;
        }
        // The overlay exists for the user watching the page; a parked page has no watcher, and
        // its two DevTools round trips would only delay the action.
        if !self.lock_state().status.open {
            return;
        }
        let overlay = lock_unpoison(&self.agent_pointer_overlay);
        let generation = self.next_agent_pointer_generation();
        #[cfg(windows)]
        {
            if self
                .cdp_call("Overlay.enable", &json!({}), AGENT_POINTER_CDP_TIMEOUT)
                .is_err()
            {
                let _ = self.hide_agent_pointer_overlay();
                return;
            }
            if self
                .cdp_call(
                    "Overlay.highlightQuad",
                    &agent_pointer_highlight_params(x, y),
                    AGENT_POINTER_CDP_TIMEOUT,
                )
                .is_err()
            {
                let _ = self.hide_agent_pointer_overlay();
                return;
            }
            drop(overlay);

            let session = self.clone();
            std::thread::spawn(move || {
                std::thread::sleep(AGENT_POINTER_LIFETIME);
                let _overlay = lock_unpoison(&session.agent_pointer_overlay);
                if session.agent_pointer_generation_is_current(generation) {
                    let _ = session.hide_agent_pointer_overlay();
                }
            });
        }
        #[cfg(not(windows))]
        {
            let _ = (generation, overlay);
        }
    }

    fn next_agent_pointer_generation(&self) -> u64 {
        self.agent_pointer_generation
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1)
    }

    fn agent_pointer_generation_is_current(&self, generation: u64) -> bool {
        self.agent_pointer_generation.load(Ordering::Acquire) == generation
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

    /// `playwright dialog` answers the dialog the page is currently blocked on. Dialogs are held
    /// natively (the page's JavaScript stays blocked in `alert`/`confirm`/`prompt`, exactly as
    /// in a real browser) until this action accepts or dismisses them; while one is held every
    /// other page action is refused. Without a held dialog the action is an error, as in
    /// `@playwright/mcp`, and the recent dialog records are returned with it.
    pub fn dialog_tool(
        &self,
        accept: Option<bool>,
        prompt_text: Option<&str>,
    ) -> Result<Value, String> {
        if prompt_text.is_some_and(|text| text.chars().count() > 16_384) {
            return Err("playwright dialog prompt_text exceeds the 16384-character limit".into());
        }
        if cfg!(not(windows)) {
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
                "playwright dialog can only be used while the page has a dialog open; there is none right now. Recent dialogs: {recent}"
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
            state.activity.record_dialog(&pending.kind, &pending.message, Some(accept));
        }
        Ok(json!({
            "dialog": pending,
            "accepted": accept,
            "promptText": prompt_text,
        }))
    }

    // ----- file upload -------------------------------------------------------------------

    /// `playwright file_upload` sets already-guard-approved host files onto an `input[type=file]`.
    /// File inputs are frequently hidden behind styled buttons, so visibility is not required.
    ///
    /// When the page opened a file chooser (a click on the input, or a scripted picker) the
    /// chooser is being held and the files are set on the element it belongs to; a target is
    /// then optional and, when given, must be that same element.
    pub fn file_upload(
        &self,
        selector: Option<&str>,
        element_ref: Option<&str>,
        validated_paths: &[PathBuf],
    ) -> Result<Value, String> {
        let target = validate_target_input(selector, element_ref, true)?;
        if validated_paths.is_empty() || validated_paths.len() > MAX_UPLOAD_FILES {
            return Err(format!(
                "playwright file_upload requires between 1 and {MAX_UPLOAD_FILES} files"
            ));
        }
        let files: Vec<String> = validated_paths
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect();
        let chooser = self.lock_state().activity.pending_file_chooser.clone();
        let has_target = target.selector.is_some() || target.element_ref.is_some();
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
                    "playwright file_upload needs a selector or ref for the file input unless the page has a file chooser open".into(),
                );
            }
        };
        self.cdp_call("DOM.setFileInputFiles", &params, EVAL_TIMEOUT)?;
        if let Some(chooser) = chooser {
            let mut state = self.lock_state();
            if state.activity.pending_file_chooser.as_ref().is_some_and(|pending| {
                pending.opened_at_ms == chooser.opened_at_ms
            }) {
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

    /// Captures a single element as PNG by clipping the page to its box. Used by
    /// `playwright screenshot` when a selector/ref is supplied.
    fn capture_region_png(
        &self,
        selector: Option<&str>,
        element_ref: Option<&str>,
    ) -> Result<BrowserPngCapture, String> {
        let target = validate_target_input(selector, element_ref, false)?;
        self.wait_actionable(&target, false)?;
        let clip = self.eval_value(
            &format!(
                "return __state.elementClip({}, {});",
                js_optional_literal(target.selector.as_deref())?,
                js_optional_literal(target.element_ref.as_deref())?
            ),
            EVAL_TIMEOUT,
        )?;
        let number = |key: &str| clip.get(key).and_then(Value::as_f64);
        let (x, y, width, height) =
            match (number("x"), number("y"), number("width"), number("height")) {
                (Some(x), Some(y), Some(width), Some(height)) if width >= 1.0 && height >= 1.0 => {
                    (x, y, width, height)
                }
                _ => return Err("target element has no screenshotable dimensions".into()),
            };
        self.capture_png_clip(
            false,
            Some(json!({ "x": x, "y": y, "width": width, "height": height, "scale": 1 })),
        )
    }

    pub fn click_tool(
        &self,
        selector: Option<&str>,
        element_ref: Option<&str>,
        button: &str,
        double: bool,
        modifiers: &[String],
    ) -> Result<Value, String> {
        let target = validate_target_input(selector, element_ref, false)?;
        if !matches!(button, "left" | "right" | "middle") {
            return Err(format!(
                "playwright click does not support button {button} (allowed: left, right, middle)"
            ));
        }
        let modifier_mask = modifier_bits(modifiers)?;
        let probe = self.wait_actionable(&target, true)?;
        let (x, y) = Self::point_from_probe(&probe)?;
        self.show_agent_pointer(x, y);
        let selector_literal = js_optional_literal(target.selector.as_deref())?;
        let ref_literal = js_optional_literal(target.element_ref.as_deref())?;
        let button_literal = js_string_literal(button)?;
        let checkpoint = self
            .eval_value("return __state.clickCheckpoint();", EVAL_TIMEOUT)?
            .as_u64()
            .unwrap_or_default();
        let watch = self.action_watch();
        let mut delivered = self.dispatch_click(x, y, button, double, modifier_mask)?;
        if delivered {
            // A click that already started a navigation has proven itself; asking the old
            // document (or the new one) about it would only race the load.
            let navigated = {
                let state = self.lock_state();
                page_load_is_pending(&state.status)
                    || watch.navigated_since(&state.activity)
                    || watch
                        .requests_since(&state.activity)
                        .iter()
                        .any(|(_, request)| request.main_frame_navigation)
            };
            if !navigated {
                delivered = match self.eval_value(
                    &format!(
                        "return __state.trustedClickObserved({selector_literal}, {ref_literal}, {checkpoint}, {button_literal});"
                    ),
                    EVAL_TIMEOUT,
                ) {
                    Ok(observed) => observed.as_bool().unwrap_or(false),
                    // The document went away underneath the probe: the click did that.
                    Err(error) if error == MODAL_STATE_INTERRUPTED => true,
                    Err(error) if error.contains("not found in the current page snapshot") => true,
                    Err(error) => return Err(error),
                };
            }
        }
        let input_mode = if delivered {
            "input-pipeline"
        } else {
            if button != "left" || double || modifier_mask != 0 {
                return Err(
                    "the embedded browser on this platform supports only ordinary left clicks (right clicks, double clicks, and modifiers require Windows WebView2)"
                        .into(),
                );
            }
            self.eval_value(
                &format!("return __state.syntheticClick({selector_literal}, {ref_literal});"),
                EVAL_TIMEOUT,
            )?;
            "synthetic"
        };
        Ok(json!({
            "element": probe.get("element").cloned().unwrap_or(Value::Null),
            "button": button,
            "double": double,
            "input": input_mode,
        }))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn type_tool(
        &self,
        selector: Option<&str>,
        element_ref: Option<&str>,
        text: &str,
        clear: bool,
        submit: bool,
        slowly: bool,
    ) -> Result<Value, String> {
        let target = validate_target_input(selector, element_ref, false)?;
        if text.chars().count() > MAX_TEXT_INPUT_CHARS {
            return Err(format!(
                "playwright type text exceeds the {MAX_TEXT_INPUT_CHARS}-character limit"
            ));
        }
        if slowly && text.chars().count() > 2_000 {
            return Err("playwright type slowly mode accepts at most 2000 characters".into());
        }
        let probe = self.wait_actionable(&target, true)?;
        let (x, y) = Self::point_from_probe(&probe)?;
        self.show_agent_pointer(x, y);
        let selector_literal = js_optional_literal(target.selector.as_deref())?;
        let ref_literal = js_optional_literal(target.element_ref.as_deref())?;

        // The element is focused (and its selection prepared) in-page; on Windows the characters
        // then arrive through the trusted input pipeline. Fill uses one Input.insertText; slowly
        // uses per-character key events. Non-Windows falls back to a native value setter.
        self.eval_value(
            &format!("return __state.prepareFill({selector_literal}, {ref_literal}, {clear});"),
            EVAL_TIMEOUT,
        )?;
        let expected_value = probe
            .pointer("/element/value")
            .and_then(Value::as_str)
            .filter(|_| probe.pointer("/element/type").and_then(Value::as_str) != Some("password"))
            .map(|previous| {
                if clear {
                    text.to_owned()
                } else {
                    format!("{previous}{text}")
                }
            });
        let mut delivered = if slowly {
            self.dispatch_typing(text)?
        } else {
            self.dispatch_fill(text)?
        };
        if !delivered {
            self.eval_value(
                &format!(
                    "return __state.setValue({selector_literal}, {ref_literal}, {}, {clear});",
                    js_string_literal(text)?
                ),
                EVAL_TIMEOUT,
            )?;
        }
        let mut element = self.describe_target(&target).unwrap_or(Value::Null);
        if delivered
            && expected_value.as_deref().is_some_and(|expected| {
                element.get("value").and_then(Value::as_str) != Some(expected)
            })
        {
            // A hidden WebView2 controller can accept Input.dispatchKeyEvent while dropping the
            // actual text insertion. Verify the postcondition and fall back to the native value
            // setter instead of reporting a successful no-op.
            self.eval_value(
                &format!(
                    "return __state.setValue({selector_literal}, {ref_literal}, {}, true);",
                    js_string_literal(expected_value.as_deref().unwrap_or_default())?
                ),
                EVAL_TIMEOUT,
            )?;
            delivered = false;
            element = self.describe_target(&target).unwrap_or(Value::Null);
        }
        if submit {
            self.key_tool("Enter", 1)?;
        }
        Ok(json!({
            "element": element,
            "slowly": slowly,
            "input": if delivered { "input-pipeline" } else { "synthetic" },
        }))
    }

    pub fn select(
        &self,
        selector: Option<&str>,
        element_ref: Option<&str>,
        values: &[String],
    ) -> Result<Value, String> {
        let target = validate_target_input(selector, element_ref, false)?;
        if values.is_empty() || values.len() > 100 {
            return Err("playwright select values must contain between 1 and 100 values".into());
        }
        if values.iter().any(|value| value.chars().count() > 16_384) {
            return Err("an individual playwright select value exceeds the 16384-character limit".into());
        }
        let probe = self.wait_actionable(&target, true)?;
        let (x, y) = Self::point_from_probe(&probe)?;
        self.show_agent_pointer(x, y);
        let body = format!(
            r#"
const element = __state.resolve({}, {});
if (!(element instanceof HTMLSelectElement)) throw new Error("target element is not a select");
if (element.disabled) throw new Error("target select is disabled");
const values = new Set({});
const found = new Set();
for (const option of element.options) {{
  option.selected = values.has(option.value);
  if (option.selected) found.add(option.value);
}}
const missing = [...values].filter(value => !found.has(value));
if (missing.length) throw new Error("select does not contain values: " + missing.join(", "));
element.dispatchEvent(new Event("input", {{bubbles:true}}));
element.dispatchEvent(new Event("change", {{bubbles:true}}));
return __state.describe(element);
"#,
            js_optional_literal(target.selector.as_deref())?,
            js_optional_literal(target.element_ref.as_deref())?,
            serde_json::to_string(values)
                .map_err(|error| format!("failed to serialize select values: {error}"))?
        );
        self.eval_value(&body, EVAL_TIMEOUT)
    }

    pub fn hover_tool(
        &self,
        selector: Option<&str>,
        element_ref: Option<&str>,
    ) -> Result<Value, String> {
        let target = validate_target_input(selector, element_ref, false)?;
        let probe = self.wait_actionable(&target, false)?;
        let (x, y) = Self::point_from_probe(&probe)?;
        self.show_agent_pointer(x, y);
        let input_mode = if self.dispatch_mouse_move(x, y)? {
            "input-pipeline"
        } else {
            // Synthetic MouseEvents cannot flip the CSS :hover state, but they still fire the
            // JS listeners many menus rely on. Real :hover only works on the CDP path.
            self.eval_value(
                &format!(
                    "return __state.syntheticHover({}, {});",
                    js_optional_literal(target.selector.as_deref())?,
                    js_optional_literal(target.element_ref.as_deref())?
                ),
                EVAL_TIMEOUT,
            )?;
            "synthetic"
        };
        Ok(json!({
            "element": probe.get("element").cloned().unwrap_or(Value::Null),
            "input": input_mode,
        }))
    }

    pub fn key_tool(&self, key: &str, repeat: u64) -> Result<Value, String> {
        validate_key(key)?;
        if !(1..=MAX_KEY_REPEAT).contains(&repeat) {
            return Err(format!(
                "playwright key repeat must be between 1 and {MAX_KEY_REPEAT}"
            ));
        }
        let spec = parse_key_spec(key)?;
        let mut input_mode = "input-pipeline";
        for _ in 0..repeat {
            if !self.dispatch_key_press(&spec)? {
                input_mode = "synthetic";
                break;
            }
        }
        if input_mode == "synthetic" {
            // Fallback platforms replay the old synthetic key path (Enter submits, Tab moves
            // focus, Escape blurs), which approximates native behavior without CDP.
            let body = format!("return __state.syntheticKey({});", js_string_literal(key)?);
            for _ in 0..repeat {
                self.eval_value(&body, EVAL_TIMEOUT)?;
            }
        }
        Ok(json!({
            "key": spec.key,
            "repeat": repeat,
            "input": input_mode,
        }))
    }

    pub fn scroll_tool(
        &self,
        x: Option<f64>,
        y: Option<f64>,
        selector: Option<&str>,
        element_ref: Option<&str>,
    ) -> Result<Value, String> {
        for delta in [x, y].into_iter().flatten() {
            if !delta.is_finite() || delta.abs() > 10_000_000.0 {
                return Err("playwright scroll x/y must be finite numbers with absolute values no greater than 10000000".into());
            }
        }
        let target = validate_target_input(selector, element_ref, true)?;
        let has_target = target.selector.is_some() || target.element_ref.is_some();

        // A target without explicit deltas just scrolls that element into view.
        if has_target && x.is_none() && y.is_none() {
            return self.eval_value(
                &format!(
                    "return __state.scrollTo({}, {});",
                    js_optional_literal(target.selector.as_deref())?,
                    js_optional_literal(target.element_ref.as_deref())?
                ),
                EVAL_TIMEOUT,
            );
        }

        let delta_x = x.unwrap_or(0.0);
        let delta_y = y.unwrap_or(600.0);
        let (wheel_x, wheel_y) = if has_target {
            let probe = self.wait_actionable(&target, false)?;
            Self::point_from_probe(&probe)?
        } else {
            let viewport = self.status().viewport;
            (viewport.width as f64 / 2.0, viewport.height as f64 / 2.0)
        };
        self.show_agent_pointer(wheel_x, wheel_y);
        // A real wheel event scrolls whatever container is under the point (nested scrollers,
        // lazy-load triggers), which window.scrollBy cannot reach.
        if self.dispatch_wheel(wheel_x, wheel_y, delta_x, delta_y)? {
            std::thread::sleep(Duration::from_millis(80));
            return self.eval_value("return __state.scrollReport();", EVAL_TIMEOUT);
        }
        self.eval_value(
            &format!(
                "return __state.scrollBy({}, {}, {delta_x}, {delta_y});",
                js_optional_literal(target.selector.as_deref())?,
                js_optional_literal(target.element_ref.as_deref())?
            ),
            EVAL_TIMEOUT,
        )
    }

    pub fn evaluate_tool(&self, source: &str, element_ref: Option<&str>) -> Result<Value, String> {
        if source.trim().is_empty() {
            return Err("playwright evaluate script must not be empty".into());
        }
        if source.chars().count() > MAX_EVALUATE_CHARS {
            return Err(format!(
                "playwright evaluate script exceeds the {MAX_EVALUATE_CHARS}-character limit"
            ));
        }
        let element_ref = match element_ref {
            Some(reference) => Some(validate_element_ref(reference)?),
            None => None,
        };

        #[cfg(windows)]
        {
            // Runtime.evaluate with replMode gives console semantics: a statement program with a
            // completion value plus top-level await, and it is unaffected by strict page CSP (the
            // legacy eval()-in-page path broke on CSP sites and rejected all promises).
            let expression = match &element_ref {
                Some(reference) => format!(
                    "(async () => {{ const element = window.__MEWORK_BROWSER_RUNTIME__.evalElement({}); return (\n{source}\n); }})()",
                    js_string_literal(reference)?
                ),
                None => source.to_owned(),
            };
            let mut response = self.cdp_call(
                "Runtime.evaluate",
                &json!({
                    "expression": expression,
                    "awaitPromise": true,
                    "replMode": true,
                    "returnByValue": false,
                    "userGesture": true,
                }),
                EVAL_TIMEOUT,
            )?;
            // Some WebView2 builds return the Promise remote object even with awaitPromise=true
            // when the expression is an async wrapper used for an element ref. Resolve it
            // explicitly so callers receive the completion value rather than a serialized `{}`.
            let promise_object_id = response
                .pointer("/result/objectId")
                .and_then(Value::as_str)
                .filter(|_| {
                    response
                        .pointer("/result/className")
                        .and_then(Value::as_str)
                        == Some("Promise")
                })
                .map(str::to_owned);
            if let Some(promise_object_id) = promise_object_id {
                response = self.cdp_call(
                    "Runtime.awaitPromise",
                    &json!({
                        "promiseObjectId": promise_object_id,
                        "returnByValue": false,
                    }),
                    EVAL_TIMEOUT,
                )?;
                let _ = self.cdp_call(
                    "Runtime.releaseObject",
                    &json!({ "objectId": promise_object_id }),
                    EVAL_TIMEOUT,
                );
            }
            if let Some(details) = response.get("exceptionDetails") {
                let description = details
                    .pointer("/exception/description")
                    .and_then(Value::as_str)
                    .or_else(|| details.get("text").and_then(Value::as_str))
                    .unwrap_or("unknown JavaScript error");
                return Err(format!("browser script failed: {}", truncate(description, 2_048)));
            }
            let result = response.get("result").cloned().unwrap_or(Value::Null);
            if let Some(object_id) = result.get("objectId").and_then(Value::as_str) {
                // Serialize the remote object through the page runtime's budgeted serializer.
                let serialized = self.cdp_call(
                    "Runtime.callFunctionOn",
                    &json!({
                        "objectId": object_id,
                        "functionDeclaration":
                            "function() { const s = window.__MEWORK_BROWSER_RUNTIME__; return s ? s.serialize(this) : null; }",
                        "returnByValue": true,
                    }),
                    EVAL_TIMEOUT,
                )?;
                let _ = self.cdp_call(
                    "Runtime.releaseObject",
                    &json!({ "objectId": object_id }),
                    EVAL_TIMEOUT,
                );
                return Ok(serialized
                    .pointer("/result/value")
                    .cloned()
                    .unwrap_or(Value::Null));
            }
            Ok(result.get("value").cloned().unwrap_or(Value::Null))
        }

        #[cfg(not(windows))]
        {
            let body = format!(
                r#"
const __source = {};
let __result;
if ({}) {{
  const element = __state.evalElement({});
  __result = eval(__source);
}} else {{
  __result = (0, eval)(__source);
}}
if (__result && typeof __result.then === "function") throw new Error("playwright evaluate on this platform does not support returned Promises; use a synchronous script instead");
return __result;
"#,
                js_string_literal(source)?,
                element_ref.is_some(),
                js_optional_literal(element_ref.as_deref())?
            );
            self.eval_value(&body, EVAL_TIMEOUT)
        }
    }

    pub fn wait(
        &self,
        selector: Option<&str>,
        text: Option<&str>,
        text_gone: Option<&str>,
        load: bool,
        timeout_ms: Option<u64>,
    ) -> Result<Value, String> {
        let selector = match selector {
            Some(value) => Some(validate_selector(value)?),
            None => None,
        };
        let bounded = |value: Option<&str>, label: &str| -> Result<Option<String>, String> {
            value
                .map(|value| {
                    if value.chars().count() > 65_536 {
                        Err(format!("playwright wait {label} exceeds the 65536-character limit"))
                    } else {
                        Ok(value.to_owned())
                    }
                })
                .transpose()
        };
        let text = bounded(text, "text")?;
        let text_gone = bounded(text_gone, "text_gone")?;
        let timeout_ms = timeout_ms.unwrap_or(DEFAULT_WAIT_TIMEOUT_MS);
        if !(1..=MAX_WAIT_TIMEOUT_MS).contains(&timeout_ms) {
            return Err(format!(
                "playwright wait timeout_ms must be between 1 and {MAX_WAIT_TIMEOUT_MS}"
            ));
        }
        if selector.is_none() && text.is_none() && text_gone.is_none() && !load {
            let started = Instant::now();
            std::thread::sleep(Duration::from_millis(timeout_ms));
            return Ok(json!({
                "matched": true,
                "elapsedMs": started.elapsed().as_millis().min(u64::MAX as u128) as u64,
                "condition": "delay"
            }));
        }
        let body = format!(
            r#"
const selector = {};
const wantedText = {};
const goneText = {};
const needsLoad = {};
let element = null;
if (selector !== null) element = document.querySelector(selector);
const selectorReady = selector === null || (element !== null && element.getClientRects().length > 0);
const haystack = String((element || document.body || document.documentElement)?.innerText || "");
const textReady = wantedText === null || haystack.includes(wantedText);
const goneReady = goneText === null || !haystack.includes(goneText);
const loadReady = !needsLoad || document.readyState === "complete";
return {{ready:selectorReady && textReady && goneReady && loadReady, selectorReady, textReady, goneReady, loadReady, readyState:document.readyState, element:element ? __state.describe(element) : null}};
"#,
            js_optional_literal(selector.as_deref())?,
            js_optional_literal(text.as_deref())?,
            js_optional_literal(text_gone.as_deref())?,
            load
        );
        let started = Instant::now();
        loop {
            // A navigation in flight has no document to probe yet, and evaluating against the
            // outgoing one would answer about a page that is already gone. Let the host-observed
            // load settle first; only `load` cares, since the other conditions describe content
            // that only exists once a document does.
            if load && page_load_is_pending(&self.status()) {
                if started.elapsed() >= Duration::from_millis(timeout_ms) {
                    return Err(format!(
                        "timed out waiting for browser condition ({timeout_ms} ms): page is still loading"
                    ));
                }
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
            let value = self.eval_value(&body, EVAL_TIMEOUT)?;
            if value.get("ready").and_then(Value::as_bool).unwrap_or(false) {
                return Ok(json!({
                    "matched": true,
                    "elapsedMs": started.elapsed().as_millis().min(u64::MAX as u128) as u64,
                    "condition": value
                }));
            }
            if started.elapsed() >= Duration::from_millis(timeout_ms) {
                return Err(format!(
                    "timed out waiting for browser condition ({} ms): selector={} text={} text_gone={} load={}",
                    timeout_ms,
                    selector.as_deref().unwrap_or("<none>"),
                    text.as_deref().unwrap_or("<none>"),
                    text_gone.as_deref().unwrap_or("<none>"),
                    load
                ));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// Async adapter for Tauri commands. All blocking WebView2 calls and callback waits happen on
    /// Tauri's blocking pool, never on the UI thread.
    #[allow(dead_code)] // Tauri command adapter; the current model executor uses the blocking form.
    pub async fn execute_tool(
        &self,
        action: PlaywrightAction,
        input: &Map<String, Value>,
        grants: BrowserToolGrants,
    ) -> Result<Value, String> {
        let runtime = self.clone();
        let input = input.clone();
        tauri::async_runtime::spawn_blocking(move || {
            runtime.execute_tool_blocking(action, &input, &grants)
        })
        .await
        .map_err(|error| format!("browser tool background task failed: {error}"))?
    }

    /// Synchronous agent-executor adapter. Call this only from an existing worker/blocking thread;
    /// use [`execute_tool`](Self::execute_tool) from Tauri commands.
    ///
    /// The lifecycle around the action follows `@playwright/mcp`: a page is created (or a
    /// suspended one resumed, or a crashed one reset to a blank document) before any action needs
    /// it, a held dialog or file chooser refuses every action but the one that clears it, and an
    /// interaction answers only after its consequences settled, carrying the page's state so the
    /// model does not need a second call to see what changed.
    pub fn execute_tool_blocking(
        &self,
        action: PlaywrightAction,
        input: &Map<String, Value>,
        grants: &BrowserToolGrants,
    ) -> Result<Value, String> {
        let _automation = lock_unpoison(&self.automation);
        // Arguments are refused before a page is created for them: a malformed call must not
        // cost a WebView, and its error must be the argument's, not the page's.
        validate_action_input(action, input, grants, self.security_level())?;
        let mut notices: Vec<String> = Vec::new();
        let status = self.status();
        let crashed = self.lock_state().activity.crash.clone();
        if let Some(crash) = crashed {
            // Playwright closes a crashed page and opens a fresh one; the equivalent here is
            // retiring the dead native surface and starting over on the blank start page.
            self.reset_after_crash()?;
            notices.push(format!("Page crashed and was reset to about:blank ({crash})."));
        } else if action != PlaywrightAction::Navigate && status.suspended {
            // Automatic LRU sleep is transparent to the next observation or interaction. Resume
            // the retained controller in place (or recreate a prior explicit cold suspension)
            // only after BrowserRuntime has reserved an awake slot for this tool call.
            self.prepare(None)?;
            self.wait_for_load(NAVIGATION_LOAD_GRACE);
        } else if action != PlaywrightAction::Navigate && !status.has_page {
            // `ensureTab`: any action on a conversation that has no page yet gets one, blank and
            // in the background, instead of an error telling the model to navigate first.
            self.prepare(None)?;
        }
        let _control = self.begin_agent_control(action)?;
        let modal_states = self.modal_states();
        if let Some(state) = modal_states.iter().find(|state| !state.is_cleared_by(action)) {
            let mut lines = vec![format!(
                "Tool \"playwright {action}\" does not handle the modal state."
            )];
            let _ = state;
            lines.push("Modal state:".to_owned());
            lines.extend(render_modal_states(&modal_states));
            return Err(lines.join("\n"));
        }
        let watch = self.action_watch();
        let result = (|| {
            let selector = || optional_input_string(input, "selector", MAX_SELECTOR_CHARS);
            let element_ref = || optional_input_string(input, "ref", 32);
            match action {
                PlaywrightAction::Navigate => {
                    let url = required_input_string(input, "url", MAX_URL_CHARS, false)?;
                    self.navigate_tool(&url)
                }
                PlaywrightAction::Snapshot => {
                    let max_chars = optional_input_u64(input, "max_chars")?
                        .map(|value| {
                            usize::try_from(value).map_err(|_| "max_chars is too large".to_owned())
                        })
                        .transpose()?;
                    self.snapshot(max_chars)
                }
                PlaywrightAction::Click => {
                    let button = optional_input_string(input, "button", 16)?
                        .unwrap_or_else(|| "left".to_owned());
                    let double = optional_input_bool(input, "double", false)?;
                    let modifiers = parse_string_array_field(input, "modifiers")?;
                    self.click_tool(
                        selector()?.as_deref(),
                        element_ref()?.as_deref(),
                        &button,
                        double,
                        &modifiers,
                    )
                }
                PlaywrightAction::Type => {
                    let text = required_input_string(input, "text", MAX_TEXT_INPUT_CHARS, true)?;
                    let clear = optional_input_bool(input, "clear", true)?;
                    let submit = optional_input_bool(input, "submit", false)?;
                    let slowly = optional_input_bool(input, "slowly", false)?;
                    self.type_tool(
                        selector()?.as_deref(),
                        element_ref()?.as_deref(),
                        &text,
                        clear,
                        submit,
                        slowly,
                    )
                }
                PlaywrightAction::FillForm => {
                    let fields = input
                        .get("fields")
                        .cloned()
                        .ok_or_else(|| "missing required parameter fields".to_owned())?;
                    self.fill_form(&fields)
                }
                PlaywrightAction::Select => {
                    let values = parse_select_values(input)?;
                    self.select(selector()?.as_deref(), element_ref()?.as_deref(), &values)
                }
                PlaywrightAction::Hover => {
                    self.hover_tool(selector()?.as_deref(), element_ref()?.as_deref())
                }
                PlaywrightAction::Key => {
                    let key = required_input_string(input, "key", MAX_KEY_CHARS, false)?;
                    let repeat = optional_input_u64(input, "repeat")?.unwrap_or(1);
                    self.key_tool(&key, repeat)
                }
                PlaywrightAction::Scroll => {
                    let x = optional_input_f64(input, "x")?;
                    let y = optional_input_f64(input, "y")?;
                    self.scroll_tool(x, y, selector()?.as_deref(), element_ref()?.as_deref())
                }
                PlaywrightAction::Evaluate => {
                    let script = required_input_string(input, "script", MAX_EVALUATE_CHARS, false)?;
                    self.evaluate_tool(&script, element_ref()?.as_deref())
                }
                PlaywrightAction::Wait => {
                    let text = optional_input_string(input, "text", 65_536)?;
                    let text_gone = optional_input_string(input, "text_gone", 65_536)?;
                    let load = optional_input_bool(input, "load", false)?;
                    let timeout_ms = optional_input_u64(input, "timeout_ms")?;
                    self.wait(
                        selector()?.as_deref(),
                        text.as_deref(),
                        text_gone.as_deref(),
                        load,
                        timeout_ms,
                    )
                }
                PlaywrightAction::Screenshot => {
                    // Validate that renderer/model input still has the declared field even though the
                    // trusted caller, not this untrusted string, chooses the actual filesystem path.
                    let _requested = required_input_string(input, "path", 4_096, false)?;
                    let path = grants.screenshot_path.as_deref().ok_or_else(|| {
                        "playwright screenshot is missing a save path validated by the path guard".to_owned()
                    })?;
                    let full_page = optional_input_bool(input, "full_page", false)?;
                    to_json_value(self.screenshot(
                        path,
                        full_page,
                        selector()?.as_deref(),
                        element_ref()?.as_deref(),
                    )?)
                }
                PlaywrightAction::Console => {
                    let only_errors = optional_input_bool(input, "only_errors", false)?;
                    let limit = optional_input_u64(input, "limit")?;
                    let clear = optional_input_bool(input, "clear", false)?;
                    self.console(only_errors, limit, clear)
                }
                PlaywrightAction::Network => {
                    let filter = optional_input_string(input, "filter", 2_048)?;
                    let limit = optional_input_u64(input, "limit")?;
                    let clear = optional_input_bool(input, "clear", false)?;
                    self.network(filter.as_deref(), limit, clear)
                }
                PlaywrightAction::Dialog => {
                    let accept = match input.get("accept") {
                        None | Some(Value::Null) => None,
                        Some(value) => Some(
                            value
                                .as_bool()
                                .ok_or_else(|| "parameter accept must be a boolean".to_owned())?,
                        ),
                    };
                    let prompt_text = optional_input_string(input, "prompt_text", 16_384)?;
                    self.dialog_tool(accept, prompt_text.as_deref())
                }
                PlaywrightAction::FileUpload => {
                    // The untrusted string still has to be present, but the guard-approved absolute
                    // paths come from the trusted caller, never from this input.
                    let _requested = input
                        .get("paths")
                        .ok_or_else(|| "missing required parameter paths".to_owned())?;
                    let paths = grants.upload_paths.as_deref().ok_or_else(|| {
                        "playwright file_upload is missing files validated by the path guard".to_owned()
                    })?;
                    self.file_upload(selector()?.as_deref(), element_ref()?.as_deref(), paths)
                }
                // Same `DOM.setFileInputFiles` machinery as `file_upload`, but the file is a
                // host-materialized transcript attachment: the input carries no path at all and
                // the grant is the only source of bytes.
                PlaywrightAction::UploadImage => {
                    let paths = grants.upload_paths.as_deref().ok_or_else(|| {
                        "playwright upload_image is missing a host-materialized image file".to_owned()
                    })?;
                    self.file_upload(selector()?.as_deref(), element_ref()?.as_deref(), paths)
                }
                PlaywrightAction::Resize => {
                    let width = required_input_u32(input, "width")?;
                    let height = required_input_u32(input, "height")?;
                    to_json_value(self.set_viewport(width, height)?)
                }
                // Tab actions never reach a page: `BrowserManager` handles them before a session
                // is even resolved.
                PlaywrightAction::TabNew
                | PlaywrightAction::TabList
                | PlaywrightAction::TabSelect
                | PlaywrightAction::TabClose
                | PlaywrightAction::Close => Err(format!(
                    "playwright {action} is executed by the browser manager, not an individual page"
                )),
            }
        })();
        let mut result = match result {
            Ok(value) => value,
            Err(error) if error == MODAL_STATE_INTERRUPTED && self.has_modal_state() => {
                json!({ "interrupted": "modal-state" })
            }
            Err(error) => return Err(error),
        };
        let aftermath = if action.waits_for_completion() {
            self.settle_after_action(&watch)
        } else {
            ActionAftermath::default()
        };
        if action.reports_page_after() || !aftermath.modal_states.is_empty() {
            if !result.is_object() {
                // `evaluate` answers with the bare serialized value; a modal state or notice
                // still has to reach the model, so the value moves under `value`.
                result = json!({ "value": result });
            }
            self.report_page_after_action(&mut result, &watch, &aftermath, notices);
        } else if !notices.is_empty() {
            if let Value::Object(object) = &mut result {
                object.insert("notices".to_owned(), json!(notices));
            }
        }
        Ok(result)
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
                    entries.as_array().is_some_and(|entries| !entries.is_empty())
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

    pub fn console(
        &self,
        only_errors: bool,
        limit: Option<u64>,
        clear: bool,
    ) -> Result<Value, String> {
        let limit = clamp_log_limit(limit, 100);
        self.eval_value(
            &format!(
                r#"
let entries = __state.consoleEntries.slice();
if ({only_errors}) entries = entries.filter(entry => entry.level === "error");
const total = entries.length;
if (entries.length > {limit}) entries = entries.slice(entries.length - {limit});
if ({clear}) __state.consoleEntries.length = 0;
return {{entries, returned:entries.length, total, cleared:{clear}}};
"#
            ),
            EVAL_TIMEOUT,
        )
    }

    pub fn network(
        &self,
        filter: Option<&str>,
        limit: Option<u64>,
        clear: bool,
    ) -> Result<Value, String> {
        let limit = clamp_log_limit(limit, 50);
        self.eval_value(
            &format!(
                r#"
const filter = {filter};
const match = url => filter === null || String(url).includes(filter);
let recorded = __state.networkEntries.filter(entry => match(entry.url));
const total = recorded.length;
if (recorded.length > {limit}) recorded = recorded.slice(recorded.length - {limit});
const resources = performance.getEntriesByType("resource")
  .filter(entry => match(entry.name)).slice(-{limit}).map(entry => ({{
    kind:"resource", url:String(entry.name).slice(0,8192), initiatorType:String(entry.initiatorType).slice(0,64),
    durationMs:Math.round(entry.duration*100)/100, transferSize:entry.transferSize, decodedBodySize:entry.decodedBodySize
  }}));
if ({clear}) {{ __state.networkEntries.length = 0; performance.clearResourceTimings(); }}
return {{entries:recorded, resources, returned:recorded.length, total, cleared:{clear}}};
"#,
                filter = js_optional_literal(filter)?
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
        let raw = self
            .recv_racing_modal_states(&receiver, timeout)
            .map_err(|error| match error {
                mpsc::RecvTimeoutError::Timeout if self.has_modal_state() => {
                    MODAL_STATE_INTERRUPTED.to_owned()
                }
                mpsc::RecvTimeoutError::Timeout => {
                    format!("timed out waiting for browser script result ({} ms)", timeout.as_millis())
                }
                mpsc::RecvTimeoutError::Disconnected => "browser script result channel closed".into(),
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
        .map_err(|error| format!("failed to verify the network-inert Chromium start page: {error}"))?;
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
        selector: Option<&str>,
        element_ref: Option<&str>,
    ) -> Result<BrowserScreenshot, String> {
        validate_screenshot_path(validated_path)?;
        let capture = if selector.is_some() || element_ref.is_some() {
            self.capture_region_png(selector, element_ref)?
        } else {
            self.capture_screenshot_png(full_page)?
        };
        std::fs::write(validated_path, &capture.bytes)
            .map_err(|error| format!("failed to write browser screenshot: {error}"))?;
        Ok(self.note_screenshot_saved(validated_path, &capture))
    }

    /// Production tool adapter for screenshot capture. Filesystem path authorization remains in
    /// `tool_executor`, while this method guarantees that capture cannot race trusted autofill and
    /// cannot bypass the credential read guard.
    pub(crate) fn capture_screenshot_for_tool(
        &self,
        full_page: bool,
        selector: Option<&str>,
        element_ref: Option<&str>,
    ) -> Result<BrowserPngCapture, String> {
        let _automation = lock_unpoison(&self.automation);
        let _control = self.begin_agent_control(PlaywrightAction::Screenshot)?;
        if selector.is_some() || element_ref.is_some() {
            self.capture_region_png(selector, element_ref)
        } else {
            self.capture_screenshot_png(full_page)
        }
    }

    fn capture_screenshot_png(&self, full_page: bool) -> Result<BrowserPngCapture, String> {
        self.capture_png_clip(full_page, None)
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

            // WebView2 can indefinitely defer Page.captureScreenshot while its controller or
            // parent window is invisible. Render an unopened task-space offscreen without
            // focusing it, then restore the exact hidden geometry after capture.
            if !was_open {
                let prepared = (|| -> Result<(), String> {
                    page.set_position(PhysicalPosition::new(-32_000, -32_000))
                        .map_err(|error| format!("failed to prepare the offscreen browser screenshot: {error}"))?;
                    if host == Some(BrowserHost::DetachedWindow) {
                        window
                            .set_position(LogicalPosition::new(-32_000.0, -32_000.0))
                            .map_err(|error| format!("failed to prepare the offscreen browser window: {error}"))?;
                        window
                            .show()
                            .map_err(|error| format!("failed to show the offscreen browser window: {error}"))?;
                    }
                    page.show()
                        .map_err(|error| format!("failed to show the offscreen browser page: {error}"))?;
                    Ok(())
                })();
                if let Err(error) = prepared {
                    restore_hidden_surface();
                    return Err(error);
                }
                let _ = self.cdp_call("Page.bringToFront", &json!({}), EVAL_TIMEOUT);
                std::thread::sleep(Duration::from_millis(80));
            }

            let capture = (|| {
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
                    &page,
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
            })();

            if !was_open {
                restore_hidden_surface();
            }
            capture
        }

        #[cfg(not(windows))]
        {
            let _ = (full_page, clip);
            Err(
                "playwright screenshot is currently supported only by Windows WebView2 (CDP Page.captureScreenshot)"
                    .into(),
            )
        }
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
        return Err("playwright screenshot accepts only validated absolute paths".into());
    }
    if path
        .extension()
        .and_then(|extension| extension.to_str())
        .map_or(true, |extension| !extension.eq_ignore_ascii_case("png"))
    {
        return Err("playwright screenshot path must end in .png".into());
    }
    let parent = path
        .parent()
        .ok_or_else(|| "playwright screenshot path has no parent directory".to_owned())?;
    if !parent.is_dir() {
        return Err("playwright screenshot parent directory must already exist".into());
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

        let restored = capture_cookies_for_cold_close(control, page)
            .map_err(|_| "could not verify cold-suspension cookie restoration; the page remains suspended".to_owned())?;
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
        .ok_or_else(|| "could not construct an exact source URL for the host-only cookie host".to_owned())?;
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
    let values = cookies
        .0
        .as_array_mut()
        .ok_or_else(|| "the cookies field in the Chromium cookie snapshot is not an array".to_owned())?;
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
            return Err(format!("cookie item {} has an invalid sourcePort", index + 1));
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
                    .ok_or_else(|| format!("cookie item {} has an invalid expires field", index + 1))?,
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
        _ => Err(format!("cookie item {} has an invalid {field} field", index + 1)),
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
        Some(_) => Err(format!("cookie item {} has an invalid {field} field", index + 1)),
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
        Some(_) => Err(format!("cookie item {} has an invalid {field} field", index + 1)),
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
        _ => Err(format!("cookie item {} has an invalid {field} field", index + 1)),
    }
}

fn parse_cookie_same_site(value: &str, index: usize) -> Result<CookieSameSite, String> {
    match value {
        "Strict" => Ok(CookieSameSite::Strict),
        "Lax" => Ok(CookieSameSite::Lax),
        "None" => Ok(CookieSameSite::None),
        _ => Err(format!("cookie item {} has an invalid sameSite field", index + 1)),
    }
}

fn parse_cookie_priority(value: &str, index: usize) -> Result<CookiePriority, String> {
    match value {
        "Low" => Ok(CookiePriority::Low),
        "Medium" => Ok(CookiePriority::Medium),
        "High" => Ok(CookiePriority::High),
        _ => Err(format!("cookie item {} has an invalid priority field", index + 1)),
    }
}

fn parse_cookie_source_scheme(value: &str, index: usize) -> Result<CookieSourceScheme, String> {
    match value {
        "Unset" => Ok(CookieSourceScheme::Unset),
        "NonSecure" => Ok(CookieSourceScheme::NonSecure),
        "Secure" => Ok(CookieSourceScheme::Secure),
        _ => Err(format!("cookie item {} has an invalid sourceScheme field", index + 1)),
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
        return Err(format!("cookie item {} topLevelSite is not canonical", index + 1));
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
            call_devtools_protocol(&control, &page, method, &params.to_string(), timeout, &|| {
                self.has_modal_state()
            })
        }
        #[cfg(not(windows))]
        {
            let _ = (method, params, timeout);
            Err("the embedded browser on this platform does not support WebView2 CDP".into())
        }
    }

    /// Runs an `Input`-domain call, reporting whether the platform accepted it. A hard error is
    /// treated as "no trusted input here" so pointer/keyboard tools degrade to synthetic events
    /// instead of failing outright.
    #[cfg_attr(not(windows), allow(unused_variables))]
    fn input_call(&self, method: &str, params: &Value) -> Result<bool, String> {
        #[cfg(windows)]
        {
            match self.cdp_call(method, params, EVAL_TIMEOUT) {
                Ok(_) => Ok(true),
                Err(_) => Ok(false),
            }
        }
        #[cfg(not(windows))]
        {
            Ok(false)
        }
    }

    fn dispatch_mouse_move(&self, x: f64, y: f64) -> Result<bool, String> {
        self.input_call(
            "Input.dispatchMouseEvent",
            &json!({"type":"mouseMoved","x":x,"y":y,"buttons":0}),
        )
    }

    fn dispatch_click(
        &self,
        x: f64,
        y: f64,
        button: &str,
        double: bool,
        modifiers: i64,
    ) -> Result<bool, String> {
        let buttons_mask = match button {
            "right" => 2,
            "middle" => 4,
            _ => 1,
        };
        if !self.input_call(
            "Input.dispatchMouseEvent",
            &json!({"type":"mouseMoved","x":x,"y":y,"buttons":0,"modifiers":modifiers}),
        )? {
            return Ok(false);
        }
        let clicks = if double { 2 } else { 1 };
        for count in 1..=clicks {
            self.input_call(
                "Input.dispatchMouseEvent",
                &json!({"type":"mousePressed","x":x,"y":y,"button":button,"buttons":buttons_mask,"clickCount":count,"modifiers":modifiers}),
            )?;
            self.input_call(
                "Input.dispatchMouseEvent",
                &json!({"type":"mouseReleased","x":x,"y":y,"button":button,"buttons":0,"clickCount":count,"modifiers":modifiers}),
            )?;
        }
        Ok(true)
    }

    fn dispatch_wheel(&self, x: f64, y: f64, delta_x: f64, delta_y: f64) -> Result<bool, String> {
        self.input_call(
            "Input.dispatchMouseEvent",
            &json!({"type":"mouseWheel","x":x,"y":y,"deltaX":delta_x,"deltaY":delta_y}),
        )
    }

    fn dispatch_key_press(&self, spec: &KeySpec) -> Result<bool, String> {
        let mask = spec.modifier_mask();
        for modifier in &spec.modifiers {
            if !self.input_call(
                "Input.dispatchKeyEvent",
                &json!({"type":"rawKeyDown","key":modifier.key(),"code":modifier.code(),"windowsVirtualKeyCode":modifier.virtual_key(),"modifiers":mask}),
            )? {
                return Ok(false);
            }
        }
        let (code, virtual_key, text) = cdp_key_fields(&spec.key);
        // A held Control/Meta means the key is a shortcut, not text to insert.
        let text = if spec.suppresses_text() { None } else { text };
        let mut down = json!({
            "type": if text.is_some() { "keyDown" } else { "rawKeyDown" },
            "key": spec.key,
            "code": code,
            "windowsVirtualKeyCode": virtual_key,
            "modifiers": mask,
        });
        if let Some(text) = &text {
            down["text"] = json!(text);
            down["unmodifiedText"] = json!(text);
        }
        if !self.input_call("Input.dispatchKeyEvent", &down)? {
            return Ok(false);
        }
        self.input_call(
            "Input.dispatchKeyEvent",
            &json!({"type":"keyUp","key":spec.key,"code":code,"windowsVirtualKeyCode":virtual_key,"modifiers":mask}),
        )?;
        for modifier in spec.modifiers.iter().rev() {
            self.input_call(
                "Input.dispatchKeyEvent",
                &json!({"type":"keyUp","key":modifier.key(),"code":modifier.code(),"windowsVirtualKeyCode":modifier.virtual_key(),"modifiers":mask}),
            )?;
        }
        Ok(true)
    }

    fn dispatch_fill(&self, text: &str) -> Result<bool, String> {
        if text.is_empty() {
            // Nothing to insert; the caller already cleared the field in-page.
            return Ok(self.input_call("Input.insertText", &json!({"text":""}))?);
        }
        self.input_call("Input.insertText", &json!({ "text": text }))
    }

    fn dispatch_typing(&self, text: &str) -> Result<bool, String> {
        for (index, character) in text.chars().enumerate() {
            let key = character.to_string();
            let (code, virtual_key, _) = cdp_key_fields(&key);
            let down = json!({"type":"keyDown","key":key,"code":code,"windowsVirtualKeyCode":virtual_key,"text":key,"unmodifiedText":key});
            let accepted = self.input_call("Input.dispatchKeyEvent", &down)?;
            if index == 0 && !accepted {
                return Ok(false);
            }
            self.input_call(
                "Input.dispatchKeyEvent",
                &json!({"type":"keyUp","key":key,"code":code,"windowsVirtualKeyCode":virtual_key}),
            )?;
        }
        Ok(true)
    }

    /// Resolves a target to a CDP remote object id for `DOM.setFileInputFiles`.
    fn resolve_object_id(&self, target: &TargetSpec) -> Result<String, String> {
        let expression = format!(
            "window.__MEWORK_BROWSER_RUNTIME__.resolve({}, {})",
            js_optional_literal(target.selector.as_deref())?,
            js_optional_literal(target.element_ref.as_deref())?
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
            return Err(format!("playwright file_upload could not locate the input: {description}"));
        }
        response
            .pointer("/result/objectId")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| "playwright file_upload could not obtain a target element reference".to_owned())
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Modifier {
    Alt,
    Control,
    Meta,
    Shift,
}

impl Modifier {
    fn parse(token: &str) -> Option<Self> {
        match token.to_ascii_lowercase().as_str() {
            "alt" | "option" => Some(Self::Alt),
            "control" | "ctrl" => Some(Self::Control),
            "meta" | "cmd" | "command" | "win" | "super" => Some(Self::Meta),
            "shift" => Some(Self::Shift),
            _ => None,
        }
    }

    fn bit(self) -> i64 {
        match self {
            Self::Alt => 1,
            Self::Control => 2,
            Self::Meta => 4,
            Self::Shift => 8,
        }
    }

    fn key(self) -> &'static str {
        match self {
            Self::Alt => "Alt",
            Self::Control => "Control",
            Self::Meta => "Meta",
            Self::Shift => "Shift",
        }
    }

    fn code(self) -> &'static str {
        match self {
            Self::Alt => "AltLeft",
            Self::Control => "ControlLeft",
            Self::Meta => "MetaLeft",
            Self::Shift => "ShiftLeft",
        }
    }

    fn virtual_key(self) -> i64 {
        match self {
            Self::Alt => 18,
            Self::Control => 17,
            Self::Meta => 91,
            Self::Shift => 16,
        }
    }
}

struct KeySpec {
    key: String,
    modifiers: Vec<Modifier>,
}

impl KeySpec {
    fn modifier_mask(&self) -> i64 {
        self.modifiers
            .iter()
            .fold(0, |mask, modifier| mask | modifier.bit())
    }

    fn suppresses_text(&self) -> bool {
        self.modifiers
            .iter()
            .any(|modifier| matches!(modifier, Modifier::Control | Modifier::Meta | Modifier::Alt))
    }
}

/// Parses `"Control+Shift+L"` style specs into a main key plus modifiers, normalizing a few
/// common aliases (`Esc`, `Return`, `Space`, `Del`).
fn parse_key_spec(spec: &str) -> Result<KeySpec, String> {
    let mut pieces: Vec<&str> = spec
        .split('+')
        .map(str::trim)
        .filter(|piece| !piece.is_empty())
        .collect();
    if pieces.is_empty() {
        return Err("key must not be empty".into());
    }
    let raw_key = pieces.pop().expect("pieces is non-empty");
    let mut modifiers = Vec::new();
    for piece in pieces {
        let modifier =
            Modifier::parse(piece).ok_or_else(|| format!("unrecognized modifier key: {piece}"))?;
        if !modifiers.contains(&modifier) {
            modifiers.push(modifier);
        }
    }
    let key = match raw_key.to_ascii_lowercase().as_str() {
        "esc" => "Escape".to_owned(),
        "return" | "enter" => "Enter".to_owned(),
        "space" => " ".to_owned(),
        "del" => "Delete".to_owned(),
        "tab" => "Tab".to_owned(),
        _ => raw_key.to_owned(),
    };
    Ok(KeySpec { key, modifiers })
}

/// Best-effort CDP `code` / `windowsVirtualKeyCode` / `text` for a key. Printable single
/// characters carry `text`; named keys map to their virtual-key codes.
fn cdp_key_fields(key: &str) -> (String, i64, Option<String>) {
    match key {
        "Enter" => ("Enter".into(), 13, Some("\r".into())),
        "Tab" => ("Tab".into(), 9, Some("\t".into())),
        "Escape" => ("Escape".into(), 27, None),
        "Backspace" => ("Backspace".into(), 8, None),
        "Delete" => ("Delete".into(), 46, None),
        "ArrowUp" => ("ArrowUp".into(), 38, None),
        "ArrowDown" => ("ArrowDown".into(), 40, None),
        "ArrowLeft" => ("ArrowLeft".into(), 37, None),
        "ArrowRight" => ("ArrowRight".into(), 39, None),
        "Home" => ("Home".into(), 36, None),
        "End" => ("End".into(), 35, None),
        "PageUp" => ("PageUp".into(), 33, None),
        "PageDown" => ("PageDown".into(), 34, None),
        " " => ("Space".into(), 32, Some(" ".into())),
        _ => {
            let chars: Vec<char> = key.chars().collect();
            if chars.len() == 1 {
                let character = chars[0];
                let code = if character.is_ascii_alphabetic() {
                    format!("Key{}", character.to_ascii_uppercase())
                } else if character.is_ascii_digit() {
                    format!("Digit{character}")
                } else {
                    String::new()
                };
                let virtual_key = if character.is_ascii_alphanumeric() {
                    character.to_ascii_uppercase() as i64
                } else {
                    0
                };
                (code, virtual_key, Some(character.to_string()))
            } else {
                (key.to_owned(), 0, None)
            }
        }
    }
}

/// CDP modifier bitmask for a `playwright click` `modifiers` array.
fn modifier_bits(modifiers: &[String]) -> Result<i64, String> {
    let mut mask = 0;
    for token in modifiers {
        let modifier =
            Modifier::parse(token).ok_or_else(|| format!("unrecognized modifier key: {token}"))?;
        mask |= modifier.bit();
    }
    Ok(mask)
}

/// Accepts a JSON string or array-of-strings, returning the values.
fn parse_string_or_array(value: &Value, label: &str) -> Result<Vec<String>, String> {
    match value {
        Value::String(value) => Ok(vec![value.clone()]),
        Value::Array(values) => values
            .iter()
            .map(|item| {
                item.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| format!("{label} array may contain only strings"))
            })
            .collect(),
        _ => Err(format!("{label} must be a string or an array of strings")),
    }
}

/// Reads an optional string-array tool argument (used for `playwright click` modifiers).
fn parse_string_array_field(input: &Map<String, Value>, key: &str) -> Result<Vec<String>, String> {
    match input.get(key) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(value) => parse_string_or_array(value, key),
    }
}

/// Clamps a log `limit` argument into `1..=MAX_LOG_LIMIT`, applying a default when absent.
fn clamp_log_limit(limit: Option<u64>, default: u64) -> u64 {
    limit.unwrap_or(default).clamp(1, MAX_LOG_LIMIT)
}

fn to_json_value<T: Serialize>(value: T) -> Result<Value, String> {
    serde_json::to_value(value).map_err(|error| format!("failed to serialize browser result: {error}"))
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
        return Err(format!("parameter {key} exceeds the {max_chars}-character limit"));
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

fn required_input_u32(input: &Map<String, Value>, key: &str) -> Result<u32, String> {
    let value = optional_input_u64(input, key)?.ok_or_else(|| format!("missing required parameter {key}"))?;
    u32::try_from(value).map_err(|_| format!("parameter {key} is outside the u32 range"))
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

fn parse_select_values(input: &Map<String, Value>) -> Result<Vec<String>, String> {
    let value = input
        .get("values")
        .ok_or_else(|| "missing required parameter values".to_owned())?;
    match value {
        Value::String(value) => Ok(vec![value.clone()]),
        Value::Array(values) => values
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| "playwright select values array may contain only strings".to_owned())
            })
            .collect(),
        _ => Err("playwright select values must be a string or an array of strings".into()),
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
                // `playwright dialog` answers.
                let settings = unsafe { core.Settings() }
                    .map_err(|error| format!("failed to read WebView2 settings: {error}"))?;
                unsafe { settings.SetAreDefaultScriptDialogsEnabled(false) }
                    .map_err(|error| format!("failed to take over WebView2 dialogs: {error}"))?;
                let dialog_state = state.clone();
                let dialog_token = callback_token.clone();
                let dialog_handler = ScriptDialogOpeningEventHandler::create(Box::new(
                    move |_, args| {
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
                        let (kind, default_text) = match default_text.strip_prefix(DIALOG_KIND_MARK) {
                            Some(real_kind) if kind == "prompt" => (real_kind.to_owned(), String::new()),
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
                        state.activity.next_dialog_id = state.activity.next_dialog_id.wrapping_add(1);
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
                    },
                ));
                let mut token = 0i64;
                unsafe { core.add_ScriptDialogOpening(&dialog_handler, &mut token) }
                    .map_err(|error| format!("failed to observe WebView2 dialogs: {error}"))?;
                retained.push(Box::new(dialog_handler));

                let failure_state = state.clone();
                let failure_token = callback_token.clone();
                let failure_handler = ProcessFailedEventHandler::create(Box::new(move |_, args| {
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
                    let description = if kind == COREWEBVIEW2_PROCESS_FAILED_KIND_BROWSER_PROCESS_EXITED {
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
                unsafe { core.add_ProcessFailed(&failure_handler, &mut token) }
                    .map_err(|error| format!("failed to observe WebView2 process failures: {error}"))?;
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
                    "Network.loadingFinished",
                    "Network.loadingFailed",
                    "Page.domContentEventFired",
                    "Page.loadEventFired",
                    "Page.frameNavigated",
                    "Page.fileChooserOpened",
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
        scheduling.map_err(|error| format!("failed to schedule WebView2 observer setup: {error}"))?;
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

    /// Answers the dialog `playwright dialog` named, on the WebView UI thread that owns it.
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
fn record_devtools_event(activity: &mut PageActivity, event: &str, parameters: &Value) {
    let string = |pointer: &str| parameters.pointer(pointer).and_then(Value::as_str);
    match event {
        "Network.requestWillBeSent" => {
            let Some(request_id) = string("/requestId") else {
                return;
            };
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
        "Network.loadingFinished" | "Network.loadingFailed" => {
            if let Some(request_id) = string("/requestId") {
                activity.finish_request(request_id);
            }
        }
        "Page.domContentEventFired" => {
            activity.dom_content_loaded_events = activity.dom_content_loaded_events.wrapping_add(1);
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
        return Err(format!("browser screenshot exceeds the {MAX_SCREENSHOT_BYTES}-byte limit"));
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
        return Err(format!("browser screenshot exceeds the {MAX_SCREENSHOT_BYTES}-byte limit"));
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
    let mut value: Value = serde_json::from_str(raw)
        .map_err(|error| format!("browser returned invalid JSON: {error}; raw={}", truncate(raw, 512)))?;
    // Some WebKit bindings wrap an already-serialized callback value in a JSON string.
    if let Value::String(inner) = &value {
        if let Ok(decoded) = serde_json::from_str::<Value>(inner) {
            value = decoded;
        }
    }
    let object = value
        .as_object()
        .ok_or_else(|| format!("browser script did not return a result object: {}", truncate(raw, 512)))?;
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
    serde_json::to_string(value).map_err(|error| format!("failed to create JavaScript string: {error}"))
}

fn js_optional_literal(value: Option<&str>) -> Result<String, String> {
    value
        .map(js_string_literal)
        .unwrap_or_else(|| Ok("null".into()))
}

fn validate_target_input(
    selector: Option<&str>,
    element_ref: Option<&str>,
    allow_empty: bool,
) -> Result<TargetSpec, String> {
    if selector.is_some() && element_ref.is_some() {
        return Err("provide either selector or ref, not both".into());
    }
    if !allow_empty && selector.is_none() && element_ref.is_none() {
        return Err("selector or ref is required".into());
    }
    Ok(TargetSpec {
        selector: selector.map(validate_selector).transpose()?,
        element_ref: element_ref.map(validate_element_ref).transpose()?,
    })
}

fn validate_selector(selector: &str) -> Result<String, String> {
    if selector.trim().is_empty() {
        return Err("selector must not be empty".into());
    }
    if selector.chars().count() > MAX_SELECTOR_CHARS {
        return Err(format!("selector exceeds the {MAX_SELECTOR_CHARS}-character limit"));
    }
    if selector.contains('\0') {
        return Err("selector must not contain NUL".into());
    }
    Ok(selector.to_owned())
}

fn validate_element_ref(element_ref: &str) -> Result<String, String> {
    let element_ref = element_ref.trim();
    let digits = element_ref
        .strip_prefix('e')
        .ok_or_else(|| "ref must use the e<number> format returned by snapshot".to_owned())?;
    if digits.is_empty()
        || digits.len() > 10
        || digits.starts_with('0')
        || !digits.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err("ref must use the e<number> format returned by snapshot".into());
    }
    Ok(element_ref.to_owned())
}

fn validate_key(key: &str) -> Result<(), String> {
    if key.trim().is_empty() || key.chars().count() > MAX_KEY_CHARS {
        return Err(format!("key must contain between 1 and {MAX_KEY_CHARS} characters"));
    }
    if key.chars().any(char::is_control) {
        return Err("key must not contain control characters".into());
    }
    Ok(())
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
        return Err(format!("browser URL exceeds the {MAX_URL_CHARS}-character limit"));
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
  const startPageCopy = () => uiLanguage === "zh-CN"
    ? {title:"新标签页", heading:"开始浏览", description:"输入 URL 以打开页面"}
    : {title:"New tab", heading:"Start browsing", description:"Enter a URL to open a page"};
  const applyStartPageCopy = () => {
    if (location.href !== "about:blank") return false;
    document.documentElement.lang = uiLanguage;
    const copy = startPageCopy();
    document.title = copy.title;
    const heading = document.querySelector(".mework-start h1");
    const description = document.querySelector(".mework-start p");
    if (heading) heading.textContent = copy.heading;
    if (description) description.textContent = copy.description;
    return true;
  };
  const setUiLanguage = language => {
    uiLanguage = String(language || "").toLowerCase().startsWith("zh") ? "zh-CN" : "en-US";
    return {language: uiLanguage, startPage: applyStartPageCopy()};
  };
  const mountStartPage = () => {
    if (location.href !== "about:blank") return false;
    if (!document.body) {
      document.addEventListener("DOMContentLoaded", mountStartPage, {once:true});
      return false;
    }
    applyUiTheme(uiTheme);
    document.documentElement.lang = uiLanguage;
    if (document.querySelector(".mework-start")) {
      applyStartPageCopy();
      return true;
    }
    const style = document.createElement("style");
    style.textContent = `
      :root{--mework-start-bg:#f7f7f4;--mework-start-fg:#666;--mework-start-heading:#292927;font-family:Inter,ui-sans-serif,-apple-system,BlinkMacSystemFont,"Segoe UI","Noto Sans SC",sans-serif;color:var(--mework-start-heading);background:var(--mework-start-bg)}
      :root[data-theme="night"]{--mework-start-bg:#171817;--mework-start-fg:#999;--mework-start-heading:#d6d6d8}
      *{box-sizing:border-box}html,body{width:100%;height:100%;margin:0}body{display:grid;place-items:center;background:var(--mework-start-bg);color:var(--mework-start-fg)}
      .mework-start{margin-top:-5vh;padding:32px;text-align:center;user-select:none}.mework-start svg{width:34px;height:34px;color:var(--mework-start-fg);fill:none;stroke:currentColor;stroke-width:1.55;stroke-linecap:round;stroke-linejoin:round}
      .mework-start h1{margin:13px 0 6px;color:var(--mework-start-heading);font-size:17px;font-weight:650;letter-spacing:-.01em}.mework-start p{margin:0;color:var(--mework-start-fg);font-size:12px;line-height:1.6}
    `;
    document.head.appendChild(style);
    const copy = startPageCopy();
    document.title = copy.title;
    document.body.innerHTML = `<main class="mework-start"><svg aria-hidden="true" viewBox="0 0 24 24"><circle cx="12" cy="12" r="9"/><path d="M3 12h18M12 3a15 15 0 0 1 0 18M12 3a15 15 0 0 0 0 18"/></svg><h1>${copy.heading}</h1><p>${copy.description}</p></main>`;
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
  // On Windows the native dialog is held open by the host instead and answered by playwright
  // dialog, so the page must reach the real window.alert/confirm/prompt.
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
        if cfg!(windows) { "true" } else { "false" },
    )
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

    fn valid_dispatch_input(action: PlaywrightAction) -> Map<String, Value> {
        object(match action {
            PlaywrightAction::Navigate => json!({"url":"https://example.com"}),
            PlaywrightAction::Snapshot => json!({"max_chars":1000}),
            PlaywrightAction::Click => json!({"selector":"button"}),
            PlaywrightAction::Type => json!({"selector":"input", "text":"hello"}),
            PlaywrightAction::FillForm => json!({"fields":[{"ref":"e1", "value":"x"}]}),
            PlaywrightAction::Select => json!({"selector":"select", "values":["one"]}),
            PlaywrightAction::Hover => json!({"ref":"e1"}),
            PlaywrightAction::Key => json!({"key":"Enter"}),
            PlaywrightAction::Scroll => json!({"x":0, "y":600}),
            PlaywrightAction::Evaluate => json!({"script":"document.title"}),
            PlaywrightAction::Wait => json!({"selector":"main", "timeout_ms":1}),
            PlaywrightAction::Screenshot => json!({"path":"page.png", "full_page":false}),
            PlaywrightAction::Console => json!({"only_errors":false}),
            PlaywrightAction::Network => json!({"clear":false}),
            PlaywrightAction::Dialog => json!({}),
            PlaywrightAction::FileUpload => json!({"selector":"input", "paths":["a.txt"]}),
            PlaywrightAction::UploadImage => json!({"selector":"input", "image_id":"1"}),
            PlaywrightAction::Resize => json!({"width":1280, "height":720}),
            PlaywrightAction::TabNew => json!({}),
            PlaywrightAction::TabList => json!({}),
            PlaywrightAction::TabSelect => json!({"tab":"main"}),
            PlaywrightAction::TabClose => json!({"tab":"agent-1"}),
            PlaywrightAction::Close => json!({}),
        })
    }

    #[test]
    fn every_playwright_action_has_a_runtime_dispatch_arm() {
        let runtime = BrowserSession::default();
        let manager = BrowserRuntime::default();

        // One wire tool, 23 actions: the catalog can no longer enumerate the operations, so the
        // enum is what this coverage test walks.
        assert_eq!(PlaywrightAction::ALL.len(), 23);
        assert_eq!(
            crate::catalog::tool_catalog()
                .into_iter()
                .filter(|tool| tool.name == "playwright")
                .count(),
            1
        );
        let grants = BrowserToolGrants::default();
        for action in PlaywrightAction::ALL {
            // Tab actions create, retarget, and destroy sessions, so they are dispatched by the
            // manager. Every other action acts on one already-selected page.
            let result = if action.is_tab_action() {
                manager.execute_agent_tab_tool(
                    "dispatch-coverage",
                    action,
                    &valid_dispatch_input(action),
                )
            } else {
                runtime.execute_tool_blocking(action, &valid_dispatch_input(action), &grants)
            };
            if let Err(error) = result {
                assert!(
                    !error.contains("does not support action")
                        && !error.contains("is executed by the browser manager"),
                    "playwright {action} is not dispatched: {error}"
                );
            }
        }
    }

    #[test]
    fn browser_dispatch_rejects_invalid_arguments_before_webview_access() {
        let runtime = BrowserSession::default();
        let invalid = [
            (
                PlaywrightAction::Navigate,
                json!({"url":"javascript://alert(1)"}),
                "browser allows only",
            ),
            (PlaywrightAction::Navigate, json!({}), "missing required parameter url"),
            (
                PlaywrightAction::Snapshot,
                json!({"max_chars":999}),
                "snapshot max_chars",
            ),
            (PlaywrightAction::Click, json!({}), "selector or ref is required"),
            (
                PlaywrightAction::Click,
                json!({"selector":"button", "ref":"e1"}),
                "provide either selector or ref",
            ),
            (
                PlaywrightAction::Click,
                json!({"selector":"button", "modifiers":["Hyper"]}),
                "unrecognized modifier key",
            ),
            (PlaywrightAction::Type, json!({"selector":"input"}), "missing required parameter text"),
            (PlaywrightAction::FillForm, json!({}), "missing required parameter fields"),
            (
                PlaywrightAction::FillForm,
                json!({"fields":[]}),
                "must contain between 1 and 50",
            ),
            (
                PlaywrightAction::Select,
                json!({"selector":"select", "values":[]}),
                "must contain between 1 and 100",
            ),
            (PlaywrightAction::Hover, json!({"ref":"element-1"}), "ref must use the e<number> format"),
            (PlaywrightAction::Key, json!({"key":"Enter\n"}), "must not contain control characters"),
            (
                PlaywrightAction::Key,
                json!({"key":"Enter", "repeat":0}),
                "repeat must be between",
            ),
            (PlaywrightAction::Scroll, json!({"y":10_000_001}), "absolute values no greater than"),
            (
                PlaywrightAction::Evaluate,
                json!({"script":"  "}),
                "script must not be empty",
            ),
            (PlaywrightAction::Wait, json!({"timeout_ms":0}), "timeout_ms must be between"),
            (PlaywrightAction::Screenshot, json!({}), "missing required parameter path"),
            (PlaywrightAction::Network, json!({"clear":1}), "clear must be a boolean"),
            (
                PlaywrightAction::FileUpload,
                json!({"selector":"input"}),
                "missing required parameter paths",
            ),
            (
                PlaywrightAction::Resize,
                json!({"width":319, "height":720}),
                "browser viewport must be",
            ),
        ];

        let grants = BrowserToolGrants::default();
        for (action, input, expected) in invalid {
            let error = runtime
                .execute_tool_blocking(action, &object(input), &grants)
                .unwrap_err();
            assert!(
                error.contains(expected),
                "playwright {action} returned an unexpected error: {error}"
            );
        }
    }

    /// Without `load` reaching `wait`, this dispatch would sleep out its timeout and report a
    /// successful delay — the exact silent failure the parameter exists to remove.
    #[test]
    fn browser_wait_dispatch_carries_the_load_condition_through() {
        let session = BrowserSession::new("wait-dispatch-load");
        {
            let mut state = session.lock_state();
            state.synthetic_surface = true;
            state.status.has_page = true;
            state.status.loading = true;
        }

        let error = session
            .execute_tool_blocking(
                PlaywrightAction::Wait,
                &object(json!({"load":true, "timeout_ms":120})),
                &BrowserToolGrants::default(),
            )
            .expect_err("load must reach wait() rather than degrading to a delay");

        assert!(error.contains("page is still loading"), "{error}");
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
            parse_browser_url("localhost:3000/app", level).unwrap().as_str(),
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
        for level in [SecurityLevel::RequestApproval, SecurityLevel::FullAccess] {
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
    fn selectors_and_snapshot_refs_are_strictly_validated() {
        assert!(validate_target_input(Some("button[data-x='a']"), None, false).is_ok());
        assert!(validate_target_input(None, Some("e42"), false).is_ok());
        assert!(validate_target_input(None, None, true).is_ok());
        assert!(validate_target_input(None, None, false).is_err());
        assert!(validate_target_input(Some("button"), Some("e1"), false).is_err());
        assert!(validate_selector("\0").is_err());
        assert!(validate_selector(&"a".repeat(MAX_SELECTOR_CHARS + 1)).is_err());
        for invalid_ref in ["", "1", "e", "e0", "e01", "e-1", "e1x", "e12345678901"] {
            assert!(
                validate_element_ref(invalid_ref).is_err(),
                "accepted {invalid_ref}"
            );
        }
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
    fn select_values_accept_a_string_or_string_array_only() {
        let one = json!({"values":"tw"}).as_object().unwrap().clone();
        assert_eq!(parse_select_values(&one).unwrap(), vec!["tw"]);
        let many = json!({"values":["tw","us"]}).as_object().unwrap().clone();
        assert_eq!(parse_select_values(&many).unwrap(), vec!["tw", "us"]);
        let bad = json!({"values":["tw",2]}).as_object().unwrap().clone();
        assert!(parse_select_values(&bad).is_err());
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
            "Start browsing",
            "开始浏览",
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
        assert!(BROWSER_INITIALIZATION_SCRIPT.contains("not found in the current page snapshot. Try capturing new snapshot."));
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
    fn trusted_agent_pointer_uses_a_purple_quad_and_generation_invalidation() {
        let params = agent_pointer_highlight_params(40.0, 80.0);
        assert_eq!(
            params["quad"],
            json!([40.0, 68.0, 52.0, 80.0, 40.0, 92.0, 28.0, 80.0])
        );
        assert_eq!(params["color"], json!({"r":124,"g":92,"b":255,"a":0.24}));
        assert_eq!(
            params["outlineColor"],
            json!({"r":162,"g":139,"b":255,"a":0.98})
        );

        let session = BrowserSession::new("trusted-agent-pointer");
        let stale = session.next_agent_pointer_generation();
        let current = session.next_agent_pointer_generation();
        assert!(!session.agent_pointer_generation_is_current(stale));
        assert!(session.agent_pointer_generation_is_current(current));
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
            false,
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

        let menu_layout = browser_layout_for_size(
            LogicalSize::new(600.0, 500.0),
            BrowserHost::MainPanel,
            Some(bounds),
            true,
        );
        // Trusted menu chrome is exposed by a native child-window region, never by resizing the
        // untrusted page. This keeps CDP/Playwright viewport coordinates stable while it is open.
        assert_eq!(menu_layout, layout);

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
            false,
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
            true,
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
            .session_for_tool("conversation-one", PlaywrightAction::Navigate)
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
            .hide_with_intent("same-hidden", true, 2)
            .expect_err("the test session has no AppHandle");
        assert!(first_hide.contains("AppHandle"));
        let retry_hide = runtime
            .hide_with_intent("same-hidden", false, 2)
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
            .hide_with_intent("layout-visibility", false, 11)
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
            .hide_with_intent("hidden-restore-layout", false, 51)
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
            state.menu_expanded = true;
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
        assert!(!unowned.lock_state().menu_expanded);
        assert!(unowned.lock_state().menu_hole.is_none());
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
            let error = match session.begin_agent_control(PlaywrightAction::Click) {
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
            Some("playwright click")
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
                .begin_agent_control(PlaywrightAction::Snapshot)
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
    fn trusted_menu_blocks_agent_geometry_and_restores_previous_owner() {
        let session = BrowserSession::new("trusted-menu-control");
        {
            let mut state = session.lock_state();
            let previous = state.status.control.clone();
            state.menu_control_before_open = Some(previous);
            state.menu_expanded = true;
            state.status.control = BrowserControlStatus {
                owner: BrowserControlOwner::User,
                updated_at_ms: 1,
                ..BrowserControlStatus::default()
            };
        }

        {
            let _automation = lock_unpoison(&session.automation);
            let error = match session.begin_agent_control(PlaywrightAction::Screenshot) {
                Ok(_) => panic!("agent must not use menu-shifted page geometry"),
                Err(error) => error,
            };
            assert!(error.contains("the user is using the browser menu"));
        }
        let waiting = session.status();
        assert!(waiting.control.handoff_requested);
        assert_eq!(
            waiting.control.requested_tool.as_deref(),
            Some("playwright screenshot")
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
    fn only_agent_minted_tabs_count_as_the_models_own_surface() {
        // The conversation's primary page and every trusted-UI tab belong to the user.
        assert!(session_is_user_owned("conv_1"));
        assert!(session_is_user_owned("conv_1#tab_9f3c"));
        // `playwright tab_new` mints exactly this shape, and only this shape skips the prompt.
        assert!(!session_is_user_owned("conv_1#agent-1"));
        assert!(!session_is_user_owned("conv_1#agent-42"));
        // A tab token that merely starts with similar text is still the user's.
        assert!(session_is_user_owned("conv_1#agentic"));
        assert!(session_is_user_owned("conv_1#tab_agent-1"));
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

    /// A navigation the interaction triggered has no document to probe yet. Waiting for
    /// `readyState` would have to evaluate against the outgoing page, so `load` waits out the
    /// host-observed load first and only reports a timeout if the load itself never settles.
    #[test]
    fn waiting_for_load_reports_the_navigation_rather_than_probing_the_outgoing_page() {
        let session = BrowserSession::new("wait-load-pending");
        {
            let mut state = session.lock_state();
            state.synthetic_surface = true;
            state.status.has_page = true;
            state.status.loading = true;
        }

        let error = session
            .wait(None, None, None, true, Some(120))
            .expect_err("a page that never finishes loading must time out");

        assert!(error.contains("page is still loading"), "{error}");
    }

    /// Without `load`, the same never-settling page is a plain delay: the other conditions
    /// describe content, and a caller that asked for none of them asked only to sleep.
    #[test]
    fn waiting_without_a_condition_stays_a_delay_even_while_the_page_loads() {
        let session = BrowserSession::new("wait-load-absent");
        {
            let mut state = session.lock_state();
            state.synthetic_surface = true;
            state.status.has_page = true;
            state.status.loading = true;
        }

        let value = session
            .wait(None, None, None, false, Some(1))
            .expect("a bare wait must not consult the page at all");

        assert_eq!(value.get("condition").and_then(Value::as_str), Some("delay"));
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

        assert!(aftermath.navigated, "a navigation that begins inside the window must be reported");
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
            state.status.error = Some("unsafe browser navigation was blocked: http://tauri.localhost".into());
        }

        let aftermath = session.settle_after_action(&watch);
        assert_eq!(
            aftermath.blocked.as_deref(),
            Some("unsafe browser navigation was blocked: http://tauri.localhost")
        );
        assert!(!aftermath.navigated, "a refused navigation has no destination to report");
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
            state.activity.record_request("xhr-1".into(), "xhr".into(), false);
            state.activity.record_request("img-1".into(), "image".into(), false);
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
        assert!(started.elapsed() >= Duration::from_millis(900), "{:?}", started.elapsed());
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
            vec!["- [\"confirm\" dialog with message \"Leave?\"]: can be handled by playwright dialog"]
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
        let error = session
            .execute_tool_blocking(
                PlaywrightAction::Snapshot,
                &object(json!({})),
                &BrowserToolGrants::default(),
            )
            .unwrap_err();
        assert!(
            error.starts_with("Tool \"playwright snapshot\" does not handle the modal state."),
            "{error}"
        );
        assert!(error.contains("can be handled by playwright dialog"), "{error}");
    }

    /// Navigating waits for the new document: a load that never reaches DOMContentLoaded within
    /// the navigation timeout is an error naming the target, not a silent `loading: true`.
    #[test]
    fn navigation_reports_the_target_when_the_document_never_loads() {
        let session = BrowserSession::new("navigate-timeout");
        {
            let mut state = session.lock_state();
            state.synthetic_surface = true;
            state.status.has_page = true;
            state.status.loading = true;
        }
        let mut watch = session.action_watch();
        // Pretend the navigation started long ago so the bound is already spent.
        watch.started = Instant::now() - NAVIGATION_TIMEOUT - Duration::from_millis(1);
        let error = session
            .wait_for_navigation(&watch, "https://example.com/slow")
            .unwrap_err();
        assert!(
            error.starts_with("Timeout 60000ms exceeded while navigating to https://example.com/slow"),
            "{error}"
        );
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

        let result =
            cleanup_stale_profiles_in_root(&root, &HashSet::from([active.clone()]));
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
            menu_expanded: false,
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
            "active", "open", "loading", "pending", "menu", "user", "agent", "reserved", "missing",
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
                "menu" => snapshot.menu_expanded = true,
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
        for tool_name in [
            PlaywrightAction::Snapshot,
            PlaywrightAction::Click,
            PlaywrightAction::Evaluate,
            PlaywrightAction::Screenshot,
            PlaywrightAction::Navigate,
        ] {
            assert!(browser_tool_needs_live_slot(tool_name, &sleeping));
        }

        // Every action creates the page it needs (ensureTab), so a never-opened session takes a
        // slot for an observation just as it does for a navigation.
        let never_opened = BrowserStatus::default();
        assert!(browser_tool_needs_live_slot(
            PlaywrightAction::Navigate,
            &never_opened
        ));
        assert!(browser_tool_needs_live_slot(
            PlaywrightAction::Snapshot,
            &never_opened
        ));
        assert!(!browser_tool_needs_live_slot(
            PlaywrightAction::Snapshot,
            &BrowserStatus {
                has_page: true,
                ..BrowserStatus::default()
            }
        ));
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

    fn tab_ids(roster: &Value) -> Vec<String> {
        roster["tabs"]
            .as_array()
            .expect("roster must carry a tabs array")
            .iter()
            .map(|tab| tab["tab"].as_str().expect("tab id").to_owned())
            .collect()
    }

    #[test]
    fn agent_tab_tools_open_select_and_list_within_one_conversation() {
        let runtime = BrowserRuntime::default();
        let conversation = "tabs-conversation";

        let initial = runtime
            .execute_agent_tab_tool(conversation, PlaywrightAction::TabList, &object(json!({})))
            .expect("listing tabs must work before any tab exists");
        assert_eq!(initial["current"], json!(AGENT_PRIMARY_TAB_ID));
        assert!(tab_ids(&initial).is_empty());

        let opened = runtime
            .execute_agent_tab_tool(conversation, PlaywrightAction::TabNew, &object(json!({})))
            .expect("opening a tab must work");
        let tab = opened["opened"].as_str().expect("opened tab id").to_owned();
        assert_ne!(tab, AGENT_PRIMARY_TAB_ID);
        assert_eq!(opened["current"], json!(tab));
        assert_eq!(tab_ids(&opened), vec![tab.clone()]);

        // The page-level tools follow the pointer without any argument of their own.
        assert_eq!(
            runtime.agent_tab_session_id(conversation).unwrap(),
            format!("{conversation}#{tab}")
        );

        let selected = runtime
            .execute_agent_tab_tool(
                conversation,
                PlaywrightAction::TabSelect,
                &object(json!({"tab": AGENT_PRIMARY_TAB_ID})),
            )
            .expect("selecting the primary tab must work");
        assert_eq!(selected["current"], json!(AGENT_PRIMARY_TAB_ID));
        assert_eq!(
            runtime.agent_tab_session_id(conversation).unwrap(),
            conversation
        );

        // The primary page joins the roster once it exists, and always sorts first.
        runtime.session(conversation).unwrap();
        let listed = runtime
            .execute_agent_tab_tool(conversation, PlaywrightAction::TabList, &object(json!({})))
            .unwrap();
        assert_eq!(tab_ids(&listed), vec![AGENT_PRIMARY_TAB_ID.to_owned(), tab]);
    }

    #[test]
    fn agent_tab_ids_stay_inside_the_calling_conversation() {
        let runtime = BrowserRuntime::default();
        runtime.session("victim").unwrap();

        for tab in ["../victim", "victim", "agent-1#agent-2", "unknown"] {
            let error = runtime
                .execute_agent_tab_tool(
                    "attacker",
                    PlaywrightAction::TabSelect,
                    &object(json!({ "tab": tab })),
                )
                .expect_err("a tab id must never reach another conversation");
            assert!(
                error.contains("does not exist or is already closed")
                    || error.contains("browser tab identifier"),
                "{tab}: {error}"
            );
        }
        assert_eq!(
            runtime.agent_tab_session_id("attacker").unwrap(),
            "attacker"
        );

        // A tab session id is not itself a conversation: it cannot mint tabs of its own.
        let error = runtime
            .execute_agent_tab_tool("attacker#agent-1", PlaywrightAction::TabNew, &object(json!({})))
            .expect_err("tab tools must be called by the conversation");
        assert!(error.contains("not a tab session"), "{error}");
    }

    #[test]
    fn agent_tab_pointer_falls_back_when_its_tab_disappears() {
        let runtime = BrowserRuntime::default();
        let conversation = "tab-fallback";
        let opened = runtime
            .execute_agent_tab_tool(conversation, PlaywrightAction::TabNew, &object(json!({})))
            .unwrap();
        let tab = opened["opened"].as_str().unwrap().to_owned();
        let session_id = format!("{conversation}#{tab}");
        assert_eq!(
            runtime.agent_tab_session_id(conversation).unwrap(),
            session_id
        );

        // A trusted close is a fence the Agent cannot observe directly; the next tool must land on
        // the conversation's own page instead of failing on a session that no longer exists.
        lock_unpoison(&runtime.state).sessions.remove(&session_id);
        assert_eq!(
            runtime.agent_tab_session_id(conversation).unwrap(),
            conversation
        );
        assert!(!lock_unpoison(&runtime.state)
            .agent_tabs
            .contains_key(conversation));
    }

    #[test]
    fn agent_tabs_and_user_tabs_never_share_a_profile() {
        let runtime = BrowserRuntime::default();
        let conversation = "cookie-isolated";

        let opened = runtime
            .execute_agent_tab_tool(conversation, PlaywrightAction::TabNew, &object(json!({})))
            .unwrap();
        let session_id = format!("{conversation}#{}", opened["opened"].as_str().unwrap());
        let agent_tab = runtime.session(&session_id).unwrap();
        let user_tab = runtime.session(conversation).unwrap();
        let user_extra_tab = runtime
            .session(&format!("{conversation}#tab_9f3c"))
            .unwrap();

        // Every tab is its own single-use profile: an Agent tab can never see cookies a user tab
        // created, and two user tabs cannot see each other's either.
        assert_eq!(agent_tab.labels.profile_root, TAB_BROWSER_PROFILE_ROOT);
        assert!(is_profile_hash(&agent_tab.labels.profile));
        assert_ne!(agent_tab.labels.profile, user_tab.labels.profile);
        assert_ne!(agent_tab.labels.profile, user_extra_tab.labels.profile);
        assert_ne!(user_tab.labels.profile, user_extra_tab.labels.profile);

        // The tab roster no longer carries any cookie-jar shape: with per-tab profiles there is no
        // shared jar for a tab to be in or out of.
        let roster = runtime
            .execute_agent_tab_tool(conversation, PlaywrightAction::TabList, &object(json!({})))
            .unwrap();
        assert!(roster.get("cookieAccess").is_none());
        for tab in roster["tabs"].as_array().unwrap() {
            assert!(tab.get("cookies").is_none());
        }
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
    fn an_agent_tab_is_never_treated_as_the_users_signed_in_surface() {
        let runtime = BrowserRuntime::default();
        let conversation = "cookie-in-play";
        let opened = runtime
            .execute_agent_tab_tool(conversation, PlaywrightAction::TabNew, &object(json!({})))
            .unwrap();
        let session_id = format!("{conversation}#{}", opened["opened"].as_str().unwrap());

        // The Agent's own tab carries no user cookies by construction, so the takeover question
        // does not apply to it.
        assert_eq!(
            runtime.pending_credential_takeover(&session_id).unwrap(),
            None
        );
        // A user tab with no live page yet has no document to take over either.
        assert_eq!(
            runtime.pending_credential_takeover(conversation).unwrap(),
            None
        );
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
    fn agent_tab_count_is_bounded_and_main_closes_like_any_tab() {
        let runtime = BrowserRuntime::default();
        let conversation = "tab-budget";
        runtime.session(conversation).unwrap();
        for _ in 1..MAX_AGENT_BROWSER_TABS {
            runtime
                .execute_agent_tab_tool(conversation, PlaywrightAction::TabNew, &object(json!({})))
                .expect("tabs below the bound must open");
        }
        let error = runtime
            .execute_agent_tab_tool(conversation, PlaywrightAction::TabNew, &object(json!({})))
            .expect_err("the tab budget must be enforced");
        assert!(error.contains(PlaywrightAction::TabClose.as_str()), "{error}");

        // The primary page closes like any other tab (Playwright lets the current page close);
        // the roster then no longer lists it and the next action opens a fresh page.
        let roster = runtime
            .execute_agent_tab_tool(
                conversation,
                PlaywrightAction::TabClose,
                &object(json!({"tab": AGENT_PRIMARY_TAB_ID})),
            )
            .expect("the primary page closes like any tab");
        assert_eq!(roster.get("closed").and_then(Value::as_str), Some(AGENT_PRIMARY_TAB_ID));
        assert!(roster["tabs"]
            .as_array()
            .unwrap()
            .iter()
            .all(|tab| tab.get("tab").and_then(Value::as_str) != Some(AGENT_PRIMARY_TAB_ID)));
        // A later action reopens it instead of hitting the closed-tab tombstone.
        runtime
            .reopen_closed_session_for_agent(conversation)
            .expect("an agent action may reopen a closed page");
        assert!(!lock_unpoison(&runtime.state)
            .closed_session_ids
            .contains(conversation));
        runtime.session(conversation).expect("the page is mintable again");
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
