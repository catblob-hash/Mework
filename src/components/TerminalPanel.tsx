import { FitAddon } from "@xterm/addon-fit";
import { Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import { LoaderCircle, RotateCcw, SquareTerminal, X } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import { useI18n } from "../i18n";
import { installCspStyleNonce } from "../lib/cspStyleNonce";
import { IconButton } from "./Common";
import {
  detachTerminal,
  openTerminal,
  resizeTerminal,
  writeTerminal
} from "../lib/terminal";
import type {
  TerminalCommandState,
  TerminalEvent,
  TerminalPhase,
  TerminalSessionState
} from "../lib/terminal";

type TerminalUiTheme = "day" | "night";

const TERMINAL_DAY_COLORS = {
  background: "#171817",
  foreground: "#d8ddd8",
  cursor: "#d8ddd8",
  selectionBackground: "#506a8c88"
} as const;

function invertRgbHex(value: string): string {
  const match = /^#([0-9a-f]{6})([0-9a-f]{2})?$/i.exec(value);
  if (!match) return value;
  const rgb = [0, 2, 4].map((offset) => (
    (255 - Number.parseInt(match[1].slice(offset, offset + 2), 16)).toString(16).padStart(2, "0")
  )).join("");
  return `#${rgb}${match[2] ?? ""}`;
}

export function terminalUiColors(theme: TerminalUiTheme): Record<keyof typeof TERMINAL_DAY_COLORS, string> {
  if (theme === "day") return TERMINAL_DAY_COLORS;
  return {
    background: invertRgbHex(TERMINAL_DAY_COLORS.background),
    foreground: invertRgbHex(TERMINAL_DAY_COLORS.foreground),
    cursor: invertRgbHex(TERMINAL_DAY_COLORS.cursor),
    selectionBackground: invertRgbHex(TERMINAL_DAY_COLORS.selectionBackground)
  };
}

function currentTerminalUiTheme(): TerminalUiTheme {
  return typeof document !== "undefined" && document.documentElement.dataset.theme === "night" ? "night" : "day";
}

/**
 * Whether a keydown is the copy chord of a Windows terminal: Ctrl/Cmd+C or Ctrl+Insert. xterm
 * would turn either into a control byte and cancel the browser event, so a panel that wants the
 * chord to copy has to decline it before xterm sees it — with text selected here, always on the
 * read-only task page.
 */
export function isCopyChord(event: KeyboardEvent): boolean {
  if (event.type !== "keydown" || event.altKey || event.shiftKey) return false;
  if (!(event.ctrlKey || event.metaKey)) return false;
  return event.key === "Insert" || event.key === "c" || event.key === "C";
}

interface TerminalMetadata {
  cwd: string;
  shell: string;
}

export interface TerminalPanelProps {
  conversationId: string;
  terminalId: string;
  label: string;
  open: boolean;
  initialState?: TerminalSessionState;
  inputDisabledReason?: string | null;
  onCommandStart?: () => boolean;
  onStateChange?: (state: TerminalSessionState) => void;
  /**
   * Kills the host session and settles once it is gone; a rejection is shown as
   * the session's failure. Absent when the host has nothing to close, which also
   * hides the control.
   */
  onClose?: () => Promise<void> | void;
}

export function terminalPanelId(conversationId: string, terminalId: string): string {
  return `conversation-terminal-${conversationId}-${terminalId}`;
}

function errorMessage(error: unknown, fallback: string): string {
  if (error instanceof Error && error.message.trim()) return error.message;
  if (typeof error === "string" && error.trim()) return error;
  return fallback;
}

function decodeBytes(decoder: TextDecoder, bytes: number[], stream: boolean): string {
  return decoder.decode(Uint8Array.from(bytes), { stream });
}

export function TerminalPanel({
  conversationId,
  terminalId,
  label,
  open,
  initialState,
  inputDisabledReason = null,
  onCommandStart = () => true,
  onStateChange = () => undefined,
  onClose
}: TerminalPanelProps) {
  const { t } = useI18n();
  const hostRef = useRef<HTMLDivElement>(null);
  const terminalRef = useRef<Terminal | null>(null);
  const fitAddonRef = useRef<FitAddon | null>(null);
  const fitFrameRef = useRef<number | null>(null);
  const mountedRef = useRef(true);
  const openRef = useRef(open);
  const wasOpenRef = useRef(false);
  const conversationIdRef = useRef(conversationId);
  const terminalIdRef = useRef(terminalId);
  const sessionIdRef = useRef<string | null>(null);
  const attemptRef = useRef(0);
  /** The open still in flight, so a close can wait for the host to have a session to kill. */
  const openingRef = useRef<Promise<void> | null>(null);
  const runningRef = useRef(false);
  const controlReadyRef = useRef(false);
  const inputDisabledReasonRef = useRef(inputDisabledReason);
  const onCommandStartRef = useRef(onCommandStart);
  const onCloseRef = useRef(onClose);
  const busyRef = useRef(initialState?.busy ?? false);
  const hasHistoryRef = useRef(initialState?.hasHistory ?? false);
  const commandRevisionRef = useRef(-1);
  const metadataRef = useRef<TerminalMetadata>({
    cwd: initialState?.cwd ?? "",
    shell: initialState?.shell ?? ""
  });
  const decoderRef = useRef(new TextDecoder());
  const phaseRef = useRef<TerminalPhase>(initialState?.phase ?? "idle");
  const failureRef = useRef<string | null>(null);
  const onStateChangeRef = useRef(onStateChange);
  const labelRef = useRef(label);
  const [activated, setActivated] = useState(open);
  const [phase, setPhase] = useState<TerminalPhase>(initialState?.phase ?? "idle");
  const [busy, setBusy] = useState(initialState?.busy ?? false);
  const [hasHistory, setHasHistory] = useState(initialState?.hasHistory ?? false);
  const [metadata, setMetadata] = useState<TerminalMetadata>(metadataRef.current);
  const [exitCode, setExitCode] = useState<number | null>(null);
  const [failure, setFailure] = useState<string | null>(null);
  const fallbackMessagesRef = useRef({
    connection: t("无法连接终端", "Unable to connect to terminal"),
    read: t("终端读取失败", "Failed to read from terminal")
  });

  openRef.current = open;
  terminalIdRef.current = terminalId;
  inputDisabledReasonRef.current = inputDisabledReason;
  onCommandStartRef.current = onCommandStart;
  onCloseRef.current = onClose;
  onStateChangeRef.current = onStateChange;
  labelRef.current = label;
  fallbackMessagesRef.current = {
    connection: t("无法连接终端", "Unable to connect to terminal"),
    read: t("终端读取失败", "Failed to read from terminal")
  };

  useEffect(() => {
    onStateChange({
      terminalId,
      conversationId,
      label,
      phase,
      busy,
      hasHistory,
      cwd: metadata.cwd,
      shell: metadata.shell,
      sessionId: sessionIdRef.current
    });
  }, [
    busy,
    conversationId,
    hasHistory,
    label,
    metadata.cwd,
    metadata.shell,
    onStateChange,
    phase,
    terminalId
  ]);

  const updatePhase = useCallback((next: TerminalPhase) => {
    phaseRef.current = next;
    if (mountedRef.current) setPhase(next);
  }, []);

  /** The phase as of now: `phaseRef.current` moves under an `await`, which an early-return guard on it would hide. */
  const currentPhase = useCallback((): TerminalPhase => phaseRef.current, []);

  const updateBusy = useCallback((next: boolean) => {
    busyRef.current = next;
    if (mountedRef.current) setBusy(next);
  }, []);

  const updateHasHistory = useCallback((next: boolean) => {
    hasHistoryRef.current = next;
    if (mountedRef.current) setHasHistory(next);
  }, []);

  const applyCommandState = useCallback((next: TerminalCommandState) => {
    if (next.revision <= commandRevisionRef.current) return;
    commandRevisionRef.current = next.revision;
    updateBusy(next.status === "running");
    updateHasHistory(next.commandCount > 0);
  }, [updateBusy, updateHasHistory]);

  /**
   * Whether keystrokes may reach the shell right now. Closing is part of the
   * gate itself, not of the paths that open it, so an open or ready handshake
   * that lands while the host is killing the session cannot let input through.
   */
  const inputAccepted = useCallback(() => (
    controlReadyRef.current
    && !inputDisabledReasonRef.current
    && phaseRef.current !== "closing"
  ), []);

  const syncTerminalInputGate = useCallback(() => {
    const textarea = terminalRef.current?.textarea;
    if (textarea) {
      // Do not use xterm's disableStdin option here: it also suppresses
      // terminal-generated VT replies such as the cursor-position response
      // PSReadLine needs during startup.
      textarea.readOnly = !inputAccepted();
    }
  }, [inputAccepted]);

  const failSession = useCallback((message: string) => {
    runningRef.current = false;
    controlReadyRef.current = false;
    syncTerminalInputGate();
    failureRef.current = message;
    if (mountedRef.current) setFailure(message);
    // A failure reported while the host is still killing the session is kept
    // for the close to end on; the close owns the phase until then.
    if (phaseRef.current !== "closing") updatePhase("error");
  }, [syncTerminalInputGate, updatePhase]);

  const scheduleFit = useCallback((focus = false) => {
    if (!openRef.current || fitFrameRef.current !== null) return;
    fitFrameRef.current = window.requestAnimationFrame(() => {
      fitFrameRef.current = null;
      if (!openRef.current) return;
      try {
        fitAddonRef.current?.fit();
      } catch {
        // xterm cannot measure while its animated region has no usable size yet.
      }
      if (focus) terminalRef.current?.focus();
    });
  }, []);

  const processEvent = useCallback((
    event: TerminalEvent,
    expectedConversationId: string,
    expectedSessionId: string,
    attempt: number
  ) => {
    if (
      attemptRef.current !== attempt
      || conversationIdRef.current !== expectedConversationId
      || sessionIdRef.current !== expectedSessionId
      || event.sessionId !== expectedSessionId
    ) return;

    if (event.type === "output") {
      const text = decodeBytes(decoderRef.current, event.data, true);
      if (text) terminalRef.current?.write(text);
      return;
    }

    if (event.type === "command_state") {
      applyCommandState(event.commandState);
      return;
    }

    if (event.type === "ready") {
      controlReadyRef.current = true;
      syncTerminalInputGate();
      if (runningRef.current && phaseRef.current !== "closing") updatePhase("running");
      scheduleFit(openRef.current);
      return;
    }

    if (event.type === "error") {
      failSession(event.message.trim() || fallbackMessagesRef.current.read);
      return;
    }

    const trailing = decodeBytes(decoderRef.current, [], false);
    if (trailing) terminalRef.current?.write(trailing);
    runningRef.current = false;
    controlReadyRef.current = false;
    syncTerminalInputGate();
    updateBusy(false);
    if (mountedRef.current) setExitCode(event.exitCode);
    // An exit that lands mid-close is recorded but the close still ends the phase.
    if (phaseRef.current !== "error" && phaseRef.current !== "closing") updatePhase("exited");
  }, [
    applyCommandState,
    failSession,
    scheduleFit,
    syncTerminalInputGate,
    updateBusy,
    updatePhase
  ]);

  const startSession = useCallback(async () => {
    const terminal = terminalRef.current;
    if (!terminal || phaseRef.current === "connecting" || phaseRef.current === "closing") return;

    const expectedConversationId = conversationId;
    const expectedTerminalId = terminalId;
    const attempt = attemptRef.current + 1;
    attemptRef.current = attempt;
    runningRef.current = false;
    const previousSessionId = sessionIdRef.current;
    sessionIdRef.current = null;
    controlReadyRef.current = false;
    syncTerminalInputGate();
    decoderRef.current = new TextDecoder();
    commandRevisionRef.current = -1;
    terminal.reset();
    terminal.clear();
    failureRef.current = null;
    if (mountedRef.current) {
      setFailure(null);
      setExitCode(null);
      setMetadata({ cwd: "", shell: "" });
    }
    updatePhase("connecting");

    if (previousSessionId) {
      try {
        await detachTerminal(expectedConversationId, expectedTerminalId, previousSessionId);
      } catch {
        // A stopped session may already have released its attachment.
      }
    }
    if (
      attemptRef.current !== attempt
      || conversationIdRef.current !== expectedConversationId
      || terminalIdRef.current !== expectedTerminalId
      || !mountedRef.current
    ) return;

    try {
      fitAddonRef.current?.fit();
    } catch {
      // Start with xterm's default dimensions; ResizeObserver will fit after expansion.
    }

    const pendingEvents: TerminalEvent[] = [];
    let acceptedSessionId: string | null = null;
    const onEvent = (event: TerminalEvent) => {
      if (
        attemptRef.current !== attempt
        || conversationIdRef.current !== expectedConversationId
        || terminalIdRef.current !== expectedTerminalId
        || !mountedRef.current
      ) return;
      if (!acceptedSessionId) {
        pendingEvents.push(event);
        return;
      }
      processEvent(event, expectedConversationId, acceptedSessionId, attempt);
    };

    try {
      const result = await openTerminal(
        expectedConversationId,
        expectedTerminalId,
        Math.max(2, terminal.cols),
        Math.max(1, terminal.rows),
        onEvent
      );
      if (
        attemptRef.current !== attempt
        || conversationIdRef.current !== expectedConversationId
        || terminalIdRef.current !== expectedTerminalId
        || !mountedRef.current
      ) {
        return;
      }

      acceptedSessionId = result.sessionId;
      sessionIdRef.current = result.sessionId;
      runningRef.current = result.running;
      controlReadyRef.current = result.ready;
      syncTerminalInputGate();
      metadataRef.current = { cwd: result.cwd, shell: result.shell };
      if (mountedRef.current) setMetadata(metadataRef.current);

      const snapshot = decodeBytes(decoderRef.current, result.snapshot, result.running);
      if (snapshot) terminal.write(snapshot);
      // A close that arrived while this open was in flight keeps its phase; it
      // settles the session itself once the host has killed it.
      if (currentPhase() !== "closing") {
        updatePhase(result.running ? (result.ready ? "running" : "connecting") : "exited");
      }
      applyCommandState(result.commandState);
      if (!result.running) updateBusy(false);

      for (const event of pendingEvents) {
        processEvent(event, expectedConversationId, result.sessionId, attempt);
      }

      scheduleFit(result.running && result.ready && openRef.current);
    } catch (error) {
      if (
        attemptRef.current === attempt
        && conversationIdRef.current === expectedConversationId
        && terminalIdRef.current === expectedTerminalId
        && mountedRef.current
      ) failSession(errorMessage(error, fallbackMessagesRef.current.connection));
    }
  }, [
    applyCommandState,
    conversationId,
    currentPhase,
    failSession,
    processEvent,
    scheduleFit,
    syncTerminalInputGate,
    terminalId,
    updateBusy,
    updatePhase
  ]);

  const launchSession = useCallback(() => {
    const opening = startSession().finally(() => {
      if (openingRef.current === opening) openingRef.current = null;
    });
    openingRef.current = opening;
  }, [startSession]);

  /**
   * The host's close is the only thing that ends a session on request: a killed
   * session sends no exit event, so the panel settles itself once the host
   * confirms the kill instead of waiting for one that never comes.
   */
  const closeSession = useCallback(async () => {
    const close = onCloseRef.current;
    if (!close || phaseRef.current === "closing") return;
    const expectedConversationId = conversationIdRef.current;
    const expectedTerminalId = terminalIdRef.current;
    const attempt = attemptRef.current;
    const stillCurrent = () => (
      attemptRef.current === attempt
      && conversationIdRef.current === expectedConversationId
      && terminalIdRef.current === expectedTerminalId
      && mountedRef.current
    );
    // Dismissing a dead session keeps its verdict; only a live one is being killed.
    const wasLive = phaseRef.current === "connecting" || phaseRef.current === "running";
    if (wasLive) failureRef.current = null;
    updatePhase("closing");
    syncTerminalInputGate();
    // An open still in flight has not given the host a session to kill yet.
    if (openingRef.current) await openingRef.current;
    if (!stillCurrent()) return;
    try {
      await close();
    } catch (error) {
      if (!stillCurrent()) return;
      // Leave "closing" first: failSession keeps a failure reported mid-close.
      updatePhase("error");
      failSession(errorMessage(error, fallbackMessagesRef.current.connection));
      return;
    }
    if (!stillCurrent()) return;
    runningRef.current = false;
    controlReadyRef.current = false;
    sessionIdRef.current = null;
    updateBusy(false);
    // Whatever the session reported while it was going down — an exit code from
    // the shell, or an error from the host — stays on the record.
    updatePhase(failureRef.current ? "error" : "exited");
    syncTerminalInputGate();
  }, [failSession, syncTerminalInputGate, updateBusy, updatePhase]);

  useEffect(() => {
    if (open) setActivated(true);
  }, [open]);

  useEffect(() => {
    syncTerminalInputGate();
  }, [inputDisabledReason, syncTerminalInputGate]);

  useEffect(() => {
    if (!activated) return;
    mountedRef.current = true;
    const host = hostRef.current;
    if (!host) return;

    const releaseCspStyleNonce = installCspStyleNonce(host.ownerDocument);
    let terminal!: Terminal;
    let fitAddon!: FitAddon;
    try {
      terminal = new Terminal({
        cursorBlink: true,
        fontFamily: "ui-monospace, SFMono-Regular, Consolas, 'Liberation Mono', monospace",
        fontSize: 12,
        lineHeight: 1.2,
        scrollback: 5_000,
        disableStdin: false,
        theme: terminalUiColors(currentTerminalUiTheme())
      });
      fitAddon = new FitAddon();
      terminal.loadAddon(fitAddon);
      terminal.open(host);
    } catch (error) {
      terminal?.dispose();
      releaseCspStyleNonce();
      throw error;
    }
    terminalRef.current = terminal;
    fitAddonRef.current = fitAddon;
    terminal.attachCustomKeyEventHandler((event) => {
      // Ctrl+C over a selection copies rather than interrupting, as every Windows terminal does,
      // and it must keep working while input is gated: reading a transcript is not input.
      if (isCopyChord(event) && terminal.hasSelection()) return false;
      return inputAccepted();
    });
    syncTerminalInputGate();

    const blockDisabledUserInput = (event: Event) => {
      if (!inputAccepted()) event.preventDefault();
    };
    host.addEventListener("beforeinput", blockDisabledUserInput, true);
    host.addEventListener("paste", blockDisabledUserInput, true);
    host.addEventListener("drop", blockDisabledUserInput, true);

    const inputDisposable = terminal.onData((data) => {
      const activeConversationId = conversationIdRef.current;
      const activeTerminalId = terminalIdRef.current;
      const activeSessionId = sessionIdRef.current;
      // Protocol replies pass before the handshake is ready, but nothing goes to a
      // shell the host is killing.
      if (!runningRef.current || !activeSessionId || phaseRef.current === "closing") return;
      // Any Enter may submit a recalled/history command even when the renderer has never seen
      // its text. This is only an optimistic UI reservation; Rust's PSConsoleHostReadLine
      // ACK barrier remains the authority that decides whether the line may execute.
      const startsCommand = /[\r\n]/.test(data);
      if (startsCommand && !onCommandStartRef.current()) return;
      if (startsCommand) updateBusy(true);
      void writeTerminal(activeConversationId, activeTerminalId, activeSessionId, data).catch((error) => {
        if (
          conversationIdRef.current === activeConversationId
          && terminalIdRef.current === activeTerminalId
          && sessionIdRef.current === activeSessionId
        ) {
          failSession(errorMessage(error, fallbackMessagesRef.current.connection));
        }
      });
    });
    const resizeDisposable = terminal.onResize(({ cols, rows }) => {
      const activeConversationId = conversationIdRef.current;
      const activeTerminalId = terminalIdRef.current;
      const activeSessionId = sessionIdRef.current;
      if (!openRef.current || !runningRef.current || !activeSessionId) return;
      void resizeTerminal(activeConversationId, activeTerminalId, activeSessionId, cols, rows).catch((error) => {
        if (
          conversationIdRef.current === activeConversationId
          && terminalIdRef.current === activeTerminalId
          && sessionIdRef.current === activeSessionId
        ) failSession(errorMessage(error, fallbackMessagesRef.current.connection));
      });
    });

    const resizeObserver = typeof ResizeObserver === "undefined"
      ? null
      : new ResizeObserver(() => scheduleFit());
    resizeObserver?.observe(host);
    const onWindowResize = () => scheduleFit();
    window.addEventListener("resize", onWindowResize);
    const themeObserver = typeof MutationObserver === "undefined"
      ? null
      : new MutationObserver(() => {
        const options = (terminal as unknown as {
          options?: { theme?: ReturnType<typeof terminalUiColors> };
        }).options;
        if (options) options.theme = terminalUiColors(currentTerminalUiTheme());
      });
    themeObserver?.observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });

    return () => {
      mountedRef.current = false;
      if (fitFrameRef.current !== null) window.cancelAnimationFrame(fitFrameRef.current);
      fitFrameRef.current = null;
      resizeObserver?.disconnect();
      themeObserver?.disconnect();
      window.removeEventListener("resize", onWindowResize);
      host.removeEventListener("beforeinput", blockDisabledUserInput, true);
      host.removeEventListener("paste", blockDisabledUserInput, true);
      host.removeEventListener("drop", blockDisabledUserInput, true);
      inputDisposable.dispose();
      resizeDisposable.dispose();
      try {
        terminal.dispose();
      } finally {
        releaseCspStyleNonce();
      }
      terminalRef.current = null;
      fitAddonRef.current = null;
    };
  }, [
    activated,
    failSession,
    inputAccepted,
    scheduleFit,
    syncTerminalInputGate,
    updateBusy
  ]);

  useEffect(() => {
    const expectedConversationId = conversationId;
    const expectedTerminalId = terminalId;
    conversationIdRef.current = expectedConversationId;
    terminalIdRef.current = expectedTerminalId;
    attemptRef.current += 1;
    sessionIdRef.current = null;
    runningRef.current = false;
    controlReadyRef.current = false;
    syncTerminalInputGate();
    busyRef.current = initialState?.busy ?? false;
    hasHistoryRef.current = initialState?.hasHistory ?? false;
    commandRevisionRef.current = -1;
    decoderRef.current = new TextDecoder();
    terminalRef.current?.reset();
    terminalRef.current?.clear();
    metadataRef.current = {
      cwd: initialState?.cwd ?? "",
      shell: initialState?.shell ?? ""
    };
    if (mountedRef.current) {
      setMetadata(metadataRef.current);
      setBusy(busyRef.current);
      setHasHistory(hasHistoryRef.current);
      setFailure(null);
      setExitCode(null);
    }
    failureRef.current = null;
    updatePhase("idle");

    return () => {
      attemptRef.current += 1;
      const activeSessionId = sessionIdRef.current;
      sessionIdRef.current = null;
      runningRef.current = false;
      controlReadyRef.current = false;
      if (activeSessionId) {
        void detachTerminal(expectedConversationId, expectedTerminalId, activeSessionId).catch(() => undefined);
      }
      // Detached, the renderer no longer sees this terminal's command state; the
      // last report must say so rather than pin a busy flag nobody will clear.
      onStateChangeRef.current({
        terminalId: expectedTerminalId,
        conversationId: expectedConversationId,
        label: labelRef.current,
        phase: "idle",
        busy: false,
        hasHistory: hasHistoryRef.current,
        cwd: metadataRef.current.cwd,
        shell: metadataRef.current.shell,
        sessionId: null
      });
    };
  }, [
    conversationId,
    syncTerminalInputGate,
    terminalId,
    updatePhase
  ]);

  useEffect(() => {
    // Expanding the drawer is the one user action that may spend a reconnect on
    // its own: a session that ended — closed from the header, or exited — comes
    // back fresh, while a failure that happened with the drawer open stays put
    // until the user retries it.
    const expanded = open && !wasOpenRef.current;
    wasOpenRef.current = open;
    if (!open || !activated) return;
    if (
      phaseRef.current === "idle"
      || (expanded && (phaseRef.current === "exited" || phaseRef.current === "error"))
    ) {
      launchSession();
      return;
    }
    scheduleFit(phaseRef.current === "running");
  }, [activated, conversationId, launchSession, open, scheduleFit, terminalId]);

  const statusLabel = inputDisabledReason
    ? inputDisabledReason
    : phase === "connecting"
    ? t("正在连接", "Connecting")
    : phase === "running"
      ? busy
        ? t("运行中", "Running")
        : hasHistory
          ? t("待机", "Idle")
          : t("空终端", "Empty terminal")
      : phase === "closing"
        ? t("正在终止", "Terminating")
        : phase === "exited"
          ? exitCode === null
            ? t("已退出", "Exited")
            : t("已退出（代码 {code}）", "Exited (code {code})", { code: exitCode })
          : phase === "error"
            ? failure ?? t("终端错误", "Terminal error")
            : t("未启动", "Not started");
  // A session that ended is the only one worth restarting, so the header offers
  // the retry only then — and offers nothing before the drawer has ever opened
  // this terminal. Closing is always on offer while the host can honour it: it
  // kills a live shell and dismisses a dead one alike.
  const restartable = phase === "error" || phase === "exited";
  const retryLabel = t("重试", "Retry");

  return (
    <section
      id={terminalPanelId(conversationId, terminalId)}
      className={`collapse-region terminal-panel-region${open ? "" : " collapse-region--closed"}`}
      aria-label={t("终端", "Terminal")}
      aria-hidden={!open || undefined}
      inert={!open || undefined}
    >
      <div className="collapse-region__inner terminal-panel-region__inner">
        <div
          className={`terminal-panel terminal-panel--${phase}${inputDisabledReason ? " terminal-panel--input-disabled" : ""}`}
          data-session-id={sessionIdRef.current ?? undefined}
        >
          <header className="terminal-panel__header">
            <div className="terminal-panel__identity">
              <SquareTerminal size={14} />
              <strong>{label}</strong>
              {(metadata.shell || metadata.cwd) && (
                <span title={[metadata.shell, metadata.cwd].filter(Boolean).join(" · ")}>
                  {[metadata.shell, metadata.cwd].filter(Boolean).join(" · ")}
                </span>
              )}
            </div>
            <span
              className={`terminal-panel__status terminal-panel__status--${phase}`}
              role={phase === "error" ? "alert" : "status"}
              aria-live="polite"
              title={inputDisabledReason ?? (phase === "error" ? failure ?? undefined : undefined)}
            >
              {(phase === "connecting" || phase === "closing") && <LoaderCircle className="spin" size={12} />}
              {statusLabel}
            </span>
            {restartable && (
              <button
                type="button"
                className="terminal-panel__retry"
                aria-label={retryLabel}
                onClick={launchSession}
              >
                <RotateCcw size={12} />
                {retryLabel}
              </button>
            )}
            {onClose && phase !== "idle" && (
              <IconButton
                className="terminal-panel__terminate"
                label={t("关闭终端", "Close the terminal")}
                disabled={phase === "closing"}
                onClick={() => void closeSession()}
              >
                <X size={13} />
              </IconButton>
            )}
          </header>
          <div
            className="terminal-panel__viewport"
            ref={hostRef}
            aria-label={t("终端输入", "Terminal input")}
            aria-disabled={Boolean(inputDisabledReason) || phase === "connecting"}
          />
        </div>
      </div>
    </section>
  );
}
