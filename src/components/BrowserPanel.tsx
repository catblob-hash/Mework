import {
  Check,
  ChevronLeft,
  ChevronRight,
  Globe2,
  LoaderCircle,
  MousePointerClick,
  Pencil,
  RotateCw,
  Trash2,
  X
} from "lucide-react";
import { useCallback, useEffect, useId, useMemo, useRef, useState, useSyncExternalStore } from "react";
import type { FormEvent, KeyboardEvent as ReactKeyboardEvent, ReactNode } from "react";
import { getI18nSnapshot, translate, useI18n } from "../i18n";
import {
  captureBrowserPage,
  getBrowserStatus,
  navigateBrowser,
  openLocalFileInBrowser,
  performBrowserAction,
  takeSelectedElement,
  type BrowserAction,
  type BrowserStatus,
  type SelectedElement
} from "../lib/browser";
import { isBrowserDevRuntime } from "../lib/backend";
import { useBrowserPageEngagement } from "../lib/browserPageEngagement";
import {
  anyRectOverlaps,
  createBrowserSnapshotStore,
  useBrowserPageProjection,
  type BrowserSnapshotStore
} from "../lib/browserPageProjection";
import {
  floatingSurfaceRects,
  floatingSurfacesOver,
  subscribeFloatingSurfaces
} from "../lib/floatingSurfaces";
import type { GitTarget } from "../lib/git";
import type { SidePaneId } from "../lib/sidePanes";
import { previewServerAddress, type PreviewTarget } from "../lib/preview";
import { IconButton } from "./Common";
import { SidePane, type SidePaneBounds } from "./SidePane";
import { SketchOverlay } from "./Sketch";
import {
  PreviewFileMenuItems,
  PreviewIdlePage,
  PreviewLogDrawer,
  PreviewLogsMenuItem,
  PreviewServerMenuItems,
  PreviewStartFailedCard,
  PreviewStartPage,
  PreviewStartingCard,
  PreviewStoppedCard,
  previewBodyState,
  usePreviewServerLogs,
  usePreviewServers,
  type PreviewBodyState,
  type PreviewServerRow
} from "./PreviewPane";

const emptyStatus: BrowserStatus = {
  hasPage: false,
  open: false,
  loading: false,
  url: "about:blank",
  title: null,
  canGoBack: false,
  canGoForward: false,
  zoom: 1,
  error: null,
  viewport: { width: 560, height: 720 },
  control: {
    owner: "available",
    handoffRequested: false,
    requestedTool: null,
    updatedAtMs: 0
  }
};

