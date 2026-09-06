import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, afterEach, describe, expect, it, vi } from "vitest";
import { ShellTaskPanel } from "./ShellTaskPanel";
import { CSP_STYLE_NONCE_CARRIER_ID } from "../lib/cspStyleNonce";
import type { ShellOutputEvent, ShellTaskSnapshot } from "../lib/shellTasks";

const shellApiMocks = vi.hoisted(() => ({
  openShellTaskOutput: vi.fn(),
  detachShellTaskOutput: vi.fn(),
  stopShellTask: vi.fn(),
  stopConversationTask: vi.fn(),
  listShellTasks: vi.fn()
}));

const xtermMocks = vi.hoisted(() => ({ terminals: [] as unknown[] }));

vi.mock("../lib/shellTasks", () => shellApiMocks);

vi.mock("@xterm/xterm", () => ({
  Terminal: class TerminalMock {
    options: { disableStdin: boolean; convertEol: boolean };
    /** What the terminal would report as selected; tests set it directly. */
    selection = "";
    /** Everything written, joined, so `selectAll` can hand back a whole transcript. */
    written: string[] = [];
    keyHandler: ((event: KeyboardEvent) => boolean) | null = null;
    write = vi.fn((text: string) => {
      this.written.push(text);
    });
    reset = vi.fn();
    dispose = vi.fn();
    loadAddon = vi.fn();
    open = vi.fn();
    hasSelection = vi.fn(() => this.selection.length > 0);
    getSelection = vi.fn(() => this.selection);
    selectAll = vi.fn(() => {
      this.selection = this.written.join("");
    });
    clearSelection = vi.fn(() => {
      this.selection = "";
    });
    attachCustomKeyEventHandler = vi.fn((handler: (event: KeyboardEvent) => boolean) => {
      this.keyHandler = handler;
    });

    constructor(options?: { disableStdin?: boolean; convertEol?: boolean }) {
      this.options = {
        disableStdin: Boolean(options?.disableStdin),
        convertEol: Boolean(options?.convertEol)
      };
      xtermMocks.terminals.push(this);
    }
  }
}));

vi.mock("@xterm/addon-fit", () => ({
  FitAddon: class FitAddonMock {
    fit = vi.fn();
  }
}));

interface TerminalMock {
  options: { disableStdin: boolean; convertEol: boolean };
  selection: string;
  written: string[];
  keyHandler: ((event: KeyboardEvent) => boolean) | null;
  write: ReturnType<typeof vi.fn>;
  reset: ReturnType<typeof vi.fn>;
  dispose: ReturnType<typeof vi.fn>;
  selectAll: ReturnType<typeof vi.fn>;
  clearSelection: ReturnType<typeof vi.fn>;
  hasSelection: ReturnType<typeof vi.fn>;
  getSelection: ReturnType<typeof vi.fn>;
}

function currentTerminal(): TerminalMock {
  return xtermMocks.terminals.at(-1) as TerminalMock;
}

function task(overrides: Partial<ShellTaskSnapshot> = {}): ShellTaskSnapshot {
  return {
    shellTaskId: "shell-1",
    conversationId: "conversation-1",
    toolName: "bash",
    command: "npm run build",
    stopping: false,
    startedAt: new Date().toISOString(),
    endedAt: null,
    outcome: null,
    exitCode: null,
    ...overrides
  };
}

/** Hands back the `onEvent` callback the panel registered, so a test can push output at it. */
function emit(): (event: ShellOutputEvent) => void {
  return shellApiMocks.openShellTaskOutput.mock.calls.at(-1)![2];
}

function keydown(key: string, modifiers: Partial<KeyboardEventInit> = {}): KeyboardEvent {
  return new KeyboardEvent("keydown", { key, cancelable: true, ...modifiers });
}

beforeEach(() => {
  xtermMocks.terminals.length = 0;
  shellApiMocks.openShellTaskOutput.mockReset();
  shellApiMocks.detachShellTaskOutput.mockReset();
  shellApiMocks.openShellTaskOutput.mockResolvedValue({
    snapshot: "",
    live: true,
    droppedHeadBytes: 0,
    subscriptionId: 7
  });
  shellApiMocks.detachShellTaskOutput.mockResolvedValue(true);

  const nonceCarrier = document.createElement("style");
  nonceCarrier.id = CSP_STYLE_NONCE_CARRIER_ID;
  nonceCarrier.nonce = "shell-task-panel-test-nonce";
  document.head.append(nonceCarrier);

  vi.stubGlobal("ResizeObserver", class ResizeObserverMock {
    observe() {}
    unobserve() {}
    disconnect() {}
  });
  vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => (
    window.setTimeout(() => callback(performance.now()), 0)
  ));
  vi.stubGlobal("cancelAnimationFrame", (handle: number) => window.clearTimeout(handle));
});

afterEach(() => {
  document.getElementById(CSP_STYLE_NONCE_CARRIER_ID)?.remove();
  vi.unstubAllGlobals();
});

