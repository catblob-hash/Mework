import {
  ArrowLeft,
  ArrowRight,
  ChevronRight,
  ExternalLink,
  Globe2,
  LoaderCircle,
  Minus,
  MoreVertical,
  Plus,
  RotateCw,
  Trash2,
  X
} from "lucide-react";
import { useCallback, useEffect, useId, useMemo, useRef, useState } from "react";
import type { FormEvent, KeyboardEvent as ReactKeyboardEvent } from "react";
import { getI18nSnapshot, translate, useI18n } from "../i18n";
import {
  getBrowserStatus,
  navigateBrowser,
  performBrowserAction,
  type BrowserAction,
  type BrowserMenuRegion,
  type BrowserStatus
} from "../lib/browser";
import { isBrowserDevRuntime } from "../lib/backend";
import { openExternalUrl } from "../lib/externalLinks";
import { IconButton } from "./Common";

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

function BrowserWelcome({
  language,
  heading,
  description
}: {
  language: string;
  heading: string;
  description: string;
}) {
  return (
    <section className="browser-panel__welcome" role="status" lang={language}>
      <div className="browser-panel__welcome-card">
        <Globe2 className="browser-panel__welcome-mark" size={34} aria-hidden="true" />
        <h2>{heading}</h2>
        <p>{description}</p>
      </div>
    </section>
  );
}

interface BrowserPanelProps {
  native: boolean;
  /**
   * Native browser session this tab drives. A conversation's first tab uses the conversation id;
   * extra tabs use their own session id. Every tab runs in its own single-use Chromium profile:
   * a sign-in made in one tab exists only there and dies when the tab closes.
   */
  sessionId?: string;
  active?: boolean;
  /**
   * Viewport rectangle of a trusted overlay that another component draws over this panel. The
   * native page is a child window above the renderer, so its own window region must be cut for
   * the overlay to be visible at all.
   */
  trustedOverlayRect?: BrowserOverlayRect | null;
  /** Uses the real Rust browser session while mirroring its URL in a sandboxed iframe. */
  browserDev?: boolean;
  /** @deprecated The shared sidebar owns close behavior. */
  onClose?: () => void;
}

type BrowserRect = Pick<DOMRect, "left" | "top" | "right" | "bottom" | "width" | "height">;

/** Trusted overlay geometry another component publishes in viewport CSS pixels. */
export type BrowserOverlayRect = BrowserRect;

/**
 * Height of the trusted React chrome above the native page, in logical pixels. It is the
 * `occludedTop` the native layout reserves inside the panel rectangle.
 */
export const BROWSER_PANEL_CHROME_HEIGHT = 44;

// Keep generations increasing across component unmounts while a conversation-owned native page
// survives. Date.now() provides a fresh floor after a renderer reload; the local increment keeps
// multiple requests in one millisecond strictly ordered and remains below Number.MAX_SAFE_INTEGER.
let latestBrowserMenuGeneration = 0;

function nextBrowserMenuGeneration(): number {
  latestBrowserMenuGeneration = Math.max(
    latestBrowserMenuGeneration + 1,
    Date.now() * 1_024
  );
  return latestBrowserMenuGeneration;
}

function browserMenuVisibility(expanded: boolean): BrowserMenuRegion {
  return {
    generation: nextBrowserMenuGeneration(),
    expanded
  };
}

export function browserMenuRegion(
  panelRect: BrowserRect,
  chromeRect: BrowserRect,
  menuRect: BrowserRect,
  generation: number
): BrowserMenuRegion | null {
  const values = [
    panelRect.left,
    chromeRect.bottom,
    menuRect.left,
    menuRect.top,
    menuRect.width,
    menuRect.height
  ];
  if (values.some((value) => !Number.isFinite(value)) || menuRect.width <= 0 || menuRect.height <= 0) {
    return null;
  }
  return {
    generation,
    expanded: true,
    rect: {
      x: menuRect.left - panelRect.left,
      y: menuRect.top - chromeRect.bottom,
      width: menuRect.width,
      height: menuRect.height
    },
    shadow: 18
  };
}

/** Removes the entire native page child while a trusted React dialog is displayed above it. */
export function browserOverlayRegion(
  panelRect: BrowserRect,
  chromeRect: BrowserRect,
  generation: number
): BrowserMenuRegion | null {
  const width = panelRect.width;
  const height = panelRect.bottom - chromeRect.bottom;
  if (
    !Number.isFinite(width)
    || !Number.isFinite(height)
    || width <= 0
    || height <= 0
  ) {
    return null;
  }
  return {
    generation,
    expanded: true,
    rect: { x: 0, y: 0, width, height },
    shadow: 0
  };
}