export function normalizeBrowserAddress(
  input: string,
  messages: {
    empty?: string;
    unsupportedProtocol?: string;
  } = {}
): string {
  const value = input.trim();
  if (!value) {
    throw new Error(messages.empty ?? translate(
      getI18nSnapshot().resolvedLanguage,
      "请输入网址或搜索内容",
      "Enter a URL or search query"
    ));
  }
  if (/^about:blank$/i.test(value)) return "about:blank";
  if (/^(localhost|127(?:\.\d{1,3}){3}|\[::1\])(?::\d+)?(?:[/#?]|$)/i.test(value)) {
    return `http://${value}`;
  }
  if (/^[a-z][a-z\d+.-]*:/i.test(value)) {
    const url = new URL(value);
    if (url.protocol !== "http:" && url.protocol !== "https:") {
      throw new Error(messages.unsupportedProtocol ?? translate(
        getI18nSnapshot().resolvedLanguage,
        "侧栏浏览器只支持 HTTP(S) 地址",
        "The sidebar browser only supports HTTP(S) addresses"
      ));
    }
    return url.toString();
  }
  if (/\s/.test(value)) return `https://www.google.com/search?q=${encodeURIComponent(value)}`;
  if (value.includes(".") || value.includes(":")) return `https://${value}`;
  return `https://www.google.com/search?q=${encodeURIComponent(value)}`;
}

/** `host:port` and `pathname+search+hash`, the two halves the address chip prints separately. */
export function splitBrowserAddress(url: string): { host: string; path: string } | null {
  if (!url || /^about:blank$/i.test(url)) return null;
  try {
    const parsed = new URL(url);
    return { host: parsed.host, path: `${parsed.pathname}${parsed.search}${parsed.hash}` };
  } catch {
    return { host: url, path: "" };
  }
}

/** The annotated composite as a composer attachment; the send pipeline only takes files. */
async function annotationImageFile(dataUrl: string): Promise<File> {
  const response = await fetch(dataUrl);
  const blob = await response.blob();
  return new File([blob], "page-annotation.png", { type: "image/png" });
}

type EmbeddedUiTheme = "day" | "night";

function currentEmbeddedUiTheme(): EmbeddedUiTheme {
  return typeof document !== "undefined" && document.documentElement.dataset.theme === "night" ? "night" : "day";
}

function useEmbeddedUiTheme(): EmbeddedUiTheme {
  const [theme, setTheme] = useState<EmbeddedUiTheme>(currentEmbeddedUiTheme);
  useEffect(() => {
    const root = document.documentElement;
    const observer = new MutationObserver(() => setTheme(currentEmbeddedUiTheme()));
    observer.observe(root, { attributes: true, attributeFilter: ["data-theme"] });
    return () => observer.disconnect();
  }, []);
  return theme;
}

interface BrowserPanelProps {
  native: boolean;
  /**
   * The pane this browser owns. Its 32px title bar *is* the browser's toolbar, so the panel draws
   * the pane rather than living inside one somebody else drew.
   */
  paneId: SidePaneId;
  onPaneClose: () => void;
  onPaneFocus?: () => void;
  /** Pane controls the sidebar owns, drawn before the close ×. */
  paneTrailing?: ReactNode;
  /** Whether this pane is shown alone over the workspace. */
  paneExpanded?: boolean;
  onPaneToggleExpand?: () => void;
  /** Stages an annotated page capture in the conversation composer. */
  onAttachImage?: (file: File) => void | Promise<void>;
  /** Receives an element the user picked out of the page. */
  onElementPicked?: (element: SelectedElement) => void;
  /** Publishes the pane body rectangle — the box the native page is positioned from. */
  onContentBoundsChange?: (bounds: SidePaneBounds) => void;
  /**
   * Native browser session this tab drives. A conversation's first tab uses the conversation id;
   * extra tabs use their own session id. Every tab runs in its own single-use Chromium profile:
   * a sign-in made in one tab exists only there and dies when the tab closes.
   */
  sessionId?: string;
  active?: boolean;
  /**
   * Workspace whose `.mework/launch.json` this pane's dev-server picker reads and whose servers it
   * starts — on whichever machine it is. Without it the pane is a plain browser: the picker shows
   * its empty state and no preview command is ever sent.
   */
  target?: PreviewTarget | null;
  /**
   * The checkout on this computer the "Open file" picker starts in. Only a workspace here has one;
   * the picker itself always reads this computer's files.
   */
  fileTarget?: GitTarget | null;
  /**
   * The page strip. Given one, it takes the pane's title bar and the browser toolbar moves to the
   * row under it, the way a browser window puts its tabs above its address bar.
   */
  tabs?: ReactNode;
  /**
   * Set while the machine this page's workspace is on cannot be reached: the page keeps what it
   * had, and a strip over it says why nothing it asks for is answering.
   */
  linkNotice?: string | null;
  /**
   * Viewport rectangle of a trusted overlay that another component draws over this panel. The
   * native page is a child window above the renderer, so it has to be taken out from under the
   * overlay for the overlay to be visible at all.
   */
  trustedOverlayRect?: BrowserOverlayRect | null;
  /**
   * Renderer-owned height at the bottom of the pane, published whenever the log drawer opens,
   * closes, or is dragged. The host sizes the native page from the pane rectangle alone, so a
   * parent that wants the page to shrink rather than sit behind the drawer must subtract this
   * from `height`.
   */
  onReservedBottomChange?: (reservedBottom: number) => void;
  /**
   * Opens this tab's page the way selecting the tab does. The address bar calls it first when
   * the tab has no page — one whose open failed, or has not happened yet — so that typing an
   * address is enough to get one.
   */
  onOpenPage?: () => Promise<void>;
  /** Uses the real Rust browser session while mirroring its URL in a sandboxed iframe. */
  browserDev?: boolean;
}

type BrowserRect = Pick<DOMRect, "left" | "top" | "right" | "bottom" | "width" | "height">;

/** Trusted overlay geometry another component publishes in viewport CSS pixels. */
export type BrowserOverlayRect = BrowserRect;

/**
 * The projection of the page, as its own subscriber.
 *
 * The frame is refreshed on a timer for as long as the page is the thing being shown, and this
 * panel also owns the toolbar, the menu and the log drawer. Subscribing here rather than holding
 * the frame in panel state keeps a once-a-second capture from re-rendering all of that.
 */
function BrowserPageStill({ store, contentKey }: { store: BrowserSnapshotStore; contentKey: string | null }) {
  const snapshot = useSyncExternalStore(store.subscribe, store.get, store.get);
  // A frame belonging to another session is not stale, it is wrong — a picture of a different
  // page under this one's chrome. The store is cleared on a session change too, but that lands a
  // commit later, and this is the render in between.
  if (!snapshot || snapshot.contentKey !== (contentKey ?? "")) return null;
  // Sized by CSS rather than from the captured box: a pane resized while the page is covered
  // would otherwise leave the pane's own background showing beside a still of the old size, and a
  // page cropped at the edge reads as a page, where a gap reads as a rendering fault.
  return (
    <img
      className="browser-panel__still"
      src={snapshot.src}
      alt=""
      draggable={false}
    />
  );
}

type OpenBrowserMenu = "overflow" | null;

/** How long the cleared-profile confirmation holds the menu open before it closes itself. */
const BROWSER_CLEARED_FLASH_MS = 1_500;

function BackendBrowserPanel({
  paneId,
  onPaneClose,
  onPaneFocus,
  paneTrailing,
  paneExpanded = false,
  onPaneToggleExpand,
  onAttachImage,
  onElementPicked,
  onContentBoundsChange,
  sessionId,
  previewFrame = false,
  nativeChild = false,
  active = true,
  target = null,
  fileTarget = null,
  tabs,
  linkNotice = null,
  trustedOverlayRect = null,
  onReservedBottomChange,
  onOpenPage
}: {
  paneId: SidePaneId;
  onPaneClose: () => void;
  onPaneFocus?: () => void;
  paneTrailing?: ReactNode;
  paneExpanded?: boolean;
  onPaneToggleExpand?: () => void;
  onAttachImage?: (file: File) => void | Promise<void>;
  onElementPicked?: (element: SelectedElement) => void;
  onContentBoundsChange?: (bounds: SidePaneBounds) => void;
  sessionId?: string;
  previewFrame?: boolean;
  nativeChild?: boolean;
  active?: boolean;
  target?: PreviewTarget | null;
  fileTarget?: GitTarget | null;
  tabs?: ReactNode;
  linkNotice?: string | null;
  trustedOverlayRect?: BrowserOverlayRect | null;
  onReservedBottomChange?: (reservedBottom: number) => void;
  onOpenPage?: () => Promise<void>;
}) {
  const { resolvedLanguage, t } = useI18n();
  const menuId = useId();
  const menuTriggerId = useId();
  const embeddedTheme = useEmbeddedUiTheme();
  const [status, setStatus] = useState<BrowserStatus>(emptyStatus);
  const [address, setAddress] = useState("");
  const [editingAddress, setEditingAddress] = useState(false);
  const [pending, setPending] = useState(false);
  const [openMenu, setOpenMenu] = useState<OpenBrowserMenu>(null);
  const [clearDataOpen, setClearDataOpen] = useState(false);
  const [annotating, setAnnotating] = useState(false);
  const pickerArmed = status.elementPicker?.armed === true;
  const pickerArmedRef = useRef(false);
  pickerArmedRef.current = pickerArmed;
  const [annotateBackdrop, setAnnotateBackdrop] = useState<string | null>(null);
  // A capture in flight outlives the click that started it; only the newest one may set a backdrop.
  const annotateGenerationRef = useRef(0);
  const sketchRef = useRef<HTMLDivElement>(null);
  const [clearingData, setClearingData] = useState<"idle" | "running" | "cleared">("idle");
  const clearedFlashRef = useRef<number | undefined>(undefined);
  const [logsOpen, setLogsOpen] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const menuOpenRef = useRef(false);
  /** Whether any trusted surface is over the page, recomputed by the measurement pass below. */
  const [covered, setCovered] = useState(false);
  /** The projection painted in the page's place while the page is sunk. */
  const [snapshotStore] = useState(createBrowserSnapshotStore);
  /** Last height reported to the parent, so an inline callback prop cannot turn into a flood. */
  const reservedBottomRef = useRef<number | null>(null);
  const editingAddressRef = useRef(false);
  const panelRef = useRef<HTMLDivElement>(null);
  /** The page area proper: the pane box minus whatever the log drawer took off the bottom. */
  const pageAreaRef = useRef<HTMLDivElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  const drawerRef = useRef<HTMLDivElement | null>(null);
  const bodyCardRef = useRef<HTMLDivElement>(null);
  const errorRef = useRef<HTMLDivElement>(null);
  const noticeRef = useRef<HTMLDivElement>(null);
  editingAddressRef.current = editingAddress;

  const navigate = useCallback(async (raw: string) => {
    if (!sessionId) return;
    try {
      const url = normalizeBrowserAddress(raw, {
        empty: t("请输入网址或搜索内容", "Enter a URL or search query"),
        unsupportedProtocol: t("侧栏浏览器只支持 HTTP(S) 地址", "The sidebar browser only supports HTTP(S) addresses")
      });
      setPending(true);
      setError(null);
      try {
        // Navigation moves a page; it does not make one. A tab without a page gets it the way
        // selecting the tab does, and the address goes to that page.
        if (nativeChild && !status.hasPage && !status.suspended && onOpenPage) {
          await onOpenPage();
          // An open that failed says why only in the status it leaves behind.
          const opened = await getBrowserStatus(sessionId);
          if (!opened.hasPage) {
            throw new Error(opened.error || t("无法打开这个标签页的页面", "Could not open this tab's page"));
          }
        }
        const next = await navigateBrowser(sessionId, url);
        setStatus(next);
        setAddress(next.url === "about:blank" ? "" : next.url);
      } finally {
        setPending(false);
      }
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    }
  }, [nativeChild, onOpenPage, sessionId, status.hasPage, status.suspended, t]);

  const openServerPage = useCallback((row: PreviewServerRow) => {
    void navigate(previewServerAddress({ port: row.port, url: row.url }));
  }, [navigate]);

  const preview = usePreviewServers(target, openServerPage);
  // A drawer that only ever follows a *running* row is empty in the two states where its output
  // matters most: while a start is still in flight, and right after one has failed.
  const logServer = useMemo(() => (
    preview.rows.find((row) => row.running && row.server)
    ?? preview.rows.find((row) => row.starting && row.server)
    ?? null
  ), [preview.rows]);
  const logLines = usePreviewServerLogs(logServer?.server?.handle ?? null, logsOpen);

  // The machine being away is said once, over whatever the pane is showing. A read that failed for
  // some other reason is only worth a strip when there is nothing else on screen to go by.
  const notice = linkNotice ?? (
    preview.unreachable && preview.configurations === null ? preview.unreachable : null
  );

  const bodyState: PreviewBodyState = useMemo(() => previewBodyState({
    url: status.url,
    configurationCount: preview.configurations?.servers.length ?? 0,
    rows: preview.rows,
    pendingName: preview.pendingName,
    startError: preview.startError,
    stopped: preview.stopped
  }), [status.url, preview.configurations, preview.rows, preview.pendingName, preview.startError, preview.stopped]);

  useEffect(() => {
    if (!sessionId) return;
    let cancelled = false;
    let timer = 0;
    const poll = async () => {
      try {
        const next = await getBrowserStatus(sessionId);
        if (cancelled) return;
        setStatus(next);
        if (!editingAddressRef.current) {
          setAddress(next.url === "about:blank" ? "" : next.url);
        }
        // The pick itself never rides the poll — its crop would be re-serialized every 700ms —
        // so the poll only learns that one is waiting and drains it through its own command.
        if (next.elementPicker?.pendingPick && onElementPicked) {
          const picked = await takeSelectedElement(sessionId);
          if (cancelled || !picked) return;
          onElementPicked(picked);
        }
      } catch (reason) {
        if (!cancelled) setError(reason instanceof Error ? reason.message : String(reason));
      } finally {
        if (!cancelled) timer = window.setTimeout(() => void poll(), 700);
      }
    };
    void poll();
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
      menuOpenRef.current = false;
      // A pane that unmounts while armed would leave the page in inspect mode with nothing left
      // to turn it off.
      if (pickerArmedRef.current) {
        pickerArmedRef.current = false;
        void performBrowserAction(sessionId, "select_element", false).catch(() => undefined);
      }
    };
  }, [sessionId, nativeChild]);

  useEffect(() => {
    if (!nativeChild || !sessionId || !status.hasPage) return;
    const locale = resolvedLanguage.toLowerCase().startsWith("zh") ? "zh-CN" : "en-US";
    void (async () => {
      await performBrowserAction(sessionId, "theme", embeddedTheme);
      await performBrowserAction(sessionId, "locale", locale);
    })().catch(() => undefined);
  }, [sessionId, embeddedTheme, nativeChild, resolvedLanguage, status.hasPage]);

  const run = useCallback(async (task: () => Promise<BrowserStatus>) => {
    if (pending) return null;
    setPending(true);
    setError(null);
    try {
      const next = await task();
      setStatus(next);
      setAddress(next.url === "about:blank" ? "" : next.url);
      return next;
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
      return null;
    } finally {
      setPending(false);
    }
  }, [pending]);

  const action = useCallback((name: BrowserAction, value: string | number | boolean | null = null) => {
    if (!sessionId) return Promise.resolve(null);
    return run(() => performBrowserAction(sessionId, name, value));
  }, [sessionId, run]);

  /** Every trusted React surface currently drawn over the native page, in viewport pixels. */
  const trustedSurfaceRects = useCallback((): BrowserRect[] => {
    const rects: BrowserRect[] = [];
    if (trustedOverlayRect) rects.push(trustedOverlayRect);
    if (sketchRef.current) rects.push(sketchRef.current.getBoundingClientRect());
    if (menuRef.current) rects.push(menuRef.current.getBoundingClientRect());
    if (bodyCardRef.current) rects.push(bodyCardRef.current.getBoundingClientRect());
    // The error strip floats over the page now that there is no chrome strip to sit in, so it
    // covers the page just like any other renderer layer drawn over it and must be counted too.
    if (errorRef.current) rects.push(errorRef.current.getBoundingClientRect());
    if (noticeRef.current) rects.push(noticeRef.current.getBoundingClientRect());
    // The log drawer is deliberately absent: it does not cover the page, it takes height from it.
    // `onReservedBottomChange` shrinks the page by exactly the drawer's height, so the two never
    // overlap — and counting it here would freeze the page for as long as the drawer is open.
    return rects;
  }, [trustedOverlayRect]);

  /**
   * Whether anything trusted is drawn over the page right now.
   *
   * A boolean, not a shape: the page is taken out from under every surface at once, so where they
   * are only matters for deciding whether they are over the page at all.
   */
  const measureCovered = useCallback((): boolean => {
    if (!active) return false;
    // The page area, not the whole pane: the log drawer takes its height off the bottom, so the
    // pane's own box would report an overlap with the drawer that the page never has.
    const page = pageAreaRef.current;
    if (!page) return false;
    const pageRect = page.getBoundingClientRect();
    return (
      anyRectOverlaps(trustedSurfaceRects(), pageRect)
      // This pane's own chrome above, plus every floating surface the rest of the app has open
      // over the page — menus, dialogs, viewers. Without the second half only the surfaces this
      // component renders would count, and anything popped from the main view would be painted
      // over by the page.
      || floatingSurfacesOver(floatingSurfaceRects(), pageRect).length > 0
    );
  }, [active, trustedSurfaceRects]);

  /**
   * Tells the host where the page belongs in the z-order.
   *
   * A refusal is not the user's to see: the pane measures the moment it mounts, while the host
   * may still be creating the page, and "the embedded browser is not open" is the expected answer
   * in that window. The status poll is what reconciles the two afterwards.
   */
  const sendParked = useCallback(async (parked: boolean) => {
    if (!nativeChild || !sessionId) return;
    const next = await performBrowserAction(sessionId, "project", parked).catch(() => null);
    if (next) setStatus(next);
  }, [nativeChild, sessionId]);

  /**
   * Tells the host whether the sink is a cover or the pane at rest.
   *
   * Only this one carries meaning: covering takes the page out of agent automation, because the
   * user is looking at a dialog and an Agent click landing behind it would be a click nobody
   * could see. Resting must not, or the Agent would lose the page every time the pointer moved.
   */
  const sendCovered = useCallback(async (isCovered: boolean) => {
    if (!nativeChild || !sessionId) return;
    const next = await performBrowserAction(sessionId, "occlude", isCovered).catch(() => null);
    if (next) setStatus(next);
  }, [nativeChild, sessionId]);

  const capture = useCallback(() => (
    sessionId ? captureBrowserPage(sessionId) : Promise.resolve(null)
  ), [sessionId]);

  /** Whether the pane is showing the page itself rather than one of its own cards. */
  const pagePresented = bodyState.kind === "page";

  const engaged = useBrowserPageEngagement({
    pageAreaRef,
    enabled: Boolean(nativeChild && sessionId && status.hasPage && pagePresented && active)
  });

  /**
   * Whether the page belongs beneath the renderer.
   *
   * Sunk is the resting state, not the exceptional one. A native child window paints over every
   * HTML layer in the app, so leaving the page on top means the pane's rounded corners, its
   * transitions, and every menu or dialog the rest of the app opens are all painted over by a
   * rectangle React does not control. It comes up only for the one thing a projection cannot do:
   * be used. Scrolling, text selection, the IME and native right-click all belong to the real
   * page, so the real page is what the user gets for as long as they are using it.
   */
  const parked = !(pagePresented && engaged && !covered);

  useBrowserPageProjection({
    contentKey: sessionId ?? null,
    enabled: Boolean(nativeChild && sessionId && status.hasPage),
    hasContent: pagePresented,
    parked,
    covered,
    placeholderRef: pageAreaRef,
    hostParked: status.projected === true,
    hostCovered: status.occluded === true,
    setParked: sendParked,
    setCovered: sendCovered,
    capture,
    sink: snapshotStore
  });

  /**
   * Opens or closes the pane's own `⋮` menu.
   *
   * Nothing here has to reach the host any more: the menu is HTML, and the page goes under it
   * because the measurement pass sees a new surface over the page, not because this function said
   * so. That is what took the generation fencing out — there is no longer an asynchronous native
   * request whose late reply could resurrect a menu a newer close already took down.
   */
  const toggleMenu = useCallback((
    next: OpenBrowserMenu,
    restoreFocus = next === null
  ): void => {
    menuOpenRef.current = next !== null;
    if (next === null) {
      if (restoreFocus) document.getElementById(menuTriggerId)?.focus();
      setOpenMenu(null);
      setClearDataOpen(false);
      return;
    }
    setError(null);
    setOpenMenu(next);
  }, [menuTriggerId]);

  useEffect(() => {
    if (openMenu !== "overflow") return;
    menuRef.current
      ?.querySelector<HTMLButtonElement>('[role="menuitem"]:not(:disabled)')
      ?.focus();
  }, [openMenu]);

  useEffect(() => {
    if (active) return;
    if (!menuOpenRef.current) return;
    menuOpenRef.current = false;
    setOpenMenu(null);
    setClearDataOpen(false);
  }, [active]);

  useEffect(() => {
    if (!nativeChild || !sessionId || !trustedOverlayRect) return;
    // A trusted overlay drawn by the sidebar is the newer intent: this pane's own menu closes so
    // the card it is covering is the only floating layer left to keep visible.
    if (menuOpenRef.current) {
      menuOpenRef.current = false;
      setOpenMenu(null);
      setClearDataOpen(false);
    }
  }, [nativeChild, sessionId, trustedOverlayRect]);


  /**
   * One measurement pass: whether anything is over the page, and the pane height the log drawer
   * took from it. Both are driven by the same observers because both change for exactly the same
   * reasons — a resize, a drawer drag, or a body state swap.
   */
  const syncGeometry = useCallback(() => {
    setCovered(measureCovered());
    if (!onReservedBottomChange) return;
    const reserved = drawerRef.current?.getBoundingClientRect().height ?? 0;
    if (reservedBottomRef.current === reserved) return;
    reservedBottomRef.current = reserved;
    onReservedBottomChange(reserved);
  }, [measureCovered, onReservedBottomChange]);

  useEffect(() => {
    // Measured now and again on the next frame: a cancelled frame must never be the only pass, or
    // a surface that appears and is superseded before the frame runs leaves the page covered.
    syncGeometry();
    const frame = window.requestAnimationFrame(syncGeometry);
    const observer = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(syncGeometry);
    if (observer && panelRef.current) observer.observe(panelRef.current);
    if (observer && drawerRef.current) observer.observe(drawerRef.current);
    window.addEventListener("resize", syncGeometry);
    // A tile that slides into the space a closed pane gave up moves this panel without resizing
    // it, and whether a surface is over the page is decided against the page's own box: a surface
    // anchored to the viewport rather than to the pane — a centred dialog, the fork tray — stops
    // overlapping without either of them changing size. The observer cannot see a move, so the
    // transition has to say so itself. Only motion of a box this panel sits inside can move it,
    // which is what the containment test is.
    let motionFrame = 0;
    const syncAfterMotion = (event: Event) => {
      if (!(event.target instanceof Element) || !event.target.contains(panelRef.current)) return;
      window.cancelAnimationFrame(motionFrame);
      motionFrame = window.requestAnimationFrame(syncGeometry);
    };
    window.addEventListener("transitionend", syncAfterMotion);
    window.addEventListener("animationend", syncAfterMotion);
    // A menu or dialog elsewhere in the app opens and closes without re-rendering this pane, so the
    // registry has to drive the re-measure itself.
    const unsubscribe = subscribeFloatingSurfaces(syncGeometry);
    return () => {
      window.cancelAnimationFrame(frame);
      window.cancelAnimationFrame(motionFrame);
      observer?.disconnect();
      window.removeEventListener("resize", syncGeometry);
      window.removeEventListener("transitionend", syncAfterMotion);
      window.removeEventListener("animationend", syncAfterMotion);
      unsubscribe();
    };
  }, [
    syncGeometry,
    openMenu,
    annotating,
    clearDataOpen,
    logsOpen,
    bodyState.kind,
    error,
    notice,
    trustedOverlayRect,
    // Presenting or withdrawing the page changes whether there is anything to cover, so the pass
    // that decides has to run on the far side of each.
    status.hasPage,
    status.open
  ]);

  useEffect(() => {
    if (openMenu === null) return;
    const onPointerDown = (event: MouseEvent) => {
      const target = event.target as Node;
      if (menuRef.current?.contains(target)) return;
      if (document.getElementById(menuTriggerId)?.contains(target)) return;
      toggleMenu(null, false);
    };
    document.addEventListener("mousedown", onPointerDown);
    return () => document.removeEventListener("mousedown", onPointerDown);
  }, [openMenu, menuTriggerId, toggleMenu]);

  /**
   * Closes the menu, then does what the item said.
   *
   * Closing is now a state update rather than a request that can fail, so the sequencing is only
   * about order: the menu is gone before the action it started runs, and nothing the action does
   * can be mistaken for the menu still being open.
   */
  const runMenuAction = useCallback(async (task: () => Promise<unknown>) => {
    toggleMenu(null, true);
    await task();
  }, [toggleMenu]);

  /**
   * The one menu action that stays in the menu while it runs.
   *
   * Clearing a profile leaves nothing on screen to look at — same tab, same page, same address —
   * so a close-then-clear reads as a dead item even when it worked. The confirmation *is* the
   * report: the row holds its own spinner, says what happened, and the menu closes itself once
   * the user has had time to read it.
   */
  const clearBrowsingData = useCallback(async () => {
    setClearingData("running");
    // `action` reports its own failure in the error strip and answers null for it.
    if (!await action("clear_data")) {
      setClearingData("idle");
      return;
    }
    setClearingData("cleared");
    window.clearTimeout(clearedFlashRef.current);
    clearedFlashRef.current = window.setTimeout(() => {
      setClearingData("idle");
      // Only the menu this confirmation is sitting in may be closed by it: dismissed in the
      // meantime, the close would pull focus back to the trigger out of nowhere.
      if (menuOpenRef.current) toggleMenu(null, true);
    }, BROWSER_CLEARED_FLASH_MS);
  }, [action, toggleMenu]);

  useEffect(() => () => window.clearTimeout(clearedFlashRef.current), []);

  const handleMenuKeyDown = useCallback((event: ReactKeyboardEvent<HTMLDivElement>) => {
    if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      toggleMenu(null, true);
      return;
    }
    if (!["ArrowDown", "ArrowUp", "Home", "End"].includes(event.key)) return;
    const items = Array.from(
      event.currentTarget.querySelectorAll<HTMLButtonElement>(
        '[role="menuitem"]:not(:disabled), [role="menuitemradio"]:not(:disabled)'
      )
    );
    if (!items.length) return;
    event.preventDefault();
    const currentIndex = items.indexOf(document.activeElement as HTMLButtonElement);
    const nextIndex = event.key === "Home"
      ? 0
      : event.key === "End"
        ? items.length - 1
        : event.key === "ArrowDown"
          ? (currentIndex + 1 + items.length) % items.length
          : (currentIndex - 1 + items.length) % items.length;
    items[nextIndex]?.focus();
  }, [toggleMenu]);

  const available = (previewFrame ? status.hasPage : status.open) && !pending;
  /**
   * Enters or leaves the drawing layer.
   *
   * The capture is dispatched *before* the layer opens, because opening it covers the pane body
   * and the host withdraws the native page from a fully covered rectangle — after which there is
   * nothing composited left to capture.
   */
  const toggleAnnotate = useCallback(() => {
    if (annotating) {
      annotateGenerationRef.current += 1;
      setAnnotating(false);
      setAnnotateBackdrop(null);
      return;
    }
    if (!sessionId) return;
    const generation = ++annotateGenerationRef.current;
    const capture = captureBrowserPage(sessionId).catch(() => null);
    setAnnotating(true);
    setAnnotateBackdrop(null);
    // The drawing layer covers the page, so an armed picker would be waiting on clicks that can
    // no longer reach it.
    if (pickerArmedRef.current) void action("select_element", false);
    void capture.then((page) => {
      if (annotateGenerationRef.current !== generation) return;
      // The page is sunk for most of a pane's life, and the drawing layer covers it besides, so
      // this capture may well come back empty. The projection is a frame of the same page a moment
      // earlier, which is a better backdrop to draw on than none.
      const src = page ? `data:image/png;base64,${page.data}` : snapshotStore.get()?.src ?? null;
      if (src) setAnnotateBackdrop(src);
    });
  }, [action, annotating, sessionId, snapshotStore]);

  const openLocalFile = useCallback(async () => {
    if (!sessionId) return;
    try {
      const next = await openLocalFileInBrowser(sessionId, fileTarget);
      if (!next) return;
      setStatus(next);
      setAddress(next.url === "about:blank" ? "" : next.url);
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    }
  }, [sessionId, fileTarget]);

  const attachAnnotation = useCallback(async (dataUrl: string) => {
    annotateGenerationRef.current += 1;
    setAnnotating(false);
    setAnnotateBackdrop(null);
    if (!onAttachImage) return;
    try {
      await onAttachImage(await annotationImageFile(dataUrl));
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    }
  }, [onAttachImage]);
  const submit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    setEditingAddress(false);
    void navigate(address);
  };
  const parts = splitBrowserAddress(status.url);
  const showBodyCard = bodyState.kind !== "page";

  const bodyCard = (() => {
    switch (bodyState.kind) {
      case "page":
        return null;
      case "no-config":
        return <PreviewIdlePage />;
      case "start-page":
        return (
          <PreviewStartPage
            rows={preview.rows}
            pendingName={preview.pendingName}
            onRun={(name) => void preview.run(name)}
            onStop={(row) => void preview.stop(row)}
            onOpen={(row) => {
              preview.adopt(row);
              openServerPage(row);
            }}
          />
        );
      case "starting":
        return <PreviewStartingCard />;
      case "start-failed":
        return (
          <PreviewStartFailedCard
            logLine={bodyState.message}
            onCopyLog={() => navigator.clipboard?.writeText(bodyState.message)}
            onRetry={() => void preview.run(bodyState.name)}
            retrying={preview.pendingName === bodyState.name}
          />
        );
      case "stopped":
        return (
          <PreviewStoppedCard
            label={bodyState.label}
            onRestart={() => {
              preview.dismissStopped();
              if (preview.stopped) void preview.run(preview.stopped.name);
            }}
            restartPending={preview.pendingName !== null}
          />
        );
    }
  })();

  const chrome = (
    <div className="browser-panel__toolbar">
      <IconButton
        id={menuTriggerId}
        label={t("服务器与设置", "Servers & settings")}
        className="browser-panel__menu-trigger"
        aria-haspopup="menu"
        aria-expanded={openMenu === "overflow"}
        aria-controls={menuId}
        onClick={() => toggleMenu(openMenu === "overflow" ? null : "overflow")}
      ><Globe2 size={14} /></IconButton>
      <div className="browser-panel__address-slot">
        {editingAddress ? (
          <form className="browser-panel__address is-editing" role="search" onSubmit={submit}>
            <label className="sr-only" htmlFor="browser-panel-address">{t("页面网址", "Page URL")}</label>
            <input
              id="browser-panel-address"
              value={address}
              placeholder={t("输入网址", "Type a URL")}
              ref={(node) => node?.focus()}
              onChange={(event) => setAddress(event.target.value)}
              onFocus={(event) => event.currentTarget.select()}
              onBlur={() => setEditingAddress(false)}
              onKeyDown={(event) => {
                if (event.key !== "Escape") return;
                event.preventDefault();
                event.stopPropagation();
                setEditingAddress(false);
                setAddress(status.url === "about:blank" ? "" : status.url);
              }}
              autoComplete="off"
              spellCheck={false}
            />
            {(pending || status.loading) && <LoaderCircle className="spin" size={13} aria-label={t("网页加载中", "Page loading")} />}
          </form>
        ) : (
          <button
            type="button"
            className="browser-panel__address"
            aria-label={parts
              ? t("页面网址：{url}", "Page URL: {url}", { url: status.url })
              : t("输入网址", "Enter a URL")}
            onClick={() => {
              setAddress(status.url === "about:blank" ? "" : status.url);
              setEditingAddress(true);
            }}
          >
            {parts ? (
              <span className="browser-panel__address-text">
                <span className="browser-panel__address-host">{parts.host}</span>
                <span className="browser-panel__address-path">{parts.path}</span>
              </span>
            ) : (
              <span className="browser-panel__address-text is-empty">{t("输入网址", "Enter a URL")}</span>
            )}
            {(pending || status.loading) && <LoaderCircle className="spin" size={13} aria-label={t("网页加载中", "Page loading")} />}
          </button>
        )}
      </div>
      <div className="browser-panel__tools">
        <IconButton label={t("后退", "Go back")} disabled={!available || !status.canGoBack} onClick={() => void action("back")}><ChevronLeft size={16} /></IconButton>
        <IconButton
          className="browser-panel__forward"
          label={t("前进", "Go forward")}
          disabled={!available || !status.canGoForward}
          onClick={() => void action("forward")}
        ><ChevronRight size={16} /></IconButton>
        <IconButton
          className={`browser-panel__annotate${annotating ? " is-pressed" : ""}`}
          label={t("标注", "Annotate")}
          aria-pressed={annotating}
          disabled={!available && !annotating}
          onClick={toggleAnnotate}
        ><Pencil size={14} /></IconButton>
        <IconButton
          className={`browser-panel__select-element${pickerArmed ? " is-pressed" : ""}`}
          label={pickerArmed ? t("退出选择模式", "Exit selection mode") : t("选择元素", "Select element")}
          aria-pressed={pickerArmed}
          disabled={!available && !pickerArmed}
          onClick={() => void action("select_element", !pickerArmed)}
        ><MousePointerClick size={14} /></IconButton>
        {/* The source's reload has no stop state; a hung load still needs a way out of it. */}
        <IconButton label={status.loading ? t("停止加载", "Stop loading") : t("刷新页面", "Reload")} disabled={!available} onClick={() => void action(status.loading ? "stop" : "reload")}>
          {status.loading ? <X size={14} /> : <RotateCw size={14} />}
        </IconButton>
      </div>
    </div>
  );

  return (
    <SidePane
      id={paneId}
      title={t("预览", "Preview")}
      header={tabs ?? chrome}
      subheader={tabs ? chrome : undefined}
      trailing={paneTrailing}
      expanded={paneExpanded}
      onToggleExpand={onPaneToggleExpand}
      onFocus={onPaneFocus}
      onClose={onPaneClose}
      onContentBoundsChange={onContentBoundsChange}
    >
      <div
        ref={panelRef}
        className="browser-panel-content"
        onKeyDown={(event) => {
          if (event.key !== "Escape") return;
          if (openMenu === null && !pickerArmed) return;
          event.preventDefault();
          event.stopPropagation();
          if (pickerArmed) void action("select_element", false);
          if (openMenu !== null) toggleMenu(null, true);
        }}
      >
        {error && <div ref={errorRef} className="browser-panel__error" role="alert">{error}</div>}
        {notice && !error && (
          <div ref={noticeRef} className="browser-panel__notice" role="status">{notice}</div>
        )}
        {annotating && (
          <div ref={sketchRef} className="browser-panel__sketch">
            <SketchOverlay
              backdropDataUrl={annotateBackdrop}
              onCancel={toggleAnnotate}
              onAttach={attachAnnotation}
            />
          </div>
        )}
        {openMenu === "overflow" && (
          <div
            ref={menuRef}
            id={menuId}
            className="browser-panel__menu"
            role="menu"
            aria-label={t("浏览器菜单", "Browser menu")}
            onKeyDown={handleMenuKeyDown}
          >
            <PreviewServerMenuItems
              rows={preview.rows}
              pendingName={preview.pendingName}
              currentUrl={status.url}
              onOpen={(row) => void runMenuAction(async () => {
                preview.adopt(row);
                openServerPage(row);
              })}
              onRun={(name) => void runMenuAction(() => preview.run(name))}
              onStop={(row) => void runMenuAction(() => preview.stop(row))}
              onStopAll={() => void runMenuAction(() => preview.stopAll())}
            />
            {preview.rows.length > 0 && <hr />}
            <PreviewFileMenuItems onOpenFile={() => void runMenuAction(openLocalFile)} />
            <hr />
            <p className="browser-panel__menu-group">{t("设置", "Settings")}</p>
            <PreviewLogsMenuItem
              expanded={logsOpen}
              onToggle={() => void runMenuAction(async () => setLogsOpen((current) => !current))}
            />
            <button
              type="button"
              role="menuitem"
              aria-expanded={clearDataOpen}
              onClick={() => setClearDataOpen((current) => !current)}
            >
              <span>{t("清除浏览数据", "Clear browsing data")}</span>
              <ChevronRight
                className={`browser-panel__submenu-chevron${clearDataOpen ? " is-open" : ""}`}
                size={14}
                aria-hidden="true"
              />
            </button>
            {clearDataOpen && (
              <div className="browser-panel__clear-data" role="group" aria-label={t("清除浏览数据", "Clear browsing data")}>
                <p>{t(
                  "清除当前标签页临时 Profile 的 Cookie、历史记录、缓存和站点存储，相当于不关页面就退出登录。每个标签页的凭据本就互不共享，并会在标签页关闭时自动销毁。",
                  "Clear cookies, history, cache, and site storage for this tab's single-use profile — signing this tab out without closing it. Each tab's credentials are never shared and are destroyed automatically when the tab closes."
                )}</p>
                <button
                  type="button"
                  role="menuitem"
                  disabled={clearingData !== "idle"}
                  onClick={() => void clearBrowsingData()}
                >
                  {clearingData === "running"
                    ? <LoaderCircle className="spin" size={13} aria-hidden="true" />
                    : clearingData === "cleared"
                      ? <Check size={13} aria-hidden="true" />
                      : <Trash2 size={13} aria-hidden="true" />}
                  <span>{clearingData === "cleared"
                    ? t("已清除此标签页的浏览数据", "This tab's data was cleared")
                    : t("清除此标签页的全部浏览数据", "Clear all data for this tab")}</span>
                </button>
              </div>
            )}
          </div>
        )}
        <div className="browser-panel__body" ref={pageAreaRef}>
          <BrowserPageStill store={snapshotStore} contentKey={sessionId ?? null} />
          {showBodyCard && <div className="browser-panel__body-slot" ref={bodyCardRef}>{bodyCard}</div>}
          {previewFrame && !showBodyCard && (
            <iframe
              className="browser-panel__frame"
              title={t("浏览器页面", "Browser page")}
              src={status.url}
              sandbox="allow-forms allow-modals allow-popups allow-popups-to-escape-sandbox allow-scripts"
              referrerPolicy="strict-origin-when-cross-origin"
            />
          )}
        </div>
        {logsOpen && (
          <PreviewLogDrawer
            ref={drawerRef}
            lines={logLines}
            serverName={logServer?.name ?? null}
            onClose={() => setLogsOpen(false)}
          />
        )}
      </div>
    </SidePane>
  );
}