describe("ShellTaskPanel", () => {
  /** There is no process on the other end, so the blunt xterm switch is the honest one. */
  it("never accepts input", async () => {
    render(<ShellTaskPanel task={task()} open />);

    await waitFor(() => expect(shellApiMocks.openShellTaskOutput).toHaveBeenCalled());
    expect(currentTerminal().options.disableStdin).toBe(true);
  });

  /**
   * These are pipes, not a pty: nothing turns a program's LF into CRLF on the way here, and to
   * xterm a bare LF only moves down a row. Without the conversion every line of a node or git
   * log starts where the previous one ended — a staircase.
   */
  it("treats a bare LF as a full line break", async () => {
    render(<ShellTaskPanel task={task()} open />);

    await waitFor(() => expect(shellApiMocks.openShellTaskOutput).toHaveBeenCalled());
    expect(currentTerminal().options.convertEol).toBe(true);
  });

  it("replays what the command printed before anyone opened the page", async () => {
    shellApiMocks.openShellTaskOutput.mockResolvedValue({
      snapshot: "compiling\n",
      live: true,
      droppedHeadBytes: 0,
      subscriptionId: 7
    });
    render(<ShellTaskPanel task={task()} open />);

    await waitFor(() => expect(currentTerminal().write).toHaveBeenCalledWith("compiling\n"));
  });

  /** The host hands over text it already decoded; the page draws it as it comes. */
  it("writes streamed output as it arrives", async () => {
    render(<ShellTaskPanel task={task()} open />);
    await waitFor(() => expect(shellApiMocks.openShellTaskOutput).toHaveBeenCalled());

    emit()({
      type: "output",
      shellTaskId: "shell-1",
      stream: "stdout",
      seq: 0,
      text: "构建 step 1\n"
    });

    expect(currentTerminal().write).toHaveBeenCalledWith("构建 step 1\n");
  });

  /** stderr is dimmed, not reddened: a warning on stderr is not a failure. */
  it("marks stderr apart from stdout", async () => {
    render(<ShellTaskPanel task={task()} open />);
    await waitFor(() => expect(shellApiMocks.openShellTaskOutput).toHaveBeenCalled());

    emit()({
      type: "output",
      shellTaskId: "shell-1",
      stream: "stderr",
      seq: 0,
      text: "warning\n"
    });

    expect(currentTerminal().write).toHaveBeenCalledWith("[2mwarning\n[0m");
  });

  it("ignores the end event and empty chunks rather than writing anything", async () => {
    render(<ShellTaskPanel task={task()} open />);
    await waitFor(() => expect(shellApiMocks.openShellTaskOutput).toHaveBeenCalled());
    const push = emit();

    push({ type: "output", shellTaskId: "shell-1", stream: "stdout", seq: 0, text: "" });
    push({ type: "end", shellTaskId: "shell-1", outcome: "succeeded", exitCode: 0 });

    expect(currentTerminal().write).not.toHaveBeenCalled();
  });

  /**
   * The page keeps the tail; the model's copy of the same command keeps the head. Presenting a
   * suffix as the whole output would be a quiet lie, so the page says so.
   */
  it("says so when the buffer dropped the beginning", async () => {
    shellApiMocks.openShellTaskOutput.mockResolvedValue({
      snapshot: "tail",
      live: false,
      droppedHeadBytes: 4096,
      subscriptionId: 0
    });
    render(<ShellTaskPanel task={task({ outcome: "succeeded", endedAt: new Date().toISOString() })} open />);

    expect(await screen.findByText(/已省略开头 4096 字节/)).toBeInTheDocument();
  });

  it("reports a subscription failure instead of showing an empty terminal", async () => {
    shellApiMocks.openShellTaskOutput.mockRejectedValue(new Error("该命令已不在任务列表中"));
    render(<ShellTaskPanel task={task()} open />);

    expect(await screen.findByRole("alert")).toHaveTextContent("该命令已不在任务列表中");
  });

  /**
   * The desktop runtime rejects with the host's raw string, not an `Error`. That string is the
   * reason, and it used to be replaced by the generic fallback.
   */
  it("shows the host's reason when the rejection is a plain string", async () => {
    shellApiMocks.openShellTaskOutput.mockRejectedValue("该命令已不在任务列表中");
    render(<ShellTaskPanel task={task()} open />);

    expect(await screen.findByRole("alert")).toHaveTextContent("该命令已不在任务列表中");
  });

  /**
   * xterm turns Ctrl+C into an interrupt and cancels the browser event, so it never copied. With
   * a selection the chord is declined so the browser copies; without one there is nothing to
   * copy and xterm may keep it.
   */
  it("lets the browser copy on Ctrl+C or Ctrl+Insert only while text is selected", async () => {
    render(<ShellTaskPanel task={task()} open />);
    await waitFor(() => expect(shellApiMocks.openShellTaskOutput).toHaveBeenCalled());
    const terminal = currentTerminal();
    const handle = terminal.keyHandler!;

    expect(handle(keydown("c", { ctrlKey: true }))).toBe(true);
    terminal.selection = "selected text";
    expect(handle(keydown("c", { ctrlKey: true }))).toBe(false);
    expect(handle(keydown("Insert", { ctrlKey: true }))).toBe(false);
    expect(handle(keydown("c", { metaKey: true }))).toBe(false);
    // Ctrl+Shift+C already reaches the browser through xterm; it is not this handler's business.
    expect(handle(keydown("C", { ctrlKey: true, shiftKey: true }))).toBe(true);
    expect(handle(new KeyboardEvent("keyup", { key: "c", ctrlKey: true }))).toBe(true);
  });

  it("selects everything on Ctrl+A instead of sending a control byte", async () => {
    render(<ShellTaskPanel task={task()} open />);
    await waitFor(() => expect(shellApiMocks.openShellTaskOutput).toHaveBeenCalled());
    const terminal = currentTerminal();
    const event = keydown("a", { ctrlKey: true });

    expect(terminal.keyHandler!(event)).toBe(false);
    expect(terminal.selectAll).toHaveBeenCalledTimes(1);
    expect(event.defaultPrevented).toBe(true);
  });

  it("copies the whole transcript from the header when nothing is selected", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    // user-event installs a clipboard of its own on setup, so the click is fired directly.
    vi.stubGlobal("navigator", { clipboard: { writeText } });
    render(<ShellTaskPanel task={task()} open />);
    await waitFor(() => expect(shellApiMocks.openShellTaskOutput).toHaveBeenCalled());
    const push = emit();
    push({ type: "output", shellTaskId: "shell-1", stream: "stdout", seq: 0, text: "line 1\nline 2\n" });
    push({ type: "output", shellTaskId: "shell-1", stream: "stderr", seq: 1, text: "  padded   \n" });

    fireEvent.click(screen.getByRole("button", { name: "复制输出" }));

    // The transcript, not xterm's buffer: that one pads the viewport with empty rows and trims
    // every row's trailing spaces, so reading it back would not reproduce the output.
    await waitFor(() => expect(writeText).toHaveBeenCalledWith("line 1\nline 2\n  padded   \n"));
    expect(currentTerminal().selectAll).not.toHaveBeenCalled();
    expect(await screen.findByRole("button", { name: "已复制" })).toBeInTheDocument();
  });

  /** A selection of nothing but spaces reads back empty; it is still what the user meant. */
  it("copies an all-whitespace selection rather than falling back to the transcript", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    vi.stubGlobal("navigator", { clipboard: { writeText } });
    render(<ShellTaskPanel task={task()} open />);
    await waitFor(() => expect(shellApiMocks.openShellTaskOutput).toHaveBeenCalled());
    emit()({ type: "output", shellTaskId: "shell-1", stream: "stdout", seq: 0, text: "secret\n" });
    const terminal = currentTerminal();
    terminal.hasSelection.mockReturnValue(true);
    terminal.getSelection.mockReturnValue("");

    fireEvent.click(screen.getByRole("button", { name: "复制输出" }));

    await waitFor(() => expect(writeText).toHaveBeenCalledWith(""));
  });

  it("copies just the selection from the header when there is one", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    vi.stubGlobal("navigator", { clipboard: { writeText } });
    render(<ShellTaskPanel task={task()} open />);
    await waitFor(() => expect(shellApiMocks.openShellTaskOutput).toHaveBeenCalled());
    currentTerminal().selection = "just this";

    fireEvent.click(screen.getByRole("button", { name: "复制输出" }));

    await waitFor(() => expect(writeText).toHaveBeenCalledWith("just this"));
    expect(currentTerminal().selectAll).not.toHaveBeenCalled();
  });

  it("offers a stop control only while the command runs", async () => {
    const onStop = vi.fn();
    const user = userEvent.setup();
    const { rerender } = render(<ShellTaskPanel task={task()} open onStop={onStop} />);

    await user.click(await screen.findByRole("button", { name: "中止命令" }));
    expect(onStop).toHaveBeenCalledTimes(1);

    rerender(
      <ShellTaskPanel
        task={task({ outcome: "succeeded", exitCode: 0, endedAt: new Date().toISOString() })}
        open
        onStop={onStop}
      />
    );
    expect(screen.queryByRole("button", { name: "中止命令" })).not.toBeInTheDocument();
    expect(screen.getByRole("status")).toHaveTextContent("已完成");
  });

  it("reports a failing exit code in the status", async () => {
    render(
      <ShellTaskPanel
        task={task({ outcome: "failed", exitCode: 2, endedAt: new Date().toISOString() })}
        open
      />
    );

    expect(await screen.findByRole("status")).toHaveTextContent("已失败（退出码 2）");
  });

  /**
   * A page that navigated away must not keep receiving a whole build's output. It detaches the
   * sink its own subscription installed — named by the id the host handed back — so a detach
   * that lands after a newer subscription from a remounted page cannot silence that one.
   */
  it("detaches its own subscription when it goes away", async () => {
    const { unmount } = render(<ShellTaskPanel task={task()} open />);
    await waitFor(() => expect(shellApiMocks.openShellTaskOutput).toHaveBeenCalled());

    unmount();

    await waitFor(() => (
      expect(shellApiMocks.detachShellTaskOutput).toHaveBeenCalledWith("conversation-1", "shell-1", 7)
    ));
  });
});
