import { ArrowLeft, ArrowRight, Copy, Minus, PanelLeft, Search, Square, X } from "lucide-react";
import { useEffect, useState } from "react";
import type { ReactNode } from "react";
import type { Window as TauriWindow } from "@tauri-apps/api/window";
import { useI18n } from "../i18n";
import type { WindowChromeKind } from "../lib/windowChrome";
import { AppBackdrop } from "./AppBackdrop";
import { IconButton } from "./Common";

/**
 * The window's own top row: what the system title bar used to hold, plus the controls that
 * belong to the whole window rather than to one pane. See `lib/windowChrome.ts` for which
 * platform gets which frame.
 */

interface WindowFrameProps {
  chrome: WindowChromeKind;
  /** What the Windows title bar carries at its left end. */
  nav?: ReactNode;
  /**
   * The page draws no top row of its own (loading, the fatal error), so on macOS a strip
   * along the top stands in for one and keeps the window movable.
   */
  bare?: boolean;
  children: ReactNode;
}

/** The window around the whole renderer: on Windows, a title bar strip above everything. */
export function WindowFrame({ chrome, nav, bare = false, children }: WindowFrameProps) {
  return (
    <div className="window-frame">
      <AppBackdrop />
      {chrome === "windows" && (
        <div className="window-titlebar" data-tauri-drag-region="deep">
          {nav}
          <div className="window-titlebar__fill" />
          <WindowCaptionControls />
        </div>
      )}
      {chrome === "mac" && bare && <div className="window-drag-strip" data-tauri-drag-region />}
      {children}
    </div>
  );
}

interface ShellNavProps {
  sidebarOpen: boolean;
  onToggleSidebar: () => void;
  canGoBack: boolean;
  canGoForward: boolean;
  onBack: () => void;
  onForward: () => void;
  onSearch: () => void;
}

/**
 * The sidebar drawer, back and forward through the conversations opened before, and the
 * conversation search. One cluster whichever way the sidebar is: it stays put while the
 * sidebar slides under it, so the drawer button never moves out from under the pointer that
 * just pressed it.
 */
export function ShellNav({
  sidebarOpen,
  onToggleSidebar,
  canGoBack,
  canGoForward,
  onBack,
  onForward,
  onSearch
}: ShellNavProps) {
  const { t } = useI18n();
  return (
    // The gaps between the buttons belong to the title bar, so they move the window.
    <div className="shell-nav" data-tauri-drag-region="deep">
      <IconButton
        label={sidebarOpen ? t("收起侧栏", "Collapse sidebar") : t("打开侧栏", "Open sidebar")}
        aria-expanded={sidebarOpen}
        onClick={onToggleSidebar}
      >
        <PanelLeft size={17} />
      </IconButton>
      <IconButton label={t("后退到上一个对话", "Back to the previous conversation")} disabled={!canGoBack} onClick={onBack}>
        <ArrowLeft size={16} />
      </IconButton>
      <IconButton label={t("前进到下一个对话", "Forward to the next conversation")} disabled={!canGoForward} onClick={onForward}>
        <ArrowRight size={16} />
      </IconButton>
      <IconButton className="shell-nav__search" label={t("搜索对话", "Search conversations")} onClick={onSearch}>
        <Search size={16} />
      </IconButton>
    </div>
  );
}

function withCurrentWindow(action: (window: TauriWindow) => Promise<unknown>): void {
  void import("@tauri-apps/api/window")
    .then(({ getCurrentWindow }) => action(getCurrentWindow()))
    .catch(() => undefined);
}

/**
 * Minimize, maximize and close for the frameless Windows window, drawn the way Windows 11
 * draws its own: flat, full-height, and a red close. Close asks the window to close, which
 * the host turns into hiding it to the tray exactly as the system button did.
 */
export function WindowCaptionControls() {
  const { t } = useI18n();
  const [maximized, setMaximized] = useState(false);

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | null = null;
    void (async () => {
      const { getCurrentWindow } = await import("@tauri-apps/api/window");
      const window = getCurrentWindow();
      const sync = async () => {
        const value = await window.isMaximized();
        if (!disposed) setMaximized(value);
      };
      await sync();
      // Maximizing is also a snap, a double-click on the title bar or a keyboard shortcut,
      // so the icon follows the window's size rather than this button's clicks.
      const stop = await window.onResized(() => { void sync(); });
      if (disposed) stop();
      else unlisten = stop;
    })().catch(() => undefined);
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, []);

  const maximizeLabel = maximized ? t("向下还原", "Restore") : t("最大化", "Maximize");
  return (
    <div className="window-caption">
      <button
        type="button"
        className="window-caption__button"
        aria-label={t("最小化", "Minimize")}
        title={t("最小化", "Minimize")}
        onClick={() => withCurrentWindow((window) => window.minimize())}
      >
        <Minus size={16} strokeWidth={1.25} />
      </button>
      <button
        type="button"
        className="window-caption__button"
        aria-label={maximizeLabel}
        title={maximizeLabel}
        onClick={() => withCurrentWindow((window) => window.toggleMaximize())}
      >
        {maximized
          ? <Copy size={13} strokeWidth={1.25} className="window-caption__restore" />
          : <Square size={12} strokeWidth={1.25} />}
      </button>
      <button
        type="button"
        className="window-caption__button window-caption__button--close"
        aria-label={t("关闭", "Close")}
        title={t("关闭", "Close")}
        onClick={() => withCurrentWindow((window) => window.close())}
      >
        <X size={16} strokeWidth={1.25} />
      </button>
    </div>
  );
}

/**
 * Whether the window is in macOS full screen, where the traffic lights leave the top row
 * and the room kept for them would be an empty gap.
 */
export function useWindowFullscreen(enabled: boolean): boolean {
  const [fullscreen, setFullscreen] = useState(false);
  useEffect(() => {
    if (!enabled) {
      setFullscreen(false);
      return;
    }
    let disposed = false;
    let unlisten: (() => void) | null = null;
    void (async () => {
      const { getCurrentWindow } = await import("@tauri-apps/api/window");
      const window = getCurrentWindow();
      const sync = async () => {
        const value = await window.isFullscreen();
        if (!disposed) setFullscreen(value);
      };
      await sync();
      const stop = await window.onResized(() => { void sync(); });
      if (disposed) stop();
      else unlisten = stop;
    })().catch(() => undefined);
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [enabled]);
  return fullscreen;
}
