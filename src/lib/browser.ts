import { hasBackendRuntime, invoke, isTauriRuntime } from "./backend";
import { browserRendererMutationAuthority } from "./browserRendererMount";
import type { GitTarget } from "./git";

export type BrowserAction =
  | "back"
  | "forward"
  | "reload"
  | "stop"
  | "devtools"
  | "screenshot"
  | "zoom_in"
  | "zoom_out"
  | "zoom_reset"
  | "zoom"
  | "viewport"
  | "clear_data"
  | "find"
  | "print"
  | "select_element"
  | "theme"
  | "locale"
  | "take_control"
  | "handoff_agent"
  | "suspend"
  | "hide"
  | "occlude"
  | "project"
  | "close";

export type BrowserActionValue =
  | string
  | number
  | boolean
  | BrowserViewport
  | null;

export interface BrowserViewport {
  width: number;
  height: number;
}

/**
 * Logical CSS-pixel bounds of the browser panel relative to the main window content area.
 * `occludedTop` reserves trusted React chrome inside that rectangle; the native page viewport is
 * therefore `height - occludedTop`. Geometry is ignored by detached browser windows, while
 * `visible` continues to show/hide those windows.
 */
export interface BrowserPanelBounds {
  x: number;
  y: number;
  width: number;
  height: number;
  visible: boolean;
  occludedTop?: number;
}

export type BrowserControlOwner = "available" | "user" | "agent";

export interface BrowserControlStatus {
  owner: BrowserControlOwner;
  handoffRequested: boolean;
  requestedTool?: string | null;
  updatedAtMs: number;
}

/** State returned by every browser command. Rust should serialize this with rename_all = "camelCase". */
export interface BrowserStatus {
  /** Whether this conversation owns a browser WebView, even while it is hidden. */
  hasPage: boolean;
  /**
   * Whether Mework safely released this task's live Chromium surface to stay within the process
   * budget. Its isolated profile and last URL remain available and `openBrowser` resumes it.
   */
  suspended?: boolean;
  suspendedAtMs?: number | null;
  /** Whether the browser WebView is currently visible in the right-side page area. */
  open: boolean;
  loading: boolean;
  url: string;
  title?: string | null;
  canGoBack: boolean;
  canGoForward: boolean;
  /** WebView zoom factor (1 = 100%). Values expressed as a percentage are also accepted by the UI. */
  zoom: number;
  viewport: BrowserViewport;
  error?: string | null;
  screenshotPath?: string | null;
  agentActivity?: {
    tool: string;
    source: string;
    active: boolean;
    updatedAtMs: number;
  } | null;
  /** Persistent user/Agent ownership of the shared page. */
  control?: BrowserControlStatus;
  /**
   * Element-picker state. Poll-safe by construction: the picked element itself is drained by
   * `takeSelectedElement`, because two 700ms pollers would otherwise re-serialize its screenshot.
   */
  elementPicker?: BrowserElementPicker | null;
  /**
   * Whether the host currently has the page stacked beneath the renderer because a trusted
   * surface is drawn over it.
   *
   * Sleeping, suspending, hiding and closing all restack the page without the pane asking, and
   * none of them is an event the pane can hear. This is its only notice that the host no longer
   * believes the page is covered — and therefore that the still frame it is painting in the
   * page's place is a picture of something that is about to be live again.
   */
  occluded?: boolean;
  /**
   * Whether the host currently has the page stacked beneath the renderer because the pane is
   * painting a still frame of it in its place.
   *
   * Same reconciliation duty as `occluded` and the same silent host-side resets, but it carries
   * none of the meaning: a projected page is fully visible to the user as a picture and stays in
   * agent automation. The pane keeps the two apart so that resting — which is nearly all the time
   * — never looks to the host like a dialog standing over the page.
   */
  projected?: boolean;
}

export interface BrowserElementPicker {
  armed: boolean;
  pendingPick: boolean;
}

/** One element the user picked out of the page, as the host assembled it. */
export interface SelectedElement {
  /** Monotonic, so a replayed drain is recognisable. */
  sequence: number;
  tagName: string;
  id?: string | null;
  classes: string[];
  /** Restricted to a fixed allowlist by the host; everything here is page-controlled text. */
  attributes: Record<string, string>;
  computedStyles: Record<string, string>;
  boundingBox: { x: number; y: number; width: number; height: number };
  /** Base64 PNG of the element and its surroundings; empty when the crop failed. */
  screenshotBase64: string;
  /**
   * The composer attachment the crop became, once its upload landed. Assigned
   * by the renderer and never read from the host: it is what keeps the crop out
   * of the image strip — the chip already stands for it — and what ties
   * removing the chip to removing the image it carries.
   */
  screenshotImageId?: string;
  innerText?: string | null;
  parentPath?: string | null;
  reactComponent?: string | null;
  reactProps?: Record<string, unknown> | null;
  sourceFile?: string | null;
  outerHtml?: string | null;
  siblingHtml?: string | null;
}

