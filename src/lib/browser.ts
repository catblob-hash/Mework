import { hasBackendRuntime, invoke, isTauriRuntime } from "./backend";
import { browserRendererMutationAuthority } from "./browserRendererMount";

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
  | "clear_data"
  | "find"
  | "print"
  | "menu"
  | "theme"
  | "locale"
  | "take_control"
  | "handoff_agent"
  | "suspend"
  | "hide"
  | "close";

export interface BrowserMenuRegion {
  /**
   * Monotonically increasing request generation. Native window-region work is asynchronous, so
   * both Rust and the renderer ignore completions older than the latest open/resize/close request.
   */
  generation: number;
  expanded: boolean;
  /**
   * Logical CSS pixels relative to the remote Chromium page's top-left corner. The native host
   * subtracts this region from the WebView window so trusted React menu content remains visible
   * without resizing the page viewport.
   */
  rect?: {
    x: number;
    y: number;
    width: number;
    height: number;
  };
  /** Extra logical pixels reserved for the trusted menu's shadow/focus ring. */
  shadow?: number;
}

export type BrowserActionValue = string | number | boolean | BrowserMenuRegion | null;

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
