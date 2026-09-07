import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import App from "./App";
import { configureI18n } from "./i18n";
import type {
  AppDocument,
  ModelRunRequest,
  ModelRunResponse,
  ModelStreamEvent,
  QueuedMessage,
  UserContext
} from "./types";
import { resetAppMocks, deferred, documentWithModel, model, runtimeMocks } from "./test/appMocks";

vi.mock("./lib/runtime", async (importOriginal) => {
  const { runtimeMocks } = await import("./test/appMockInstances");
  return { ...await importOriginal<typeof import("./lib/runtime")>(), ...runtimeMocks };
});
vi.mock("./lib/terminal", async () => (await import("./test/appMockInstances")).terminalMocks);
vi.mock("./lib/browser", async () => (await import("./test/appMockInstances")).browserMocks);
vi.mock("./lib/browserRendererMount", async () => {
  const { browserRendererMountMocks } = await import("./test/appMockInstances");
  return {
    startBrowserRendererMountHeartbeat: browserRendererMountMocks.startHeartbeat,
    stopBrowserRendererMountHeartbeat: browserRendererMountMocks.stopHeartbeat
  };
});
vi.mock("./lib/git", async (importOriginal) => {
  const { gitMocks } = await import("./test/appMockInstances");
  return { ...await importOriginal<typeof import("./lib/git")>(), ...gitMocks };
});
vi.mock("./components/TerminalPanel", async () => (await import("./test/appMockInstances")).terminalPanelModuleMock());

afterEach(() => configureI18n("zh-CN"));

