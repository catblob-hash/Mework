import {
  AlertTriangle,
  Ban,
  Check,
  Copy,
  LoaderCircle,
  Server,
  X
} from "lucide-react";
import { forwardRef, useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { PointerEvent as ReactPointerEvent, ReactNode } from "react";
import { useI18n } from "../i18n";
import {
  isPreviewAttachment,
  listPreviewConfigurations,
  listPreviewServers,
  previewLogLines,
  previewLogSeverity,
  previewServerAddress,
  previewUrlIsServedAt,
  readPreviewServerLogs,
  setPreviewAutoVerify,
  startPreviewServer,
  stopPreviewServer,
  PREVIEW_LOG_POLL_INTERVAL_MS,
  PREVIEW_MAX_LOG_LINES,
  PREVIEW_SERVER_POLL_INTERVAL_MS,
  previewTargetKey,
  type PreviewConfigurationList,
  type PreviewConfiguredServer,
  type PreviewServerSnapshot,
  type PreviewTarget
} from "../lib/preview";
import { IconButton } from "./Common";

/** `Cf` — the start page lists this many servers before "See all". */
export const PREVIEW_START_PAGE_LIMIT = 5;

/** `zd` — how long the copy confirmation replaces the copy affordance. */
export const PREVIEW_COPY_FLASH_MS = 1500;

export type PreviewViewportPreset = "responsive" | "mobile" | "tablet";

export interface PreviewViewportSize {
  width: number;
  height: number;
}

/**
 * The sizes `preview_resize` emulates. There is deliberately no desktop preset: "Responsive" is
 * the reset, and its size is the pane itself.
 */
export const PREVIEW_VIEWPORT_PRESETS: {
  preset: PreviewViewportPreset;
  size: PreviewViewportSize | null;
}[] = [
  { preset: "responsive", size: null },
  { preset: "mobile", size: { width: 375, height: 812 } },
  { preset: "tablet", size: { width: 768, height: 1024 } }
];

/** One row of the server picker: a configuration, joined to the process answering it if any. */
export interface PreviewServerRow {
  name: string;
  port: number;
  url?: string | null;
  /** Absent for a configuration nothing is running for. */
  server: PreviewServerSnapshot | null;
  running: boolean;
  starting: boolean;
  /**
   * The attach form: a `url` with no command. There is no process to run or stop — the row is
   * openable as it stands, because the server it points at is somebody else's.
   */
  attach: boolean;
}

/**
 * Joins `.mework/launch.json` to the running processes, configurations first and in file order,
 * then any running server the file no longer describes.
 */
export function previewServerRows(
  configurations: PreviewConfiguredServer[],
  servers: PreviewServerSnapshot[]
): PreviewServerRow[] {
  const claimed = new Set<string>();
  const live = (name: string) => servers.find(
    (server) => server.name === name
      && !claimed.has(server.serverId)
      && (server.status === "running" || server.status === "starting")
  );
  const rows = configurations.map((configuration) => {
    const server = live(configuration.name) ?? null;
    if (server) claimed.add(server.serverId);
    return {
      name: configuration.name,
      port: server?.port ?? configuration.port,
      // A server on another machine is reached through the port it was forwarded to, which only
      // the running process knows; a configured url names that machine's own localhost.
      url: server?.url ?? configuration.url ?? null,
      server,
      running: server?.status === "running",
      starting: server?.status === "starting",
      attach: !configuration.command && Boolean(configuration.url)
    };
  });
  const orphans = servers
    .filter((server) => !claimed.has(server.serverId))
    .filter((server) => server.status === "running" || server.status === "starting")
    .map((server) => ({
      name: server.name,
      port: server.port,
      url: server.url ?? null,
      server,
      running: server.status === "running",
      starting: server.status === "starting",
      attach: false
    }));
  return [...rows, ...orphans];
}

/**
 * What a row shows beside its name. A process is addressed by port; an attach row is addressed by
 * the url it points at, and a non-localhost one has no port at all to print.
 */
export function previewRowDetail(row: PreviewServerRow): string {
  if (!row.attach) {
    // A server on another machine keeps its own port in the label — that is the port its logs and
    // its configuration talk about — and says where it runs.
    return row.server?.machine ? `${row.server.machine} :${row.port}` : `:${row.port}`;
  }
  const address = previewServerAddress({ port: row.port, url: row.url });
  try {
    return new URL(address).host;
  } catch {
    return address;
  }
}

/**
 * Whether the page on screen came from this row's server. The reference marks the bound server
 * with a radio check; Mework has no binding to read, so the committed origin stands in for it.
 */
export function previewRowIsOpen(row: PreviewServerRow, url: string | null): boolean {
  return previewUrlIsServedAt(url, previewServerAddress({ port: row.port, url: row.url }));
}

export type PreviewBodyState =
  | { kind: "page" }
  | { kind: "no-config" }
  | { kind: "start-page" }
  | { kind: "starting" }
  | { kind: "start-failed"; name: string; message: string }
  | { kind: "stopped"; label: string };

/**
 * Which of the source's body states the pane is in.
 *
 * A live page wins over the start page but not over a failure or a start in flight: those two are
 * about the action the user just took, and hiding them behind whatever was already loaded would
 * lose the only report of it.
 *
 * The stopped card needs a page under it. Stopping a dev server now takes down the page it was
 * serving, so the common ending is no page at all — and what belongs there is the start page the
 * server can be run from again, not a card saying the thing you just closed is closed.
 */
export function previewBodyState(input: {
  url: string;
  configurationCount: number;
  rows: PreviewServerRow[];
  pendingName: string | null;
  startError: { name: string; message: string } | null;
  stopped: { label: string } | null;
}): PreviewBodyState {
  if (input.startError) {
    return { kind: "start-failed", name: input.startError.name, message: input.startError.message };
  }
  if (input.pendingName !== null || input.rows.some((row) => row.starting)) {
    return { kind: "starting" };
  }
  if (input.url && input.url !== "about:blank") {
    // A page that outlived its server was showing something else — a docs site typed into the
    // address bar — so the card is the only thing that says the server behind it has gone.
    return input.stopped ? { kind: "stopped", label: input.stopped.label } : { kind: "page" };
  }
  if (input.configurationCount === 0) return { kind: "no-config" };
  return { kind: "start-page" };
}

function PreviewBodyCard({
  className = "",
  contentClassName = "",
  children
}: {
  className?: string;
  contentClassName?: string;
  children: ReactNode;
}) {
  return (
    <div className={`browser-panel__body-card ${className}`} role="status">
      <div className={`browser-panel__body-card-inner ${contentClassName}`}>{children}</div>
    </div>
  );
}

/**
 * Standby: nothing to preview and nothing configured to preview it with.
 *
 * Drawn by the main window's React over a fully withdrawn native page region, so this is the only
 * standby surface there is — the embedded page's own `about:blank` document is a bare themed
 * backdrop. One centred line, no heading and no instructions: the toolbar above already says what
 * the address field is for.
 */
export function PreviewIdlePage() {
  const { resolvedLanguage, t } = useI18n();
  return (
    <div className="browser-panel__welcome" role="status" lang={resolvedLanguage}>
      <p className="browser-panel__welcome-line">{t("没有开发服务器", "No dev server")}</p>
    </div>
  );
}

/** `Tf` — configurations exist, none of them is answering yet. */
export function PreviewStartPage({
  rows,
  pendingName,
  onRun,
  onStop,
  onOpen
}: {
  rows: PreviewServerRow[];
  pendingName: string | null;
  onRun: (name: string) => void;
  onStop: (row: PreviewServerRow) => void;
  onOpen: (row: PreviewServerRow) => void;
}) {
  const { t } = useI18n();
  const [expanded, setExpanded] = useState(false);
  const visible = expanded ? rows : rows.slice(0, PREVIEW_START_PAGE_LIMIT);
  return (
    <div className="browser-panel__body-card browser-panel__start-page" role="status">
      <div className="browser-panel__body-card-inner browser-panel__start-page-inner">
        <ul className="browser-panel__server-list">
          {visible.map((row) => {
            const stoppable = row.running || row.starting;
            const busy = row.starting || row.name === pendingName;
            return (
              <li key={`${row.name}:${row.server?.serverId ?? "config"}`} aria-busy={busy || undefined}>
                <button
                  type="button"
                  className="browser-panel__server-open"
                  disabled={!row.running && !row.attach}
                  aria-label={t("打开 {name}", "Open {name}", { name: row.name })}
                  onClick={() => onOpen(row)}
                >
                  <Server size={14} aria-hidden="true" />
                  <span className="browser-panel__server-name">{row.name}</span>
                  <span className="browser-panel__server-detail">{previewRowDetail(row)}</span>
                </button>
                {/* No action for an attach row: there is nothing to run, and nothing to stop. */}
                {!row.attach && (
                  <button
                    type="button"
                    className={`browser-panel__server-action${stoppable ? " is-stop" : ""}`}
                    disabled={pendingName !== null && pendingName !== row.name}
                    aria-label={stoppable
                      ? t("停止 {name}", "Stop {name}", { name: row.name })
                      : t("运行 {name}", "Run {name}", { name: row.name })}
                    onClick={() => (stoppable ? onStop(row) : onRun(row.name))}
                  >
                    {busy
                      ? <LoaderCircle className="spin" size={12} aria-hidden="true" />
                      : stoppable
                        ? t("停止", "Stop")
                        : t("运行", "Run")}
                  </button>
                )}
              </li>
            );
          })}
        </ul>
        {visible.length < rows.length && (
          <button type="button" className="browser-panel__see-all" onClick={() => setExpanded(true)}>
            {t("查看全部", "See all")}
          </button>
        )}
      </div>
    </div>
  );
}

/** The centred spinner the source shows while a start is in flight. */
export function PreviewStartingCard() {
  const { t } = useI18n();
  return (
    <PreviewBodyCard className="browser-panel__starting">
      <LoaderCircle className="spin" size={22} aria-hidden="true" />
      <p className="browser-panel__body-card-title">{t("正在启动服务器", "Starting server")}</p>
    </PreviewBodyCard>
  );
}

/** `Bd` — a start that never produced a running server. */
export function PreviewStartFailedCard({
  logLine,
  onCopyLog,
  onRetry,
  retrying = false
}: {
  logLine: string;
  onCopyLog: () => Promise<void> | void;
  onRetry?: () => void;
  retrying?: boolean;
}) {
  const { t } = useI18n();
  const [copied, setCopied] = useState(false);
  const timer = useRef<number | undefined>(undefined);
  useEffect(() => () => window.clearTimeout(timer.current), []);
  const copy = useCallback(async () => {
    try {
      await onCopyLog();
    } catch {
      setCopied(false);
      return;
    }
    window.clearTimeout(timer.current);
    setCopied(true);
    timer.current = window.setTimeout(() => setCopied(false), PREVIEW_COPY_FLASH_MS);
  }, [onCopyLog]);
  const meta = t("错误详情已发送给模型", "Error details were sent to Claude");
  return (
    <PreviewBodyCard className="browser-panel__start-failed">
      <AlertTriangle size={30} aria-hidden="true" />
      <p className="browser-panel__body-card-title">{t("开发服务器启动失败", "Dev server failed to start")}</p>
      {logLine ? (
        <div className="browser-panel__start-failed-log">
          <span>
            <em>{`${meta} · `}</em>
            {logLine}
          </span>
          <IconButton
            label={copied ? t("已复制", "Copied") : t("复制错误日志", "Copy error log")}
            onClick={() => void copy()}
          >
            {copied ? <Check size={13} /> : <Copy size={13} />}
          </IconButton>
        </div>
      ) : (
        <p className="browser-panel__body-card-footnote">{meta}</p>
      )}
      {onRetry && (
        <button type="button" className="browser-panel__card-action" disabled={retrying} onClick={onRetry}>
          {retrying && <LoaderCircle className="spin" size={12} aria-hidden="true" />}
          {t("重试", "Try again")}
        </button>
      )}
    </PreviewBodyCard>
  );
}

/** `yf` — the server that was serving this page is gone. */
export function PreviewStoppedCard({
  label,
  onRestart,
  restartPending = false
}: {
  label: string;
  onRestart: () => void;
  restartPending?: boolean;
}) {
  const { t } = useI18n();
  const restart = t("重启", "Restart");
  return (
    <PreviewBodyCard className="browser-panel__stopped" contentClassName="browser-panel__stopped-inner">
      <div className="browser-panel__stopped-row">
        <Server size={14} aria-hidden="true" />
        <span className="browser-panel__server-name">{label}</span>
        <span className="browser-panel__server-detail">{t("已停止", "Stopped")}</span>
        <button
          type="button"
          className="browser-panel__server-action"
          disabled={restartPending}
          aria-label={`${restart} ${label}`}
          onClick={onRestart}
        >
          {restartPending ? <LoaderCircle className="spin" size={12} aria-hidden="true" /> : restart}
        </button>
      </div>
      <p className="browser-panel__body-card-footnote">
        {t(
          "开发服务器已停止。重启它，或者关闭预览。",
          "The dev server stopped. Restart it, or close the preview."
        )}
      </p>
    </PreviewBodyCard>
  );
}

/**
 * `Yf` — the dev-server output, docked below the page rather than over it.
 *
 * The grip drags with pointer capture from the drawer's own height, so a pointer that leaves the
 * strip mid-drag keeps resizing instead of dropping the gesture on the page underneath.
 */
export const PreviewLogDrawer = forwardRef<HTMLDivElement, {
  lines: string[];
  /** The server the lines are coming from, or null when there is none to follow. */
  serverName?: string | null;
  onClose: () => void;
}>(function PreviewLogDrawer({ lines, serverName = null, onClose }, ref) {
  const { t } = useI18n();
  const container = useRef<HTMLDivElement | null>(null);
  const body = useRef<HTMLDivElement | null>(null);
  const stuckToBottom = useRef(true);
  const drag = useRef<AbortController | null>(null);

  useEffect(() => {
    const element = body.current;
    if (!element || !stuckToBottom.current) return;
    element.scrollTop = element.scrollHeight;
  }, [lines]);
  useEffect(() => () => drag.current?.abort(), []);

  const onScroll = useCallback(() => {
    const element = body.current;
    if (!element) return;
    stuckToBottom.current = element.scrollHeight - element.scrollTop - element.clientHeight < 8;
  }, []);

  const onGripPointerDown = useCallback((event: ReactPointerEvent<HTMLDivElement>) => {
    if (event.button !== 0) return;
    const element = container.current;
    if (!element) return;
    event.currentTarget.setPointerCapture?.(event.pointerId);
    const startY = event.clientY;
    const startHeight = element.getBoundingClientRect().height;
    const controller = new AbortController();
    drag.current = controller;
    const view = element.ownerDocument.defaultView ?? window;
    const move = (moved: PointerEvent) => {
      element.style.height = `${Math.max(80, startHeight + (startY - moved.clientY))}px`;
    };
    const end = () => controller.abort();
    view.addEventListener("pointermove", move, { signal: controller.signal });
    view.addEventListener("pointerup", end, { signal: controller.signal });
    view.addEventListener("pointercancel", end, { signal: controller.signal });
  }, []);

  return (
    <div
      className="browser-panel__log-drawer"
      ref={(node) => {
        container.current = node;
        if (typeof ref === "function") ref(node);
        else if (ref) ref.current = node;
      }}
    >
      <div
        className="browser-panel__log-grip"
        role="separator"
        aria-orientation="horizontal"
        onPointerDown={onGripPointerDown}
      />
      <IconButton
        className="browser-panel__log-close"
        label={t("关闭开发服务器日志", "Close dev server logs")}
        onClick={onClose}
      >
        <X size={13} />
      </IconButton>
      <div className="browser-panel__log-body" ref={body} onScroll={onScroll}>
        {lines.length === 0 ? (
          // "Waiting for output" asserts a server that is printing nothing. With none to follow
          // that is a lie, and the drawer reads as a surface nobody wired up.
          <div className="browser-panel__log-empty">
            {serverName
              ? t("等待 {name} 的输出…", "Waiting for output from {name}…", { name: serverName })
              : t("没有正在运行的开发服务器", "No dev server is running")}
          </div>
        ) : (
          lines.map((line, index) => {
            const severity = previewLogSeverity(line);
            return (
              <div
                key={index}
                className={`browser-panel__log-line${severity ? ` is-${severity}` : ""}`}
              >
                {line}
              </div>
            );
          })
        )}
      </div>
    </div>
  );
});

/**
 * The "Servers" section of the overflow menu.
 *
 * Rendered as menu items rather than a submenu because this pane's menu is a single flat native
 * region hole; a second floating layer would need a hole of its own to be visible at all.
 *
 * A row is one hit target: the whole strip highlights, and the trailing `Run` is a label rather
 * than a control, because pressing anywhere on the row already runs it. Only a row that has a
 * process behind it grows a second control — the red `Stop` — and there the row itself opens the
 * page instead of running one.
 */
export function PreviewServerMenuItems({
  rows,
  pendingName,
  currentUrl = null,
  onOpen,
  onRun,
  onStop,
  onStopAll
}: {
  rows: PreviewServerRow[];
  pendingName: string | null;
  /** The committed page address, which marks the row it came from. */
  currentUrl?: string | null;
  onOpen: (row: PreviewServerRow) => void;
  onRun: (name: string) => void;
  onStop: (row: PreviewServerRow) => void;
  onStopAll: () => void;
}) {
  const { t } = useI18n();
  const anyRunning = rows.some((row) => row.running || row.starting);
  // The reference drops the whole group rather than explaining its absence: a browser with no
  // servers says so in the pane body, where there is room for the sentence.
  if (rows.length === 0) return null;
  return (
    <>
      <p className="browser-panel__menu-group">{t("服务器", "Servers")}</p>
      {rows.map((row) => {
        const stoppable = row.running || row.starting;
        const busy = row.starting || row.name === pendingName;
        // An attach row has nothing to run: the server it points at is already someone else's, and
        // the host answers its start call by pointing the preview at it rather than by spawning.
        const openable = row.running || row.attach;
        return (
          <div
            className={`browser-panel__menu-server${stoppable ? " is-running" : ""}`}
            key={`${row.name}:${row.server?.serverId ?? "config"}`}
          >
            <button
              type="button"
              role="menuitemradio"
              aria-checked={previewRowIsOpen(row, currentUrl)}
              aria-label={openable
                ? t("打开 {name}", "Open {name}", { name: row.name })
                : t("运行 {name}", "Run {name}", { name: row.name })}
              disabled={row.starting || (pendingName !== null && pendingName !== row.name)}
              onClick={() => (openable ? onOpen(row) : onRun(row.name))}
            >
              <span className="browser-panel__menu-server-name">{row.name}</span>
              {!stoppable && (
                <span className="browser-panel__menu-server-tag" aria-hidden="true">
                  {busy
                    ? <LoaderCircle className="spin" size={11} />
                    : row.attach
                      ? t("打开", "Open")
                      : t("运行", "Run")}
                </span>
              )}
            </button>
            {stoppable && (
              <button
                type="button"
                className="browser-panel__server-action is-stop"
                disabled={pendingName !== null && pendingName !== row.name}
                aria-label={t("停止 {name}", "Stop {name}", { name: row.name })}
                onClick={() => onStop(row)}
              >
                {busy
                  ? <LoaderCircle className="spin" size={11} aria-hidden="true" />
                  : t("停止", "Stop")}
              </button>
            )}
          </div>
        );
      })}
      {anyRunning && (
        <button type="button" role="menuitem" className="is-danger" onClick={onStopAll}>
          <Ban size={13} aria-hidden="true" />
          <span>{t("停止所有服务器", "Stop all servers")}</span>
        </button>
      )}
    </>
  );
}

/**
 * The Files group of the browser menu.
 *
 * The reference also lists every file opened this session, because each of its rows carries a
 * self-contained data:/file: URL it can go back to. Mework shows a local file through a virtual
 * host mapping that the host drops the moment the page navigates away, so a history row here
 * would address something that no longer resolves.
 */
export function PreviewFileMenuItems({ onOpenFile }: { onOpenFile: () => void }) {
  const { t } = useI18n();
  return (
    <>
      <p className="browser-panel__menu-group">{t("文件", "Files")}</p>
      {/* No leading icon: the row's single child lands at the start edge of the item, which is
          where a menu label belongs. An icon would be that start edge instead, and
          `justify-content: space-between` would push the label across to the far side. */}
      <button type="button" role="menuitem" onClick={onOpenFile}>
        <span>{t("打开文件", "Open file")}</span>
      </button>
    </>
  );
}

/** The logs toggle that sits under the server section. */
export function PreviewLogsMenuItem({
  expanded,
  disabled = false,
  onToggle
}: {
  expanded: boolean;
  disabled?: boolean;
  onToggle: () => void;
}) {
  const { t } = useI18n();
  return (
    <button type="button" role="menuitem" disabled={disabled} onClick={onToggle}>
      <span>
        {expanded
          ? t("隐藏开发服务器日志", "Hide dev server logs")
          : t("显示开发服务器日志", "Show dev server logs")}
      </span>
    </button>
  );
}

export interface PreviewServersController {
  configurations: PreviewConfigurationList | null;
  rows: PreviewServerRow[];
  /** Why the last read failed — for a workspace on another machine, usually the machine itself. */
  unreachable: string | null;
  pendingName: string | null;
  startError: { name: string; message: string } | null;
  stopped: { label: string; name: string } | null;
  run: (name: string) => Promise<void>;
  /**
   * Names the server whose page the pane is showing. Its disappearance from the process list is
   * what turns the body into the stopped card, so opening a server someone else started has to
   * claim it exactly the way starting one does.
   */
  adopt: (row: PreviewServerRow) => void;
  stop: (row: PreviewServerRow) => Promise<void>;
  stopAll: () => Promise<void>;
  dismissStopped: () => void;
  setAutoVerify: (enabled: boolean) => Promise<void>;
  refresh: () => void;
}

/**
 * Owns `.mework/launch.json`, the running-server list, and every start and stop the pane makes.
 *
 * Without a target the pane has no workspace to read, which is a state to render — an empty picker
 * and the no-config body — not an error, so nothing is polled and nothing throws.
 *
 * A workspace on another machine is read and started there; while its machine cannot be reached
 * the last answer stands and `unreachable` carries why, so the pane can say so instead of
 * pretending the file is empty.
 */
export function usePreviewServers(
  target: PreviewTarget | null | undefined,
  onServerReady: (row: PreviewServerRow) => void
): PreviewServersController {
  const [configurations, setConfigurations] = useState<PreviewConfigurationList | null>(null);
  const [servers, setServers] = useState<PreviewServerSnapshot[]>([]);
  const [pendingName, setPendingName] = useState<string | null>(null);
  const [startError, setStartError] = useState<{ name: string; message: string } | null>(null);
  const [stopped, setStopped] = useState<{ label: string; name: string } | null>(null);
  const [refreshKey, setRefreshKey] = useState(0);
  const [unreachable, setUnreachable] = useState<string | null>(null);
  const activeServer = useRef<{ serverId: string; name: string; port: number } | null>(null);
  /** A remote server this pane started, whose page opens once the host says it answers. */
  const pendingOpen = useRef<PreviewServerRow | null>(null);
  const readyCallback = useRef(onServerReady);
  readyCallback.current = onServerReady;
  const targetKey = target ? previewTargetKey(target) : null;

  useEffect(() => {
    // Another workspace's answers are not this one's, not even for the tick it takes to replace them.
    setConfigurations(null);
    setServers([]);
    setUnreachable(null);
    setStartError(null);
    setStopped(null);
    activeServer.current = null;
    pendingOpen.current = null;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [targetKey]);

  useEffect(() => {
    if (!target) {
      setConfigurations(null);
      setServers([]);
      return;
    }
    let cancelled = false;
    let timer = 0;
    const poll = async () => {
      try {
        const [listed, running] = await Promise.all([
          listPreviewConfigurations(target),
          listPreviewServers(target)
        ]);
        if (cancelled) return;
        setConfigurations(listed);
        setServers(running);
        setUnreachable(null);
        const pending = pendingOpen.current;
        if (pending?.server) {
          const now = running.find((server) => server.serverId === pending.server?.serverId);
          if (!now || now.status === "running") {
            pendingOpen.current = null;
            // Gone before it answered: the stopped card says so, and there is nothing to open.
            if (now) readyCallback.current({ ...pending, server: now, running: true, starting: false });
          }
        }
        const active = activeServer.current;
        if (active && !running.some((server) => server.serverId === active.serverId)) {
          activeServer.current = null;
          setStopped({ label: `${active.name}:${active.port}`, name: active.name });
        }
      } catch (reason) {
        // A workspace that has gone away answers again on the next tick; the pane keeps what it
        // has. Only the reason is new: a machine that cannot be reached is worth saying so.
        if (!cancelled) setUnreachable(reason instanceof Error ? reason.message : String(reason));
      } finally {
        if (!cancelled) timer = window.setTimeout(() => void poll(), PREVIEW_SERVER_POLL_INTERVAL_MS);
      }
    };
    void poll();
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [targetKey, refreshKey]);

  const rows = useMemo(
    () => previewServerRows(configurations?.servers ?? [], servers),
    [configurations, servers]
  );

  const refresh = useCallback(() => setRefreshKey((value) => value + 1), []);

  const run = useCallback(async (name: string) => {
    if (!target) return;
    setStartError(null);
    setStopped(null);
    setPendingName(name);
    try {
      const outcome = await startPreviewServer(target, name);
      if (isPreviewAttachment(outcome)) {
        // Nothing was started, so there is no process to claim. Claiming one would arm the poll
        // loop's stopped card against a server that was never in the list to begin with.
        readyCallback.current({
          name: outcome.attached.name,
          port: outcome.attached.port,
          url: outcome.attached.url,
          server: null,
          running: false,
          starting: false,
          attach: true
        });
        return;
      }
      activeServer.current = {
        serverId: outcome.server.serverId,
        name: outcome.server.name,
        port: outcome.server.port
      };
      setServers((current) => [
        ...current.filter((server) => server.serverId !== outcome.server.serverId),
        outcome.server
      ]);
      const row: PreviewServerRow = {
        name: outcome.server.name,
        port: outcome.server.port,
        url: configurations?.servers.find((entry) => entry.name === outcome.server.name)?.url ?? null,
        server: outcome.server,
        running: outcome.server.status === "running",
        starting: outcome.server.status === "starting",
        attach: false
      };
      // A server on another machine is still being waited out there when the start returns, and
      // a page opened now would land on a refused connection a round trip before it could have
      // answered. The pane holds its starting card instead and opens the page once the host says
      // the server answers.
      if (outcome.server.machine && outcome.server.status === "starting") {
        pendingOpen.current = row;
      } else {
        readyCallback.current(row);
      }
    } catch (reason) {
      setStartError({ name, message: reason instanceof Error ? reason.message : String(reason) });
    } finally {
      setPendingName(null);
      refresh();
    }
  }, [target, configurations, refresh]);

  const stop = useCallback(async (row: PreviewServerRow) => {
    const serverId = row.server?.serverId;
    if (!serverId) return;
    if (activeServer.current?.serverId === serverId) activeServer.current = null;
    setServers((current) => current.filter((server) => server.serverId !== serverId));
    try {
      await stopPreviewServer(serverId);
    } finally {
      refresh();
    }
  }, [refresh]);

  const stopAll = useCallback(async () => {
    const live = rows.map((row) => row.server?.serverId).filter((id): id is string => Boolean(id));
    activeServer.current = null;
    setServers([]);
    try {
      await Promise.all(live.map((serverId) => stopPreviewServer(serverId).catch(() => false)));
    } finally {
      refresh();
    }
  }, [rows, refresh]);

  const adopt = useCallback((row: PreviewServerRow) => {
    if (!row.server) return;
    activeServer.current = {
      serverId: row.server.serverId,
      name: row.server.name,
      port: row.server.port
    };
    setStopped(null);
  }, []);

  const setAutoVerify = useCallback(async (enabled: boolean) => {
    if (!target) return;
    try {
      await setPreviewAutoVerify(target, enabled);
    } finally {
      refresh();
    }
  }, [target, refresh]);

  return {
    configurations,
    rows,
    unreachable,
    pendingName,
    startError,
    stopped,
    run,
    adopt,
    stop,
    stopAll,
    dismissStopped: useCallback(() => setStopped(null), []),
    setAutoVerify,
    refresh
  };
}

/** Polls one server's buffered output on the source's cadence while the drawer is open. */
export function usePreviewServerLogs(serverId: string | null, enabled: boolean): string[] {
  const [lines, setLines] = useState<string[]>([]);
  useEffect(() => {
    if (!enabled || !serverId) {
      setLines([]);
      return;
    }
    let cancelled = false;
    let timer = 0;
    const poll = async () => {
      try {
        const rendered = await readPreviewServerLogs(serverId, { lines: PREVIEW_MAX_LOG_LINES });
        if (!cancelled) setLines(previewLogLines(rendered));
      } catch {
        // A stopped server takes its buffer with it; the drawer falls back to its empty state.
      } finally {
        if (!cancelled) timer = window.setTimeout(() => void poll(), PREVIEW_LOG_POLL_INTERVAL_MS);
      }
    };
    void poll();
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
  }, [serverId, enabled]);
  return lines;
}

export { previewServerAddress };
