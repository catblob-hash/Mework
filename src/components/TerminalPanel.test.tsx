import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  TerminalPanel,
  terminalPanelId,
  terminalUiColors
} from "./TerminalPanel";
import type { OpenTerminalResult, TerminalEvent } from "../lib/terminal";
import { CSP_STYLE_NONCE_CARRIER_ID } from "../lib/cspStyleNonce";

const terminalApiMocks = vi.hoisted(() => ({
  openTerminal: vi.fn(),
  writeTerminal: vi.fn(),
  resizeTerminal: vi.fn(),
  detachTerminal: vi.fn(),
  closeTerminal: vi.fn()
}));

const xtermMocks = vi.hoisted(() => ({
  terminals: [] as unknown[],
  fitAddons: [] as unknown[]
}));

vi.mock("../lib/terminal", () => terminalApiMocks);

vi.mock("@xterm/xterm", () => ({
  Terminal: class TerminalMock {
    cols = 80;
    rows = 24;
    options = { disableStdin: false };
    textarea = { readOnly: false };
    write = vi.fn();
    reset = vi.fn();
    clear = vi.fn();
    focus = vi.fn();
    dispose = vi.fn();
    loadAddon = vi.fn();
    open = vi.fn((host: HTMLElement) => {
      const runtimeStyle = host.ownerDocument.createElement("style");
      runtimeStyle.dataset.xtermRuntimeStyle = "true";
      runtimeStyle.textContent = ".xterm { display: block; }";
      host.append(runtimeStyle);
    });
    dataHandlers = new Set<(data: string) => void>();
    resizeHandlers = new Set<(size: { cols: number; rows: number }) => void>();
    keyEventHandler: ((event: KeyboardEvent) => boolean) | null = null;
    /** What the terminal would report as selected; tests set it directly. */
    selection = "";
    hasSelection = vi.fn(() => this.selection.length > 0);

    constructor(options?: { disableStdin?: boolean }) {
      this.options.disableStdin = Boolean(options?.disableStdin);
      xtermMocks.terminals.push(this);
    }

    onData(handler: (data: string) => void) {
      this.dataHandlers.add(handler);
      return { dispose: () => this.dataHandlers.delete(handler) };
    }

    onResize(handler: (size: { cols: number; rows: number }) => void) {
      this.resizeHandlers.add(handler);
      return { dispose: () => this.resizeHandlers.delete(handler) };
    }

    attachCustomKeyEventHandler(handler: (event: KeyboardEvent) => boolean) {
      this.keyEventHandler = handler;
    }

    emitData(data: string) {
      // Typed input reaches xterm as a keydown first; the gate sees that event.
      const keydown = new KeyboardEvent("keydown", { key: data });
      if (this.textarea.readOnly || this.keyEventHandler?.(keydown) === false) return;
      for (const handler of this.dataHandlers) handler(data);
    }

    emitProtocolData(data: string) {
      for (const handler of this.dataHandlers) handler(data);
    }

    emitResize(cols: number, rows: number) {
      this.cols = cols;
      this.rows = rows;
      for (const handler of this.resizeHandlers) handler({ cols, rows });
    }
  }
}));

vi.mock("@xterm/addon-fit", () => ({
  FitAddon: class FitAddonMock {
    fit = vi.fn();

    constructor() {
      xtermMocks.fitAddons.push(this);
    }
  }
}));

interface TerminalMock {
  cols: number;
  rows: number;
  options: { disableStdin: boolean };
  textarea: { readOnly: boolean };
  selection: string;
  keyEventHandler: ((event: KeyboardEvent) => boolean) | null;
  write: ReturnType<typeof vi.fn>;
  reset: ReturnType<typeof vi.fn>;
  clear: ReturnType<typeof vi.fn>;
  focus: ReturnType<typeof vi.fn>;
  dispose: ReturnType<typeof vi.fn>;
  emitData: (data: string) => void;
  emitProtocolData: (data: string) => void;
  emitResize: (cols: number, rows: number) => void;
}

function currentTerminal(): TerminalMock {
  return xtermMocks.terminals.at(-1) as TerminalMock;
}