function PreviewBrowserPanel({
  paneId,
  onPaneClose,
  onPaneFocus,
  paneTrailing,
  paneExpanded = false,
  onPaneToggleExpand,
  onContentBoundsChange,
  tabs
}: {
  paneId: SidePaneId;
  onPaneClose: () => void;
  onPaneFocus?: () => void;
  paneTrailing?: ReactNode;
  paneExpanded?: boolean;
  onPaneToggleExpand?: () => void;
  onAttachImage?: (file: File) => void | Promise<void>;
  onElementPicked?: (element: SelectedElement) => void;
  onContentBoundsChange?: (bounds: SidePaneBounds) => void;
  tabs?: ReactNode;
}) {
  const { t } = useI18n();
  const [history, setHistory] = useState(["about:blank"]);
  const [historyIndex, setHistoryIndex] = useState(0);
  const [address, setAddress] = useState("about:blank");
  const [reloadKey, setReloadKey] = useState(0);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const currentUrl = history[historyIndex] ?? "about:blank";

  useEffect(() => setAddress(currentUrl), [currentUrl]);
  useEffect(() => {
    if (currentUrl === "about:blank") setLoading(false);
  }, [currentUrl, reloadKey]);
  const frame = useMemo(() => ({ url: currentUrl, key: `${historyIndex}-${reloadKey}` }), [currentUrl, historyIndex, reloadKey]);
  const navigate = (raw: string) => {
    try {
      const url = normalizeBrowserAddress(raw, {
        empty: t("请输入网址或搜索内容", "Enter a URL or search query"),
        unsupportedProtocol: t("侧栏浏览器只支持 HTTP(S) 地址", "The sidebar browser only supports HTTP(S) addresses")
      });
      setHistory((current) => [...current.slice(0, historyIndex + 1), url]);
      setHistoryIndex((current) => current + 1);
      setAddress(url);
      setError(null);
      setLoading(true);
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    }
  };

  const chrome = (
    <div className="browser-panel__toolbar">
      <div className="browser-panel__address-slot">
        <form className="browser-panel__address is-editing" role="search" onSubmit={(event) => { event.preventDefault(); navigate(address); }}>
          <label className="sr-only" htmlFor="browser-panel-address">{t("页面网址", "Page URL")}</label>
          <input id="browser-panel-address" value={address} placeholder={t("输入网址", "Type a URL")} onChange={(event) => setAddress(event.target.value)} />
          {loading && <LoaderCircle className="spin" size={13} aria-label={t("网页加载中", "Page loading")} />}
        </form>
      </div>
      <div className="browser-panel__tools">
        <IconButton label={t("后退", "Go back")} disabled={historyIndex === 0} onClick={() => { setHistoryIndex((value) => value - 1); setLoading(true); }}><ChevronLeft size={16} /></IconButton>
        <IconButton
          className="browser-panel__forward"
          label={t("前进", "Go forward")}
          disabled={historyIndex >= history.length - 1}
          onClick={() => { setHistoryIndex((value) => value + 1); setLoading(true); }}
        ><ChevronRight size={16} /></IconButton>
        <IconButton label={t("刷新页面", "Reload")} onClick={() => { setReloadKey((value) => value + 1); setLoading(true); }}><RotateCw size={14} /></IconButton>
      </div>
    </div>
  );

  return (
    <SidePane
      id={paneId}
      title={t("预览", "Preview")}
      header={tabs ?? chrome}
      subheader={tabs ? chrome : undefined}
      trailing={paneTrailing}
      expanded={paneExpanded}
      onToggleExpand={onPaneToggleExpand}
      onFocus={onPaneFocus}
      onClose={onPaneClose}
      onContentBoundsChange={onContentBoundsChange}
    >
      <div className="browser-panel-content">
        {error && <div className="browser-panel__error" role="alert">{error}</div>}
        <div className="browser-panel__body">
          {frame.url === "about:blank" ? (
            <PreviewIdlePage />
          ) : (
            <iframe
              key={frame.key}
              className="browser-panel__frame"
              title={t("浏览器页面", "Browser page")}
              src={frame.url}
              sandbox="allow-forms allow-modals allow-popups allow-popups-to-escape-sandbox allow-scripts"
              referrerPolicy="strict-origin-when-cross-origin"
              onLoad={() => setLoading(false)}
            />
          )}
        </div>
      </div>
    </SidePane>
  );
}

