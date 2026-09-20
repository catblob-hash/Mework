import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  closeTerminal,
  detachTerminal,
  openTerminal,
  resizeTerminal,
  writeTerminal
} from "./terminal";
import type { OpenTerminalResult, TerminalEvent } from "./terminal";

const coreMocks = vi.hoisted(() => ({
  invoke: vi.fn(),
  channels: [] as Array<{ onmessage: (message: unknown) => void }>
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: coreMocks.invoke,
  Channel: class<T> {
    onmessage: (message: T) => void = () => undefined;

    constructor() {
      coreMocks.channels.push(this as unknown as { onmessage: (message: unknown) => void });
    }
  }
}));

function enableDesktopRuntime() {
  Object.defineProperty(window, "__TAURI_INTERNALS__", {
    configurable: true,
    value: {}
  });
}

describe("terminal IPC", () => {
  beforeEach(() => {
    coreMocks.invoke.mockReset();
    coreMocks.channels.length = 0;
    enableDesktopRuntime();
  });

  afterEach(() => {
    delete (window as Window & { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
  });

  it("opens a terminal through a channel and forwards session-scoped events", async () => {
    const response: OpenTerminalResult = {
      sessionId: "session-1",
      created: true,
      running: true,
      ready: false,
      cwd: "C:/workspace",
      shell: "PowerShell",
      snapshot: [62, 32],
      commandState: {
        revision: 4,
        status: "running",
        commandId: "command-4",
        commandCount: 2
      }
    };
    coreMocks.invoke.mockResolvedValueOnce(response);
    const onEvent = vi.fn();

    const opening = openTerminal("conversation-1", "terminal-1", 100, 30, onEvent);

    expect(coreMocks.channels).toHaveLength(1);
    expect(coreMocks.invoke).toHaveBeenCalledWith("open_terminal", {
      conversationId: "conversation-1",
      terminalId: "terminal-1",
      cols: 100,
      rows: 30,
      onEvent: coreMocks.channels[0]
    });
    const output: TerminalEvent = {
      type: "output",
      sessionId: "session-1",
      data: [104, 105]
    };
    coreMocks.channels[0].onmessage(output);
    expect(onEvent).toHaveBeenCalledWith(output);
    const commandState: TerminalEvent = {
      type: "command_state",
      sessionId: "session-1",
      commandState: {
        revision: 5,
        status: "idle",
        commandId: null,
        commandCount: 2
      }
    };
    coreMocks.channels[0].onmessage(commandState);
    expect(onEvent).toHaveBeenLastCalledWith(commandState);
    const ready: TerminalEvent = {
      type: "ready",
      sessionId: "session-1"
    };
    coreMocks.channels[0].onmessage(ready);
    expect(onEvent).toHaveBeenLastCalledWith(ready);
    await expect(opening).resolves.toEqual(response);
  });

  it("binds every operation to the owning task and independent terminal id", async () => {
    coreMocks.invoke.mockResolvedValue(undefined);

    await writeTerminal("conversation-1", "terminal-1", "session-1", "dir\r");
    await resizeTerminal("conversation-1", "terminal-1", "session-1", 120, 36);
    await detachTerminal("conversation-1", "terminal-1", "session-1");
    await closeTerminal("conversation-1", "terminal-1");

    expect(coreMocks.invoke.mock.calls).toEqual([
      ["write_terminal", { conversationId: "conversation-1", terminalId: "terminal-1", sessionId: "session-1", data: "dir\r" }],
      ["resize_terminal", { conversationId: "conversation-1", terminalId: "terminal-1", sessionId: "session-1", cols: 120, rows: 36 }],
      ["detach_terminal", { conversationId: "conversation-1", terminalId: "terminal-1", sessionId: "session-1" }],
      ["close_terminal", { conversationId: "conversation-1", terminalId: "terminal-1" }]
    ]);
  });

  it("reports a clear error outside the desktop runtime before constructing a channel", async () => {
    delete (window as Window & { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;

    await expect(openTerminal("conversation-1", "terminal-1", 80, 24, vi.fn()))
      .rejects.toThrow("终端仅可在桌面应用中使用");
    expect(coreMocks.channels).toHaveLength(0);
    expect(coreMocks.invoke).not.toHaveBeenCalled();
  });
});
