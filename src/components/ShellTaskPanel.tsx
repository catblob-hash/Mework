import { FitAddon } from "@xterm/addon-fit";
import { Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import { Check, Copy, LoaderCircle, Square } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { useI18n } from "../i18n";
import { installCspStyleNonce } from "../lib/cspStyleNonce";
import { detachShellTaskOutput, openShellTaskOutput } from "../lib/shellTasks";
import type { ShellOutputEvent, ShellOutputStream, ShellTaskSnapshot } from "../lib/shellTasks";
import { IconButton } from "./Common";
import { isCopyChord, terminalUiColors } from "./TerminalPanel";
import "./ShellTaskPanel.css";

export function shellTaskPanelId(shellTaskId: string): string {
  return `shell-task-output-${shellTaskId}`;
}

function currentTerminalUiTheme(): "day" | "night" {
  return typeof document !== "undefined" && document.documentElement.dataset.theme === "night"
    ? "night"
    : "day";
}

/** stderr is dimmed rather than coloured red: a warning on stderr is not a failure. */
const STDERR_PREFIX = "\u001b[2m";
const STDERR_SUFFIX = "\u001b[0m";

/** How long the copy control reports its outcome before reverting to its label. */
const COPY_STATUS_MS = 1600;

/**
 * Longest transcript kept for the copy control, in UTF-16 units. The host retains a 256 KiB
 * tail and the page receives everything after it subscribed, so a chatty build could otherwise
 * grow this without bound; like the host's buffer it keeps the end.
 */
const MAX_TRANSCRIPT_LENGTH = 4 * 1024 * 1024;

function appendTranscript(transcript: string, text: string): string {
  const joined = transcript + text;
  return joined.length > MAX_TRANSCRIPT_LENGTH
    ? joined.slice(joined.length - MAX_TRANSCRIPT_LENGTH)
    : joined;
}

/**
 * The desktop runtime rejects an invoke with the raw string the host returned, not an `Error`;
 * only the browser-dev bridge wraps it. Both carry the host's reason and both are worth showing.
 */
function errorMessage(error: unknown, fallback: string): string {
  if (error instanceof Error && error.message.trim()) return error.message;
  if (typeof error === "string" && error.trim()) return error;
  return fallback;
}

function isSelectAllChord(event: KeyboardEvent): boolean {
  return event.type === "keydown"
    && (event.ctrlKey || event.metaKey)
    && !event.altKey
    && !event.shiftKey
    && (event.key === "a" || event.key === "A");
}

async function writeClipboardText(text: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    // The async API is refused outside a secure context or a user gesture; the legacy command
    // still copies from a selected element.
    const textarea = document.createElement("textarea");
    textarea.value = text;
    textarea.setAttribute("readonly", "");
    textarea.style.position = "fixed";
    textarea.style.opacity = "0";
    document.body.appendChild(textarea);
    textarea.select();
    try {
      return typeof document.execCommand === "function" && document.execCommand("copy");
    } catch {
      return false;
    } finally {
      textarea.remove();
    }
  }
}

export interface ShellTaskPanelProps {
  task: ShellTaskSnapshot;
  /** Only the visible page measures and fits; xterm cannot size a hidden box. */
  open: boolean;
  onStop?: () => void;
  stopping?: boolean;
}

/**
 * One `bash` / `powershell` command's output, read-only.
 *
 * Deliberately not `TerminalPanel`: that one owns a PTY — open, write, resize, detach — and is
 * being kept for the interactive terminal that will live in a bottom drawer. This page has no
 * process to talk to. It subscribes to a command the model started and draws what comes back.
 */