function BackendBrowserPanel({
  sessionId,
  previewFrame = false,
  nativeChild = false,
  active = true,
  trustedOverlayRect = null
}: {
  sessionId?: string;
  previewFrame?: boolean;
  nativeChild?: boolean;
  active?: boolean;
  trustedOverlayRect?: BrowserOverlayRect | null;
}) {
  const { resolvedLanguage, t } = useI18n();
  const menuId = useId();
  const menuTriggerId = useId();
  const embeddedTheme = useEmbeddedUiTheme();
  const [status, setStatus] = useState<BrowserStatus>(emptyStatus);
  const [address, setAddress] = useState("");
  const [pending, setPending] = useState(false);
  const [menuOpen, setMenuOpen] = useState(false);
  const [clearDataOpen, setClearDataOpen] = useState(false);
  const [clearingData, setClearingData] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const menuOpenRef = useRef(false);
  const menuGenerationRef = useRef(0);
  /** Whether the native region hole currently installed belongs to an outside trusted overlay. */
  const externalOverlayRegionRef = useRef(false);
  const panelRef = useRef<HTMLDivElement>(null);
  const chromeRef = useRef<HTMLElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!sessionId) return;
    let cancelled = false;
    let timer = 0;
    const poll = async () => {
      try {
        const next = await getBrowserStatus(sessionId);
        if (cancelled) return;
        setStatus(next);
        if (document.activeElement?.id !== "browser-panel-address") {
          setAddress(next.url === "about:blank" ? "" : next.url);
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
      const hadTrustedOverlay = menuOpenRef.current || externalOverlayRegionRef.current;
      menuOpenRef.current = false;
      externalOverlayRegionRef.current = false;
      if (nativeChild && hadTrustedOverlay) {
        const request = browserMenuVisibility(false);
        menuGenerationRef.current = request.generation;
        void performBrowserAction(sessionId, "menu", request).catch(() => undefined);
      }
    };
  }, [sessionId, nativeChild]);

  useEffect(() => {
    if (active) return;
    if (!menuOpenRef.current && !externalOverlayRegionRef.current) return;
    menuOpenRef.current = false;
    externalOverlayRegionRef.current = false;
    setMenuOpen(false);
    setClearDataOpen(false);
    if (nativeChild && sessionId) {
      const request = browserMenuVisibility(false);
      menuGenerationRef.current = request.generation;
      void performBrowserAction(sessionId, "menu", request).catch(() => undefined);
    }
  }, [active, sessionId, nativeChild]);

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

  const navigate = useCallback(async (raw: string) => {
    if (!sessionId) return;
    try {
      const url = normalizeBrowserAddress(raw, {
        empty: t("请输入网址或搜索内容", "Enter a URL or search query"),
        unsupportedProtocol: t("侧栏浏览器只支持 HTTP(S) 地址", "The sidebar browser only supports HTTP(S) addresses")
      });
      await run(() => navigateBrowser(sessionId, url));
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    }
  }, [sessionId, run, t]);


  const toggleMenu = useCallback(async (next: boolean, restoreFocus = !next): Promise<boolean> => {
    if (!sessionId) return false;
    const request = browserMenuVisibility(next);
    menuGenerationRef.current = request.generation;
    menuOpenRef.current = next;
    if (!next) {
      setMenuOpen(false);
      setClearDataOpen(false);
      if (restoreFocus) {
        document.getElementById(menuTriggerId)?.focus();
      }
    }
    try {
      if (nativeChild) {
        setError(null);
        const nextStatus = await performBrowserAction(sessionId, "menu", request);
        if (menuGenerationRef.current !== request.generation) return false;
        setStatus(nextStatus);
        if (document.activeElement?.id !== "browser-panel-address") {
          setAddress(nextStatus.url === "about:blank" ? "" : nextStatus.url);
        }
      }
      if (next) setMenuOpen(true);
      return true;
    } catch (reason) {
      if (menuGenerationRef.current !== request.generation) return false;
      // A WebView UI closure can finish after native IPC reports a timeout. Publish a newer close
      // before hiding trusted chrome so that any delayed hole request is stale in Rust as well.
      menuOpenRef.current = false;
      setMenuOpen(false);
      if (nativeChild) {
        const closeRequest = browserMenuVisibility(false);
        menuGenerationRef.current = closeRequest.generation;
        void performBrowserAction(sessionId, "menu", closeRequest).catch(() => undefined);
      }
      setError(reason instanceof Error ? reason.message : String(reason));
      return false;
    }
  }, [sessionId, menuTriggerId, nativeChild]);

  useEffect(() => {
    if (!menuOpen) return;
    menuRef.current
      ?.querySelector<HTMLButtonElement>('[role="menuitem"]:not(:disabled)')
      ?.focus();
  }, [menuOpen]);

  const syncNativeMenuRegion = useCallback(() => {
    if (!nativeChild || !sessionId || !menuOpenRef.current) return;
    const panel = panelRef.current;
    const chrome = chromeRef.current;
    const menu = menuRef.current;
    if (!panel || !chrome || !menu) return;
    const panelRect = panel.getBoundingClientRect();
    const chromeRect = chrome.getBoundingClientRect();
    const menuRect = menu.getBoundingClientRect();
    const generation = nextBrowserMenuGeneration();
    const region = browserMenuRegion(panelRect, chromeRect, menuRect, generation);
    if (!region) return;
    menuGenerationRef.current = generation;
    void performBrowserAction(sessionId, "menu", region)
      .then((nextStatus) => {
        if (menuGenerationRef.current === generation) setStatus(nextStatus);
      })
      .catch((reason) => {
        if (menuGenerationRef.current !== generation) return;
        menuOpenRef.current = false;
        setMenuOpen(false);
        setClearDataOpen(false);
        const closeRequest = browserMenuVisibility(false);
        menuGenerationRef.current = closeRequest.generation;
        void performBrowserAction(sessionId, "menu", closeRequest).catch(() => undefined);
        setError(reason instanceof Error ? reason.message : String(reason));
        document.getElementById(menuTriggerId)?.focus();
      });
  }, [sessionId, menuTriggerId, nativeChild]);

  useEffect(() => {
    if (!nativeChild || !sessionId) return;
    if (trustedOverlayRect) {
      // A trusted overlay drawn by the sidebar replaces this panel's own menu hole: only one
      // native region can be installed at a time, and the outer overlay is the newer intent.
      if (menuOpenRef.current) {
        menuOpenRef.current = false;
        setMenuOpen(false);
        setClearDataOpen(false);
      }
      const panel = panelRef.current;
      const chrome = chromeRef.current;
      if (!panel || !chrome) return;
      const generation = nextBrowserMenuGeneration();
      const region = browserMenuRegion(
        panel.getBoundingClientRect(),
        chrome.getBoundingClientRect(),
        trustedOverlayRect,
        generation
      );
      if (!region) return;
      menuGenerationRef.current = generation;
      externalOverlayRegionRef.current = true;
      void performBrowserAction(sessionId, "menu", region)
        .then((nextStatus) => {
          if (menuGenerationRef.current === generation) setStatus(nextStatus);
        })
        .catch((reason) => {
          if (menuGenerationRef.current !== generation) return;
          setError(reason instanceof Error ? reason.message : String(reason));
        });
      return;
    }
    if (!externalOverlayRegionRef.current) return;
    externalOverlayRegionRef.current = false;
    if (menuOpenRef.current) return;
    const request = browserMenuVisibility(false);
    menuGenerationRef.current = request.generation;
    void performBrowserAction(sessionId, "menu", request)
      .then((nextStatus) => {
        if (menuGenerationRef.current === request.generation) setStatus(nextStatus);
      })
      .catch(() => undefined);
  }, [nativeChild, sessionId, trustedOverlayRect]);



  useEffect(() => {
    if (!menuOpen || !nativeChild) return;
    const frame = window.requestAnimationFrame(syncNativeMenuRegion);
    const observed = menuRef.current;
    const observer = typeof ResizeObserver === "undefined" || !observed
      ? null
      : new ResizeObserver(syncNativeMenuRegion);
    if (observer && observed) observer.observe(observed);
    window.addEventListener("resize", syncNativeMenuRegion);
    return () => {
      window.cancelAnimationFrame(frame);
      observer?.disconnect();
      window.removeEventListener("resize", syncNativeMenuRegion);
    };
  }, [clearDataOpen, menuOpen, nativeChild, syncNativeMenuRegion]);


  useEffect(() => {
    if (!menuOpen) return;
    const onPointerDown = (event: MouseEvent) => {
      const target = event.target as Node;
      if (menuRef.current?.contains(target)) return;
      if (document.getElementById(menuTriggerId)?.contains(target)) return;
      void toggleMenu(false, false);
    };
    document.addEventListener("mousedown", onPointerDown);
    return () => document.removeEventListener("mousedown", onPointerDown);
  }, [menuOpen, menuTriggerId, toggleMenu]);

  const runMenuAction = useCallback(async (task: () => Promise<unknown>) => {
    if (!await toggleMenu(false, true)) return;
    await task();
  }, [toggleMenu]);

  const handleMenuKeyDown = useCallback((event: ReactKeyboardEvent<HTMLDivElement>) => {
    if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      void toggleMenu(false, true);
      return;
    }
    if (!["ArrowDown", "ArrowUp", "Home", "End"].includes(event.key)) return;
    const items = Array.from(
      menuRef.current?.querySelectorAll<HTMLButtonElement>('[role="menuitem"]:not(:disabled)') ?? []
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
  const submit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    void navigate(address);
  };

  return (
    <div
      ref={panelRef}
      className="browser-panel-content"
      onKeyDown={(event) => {
        if (event.key !== "Escape" || !menuOpen) return;
        event.preventDefault();
        event.stopPropagation();
        void toggleMenu(false, true);
      }}
    >
      <header ref={chromeRef} className="browser-panel__chrome">
        <div className="browser-panel__command-bar">
          <div className="browser-panel__navigation" aria-label={t("页面导航", "Page navigation")}>
            <IconButton label={t("后退", "Back")} disabled={!available || !status.canGoBack} onClick={() => void action("back")}><ArrowLeft size={16} /></IconButton>
            <IconButton label={t("前进", "Forward")} disabled={!available || !status.canGoForward} onClick={() => void action("forward")}><ArrowRight size={16} /></IconButton>
            <IconButton label={status.loading ? t("停止加载", "Stop loading") : t("刷新页面", "Reload page")} disabled={!available} onClick={() => void action(status.loading ? "stop" : "reload")}>
              {status.loading ? <X size={14} /> : <RotateCw size={14} />}
            </IconButton>
          </div>
          <form className="browser-panel__address" role="search" onSubmit={submit}>
            <Globe2 size={13} aria-hidden="true" />
            <label className="sr-only" htmlFor="browser-panel-address">{t("网址或搜索内容", "URL or search query")}</label>
            <input
              id="browser-panel-address"
              value={address}
              placeholder={t("输入 URL", "Enter URL")}
              onChange={(event) => setAddress(event.target.value)}
              onFocus={(event) => event.currentTarget.select()}
              autoComplete="off"
              spellCheck={false}
            />
            {(pending || status.loading) && <LoaderCircle className="spin" size={13} aria-label={t("网页加载中", "Page loading")} />}
          </form>
          <IconButton
            id={menuTriggerId}
            label={t("自定义及控制浏览器", "Customize and control browser")}
            className="browser-panel__menu-trigger"
            aria-haspopup="menu"
            aria-expanded={menuOpen}
            aria-controls={menuId}
            disabled={!available}
            onClick={() => void toggleMenu(!menuOpen)}
          ><MoreVertical size={16} /></IconButton>
        </div>
      </header>

      {error && <div className="browser-panel__error" role="alert">{error}</div>}
      {menuOpen && (
        <div
          ref={menuRef}
          id={menuId}
          className="browser-panel__menu"
          role="menu"
          aria-label={t("浏览器菜单", "Browser menu")}
          onKeyDown={handleMenuKeyDown}
        >
          <button type="button" role="menuitem" onClick={() => void runMenuAction(async () => {
            const query = window.prompt(t("在页面中查找：", "Find on page:"))?.trim();
            if (query) await action("find", query);
          })}><span>{t("在页面中查找", "Find on page")}</span><kbd>Ctrl+F</kbd></button>
          <button type="button" role="menuitem" onClick={() => void runMenuAction(
            async () => { await action("print"); }
          )}><span>{t("打印", "Print")}</span><kbd>Ctrl+P</kbd></button>
          <hr />
          <div className="browser-panel__zoom" role="group" aria-label={t("页面缩放", "Page zoom")}>
            <strong>{t("缩放", "Zoom")}</strong>
            <span>
              <IconButton label={t("缩小页面", "Zoom out")} onClick={() => void action("zoom_out")}><Minus size={14} /></IconButton>
              <button type="button" onClick={() => void action("zoom_reset")}>{Math.round((status.zoom || 1) * 100)}%</button>
              <IconButton label={t("放大页面", "Zoom in")} onClick={() => void action("zoom_in")}><Plus size={14} /></IconButton>
            </span>
          </div>
          <hr />
          <button type="button" role="menuitem" onClick={() => void runMenuAction(
            async () => { await action("devtools"); }
          )}><span>{t("显示设备工具栏", "Show device toolbar")}</span><kbd>F12</kbd></button>
          <button type="button" role="menuitem" onClick={() => void runMenuAction(
            async () => { await action("screenshot"); }
          )}><span>{t("截取屏幕截图", "Capture screenshot")}</span></button>
          <hr />
          <button type="button" role="menuitem" disabled><span>{t("下载", "Downloads")}</span></button>
          <button type="button" role="menuitem" onClick={() => void runMenuAction(
            async () => { await action("suspend"); }
          )}><span>{t("挂起此任务页面", "Suspend this task page")}</span></button>
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
                disabled={clearingData}
                onClick={() => void runMenuAction(async () => {
                  setClearingData(true);
                  try {
                    await action("clear_data");
                  } finally {
                    setClearingData(false);
                  }
                })}
              >
                {clearingData
                  ? <LoaderCircle className="spin" size={13} aria-hidden="true" />
                  : <Trash2 size={13} aria-hidden="true" />}
                <span>{t("清除此标签页的全部浏览数据", "Clear all data for this tab")}</span>
              </button>
            </div>
          )}
          <hr />
          <button type="button" role="menuitem" disabled><span>{t("浏览器设置", "Browser settings")}</span></button>
        </div>
      )}
      {previewFrame && (status.url === "about:blank" ? (
        <BrowserWelcome
          language={resolvedLanguage}
          heading={t("开始浏览", "Start browsing")}
          description={t("输入 URL 以打开页面", "Enter a URL to open a page")}
        />
      ) : (
        <iframe
          className="browser-panel__frame"
          title={t("浏览器页面", "Browser page")}
          src={status.url}
          sandbox="allow-forms allow-modals allow-popups allow-popups-to-escape-sandbox allow-scripts"
          referrerPolicy="strict-origin-when-cross-origin"
        />
      ))}
    </div>
  );
}

