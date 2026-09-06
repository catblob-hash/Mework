import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  createTerminalController,
  terminalSessionIsEmpty,
  terminalSessionKey
} from "./terminalController";
import type { TerminalSessionState } from "./terminal";

const terminalMocks = vi.hoisted(() => ({
  closeTerminal: vi.fn()
}));
vi.mock("./terminal", () => terminalMocks);

const KEY = terminalSessionKey("conversation-1", "terminal-1");

function session(overrides: Partial<TerminalSessionState> = {}): TerminalSessionState {
  return {
    terminalId: "terminal-1",
    conversationId: "conversation-1",
    label: "Terminal 1",
    phase: "running",
    busy: false,
    hasHistory: false,
    cwd: "",
    shell: "",
    sessionId: "session-1",
    ...overrides
  };
}

beforeEach(() => {
  terminalMocks.closeTerminal.mockReset().mockResolvedValue(undefined);
});

describe("terminalSessionIsEmpty", () => {
  it("treats missing, idle sessions as empty and busy or used ones as not", () => {
    expect(terminalSessionIsEmpty(undefined)).toBe(true);
    expect(terminalSessionIsEmpty(session())).toBe(true);
    expect(terminalSessionIsEmpty(session({ busy: true }))).toBe(false);
    expect(terminalSessionIsEmpty(session({ hasHistory: true }))).toBe(false);
  });
});