export function ShellTaskPanel({ task, open, onStop, stopping = false }: ShellTaskPanelProps) {
  const { t } = useI18n();
  const hostRef = useRef<HTMLElement>(null);
  const terminalRef = useRef<Terminal | null>(null);
  const fitAddonRef = useRef<FitAddon | null>(null);
  const fitFrameRef = useRef<number | null>(null);
  const copyStatusTimerRef = useRef<number | null>(null);
  /** Everything written to the terminal since the last subscribe, for the copy control. */
  const transcriptRef = useRef("");
  const openRef = useRef(open);
  const [failure, setFailure] = useState<string | null>(null);
  const [droppedHeadBytes, setDroppedHeadBytes] = useState(0);
  const [copyStatus, setCopyStatus] = useState<"idle" | "success" | "error">("idle");
  openRef.current = open;

  useEffect(() => {
    const host = hostRef.current;
    if (!host) return;
    const releaseCspStyleNonce = installCspStyleNonce(host.ownerDocument);
    let terminal!: Terminal;
    let fitAddon!: FitAddon;
    try {
      terminal = new Terminal({
        cursorBlink: false,
        fontFamily: "ui-monospace, SFMono-Regular, Consolas, 'Liberation Mono', monospace",
        fontSize: 12,
        lineHeight: 1.2,
        scrollback: 20_000,
        // Nothing here can accept input: there is no process on the other end. Unlike the
        // interactive panel there are also no VT replies to preserve, so the blunt switch is
        // the honest one.
        disableStdin: true,
        // These are pipes, not a pty: no line discipline turns a program's LF into CRLF on the
        // way here, and node, git, python and everything the bash tool runs write LF alone. To
        // xterm a bare LF is "down one row", so without this every line would start where the
        // previous one ended — the staircase the task page used to show.
        convertEol: true,
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
    // xterm turns Ctrl+C into an interrupt and Ctrl+A into a control byte and cancels the browser
    // event either way, so neither would ever copy or select here — where there is no process
    // for them to reach. Declining the event lets the browser run its copy, which xterm's own
    // `copy` listener then fills with the selection.
    terminal.attachCustomKeyEventHandler((event) => {
      if (isCopyChord(event)) return !terminal.hasSelection();
      if (isSelectAllChord(event)) {
        event.preventDefault();
        terminal.selectAll();
        return false;
      }
      return true;
    });
    terminalRef.current = terminal;
    fitAddonRef.current = fitAddon;
    return () => {
      if (fitFrameRef.current !== null) window.cancelAnimationFrame(fitFrameRef.current);
      fitFrameRef.current = null;
      if (copyStatusTimerRef.current !== null) window.clearTimeout(copyStatusTimerRef.current);
      copyStatusTimerRef.current = null;
      terminalRef.current = null;
      fitAddonRef.current = null;
      terminal.dispose();
      releaseCspStyleNonce();
    };
  }, []);

  useEffect(() => {
    const terminal = terminalRef.current;
    if (!terminal) return;
    let cancelled = false;
    terminal.reset();
    transcriptRef.current = "";
    setFailure(null);
    setDroppedHeadBytes(0);

    const write = (text: string, stream: ShellOutputStream) => {
      if (!text) return;
      // Kept verbatim beside the terminal, because xterm's own buffer is not a transcript: it
      // pads the viewport with empty rows and trims every row's trailing spaces.
      transcriptRef.current = appendTranscript(transcriptRef.current, text);
      // The host reads both pipes on their own threads, so chunks arrive interleaved rather than
      // in the "all stdout, then stderr" order the model's copy of this output uses.
      terminal.write(stream === "stderr" ? `${STDERR_PREFIX}${text}${STDERR_SUFFIX}` : text);
    };

    const onEvent = (event: ShellOutputEvent) => {
      if (cancelled || event.type !== "output") return;
      write(event.text, event.stream);
    };

    const opened = openShellTaskOutput(task.conversationId, task.shellTaskId, onEvent);
    void opened
      .then((handle) => {
        if (cancelled) return;
        setDroppedHeadBytes(handle.droppedHeadBytes);
        // The snapshot is what the host still retains; it lost the pipe boundaries, so it is
        // replayed as plain output.
        write(handle.snapshot, "stdout");
      })
      .catch((error: unknown) => {
        if (cancelled) return;
        setFailure(errorMessage(error, t("无法读取命令输出", "Unable to read the command output")));
      });

    return () => {
      cancelled = true;
      // Detach the sink this subscription installed and no other: under StrictMode the effect
      // runs, cleans up, and runs again before either subscribe has answered, and a detach that
      // reached the host after the second subscribe would otherwise leave the page a snapshot
      // that never updates.
      void opened
        .then((handle) => detachShellTaskOutput(task.conversationId, task.shellTaskId, handle.subscriptionId))
        .catch(() => undefined);
    };
  }, [t, task.conversationId, task.shellTaskId]);

  const copyOutput = async () => {
    const terminal = terminalRef.current;
    if (!terminal) return;
    // A selection is what the user meant, even one that reads back as nothing but spaces;
    // without one the whole transcript is.
    const text = terminal.hasSelection() ? terminal.getSelection() : transcriptRef.current;
    const copied = await writeClipboardText(text);
    if (!terminalRef.current) return;
    setCopyStatus(copied ? "success" : "error");
    if (copyStatusTimerRef.current !== null) window.clearTimeout(copyStatusTimerRef.current);
    copyStatusTimerRef.current = window.setTimeout(() => {
      copyStatusTimerRef.current = null;
      setCopyStatus("idle");
    }, COPY_STATUS_MS);
  };

  useEffect(() => {
    if (!open) return;
    const host = hostRef.current;
    if (!host) return;
    const scheduleFit = () => {
      if (fitFrameRef.current !== null) return;
      fitFrameRef.current = window.requestAnimationFrame(() => {
        fitFrameRef.current = null;
        if (!openRef.current) return;
        try {
          fitAddonRef.current?.fit();
        } catch {
          // xterm cannot measure a box that has no usable size yet.
        }
      });
    };
    scheduleFit();
    const observer = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(scheduleFit);
    observer?.observe(host);
    window.addEventListener("resize", scheduleFit);
    return () => {
      observer?.disconnect();
      window.removeEventListener("resize", scheduleFit);
    };
  }, [open]);

  const running = task.outcome === null;
  const statusLabel = running
    ? task.stopping
      ? t("正在中止", "Stopping")
      : t("正在运行", "Running")
    : task.outcome === "stopped"
      ? t("已中止", "Stopped")
      : task.exitCode !== null && task.outcome === "failed"
        ? t("已失败（退出码 {code}）", "Failed (exit {code})", { code: task.exitCode })
        : task.outcome === "failed"
          ? t("已失败", "Failed")
          : t("已完成", "Finished");

  const copyLabel = copyStatus === "success"
    ? t("已复制", "Copied")
    : copyStatus === "error"
      ? t("复制失败", "Copy failed")
      : t("复制输出", "Copy output");

  return (
    <div className="shell-task-panel" id={shellTaskPanelId(task.shellTaskId)}>
      <header className="shell-task-panel__header">
        <code className="shell-task-panel__command" title={task.command}>{task.command}</code>
        <span
          className={`shell-task-panel__status shell-task-panel__status--${task.outcome ?? "running"}`}
          role="status"
          aria-live="polite"
        >
          {running && <LoaderCircle className="spin" size={12} aria-hidden="true" />}
          {statusLabel}
        </span>
        <IconButton
          label={copyLabel}
          className={`shell-task-panel__copy shell-task-panel__copy--${copyStatus}`}
          onClick={() => void copyOutput()}
        >
          {copyStatus === "success" ? <Check size={12} /> : <Copy size={12} />}
        </IconButton>
        {running && onStop && (
          <IconButton
            label={stopping
              ? t("正在中止命令", "Stopping the command")
              : t("中止命令", "Stop the command")}
            className="shell-task-panel__stop"
            disabled={stopping}
            onClick={onStop}
          >
            <Square size={9} fill="currentColor" />
          </IconButton>
        )}
      </header>
      {droppedHeadBytes > 0 && (
        <p className="shell-task-panel__truncation">
          {t(
            "输出过长，已省略开头 {bytes} 字节。模型收到的是开头那一段，这里显示的是结尾。",
            "Output too long; the first {bytes} bytes were dropped. The model received the beginning; this shows the end.",
            { bytes: droppedHeadBytes }
          )}
        </p>
      )}
      {failure && <p className="shell-task-panel__error" role="alert">{failure}</p>}
      <section
        className="shell-task-panel__viewport"
        ref={hostRef}
        aria-label={t("命令输出（只读）", "Command output (read-only)")}
      />
    </div>
  );
}
