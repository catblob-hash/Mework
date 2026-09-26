import { FitAddon } from "@xterm/addon-fit";
import { Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import { LoaderCircle, RotateCcw } from "lucide-react";
import { forwardRef, useCallback, useEffect, useImperativeHandle, useRef, useState } from "react";
import { useI18n } from "../i18n";
import { installCspStyleNonce } from "../lib/cspStyleNonce";
import { detachTerminal, openTerminal, resizeTerminal, writeTerminal } from "../lib/terminal";
import { mirrorLightnessHex } from "../lib/themeMirror";
import { BRAND_AMBER } from "./brandMark";
import type {
  TerminalCommandState,
  TerminalEvent,
  TerminalLaunchChoice,
  TerminalPhase,
  TerminalSessionState,
} from "../lib/terminal";

type TerminalUiTheme = "day" | "night";

/* The chrome around the grid, kept in step with `--color-e8e7e8`/`--color-272227`
   in the palette; the selection is xterm's own and has no token. The cursor is the brand
   mark in its amber, `--color-c98a1b` — `terminal.css` cuts xterm's block into the mark's
   shape — and the glyph it covers is `cursorAccent`. */
const TERMINAL_DAY_COLORS = {
  background: "#e8e7e8",
  foreground: "#272227",
  cursor: BRAND_AMBER,
  cursorAccent: "#272227",
  selectionBackground: "#af957388",
} as const;

export function terminalUiColors(
  theme: TerminalUiTheme,
): Record<keyof typeof TERMINAL_DAY_COLORS, string> {
  if (theme === "day") return TERMINAL_DAY_COLORS;
  return {
    background: mirrorLightnessHex(TERMINAL_DAY_COLORS.background),
    foreground: mirrorLightnessHex(TERMINAL_DAY_COLORS.foreground),
    cursor: mirrorLightnessHex(TERMINAL_DAY_COLORS.cursor),
    // Amber stays mid-light at night, so the covered glyph keeps the dark ink rather than
    // mirroring to a light one it could not be read against: the night ground.
    cursorAccent: mirrorLightnessHex(TERMINAL_DAY_COLORS.background),
    selectionBackground: mirrorLightnessHex(TERMINAL_DAY_COLORS.selectionBackground),
  };
}

function currentTerminalUiTheme(): TerminalUiTheme {
  return typeof document !== "undefined" && document.documentElement.dataset.theme === "night"
    ? "night"
    : "day";
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
  /**
   * Which workspace the shell starts in and which shell it is. Read when the
   * panel opens its session; a tab keeps one choice for its whole life, so a
   * change here does not restart a running shell.
   */
  launch?: TerminalLaunchChoice;
  /**
   * Set only while the terminal belongs to the draft: the project the draft is aimed at, which the
   * host resolves the shell's directory from because it has no conversation to read it off yet.
   * Read when the panel opens its session, like `launch`; the draft becoming real does not
   * restart a running shell.
   */
  draftWorkspaceId?: string | null;
  /**
   * Awaited before every open, for whatever the host must already know when the open reaches it —
   * a project the draft was just aimed at, say, that is still on its way to the host.
   */
  beforeOpen?: () => Promise<void>;
  initialState?: TerminalSessionState;
  inputDisabledReason?: string | null;
  onCommandStart?: () => boolean;
  onStateChange?: (state: TerminalSessionState) => void;
  /**
   * Kills the host session and settles once it is gone; a rejection is shown as
   * the session's failure. The reference shell hangs its close control off the
   * terminal's own tab, so the host renders it there from this handle — the
   * panel draws no close control of its own.
   */
  onClose?: () => Promise<void> | void;
  /**
   * Fires once per clean exit (code 0) that ends the session on its own — not
   * one killed by the close control. The reference shell folds a terminal away
   * on a clean exit and keeps a failing one on screen; the host decides what
   * folding means for the tab that held it.
   */
  onCleanExit?: () => void;
}

/** What the host's title-bar controls may drive on the panel, by ref. */
export interface TerminalPanelHandle {
  /** Requests the kill: enters `closing`, awaits the host close, settles. */
  close(): Promise<void>;
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

export const TerminalPanel = forwardRef<TerminalPanelHandle, TerminalPanelProps>(
  function TerminalPanel(
    {
      conversationId,
      terminalId,
      label,
      open,
      launch,
      draftWorkspaceId = null,
      beforeOpen,
      initialState,
      inputDisabledReason = null,
      onCommandStart = () => true,
      onStateChange = () => undefined,
      onClose,
      onCleanExit = () => undefined,
    },
    ref,
  ) {
    const { t } = useI18n();
    const hostRef = useRef<HTMLDivElement>(null);
    const terminalRef = useRef<Terminal | null>(null);
    const fitAddonRef = useRef<FitAddon | null>(null);
    const fitFrameRef = useRef<number | null>(null);
    const mountedRef = useRef(true);
    const openRef = useRef(open);
    const conversationIdRef = useRef(conversationId);
    const terminalIdRef = useRef(terminalId);
    const launchRef = useRef(launch);
    launchRef.current = launch;
    const draftWorkspaceIdRef = useRef(draftWorkspaceId);
    draftWorkspaceIdRef.current = draftWorkspaceId;
    const beforeOpenRef = useRef(beforeOpen);
    beforeOpenRef.current = beforeOpen;
    const sessionIdRef = useRef<string | null>(null);
    const attemptRef = useRef(0);
    /** The open still in flight, so a close can wait for the host to have a session to kill. */
    const openingRef = useRef<Promise<void> | null>(null);
    const runningRef = useRef(false);
    const controlReadyRef = useRef(false);
    const inputDisabledReasonRef = useRef(inputDisabledReason);
    const onCommandStartRef = useRef(onCommandStart);
    const onCloseRef = useRef(onClose);
    const onCleanExitRef = useRef(onCleanExit);
    const busyRef = useRef(initialState?.busy ?? false);
    const hasHistoryRef = useRef(initialState?.hasHistory ?? false);
    const commandRevisionRef = useRef(-1);
    const metadataRef = useRef<TerminalMetadata>({
      cwd: initialState?.cwd ?? "",
      shell: initialState?.shell ?? "",
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
      read: t("终端读取失败", "Failed to read from terminal"),
    });

    openRef.current = open;
    terminalIdRef.current = terminalId;
    inputDisabledReasonRef.current = inputDisabledReason;
    onCommandStartRef.current = onCommandStart;
    onCloseRef.current = onClose;
    onStateChangeRef.current = onStateChange;
    onCleanExitRef.current = onCleanExit;
    labelRef.current = label;
    fallbackMessagesRef.current = {
      connection: t("无法连接终端", "Unable to connect to terminal"),
      read: t("终端读取失败", "Failed to read from terminal"),
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
        sessionId: sessionIdRef.current,
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
      terminalId,
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

    const applyCommandState = useCallback(
      (next: TerminalCommandState) => {
        if (next.revision <= commandRevisionRef.current) return;
        commandRevisionRef.current = next.revision;
        updateBusy(next.status === "running");
        updateHasHistory(next.commandCount > 0);
      },
      [updateBusy, updateHasHistory],
    );

    /**
     * Whether keystrokes may reach the shell right now. Closing is part of the
     * gate itself, not of the paths that open it, so an open or ready handshake
     * that lands while the host is killing the session cannot let input through.
     */
    const inputAccepted = useCallback(
      () =>
        controlReadyRef.current &&
        !inputDisabledReasonRef.current &&
        phaseRef.current !== "closing",
      [],
    );

    const syncTerminalInputGate = useCallback(() => {
      const textarea = terminalRef.current?.textarea;
      if (textarea) {
        // Do not use xterm's disableStdin option here: it also suppresses
        // terminal-generated VT replies such as the cursor-position response
        // PSReadLine needs during startup.
        textarea.readOnly = !inputAccepted();
      }
    }, [inputAccepted]);

    const failSession = useCallback(
      (message: string) => {
        runningRef.current = false;
        controlReadyRef.current = false;
        syncTerminalInputGate();
        failureRef.current = message;
        if (mountedRef.current) setFailure(message);
        // A failure reported while the host is still killing the session is kept
        // for the close to end on; the close owns the phase until then.
        if (phaseRef.current !== "closing") updatePhase("error");
      },
      [syncTerminalInputGate, updatePhase],
    );

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

    const processEvent = useCallback(
      (
        event: TerminalEvent,
        expectedConversationId: string,
        expectedSessionId: string,
        attempt: number,
      ) => {
        if (
          attemptRef.current !== attempt ||
          conversationIdRef.current !== expectedConversationId ||
          sessionIdRef.current !== expectedSessionId ||
          event.sessionId !== expectedSessionId
        )
          return;

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
        if (phaseRef.current !== "error" && phaseRef.current !== "closing") {
          updatePhase("exited");
          // A clean exit ends the session on its own, like the reference shell
          // folding its tab away; anything else stays on screen for the retry.
          if (event.exitCode === 0) onCleanExitRef.current();
        }
      },
      [applyCommandState, failSession, scheduleFit, syncTerminalInputGate, updateBusy, updatePhase],
    );

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
        attemptRef.current !== attempt ||
        conversationIdRef.current !== expectedConversationId ||
        terminalIdRef.current !== expectedTerminalId ||
        !mountedRef.current
      )
        return;

      if (beforeOpenRef.current) {
        // A failure here is the open's to report: the host names what it could not find.
        await beforeOpenRef.current().catch(() => undefined);
        if (
          attemptRef.current !== attempt ||
          conversationIdRef.current !== expectedConversationId ||
          terminalIdRef.current !== expectedTerminalId ||
          !mountedRef.current
        )
          return;
      }

      try {
        fitAddonRef.current?.fit();
      } catch {
        // Start with xterm's default dimensions; ResizeObserver will fit after expansion.
      }

      const pendingEvents: TerminalEvent[] = [];
      let acceptedSessionId: string | null = null;
      const onEvent = (event: TerminalEvent) => {
        if (
          attemptRef.current !== attempt ||
          conversationIdRef.current !== expectedConversationId ||
          terminalIdRef.current !== expectedTerminalId ||
          !mountedRef.current
        )
          return;
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
          onEvent,
          launchRef.current,
          draftWorkspaceIdRef.current,
        );
        if (
          attemptRef.current !== attempt ||
          conversationIdRef.current !== expectedConversationId ||
          terminalIdRef.current !== expectedTerminalId ||
          !mountedRef.current
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
          attemptRef.current === attempt &&
          conversationIdRef.current === expectedConversationId &&
          terminalIdRef.current === expectedTerminalId &&
          mountedRef.current
        )
          failSession(errorMessage(error, fallbackMessagesRef.current.connection));
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
      updatePhase,
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
      const stillCurrent = () =>
        attemptRef.current === attempt &&
        conversationIdRef.current === expectedConversationId &&
        terminalIdRef.current === expectedTerminalId &&
        mountedRef.current;
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
          fontFamily: '"SF Mono", ui-monospace, Menlo, Consolas, monospace',
          fontSize: 12,
          scrollback: 10_000,
          minimumContrastRatio: 3,
          disableStdin: false,
          theme: terminalUiColors(currentTerminalUiTheme()),
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
        void writeTerminal(activeConversationId, activeTerminalId, activeSessionId, data).catch(
          (error) => {
            if (
              conversationIdRef.current === activeConversationId &&
              terminalIdRef.current === activeTerminalId &&
              sessionIdRef.current === activeSessionId
            ) {
              failSession(errorMessage(error, fallbackMessagesRef.current.connection));
            }
          },
        );
      });
      const resizeDisposable = terminal.onResize(({ cols, rows }) => {
        const activeConversationId = conversationIdRef.current;
        const activeTerminalId = terminalIdRef.current;
        const activeSessionId = sessionIdRef.current;
        if (!openRef.current || !runningRef.current || !activeSessionId) return;
        void resizeTerminal(
          activeConversationId,
          activeTerminalId,
          activeSessionId,
          cols,
          rows,
        ).catch((error) => {
          if (
            conversationIdRef.current === activeConversationId &&
            terminalIdRef.current === activeTerminalId &&
            sessionIdRef.current === activeSessionId
          )
            failSession(errorMessage(error, fallbackMessagesRef.current.connection));
        });
      });

      const resizeObserver =
        typeof ResizeObserver === "undefined" ? null : new ResizeObserver(() => scheduleFit());
      resizeObserver?.observe(host);
      const onWindowResize = () => scheduleFit();
      window.addEventListener("resize", onWindowResize);
      const themeObserver =
        typeof MutationObserver === "undefined"
          ? null
          : new MutationObserver(() => {
              const options = (
                terminal as unknown as {
                  options?: { theme?: ReturnType<typeof terminalUiColors> };
                }
              ).options;
              if (options) options.theme = terminalUiColors(currentTerminalUiTheme());
            });
      themeObserver?.observe(document.documentElement, {
        attributes: true,
        attributeFilter: ["data-theme"],
      });

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
    }, [activated, failSession, inputAccepted, scheduleFit, syncTerminalInputGate, updateBusy]);

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
        shell: initialState?.shell ?? "",
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
          void detachTerminal(expectedConversationId, expectedTerminalId, activeSessionId).catch(
            () => undefined,
          );
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
          sessionId: null,
        });
      };
    }, [conversationId, syncTerminalInputGate, terminalId, updatePhase]);

    useEffect(() => {
      if (!open || !activated) return;
      // Becoming visible is not a request for a shell. A tab is shown and hidden every time
      // the user moves between terminals, and the pane is hidden and shown without touching
      // anything it holds, so only a terminal that never started starts here — a session that
      // ended keeps its verdict on screen until the retry asks for a new one.
      if (phaseRef.current === "idle") {
        launchSession();
        return;
      }
      scheduleFit(phaseRef.current === "running");
    }, [activated, conversationId, launchSession, open, scheduleFit, terminalId]);

    // A session that ended is the only one worth restarting, so the retry only
    // appears once the shell can be brought back — closing is always on offer
    // while the host can honour it: it kills a live shell and dismisses a dead
    // one alike.
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
                ? (failure ?? t("终端错误", "Terminal error"))
                : t("未启动", "Not started");
    const restartable = phase === "error" || phase === "exited";
    const settled = phase === "exited" || phase === "error";
    const retryLabel = t("重试", "Retry");

    useImperativeHandle(
      ref,
      () => ({
        close: () => closeSession(),
      }),
      [closeSession],
    );

    return (
      <section
        id={terminalPanelId(conversationId, terminalId)}
        className={`collapse-region terminal-panel-region${open ? "" : " collapse-region--closed"}`}
        aria-label={label}
        aria-hidden={!open || undefined}
        inert={!open || undefined}
      >
        <div className="collapse-region__inner terminal-panel-region__inner">
          <div
            className={`terminal-panel terminal-panel--${phase}${inputDisabledReason ? " terminal-panel--input-disabled" : ""}`}
            data-session-id={sessionIdRef.current ?? undefined}
          >
            {/* The reference shell draws no chrome of its own — status lives in the
              accessibility tree and the overlays carry what the eye needs. */}
            {!settled && (
              <span
                className={`terminal-panel__status terminal-panel__status--${phase} sr-only`}
                role="status"
                aria-live="polite"
                title={inputDisabledReason ?? undefined}
              >
                {statusLabel}
              </span>
            )}
            <div
              className="terminal-panel__viewport"
              ref={hostRef}
              aria-label={t("终端输入", "Terminal input")}
              aria-disabled={Boolean(inputDisabledReason) || phase === "connecting"}
            />
            {(phase === "connecting" || phase === "closing") && (
              <div className="terminal-panel__veil" aria-hidden="true">
                <LoaderCircle className="spin" size={16} />
              </div>
            )}
            {settled && (
              <div
                className={`terminal-panel__overlay terminal-panel__overlay--${phase}`}
                role={phase === "error" ? "alert" : "status"}
                aria-live="polite"
              >
                <p className="terminal-panel__overlay-message">{statusLabel}</p>
                {restartable && (
                  <button type="button" className="terminal-panel__retry" onClick={launchSession}>
                    <RotateCcw size={13} aria-hidden="true" />
                    {retryLabel}
                  </button>
                )}
              </div>
            )}
          </div>
        </div>
      </section>
    );
  },
);