describe("createTerminalController", () => {
  it("notifies subscribers on register and update but not on identical updates", () => {
    const controller = createTerminalController();
    const listener = vi.fn();
    controller.subscribe(listener);

    controller.register(session());
    expect(listener).toHaveBeenCalledTimes(1);
    expect(controller.current()[KEY]).toEqual(session());

    controller.update(session());
    expect(listener).toHaveBeenCalledTimes(1);

    controller.update(session({ busy: true }));
    expect(listener).toHaveBeenCalledTimes(2);
    expect(controller.current()[KEY]?.busy).toBe(true);
  });

  it("upserts unknown live sessions through update", () => {
    const controller = createTerminalController();
    controller.update(session({ phase: "connecting", sessionId: null }));
    expect(controller.current()[KEY]).toEqual(session({ phase: "connecting", sessionId: null }));
  });

  /**
   * The store answers "is a terminal of this workspace busy" for the Git write
   * guard and the task list. Once the panel stops observing a session — it was
   * closed, it exited, or the user switched conversation and the panel reported
   * it idle on the way out — nothing could ever clear its busy flag, so the
   * report removes the entry instead of freezing it.
   */
  it("drops a session the renderer no longer observes as live", () => {
    const controller = createTerminalController();
    const listener = vi.fn();
    controller.subscribe(listener);
    controller.update(session({ busy: true, hasHistory: true }));
    expect(controller.current()[KEY]).toBeDefined();

    controller.update(session({ phase: "idle", busy: false, hasHistory: true, sessionId: null }));
    expect(controller.current()[KEY]).toBeUndefined();
    expect(listener).toHaveBeenCalledTimes(2);

    controller.update(session({ phase: "exited", busy: false }));
    expect(controller.current()[KEY]).toBeUndefined();
    // Reporting a non-live session that is not stored changes nothing and stays quiet.
    expect(listener).toHaveBeenCalledTimes(2);

    controller.update(session({ phase: "error", busy: true }));
    expect(controller.current()[KEY]).toBeUndefined();

    controller.update(session({ phase: "closing", busy: true }));
    expect(controller.current()[KEY]).toMatchObject({ phase: "closing", busy: true });
  });

  /**
   * Every conversation's composer drawer uses the same terminal id, so the id
   * alone once made the second conversation overwrite the first one's entry —
   * and lose its busy terminal from the task list and the Git write guard.
   */
  it("keeps the same terminal id apart per conversation", () => {
    const controller = createTerminalController();
    controller.update(session({ terminalId: "composer", busy: true, hasHistory: true }));
    controller.update(session({ terminalId: "composer", conversationId: "conversation-2" }));

    expect(Object.keys(controller.current())).toHaveLength(2);
    expect(controller.current()[terminalSessionKey("conversation-1", "composer")]).toMatchObject({
      conversationId: "conversation-1",
      busy: true
    });
    expect(controller.current()[terminalSessionKey("conversation-2", "composer")]).toMatchObject({
      conversationId: "conversation-2",
      busy: false
    });
    expect(controller.markCommandStarted("conversation-2", "composer")).toBe(true);
    expect(controller.current()[terminalSessionKey("conversation-1", "composer")]?.hasHistory).toBe(true);
    expect(controller.current()[terminalSessionKey("conversation-2", "composer")]?.hasHistory).toBe(true);
  });

  it("keeps current() referentially stable between mutations", () => {
    const controller = createTerminalController();
    controller.register(session());
    const first = controller.current();
    expect(controller.current()).toBe(first);
    controller.update(session({ busy: true }));
    expect(controller.current()).not.toBe(first);
  });

  it("marks command start as busy with history and rejects unknown terminals", () => {
    const controller = createTerminalController();
    expect(controller.markCommandStarted("conversation-1", "terminal-1")).toBe(false);
    controller.register(session());
    expect(controller.markCommandStarted("conversation-2", "terminal-1")).toBe(false);
    expect(controller.markCommandStarted("conversation-1", "terminal-1")).toBe(true);
    expect(controller.current()[KEY]).toMatchObject({ busy: true, hasHistory: true });
  });

  it("removes the session and runs onClosed after a successful close", async () => {
    const controller = createTerminalController();
    controller.register(session());
    controller.register(session({ conversationId: "conversation-2" }));
    const onClosed = vi.fn();
    await controller.requestClose("conversation-1", "terminal-1", { onClosed });
    expect(terminalMocks.closeTerminal).toHaveBeenCalledWith("conversation-1", "terminal-1");
    expect(controller.current()[KEY]).toBeUndefined();
    expect(controller.current()[terminalSessionKey("conversation-2", "terminal-1")]).toBeDefined();
    expect(onClosed).toHaveBeenCalledTimes(1);
  });

  it("shares one in-flight close task and only honors the first caller's handlers", async () => {
    const controller = createTerminalController();
    controller.register(session());
    let release!: () => void;
    terminalMocks.closeTerminal.mockImplementationOnce(() => new Promise<void>((resolve) => {
      release = resolve;
    }));
    const firstClosed = vi.fn();
    const secondClosed = vi.fn();
    const first = controller.requestClose("conversation-1", "terminal-1", { onClosed: firstClosed });
    const second = controller.requestClose("conversation-1", "terminal-1", { onClosed: secondClosed });
    expect(second).toBe(first);
    // Another conversation's terminal of the same id is a different close task.
    const other = controller.requestClose("conversation-2", "terminal-1");
    expect(other).not.toBe(first);
    // The close tasks start on a microtask; let them reach closeTerminal first.
    await Promise.resolve();
    release();
    await Promise.all([first, other]);
    expect(terminalMocks.closeTerminal).toHaveBeenCalledTimes(2);
    expect(terminalMocks.closeTerminal).toHaveBeenNthCalledWith(1, "conversation-1", "terminal-1");
    expect(terminalMocks.closeTerminal).toHaveBeenNthCalledWith(2, "conversation-2", "terminal-1");
    expect(firstClosed).toHaveBeenCalledTimes(1);
    expect(secondClosed).not.toHaveBeenCalled();
  });

  it("runs onError, keeps the session, rejects, and allows a retry after failure", async () => {
    const controller = createTerminalController();
    controller.register(session());
    terminalMocks.closeTerminal.mockRejectedValueOnce(new Error("close failed"));
    const onError = vi.fn();
    await expect(
      controller.requestClose("conversation-1", "terminal-1", { onError })
    ).rejects.toThrow("close failed");
    expect(onError).toHaveBeenCalledWith(expect.objectContaining({ message: "close failed" }));
    expect(controller.current()[KEY]).toBeDefined();

    await controller.requestClose("conversation-1", "terminal-1");
    expect(controller.current()[KEY]).toBeUndefined();
  });
});