function PreviewBrowserPanel() {
  const { resolvedLanguage, t } = useI18n();
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

  return (
    <div className="browser-panel-content">
      <header className="browser-panel__preview-toolbar">
        <IconButton label={t("后退", "Back")} disabled={historyIndex === 0} onClick={() => { setHistoryIndex((value) => value - 1); setLoading(true); }}><ArrowLeft size={15} /></IconButton>
        <IconButton label={t("前进", "Forward")} disabled={historyIndex >= history.length - 1} onClick={() => { setHistoryIndex((value) => value + 1); setLoading(true); }}><ArrowRight size={15} /></IconButton>
        <IconButton label={t("刷新页面", "Reload page")} onClick={() => { setReloadKey((value) => value + 1); setLoading(true); }}><RotateCw size={14} /></IconButton>
        <form className="browser-panel__address" role="search" onSubmit={(event) => { event.preventDefault(); navigate(address); }}>
          <Globe2 size={13} /><label className="sr-only" htmlFor="browser-panel-address">{t("网址或搜索内容", "URL or search query")}</label>
          <input id="browser-panel-address" value={address} onChange={(event) => setAddress(event.target.value)} />
          {loading && <LoaderCircle className="spin" size={13} aria-label={t("网页加载中", "Page loading")} />}
        </form>
        <IconButton label={t("在系统浏览器打开", "Open in system browser")} disabled={currentUrl === "about:blank"} onClick={() => { void openExternalUrl(currentUrl).catch((reason: unknown) => setError(reason instanceof Error ? reason.message : String(reason))); }}><ExternalLink size={14} /></IconButton>
      </header>
      {error && <div className="browser-panel__error" role="alert">{error}</div>}
      {frame.url === "about:blank" ? (
        <BrowserWelcome
          language={resolvedLanguage}
          heading={t("开始浏览", "Start browsing")}
          description={t("输入 URL 以打开页面", "Enter a URL to open a page")}
        />
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
  );
}

export function BrowserPanel({
  native,
  sessionId,
  active = true,
  trustedOverlayRect = null,
  browserDev = isBrowserDevRuntime()
}: BrowserPanelProps) {
  if (native || browserDev) {
    return (
      <BackendBrowserPanel
        key={sessionId ?? "no-session"}
        sessionId={sessionId}
        previewFrame={browserDev && !native}
        nativeChild={native}
        active={active}
        trustedOverlayRect={trustedOverlayRect}
      />
    );
  }
  return <PreviewBrowserPanel />;
}