describe("App model run flow — streamQueue", () => {
  beforeEach(resetAppMocks);

  it("blocks ordinary send when projected context images meet a text-only model", async () => {
    const document = documentWithModel();
    const image = {
      id: "history-image",
      name: "history.png",
      mime: "image/png",
      width: 640,
      height: 480,
      bytes: 1000
    };
    document.workspaces[0].conversations[0].contexts = [{
      id: "history-image-user",
      kind: "user",
      content: "看这张图",
      images: [image],
      createdAt: "2026-07-20T00:00:00Z"
    }];
    runtimeMocks.loadDocument.mockResolvedValue(document);

    const user = userEvent.setup();
    render(<App />);
    const composer = await screen.findByLabelText("向 Agent 发送消息");
    await user.type(composer, "继续分析");
    await user.click(screen.getByRole("button", { name: "发送" }));
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    expect(composer).toHaveValue("继续分析");
    expect(runtimeMocks.runModel).not.toHaveBeenCalled();

    // Branching only drafts text into a new conversation, so it never reaches
    // the vision guard and never starts a request.
    const imageMessage = screen.getByText("看这张图").closest("article")!;
    await user.click(within(imageMessage).getByRole("button", { name: "从此消息分支" }));
    await waitFor(() => expect(screen.getByRole("textbox", { name: "向 Agent 发送消息" })).toHaveValue("看这张图"));
    expect(runtimeMocks.runModel).not.toHaveBeenCalled();
  });




  it("surfaces a blocked lifecycle hook without offering an API retry", async () => {
    const document = documentWithModel();
    runtimeMocks.loadDocument.mockResolvedValue(document);
    runtimeMocks.runModel.mockResolvedValue({
      contexts: [{
        id: "ctx_hook_failure",
        kind: "system",
        content: "UserPromptSubmit 生命周期钩子已阻止提示词",
        localOnly: true,
        createdAt: "2026-07-11T00:00:00Z"
      }],
      usage: {},
      model: model.id,
      providerName: "OpenAI Responses",
      durationMs: 3,
      stopReason: "hook_blocked"
    });

    const user = userEvent.setup();
    render(<App />);
    await user.type(await screen.findByLabelText("向 Agent 发送消息"), "触发钩子");
    await user.click(screen.getByRole("button", { name: "发送" }));

    expect(await screen.findByText("UserPromptSubmit 生命周期钩子已阻止提示词")).toBeInTheDocument();
    expect(await screen.findByRole("button", { name: /test-model 在 .* 后中止/ })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "重试" })).not.toBeInTheDocument();
    expect(window.document.querySelector(".composer-run-error")).toBeNull();
  });

  it("renders streamed deltas immediately and replaces them with the final persisted response", async () => {
    const document = documentWithModel();
    runtimeMocks.loadDocument.mockResolvedValue(document);
    let emit!: (event: { type: "text_delta"; round: number; delta: string }) => void;
    let resolveRun!: (value: unknown) => void;
    runtimeMocks.runModel.mockImplementation((_request, onEvent) => {
      emit = onEvent;
      return new Promise((resolve) => { resolveRun = resolve; });
    });

    const user = userEvent.setup();
    render(<App />);
    await user.type(await screen.findByLabelText("向 Agent 发送消息"), "开始流式输出");
    await user.click(screen.getByRole("button", { name: "发送" }));
    await waitFor(() => expect(runtimeMocks.runModel).toHaveBeenCalledTimes(1));
    const waiting = screen.getByRole("status", { name: "模型正在生成" });
    expect(waiting).toHaveAttribute("data-stream-waiting", "true");
    expect(waiting.querySelector(".stream-waiting__cat")).toBeInTheDocument();

    act(() => {
      emit({ type: "text_delta", round: 0, delta: "第一段" });
      emit({ type: "text_delta", round: 0, delta: "第二段" });
    });
    const streaming = await screen.findByText("第一段第二段");
    const streamingArticle = streaming.closest("article")!;
    expect(streamingArticle).toHaveAttribute("aria-busy", "true");
    expect(within(streamingArticle).getByText("模型回复")).toBeInTheDocument();
    expect(screen.queryByText("模型回复 · 正在生成")).not.toBeInTheDocument();
    expect(within(streamingArticle).getByRole("button", { name: "编辑上下文" })).toBeDisabled();
    expect(within(streamingArticle).getByRole("button", { name: "删除上下文" })).toBeDisabled();
    expect(streamingArticle.querySelector(".streaming-cursor")).not.toBeInTheDocument();

    await act(async () => resolveRun({
      contexts: [{ id: "ctx_stream_final", kind: "assistant", content: "最终回复", createdAt: "2026-07-11T00:00:00Z" }],
      usage: {},
      model: model.id,
      providerName: "OpenAI Responses",
      durationMs: 15
    }));
    expect(await screen.findByText("最终回复")).toBeInTheDocument();
    expect(screen.queryByText("第一段第二段")).not.toBeInTheDocument();
    expect(screen.queryByRole("status", { name: "模型正在生成" })).not.toBeInTheDocument();
  });

  it("streams the three usage counters, auto-collapses a completed turn, and keeps its final reply outside", async () => {
    const document = documentWithModel();
    runtimeMocks.loadDocument.mockResolvedValue(document);
    let emit!: (event: ModelStreamEvent) => void;
    let resolveRun!: (value: ModelRunResponse) => void;
    runtimeMocks.runModel.mockImplementation((_request, onEvent) => {
      emit = onEvent;
      return new Promise<ModelRunResponse>((resolve) => { resolveRun = resolve; });
    });

    const user = userEvent.setup();
    const { container } = render(<App />);
    await user.type(await screen.findByLabelText("向 Agent 发送消息"), "统计这一轮");
    await user.click(screen.getByRole("button", { name: "发送" }));
    await waitFor(() => expect(runtimeMocks.runModel).toHaveBeenCalledTimes(1));

    const runningTurn = screen.getByRole("button", { name: /test-model 运行了 .*输入—.*缓存输入—.*输出—/ });
    expect(runningTurn).toHaveAttribute("aria-expanded", "true");
    act(() => emit({
      type: "usage_updated",
      round: 1,
      usage: { inputTokens: 120, cachedInputTokens: 40, outputTokens: 8, totalTokens: 128 }
    }));
    expect(await screen.findByRole("button", {
      name: /test-model 运行了 .*输入120.*缓存输入40.*输出8/
    })).toBeInTheDocument();

    await act(async () => resolveRun({
      contexts: [
        { id: "ctx_usage_reasoning", kind: "reasoning", content: "内部统计步骤", createdAt: "2026-07-24T00:00:00Z" },
        { id: "ctx_usage_answer", kind: "assistant", content: "最终统计回复", createdAt: "2026-07-24T00:00:01Z" }
      ],
      usage: { inputTokens: 120, cachedInputTokens: 40, outputTokens: 12, totalTokens: 132 },
      model: model.id,
      providerName: "OpenAI Responses",
      durationMs: 62_000
    }));

    const completedTurn = await screen.findByRole("button", {
      name: /test-model 运行了 1m2s.*输入120.*缓存输入40.*输出12/
    });
    expect(completedTurn).toHaveAttribute("aria-expanded", "false");
    expect(container.querySelector('[data-context-id="ctx_usage_reasoning"]')?.closest("[data-turn-id]")).not.toBeNull();
    expect(container.querySelector('[data-context-id="ctx_usage_answer"]')?.closest("[data-turn-id]")).toBeNull();
  });

  it("preserves the user's turn disclosure choice when a normal run finishes", async () => {
    const document = documentWithModel();
    runtimeMocks.loadDocument.mockResolvedValue(document);
    let resolveRun!: (value: ModelRunResponse) => void;
    runtimeMocks.runModel.mockImplementation(() => (
      new Promise<ModelRunResponse>((resolve) => { resolveRun = resolve; })
    ));

    const user = userEvent.setup();
    render(<App />);
    await user.type(await screen.findByLabelText("向 Agent 发送消息"), "保持展开");
    await user.click(screen.getByRole("button", { name: "发送" }));
    await waitFor(() => expect(runtimeMocks.runModel).toHaveBeenCalledTimes(1));
    const runningTurn = screen.getByRole("button", { name: /test-model 运行了/ });
    await user.click(runningTurn);
    await user.click(runningTurn);
    expect(runningTurn).toHaveAttribute("aria-expanded", "true");

    await act(async () => resolveRun({
      contexts: [
        { id: "ctx_touched_reasoning", kind: "reasoning", content: "用户点过后仍展开", createdAt: "2026-07-24T00:00:00Z" },
        { id: "ctx_touched_answer", kind: "assistant", content: "完成回复", createdAt: "2026-07-24T00:00:01Z" }
      ],
      usage: {},
      model: model.id,
      providerName: "OpenAI Responses",
      durationMs: 20
    }));

    expect(await screen.findByRole("button", { name: /test-model 运行了/ }))
      .toHaveAttribute("aria-expanded", "true");
    expect(screen.getByRole("button", { name: "思考过程" })).toBeInTheDocument();
  });

  it("queues composer messages during streaming, steers one into the live turn, then sends the rest", async () => {
    const document = documentWithModel();
    runtimeMocks.loadDocument.mockResolvedValue(document);
    let emit!: (event: ModelStreamEvent) => void;
    let resolveFirstRun!: (value: ModelRunResponse) => void;
    runtimeMocks.runModel
      .mockImplementationOnce((_request, onEvent) => {
        emit = onEvent;
        return new Promise<ModelRunResponse>((resolve) => { resolveFirstRun = resolve; });
      })
      .mockResolvedValueOnce({
        contexts: [{
          id: "queued-second",
          kind: "assistant",
          content: "第二条排队消息已处理",
          createdAt: "2026-07-24T00:00:04Z"
        }],
        usage: {},
        model: model.id,
        providerName: "OpenAI Responses",
        durationMs: 8
      });

    const user = userEvent.setup();
    render(<App />);
    const composer = await screen.findByLabelText("向 Agent 发送消息");
    await user.type(composer, "开始长任务");
    await user.click(screen.getByRole("button", { name: "发送" }));
    await waitFor(() => expect(runtimeMocks.runModel).toHaveBeenCalledTimes(1));
    expect(screen.getByRole("button", { name: "停止生成" })).toBeInTheDocument();

    await user.type(composer, "优先检查失败日志");
    expect(screen.getByRole("button", { name: "加入排队消息" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "停止生成" })).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "加入排队消息" }));
    expect(composer).toHaveValue("");

    await user.type(composer, "然后补齐回归测试");
    await user.keyboard("{Enter}");
    const queue = await screen.findByRole("region", { name: "排队消息" });
    expect(within(queue).getAllByRole("listitem")).toHaveLength(2);
    expect(within(queue).getByText("优先检查失败日志")).toBeInTheDocument();
    expect(within(queue).getByText("然后补齐回归测试")).toBeInTheDocument();
    await waitFor(() => expect(runtimeMocks.saveDocument.mock.calls.some(([saved]) => (
      (saved as AppDocument).workspaces[0].conversations[0].queuedMessages.length === 2
    ))).toBe(true));

    await user.click(within(queue).getByRole("button", {
      name: "将“优先检查失败日志”引导到当前回合"
    }));
    await waitFor(() => expect(runtimeMocks.steerModelRun).toHaveBeenCalledTimes(1));
    const [requestId, steeredMessage] = runtimeMocks.steerModelRun.mock.calls[0] as [
      string,
      { id: string; content: string; createdAt: string }
    ];
    expect(requestId).toBe(runtimeMocks.runModel.mock.calls[0][2]);
    expect(steeredMessage.content).toBe("优先检查失败日志");
    expect(within(queue).getByText("优先检查失败日志")).toBeInTheDocument();
    expect(within(queue).getByRole("button", {
      name: "将“优先检查失败日志”引导到当前回合"
    })).toBeDisabled();

    act(() => emit({
      type: "user_input_received",
      round: 2,
      id: steeredMessage.id,
      content: steeredMessage.content,
      createdAt: steeredMessage.createdAt
    }));
    await waitFor(() => {
      expect(screen.queryByText("优先检查失败日志", { selector: ".queued-messages span" }))
        .not.toBeInTheDocument();
    });
    expect(screen.getByText("优先检查失败日志")).toBeInTheDocument();
    // Steering closes the round it landed in, but that round never produced a
    // message, so it has nothing to disclose and draws no header of its own.
    // Only the round the steered message opened is on screen.
    expect(screen.queryByRole("button", { name: /test-model 在 .* 后中止/ })).not.toBeInTheDocument();
    expect(screen.getAllByRole("button", { name: /test-model 运行了/ })).toHaveLength(1);
    expect(within(screen.getByRole("region", { name: "排队消息" })).getAllByRole("listitem"))
      .toHaveLength(1);

    await act(async () => resolveFirstRun({
      contexts: [
        {
          id: steeredMessage.id,
          kind: "user",
          content: steeredMessage.content,
          createdAt: steeredMessage.createdAt
        },
        {
          id: "queued-first-answer",
          kind: "assistant",
          content: "优先日志已检查",
          createdAt: "2026-07-24T00:00:03Z"
        }
      ],
      usage: {},
      model: model.id,
      providerName: "OpenAI Responses",
      durationMs: 12
    }));

    await waitFor(() => expect(runtimeMocks.runModel).toHaveBeenCalledTimes(2));
    const queuedRequest = runtimeMocks.runModel.mock.calls[1][0] as ModelRunRequest;
    expect(queuedRequest.contexts.at(-1)).toEqual(expect.objectContaining({
      kind: "user",
      content: "然后补齐回归测试"
    }));
    expect(await screen.findByText("第二条排队消息已处理")).toBeInTheDocument();
    expect(screen.queryByRole("region", { name: "排队消息" })).not.toBeInTheDocument();
  });

  it("waits for a newly queued message to be saved before steering it", async () => {
    const document = documentWithModel();
    runtimeMocks.loadDocument.mockResolvedValue(document);
    runtimeMocks.runModel.mockImplementation(() => new Promise(() => undefined));
    const user = userEvent.setup();
    render(<App />);
    const composer = await screen.findByLabelText("向 Agent 发送消息");
    await user.type(composer, "启动长任务");
    await user.click(screen.getByRole("button", { name: "发送" }));
    await waitFor(() => expect(runtimeMocks.runModel).toHaveBeenCalledTimes(1));

    const queueSave = deferred<void>();
    runtimeMocks.saveDocument.mockReset().mockReturnValue(queueSave.promise);
    await user.type(composer, "立刻引导但必须先保存");
    await user.click(screen.getByRole("button", { name: "加入排队消息" }));
    const queue = await screen.findByRole("region", { name: "排队消息" });
    const steer = within(queue).getByRole("button", {
      name: "将“立刻引导但必须先保存”引导到当前回合"
    });
    await waitFor(() => expect(runtimeMocks.saveDocument).toHaveBeenCalledTimes(1));
    expect(runtimeMocks.saveDocument.mock.calls[0][1]).toMatchObject({
      immutableSnapshot: true,
      durable: true
    });
    await user.click(steer);
    expect(runtimeMocks.steerModelRun).not.toHaveBeenCalled();

    await act(async () => queueSave.resolve());
    await waitFor(() => expect(runtimeMocks.steerModelRun).toHaveBeenCalledTimes(1));
    expect(runtimeMocks.saveDocument.mock.invocationCallOrder[0])
      .toBeLessThan(runtimeMocks.steerModelRun.mock.invocationCallOrder[0]);
  });

  it("keeps a queued message and does not steer it when its save fails", async () => {
    const document = documentWithModel();
    runtimeMocks.loadDocument.mockResolvedValue(document);
    runtimeMocks.runModel.mockImplementation(() => new Promise(() => undefined));
    const user = userEvent.setup();
    render(<App />);
    const composer = await screen.findByLabelText("向 Agent 发送消息");
    await user.type(composer, "启动失败保存测试");
    await user.click(screen.getByRole("button", { name: "发送" }));
    await waitFor(() => expect(runtimeMocks.runModel).toHaveBeenCalledTimes(1));

    const queueSave = deferred<void>();
    runtimeMocks.saveDocument.mockReset().mockReturnValue(queueSave.promise);
    await user.type(composer, "保存失败仍留在队列");
    await user.click(screen.getByRole("button", { name: "加入排队消息" }));
    const queue = await screen.findByRole("region", { name: "排队消息" });
    const steer = within(queue).getByRole("button", {
      name: "将“保存失败仍留在队列”引导到当前回合"
    });
    await waitFor(() => expect(runtimeMocks.saveDocument).toHaveBeenCalledTimes(1));
    expect(runtimeMocks.saveDocument.mock.calls[0][1]).toMatchObject({
      immutableSnapshot: true,
      durable: true
    });
    await user.click(steer);
    expect(runtimeMocks.steerModelRun).not.toHaveBeenCalled();

    await act(async () => {
      queueSave.reject(new Error("queue disk unavailable"));
      await Promise.resolve();
    });
    await waitFor(() => expect(steer).toBeEnabled());
    expect(runtimeMocks.steerModelRun).not.toHaveBeenCalled();
    expect(within(queue).getByText("保存失败仍留在队列")).toBeInTheDocument();

    const durableRetry = deferred<void>();
    runtimeMocks.saveDocument.mockReturnValueOnce(durableRetry.promise);
    await user.click(steer);
    await waitFor(() => expect(runtimeMocks.saveDocument).toHaveBeenCalledTimes(2));
    expect(runtimeMocks.saveDocument.mock.calls[1][1]).toMatchObject({
      immutableSnapshot: true,
      durable: true
    });
    expect(runtimeMocks.steerModelRun).not.toHaveBeenCalled();

    await act(async () => durableRetry.resolve());
    await waitFor(() => expect(runtimeMocks.steerModelRun).toHaveBeenCalledTimes(1));
  });

  it("rolls back a failed automatic image queue promotion and retries it only on explicit request", async () => {
    const document = documentWithModel();
    document.globalSettings.apiProviders[0].models[0] = {
      ...document.globalSettings.apiProviders[0].models[0],
      capabilities: ["image_recognition"]
    };
    const conversation = document.workspaces[0].conversations[0];
    conversation.contexts = [
      {
        id: "queue-rollback-prior-user",
        kind: "user",
        content: "此前请求",
        createdAt: "2026-07-24T00:00:00Z"
      },
      {
        id: "queue-rollback-prior-assistant",
        kind: "assistant",
        content: "此前回复",
        createdAt: "2026-07-24T00:00:01Z"
      }
    ];
    const queuedImages = [
      {
        id: "queue-rollback-image-a",
        name: "rollback-a.png",
        mime: "image/png",
        width: 20,
        height: 12,
        bytes: 80
      },
      {
        id: "queue-rollback-image-b",
        name: "rollback-b.png",
        mime: "image/png",
        width: 12,
        height: 20,
        bytes: 96
      }
    ];
    const queuedHead: QueuedMessage = {
      id: "queue-rollback-image-head",
      content: "",
      images: queuedImages,
      createdAt: "2026-07-24T00:00:02Z"
    };
    const queuedTail: QueuedMessage = {
      id: "queue-rollback-tail",
      content: "随后处理尾部消息",
      createdAt: "2026-07-24T00:00:03Z"
    };
    conversation.queuedMessages = [queuedHead, queuedTail];
    runtimeMocks.loadDocument.mockResolvedValue(document);

    let rejectedFirstPromotion = false;
    runtimeMocks.saveDocument.mockImplementation((saved: AppDocument) => {
      const savedConversation = saved.workspaces[0].conversations[0];
      const headInContexts = savedConversation.contexts.some(
        (context) => context.id === queuedHead.id
      );
      const headInQueue = savedConversation.queuedMessages.some(
        (message) => message.id === queuedHead.id
      );
      if (headInContexts && !headInQueue && !rejectedFirstPromotion) {
        rejectedFirstPromotion = true;
        return Promise.reject(new Error("automatic queue promotion save failed"));
      }
      return Promise.resolve();
    });

    const firstRun = deferred<ModelRunResponse>();
    runtimeMocks.runModel
      .mockImplementationOnce(() => firstRun.promise)
      .mockResolvedValueOnce({
        contexts: [{
          id: "queue-rollback-tail-answer",
          kind: "assistant",
          content: "尾部消息已处理",
          createdAt: "2026-07-24T00:00:05Z"
        }],
        usage: {},
        model: model.id,
        providerName: "OpenAI Responses",
        durationMs: 4
      });

    const user = userEvent.setup();
    const { container } = render(<App />);
    const composer = await screen.findByLabelText("向 Agent 发送消息");
    await waitFor(() => expect(runtimeMocks.saveDocument).toHaveBeenCalledTimes(3));

    const queue = screen.getByRole("region", { name: "排队消息" });
    const queuedItems = within(queue).getAllByRole("listitem");
    expect(queuedItems).toHaveLength(2);
    expect(queuedItems[0]).toHaveTextContent("rollback-a.png + 另外 1 张");
    expect(queuedItems[1]).toHaveTextContent("随后处理尾部消息");
    expect(container.querySelector(`[data-context-id="${queuedHead.id}"]`)).toBeNull();
    expect(runtimeMocks.runModel).not.toHaveBeenCalled();
    const retry = within(queue).getByRole("button", {
      name: "重试发送排队消息“rollback-a.png + 另外 1 张”"
    });

    const rolledBack = runtimeMocks.saveDocument.mock.calls[2][0] as AppDocument;
    const rolledBackConversation = rolledBack.workspaces[0].conversations[0];
    expect(rolledBackConversation.contexts.map((context) => context.id)).toEqual([
      "queue-rollback-prior-user",
      "queue-rollback-prior-assistant"
    ]);
    expect(rolledBackConversation.queuedMessages).toEqual([queuedHead, queuedTail]);
    expect(runtimeMocks.saveDocument.mock.calls[1][1]).toMatchObject({
      immutableSnapshot: true,
      durable: true
    });
    expect(runtimeMocks.saveDocument.mock.calls[2][1]).toMatchObject({
      immutableSnapshot: true,
      durable: true
    });
    await act(async () => {
      await new Promise((resolve) => window.setTimeout(resolve, 30));
    });
    expect(runtimeMocks.saveDocument).toHaveBeenCalledTimes(3);

    await user.type(composer, "仍在编辑的草稿");
    await user.click(retry);
    await waitFor(() => expect(runtimeMocks.runModel).toHaveBeenCalledTimes(1));

    const firstRequest = runtimeMocks.runModel.mock.calls[0][0] as ModelRunRequest;
    expect(firstRequest.contexts.map((context) => context.id)).toEqual([
      "queue-rollback-prior-user",
      "queue-rollback-prior-assistant",
      queuedHead.id
    ]);
    expect((firstRequest.contexts.at(-1) as UserContext).images)
      .toEqual(queuedImages.map((image, index) => ({ ...image, shortId: index + 1 })));
    expect(composer).toHaveValue("仍在编辑的草稿");

    const firstRunInvocation = runtimeMocks.runModel.mock.invocationCallOrder[0];
    const promotionSavesBeforeFirstRun = runtimeMocks.saveDocument.mock.calls.filter(
      ([saved], index) => {
        if (runtimeMocks.saveDocument.mock.invocationCallOrder[index] >= firstRunInvocation) return false;
        const savedConversation = (saved as AppDocument).workspaces[0].conversations[0];
        return savedConversation.contexts.some((context) => context.id === queuedHead.id)
          && !savedConversation.queuedMessages.some((message) => message.id === queuedHead.id);
      }
    );
    expect(promotionSavesBeforeFirstRun).toHaveLength(2);

    await act(async () => firstRun.resolve({
      contexts: [{
        id: "queue-rollback-head-answer",
        kind: "assistant",
        content: "图片消息已处理",
        createdAt: "2026-07-24T00:00:04Z"
      }],
      usage: {},
      model: model.id,
      providerName: "OpenAI Responses",
      durationMs: 5
    }));
    await waitFor(() => expect(runtimeMocks.runModel).toHaveBeenCalledTimes(2));
    const secondRequest = runtimeMocks.runModel.mock.calls[1][0] as ModelRunRequest;
    expect(secondRequest.contexts.at(-1)).toEqual(expect.objectContaining({
      id: queuedTail.id,
      kind: "user",
      content: queuedTail.content
    }));
    expect(await screen.findByText("尾部消息已处理")).toBeInTheDocument();
    expect(screen.queryByRole("region", { name: "排队消息" })).not.toBeInTheDocument();
    expect(composer).toHaveValue("仍在编辑的草稿");
  });

  it("latches a permanently failing queued promotion without an automatic retry loop", async () => {
    const document = documentWithModel();
    document.globalSettings.apiProviders[0].models[0] = {
      ...document.globalSettings.apiProviders[0].models[0],
      capabilities: ["image_recognition"]
    };
    const queuedMessage: QueuedMessage = {
      id: "queue-permanent-promotion-failure",
      content: "",
      images: [{
        id: "queue-permanent-promotion-image",
        name: "permanent-failure.png",
        mime: "image/png",
        width: 16,
        height: 16,
        bytes: 64
      }],
      createdAt: "2026-07-24T00:00:02Z"
    };
    document.workspaces[0].conversations[0].queuedMessages = [queuedMessage];
    runtimeMocks.loadDocument.mockResolvedValue(document);

    let promotionAttempts = 0;
    runtimeMocks.saveDocument.mockImplementation((saved: AppDocument) => {
      const savedConversation = saved.workspaces[0].conversations[0];
      const promoted = savedConversation.contexts.some(
        (context) => context.id === queuedMessage.id
      ) && !savedConversation.queuedMessages.some(
        (message) => message.id === queuedMessage.id
      );
      if (promoted) {
        promotionAttempts += 1;
        return Promise.reject(new Error("promotion remains unavailable"));
      }
      return Promise.resolve();
    });

    const user = userEvent.setup();
    render(<App />);

    const firstRetry = await screen.findByRole("button", {
      name: "重试发送排队消息“permanent-failure.png”"
    });
    await waitFor(() => expect(promotionAttempts).toBe(1));
    await act(async () => {
      await new Promise((resolve) => window.setTimeout(resolve, 500));
    });
    const firstSaveCount = runtimeMocks.saveDocument.mock.calls.length;
    await act(async () => {
      await new Promise((resolve) => window.setTimeout(resolve, 500));
    });
    expect(promotionAttempts).toBe(1);
    expect(runtimeMocks.saveDocument).toHaveBeenCalledTimes(firstSaveCount);
    expect(runtimeMocks.runModel).not.toHaveBeenCalled();

    await user.click(firstRetry);
    await waitFor(() => expect(promotionAttempts).toBe(2));
    const secondRetry = await screen.findByRole("button", {
      name: "重试发送排队消息“permanent-failure.png”"
    });
    expect(secondRetry).toBeEnabled();
    await act(async () => {
      await new Promise((resolve) => window.setTimeout(resolve, 500));
    });
    const secondSaveCount = runtimeMocks.saveDocument.mock.calls.length;
    await act(async () => {
      await new Promise((resolve) => window.setTimeout(resolve, 500));
    });
    expect(promotionAttempts).toBe(2);
    expect(runtimeMocks.saveDocument).toHaveBeenCalledTimes(secondSaveCount);
    expect(runtimeMocks.runModel).not.toHaveBeenCalled();
    expect(within(screen.getByRole("region", { name: "排队消息" }))
      .getByText("permanent-failure.png")).toBeInTheDocument();
  }, 10_000);

  it("does not hot-loop when both queued promotion and its durable rollback fail", async () => {
    const document = documentWithModel();
    document.globalSettings.apiProviders[0].models[0] = {
      ...document.globalSettings.apiProviders[0].models[0],
      capabilities: ["image_recognition"]
    };
    const queuedMessage: QueuedMessage = {
      id: "queue-rollback-persistence-failure",
      content: "",
      images: [{
        id: "queue-rollback-persistence-image",
        name: "rollback-persistence-failure.png",
        mime: "image/png",
        width: 16,
        height: 16,
        bytes: 64
      }],
      createdAt: "2026-07-24T00:00:02Z"
    };
    document.workspaces[0].conversations[0].queuedMessages = [queuedMessage];
    runtimeMocks.loadDocument.mockResolvedValue(document);

    let promotionAttempts = 0;
    let rollbackAttempts = 0;
    runtimeMocks.saveDocument.mockImplementation((saved: AppDocument, options) => {
      const savedConversation = saved.workspaces[0].conversations[0];
      const promoted = savedConversation.contexts.some(
        (context) => context.id === queuedMessage.id
      ) && !savedConversation.queuedMessages.some(
        (message) => message.id === queuedMessage.id
      );
      if (promoted) {
        promotionAttempts += 1;
        return Promise.reject(new Error("promotion persistence failed"));
      }
      const rolledBack = options?.durable && promotionAttempts > 0
        && savedConversation.contexts.every((context) => context.id !== queuedMessage.id)
        && savedConversation.queuedMessages.some((message) => message.id === queuedMessage.id);
      if (rolledBack && rollbackAttempts === 0) {
        rollbackAttempts += 1;
        return Promise.reject(new Error("rollback persistence failed"));
      }
      return Promise.resolve();
    });

    render(<App />);

    const retry = await screen.findByRole("button", {
      name: "重试发送排队消息“rollback-persistence-failure.png”"
    });
    expect(retry).toBeEnabled();
    await waitFor(() => {
      expect(promotionAttempts).toBe(1);
      expect(rollbackAttempts).toBe(1);
    });
    // Startup and background saves may interleave; identify the transaction by
    // its durable flag and payload instead of the global save call index.
    const durableSaves = runtimeMocks.saveDocument.mock.calls.filter(([, options]) => options?.durable);
    expect(durableSaves).toHaveLength(2);
    for (const [, options] of durableSaves) {
      expect(options).toMatchObject({ immutableSnapshot: true, durable: true });
    }
    expect(durableSaves[0][0].workspaces[0].conversations[0].contexts)
      .toEqual(expect.arrayContaining([expect.objectContaining({ id: queuedMessage.id })]));
    expect(durableSaves[1][0].workspaces[0].conversations[0].queuedMessages)
      .toEqual(expect.arrayContaining([expect.objectContaining({ id: queuedMessage.id })]));

    await act(async () => {
      await new Promise((resolve) => window.setTimeout(resolve, 500));
    });
    const stabilizedSaveCount = runtimeMocks.saveDocument.mock.calls.length;
    await act(async () => {
      await new Promise((resolve) => window.setTimeout(resolve, 500));
    });
    expect(runtimeMocks.saveDocument).toHaveBeenCalledTimes(stabilizedSaveCount);
    expect(promotionAttempts).toBe(1);
    expect(rollbackAttempts).toBe(1);
    expect(runtimeMocks.runModel).not.toHaveBeenCalled();
    expect(within(screen.getByRole("region", { name: "排队消息" }))
      .getByText("rollback-persistence-failure.png")).toBeInTheDocument();
  });
});