export function BrowserPanel({
  native,
  paneId,
  onPaneClose,
  onPaneFocus,
  paneTrailing,
  paneExpanded = false,
  onPaneToggleExpand,
  onAttachImage,
  onElementPicked,
  onContentBoundsChange,
  sessionId,
  active = true,
  target = null,
  fileTarget = null,
  tabs,
  linkNotice = null,
  trustedOverlayRect = null,
  onReservedBottomChange,
  onOpenPage,
  browserDev = isBrowserDevRuntime()
}: BrowserPanelProps) {
  const pane = { paneId, onPaneClose, onPaneFocus, paneTrailing, paneExpanded, onPaneToggleExpand, onAttachImage, onElementPicked, onContentBoundsChange };
  if (native || browserDev) {
    return (
      <BackendBrowserPanel
        key={sessionId ?? "no-session"}
        {...pane}
        sessionId={sessionId}
        previewFrame={browserDev && !native}
        nativeChild={native}
        active={active}
        target={target}
        fileTarget={fileTarget}
        tabs={tabs}
        linkNotice={linkNotice}
        trustedOverlayRect={trustedOverlayRect}
        onReservedBottomChange={onReservedBottomChange}
        onOpenPage={onOpenPage}
      />
    );
  }
  return <PreviewBrowserPanel {...pane} tabs={tabs} />;
}