export type BrowserCloseStatus = "closed" | "cleanupPending" | "rejected";

export type BrowserCloseErrorCode =
  | "invalidRequest"
  | "staleIntent"
  | "intentCollision"
  | "lifecycleUnavailable"
  | "lifecycleSuperseded"
  | "nativeCleanupFailed"
  | "nativeCleanupSurfaceHideFailed"
  | "internalFailure";

/**
 * Structured native close outcome. It intentionally carries no URL, title, or native error text:
 * once `intentAccepted` is true, Closed remains authoritative even when native resource cleanup
 * must be retried.
 */
export interface BrowserCloseDisposition {
  status: BrowserCloseStatus;
  intentAccepted: boolean;
  cleanupComplete: boolean;
  surfaceHidden: boolean;
  errorCode?: BrowserCloseErrorCode;
  message?: string;
}

const PREVIEW_ERROR = "此操作仅可在 Mework 桌面应用的内置浏览器中使用";

export function isDesktopBrowserRuntime(): boolean {
  return hasBackendRuntime();
}

function requireDesktopRuntime(): void {
  if (!isDesktopBrowserRuntime()) throw new Error(PREVIEW_ERROR);
}

async function rendererMountArgument(): Promise<Record<string, never> | {
  rendererMountId: string;
  rendererMountGeneration: number;
}> {
  // The authenticated loopback browser-dev router injects its own process-local lease and ignores
  // client-provided renderer fields. Production Tauri must always use the native page-load
  // challenge; no renderer may synthesize or cache a bypass value.
  if (!isTauriRuntime()) return {};
  return browserRendererMutationAuthority();
}

function previewStatus(url: string): BrowserStatus {
  return {
    hasPage: true,
    open: true,
    loading: false,
    url,
    title: "浏览器预览",
    canGoBack: false,
    canGoForward: false,
    zoom: 1,
    viewport: { width: 1200, height: 742 },
    control: {
      owner: "available",
      handoffRequested: false,
      requestedTool: null,
      updatedAtMs: 0
    }
  };
}

function lifecycleEpochArgument(lifecycleEpoch: number | undefined): {
  lifecycleEpoch?: number;
} {
  if (lifecycleEpoch === undefined) return {};
  if (
    !Number.isSafeInteger(lifecycleEpoch)
    || lifecycleEpoch < 1
    || lifecycleEpoch > Number.MAX_SAFE_INTEGER
  ) {
    throw new Error("浏览器生命周期 epoch 必须是正的安全整数");
  }
  return { lifecycleEpoch };
}

/**
 * Opens the native browser. Trusted UI callers pass the intent epoch they issued before any
 * asynchronous work so an older renderer cannot recreate a page after a newer close.
 * In a regular web preview this is the only supported operation.
 */
export async function openBrowser(
  sessionId: string,
  url?: string | null,
  lifecycleEpoch?: number
): Promise<BrowserStatus> {
  const target = url?.trim() || "about:blank";
  if (!isDesktopBrowserRuntime()) {
    const popup = window.open(target, "_blank", "noopener,noreferrer");
    if (!popup && target !== "about:blank") {
      throw new Error("浏览器阻止了新标签页，请允许弹出窗口后重试");
    }
    return previewStatus(target);
  }
  if (lifecycleEpoch === undefined) {
    throw new Error("打开内置浏览器需要可信 UI 签发的生命周期 epoch");
  }
  const lifecycleArgument = lifecycleEpochArgument(lifecycleEpoch);
  const rendererAuthority = await rendererMountArgument();
  return invoke<BrowserStatus>("browser_open", {
    sessionId,
    url: url?.trim() || null,
    ...lifecycleArgument,
    ...rendererAuthority
  });
}

/**
 * Permanently closes one exact native browser generation.
 *
 * Callers must inspect `intentAccepted` separately from `cleanupComplete`: a cleanup retry never
 * turns an already accepted Closed epoch back into Open.
 */
