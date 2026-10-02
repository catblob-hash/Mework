import { beforeEach, describe, expect, it, vi } from "vitest";
import type { AppPushEvent } from "./appEvents";

const mocks = vi.hoisted(() => {
  const listeners = new Set<(event: AppPushEvent) => void>();
  return {
    listeners,
    emit(event: AppPushEvent) {
      for (const listener of [...listeners]) listener(event);
    },
    getToolExplanations: vi.fn()
  };
});

vi.mock("./appEvents", () => ({
  onAppPushEvent: (listener: (event: AppPushEvent) => void) => {
    mocks.listeners.add(listener);
    return () => mocks.listeners.delete(listener);
  }
}));
vi.mock("./runtime", () => ({
  getToolExplanations: mocks.getToolExplanations,
  localModelActivate: vi.fn(),
  localModelCancelInstall: vi.fn(),
  localModelDefaultPrompts: vi.fn(),
  localModelInstall: vi.fn(),
  localModelPromptInfo: vi.fn(),
  localModelRemove: vi.fn(),
  localModelStatus: vi.fn()
}));

const {
  createLocalModelController,
  loadToolExplanations,
  resetToolExplanationsForTests,
  subscribeToolExplanations,
  toolErrorExplanation,
  toolExplanation,
  toolExplanationVersion
} = await import("./localModel");

const ready = {
  machine: { chip: "Apple M4", model: "Mac16,1", osVersion: "15.4", appleSilicon: true, neuralEngineCores: 16 },
  variants: [
    { id: "ane" as const, phase: "ready" as const, downloadBytes: 1, diskBytes: 1 },
    { id: "mlx" as const, phase: "missing" as const, downloadBytes: 1, diskBytes: 0 }
  ],
  active: "ane" as const,
  recommended: "ane" as const,
  warming: false,
  loading: false,
  device: "Apple Neural Engine",
  loaded: true,
  running: 0,
  queued: 0,
  slots: 4,
  context: 1024,
  diskBytes: 1,
  lastError: null
};

describe("local model controller", () => {
  it("follows pushed status and the verbs' answers", async () => {
    const pushListeners = new Set<(event: AppPushEvent) => void>();
    const missing = { ...ready, active: null, variants: ready.variants.map((v) => ({ ...v, phase: "missing" as const })) };
    const downloading = {
      ...missing,
      variants: [{ id: "mlx" as const, phase: "downloading" as const, received: 0, total: 10, source: "huggingFace" as const, downloadBytes: 10, diskBytes: 0 }]
    };
    const install = vi.fn(async () => downloading);
    const activate = vi.fn(async () => ready);
    const controller = createLocalModelController({
      status: vi.fn(async () => missing),
      install,
      activate,
      cancelInstall: vi.fn(async () => {}),
      remove: vi.fn(async () => ready),
      promptInfo: vi.fn(),
      defaultPrompts: vi.fn(),
      subscribePush: (listener) => {
        pushListeners.add(listener);
        return () => pushListeners.delete(listener);
      }
    });
    const seen: string[] = [];
    controller.subscribe(() => seen.push(controller.current()?.variants[0]?.phase ?? "none"));
    await controller.refresh();
    await controller.install("mlx", true);
    expect(install).toHaveBeenCalledWith("mlx", true);
    for (const listener of pushListeners) listener({ type: "localModelChanged", status: ready });
    await controller.activate("ane");
    expect(activate).toHaveBeenCalledWith("ane");
    expect(seen).toEqual(["missing", "downloading", "ready", "ready"]);
  });

  it("shares one status request between overlapping refreshes", async () => {
    let answer: (status: typeof ready) => void = () => {};
    const status = vi.fn(
      () =>
        new Promise<typeof ready>((resolve) => {
          answer = resolve;
        })
    );
    const promptInfo = vi.fn(async () => ({ tokens: 1, cacheBytes: null, maxTokens: 2 }));
    const controller = createLocalModelController({
      status,
      install: vi.fn(),
      activate: vi.fn(),
      cancelInstall: vi.fn(async () => {}),
      remove: vi.fn(),
      promptInfo,
      defaultPrompts: vi.fn(),
      subscribePush: () => () => {}
    });
    const first = controller.refresh();
    const second = controller.refresh();
    expect(status).toHaveBeenCalledTimes(1);
    answer(ready);
    await Promise.all([first, second]);
    expect(controller.current()).toBe(ready);
    // Once answered, the next refresh asks again.
    const third = controller.refresh();
    expect(status).toHaveBeenCalledTimes(2);
    answer(ready);
    await third;

    await controller.promptInfo("title", "Name it.");
    expect(promptInfo).toHaveBeenLastCalledWith("title", "Name it.", false);
    await controller.promptInfo("title", "Name it.", true);
    expect(promptInfo).toHaveBeenLastCalledWith("title", "Name it.", true);
  });
});

describe("tool explanations", () => {
  beforeEach(() => {
    resetToolExplanationsForTests();
    mocks.getToolExplanations.mockReset();
  });

  it("records pushed explanations by card id and by running call id", () => {
    const onChange = vi.fn();
    const unsubscribe = subscribeToolExplanations(onChange);
    const before = toolExplanationVersion();
    mocks.emit({ type: "toolExplained", conversationId: "c1", contextId: "tool_1", callId: "call_9", text: "列出文件", error: false });
    expect(toolExplanation("tool_1")).toBe("列出文件");
    expect(toolExplanation("not-yet-saved", "c1", "call_9")).toBe("列出文件");
    expect(toolExplanation("not-yet-saved", "c2", "call_9")).toBeUndefined();
    expect(toolExplanationVersion()).toBeGreaterThan(before);
    expect(onChange).toHaveBeenCalledTimes(1);
    unsubscribe();
  });

  it("keeps why a call failed apart from what it does", () => {
    const unsubscribe = subscribeToolExplanations(() => {});
    mocks.emit({ type: "toolExplained", conversationId: "c1", contextId: "tool_1", callId: "call_9", text: "安装依赖", error: false });
    mocks.emit({ type: "toolExplained", conversationId: "c1", contextId: "tool_1", callId: "call_9", text: "没有安装 pnpm", error: true });
    expect(toolExplanation("tool_1")).toBe("安装依赖");
    expect(toolErrorExplanation("tool_1")).toBe("没有安装 pnpm");
    expect(toolErrorExplanation("tool_2")).toBeUndefined();
    unsubscribe();
  });

  it("loads a conversation's stored explanations once", async () => {
    mocks.getToolExplanations.mockResolvedValue({ explanations: { tool_a: "运行测试" }, errors: { tool_a: "断言失败" } });
    await loadToolExplanations("c1");
    await loadToolExplanations("c1");
    expect(mocks.getToolExplanations).toHaveBeenCalledTimes(1);
    expect(toolExplanation("tool_a")).toBe("运行测试");
    expect(toolErrorExplanation("tool_a")).toBe("断言失败");
  });

  it("retries a load that failed", async () => {
    const error = vi.spyOn(console, "error").mockImplementation(() => {});
    mocks.getToolExplanations.mockRejectedValueOnce(new Error("offline")).mockResolvedValueOnce({ explanations: { tool_b: "构建项目" }, errors: {} });
    await loadToolExplanations("c2");
    await loadToolExplanations("c2");
    expect(toolExplanation("tool_b")).toBe("构建项目");
    error.mockRestore();
  });
});