function response(overrides: Partial<OpenTerminalResult> = {}): OpenTerminalResult {
  return {
    sessionId: "session-1",
    created: true,
    running: true,
    ready: true,
    cwd: "C:/workspace",
    shell: "PowerShell",
    snapshot: [],
    commandState: {
      revision: 0,
      status: "idle",
      commandId: null,
      commandCount: 0
    },
    ...overrides
  };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

describe("TerminalPanel", () => {
  it("uses strict RGB inverses for the night terminal UI while preserving alpha", () => {
    expect(terminalUiColors("day")).toEqual({
      background: "#171817",
      foreground: "#d8ddd8",
      cursor: "#d8ddd8",
      selectionBackground: "#506a8c88"
    });
    expect(terminalUiColors("night")).toEqual({
      background: "#e8e7e8",
      foreground: "#272227",
      cursor: "#272227",
      selectionBackground: "#af957388"
    });
  });

  beforeEach(() => {
    terminalApiMocks.openTerminal.mockReset().mockResolvedValue(response());
    terminalApiMocks.writeTerminal.mockReset().mockResolvedValue(undefined);
    terminalApiMocks.resizeTerminal.mockReset().mockResolvedValue(undefined);
    terminalApiMocks.detachTerminal.mockReset().mockResolvedValue(undefined);
    terminalApiMocks.closeTerminal.mockReset().mockResolvedValue(undefined);
    xtermMocks.terminals.length = 0;
    xtermMocks.fitAddons.length = 0;

    const nonceCarrier = document.createElement("style");
    nonceCarrier.id = CSP_STYLE_NONCE_CARRIER_ID;
    nonceCarrier.nonce = "terminal-panel-test-nonce";
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

  it("gives every xterm runtime stylesheet the document CSP nonce", async () => {
    const { container } = render(
      <TerminalPanel conversationId="conversation-1" terminalId="terminal-1" label="终端 1" open />
    );
    await screen.findByText("空终端");

    const styles = container.querySelectorAll<HTMLStyleElement>("style[data-xterm-runtime-style]");
    expect(styles).toHaveLength(1);
    expect([...styles].every((style) => style.nonce === "terminal-panel-test-nonce")).toBe(true);
  });

  it("stays mounted while collapsed and only starts when first opened", async () => {
    const { container, rerender, unmount } = render(
      <TerminalPanel conversationId="conversation-1" terminalId="terminal-1" label="终端 1" open={false} />
    );
    const region = container.querySelector<HTMLElement>("#conversation-terminal-conversation-1-terminal-1")!;

    expect(terminalPanelId("conversation-1", "terminal-1")).toBe("conversation-terminal-conversation-1-terminal-1");
    expect(region).toHaveClass("collapse-region--closed");
    expect(region).toHaveAttribute("aria-hidden", "true");
    expect(region).toHaveAttribute("inert");
    expect(container.querySelector(".terminal-panel__viewport")).toBeInTheDocument();
    expect(terminalApiMocks.openTerminal).not.toHaveBeenCalled();

    rerender(<TerminalPanel conversationId="conversation-1" terminalId="terminal-1" label="终端 1" open />);
    await waitFor(() => expect(terminalApiMocks.openTerminal).toHaveBeenCalledOnce());
    expect(region).not.toHaveClass("collapse-region--closed");
    expect(await screen.findByText("空终端")).toBeInTheDocument();

    rerender(<TerminalPanel conversationId="conversation-1" terminalId="terminal-1" label="终端 1" open={false} />);
    expect(region).toHaveClass("collapse-region--closed");
    expect(terminalApiMocks.detachTerminal).not.toHaveBeenCalled();
    expect(currentTerminal().dispose).not.toHaveBeenCalled();

    unmount();
    expect(terminalApiMocks.detachTerminal).toHaveBeenCalledWith("conversation-1", "terminal-1", "session-1");
    expect(currentTerminal().dispose).toHaveBeenCalledOnce();
    expect(terminalApiMocks.closeTerminal).not.toHaveBeenCalled();
  });

  it("writes the snapshot before queued events and ignores events from another session", async () => {
    const opening = deferred<OpenTerminalResult>();
    let onEvent: ((event: TerminalEvent) => void) | null = null;
    terminalApiMocks.openTerminal.mockImplementation((
      _conversationId: string,
      _terminalId: string,
      _cols: number,
      _rows: number,
      handler: (event: TerminalEvent) => void
    ) => {
      onEvent = handler;
      return opening.promise;
    });
    render(<TerminalPanel conversationId="conversation-1" terminalId="terminal-1" label="终端 1" open />);
    await waitFor(() => expect(onEvent).not.toBeNull());

    act(() => {
      onEvent?.({
        type: "command_state",
        sessionId: "session-1",
        commandState: {
          revision: 2,
          status: "running",
          commandId: "command-1",
          commandCount: 1
        }
      });
      onEvent?.({ type: "output", sessionId: "session-1", data: [66] });
      onEvent?.({ type: "output", sessionId: "old-session", data: [88] });
    });
    await act(async () => {
      opening.resolve(response({
        snapshot: [65],
        commandState: {
          revision: 1,
          status: "idle",
          commandId: null,
          commandCount: 0
        }
      }));
      await opening.promise;
    });

    await waitFor(() => expect(currentTerminal().write).toHaveBeenCalledTimes(2));
    expect(currentTerminal().write.mock.calls.map((call) => call[0])).toEqual(["A", "B"]);
    expect(screen.getByText("PowerShell · C:/workspace")).toBeInTheDocument();
    expect(screen.getByText("运行中")).toBeInTheDocument();
  });

  it("scopes input and resize operations to the active session", async () => {
    render(<TerminalPanel conversationId="conversation-1" terminalId="terminal-1" label="终端 1" open />);
    await screen.findByText("空终端");
    const terminal = currentTerminal();

    act(() => terminal.emitData("pwd\r"));
    expect(screen.getByText("运行中")).toBeInTheDocument();
    act(() => terminal.emitResize(118, 34));

    await waitFor(() => {
      expect(terminalApiMocks.writeTerminal).toHaveBeenCalledWith(
        "conversation-1",
        "terminal-1",
        "session-1",
        "pwd\r"
      );
      expect(terminalApiMocks.resizeTerminal).toHaveBeenCalledWith(
        "conversation-1",
        "terminal-1",
        "session-1",
        118,
        34
      );
    });
  });

  it("uses the open response command state even when its snapshot looks like a prompt", async () => {
    terminalApiMocks.openTerminal.mockResolvedValue(response({
      snapshot: Array.from(new TextEncoder().encode("\r\nPS C:\\workspace> ")),
      commandState: {
        revision: 8,
        status: "running",
        commandId: "command-8",
        commandCount: 3
      }
    }));
    const onStateChange = vi.fn();

    render(
      <TerminalPanel
        conversationId="conversation-1"
        terminalId="terminal-1"
        label="终端 1"
        open
        onStateChange={onStateChange}
      />
    );

    expect(await screen.findByText("运行中")).toBeInTheDocument();
    expect(onStateChange).toHaveBeenLastCalledWith(expect.objectContaining({
      busy: true,
      hasHistory: true
    }));
  });

  /**
   * xterm turns Ctrl+C into ETX and cancels the browser event, so it never copied. Over a
   * selection the chord is declined so the browser copies it — even while input is gated,
   * because reading a transcript is not input — and without one it still interrupts.
   */
  it("lets Ctrl+C copy a selection and interrupt otherwise", async () => {
    const { rerender } = render(
      <TerminalPanel
        conversationId="conversation-1"
        terminalId="terminal-1"
        label="终端 1"
        open
        inputDisabledReason="Git 写操作进行中"
      />
    );
    await screen.findByText("Git 写操作进行中");
    const terminal = currentTerminal();
    const gate = terminal.keyEventHandler!;
    const ctrlC = () => new KeyboardEvent("keydown", { key: "c", ctrlKey: true });

    // Gated and nothing selected: the chord is dropped like every other key.
    expect(gate(ctrlC())).toBe(false);
    terminal.selection = "some output";
    expect(gate(ctrlC())).toBe(false);
    expect(gate(new KeyboardEvent("keydown", { key: "Insert", ctrlKey: true }))).toBe(false);

    rerender(
      <TerminalPanel conversationId="conversation-1" terminalId="terminal-1" label="终端 1" open />
    );
    await waitFor(() => expect(terminal.textarea.readOnly).toBe(false));
    // Open and selected: still the browser's copy, not an interrupt to the shell.
    expect(gate(ctrlC())).toBe(false);
    // Open and nothing selected: xterm keeps it and sends the interrupt.
    terminal.selection = "";
    expect(gate(ctrlC())).toBe(true);
    // Ctrl+Shift+C is not the chord; xterm passes it to the browser on its own.
    terminal.selection = "some output";
    expect(gate(new KeyboardEvent("keydown", { key: "C", ctrlKey: true, shiftKey: true }))).toBe(true);
  });

  it("blocks terminal input while a Git mutation owns the workspace", async () => {
    const onCommandStart = vi.fn(() => true);
    const { container, rerender } = render(
      <TerminalPanel
        conversationId="conversation-1"
        terminalId="terminal-1"
        label="终端 1"
        open
        inputDisabledReason="Git 写操作进行中"
        onCommandStart={onCommandStart}
      />
    );
    await screen.findByText("Git 写操作进行中");
    const terminal = currentTerminal();
    expect(terminal.options.disableStdin).toBe(false);
    expect(terminal.textarea.readOnly).toBe(true);
    expect(container.querySelector(".terminal-panel__viewport")).toHaveAttribute("aria-disabled", "true");

    act(() => terminal.emitData("git status\r"));
    expect(onCommandStart).not.toHaveBeenCalled();
    expect(terminalApiMocks.writeTerminal).not.toHaveBeenCalled();

    rerender(
      <TerminalPanel
        conversationId="conversation-1"
        terminalId="terminal-1"
        label="终端 1"
        open
        onCommandStart={onCommandStart}
      />
    );
    await waitFor(() => expect(terminal.textarea.readOnly).toBe(false));
    act(() => terminal.emitData("git status\r"));
    expect(onCommandStart).toHaveBeenCalledOnce();
    await waitFor(() => expect(terminalApiMocks.writeTerminal).toHaveBeenCalledWith(
      "conversation-1",
      "terminal-1",
      "session-1",
      "git status\r"
    ));
  });

  it("relays terminal protocol replies before the trusted shell handshake is ready", async () => {
    const handlers: Array<(event: TerminalEvent) => void> = [];
    terminalApiMocks.openTerminal.mockImplementation((
      _conversationId: string,
      _terminalId: string,
      _cols: number,
      _rows: number,
      handler: (event: TerminalEvent) => void
    ) => {
      handlers.push(handler);
      return Promise.resolve(response({ ready: false }));
    });
    render(
      <TerminalPanel
        conversationId="conversation-1"
        terminalId="terminal-1"
        label="终端 1"
        open
      />
    );

    await screen.findByText("正在连接");
    const terminal = currentTerminal();
    expect(terminal.options.disableStdin).toBe(false);
    expect(terminal.textarea.readOnly).toBe(true);

    act(() => terminal.emitProtocolData("\u001b[1;1R"));
    await waitFor(() => expect(terminalApiMocks.writeTerminal).toHaveBeenCalledWith(
      "conversation-1",
      "terminal-1",
      "session-1",
      "\u001b[1;1R"
    ));
    act(() => terminal.emitData("git status\r"));
    expect(terminalApiMocks.writeTerminal).toHaveBeenCalledTimes(1);

    act(() => handlers[0]({ type: "ready", sessionId: "session-1" }));
    expect(await screen.findByText("空终端")).toBeInTheDocument();
    expect(terminal.textarea.readOnly).toBe(false);
  });

  it("does not send a command when the workspace lock loses the start race", async () => {
    const onCommandStart = vi.fn(() => false);
    render(
      <TerminalPanel
        conversationId="conversation-1"
        terminalId="terminal-1"
        label="终端 1"
        open
        onCommandStart={onCommandStart}
      />
    );
    await screen.findByText("空终端");
    act(() => currentTerminal().emitData("git status\r"));
    expect(onCommandStart).toHaveBeenCalledOnce();
    expect(terminalApiMocks.writeTerminal).not.toHaveBeenCalled();
    expect(screen.getByText("空终端")).toBeInTheDocument();
  });

  it("does not let a stale open response detach a session reused by a newer attempt", async () => {
    const firstOpening = deferred<OpenTerminalResult>();
    const secondOpening = deferred<OpenTerminalResult>();
    terminalApiMocks.openTerminal
      .mockImplementationOnce(() => firstOpening.promise)
      .mockImplementationOnce(() => secondOpening.promise);
    const { rerender } = render(<TerminalPanel conversationId="conversation-1" terminalId="terminal-1" label="终端 1" open />);
    await waitFor(() => expect(terminalApiMocks.openTerminal).toHaveBeenCalledOnce());

    rerender(<TerminalPanel conversationId="conversation-2" terminalId="terminal-2" label="终端 2" open />);
    await waitFor(() => expect(terminalApiMocks.openTerminal).toHaveBeenCalledTimes(2));
    await act(async () => {
      secondOpening.resolve(response({ sessionId: "shared-session" }));
      await secondOpening.promise;
    });
    await screen.findByText("空终端");

    await act(async () => {
      firstOpening.resolve(response({ sessionId: "shared-session" }));
      await firstOpening.promise;
    });

    expect(terminalApiMocks.detachTerminal).not.toHaveBeenCalled();
  });

  it("restores the authoritative idle state after a command finishes while detached", async () => {
    terminalApiMocks.openTerminal
      .mockResolvedValueOnce(response({
        commandState: {
          revision: 3,
          status: "running",
          commandId: "command-3",
          commandCount: 1
        }
      }))
      .mockResolvedValueOnce(response({
        created: false,
        snapshot: Array.from(new TextEncoder().encode("\r\nfinished without a recognized prompt")),
        commandState: {
          revision: 4,
          status: "idle",
          commandId: null,
          commandCount: 1
        }
      }));

    const first = render(
      <TerminalPanel conversationId="conversation-1" terminalId="terminal-1" label="终端 1" open />
    );
    await screen.findByText("运行中");
    first.unmount();
    expect(terminalApiMocks.detachTerminal).toHaveBeenCalledWith(
      "conversation-1",
      "terminal-1",
      "session-1"
    );

    render(
      <TerminalPanel
        conversationId="conversation-1"
        terminalId="terminal-1"
        label="终端 1"
        open
        initialState={{
          terminalId: "terminal-1",
          conversationId: "conversation-1",
          label: "终端 1",
          phase: "running",
          busy: true,
          hasHistory: true,
          cwd: "C:/workspace",
          shell: "PowerShell",
          sessionId: "session-1"
        }}
      />
    );

    expect(await screen.findByText("待机")).toBeInTheDocument();
  });

  it("clears an active command when the shell exits", async () => {
    const handlers: Array<(event: TerminalEvent) => void> = [];
    const onStateChange = vi.fn();
    terminalApiMocks.openTerminal.mockImplementation((
      _conversationId: string,
      _terminalId: string,
      _cols: number,
      _rows: number,
      handler: (event: TerminalEvent) => void
    ) => {
      handlers.push(handler);
      return Promise.resolve(response({
        commandState: {
          revision: 1,
          status: "running",
          commandId: "command-1",
          commandCount: 1
        }
      }));
    });
    render(
      <TerminalPanel
        conversationId="conversation-1"
        terminalId="terminal-1"
        label="终端 1"
        open
        onStateChange={onStateChange}
      />
    );
    await screen.findByText("运行中");

    act(() => handlers[0]({ type: "exit", sessionId: "session-1", exitCode: 0 }));

    expect(screen.getByText("已退出（代码 0）")).toBeInTheDocument();
    expect(onStateChange).toHaveBeenLastCalledWith(expect.objectContaining({
      phase: "exited",
      busy: false,
      hasHistory: true
    }));
  });

  it("shows streamed terminal errors and only reconnects when the user asks", async () => {
    const handlers: Array<(event: TerminalEvent) => void> = [];
    const onStateChange = vi.fn();
    terminalApiMocks.openTerminal.mockImplementation((
      _conversationId: string,
      _terminalId: string,
      _cols: number,
      _rows: number,
      handler: (event: TerminalEvent) => void
    ) => {
      handlers.push(handler);
      return Promise.resolve(response({ sessionId: `session-${handlers.length}` }));
    });
    render(
      <TerminalPanel
        conversationId="conversation-1"
        terminalId="terminal-1"
        label="终端 1"
        open
        onStateChange={onStateChange}
      />
    );
    await screen.findByText("空终端");

    act(() => {
      handlers[0]({
        type: "command_state",
        sessionId: "session-1",
        commandState: {
          revision: 1,
          status: "running",
          commandId: "command-1",
          commandCount: 1
        }
      });
      handlers[0]({ type: "error", sessionId: "session-1", message: "PTY 读取失败" });
    });
    expect(screen.getByRole("alert")).toHaveTextContent("PTY 读取失败");
    expect(onStateChange).toHaveBeenLastCalledWith(expect.objectContaining({
      phase: "error",
      busy: true,
      hasHistory: true
    }));

    // No `onClose`, so the header offers nothing to close — the control is opt-in.
    expect(screen.queryByRole("button", { name: "关闭终端" })).not.toBeInTheDocument();
    expect(terminalApiMocks.openTerminal).toHaveBeenCalledOnce();

    // A dead session offers a way back, but only a click may spend it: reconnecting
    // on its own would erase the very failure the alert is reporting.
    fireEvent.click(screen.getByRole("button", { name: "重试" }));
    await waitFor(() => expect(terminalApiMocks.openTerminal).toHaveBeenCalledTimes(2));
  });

  /**
   * A killed session sends no exit event — the host drops its sink before the shell
   * dies — so the panel used to keep saying "idle" with a close button that did
   * nothing. The panel now settles itself on the host's word.
   */
  it("closes from the header on the host's confirmation and comes back fresh when reopened", async () => {
    const closing = deferred<void>();
    const onClose = vi.fn(() => closing.promise);
    const onStateChange = vi.fn();
    terminalApiMocks.openTerminal
      .mockResolvedValueOnce(response({ sessionId: "session-1" }))
      .mockResolvedValueOnce(response({ sessionId: "session-2" }));
    const view = render(
      <TerminalPanel
        conversationId="conversation-1"
        terminalId="terminal-1"
        label="终端 1"
        open
        onClose={onClose}
        onStateChange={onStateChange}
      />
    );
    await screen.findByText("空终端");
    const terminal = currentTerminal();
    act(() => terminal.emitData("Get-Location\r"));
    expect(screen.getByText("运行中")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "重试" })).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "关闭终端" }));
    expect(onClose).toHaveBeenCalledOnce();
    // Closing is the host's job; the panel only reports the request and waits.
    expect(terminalApiMocks.closeTerminal).not.toHaveBeenCalled();
    expect(await screen.findByText("正在终止")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "关闭终端" })).toBeDisabled();
    expect(terminal.textarea.readOnly).toBe(true);
    // A second click while the first close is pending must not ask the host twice.
    fireEvent.click(screen.getByRole("button", { name: "关闭终端" }));
    expect(onClose).toHaveBeenCalledOnce();

    await act(async () => {
      closing.resolve();
      await closing.promise;
    });
    expect(await screen.findByText("已退出")).toBeInTheDocument();
    expect(onStateChange).toHaveBeenLastCalledWith(expect.objectContaining({
      phase: "exited",
      busy: false,
      sessionId: null
    }));
    expect(screen.getByRole("button", { name: "重试" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "关闭终端" })).toBeEnabled();
    // The killed session is gone at the host too, so folding the drawer must not detach it.
    view.rerender(
      <TerminalPanel
        conversationId="conversation-1"
        terminalId="terminal-1"
        label="终端 1"
        open={false}
        onClose={onClose}
        onStateChange={onStateChange}
      />
    );
    expect(terminalApiMocks.detachTerminal).not.toHaveBeenCalled();
    expect(terminalApiMocks.openTerminal).toHaveBeenCalledOnce();

    // Expanding the drawer again is the user asking for a terminal: start one.
    view.rerender(
      <TerminalPanel
        conversationId="conversation-1"
        terminalId="terminal-1"
        label="终端 1"
        open
        onClose={onClose}
        onStateChange={onStateChange}
      />
    );
    await waitFor(() => expect(terminalApiMocks.openTerminal).toHaveBeenCalledTimes(2));
    expect(await screen.findByText("空终端")).toBeInTheDocument();
    expect(onStateChange).toHaveBeenLastCalledWith(expect.objectContaining({
      phase: "running",
      sessionId: "session-2"
    }));
  });

  it("reports a refused close in place and keeps the session", async () => {
    const onClose = vi.fn(() => Promise.reject(new Error("终端仍在被 Git 操作使用")));
    const onStateChange = vi.fn();
    const { container } = render(
      <TerminalPanel
        conversationId="conversation-1"
        terminalId="terminal-1"
        label="终端 1"
        open
        onClose={onClose}
        onStateChange={onStateChange}
      />
    );
    await screen.findByText("空终端");

    fireEvent.click(screen.getByRole("button", { name: "关闭终端" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("终端仍在被 Git 操作使用");
    expect(screen.getByRole("button", { name: "重试" })).toBeInTheDocument();
    // The failure stays until the user acts on it: no reconnect on its own.
    expect(terminalApiMocks.openTerminal).toHaveBeenCalledOnce();
    // The shell is still up at the host, so the panel keeps its id: the retry
    // below must release that attachment before reattaching, not leak it.
    expect(container.querySelector(".terminal-panel")).toHaveAttribute("data-session-id", "session-1");
    expect(onStateChange).toHaveBeenLastCalledWith(expect.objectContaining({
      phase: "error",
      sessionId: "session-1"
    }));

    fireEvent.click(screen.getByRole("button", { name: "重试" }));
    await waitFor(() => expect(terminalApiMocks.openTerminal).toHaveBeenCalledTimes(2));
    expect(terminalApiMocks.detachTerminal).toHaveBeenCalledWith("conversation-1", "terminal-1", "session-1");
    expect(await screen.findByText("空终端")).toBeInTheDocument();
  });

  it("lets a close wait for the open it races so the host has a session to kill", async () => {
    const opening = deferred<OpenTerminalResult>();
    let onEvent: ((event: TerminalEvent) => void) | null = null;
    terminalApiMocks.openTerminal.mockImplementation((
      _conversationId: string,
      _terminalId: string,
      _cols: number,
      _rows: number,
      handler: (event: TerminalEvent) => void
    ) => {
      onEvent = handler;
      return opening.promise;
    });
    const closing = deferred<void>();
    const onClose = vi.fn(() => closing.promise);
    const onCommandStart = vi.fn(() => true);
    const onStateChange = vi.fn();
    render(
      <TerminalPanel
        conversationId="conversation-1"
        terminalId="terminal-1"
        label="终端 1"
        open
        onClose={onClose}
        onCommandStart={onCommandStart}
        onStateChange={onStateChange}
      />
    );
    await screen.findByText("正在连接");
    const terminal = currentTerminal();

    fireEvent.click(screen.getByRole("button", { name: "关闭终端" }));
    expect(await screen.findByText("正在终止")).toBeInTheDocument();
    onStateChange.mockClear();
    await Promise.resolve();
    expect(onClose).not.toHaveBeenCalled();

    // The open lands mid-close and reports a ready shell: the phase and the
    // input gate both stay shut, so nothing is typed into a shell being killed.
    await act(async () => {
      opening.resolve(response({ ready: true }));
      await opening.promise;
    });
    await waitFor(() => expect(onClose).toHaveBeenCalledOnce());
    expect(screen.getByText("正在终止")).toBeInTheDocument();
    expect(terminal.textarea.readOnly).toBe(true);
    expect(terminal.keyEventHandler!(new KeyboardEvent("keydown", { key: "a" }))).toBe(false);
    act(() => terminal.emitProtocolData("dir\r"));
    expect(onCommandStart).not.toHaveBeenCalled();
    expect(terminalApiMocks.writeTerminal).not.toHaveBeenCalled();
    // The host's one-shot ready handshake arriving now must not reopen the gate either.
    act(() => onEvent?.({ type: "ready", sessionId: "session-1" }));
    expect(terminal.textarea.readOnly).toBe(true);
    expect(screen.getByText("正在终止")).toBeInTheDocument();

    await act(async () => {
      closing.resolve();
      await closing.promise;
    });
    expect(await screen.findByText("已退出")).toBeInTheDocument();
    const phases = onStateChange.mock.calls.map((call) => (call[0] as { phase: string }).phase);
    expect(phases).not.toContain("running");
    expect(phases.at(-1)).toBe("exited");
  });

  it("keeps the exit code a shell reports while it is being closed", async () => {
    let onEvent: ((event: TerminalEvent) => void) | null = null;
    terminalApiMocks.openTerminal.mockImplementation((
      _conversationId: string,
      _terminalId: string,
      _cols: number,
      _rows: number,
      handler: (event: TerminalEvent) => void
    ) => {
      onEvent = handler;
      return Promise.resolve(response());
    });
    const closing = deferred<void>();
    const onClose = vi.fn(() => closing.promise);
    render(
      <TerminalPanel
        conversationId="conversation-1"
        terminalId="terminal-1"
        label="终端 1"
        open
        onClose={onClose}
      />
    );
    await screen.findByText("空终端");

    fireEvent.click(screen.getByRole("button", { name: "关闭终端" }));
    expect(await screen.findByText("正在终止")).toBeInTheDocument();
    // The user had typed `exit` just before: the shell's own exit lands first.
    act(() => onEvent?.({ type: "exit", sessionId: "session-1", exitCode: 0 }));
    // Still the close's phase — no Retry re-armed, X still held — until the host answers.
    expect(screen.getByText("正在终止")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "重试" })).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "关闭终端" })).toBeDisabled();

    await act(async () => {
      closing.resolve();
      await closing.promise;
    });
    expect(await screen.findByText("已退出（代码 0）")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "重试" })).toBeInTheDocument();
  });

  it("dismisses a session that already exited without erasing its verdict", async () => {
    const handlers: Array<(event: TerminalEvent) => void> = [];
    terminalApiMocks.openTerminal.mockImplementation((
      _conversationId: string,
      _terminalId: string,
      _cols: number,
      _rows: number,
      handler: (event: TerminalEvent) => void
    ) => {
      handlers.push(handler);
      return Promise.resolve(response());
    });
    const onClose = vi.fn(() => Promise.resolve());
    render(
      <TerminalPanel
        conversationId="conversation-1"
        terminalId="terminal-1"
        label="终端 1"
        open
        onClose={onClose}
      />
    );
    await screen.findByText("空终端");
    act(() => handlers[0]({ type: "exit", sessionId: "session-1", exitCode: 0 }));
    expect(screen.getByText("已退出（代码 0）")).toBeInTheDocument();

    // The shell is gone; the close still runs so the host can drop its record and
    // fold the drawer, and what the shell reported stays on screen meanwhile.
    fireEvent.click(screen.getByRole("button", { name: "关闭终端" }));
    await waitFor(() => expect(onClose).toHaveBeenCalledOnce());
    expect(await screen.findByText("已退出（代码 0）")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "关闭终端" })).toBeEnabled();
  });

  /**
   * Switching conversation detaches the panel from a shell that may still be
   * running a command. Nothing would ever clear a busy flag left behind, so the
   * last report for the old conversation says the renderer no longer sees it.
   */
  it("reports the terminal it leaves behind as no longer observed", async () => {
    const onStateChange = vi.fn();
    terminalApiMocks.openTerminal
      .mockResolvedValueOnce(response({
        sessionId: "session-a",
        commandState: { revision: 1, status: "running", commandId: "command-1", commandCount: 1 }
      }))
      .mockResolvedValueOnce(response({ sessionId: "session-b" }));
    const { rerender } = render(
      <TerminalPanel
        conversationId="conversation-a"
        terminalId="composer"
        label="终端"
        open
        onStateChange={onStateChange}
      />
    );
    await screen.findByText("运行中");
    expect(onStateChange).toHaveBeenLastCalledWith(expect.objectContaining({
      conversationId: "conversation-a",
      phase: "running",
      busy: true
    }));

    rerender(
      <TerminalPanel
        conversationId="conversation-b"
        terminalId="composer"
        label="终端"
        open
        onStateChange={onStateChange}
      />
    );
    expect(terminalApiMocks.detachTerminal).toHaveBeenCalledWith("conversation-a", "composer", "session-a");
    const leaving = onStateChange.mock.calls
      .map((call) => call[0] as { conversationId: string; phase: string; busy: boolean; hasHistory: boolean })
      .filter((state) => state.conversationId === "conversation-a")
      .at(-1);
    expect(leaving).toMatchObject({ phase: "idle", busy: false, hasHistory: true });
    await screen.findByText("空终端");
    expect(onStateChange).toHaveBeenLastCalledWith(expect.objectContaining({
      conversationId: "conversation-b",
      phase: "running",
      sessionId: "session-b"
    }));
  });

  it("derives command history and busy state only from authoritative lifecycle events", async () => {
    const onStateChange = vi.fn();
    let onEvent: ((event: TerminalEvent) => void) | null = null;
    terminalApiMocks.openTerminal.mockImplementation((
      _conversationId: string,
      _terminalId: string,
      _cols: number,
      _rows: number,
      handler: (event: TerminalEvent) => void
    ) => {
      onEvent = handler;
      return Promise.resolve(response());
    });
    render(
      <TerminalPanel
        conversationId="conversation-1"
        terminalId="terminal-1"
        label="终端 1"
        open
        onStateChange={onStateChange}
      />
    );
    await screen.findByText("空终端");
    act(() => currentTerminal().emitData("Get-Location\r"));
    expect(screen.getByText("运行中")).toBeInTheDocument();
    expect(onStateChange).toHaveBeenLastCalledWith(expect.objectContaining({
      hasHistory: false,
      busy: true
    }));

    act(() => onEvent?.({
      type: "command_state",
      sessionId: "session-1",
      commandState: {
        revision: 1,
        status: "running",
        commandId: "command-1",
        commandCount: 1
      }
    }));
    expect(onStateChange).toHaveBeenLastCalledWith(expect.objectContaining({
      hasHistory: true,
      busy: true
    }));

    const prompt = Array.from(new TextEncoder().encode("\r\nPS C:\\workspace> "));
    act(() => onEvent?.({ type: "output", sessionId: "session-1", data: prompt }));
    expect(screen.getByText("运行中")).toBeInTheDocument();

    act(() => onEvent?.({
      type: "command_state",
      sessionId: "session-1",
      commandState: {
        revision: 0,
        status: "idle",
        commandId: null,
        commandCount: 0
      }
    }));
    expect(screen.getByText("运行中")).toBeInTheDocument();

    act(() => onEvent?.({
      type: "command_state",
      sessionId: "session-1",
      commandState: {
        revision: 2,
        status: "idle",
        commandId: null,
        commandCount: 1
      }
    }));
    expect(screen.getByText("待机")).toBeInTheDocument();
    expect(onStateChange).toHaveBeenLastCalledWith(expect.objectContaining({
      hasHistory: true,
      busy: false
    }));
  });

  it("keeps an opening failure passive until the user retries it", async () => {
    terminalApiMocks.openTerminal.mockRejectedValueOnce(new Error("shell 启动失败"));
    render(<TerminalPanel conversationId="conversation-1" terminalId="terminal-1" label="终端 1" open />);

    expect(await screen.findByRole("alert")).toHaveTextContent("shell 启动失败");
    expect(screen.queryByRole("button", { name: "关闭终端" })).not.toBeInTheDocument();
    // A failed open must not loop: one attempt, then wait for the retry control.
    expect(terminalApiMocks.openTerminal).toHaveBeenCalledOnce();

    fireEvent.click(screen.getByRole("button", { name: "重试" }));
    await waitFor(() => expect(terminalApiMocks.openTerminal).toHaveBeenCalledTimes(2));
    expect(await screen.findByText("空终端")).toBeInTheDocument();
  });

});