export async function closeBrowserSession(
  sessionId: string,
  lifecycleEpoch: number
): Promise<BrowserCloseDisposition> {
  requireDesktopRuntime();
  const lifecycleArgument = lifecycleEpochArgument(lifecycleEpoch);
  const rendererAuthority = await rendererMountArgument();
  return invoke<BrowserCloseDisposition>("browser_close", {
    sessionId,
    ...lifecycleArgument,
    ...rendererAuthority
  });
}

export async function getBrowserStatus(sessionId: string): Promise<BrowserStatus> {
  requireDesktopRuntime();
  const rendererAuthority = await rendererMountArgument();
  return invoke<BrowserStatus>("browser_status", {
    sessionId,
    ...rendererAuthority
  });
}

/** Synchronizes the native remote page with the currently active trusted sidebar. */
export async function setBrowserPanelBounds(
  sessionId: string,
  bounds: BrowserPanelBounds,
  lifecycleEpoch: number
): Promise<BrowserStatus> {
  requireDesktopRuntime();
  const lifecycleArgument = lifecycleEpochArgument(lifecycleEpoch);
  const rendererAuthority = await rendererMountArgument();
  return invoke<BrowserStatus>("browser_set_panel_bounds", {
    sessionId,
    bounds,
    ...lifecycleArgument,
    ...rendererAuthority
  });
}

/** A page capture by value, for a renderer layer that has to draw on top of it. */
export interface BrowserPageCapture {
  /** Base64 PNG, no data-URL prefix. */
  data: string;
  width: number;
  height: number;
}

/**
 * Captures the live page for the annotate surface.
 *
 * Null rather than an error when there is nothing to capture: the pane opens the drawing layer
 * either way and falls back to a blank backdrop, which is better than refusing to open at all.
 */
export async function captureBrowserPage(sessionId: string): Promise<BrowserPageCapture | null> {
  requireDesktopRuntime();
  const rendererAuthority = await rendererMountArgument();
  return invoke<BrowserPageCapture | null>("browser_capture_page", {
    sessionId,
    ...rendererAuthority
  });
}

/**
 * Opens a native file dialog and shows the chosen file; null when the user cancels.
 *
 * `target` only decides where the dialog opens — the reference passes the session's working
 * directory as its `defaultPath`. The picked file is what authorizes the preview, so a target the
 * host cannot resolve costs nothing but a less convenient starting directory.
 */
export async function openLocalFileInBrowser(
  sessionId: string,
  target: GitTarget | null = null
): Promise<BrowserStatus | null> {
  requireDesktopRuntime();
  const rendererAuthority = await rendererMountArgument();
  return invoke<BrowserStatus | null>("browser_open_local_file", {
    sessionId,
    target,
    ...rendererAuthority
  });
}

/** Drains the element the user picked, if any. Separate from the poll: the crop is large. */
export async function takeSelectedElement(sessionId: string): Promise<SelectedElement | null> {
  requireDesktopRuntime();
  const rendererAuthority = await rendererMountArgument();
  return invoke<SelectedElement | null>("browser_take_selected_element", {
    sessionId,
    ...rendererAuthority
  });
}

export async function navigateBrowser(sessionId: string, url: string): Promise<BrowserStatus> {
  requireDesktopRuntime();
  const rendererAuthority = await rendererMountArgument();
  return invoke<BrowserStatus>("browser_navigate", {
    sessionId,
    url,
    ...rendererAuthority
  });
}

export async function performBrowserAction(
  sessionId: string,
  action: BrowserAction,
  value: BrowserActionValue = null,
  lifecycleEpoch?: number
): Promise<BrowserStatus> {
  requireDesktopRuntime();
  if ((action === "close" || action === "hide") && lifecycleEpoch === undefined) {
    throw new Error(
      action === "close"
        ? "关闭内置浏览器需要可信 UI 签发的生命周期 epoch"
        : "收起内置浏览器需要可信 UI 签发的生命周期 epoch"
    );
  }
  if (lifecycleEpoch !== undefined && action !== "close" && action !== "hide") {
    throw new Error("浏览器生命周期 epoch 只能用于收起或关闭操作");
  }
  const lifecycleArgument = lifecycleEpochArgument(lifecycleEpoch);
  const rendererAuthority = await rendererMountArgument();
  return invoke<BrowserStatus>("browser_action", {
    sessionId,
    action,
    value,
    ...lifecycleArgument,
    ...rendererAuthority
  });
}
