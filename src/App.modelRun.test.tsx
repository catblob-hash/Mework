import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import App from "./App";
import { configureI18n } from "./i18n";
import type {
  AppDocument,
  ModelProfile,
  ModelRunResponse,
  ModelStreamEvent,
  SubagentRunRecord,
  ToolContext
} from "./types";
import {
  resetAppMocks,
  budgetImages,
  chooseComposerOption,
  composerOptionTrigger,
  composerOptionValue,
  deferred,
  documentWithModel,
  model,
  openComposerOption,
  runtimeMocks
} from "./test/appMocks";
import { emitAppPushEvent } from "./test/appMockInstances";
import { CONVERSATION_TURNS_STORAGE_KEY } from "./lib/conversationTurns";

vi.mock("./lib/appEvents", async () => (await import("./test/appMockInstances")).appEventsModuleMock());

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

describe("App model run flow — modelRun", () => {
  beforeEach(resetAppMocks);

  it("keeps reasoning effort per conversation and lets new conversations inherit the last selection", async () => {
    const document = documentWithModel();
    const first = document.workspaces[0].conversations[0];
    first.title = "低思考任务";
    first.settings.reasoningEffort = "low";
    const second = {
      ...first,
      id: "conv_second_effort",
      title: "高思考任务",
      createdAt: "2026-07-10T00:00:00Z",
      updatedAt: "2026-07-10T00:00:00Z",
      settings: { ...first.settings, reasoningEffort: "high" as const },
      contexts: []
    };
    document.workspaces[0].conversations = [first, second];
    document.globalSettings.lastReasoningEffort = "low";
    runtimeMocks.loadDocument.mockResolvedValue(document);

    const user = userEvent.setup();
    render(<App />);
    await screen.findByRole("button", { name: /^思考程度：/ });
    expect(composerOptionValue("思考程度")).toBe("低");

    await chooseComposerOption(user, "思考程度", "极高");
    expect(composerOptionValue("思考程度")).toBe("极高");
    await waitFor(() => expect(runtimeMocks.saveDocument.mock.calls.some((call) => {
      const saved = call[0] as AppDocument;
      return saved.globalSettings.lastReasoningEffort === "xhigh"
        && saved.workspaces[0].conversations.find((conversation) => conversation.id === first.id)?.settings.reasoningEffort === "xhigh"
        && saved.workspaces[0].conversations.find((conversation) => conversation.id === second.id)?.settings.reasoningEffort === "high";
    })).toBe(true));

    await user.click(screen.getByText("高思考任务"));
    expect(composerOptionValue("思考程度")).toBe("高");

    await user.click(screen.getByRole("button", { name: /^新建任务/ }));
    expect(composerOptionValue("思考程度")).toBe("极高");

    await user.click(screen.getByText("高思考任务"));
    expect(composerOptionValue("思考程度")).toBe("高");
  });

  it("offers exactly the five provider effort rungs and no orchestration mode", async () => {
    const document = documentWithModel();
    document.workspaces[0].conversations[0].settings.reasoningEffort = "low";
    runtimeMocks.loadDocument.mockResolvedValue(document);

    const user = userEvent.setup();
    render(<App />);

    await screen.findByRole("button", { name: /^思考程度：/ });
    expect(composerOptionValue("思考程度")).toBe("低");
    expect(composerOptionTrigger("思考程度")).toBeEnabled();
    // The retired ultracode rung must not come back: effort is a purely
    // provider-facing axis, and orchestration is the `workflow` tool, which the
    // user enables in the tool list like any other tool.
    const menu = await openComposerOption(user, "思考程度");
    expect(within(menu).getAllByRole("menuitemradio").map((item) => item.textContent)).toEqual([
      "关闭思考",
      "低",
      "中",
      "高",
      "极高"
    ]);

    await user.click(within(menu).getByRole("menuitemradio", { name: "高" }));
    expect(composerOptionValue("思考程度")).toBe("高");
    await waitFor(() => expect(runtimeMocks.saveDocument.mock.calls.some((call) => {
      const saved = call[0] as AppDocument;
      return saved.workspaces[0].conversations[0].settings.reasoningEffort === "high"
        && saved.globalSettings.lastReasoningEffort === "high";
    })).toBe(true));
  });

  it("shows no keyword affordance for the retired ultracode mode", async () => {
    runtimeMocks.loadDocument.mockResolvedValue(documentWithModel());

    const user = userEvent.setup();
    render(<App />);

    const composer = await screen.findByLabelText("向 Agent 发送消息");
    await user.type(composer, "ultracode 请开始");

    expect(screen.queryByText(/最高思考程度|maximum reasoning/i)).toBeNull();
    expect(screen.queryByRole("button", { name: /Ultracode/i })).toBeNull();
    // Typing the word is now just text: it must not change the effort picker.
    expect(composerOptionValue("思考程度")).toBe("关闭思考");
  });

  it("orders the composer safety, model, and reasoning controls", async () => {
    const document = documentWithModel();
    const firstConversation = document.workspaces[0].conversations[0];
    firstConversation.title = "请求批准任务";
    firstConversation.contexts = [{
      id: "ctx_composer_order",
      kind: "user",
      content: "保留上下文用量 chip",
      createdAt: "2026-07-11T00:00:00Z"
    }];
    const secondConversation = {
      ...firstConversation,
      id: "conv_full_access",
      title: "完全访问任务",
      settings: { ...firstConversation.settings, securityLevel: "full_access" as const },
      contexts: []
    };
    document.workspaces[0].conversations = [firstConversation, secondConversation];
    runtimeMocks.loadDocument.mockResolvedValue(document);

    const user = userEvent.setup();
    render(<App />);

    await screen.findByRole("button", { name: /^安全层级：/ });
    const securityPicker = composerOptionTrigger("安全层级");
    const modelPicker = composerOptionTrigger("模型");
    const reasoningPicker = composerOptionTrigger("思考程度");
    const tools = securityPicker.closest(".composer__tools");
    const options = modelPicker.closest(".composer__options");
    const addContent = screen.getByRole("button", { name: "添加内容" });
    const usage = options?.querySelector(".context-usage-meter");

    expect(tools).not.toBeNull();
    expect(options).not.toBeNull();
    expect(tools).toContainElement(securityPicker);
    expect(tools).toContainElement(addContent);
    expect(options).toContainElement(modelPicker);
    expect(options).toContainElement(reasoningPicker);
    expect(usage).not.toBeNull();
    expect(securityPicker.compareDocumentPosition(addContent) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(modelPicker.compareDocumentPosition(reasoningPicker) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(reasoningPicker.compareDocumentPosition(usage!) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(modelPicker).toHaveAttribute("title", "OpenAI Responses · test-model");

    await chooseComposerOption(user, "思考程度", "低");
    expect(composerOptionValue("思考程度")).toBe("低");

    await chooseComposerOption(user, "安全层级", "允许编辑");
    expect(composerOptionValue("安全层级")).toBe("允许编辑");
    await waitFor(() => expect(runtimeMocks.saveDocument.mock.calls.some((call) => (
      (call[0] as AppDocument).workspaces[0].conversations[0].settings.securityLevel === "allow_edits"
    ))).toBe(true));

    await user.click(screen.getByText("完全访问任务"));
    expect(composerOptionValue("安全层级")).toBe("完全访问");
    await user.click(screen.getByText("请求批准任务"));
    expect(composerOptionValue("安全层级")).toBe("允许编辑");
  });

  it("does not render placeholder overflow or tool-count controls", async () => {
    runtimeMocks.loadDocument.mockResolvedValue(documentWithModel());
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");
    expect(screen.queryByRole("button", { name: "更多操作" })).not.toBeInTheDocument();
    expect(screen.queryByText(/个可自动调用/)).not.toBeInTheDocument();
  });

  it("uses the provider and model selected in the composer", async () => {
    const document = documentWithModel();
    const secondaryModel: ModelProfile = {
      ...model,
      id: "secondary-model"
    };
    document.globalSettings.apiProviders = [
      {
        ...document.globalSettings.apiProviders[0],
        id: "provider-primary",
        name: "Primary Provider",
        enabled: true,
        models: [model],
        activeModelId: model.id
      },
      {
        ...document.globalSettings.apiProviders[1],
        id: "provider-secondary",
        name: "Secondary Provider",
        enabled: true,
        baseUrl: "http://localhost:8787/v1",
        models: [secondaryModel],
        activeModelId: secondaryModel.id
      }
    ];
    document.globalSettings.activeProviderId = "provider-primary";
    runtimeMocks.loadDocument.mockResolvedValue(document);
    runtimeMocks.runModel.mockResolvedValue({
      contexts: [{ id: "ctx_secondary", kind: "assistant", content: "Secondary reply", createdAt: "2026-07-11T00:00:00Z" }],
      usage: {},
      model: secondaryModel.id,
      providerName: "Secondary Provider",
      durationMs: 9
    });

    const user = userEvent.setup();
    render(<App />);
    await screen.findByRole("button", { name: /^模型：/ });
    await chooseComposerOption(user, "模型", new RegExp(`^${secondaryModel.id}.*Secondary Provider$`));
    expect(composerOptionValue("模型")).toBe("Secondary Provider · secondary-model");

    const composer = screen.getByLabelText("向 Agent 发送消息");
    await user.type(composer, "使用第二个提供商");
    await user.click(screen.getByRole("button", { name: "发送" }));

    await waitFor(() => expect(runtimeMocks.runModel).toHaveBeenCalledTimes(1));
    expect(runtimeMocks.runModel).toHaveBeenCalledWith(expect.objectContaining({
      provider: expect.objectContaining({ id: "provider-secondary", activeModelId: secondaryModel.id }),
      model: secondaryModel
    }), expect.any(Function), expect.any(String));
  });

  it("S11 完成唤醒：taskSettled 推送让空闲会话发起一次不带用户消息的运行", async () => {
    const document = documentWithModel();
    const conversation = document.workspaces[0].conversations[0];
    conversation.contexts = [
      { id: "ctx_user_seed", kind: "user", content: "跑个后台任务", createdAt: "2026-07-11T00:00:00Z" },
      { id: "ctx_assistant_seed", kind: "assistant", content: "已派生，回合结束。", createdAt: "2026-07-11T00:00:01Z" }
    ];
    runtimeMocks.loadDocument.mockResolvedValue(document);
    runtimeMocks.runModel.mockResolvedValue({
      contexts: [{ id: "ctx_wake_reply", kind: "assistant", content: "任务结果已送达", createdAt: "2026-07-11T00:00:02Z" }],
      usage: {},
      model: model.id,
      providerName: "Test Provider",
      durationMs: 5
    });

    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");

    await act(async () => {
      emitAppPushEvent({ type: "taskSettled", conversationId: conversation.id });
    });

    await waitFor(() => expect(runtimeMocks.runModel).toHaveBeenCalledTimes(1));
    const request = runtimeMocks.runModel.mock.calls[0][0];
    expect(request.conversationId).toBe(conversation.id);
    // A wake run adds no user message; it uses the existing timeline as request
    // context.
    expect(request.contexts.map((context: { id: string }) => context.id)).toEqual([
      "ctx_user_seed",
      "ctx_assistant_seed"
    ]);
    expect(await screen.findByText("任务结果已送达")).toBeInTheDocument();
  });

  it("S11 电平重扫：reload 丢掉的 taskSettled 记账由收养后的宿主重扫补回", async () => {
    const document = documentWithModel();
    const conversation = document.workspaces[0].conversations[0];
    conversation.contexts = [
      { id: "ctx_user_seed", kind: "user", content: "跑个后台任务", createdAt: "2026-07-11T00:00:00Z" },
      { id: "ctx_assistant_seed", kind: "assistant", content: "已派生，回合结束。", createdAt: "2026-07-11T00:00:01Z" }
    ];
    runtimeMocks.loadDocument.mockResolvedValue(document);
    // A host push may be missed. The post-adoption level scan is the only
    // signal in this case.
    runtimeMocks.listWakePendingConversations.mockResolvedValue([conversation.id]);
    runtimeMocks.runModel.mockResolvedValue({
      contexts: [{ id: "ctx_wake_reply", kind: "assistant", content: "补扫唤醒送达", createdAt: "2026-07-11T00:00:02Z" }],
      usage: {},
      model: model.id,
      providerName: "Test Provider",
      durationMs: 5
    });

    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");

    await waitFor(() => expect(runtimeMocks.runModel).toHaveBeenCalledTimes(1));
    const request = runtimeMocks.runModel.mock.calls[0][0];
    expect(request.conversationId).toBe(conversation.id);
    expect(request.contexts.map((context: { id: string }) => context.id)).toEqual([
      "ctx_user_seed",
      "ctx_assistant_seed"
    ]);
  });

  it("S11 收养失败不算完成：闩重开重试，成功后唤醒照常放行", async () => {
    const document = documentWithModel();
    const conversation = document.workspaces[0].conversations[0];
    conversation.contexts = [
      { id: "ctx_user_seed", kind: "user", content: "跑个后台任务", createdAt: "2026-07-11T00:00:00Z" },
      { id: "ctx_assistant_seed", kind: "assistant", content: "已派生，回合结束。", createdAt: "2026-07-11T00:00:01Z" }
    ];
    runtimeMocks.loadDocument.mockResolvedValue(document);
    // A failed adoption enumeration must reopen the retry latch. Wake-up may
    // proceed only after a successful retry.
    runtimeMocks.listResumableRuns
      .mockRejectedValueOnce(new Error("宿主暂不可用"))
      .mockResolvedValue([]);
    runtimeMocks.listWakePendingConversations.mockResolvedValue([conversation.id]);
    runtimeMocks.runModel.mockResolvedValue({
      contexts: [{ id: "ctx_wake_reply", kind: "assistant", content: "重试后唤醒送达", createdAt: "2026-07-11T00:00:02Z" }],
      usage: {},
      model: model.id,
      providerName: "Test Provider",
      durationMs: 5
    });

    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");

    await waitFor(
      () => expect(runtimeMocks.runModel).toHaveBeenCalledTimes(1),
      { timeout: 8000 }
    );
    expect(runtimeMocks.listResumableRuns.mock.calls.length).toBeGreaterThanOrEqual(2);
  }, 15000);

  it("keeps the draft and opens provider settings when no model is selected", async () => {
    const document = documentWithModel();
    document.globalSettings.apiProviders[0].models = [];
    document.globalSettings.apiProviders[0].activeModelId = null;
    runtimeMocks.loadDocument.mockResolvedValue(document);

    const user = userEvent.setup();
    render(<App />);
    const composer = await screen.findByLabelText("向 Agent 发送消息");
    await user.type(composer, "不要丢掉这段草稿");
    await user.keyboard("{Enter}");

    expect(runtimeMocks.runModel).not.toHaveBeenCalled();
    expect(composer).toHaveValue("不要丢掉这段草稿");
    expect(await screen.findByRole("navigation", { name: "全局设置分类" })).toBeInTheDocument();
  });

  it("keeps a persistent model error and retries without duplicating the user context", async () => {
    const document = documentWithModel();
    const retryModel: ModelProfile = {
      ...model,
      id: "retry-provider-model"
    };
    document.globalSettings.apiProviders.push({
      ...document.globalSettings.apiProviders[1],
      id: "provider-retry",
      name: "Retry Provider",
      enabled: true,
      baseUrl: "http://localhost:8989/v1",
      models: [retryModel],
      activeModelId: retryModel.id
    });
    runtimeMocks.loadDocument.mockResolvedValue(document);
    runtimeMocks.runModel
      .mockRejectedValueOnce(new Error("网络中断"))
      .mockResolvedValueOnce({
        contexts: [{ id: "ctx_retry", kind: "assistant", content: "重试成功", createdAt: "2026-07-11T00:00:00Z" }],
        usage: {},
        model: retryModel.id,
        providerName: "Retry Provider",
        durationMs: 8
      });

    const user = userEvent.setup();
    render(<App />);
    const composer = await screen.findByLabelText("向 Agent 发送消息");
    await user.type(composer, "只发送一次");
    await user.click(screen.getByRole("button", { name: "发送" }));
    expect(await screen.findByText("网络中断")).toBeInTheDocument();

    await chooseComposerOption(user, "模型", new RegExp(`^${retryModel.id}.*Retry Provider$`));
    await user.click(screen.getByRole("button", { name: "重试" }));
    expect(await screen.findByText("重试成功")).toBeInTheDocument();
    expect(runtimeMocks.runModel).toHaveBeenCalledTimes(2);
    expect(runtimeMocks.runModel).toHaveBeenNthCalledWith(2, expect.objectContaining({
      provider: expect.objectContaining({ id: "provider-retry", activeModelId: retryModel.id }),
      model: retryModel
    }), expect.any(Function), expect.any(String));
    expect(screen.getAllByText("只发送一次").filter((element) => element.closest(".context-card--user"))).toHaveLength(1);
    expect(screen.queryByText("网络中断")).not.toBeInTheDocument();
  });

  it("keeps retry available but blocks it when generated history exceeds the request image budget", async () => {
    const document = documentWithModel();
    document.globalSettings.apiProviders[0].models[0] = {
      ...document.globalSettings.apiProviders[0].models[0],
      capabilities: ["image_recognition"]
    };
    runtimeMocks.loadDocument.mockResolvedValue(document);
    const imageTool = (id: string, prefix: string): ToolContext => ({
      id,
      kind: "tool",
      toolName: "read",
      round: 1,
      input: { path: `${prefix}.png` },
      result: {
        success: true,
        output: "图片",
        images: budgetImages(prefix, 11),
        executedAt: "2026-07-24T00:00:01Z",
        durationMs: 1
      },
      createdAt: "2026-07-24T00:00:01Z"
    });
    runtimeMocks.runModel.mockResolvedValueOnce({
      contexts: [
        imageTool("retry-budget-tool-a", "retry-budget-a"),
        imageTool("retry-budget-tool-b", "retry-budget-b")
      ],
      usage: {},
      model: model.id,
      providerName: "OpenAI Responses",
      durationMs: 2,
      stopReason: "error",
      error: { message: "续接失败", round: 2, attempts: 1 }
    });

    const user = userEvent.setup();
    render(<App />);
    await user.type(await screen.findByLabelText("向 Agent 发送消息"), "触发带图失败");
    await user.click(screen.getByRole("button", { name: "发送" }));
    const retry = await screen.findByRole("button", { name: "重试" });
    expect(runtimeMocks.runModel).toHaveBeenCalledTimes(1);

    await user.click(retry);

    // A budget rejection sends no request but leaves retry available.
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    expect(runtimeMocks.runModel).toHaveBeenCalledTimes(1);
    expect(screen.getByRole("button", { name: "重试" })).toBeInTheDocument();
  });

  it("keeps a failed image request available when retry switches to a text-only model", async () => {
    const document = documentWithModel();
    const visionModel: ModelProfile = {
      ...model,
      id: "vision-model",
      capabilities: ["image_recognition"]
    };
    const textOnlyModel: ModelProfile = {
      ...model,
      id: "text-only-model",
      capabilities: []
    };
    document.globalSettings.apiProviders[0].models = [visionModel, textOnlyModel];
    document.globalSettings.apiProviders[0].activeModelId = visionModel.id;
    document.workspaces[0].conversations[0].contexts = [{
      id: "retry-image-user",
      kind: "user",
      content: "",
      images: [{
        id: "retry-image",
        name: "retry.png",
        mime: "image/png",
        width: 32,
        height: 32,
        bytes: 64
      }],
      createdAt: "2026-07-20T00:00:00Z"
    }];
    runtimeMocks.loadDocument.mockResolvedValue(document);
    runtimeMocks.runModel.mockResolvedValue({
      contexts: [],
      usage: {},
      model: visionModel.id,
      providerName: "OpenAI Responses",
      durationMs: 5,
      stopReason: "error",
      error: { message: "图像请求暂时失败", round: 1, attempts: 1 }
    });

    const user = userEvent.setup();
    render(<App />);
    await user.type(await screen.findByLabelText("向 Agent 发送消息"), "重试这张图");
    await user.click(screen.getByRole("button", { name: "发送" }));
    expect(await screen.findByRole("button", { name: "重试" })).toBeInTheDocument();
    await chooseComposerOption(user, "模型", new RegExp(`^${textOnlyModel.id}.*OpenAI Responses$`));
    await user.click(screen.getByRole("button", { name: "重试" }));

    // A text-only model rejects this retry without sending a request or
    // consuming the retryable error.
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    expect(runtimeMocks.runModel).toHaveBeenCalledTimes(1);
    expect(screen.getByRole("button", { name: "重试" })).toBeInTheDocument();
  });

  it("sends the selected reasoning effort and excludes disabled providers", async () => {
    const document = documentWithModel();
    // Presence in the provider's list is now the whole of usability, so a second
    // installed model must be offered while a disabled provider stays hidden.
    const secondModel: ModelProfile = { ...model, id: "second-model" };
    const hiddenProviderModel: ModelProfile = { ...model, id: "hidden-provider-model" };
    document.globalSettings.apiProviders[0].models.push(secondModel);
    document.globalSettings.apiProviders.push({
      ...document.globalSettings.apiProviders[1],
      id: "provider-disabled",
      name: "Disabled Provider",
      enabled: false,
      baseUrl: "http://localhost:7878/v1",
      models: [hiddenProviderModel],
      activeModelId: hiddenProviderModel.id
    });
    runtimeMocks.loadDocument.mockResolvedValue(document);
    runtimeMocks.runModel.mockResolvedValue({
      contexts: [{ id: "ctx_reasoning", kind: "assistant", content: "Reasoned reply", createdAt: "2026-07-11T00:00:00Z" }],
      usage: {},
      model: model.id,
      providerName: "OpenAI Responses",
      durationMs: 7
    });

    const user = userEvent.setup();
    render(<App />);
    await screen.findByRole("button", { name: /^模型：/ });
    const modelMenu = await openComposerOption(user, "模型");
    expect(within(modelMenu).getByRole("menuitemradio", { name: /^test-model.*OpenAI Responses$/ })).toBeInTheDocument();
    expect(within(modelMenu).queryByRole("menuitemradio", { name: /^second-model.*OpenAI Responses$/ })).toBeInTheDocument();
    expect(within(modelMenu).queryByRole("menuitemradio", { name: /^hidden-provider-model.*Disabled Provider$/ })).not.toBeInTheDocument();

    await user.click(within(modelMenu).getByRole("menuitemradio", { name: /^test-model.*OpenAI Responses$/ }));
    await chooseComposerOption(user, "思考程度", "高");
    await user.type(screen.getByLabelText("向 Agent 发送消息"), "深入思考");
    await user.click(screen.getByRole("button", { name: "发送" }));

    await waitFor(() => expect(runtimeMocks.runModel).toHaveBeenCalledTimes(1));
    expect(runtimeMocks.runModel).toHaveBeenCalledWith(expect.objectContaining({
      provider: expect.objectContaining({ id: document.globalSettings.apiProviders[0].id, enabled: true }),
      model,
      reasoningEffort: "high"
    }), expect.any(Function), expect.any(String));
  });

  it("keeps a stopped run visible until the backend returns its settlement", async () => {
    const document = documentWithModel();
    runtimeMocks.loadDocument.mockResolvedValue(document);
    let emit!: (event: ModelStreamEvent) => void;
    let resolveRun!: (value: unknown) => void;
    runtimeMocks.runModel.mockImplementation((_request, onEvent) => {
      emit = onEvent;
      return new Promise((resolve) => { resolveRun = resolve; });
    });

    const user = userEvent.setup();
    render(<App />);
    const composer = await screen.findByLabelText("向 Agent 发送消息");
    await user.type(composer, "稍后返回");
    await user.click(screen.getByRole("button", { name: "发送" }));
    act(() => emit({ type: "text_delta", round: 1, delta: "停止前已经生成的内容" }));
    expect(await screen.findByText("停止前已经生成的内容")).toBeInTheDocument();
    const stopButton = await screen.findByRole("button", { name: "停止生成" });
    expect(stopButton.querySelector(".stop-icon")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "停止等待" })).not.toBeInTheDocument();
    expect(screen.getByRole("status", { name: "模型正在生成" })).toHaveAttribute("data-stream-waiting", "true");
    await user.click(stopButton);
    await waitFor(() => expect(runtimeMocks.cancelModelRun).toHaveBeenCalledWith(expect.any(String)));
    expect(screen.getByRole("button", { name: "正在停止生成" })).toBeDisabled();
    expect(screen.getByRole("status", { name: "模型正在生成" })).toBeInTheDocument();

    await act(async () => resolveRun({
      contexts: [],
      usage: {},
      model: model.id,
      providerName: "OpenAI Responses",
      durationMs: 500,
      stopReason: "cancelled"
    }));
    const partial = screen.getByText("停止前已经生成的内容").closest("article");
    expect(partial).not.toHaveAttribute("aria-busy");
    expect(partial?.closest("[data-turn-id]")).not.toBeNull();
    expect(screen.getByRole("button", { name: /test-model 在 .* 后中止/ }))
      .toHaveAttribute("aria-expanded", "true");
    expect(screen.queryByRole("status", { name: "模型正在生成" })).not.toBeInTheDocument();
    await waitFor(() => expect(runtimeMocks.saveDocument.mock.calls.some(([saved]) => (
      JSON.stringify(saved).includes("停止前已经生成的内容")
      && JSON.stringify(saved).includes('"interrupted":true')
    ))).toBe(true));
  });

  it("discards the round outright when a stop lands before the first message", async () => {
    const document = documentWithModel();
    runtimeMocks.loadDocument.mockResolvedValue(document);
    let resolveFirst!: (value: ModelRunResponse) => void;
    let emitSecond!: (event: ModelStreamEvent) => void;
    runtimeMocks.runModel
      .mockImplementationOnce(() => new Promise<ModelRunResponse>((resolve) => { resolveFirst = resolve; }))
      .mockImplementationOnce((_request, onEvent) => {
        emitSecond = onEvent;
        return new Promise<ModelRunResponse>(() => undefined);
      });

    const user = userEvent.setup();
    const { container } = render(<App />);
    await user.type(await screen.findByLabelText("向 Agent 发送消息"), "刚发出去就后悔了");
    await user.click(screen.getByRole("button", { name: "发送" }));
    await waitFor(() => expect(runtimeMocks.runModel).toHaveBeenCalledTimes(1));

    // Stopped before a single delta arrived, so the round produced nothing.
    await user.click(await screen.findByRole("button", { name: "停止生成" }));
    await waitFor(() => expect(runtimeMocks.cancelModelRun).toHaveBeenCalled());
    await act(async () => resolveFirst({
      contexts: [],
      usage: {},
      model: model.id,
      providerName: "Test Provider",
      durationMs: 4_000,
      stopReason: "cancelled"
    }));

    // A header over an empty body says nothing the timeline does not already
    // show, and reads as a round that lost its messages.
    expect(container.querySelectorAll("[data-turn-id]")).toHaveLength(0);
    expect(screen.queryByRole("button", { name: /test-model 在 .* 后中止/ })).not.toBeInTheDocument();
    expect(screen.getAllByText("刚发出去就后悔了").some(
      (element) => element.closest(".context-card--user")
    )).toBe(true);

    // Discarded, not merely hidden. A round that produced nothing is not a
    // round, so nothing survives in storage for a later run to continue.
    expect(Object.values(
      JSON.parse(window.localStorage.getItem(CONVERSATION_TURNS_STORAGE_KEY) ?? "{}") as Record<string, unknown[]>
    ).flat()).toHaveLength(0);

    // Sending again therefore opens a round of its own, starting at zero rather
    // than inheriting the four seconds the stopped attempt spent waiting.
    await user.click(screen.getByRole("button", { name: "发送" }));
    await waitFor(() => expect(runtimeMocks.runModel).toHaveBeenCalledTimes(2));
    act(() => emitSecond({ type: "text_delta", round: 1, delta: "这次来得及回答" }));
    expect(await screen.findByText("这次来得及回答")).toBeInTheDocument();
    const opened = await screen.findByRole("button", { name: /test-model 运行了/ });
    expect(opened).toHaveAccessibleName(/运行了 0s|运行了 1s/);
    expect(container.querySelectorAll("[data-turn-id]")).toHaveLength(1);
  });

  it("continues the round a stop left unfinished when the composer is empty, even after its messages are deleted", async () => {
    const document = documentWithModel();
    runtimeMocks.loadDocument.mockResolvedValue(document);
    let emitFirst!: (event: ModelStreamEvent) => void;
    let emitSecond!: (event: ModelStreamEvent) => void;
    let resolveFirst!: (value: ModelRunResponse) => void;
    let resolveSecond!: (value: ModelRunResponse) => void;
    runtimeMocks.runModel
      .mockImplementationOnce((_request, onEvent) => {
        emitFirst = onEvent;
        return new Promise<ModelRunResponse>((resolve) => { resolveFirst = resolve; });
      })
      .mockImplementationOnce((_request, onEvent) => {
        emitSecond = onEvent;
        return new Promise<ModelRunResponse>((resolve) => { resolveSecond = resolve; });
      });

    const user = userEvent.setup();
    const { container } = render(<App />);
    await user.type(await screen.findByLabelText("向 Agent 发送消息"), "第一轮问题");
    await user.click(screen.getByRole("button", { name: "发送" }));
    await waitFor(() => expect(runtimeMocks.runModel).toHaveBeenCalledTimes(1));
    act(() => {
      emitFirst({
        type: "usage_updated",
        round: 1,
        usage: { inputTokens: 100, cachedInputTokens: 20, outputTokens: 7, totalTokens: 127 }
      });
      emitFirst({ type: "text_delta", round: 1, delta: "只写到一半" });
    });
    await user.click(await screen.findByRole("button", { name: "停止生成" }));
    await waitFor(() => expect(runtimeMocks.cancelModelRun).toHaveBeenCalled());
    await act(async () => resolveFirst({
      contexts: [],
      usage: { inputTokens: 100, cachedInputTokens: 20, outputTokens: 7, totalTokens: 127 },
      model: model.id,
      providerName: "Test Provider",
      durationMs: 12_000,
      stopReason: "cancelled"
    }));
    await screen.findByRole("button", { name: /test-model 在 12s 后中止/ });

    // The user throws the whole stopped round away, anchor message included.
    for (
      let remaining = screen.queryAllByRole("button", { name: "删除上下文" });
      remaining.length;
      remaining = screen.queryAllByRole("button", { name: "删除上下文" })
    ) {
      await user.click(remaining[0]);
    }
    await waitFor(() => expect(container.querySelectorAll("[data-context-id]")).toHaveLength(0));

    // Sending with nothing typed is not a new round; it resumes the one the
    // stop left open, so its elapsed time and token counters keep running.
    await user.click(screen.getByRole("button", { name: "发送" }));
    await waitFor(() => expect(runtimeMocks.runModel).toHaveBeenCalledTimes(2));
    expect(runtimeMocks.runModel.mock.calls[1][0].contexts).toEqual([]);
    act(() => emitSecond({
      type: "usage_updated",
      round: 1,
      usage: { inputTokens: 140, cachedInputTokens: 30, outputTokens: 5, totalTokens: 175 }
    }));
    expect(await screen.findByRole("button", {
      name: /test-model 运行了 .*输入240.*缓存输入50.*输出12/
    })).toHaveAttribute("aria-expanded", "true");

    await act(async () => resolveSecond({
      contexts: [
        {
          id: "ctx_continued_reasoning",
          kind: "reasoning",
          content: "接着上一轮补完",
          createdAt: "2026-07-24T00:00:03Z"
        },
        {
          id: "ctx_continued_final",
          kind: "assistant",
          content: "补完了被打断的那一轮",
          createdAt: "2026-07-24T00:00:04Z"
        }
      ],
      usage: { inputTokens: 140, cachedInputTokens: 30, outputTokens: 9, totalTokens: 179 },
      model: model.id,
      providerName: "Test Provider",
      durationMs: 8_000,
      stopReason: "completed"
    }));

    // 12s + 8s of wall clock, and both requests' input, cached-input and output
    // counters added up — the real cost of the round, not just its last leg.
    const completed = await screen.findByRole("button", {
      name: /test-model 运行了 20s.*输入240.*缓存输入50.*输出16/
    });
    expect(container.querySelectorAll("[data-turn-id]")).toHaveLength(1);
    expect(completed).toHaveAttribute("aria-expanded", "false");
    expect(container.querySelector('[data-context-id="ctx_continued_reasoning"]')?.closest("[data-turn-id]"))
      .not.toBeNull();
    expect(screen.getByText("补完了被打断的那一轮")).toBeInTheDocument();
  });

  it("adopts a host-owned run on load, replays its buffer and applies the settlement", async () => {
    const document = documentWithModel();
    const conversationId = document.workspaces[0].conversations[0].id;
    runtimeMocks.loadDocument.mockResolvedValue(document);
    runtimeMocks.listResumableRuns.mockResolvedValue([
      { conversationId, requestId: "run_adopted", running: true }
    ]);
    let adoptedOnEvent!: (event: ModelStreamEvent) => void;
    runtimeMocks.attachModelRun.mockImplementation(async (
      _conversationId: string,
      onEvent: (event: ModelStreamEvent) => void
    ) => {
      adoptedOnEvent = onEvent;
      // Buffered events replayed before the attach response must enter the live
      // projection unchanged.
      onEvent({ type: "text_delta", round: 1, delta: "重放的" });
      onEvent({ type: "text_delta", round: 1, delta: "直播内容" });
      return {
        status: "running" as const,
        requestId: "run_adopted",
        request: {
          provider: document.globalSettings.apiProviders[0],
          model,
          reasoningEffort: "medium" as const,
          conversationId,
          workspacePath: "C:/workspace",
          systemPrompt: "",
          enabledTools: [],
          contexts: [],
          tools: []
        },
        droppedEvents: 0
      };
    });
    runtimeMocks.takeRunSettlement.mockResolvedValue({
      response: {
        contexts: [{
          id: "ctx_adopted",
          kind: "assistant",
          content: "重放的直播内容",
          createdAt: "2026-07-11T00:00:00Z"
        }],
        usage: {},
        model: model.id,
        providerName: "OpenAI Responses",
        durationMs: 5
      } satisfies ModelRunResponse
    });

    render(<App />);
    expect(await screen.findByText("重放的直播内容")).toBeInTheDocument();
    expect(globalThis.document.querySelectorAll("[data-turn-id]")).toHaveLength(1);
    expect(screen.getByText("重放的直播内容").closest("[data-turn-id]")).not.toBeNull();
    expect(runtimeMocks.runModel).not.toHaveBeenCalled();
    expect(screen.getByRole("status", { name: "模型正在生成" })).toBeInTheDocument();

    // `run_concluded` claims the settlement slot before removing the run while
    // retaining finalized content.
    act(() => adoptedOnEvent({ type: "run_concluded", requestId: "run_adopted" }));
    await waitFor(() => expect(runtimeMocks.takeRunSettlement).toHaveBeenCalledWith(conversationId));
    await waitFor(() => (
      expect(screen.queryByRole("status", { name: "模型正在生成" })).not.toBeInTheDocument()
    ));
    expect(screen.getByText("重放的直播内容")).toBeInTheDocument();
  });

  it("applies the retained settlement of a run that finished while the renderer was away", async () => {
    const document = documentWithModel();
    const conversationId = document.workspaces[0].conversations[0].id;
    runtimeMocks.loadDocument.mockResolvedValue(document);
    runtimeMocks.listResumableRuns.mockResolvedValue([
      { conversationId, requestId: "run_finished_away", running: false }
    ]);
    runtimeMocks.attachModelRun.mockResolvedValue({
      status: "finished" as const,
      requestId: "run_finished_away",
      request: {
        provider: document.globalSettings.apiProviders[0],
        model,
        reasoningEffort: "medium" as const,
        conversationId,
        workspacePath: "C:/workspace",
        systemPrompt: "",
        enabledTools: [],
        contexts: [],
        tools: []
      },
      settlement: {
        response: {
          contexts: [{
            id: "ctx_finished_away",
            kind: "assistant",
            content: "离场期间完成的回复",
            createdAt: "2026-07-11T00:00:00Z"
          }],
          usage: {},
          model: model.id,
          providerName: "OpenAI Responses",
          durationMs: 5
        } satisfies ModelRunResponse
      }
    });

    render(<App />);
    expect(await screen.findByText("离场期间完成的回复")).toBeInTheDocument();
    expect(runtimeMocks.runModel).not.toHaveBeenCalled();
    await waitFor(() => (
      expect(screen.queryByRole("status", { name: "模型正在生成" })).not.toBeInTheDocument()
    ));
    // Finalized content is persisted through the same path used for initiated
    // runs.
    await waitFor(() => expect(runtimeMocks.saveDocument.mock.calls.some(([saved]) => (
      JSON.stringify(saved).includes("离场期间完成的回复")
    ))).toBe(true));
  });

  it("keeps a run visible when native cancellation cleanup is not acknowledged", async () => {
    const document = documentWithModel();
    runtimeMocks.loadDocument.mockResolvedValue(document);
    const cancellation = deferred<boolean>();
    runtimeMocks.cancelModelRun.mockReturnValue(cancellation.promise);
    let resolveRun!: (value: unknown) => void;
    runtimeMocks.runModel.mockImplementation(() => new Promise((resolve) => {
      resolveRun = resolve;
    }));

    const user = userEvent.setup();
    render(<App />);
    await user.type(await screen.findByLabelText("向 Agent 发送消息"), "保持停止边界");
    await user.click(screen.getByRole("button", { name: "发送" }));
    await waitFor(() => expect(runtimeMocks.runModel).toHaveBeenCalledTimes(1));

    await user.click(screen.getByRole("button", { name: "停止生成" }));
    await waitFor(() => expect(runtimeMocks.cancelModelRun).toHaveBeenCalledTimes(1));
    expect(screen.getByRole("button", { name: "正在停止生成" })).toBeDisabled();
    expect(screen.getByRole("status", { name: "模型正在生成" })).toBeInTheDocument();

    // If cleanup is not acknowledged, stopping fails silently but the control
    // becomes available again and the run remains visible.
    await act(async () => cancellation.reject(new Error("input cleanup not acknowledged")));
    await waitFor(() => expect(screen.getByRole("button", { name: "停止生成" })).toBeEnabled());
    expect(screen.getByRole("status", { name: "模型正在生成" })).toBeInTheDocument();

    await act(async () => resolveRun({
      contexts: [],
      usage: {},
      model: model.id,
      providerName: "OpenAI Responses",
      durationMs: 1,
      stopReason: "completed"
    }));
  });

  it("persists completed tools and the trusted Interrupted child settlement", async () => {
    const document = documentWithModel();
    runtimeMocks.loadDocument.mockResolvedValue(document);
    let emit!: (event: ModelStreamEvent) => void;
    let resolveRun!: (value: unknown) => void;
    runtimeMocks.runModel.mockImplementation((_request, onEvent) => {
      emit = onEvent;
      return new Promise((resolve) => { resolveRun = resolve; });
    });

    const user = userEvent.setup();
    render(<App />);
    await user.type(await screen.findByLabelText("向 Agent 发送消息"), "中断工具轮");
    await user.click(screen.getByRole("button", { name: "发送" }));
    await waitFor(() => expect(runtimeMocks.runModel).toHaveBeenCalledTimes(1));

    act(() => {
      emit({ type: "reasoning_delta", round: 1, delta: "先完成读取，再运行子代理。" });
      emit({ type: "tool_call_announced", round: 1, callId: "call-read-complete", toolName: "read", contextId: "ctx-call-read-complete" });
      emit({ type: "tool_call_announced", round: 1, callId: "call-subagent-running", toolName: "subagent", contextId: "ctx-call-subagent-running" });
      emit({ type: "tool_call_arguments_ready", round: 1, callId: "call-read-complete", input: { path: "README.md" } });
      emit({
        type: "tool_call_arguments_ready",
        round: 1,
        callId: "call-subagent-running",
        input: { task: "检查中断持久化", label: "中断审查" }
      });
      emit({ type: "tool_execution_started", round: 1, callId: "call-read-complete" });
      emit({
        type: "tool_execution_completed",
        round: 1,
        callId: "call-read-complete",
        result: {
          success: true,
          output: "read completed",
          executedAt: "2026-07-21T00:00:00Z",
          durationMs: 4
        }
      });
      emit({ type: "tool_execution_started", round: 1, callId: "call-subagent-running" });
      emit({
        type: "subagent_delta",
        round: 1,
        callId: "call-subagent-running",
        channel: "update",
        delta: "已开始检查"
      });
    });

    await user.click(await screen.findByRole("button", { name: "停止生成" }));
    await act(async () => resolveRun({
      contexts: [{
        id: "ctx-call-read-complete",
        kind: "tool",
        toolName: "read",
        round: 1,
        input: { path: "README.md" },
        result: {
          success: true,
          output: "read completed",
          executedAt: "2026-07-21T00:00:00Z",
          durationMs: 4
        },
        createdAt: "2026-07-21T00:00:00Z"
      }, {
        id: "ctx-call-subagent-running",
        kind: "tool",
        toolName: "subagent",
        round: 1,
        input: { task: "检查中断持久化", label: "中断审查" },
        result: {
          success: true,
          output: "子代理已派生",
          executedAt: "2026-07-21T00:00:01Z",
          durationMs: 1
        },
        subagent: {
          task: "检查中断持久化",
          status: "interrupted",
          contexts: [],
          updates: [{ content: "已开始检查", createdAt: "2026-07-21T00:00:02Z" }]
        },
        attestation: "host-attestation",
        createdAt: "2026-07-21T00:00:01Z"
      }],
      usage: {},
      model: model.id,
      providerName: "OpenAI Responses",
      durationMs: 50,
      stopReason: "cancelled"
    }));
    // The completed read and the child's Interrupted record both come from the
    // backend settlement. The renderer may keep partial text, but it never
    // invents the addressable child record or its receipt.
    await waitFor(() => expect(runtimeMocks.saveDocument.mock.calls.some(([saved]) => {
      const contexts = (saved as AppDocument).workspaces
        .flatMap((workspace) => workspace.conversations)
        .flatMap((conversation) => conversation.contexts);
      return contexts.some((context) => (
        context.kind === "tool" && context.toolName === "read" && context.result.output === "read completed"
      ));
    })).toBe(true));
    const settledSubagent = runtimeMocks.saveDocument.mock.calls
      .map(([saved]) => (saved as AppDocument).workspaces
        .flatMap((workspace) => workspace.conversations)
        .flatMap((conversation) => conversation.contexts)
        .find((context): context is ToolContext => (
          context.kind === "tool" && context.toolName === "subagent" && context.subagent !== undefined
        )))
      .find((context) => context !== undefined);
    expect(settledSubagent?.subagent?.status).toBe("interrupted");
    expect(settledSubagent?.attestation).toBe("host-attestation");
  });

  it("never synthesizes an unsigned addressable subagent record from an interrupted stream", async () => {
    const document = documentWithModel();
    runtimeMocks.loadDocument.mockResolvedValue(document);
    let emit!: (event: ModelStreamEvent) => void;
    let resolveRun!: (value: unknown) => void;
    runtimeMocks.runModel.mockImplementation((_request, onEvent) => {
      emit = onEvent;
      return new Promise((resolve) => { resolveRun = resolve; });
    });

    const user = userEvent.setup();
    render(<App />);
    await user.type(await screen.findByLabelText("向 Agent 发送消息"), "派生后立即中断");
    await user.click(screen.getByRole("button", { name: "发送" }));
    await waitFor(() => expect(runtimeMocks.runModel).toHaveBeenCalledTimes(1));

    act(() => {
      emit({ type: "tool_call_announced", round: 1, callId: "call-addressable-running", toolName: "agent_spawn", contextId: "ctx-call-addressable-running" });
      emit({
        type: "tool_call_arguments_ready",
        round: 1,
        callId: "call-addressable-running",
        input: { task: "检查身份持久化", name: "receipt-reviewer", label: "收据审查" }
      });
      emit({ type: "tool_execution_started", round: 1, callId: "call-addressable-running" });
      emit({
        type: "tool_execution_completed",
        round: 1,
        callId: "call-addressable-running",
        result: {
          success: true,
          output: "ok",
          executedAt: "2026-07-24T00:00:00Z",
          durationMs: 0
        }
      });
      emit({
        type: "subagent_delta",
        round: 1,
        callId: "call-addressable-running",
        channel: "update",
        delta: "尚未收到宿主签名的最终记录"
      });
    });

    await user.click(await screen.findByRole("button", { name: "停止生成" }));
    await act(async () => resolveRun({
      contexts: [],
      usage: {},
      model: model.id,
      providerName: "OpenAI Responses",
      durationMs: 50,
      stopReason: "cancelled"
    }));
    await waitFor(() => expect(runtimeMocks.saveDocument.mock.calls.some(([saved]) => {
      const savedDocument = saved as AppDocument;
      const persisted = savedDocument.workspaces
        .flatMap((workspace) => workspace.conversations)
        .flatMap((conversation) => conversation.contexts)
        .find((context): context is ToolContext => (
          context.kind === "tool"
          && context.toolName === "agent_spawn"
          && context.input.name === "receipt-reviewer"
        ));
      return persisted !== undefined
        && persisted.result.output === "ok"
        && persisted.subagent === undefined
        && persisted.live === undefined;
    })).toBe(true));
  });

  it("leaves no unattested tool result behind after an interrupt, so later saves still go through", async () => {
    const document = documentWithModel();
    runtimeMocks.loadDocument.mockResolvedValue(document);
    let emit!: (event: ModelStreamEvent) => void;
    let resolveRun!: (value: unknown) => void;
    runtimeMocks.runModel.mockImplementation((_request, onEvent) => {
      emit = onEvent;
      return new Promise((resolve) => { resolveRun = resolve; });
    });

    const user = userEvent.setup();
    render(<App />);
    await user.type(await screen.findByLabelText("向 Agent 发送消息"), "中断后继续保存");
    await user.click(screen.getByRole("button", { name: "发送" }));
    await waitFor(() => expect(runtimeMocks.runModel).toHaveBeenCalledTimes(1));

    act(() => {
      emit({ type: "text_delta", round: 1, delta: "中断前已经收到的正文" });
      emit({ type: "tool_call_announced", round: 1, callId: "call-never-returns", toolName: "read", contextId: "ctx-call-never-returns" });
      emit({
        type: "tool_call_arguments_ready",
        round: 1,
        callId: "call-never-returns",
        input: { path: "NEVER.md" }
      });
      emit({ type: "tool_execution_started", round: 1, callId: "call-never-returns" });
    });

    await user.click(await screen.findByRole("button", { name: "停止生成" }));
    resolveRun({
      contexts: [],
      usage: {},
      model: model.id,
      providerName: "OpenAI Responses",
      durationMs: 50,
      stopReason: "cancelled"
    });

    // Every tool context the renderer persists must carry a result the backend
    // actually returned. `validate_tool_receipts` rejects the entire document
    // over a single unattested one, and because the poisoned context is saved
    // again on every later write, one interrupt would stop the app persisting
    // anything at all. A call cut off before its result has no receipt to
    // match, so it must not reach the document.
    await waitFor(() => expect(runtimeMocks.saveDocument.mock.calls.some(([saved]) => (
      JSON.stringify(saved).includes("中断前已经收到的正文")
    ))).toBe(true));
    for (const [saved] of runtimeMocks.saveDocument.mock.calls) {
      const tools = (saved as AppDocument).workspaces
        .flatMap((workspace) => workspace.conversations)
        .flatMap((conversation) => conversation.contexts)
        .filter((context): context is ToolContext => context.kind === "tool");
      expect(tools.some((tool) => tool.input.path === "NEVER.md")).toBe(false);
      for (const tool of tools) {
        expect(tool.streaming).toBeUndefined();
        expect(tool.live).toBeUndefined();
      }
    }
  });

  it("persists streamed output when the model request fails", async () => {
    const document = documentWithModel();
    runtimeMocks.loadDocument.mockResolvedValue(document);
    let emit!: (event: ModelStreamEvent) => void;
    let rejectRun!: (error: Error) => void;
    runtimeMocks.runModel.mockImplementation((_request, onEvent) => {
      emit = onEvent;
      return new Promise((_resolve, reject) => { rejectRun = reject; });
    });

    const user = userEvent.setup();
    render(<App />);
    await user.type(await screen.findByLabelText("向 Agent 发送消息"), "保留异常前的输出");
    await user.click(screen.getByRole("button", { name: "发送" }));
    act(() => emit({ type: "text_delta", round: 1, delta: "连接断开前已经收到的内容" }));
    expect(await screen.findByText("连接断开前已经收到的内容")).toBeInTheDocument();

    await act(async () => rejectRun(new Error("连接意外断开")));
    expect(await screen.findByText("连接意外断开")).toBeInTheDocument();
    const partial = screen.getByText("连接断开前已经收到的内容").closest("article");
    expect(partial).not.toHaveAttribute("aria-busy");
    await waitFor(() => expect(runtimeMocks.saveDocument.mock.calls.some(([saved]) => (
      JSON.stringify(saved).includes("连接断开前已经收到的内容")
    ))).toBe(true));
    expect(runtimeMocks.saveDocument.mock.calls.some(([saved]) => {
      const serialized = JSON.stringify(saved);
      return serialized.includes("连接断开前已经收到的内容")
        && serialized.includes('"interrupted":true');
    })).toBe(true);
  });

  it("reads a first-round connection failure inside its turn instead of leaving an empty header", async () => {
    const document = documentWithModel();
    runtimeMocks.loadDocument.mockResolvedValue(document);
    runtimeMocks.runModel
      .mockRejectedValueOnce(new Error("无法连接 API：端口错误"))
      .mockResolvedValueOnce({
        contexts: [{ id: "ctx_ok", kind: "assistant", content: "这次连上了", createdAt: "2026-07-11T00:00:00Z" }],
        usage: {},
        model: model.id,
        providerName: "OpenAI Responses",
        durationMs: 6
      });

    const user = userEvent.setup();
    const { container } = render(<App />);
    const composer = await screen.findByLabelText("向 Agent 发送消息");
    await user.type(composer, "第一轮就连不上");
    await user.click(screen.getByRole("button", { name: "发送" }));

    // The failure is a message in the timeline, inside the turn that failed —
    // not a bare "stopped after Ns" header whose only explanation lives in a
    // composer banner that a reload throws away.
    const notice = await screen.findByText("无法连接 API：端口错误");
    expect(notice.closest("[data-turn-id]")).not.toBeNull();
    expect(container.querySelector(".composer-run-error")).toBeNull();
    expect(container.querySelectorAll("[data-turn-id]")).toHaveLength(1);
    // Presentation only: the notice must never reach the persisted conversation.
    expect(runtimeMocks.saveDocument.mock.calls.every(([saved]) => (
      !JSON.stringify(saved).includes("无法连接 API：端口错误")
    ))).toBe(true);

    // Sending again retracts the notice, and the turn that existed only to carry
    // it goes with it rather than surviving as an empty header.
    await user.type(composer, "再试一次");
    await user.click(screen.getByRole("button", { name: "发送" }));
    expect(await screen.findByText("这次连上了")).toBeInTheDocument();
    expect(screen.queryByText("无法连接 API：端口错误")).not.toBeInTheDocument();
    expect(container.querySelectorAll("[data-turn-id]")).toHaveLength(1);
  });

  it("drops an empty failed round when the next run retracts its notice, instead of continuing it", async () => {
    const document = documentWithModel();
    runtimeMocks.loadDocument.mockResolvedValue(document);
    let emitSecond!: (event: ModelStreamEvent) => void;
    runtimeMocks.runModel
      .mockRejectedValueOnce(new Error("上游直接拒绝"))
      .mockImplementationOnce((_request, onEvent) => {
        emitSecond = onEvent;
        return new Promise<ModelRunResponse>(() => undefined);
      });

    const user = userEvent.setup();
    const { container } = render(<App />);
    await user.type(await screen.findByLabelText("向 Agent 发送消息"), "第一轮就失败");
    await user.click(screen.getByRole("button", { name: "发送" }));

    // The notice is the only thing this round ever produced, so the record
    // survives to carry it.
    expect(await screen.findByText("上游直接拒绝")).toBeInTheDocument();
    expect(container.querySelectorAll("[data-turn-id]")).toHaveLength(1);

    // Sending again retracts the notice, and with it the last thing the round
    // had to show — so the round goes too, and the run that follows opens one
    // of its own instead of inheriting a round that never produced anything.
    await user.click(screen.getByRole("button", { name: "发送" }));
    await waitFor(() => expect(runtimeMocks.runModel).toHaveBeenCalledTimes(2));
    act(() => emitSecond({ type: "text_delta", round: 1, delta: "这次答上了" }));
    expect(await screen.findByText("这次答上了")).toBeInTheDocument();
    expect(screen.queryByText("上游直接拒绝")).not.toBeInTheDocument();
    expect(container.querySelectorAll("[data-turn-id]")).toHaveLength(1);

    const stored = Object.values(
      JSON.parse(window.localStorage.getItem(CONVERSATION_TURNS_STORAGE_KEY) ?? "{}") as Record<
        string,
        Array<{ segmentCount: number; usageOffset: unknown; error?: unknown }>
      >
    ).flat();
    // `segmentCount` is the discriminator: a continued round would be its
    // second segment and would carry the failed attempt's counters forward.
    expect(stored).toHaveLength(1);
    expect(stored[0].segmentCount).toBe(1);
    expect(stored[0].usageOffset).toEqual({});
    expect(stored[0].error).toBeUndefined();
  });

  it("keeps a dismissed failure from leaving its empty turn header behind", async () => {
    const document = documentWithModel();
    runtimeMocks.loadDocument.mockResolvedValue(document);
    runtimeMocks.runModel.mockRejectedValue(new Error("上游直接拒绝"));

    const user = userEvent.setup();
    const { container } = render(<App />);
    await user.type(await screen.findByLabelText("向 Agent 发送消息"), "触发失败");
    await user.click(screen.getByRole("button", { name: "发送" }));
    expect(await screen.findByText("上游直接拒绝")).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "关闭模型错误" }));

    expect(screen.queryByText("上游直接拒绝")).not.toBeInTheDocument();
    expect(container.querySelectorAll("[data-turn-id]")).toHaveLength(0);
    // The user's own message is theirs, not the run's, and stays put.
    expect(screen.getAllByText("触发失败").some(
      (element) => element.closest(".context-card--user")
    )).toBe(true);
  });

  it("shows a structured request failure as a transient notice and never persists it as context", async () => {
    const document = documentWithModel();
    runtimeMocks.loadDocument.mockResolvedValue(document);
    runtimeMocks.runModel
      .mockResolvedValueOnce({
        contexts: [{
          id: "ctx_partial",
          kind: "assistant",
          content: "重试耗尽前保留的部分回复",
          round: 1,
          interrupted: true,
          createdAt: "2026-07-11T00:00:00Z"
        }],
        usage: {},
        model: model.id,
        providerName: "OpenAI Responses",
        durationMs: 20,
        stopReason: "error",
        error: { message: "模型 API 请求失败：HTTP 503 服务过载", round: 1, attempts: 6 }
      })
      .mockResolvedValueOnce({
        contexts: [{ id: "ctx_next", kind: "assistant", content: "续接成功", createdAt: "2026-07-11T00:01:00Z" }],
        usage: {},
        model: model.id,
        providerName: "OpenAI Responses",
        durationMs: 8
      });

    const user = userEvent.setup();
    render(<App />);
    const composer = await screen.findByLabelText("向 Agent 发送消息");
    await user.type(composer, "触发结构化错误");
    await user.click(screen.getByRole("button", { name: "发送" }));

    // The banner carries the real failure text and the retry count; the
    // partially streamed reply stays in the timeline as an interrupted card.
    expect(await screen.findByText("模型 API 请求失败：HTTP 503 服务过载（已自动重试 5 次）")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "重试" })).toBeInTheDocument();
    expect(screen.getByText("重试耗尽前保留的部分回复")).toBeInTheDocument();
    expect(screen.getByText("重试耗尽前保留的部分回复").closest("[data-turn-id]")).not.toBeNull();
    expect(screen.getByRole("button", { name: /test-model 在 .* 后中止/ }))
      .toHaveAttribute("aria-expanded", "true");
    await waitFor(() => expect(runtimeMocks.saveDocument.mock.calls.some(([saved]) => (
      JSON.stringify(saved).includes("重试耗尽前保留的部分回复")
    ))).toBe(true));
    // The error text is a transient hint only — it must never enter the
    // persisted timeline document.
    expect(runtimeMocks.saveDocument.mock.calls.every(([saved]) => (
      !JSON.stringify(saved).includes("HTTP 503 服务过载")
    ))).toBe(true);

    // Sending the next message auto-dismisses the notice.
    await user.type(composer, "继续对话");
    await user.click(screen.getByRole("button", { name: "发送" }));
    expect(await screen.findByText("续接成功")).toBeInTheDocument();
    expect(screen.queryByText("模型 API 请求失败：HTTP 503 服务过载（已自动重试 5 次）")).not.toBeInTheDocument();
  });

  it("keeps the failed partial during a retry wait and replaces it when the retry streams", async () => {
    const document = documentWithModel();
    runtimeMocks.loadDocument.mockResolvedValue(document);
    let emit!: (event: ModelStreamEvent) => void;
    let resolveRun!: (value: unknown) => void;
    runtimeMocks.runModel.mockImplementation((_request, onEvent) => {
      emit = onEvent;
      return new Promise((resolve) => { resolveRun = resolve; });
    });

    const user = userEvent.setup();
    render(<App />);
    await user.type(await screen.findByLabelText("向 Agent 发送消息"), "测试重试提示");
    await user.click(screen.getByRole("button", { name: "发送" }));

    act(() => emit({ type: "text_delta", round: 1, delta: "第一次尝试的部分内容" }));
    expect(await screen.findByText("第一次尝试的部分内容")).toBeInTheDocument();

    act(() => emit({
      type: "stream_retry_scheduled",
      round: 1,
      attempt: 1,
      maxAttempts: 5,
      delayMs: 400,
      message: "连接被重置"
    }));
    // While waiting for the retry, the partial stays visible next to the notice.
    expect(await screen.findByText(/正在第 1\/5 次重试/)).toBeInTheDocument();
    expect(screen.getByText("第一次尝试的部分内容")).toBeInTheDocument();

    // The retried attempt's first delta discards the stale partial and the notice.
    act(() => emit({ type: "text_delta", round: 1, delta: "重试后的完整回复" }));
    expect(await screen.findByText("重试后的完整回复")).toBeInTheDocument();
    expect(screen.queryByText("第一次尝试的部分内容")).not.toBeInTheDocument();
    expect(screen.queryByText(/正在第 1\/5 次重试/)).not.toBeInTheDocument();

    await act(async () => resolveRun({
      contexts: [{ id: "ctx_final", kind: "assistant", content: "重试后的完整回复", round: 1, createdAt: "2026-07-11T00:00:00Z" }],
      usage: {},
      model: model.id,
      providerName: "OpenAI Responses",
      durationMs: 30
    }));
    expect(await screen.findByText("重试后的完整回复")).toBeInTheDocument();
  });

  it("keeps a browser path fallback and requires an absolute workspace path", async () => {
    runtimeMocks.loadDocument.mockResolvedValue(documentWithModel());
    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");
    await user.click(screen.getByRole("button", { name: "添加工作区" }));
    const dialog = screen.getByRole("dialog", { name: "添加工作区" });
    expect(within(dialog).getByText("浏览器预览无法读取系统目录，请粘贴绝对路径。")).toBeInTheDocument();
    const path = within(dialog).getByRole("textbox", { name: /工作区路径/ });
    await user.type(path, "relative/path");
    await user.click(within(dialog).getByRole("button", { name: "添加工作区" }));
    // Relative paths are rejected without closing the dialog or creating a
    // workspace.
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    expect(screen.getByRole("dialog", { name: "添加工作区" })).toBeInTheDocument();
    expect(path).toHaveValue("relative/path");
    expect(screen.queryByRole("button", { name: "relative/path" })).not.toBeInTheDocument();

    await user.clear(path);
    await user.type(path, "C:\\Temp\\qa-workspace");
    await user.type(within(dialog).getByLabelText("显示名称"), "QA Workspace");
    await user.click(within(dialog).getByRole("button", { name: "添加工作区" }));
    expect(await screen.findByRole("button", { name: "QA Workspace" })).toBeInTheDocument();
  });

  it("offers restored shell tools and asks the backend classifier in a temporary workspace", async () => {
    const document = documentWithModel();
    runtimeMocks.loadDocument.mockResolvedValue(document);
    const user = userEvent.setup();
    const { container } = render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");
    await user.click(screen.getByRole("button", { name: `工作区：${document.workspaces[0].name}` }));
    await user.click(within(screen.getByRole("menu", { name: "选择工作区" })).getByRole("menuitemradio", { name: "临时工作区" }));
    expect(screen.getByRole("button", { name: "工作区：临时工作区" })).toBeInTheDocument();

    fireEvent.contextMenu(container.querySelector(".usage-stats")!, { clientX: 40, clientY: 180 });
    await user.click(screen.getByRole("menuitem", { name: /工具调用/ }));
    await user.click(screen.getByRole("button", { name: /PowerShell/ }));
    await user.type(screen.getByLabelText("命令 *"), "Get-ChildItem");
    await user.click(screen.getByRole("button", { name: "执行并添加" }));

    const request = expect.objectContaining({ toolName: "powershell", input: { command: "Get-ChildItem" } });
    await waitFor(() => expect(runtimeMocks.requestToolApproval).toHaveBeenCalledWith(request));
    expect(runtimeMocks.executeTool).toHaveBeenCalledWith(request, "approval-once");
  });

  it("draws the approval card above the composer for a call the model raised and resumes the run", async () => {
    const document = documentWithModel();
    runtimeMocks.loadDocument.mockResolvedValue(document);
    let emit!: (event: ModelStreamEvent) => void;
    let resolveRun!: (value: ModelRunResponse) => void;
    runtimeMocks.runModel.mockImplementation((_request, onEvent) => {
      emit = onEvent;
      return new Promise((resolve) => { resolveRun = resolve; });
    });

    const user = userEvent.setup();
    render(<App />);
    await user.type(await screen.findByLabelText("向 Agent 发送消息"), "改一下文件");
    await user.click(screen.getByRole("button", { name: "发送" }));
    await waitFor(() => expect(runtimeMocks.runModel).toHaveBeenCalledTimes(1));

    act(() => emit({
      type: "tool_approval_requested",
      promptId: "prompt-model-1",
      toolName: "write_file",
      label: "写入文件",
      summary: "src/main.rs",
      riskLevel: "中",
      reason: "写入工作区文件会覆盖现有内容",
      allowAlwaysOffered: true
    }));

    const card = await screen.findByRole("dialog", { name: "需要你的确认" });
    expect(card).toHaveTextContent("src/main.rs");
    expect(within(card).getByRole("button", { name: "总是允许" })).toBeInTheDocument();

    await user.click(within(card).getByRole("button", { name: "允许" }));
    expect(runtimeMocks.resolveToolPrompt).toHaveBeenCalledWith("prompt-model-1", "allow_once", undefined);
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "需要你的确认" })).not.toBeInTheDocument());

    await act(async () => resolveRun({
      contexts: [{ id: "ctx_after_approval", kind: "assistant", content: "文件已写入", createdAt: "2026-07-11T00:00:00Z" }],
      usage: {},
      model: model.id,
      providerName: "OpenAI Responses",
      durationMs: 9
    }));
    expect(await screen.findByText("文件已写入")).toBeInTheDocument();
  });

  it("draws a background task's approval card with no run in sight, and answers it the same way", async () => {
    // Background workflow steps can require approval after their dispatching
    // turn has ended. Push delivery must render the same card and resolve it
    // through the same path.
    const document = documentWithModel();
    const conversation = document.workspaces[0].conversations[0];
    runtimeMocks.loadDocument.mockResolvedValue(document);

    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");
    expect(runtimeMocks.runModel).not.toHaveBeenCalled();

    await act(async () => {
      emitAppPushEvent({
        type: "toolApprovalRequested",
        conversationId: conversation.id,
        promptId: "prompt-task-1",
        toolName: "write",
        label: "写入文件",
        summary: "bg-step-output.txt",
        riskLevel: "中",
        reason: "请求批准模式要求确认所有写入操作",
        requester: "ws1",
        allowAlwaysOffered: true
      });
    });

    const card = await screen.findByRole("dialog", { name: "需要你的确认" });
    expect(card).toHaveTextContent("bg-step-output.txt");

    const user = userEvent.setup();
    await user.click(within(card).getByRole("button", { name: "允许" }));
    expect(runtimeMocks.resolveToolPrompt).toHaveBeenCalledWith("prompt-task-1", "allow_once", undefined);
    await waitFor(() =>
      expect(screen.queryByRole("dialog", { name: "需要你的确认" })).not.toBeInTheDocument());
  });

  it("takes a background card down when the host resolves it on the push channel", async () => {
    const document = documentWithModel();
    const conversation = document.workspaces[0].conversations[0];
    runtimeMocks.loadDocument.mockResolvedValue(document);

    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");

    await act(async () => {
      emitAppPushEvent({
        type: "toolApprovalRequested",
        conversationId: conversation.id,
        promptId: "prompt-task-2",
        toolName: "write",
        label: "写入文件",
        summary: "notes.md",
        riskLevel: "中",
        reason: "请求批准模式要求确认所有写入操作",
        requester: "ws1",
        allowAlwaysOffered: true
      });
    });
    await screen.findByRole("dialog", { name: "需要你的确认" });

    // A stopped task or expired card is retracted by the host. The renderer must
    // not leave a button with no receiver.
    await act(async () => {
      emitAppPushEvent({
        type: "toolApprovalResolved",
        conversationId: conversation.id,
        promptId: "prompt-task-2",
        approved: false
      });
    });
    await waitFor(() =>
      expect(screen.queryByRole("dialog", { name: "需要你的确认" })).not.toBeInTheDocument());
    expect(runtimeMocks.resolveToolPrompt).not.toHaveBeenCalled();
  });

  it("re-raises a background card the renderer missed, after a reload", async () => {
    // Push delivery is edge-triggered and may predate every open run. Rescan
    // after reload so the waiting worker remains answerable.
    const document = documentWithModel();
    const conversation = document.workspaces[0].conversations[0];
    runtimeMocks.loadDocument.mockResolvedValue(document);
    runtimeMocks.listPendingToolPrompts.mockResolvedValue([{
      conversationId: conversation.id,
      promptId: "prompt-task-3",
      toolName: "write",
      label: "写入文件",
      summary: "recovered.txt",
      riskLevel: "中",
      reason: "请求批准模式要求确认所有写入操作",
      requester: "ws1",
      allowAlwaysOffered: true
    }]);

    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");

    const card = await screen.findByRole("dialog", { name: "需要你的确认" });
    expect(card).toHaveTextContent("recovered.txt");
  });

  it("jumps to the requesting subagent's page when its card arrives, and answers there", async () => {
    // A card with `sourceAgent` must appear on the requesting subagent page so
    // it remains visible and answerable while the conversation panel is hidden.
    const document = documentWithModel();
    const conversation = document.workspaces[0].conversations[0];
    conversation.contexts = [{
      id: "call-spawn-researcher",
      kind: "tool",
      toolName: "agent_spawn",
      input: { name: "researcher", prompt: "调研路由" },
      result: {
        success: true,
        output: "已派生",
        executedAt: "2026-07-11T00:00:00Z",
        durationMs: 3
      },
      subagent: {
        name: "researcher",
        task: "调研路由",
        // An in-flight record deliberately carries the live status; the settled
        // record type does not include it, hence the assertion.
        status: "running" as unknown as SubagentRunRecord["status"],
        contexts: [],
        updates: []
      },
      createdAt: "2026-07-11T00:00:00Z"
    }];
    runtimeMocks.loadDocument.mockResolvedValue(document);

    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");

    await act(async () => {
      emitAppPushEvent({
        type: "toolApprovalRequested",
        conversationId: conversation.id,
        promptId: "prompt-from-child",
        toolName: "write",
        label: "写入文件",
        summary: "notes.md",
        riskLevel: "中",
        reason: "请求批准模式要求确认所有写入操作",
        requester: "researcher",
        sourceAgent: "researcher",
        sourceCallId: "call-spawn-researcher",
        allowAlwaysOffered: true
      });
    });

    // Render one card on the subagent page; the empty conversation panel must
    // not create a duplicate dialog.
    const card = await screen.findByRole("dialog", { name: "需要你的确认" });
    expect(screen.getAllByRole("dialog", { name: "需要你的确认" })).toHaveLength(1);
    expect(card.closest(".main-pane__subagent")).not.toBeNull();
    await user.click(within(card).getByRole("button", { name: "允许" }));
    expect(runtimeMocks.resolveToolPrompt).toHaveBeenCalledWith("prompt-from-child", "allow_once", undefined);
  });

  it("stacks concurrent cards behind a pager instead of hiding them", async () => {
    // Multiple sources wait concurrently. Render one card at a time with its
    // stack position; paging does not answer a card.
    const document = documentWithModel();
    const conversation = document.workspaces[0].conversations[0];
    runtimeMocks.loadDocument.mockResolvedValue(document);
    const baseCard = {
      toolName: "write",
      label: "写入文件",
      riskLevel: "中",
      reason: "请求批准模式要求确认所有写入操作",
      allowAlwaysOffered: true
    };

    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");
    await act(async () => {
      emitAppPushEvent({
        type: "toolApprovalRequested",
        conversationId: conversation.id,
        promptId: "prompt-stack-1",
        summary: "first.txt",
        ...baseCard
      });
      emitAppPushEvent({
        type: "toolApprovalRequested",
        conversationId: conversation.id,
        promptId: "prompt-stack-2",
        summary: "second.txt",
        ...baseCard
      });
    });

    const card = await screen.findByRole("dialog", { name: "需要你的确认" });
    expect(card).toHaveTextContent("first.txt");
    expect(card).toHaveTextContent("1/2");

    await user.click(screen.getByRole("button", { name: "下一条待确认" }));
    const flipped = await screen.findByRole("dialog", { name: "需要你的确认" });
    expect(flipped).toHaveTextContent("second.txt");
    expect(flipped).toHaveTextContent("2/2");
    expect(runtimeMocks.resolveToolPrompt).not.toHaveBeenCalled();

    await user.click(within(flipped).getByRole("button", { name: "允许" }));
    expect(runtimeMocks.resolveToolPrompt).toHaveBeenCalledWith("prompt-stack-2", "allow_once", undefined);
    await waitFor(() => {
      const remaining = screen.getByRole("dialog", { name: "需要你的确认" });
      expect(remaining).toHaveTextContent("first.txt");
    });
    expect(screen.queryByText("1/1")).not.toBeInTheDocument();
  });

  it("keeps a background card that the host still holds when a foreground run ends", async () => {
    // A foreground run ending must preserve cards the host still holds for
    // background workflow steps.
    const document = documentWithModel();
    const conversation = document.workspaces[0].conversations[0];
    runtimeMocks.loadDocument.mockResolvedValue(document);
    const backgroundCard = {
      conversationId: conversation.id,
      promptId: "prompt-task-alive",
      toolName: "write",
      label: "写入文件",
      summary: "bg-step-output.txt",
      riskLevel: "中",
      reason: "请求批准模式要求确认所有写入操作",
      requester: "ws1",
      allowAlwaysOffered: true
    };
    runtimeMocks.listPendingToolPrompts.mockResolvedValue([backgroundCard]);
    runtimeMocks.runModel.mockResolvedValue({
      contexts: [{ id: "ctx_reply", kind: "assistant", content: "前台回合结束", createdAt: "2026-07-11T00:00:00Z" }],
      usage: {},
      model: model.id,
      providerName: "Test Provider",
      durationMs: 5
    });

    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");
    await act(async () => {
      emitAppPushEvent({ type: "toolApprovalRequested", ...backgroundCard });
    });
    await screen.findByRole("dialog", { name: "需要你的确认" });

    await user.type(screen.getByLabelText("向 Agent 发送消息"), "顺手问一句");
    await user.click(screen.getByRole("button", { name: "发送" }));
    expect(await screen.findByText("前台回合结束")).toBeInTheDocument();

    const card = await screen.findByRole("dialog", { name: "需要你的确认" });
    expect(card).toHaveTextContent("bg-step-output.txt");
    await user.click(within(card).getByRole("button", { name: "允许" }));
    expect(runtimeMocks.resolveToolPrompt).toHaveBeenCalledWith("prompt-task-alive", "allow_once", undefined);
  });

  it("still drops a card the host no longer holds when a run ends", async () => {
    // Cleanup must still remove cards the host no longer recognizes; otherwise
    // the UI leaves an unanswerable button. This prevents a no-op cleanup from
    // satisfying the preceding preservation test.
    const document = documentWithModel();
    const conversation = document.workspaces[0].conversations[0];
    runtimeMocks.loadDocument.mockResolvedValue(document);
    runtimeMocks.listPendingToolPrompts.mockResolvedValue([]);
    runtimeMocks.runModel.mockResolvedValue({
      contexts: [{ id: "ctx_reply", kind: "assistant", content: "回合结束", createdAt: "2026-07-11T00:00:00Z" }],
      usage: {},
      model: model.id,
      providerName: "Test Provider",
      durationMs: 5
    });

    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");
    await act(async () => {
      emitAppPushEvent({
        type: "toolApprovalRequested",
        conversationId: conversation.id,
        promptId: "prompt-orphan",
        toolName: "write",
        label: "写入文件",
        summary: "orphan.txt",
        riskLevel: "中",
        reason: "请求批准模式要求确认所有写入操作",
        allowAlwaysOffered: true
      });
    });
    await screen.findByRole("dialog", { name: "需要你的确认" });

    await user.type(screen.getByLabelText("向 Agent 发送消息"), "顺手问一句");
    await user.click(screen.getByRole("button", { name: "发送" }));
    expect(await screen.findByText("回合结束")).toBeInTheDocument();

    await waitFor(() =>
      expect(screen.queryByRole("dialog", { name: "需要你的确认" })).not.toBeInTheDocument());
  });

  it("withholds the blanket allowance for a shell call and clears the card when the backend retracts it", async () => {
    const document = documentWithModel();
    runtimeMocks.loadDocument.mockResolvedValue(document);
    let emit!: (event: ModelStreamEvent) => void;
    let resolveRun!: (value: ModelRunResponse) => void;
    runtimeMocks.runModel.mockImplementation((_request, onEvent) => {
      emit = onEvent;
      return new Promise((resolve) => { resolveRun = resolve; });
    });

    const user = userEvent.setup();
    render(<App />);
    await user.type(await screen.findByLabelText("向 Agent 发送消息"), "跑一下构建");
    await user.click(screen.getByRole("button", { name: "发送" }));
    await waitFor(() => expect(runtimeMocks.runModel).toHaveBeenCalledTimes(1));

    act(() => emit({
      type: "tool_approval_requested",
      promptId: "prompt-shell-1",
      toolName: "powershell",
      label: "PowerShell",
      summary: "npm run build",
      riskLevel: "高",
      reason: "命令会在工作区内执行",
      allowAlwaysOffered: false
    }));

    const card = await screen.findByRole("dialog", { name: "需要你的确认" });
    expect(within(card).queryByRole("button", { name: "总是允许" })).not.toBeInTheDocument();

    // The run was cancelled host-side, so the card is retracted rather than answered.
    act(() => emit({ type: "tool_approval_resolved", promptId: "prompt-shell-1", approved: false }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "需要你的确认" })).not.toBeInTheDocument());
    expect(runtimeMocks.resolveToolPrompt).not.toHaveBeenCalled();

    await act(async () => resolveRun({
      contexts: [{ id: "ctx_after_denial", kind: "assistant", content: "命令未执行", createdAt: "2026-07-11T00:00:00Z" }],
      usage: {},
      model: model.id,
      providerName: "OpenAI Responses",
      durationMs: 9
    }));
    expect(await screen.findByText("命令未执行")).toBeInTheDocument();
  });

  it("waits on the card before running a tool call the user assembled by hand", async () => {
    const document = documentWithModel();
    runtimeMocks.loadDocument.mockResolvedValue(document);
    runtimeMocks.requestToolApproval.mockResolvedValue({
      expiresInMs: 90_000,
      prompt: {
        promptId: "prompt-manual-1",
        toolName: "powershell",
        label: "PowerShell",
        summary: "Get-ChildItem",
        riskLevel: "高",
        reason: "命令会在工作区内执行",
        allowAlwaysOffered: false
      }
    });
    runtimeMocks.resolveToolPrompt.mockResolvedValue({ nonce: "manual-nonce", expiresInMs: 90_000 });

    const user = userEvent.setup();
    const { container } = render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");
    await user.click(screen.getByRole("button", { name: `工作区：${document.workspaces[0].name}` }));
    await user.click(within(screen.getByRole("menu", { name: "选择工作区" })).getByRole("menuitemradio", { name: "临时工作区" }));

    fireEvent.contextMenu(container.querySelector(".usage-stats")!, { clientX: 40, clientY: 180 });
    await user.click(screen.getByRole("menuitem", { name: /工具调用/ }));
    await user.click(screen.getByRole("button", { name: /PowerShell/ }));
    await user.type(screen.getByLabelText("命令 *"), "Get-ChildItem");
    await user.click(screen.getByRole("button", { name: "执行并添加" }));

    const card = await screen.findByRole("dialog", { name: "需要你的确认" });
    expect(card).toHaveTextContent("Get-ChildItem");
    // Nothing runs until the card is answered — the nonce comes from the answer.
    expect(runtimeMocks.executeTool).not.toHaveBeenCalled();

    await user.click(within(card).getByRole("button", { name: "允许" }));
    expect(runtimeMocks.resolveToolPrompt).toHaveBeenCalledWith("prompt-manual-1", "allow_once", undefined);
    await waitFor(() => expect(runtimeMocks.executeTool).toHaveBeenCalledWith(
      expect.objectContaining({ toolName: "powershell", input: { command: "Get-ChildItem" } }),
      "manual-nonce"
    ));
  });
});
