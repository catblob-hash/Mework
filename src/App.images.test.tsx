import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
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
import { resetAppMocks, budgetImages, deferred, documentWithModel, model, runtimeMocks } from "./test/appMocks";

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

describe("App model run flow — images", () => {
  beforeEach(resetAppMocks);

  it("persists a new text-and-image user turn before starting the model request", async () => {
    const document = documentWithModel();
    document.globalSettings.apiProviders[0].models[0] = {
      ...document.globalSettings.apiProviders[0].models[0],
      capabilities: ["image_recognition"]
    };
    runtimeMocks.loadDocument.mockResolvedValue(document);
    runtimeMocks.prepareImageAttachment.mockResolvedValue({
      id: "persist-before-run-image",
      name: "persist-before-run.png",
      mime: "image/png",
      width: 2,
      height: 2,
      bytes: 4
    });
    runtimeMocks.runModel.mockResolvedValue({
      contexts: [],
      usage: {},
      model: model.id,
      providerName: "OpenAI Responses",
      durationMs: 1
    });

    const user = userEvent.setup();
    render(<App />);
    const composer = await screen.findByLabelText("向 Agent 发送消息");
    const composerRegion = composer.closest(".composer") as HTMLElement;
    await waitFor(() => expect(runtimeMocks.saveDocument).toHaveBeenCalled());
    const image = new File([new Uint8Array([1, 2, 3, 4])], "persist-before-run.png", {
      type: "image/png"
    });
    Object.defineProperty(image, "arrayBuffer", {
      configurable: true,
      value: async () => new Uint8Array([1, 2, 3, 4]).buffer
    });
    fireEvent.drop(composerRegion, {
      dataTransfer: { files: [image], types: ["Files"] }
    });
    expect(await within(composerRegion).findByRole("img", { name: "persist-before-run.png" }))
      .toBeInTheDocument();
    await user.type(composer, "先保存这条图文消息");

    const save = deferred<void>();
    runtimeMocks.saveDocument.mockReset().mockReturnValue(save.promise);
    await user.click(screen.getByRole("button", { name: "发送" }));
    await waitFor(() => expect(runtimeMocks.saveDocument).toHaveBeenCalledTimes(1));
    const saved = runtimeMocks.saveDocument.mock.calls[0][0] as AppDocument;
    expect(runtimeMocks.saveDocument.mock.calls[0][1]).toMatchObject({
      immutableSnapshot: true,
      durable: true
    });
    expect(saved.workspaces[0].conversations[0].contexts.at(-1)).toMatchObject({
      kind: "user",
      content: "[Image #1]先保存这条图文消息",
      images: [expect.objectContaining({ id: "persist-before-run-image", shortId: 1 })]
    });
    expect(runtimeMocks.runModel).not.toHaveBeenCalled();
    expect(composer).toHaveValue("[Image #1]先保存这条图文消息");
    expect(within(composerRegion).getByRole("img", { name: "persist-before-run.png" }))
      .toBeInTheDocument();

    await act(async () => save.resolve());
    await waitFor(() => expect(runtimeMocks.runModel).toHaveBeenCalledTimes(1));
    expect(runtimeMocks.saveDocument.mock.invocationCallOrder[0])
      .toBeLessThan(runtimeMocks.runModel.mock.invocationCallOrder[0]);
    expect(composer).toHaveValue("");
    expect(within(composerRegion).queryByRole("img", { name: "persist-before-run.png" }))
      .not.toBeInTheDocument();
  });

  it("durably rolls back a failed text-and-image user turn without losing its draft", async () => {
    const document = documentWithModel();
    document.globalSettings.apiProviders[0].models[0] = {
      ...document.globalSettings.apiProviders[0].models[0],
      capabilities: ["image_recognition"]
    };
    const originalConversation = document.workspaces[0].conversations[0];
    const originalTitle = originalConversation.title;
    const originalUpdatedAt = originalConversation.updatedAt;
    const originalContextIds = originalConversation.contexts.map((context) => context.id);
    runtimeMocks.loadDocument.mockResolvedValue(document);
    runtimeMocks.prepareImageAttachment.mockResolvedValue({
      id: "failed-save-image",
      name: "failed-save.png",
      mime: "image/png",
      width: 2,
      height: 2,
      bytes: 4
    });

    const user = userEvent.setup();
    render(<App />);
    const composer = await screen.findByLabelText("向 Agent 发送消息");
    const composerRegion = composer.closest(".composer") as HTMLElement;
    await waitFor(() => expect(runtimeMocks.saveDocument).toHaveBeenCalled());
    const image = new File([new Uint8Array([1])], "failed-save.png", { type: "image/png" });
    Object.defineProperty(image, "arrayBuffer", {
      configurable: true,
      value: async () => new Uint8Array([1]).buffer
    });
    fireEvent.drop(composerRegion, {
      dataTransfer: { files: [image], types: ["Files"] }
    });
    expect(await within(composerRegion).findByRole("img", { name: "failed-save.png" }))
      .toBeInTheDocument();
    await user.type(composer, "保存失败也不能丢");

    const failedUserTurnSave = deferred<void>();
    runtimeMocks.saveDocument
      .mockReset()
      .mockImplementationOnce(() => failedUserTurnSave.promise)
      .mockResolvedValue(undefined);
    await user.click(screen.getByRole("button", { name: "发送" }));
    await waitFor(() => expect(runtimeMocks.saveDocument).toHaveBeenCalledTimes(1));
    const failedSnapshot = runtimeMocks.saveDocument.mock.calls[0][0] as AppDocument;
    const failedConversation = failedSnapshot.workspaces[0].conversations[0];
    const pendingContext = failedConversation.contexts.at(-1) as UserContext;
    expect(runtimeMocks.saveDocument.mock.calls[0][1]).toMatchObject({
      immutableSnapshot: true,
      durable: true
    });
    expect(failedConversation.title).toBe("保存失败也不能丢");
    expect(pendingContext).toMatchObject({
      kind: "user",
      content: "[Image #1]保存失败也不能丢",
      images: [expect.objectContaining({ id: "failed-save-image", shortId: 1 })]
    });
    expect(runtimeMocks.runModel).not.toHaveBeenCalled();
    await act(async () => {
      failedUserTurnSave.reject(new Error("disk unavailable"));
      await Promise.resolve();
    });

    await waitFor(() => expect(runtimeMocks.saveDocument).toHaveBeenCalledTimes(2));
    const rolledBackSnapshot = runtimeMocks.saveDocument.mock.calls[1][0] as AppDocument;
    const rolledBackConversation = rolledBackSnapshot.workspaces[0].conversations[0];
    expect(runtimeMocks.saveDocument.mock.calls[1][1]).toMatchObject({
      immutableSnapshot: true,
      durable: true
    });
    expect(rolledBackConversation.contexts.map((context) => context.id)).toEqual(originalContextIds);
    expect(rolledBackConversation.contexts.some((context) => context.id === pendingContext.id))
      .toBe(false);
    expect(rolledBackConversation.title).toBe(originalTitle);
    expect(rolledBackConversation.updatedAt).toBe(originalUpdatedAt);
    expect(runtimeMocks.runModel).not.toHaveBeenCalled();
    expect(composer).toHaveValue("[Image #1]保存失败也不能丢");
    expect(within(composerRegion).getByRole("img", { name: "failed-save.png" }))
      .toBeInTheDocument();
    expect(screen.getByRole("complementary", { name: "工作区和对话" }))
      .toHaveTextContent("新任务");
  });

  it("sends a pure-image message selected or dropped into the composer", async () => {
    const document = documentWithModel();
    document.globalSettings.apiProviders[0].models[0] = {
      ...document.globalSettings.apiProviders[0].models[0],
      capabilities: ["image_recognition"]
    };
    runtimeMocks.loadDocument.mockResolvedValue(document);
    runtimeMocks.prepareImageAttachment.mockImplementation(async (name: string) => ({
      id: `image-${name}`,
      name,
      mime: "image/png",
      width: 10,
      height: 10,
      bytes: 4
    }));
    runtimeMocks.runModel.mockResolvedValue({
      contexts: [],
      usage: {},
      model: model.id,
      providerName: "OpenAI Responses",
      durationMs: 2
    });

    const user = userEvent.setup();
    const { container } = render(<App />);
    const composer = await screen.findByLabelText("向 Agent 发送消息");
    // The add button opens a menu; image availability belongs to its menu item.
    await user.click(screen.getByRole("button", { name: "添加内容" }));
    expect(screen.getByRole("menuitem", { name: /添加图片/ })).toBeEnabled();
    await user.keyboard("{Escape}");
    const fileInput = container.querySelector<HTMLInputElement>('input[type="file"]')!;
    const selected = new File([new Uint8Array([1, 2, 3, 4])], "selected.png", { type: "image/png" });
    const dropped = new File([new Uint8Array([5, 6, 7, 8])], "dropped.png", { type: "image/png" });
    for (const file of [selected, dropped]) {
      Object.defineProperty(file, "arrayBuffer", {
        configurable: true,
        value: async () => new Uint8Array([1, 2, 3, 4]).buffer
      });
    }
    await user.upload(fileInput, selected);
    fireEvent.drop(composer.closest(".composer")!, {
      dataTransfer: { files: [dropped], types: ["Files"] }
    });
    expect(await screen.findByRole("img", { name: "selected.png" })).toBeInTheDocument();
    expect(await screen.findByRole("img", { name: "dropped.png" })).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "发送" }));
    await waitFor(() => expect(runtimeMocks.runModel).toHaveBeenCalledTimes(1));
    const request = runtimeMocks.runModel.mock.calls[0][0] as ModelRunRequest;
    expect(request.contexts.at(-1)).toMatchObject({
      kind: "user",
      content: "[Image #1] [Image #2]",
      images: [
        expect.objectContaining({ id: "image-selected.png", shortId: 1 }),
        expect.objectContaining({ id: "image-dropped.png", shortId: 2 })
      ]
    });
  });

  it("deletes and restores an image context without changing its attachment reference", async () => {
    const document = documentWithModel();
    const image = {
      id: "undo-image-reference",
      name: "undo-image.png",
      mime: "image/png",
      width: 12,
      height: 8,
      bytes: 96
    };
    document.workspaces[0].conversations[0].contexts = [{
      id: "undo-image-user",
      kind: "user",
      content: "",
      images: [image],
      createdAt: "2026-07-24T00:00:00Z"
    }];
    runtimeMocks.loadDocument.mockResolvedValue(document);

    const user = userEvent.setup();
    render(<App />);
    const thumbnail = await screen.findByRole("img", { name: image.name });
    const article = thumbnail.closest("article");
    expect(article).not.toBeNull();
    runtimeMocks.saveDocument.mockClear();

    await user.click(within(article!).getByRole("button", { name: "删除上下文" }));

    expect(screen.queryByRole("img", { name: image.name })).not.toBeInTheDocument();
    await waitFor(() => expect(runtimeMocks.saveDocument.mock.calls.some(([saved]) => (
      (saved as AppDocument).workspaces[0].conversations[0].contexts.length === 0
    ))).toBe(true));

    await user.click(screen.getByRole("button", { name: "撤销删除上下文" }));

    expect(await screen.findByRole("img", { name: image.name })).toBeInTheDocument();
    await waitFor(() => expect(runtimeMocks.saveDocument.mock.calls.some(([saved]) => {
      const restored = (saved as AppDocument).workspaces[0].conversations[0].contexts[0];
      return restored?.id === "undo-image-user"
        && restored.kind === "user"
        && JSON.stringify(restored.images) === JSON.stringify([image]);
    })).toBe(true));
  });

  it("gates every composer image entry when the selected model has no vision capability", async () => {
    runtimeMocks.loadDocument.mockResolvedValue(documentWithModel());
    const user = userEvent.setup();
    render(<App />);
    const composer = await screen.findByLabelText("向 Agent 发送消息");
    await user.click(screen.getByRole("button", { name: "添加内容" }));
    const addImages = screen.getByRole("menuitem", { name: /添加图片/ });
    expect(addImages).toBeDisabled();
    expect(addImages).toHaveTextContent("当前模型不支持图片输入");
    await user.keyboard("{Escape}");

    const file = new File([new Uint8Array([1])], "blocked.png", { type: "image/png" });
    fireEvent.paste(composer, {
      clipboardData: {
        files: [file],
        getData: () => ""
      }
    });
    // Rejection is silent. Allow event-loop turns before verifying no upload began.
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    expect(runtimeMocks.prepareImageAttachment).not.toHaveBeenCalled();
    expect(screen.queryByRole("img", { name: "blocked.png" })).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "添加内容" }));
    expect(screen.getByRole("menuitem", { name: /添加图片/ }))
      .toHaveTextContent("当前模型不支持图片输入");
  });

  /// Images outlive the model that accepted them: attach under a vision model,
  /// switch to a text-only one, and the request still carries them. Sending then
  /// refused deep in the pipeline with nothing shown at all — the composer simply
  /// did nothing, which reads as a broken button rather than a decision.
  it("explains an unsendable image history instead of leaving the composer dead", async () => {
    const document = documentWithModel();
    // The selected model keeps its default text-only capabilities.
    document.workspaces[0].conversations[0].contexts = [
      {
        id: "history-image",
        kind: "user",
        content: "带图的历史消息",
        images: budgetImages("history-image", 1),
        createdAt: "2026-07-24T00:00:00Z"
      }
    ];
    runtimeMocks.loadDocument.mockResolvedValue(document);

    const user = userEvent.setup();
    render(<App />);
    const composer = await screen.findByLabelText("向 Agent 发送消息");
    await user.type(composer, "继续");

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("当前模型没有启用图片输入");
    // The reason is stated, so the control that cannot work is visibly disabled
    // rather than silently doing nothing when pressed.
    expect(screen.getByRole("button", { name: "发送" })).toBeDisabled();
    expect(runtimeMocks.runModel).not.toHaveBeenCalled();
    expect(composer).toHaveValue("继续");
  });

  it("caps one composer message before excess image bytes are uploaded", async () => {
    const document = documentWithModel();
    document.globalSettings.apiProviders[0].models[0] = {
      ...document.globalSettings.apiProviders[0].models[0],
      capabilities: ["image_recognition"]
    };
    runtimeMocks.loadDocument.mockResolvedValue(document);
    runtimeMocks.prepareImageAttachment.mockImplementation(async (name: string) => ({
      id: `image-${name}`,
      name,
      mime: "image/png",
      width: 1,
      height: 1,
      bytes: 1
    }));

    const user = userEvent.setup();
    const { container } = render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");
    const fileInput = container.querySelector<HTMLInputElement>('input[type="file"]')!;
    const files = Array.from({ length: 21 }, (_, index) => {
      const file = new File([new Uint8Array([index])], `image-${index}.png`, { type: "image/png" });
      Object.defineProperty(file, "arrayBuffer", {
        configurable: true,
        value: async () => new Uint8Array([index]).buffer
      });
      return file;
    });

    await user.upload(fileInput, files);

    // Silently discard the 21st image before upload; the composer renders at most 20 thumbnails.
    await waitFor(() => expect(runtimeMocks.prepareImageAttachment).toHaveBeenCalledTimes(20));
    expect(await screen.findByRole("list", { name: "20 张图片" })).toBeInTheDocument();
    expect(runtimeMocks.prepareImageAttachment).toHaveBeenCalledTimes(20);
  });

  it("rejects an added image against the full visible history before writing the timeline", async () => {
    const document = documentWithModel();
    document.globalSettings.apiProviders[0].models[0] = {
      ...document.globalSettings.apiProviders[0].models[0],
      capabilities: ["image_recognition"]
    };
    document.workspaces[0].conversations[0].contexts = [{
      id: "twenty-history-images",
      kind: "user",
      content: "历史图片",
      images: Array.from({ length: 20 }, (_, index) => ({
        id: `history-image-${index}`,
        name: `history-${index}.png`,
        mime: "image/png",
        width: 1,
        height: 1,
        bytes: 1
      })),
      createdAt: "2026-07-24T00:00:00Z"
    }];
    runtimeMocks.loadDocument.mockResolvedValue(document);
    runtimeMocks.prepareImageAttachment.mockResolvedValue({
      id: "extra-image",
      name: "extra.png",
      mime: "image/png",
      width: 1,
      height: 1,
      bytes: 1
    });

    const user = userEvent.setup();
    render(<App />);
    const composer = await screen.findByLabelText("向 Agent 发送消息");
    await screen.findByText("历史图片");
    const extra = new File([new Uint8Array([1])], "extra.png", { type: "image/png" });
    Object.defineProperty(extra, "arrayBuffer", {
      configurable: true,
      value: async () => new Uint8Array([1]).buffer
    });
    fireEvent.drop(composer.closest(".composer")!, {
      dataTransfer: { files: [extra], types: ["Files"] }
    });
    expect(await screen.findByRole("img", { name: "extra.png" })).toBeInTheDocument();
    await waitFor(() => expect(screen.getByRole("button", { name: "发送" })).toBeEnabled());

    await user.click(screen.getByRole("button", { name: "发送" }));

    // A silent budget rejection sends no request, preserves the draft image, and leaves the timeline unchanged.
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    expect(runtimeMocks.runModel).not.toHaveBeenCalled();
    expect(screen.getByRole("img", { name: "extra.png" })).toBeInTheDocument();
    expect(runtimeMocks.saveDocument.mock.calls.every(([saved]) => (
      (saved as AppDocument).workspaces[0].conversations[0].contexts.length === 1
    ))).toBe(true);
  });

  it("rejects a pure-text send when several valid history items exceed the request image budget", async () => {
    const document = documentWithModel();
    document.globalSettings.apiProviders[0].models[0] = {
      ...document.globalSettings.apiProviders[0].models[0],
      capabilities: ["image_recognition"]
    };
    document.workspaces[0].conversations[0].contexts = [
      {
        id: "history-budget-a",
        kind: "user",
        content: "第一批图片",
        images: budgetImages("history-a", 11),
        createdAt: "2026-07-24T00:00:00Z"
      },
      {
        id: "history-budget-b",
        kind: "user",
        content: "第二批图片",
        images: budgetImages("history-b", 11),
        createdAt: "2026-07-24T00:00:01Z"
      }
    ];
    runtimeMocks.loadDocument.mockResolvedValue(document);

    const user = userEvent.setup();
    render(<App />);
    const composer = await screen.findByLabelText("向 Agent 发送消息");
    await user.type(composer, "只补充文字");
    await user.click(screen.getByRole("button", { name: "发送" }));

    // A silent budget rejection sends no request and preserves the draft.
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    expect(runtimeMocks.runModel).not.toHaveBeenCalled();
    expect(composer).toHaveValue("只补充文字");
  });

  it("keeps a pure-text queue head when valid history items exceed the request image budget", async () => {
    const document = documentWithModel();
    document.globalSettings.apiProviders[0].models[0] = {
      ...document.globalSettings.apiProviders[0].models[0],
      capabilities: ["image_recognition"]
    };
    document.workspaces[0].conversations[0].contexts = [
      {
        id: "queued-history-budget-a",
        kind: "user",
        content: "第一批图片",
        images: budgetImages("queued-history-a", 11),
        createdAt: "2026-07-24T00:00:00Z"
      },
      {
        id: "queued-history-budget-b",
        kind: "user",
        content: "第二批图片",
        images: budgetImages("queued-history-b", 11),
        createdAt: "2026-07-24T00:00:01Z"
      }
    ];
    document.workspaces[0].conversations[0].queuedMessages = [{
      id: "pure-text-over-budget-head",
      content: "排队的纯文本",
      createdAt: "2026-07-24T00:00:02Z"
    }];
    runtimeMocks.loadDocument.mockResolvedValue(document);

    render(<App />);

    // A budget-blocked automatic dispatch preserves its queue head and sends no request.
    const queue = await screen.findByRole("region", { name: "排队消息" });
    expect(within(queue).getByText("排队的纯文本")).toBeInTheDocument();
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    expect(runtimeMocks.runModel).not.toHaveBeenCalled();
    expect(within(queue).getByText("排队的纯文本")).toBeInTheDocument();
  });

  it("branches from an image-heavy message without assembling a request", async () => {
    const document = documentWithModel();
    document.globalSettings.apiProviders[0].models[0] = {
      ...document.globalSettings.apiProviders[0].models[0],
      capabilities: ["image_recognition"]
    };
    document.workspaces[0].conversations[0].contexts = [
      {
        id: "fork-budget-a",
        kind: "user",
        content: "分支第一批",
        images: budgetImages("fork-a", 11),
        createdAt: "2026-07-24T00:00:00Z"
      },
      {
        id: "fork-budget-b",
        kind: "user",
        content: "分支超限点",
        images: budgetImages("fork-b", 11),
        createdAt: "2026-07-24T00:00:01Z"
      }
    ];
    runtimeMocks.loadDocument.mockResolvedValue(document);

    const user = userEvent.setup();
    render(<App />);
    const article = (await screen.findByText("分支超限点")).closest("article")!;
    await user.click(within(article).getByRole("button", { name: "从此消息分支" }));

    // Only the message text carries over, so no image budget is ever projected.
    await waitFor(() => expect(screen.getByRole("textbox", { name: "向 Agent 发送消息" })).toHaveValue("分支超限点"));
    expect(runtimeMocks.runModel).not.toHaveBeenCalled();
  });

  it("allows text at the image limit but rejects an image steer against the running context", async () => {
    const document = documentWithModel();
    document.globalSettings.apiProviders[0].models[0] = {
      ...document.globalSettings.apiProviders[0].models[0],
      capabilities: ["image_recognition"]
    };
    document.workspaces[0].conversations[0].contexts = [{
      id: "steer-budget-history",
      kind: "user",
      content: "",
      images: Array.from({ length: 20 }, (_, index) => ({
        id: `steer-history-${index}`,
        name: `history-${index}.png`,
        mime: "image/png",
        width: 1,
        height: 1,
        bytes: 1
      })),
      createdAt: "2026-07-24T00:00:00Z"
    }];
    runtimeMocks.loadDocument.mockResolvedValue(document);
    runtimeMocks.prepareImageAttachment.mockResolvedValue({
      id: "steer-extra",
      name: "steer.png",
      mime: "image/png",
      width: 1,
      height: 1,
      bytes: 1
    });
    runtimeMocks.runModel.mockImplementation(() => new Promise(() => undefined));

    const user = userEvent.setup();
    render(<App />);
    const composer = await screen.findByLabelText("向 Agent 发送消息");
    await user.type(composer, "纯文本仍可发送");
    await user.click(screen.getByRole("button", { name: "发送" }));
    await waitFor(() => expect(runtimeMocks.runModel).toHaveBeenCalledTimes(1));

    const image = new File([new Uint8Array([1])], "steer.png", { type: "image/png" });
    Object.defineProperty(image, "arrayBuffer", {
      configurable: true,
      value: async () => new Uint8Array([1]).buffer
    });
    fireEvent.drop(composer.closest(".composer")!, {
      dataTransfer: { files: [image], types: ["Files"] }
    });
    expect(await screen.findByRole("img", { name: "steer.png" })).toBeInTheDocument();
    await waitFor(() => expect(screen.getByRole("button", { name: "加入排队消息" })).toBeEnabled());
    await user.click(screen.getByRole("button", { name: "加入排队消息" }));

    const queue = await screen.findByRole("region", { name: "排队消息" });
    await user.click(within(queue).getByRole("button", {
      name: "将“[Image #1] · steer.png”引导到当前回合"
    }));

    // A budget-blocked steer does not send input or remove the queued message.
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    expect(runtimeMocks.steerModelRun).not.toHaveBeenCalled();
    expect(within(queue).getByText("1 张图片")).toBeInTheDocument();
  });

  it("serializes rapid image additions without dropping a later batch", async () => {
    const document = documentWithModel();
    document.globalSettings.apiProviders[0].models[0] = {
      ...document.globalSettings.apiProviders[0].models[0],
      capabilities: ["image_recognition"]
    };
    runtimeMocks.loadDocument.mockResolvedValue(document);
    const pendingUploads: Array<{
      name: string;
      resolve: (image: {
      id: string;
      name: string;
      mime: string;
      width: number;
      height: number;
      bytes: number;
      }) => void;
    }> = [];
    runtimeMocks.prepareImageAttachment.mockImplementation((name: string) => new Promise((resolve) => {
      pendingUploads.push({ name, resolve });
    }));

    render(<App />);
    const composer = await screen.findByLabelText("向 Agent 发送消息");
    const first = new File([new Uint8Array([1])], "first.png", { type: "image/png" });
    const second = new File([new Uint8Array([2])], "second.png", { type: "image/png" });
    for (const file of [first, second]) {
      Object.defineProperty(file, "arrayBuffer", {
        configurable: true,
        value: async () => new Uint8Array([1]).buffer
      });
    }

    fireEvent.drop(composer.closest(".composer")!, {
      dataTransfer: { files: [first], types: ["Files"] }
    });
    fireEvent.drop(composer.closest(".composer")!, {
      dataTransfer: { files: [second], types: ["Files"] }
    });

    await waitFor(() => expect(runtimeMocks.prepareImageAttachment).toHaveBeenCalledTimes(1));
    expect(pendingUploads[0]?.name).toBe("first.png");
    await act(async () => pendingUploads[0].resolve({
      id: "image-first",
      name: "first.png",
      mime: "image/png",
      width: 1,
      height: 1,
      bytes: 1
    }));
    expect(await screen.findByRole("img", { name: "first.png" })).toBeInTheDocument();
    await waitFor(() => expect(runtimeMocks.prepareImageAttachment).toHaveBeenCalledTimes(2));
    expect(pendingUploads[1]?.name).toBe("second.png");
    await act(async () => pendingUploads[1].resolve({
      id: "image-second",
      name: "second.png",
      mime: "image/png",
      width: 1,
      height: 1,
      bytes: 1
    }));
    expect(await screen.findByRole("img", { name: "second.png" })).toBeInTheDocument();
  });

  it("does not restore a pending image draft after its task is deleted and the id is reused", async () => {
    const document = documentWithModel();
    document.globalSettings.apiProviders[0].models[0] = {
      ...document.globalSettings.apiProviders[0].models[0],
      capabilities: ["image_recognition"]
    };
    const removed = document.workspaces[0].conversations[0];
    removed.id = "conv_reused-upload";
    removed.title = "待删除图片任务";
    document.workspaces[0].conversations = [
      removed,
      {
        ...removed,
        id: "conv_upload-sibling",
        title: "保留任务",
        contexts: [],
        queuedMessages: []
      }
    ];
    runtimeMocks.loadDocument.mockResolvedValue(document);
    const upload = deferred<{
      id: string;
      name: string;
      mime: string;
      width: number;
      height: number;
      bytes: number;
    }>();
    runtimeMocks.prepareImageAttachment.mockReturnValue(upload.promise);

    const user = userEvent.setup();
    render(<App />);
    const composer = await screen.findByLabelText("向 Agent 发送消息");
    const file = new File([new Uint8Array([1])], "late-task.png", { type: "image/png" });
    Object.defineProperty(file, "arrayBuffer", {
      configurable: true,
      value: async () => new Uint8Array([1]).buffer
    });
    fireEvent.drop(composer.closest(".composer")!, {
      dataTransfer: { files: [file], types: ["Files"] }
    });
    await waitFor(() => expect(runtimeMocks.prepareImageAttachment).toHaveBeenCalledTimes(1));

    const navigation = screen.getByRole("complementary", { name: "工作区和对话" });
    await user.click(within(navigation).getByRole("button", { name: "删除 待删除图片任务" }));
    await user.click(within(navigation).getByRole("button", { name: "确认删除 待删除图片任务" }));
    await waitFor(() => expect(within(navigation).queryByText("待删除图片任务")).not.toBeInTheDocument());

    const randomUuid = vi.spyOn(globalThis.crypto, "randomUUID")
      .mockReturnValue("reused-upload" as ReturnType<Crypto["randomUUID"]>);
    try {
      await user.click(within(navigation).getByRole("button", { name: "新建任务" }));
      const replacementComposer = await screen.findByLabelText("向 Agent 发送消息");
      await act(async () => upload.resolve({
        id: "late-task-image",
        name: "late-task.png",
        mime: "image/png",
        width: 1,
        height: 1,
        bytes: 1
      }));
      expect(within(replacementComposer.closest(".composer") as HTMLElement)
        .queryByRole("img", { name: "late-task.png" })).not.toBeInTheDocument();
    } finally {
      randomUuid.mockRestore();
    }
  });

  it("does not restore a pending image draft after its workspace is deleted and the id is reused", async () => {
    const document = documentWithModel();
    document.globalSettings.apiProviders[0].models[0] = {
      ...document.globalSettings.apiProviders[0].models[0],
      capabilities: ["image_recognition"]
    };
    const removedWorkspace = document.workspaces[0];
    removedWorkspace.id = "workspace_pending-upload";
    removedWorkspace.name = "待删除图片工作区";
    const removedConversation = removedWorkspace.conversations[0];
    removedConversation.id = "conv_reused-workspace-upload";
    const fallbackWorkspace = {
      ...removedWorkspace,
      id: "workspace_upload-fallback",
      name: "保留工作区",
      path: "C:\\test\\fallback",
      conversations: [{
        ...removedConversation,
        id: "conv_workspace-upload-sibling",
        title: "保留任务",
        contexts: [],
        queuedMessages: []
      }]
    };
    document.workspaces = [removedWorkspace, fallbackWorkspace];
    runtimeMocks.loadDocument.mockResolvedValue(document);
    const upload = deferred<{
      id: string;
      name: string;
      mime: string;
      width: number;
      height: number;
      bytes: number;
    }>();
    runtimeMocks.prepareImageAttachment.mockReturnValue(upload.promise);

    const user = userEvent.setup();
    render(<App />);
    const composer = await screen.findByLabelText("向 Agent 发送消息");
    const file = new File([new Uint8Array([1])], "late-workspace.png", { type: "image/png" });
    Object.defineProperty(file, "arrayBuffer", {
      configurable: true,
      value: async () => new Uint8Array([1]).buffer
    });
    fireEvent.drop(composer.closest(".composer")!, {
      dataTransfer: { files: [file], types: ["Files"] }
    });
    await waitFor(() => expect(runtimeMocks.prepareImageAttachment).toHaveBeenCalledTimes(1));

    const navigation = screen.getByRole("complementary", { name: "工作区和对话" });
    await user.click(within(navigation).getByRole("button", { name: "删除工作区 待删除图片工作区" }));
    await user.click(within(navigation).getByRole("button", { name: "确认删除工作区 待删除图片工作区" }));
    await waitFor(() => expect(
      window.document.querySelector('[data-workspace-group-id="workspace_pending-upload"]')
    ).not.toBeInTheDocument());

    const randomUuid = vi.spyOn(globalThis.crypto, "randomUUID")
      .mockReturnValue("reused-workspace-upload" as ReturnType<Crypto["randomUUID"]>);
    try {
      await user.click(within(navigation).getByRole("button", { name: "新建任务" }));
      const replacementComposer = await screen.findByLabelText("向 Agent 发送消息");
      await act(async () => upload.resolve({
        id: "late-workspace-image",
        name: "late-workspace.png",
        mime: "image/png",
        width: 1,
        height: 1,
        bytes: 1
      }));
      expect(within(replacementComposer.closest(".composer") as HTMLElement)
        .queryByRole("img", { name: "late-workspace.png" })).not.toBeInTheDocument();
    } finally {
      randomUuid.mockRestore();
    }
  });

  it("keeps the active stop action enabled while a new image is still uploading", async () => {
    const document = documentWithModel();
    document.globalSettings.apiProviders[0].models[0] = {
      ...document.globalSettings.apiProviders[0].models[0],
      capabilities: ["image_recognition"]
    };
    runtimeMocks.loadDocument.mockResolvedValue(document);
    let resolveRun!: (value: ModelRunResponse) => void;
    runtimeMocks.runModel.mockImplementation(() => new Promise((resolve) => {
      resolveRun = resolve;
    }));
    let resolveImage!: (image: {
      id: string;
      name: string;
      mime: string;
      width: number;
      height: number;
      bytes: number;
    }) => void;
    runtimeMocks.prepareImageAttachment.mockImplementation(() => new Promise((resolve) => {
      resolveImage = resolve;
    }));

    const user = userEvent.setup();
    render(<App />);
    const composer = await screen.findByLabelText("向 Agent 发送消息");
    await user.type(composer, "开始长任务");
    await user.click(screen.getByRole("button", { name: "发送" }));
    await waitFor(() => expect(runtimeMocks.runModel).toHaveBeenCalledTimes(1));

    const image = new File([new Uint8Array([1])], "pending.png", { type: "image/png" });
    Object.defineProperty(image, "arrayBuffer", {
      configurable: true,
      value: async () => new Uint8Array([1]).buffer
    });
    fireEvent.drop(composer.closest(".composer")!, {
      dataTransfer: { files: [image], types: ["Files"] }
    });
    await waitFor(() => expect(runtimeMocks.prepareImageAttachment).toHaveBeenCalledTimes(1));

    const stopButton = screen.getByRole("button", { name: "停止生成" });
    expect(stopButton).toBeEnabled();
    await user.click(stopButton);
    expect(runtimeMocks.cancelModelRun).toHaveBeenCalledWith(expect.any(String));

    await act(async () => resolveImage({
      id: "pending-image",
      name: "pending.png",
      mime: "image/png",
      width: 1,
      height: 1,
      bytes: 1
    }));
    await act(async () => resolveRun({
      contexts: [],
      usage: {},
      model: model.id,
      providerName: "OpenAI Responses",
      durationMs: 1,
      stopReason: "cancelled"
    }));
  });

  it("preserves an image-only paste through queue, steer, cancellation, and timeline rendering", async () => {
    const document = documentWithModel();
    document.globalSettings.apiProviders[0].models[0] = {
      ...document.globalSettings.apiProviders[0].models[0],
      capabilities: ["image_recognition"]
    };
    const image = {
      id: "image-queue",
      name: "pasted.png",
      mime: "image/png",
      width: 320,
      height: 200,
      bytes: 4
    };
    runtimeMocks.prepareImageAttachment.mockResolvedValue(image);
    runtimeMocks.loadDocument.mockResolvedValue(document);
    let emit!: (event: ModelStreamEvent) => void;
    let resolveRun!: (value: ModelRunResponse) => void;
    runtimeMocks.runModel.mockImplementation((_request, onEvent) => {
      emit = onEvent;
      return new Promise<ModelRunResponse>((resolve) => { resolveRun = resolve; });
    });

    const user = userEvent.setup();
    render(<App />);
    const composer = await screen.findByLabelText("向 Agent 发送消息");
    await user.type(composer, "开始长任务");
    await user.click(screen.getByRole("button", { name: "发送" }));
    await waitFor(() => expect(runtimeMocks.runModel).toHaveBeenCalledTimes(1));

    const file = new File([new Uint8Array([1, 2, 3, 4])], "pasted.png", { type: "image/png" });
    Object.defineProperty(file, "arrayBuffer", {
      configurable: true,
      value: async () => new Uint8Array([1, 2, 3, 4]).buffer
    });
    fireEvent.paste(composer, {
      clipboardData: {
        files: [file],
        getData: () => ""
      }
    });
    expect(await screen.findByRole("img", { name: "pasted.png" })).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "加入排队消息" }));

    const queue = await screen.findByRole("region", { name: "排队消息" });
    expect(within(queue).getByText("1 张图片")).toBeInTheDocument();
    await waitFor(() => expect(runtimeMocks.saveDocument.mock.calls.some(([saved]) => {
      const queued = (saved as AppDocument).workspaces[0].conversations[0].queuedMessages[0];
      return queued?.content === "[Image #1]" && queued.images?.[0]?.id === image.id;
    })).toBe(true));

    await user.click(within(queue).getByRole("button", {
      name: "将“[Image #1] · pasted.png”引导到当前回合"
    }));
    await waitFor(() => expect(runtimeMocks.steerModelRun).toHaveBeenCalledWith(
      expect.any(String),
      expect.objectContaining({ content: "[Image #1]", images: [{ ...image, shortId: 1 }] })
    ));
    const steered = runtimeMocks.steerModelRun.mock.calls[0][1] as QueuedMessage;
    act(() => emit({
      type: "user_input_received",
      round: 2,
      id: steered.id,
      content: steered.content,
      images: steered.images,
      createdAt: steered.createdAt
    }));
    await waitFor(() => expect(screen.queryByRole("region", { name: "排队消息" })).not.toBeInTheDocument());
    expect(screen.getAllByRole("img", { name: "pasted.png" })).toHaveLength(1);

    await user.click(screen.getByRole("button", { name: "停止生成" }));
    expect(runtimeMocks.cancelModelRun).toHaveBeenCalledTimes(1);
    await act(async () => resolveRun({
      contexts: [{
        id: steered.id,
        kind: "user",
        content: steered.content,
        images: steered.images,
        createdAt: steered.createdAt
      }],
      usage: {},
      model: model.id,
      providerName: "OpenAI Responses",
      durationMs: 12,
      stopReason: "cancelled"
    }));
    await waitFor(() => expect(screen.queryByRole("button", { name: "停止生成" })).not.toBeInTheDocument());
    expect(screen.getAllByRole("img", { name: "pasted.png" })).toHaveLength(1);
  });

  it("uses the launch-time vision snapshot for steer and the current model for future uploads", async () => {
    const document = documentWithModel();
    document.globalSettings.apiProviders[0].models[0] = {
      ...document.globalSettings.apiProviders[0].models[0],
      capabilities: ["image_recognition"]
    };
    runtimeMocks.loadDocument.mockResolvedValue(document);
    runtimeMocks.prepareImageAttachment.mockImplementation(async (name: string) => ({
      id: `snapshot-${name}`,
      name,
      mime: "image/png",
      width: 1,
      height: 1,
      bytes: 1
    }));
    runtimeMocks.runModel.mockImplementation(() => new Promise(() => undefined));

    const user = userEvent.setup();
    render(<App />);
    const composer = await screen.findByLabelText("向 Agent 发送消息");
    await user.type(composer, "启动视觉回合");
    await user.click(screen.getByRole("button", { name: "发送" }));
    await waitFor(() => expect(runtimeMocks.runModel).toHaveBeenCalledTimes(1));

    const queuedImage = new File([new Uint8Array([1])], "queued.png", { type: "image/png" });
    Object.defineProperty(queuedImage, "arrayBuffer", {
      configurable: true,
      value: async () => new Uint8Array([1]).buffer
    });
    fireEvent.drop(composer.closest(".composer")!, {
      dataTransfer: { files: [queuedImage], types: ["Files"] }
    });
    expect(await screen.findByRole("img", { name: "queued.png" })).toBeInTheDocument();
    await waitFor(() => expect(screen.getByRole("button", { name: "加入排队消息" })).toBeEnabled());
    await user.click(screen.getByRole("button", { name: "加入排队消息" }));

    await user.click(screen.getByRole("button", { name: "设置" }));
    const settingsSidebar = await screen.findByRole("complementary", { name: "全局设置导航" });
    await user.click(within(settingsSidebar).getByRole("button", { name: "模型提供商" }));
    await user.click(await screen.findByRole("button", { name: "模型 test-model 的属性" }));
    const modelDialog = screen.getByRole("dialog", { name: "模型属性" });
    const visionSwitch = within(modelDialog).getByRole("switch", { name: "test-model 视觉输入" });
    expect(visionSwitch).toHaveAttribute("aria-checked", "true");
    await user.click(visionSwitch);
    await user.click(within(modelDialog).getByRole("button", { name: "保存" }));
    await user.click(within(settingsSidebar).getByRole("button", { name: "返回" }));

    const currentComposer = await screen.findByLabelText("向 Agent 发送消息");
    const futureImage = new File([new Uint8Array([2])], "future.png", { type: "image/png" });
    Object.defineProperty(futureImage, "arrayBuffer", {
      configurable: true,
      value: async () => new Uint8Array([2]).buffer
    });
    fireEvent.drop(currentComposer.closest(".composer")!, {
      dataTransfer: { files: [futureImage], types: ["Files"] }
    });
    // After switching to a text-only model, silently reject new image uploads.
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    expect(runtimeMocks.prepareImageAttachment).toHaveBeenCalledTimes(1);
    expect(screen.queryByRole("img", { name: "future.png" })).not.toBeInTheDocument();

    const queue = screen.getByRole("region", { name: "排队消息" });
    const steer = within(queue).getByRole("button", {
      name: "将“[Image #1] · queued.png”引导到当前回合"
    });
    expect(steer).toBeEnabled();
    await user.click(steer);
    await waitFor(() => expect(runtimeMocks.steerModelRun).toHaveBeenCalledWith(
      expect.any(String),
      expect.objectContaining({ images: [expect.objectContaining({ id: "snapshot-queued.png" })] })
    ));
  });

  it("keeps a reloaded image queue intact until a vision-capable model is selected", async () => {
    const document = documentWithModel();
    const image = {
      id: "reload-image",
      name: "reload.png",
      mime: "image/png",
      width: 20,
      height: 20,
      bytes: 80
    };
    document.workspaces[0].conversations[0].queuedMessages = [{
      id: "reload-image-message",
      content: "",
      images: [image],
      createdAt: "2026-07-24T00:00:00Z"
    }];
    runtimeMocks.loadDocument.mockResolvedValue(document);

    render(<App />);

    const queue = await screen.findByRole("region", { name: "排队消息" });
    expect(within(queue).getByText("1 张图片")).toBeInTheDocument();
    // A blocked automatic dispatch leaves the queue intact and sends no request.
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    expect(runtimeMocks.runModel).not.toHaveBeenCalled();
    expect(runtimeMocks.saveDocument.mock.calls.every(([saved]) => (
      (saved as AppDocument).workspaces[0].conversations[0].queuedMessages.some(
        (message) => message.id === "reload-image-message"
      )
    ))).toBe(true);
  });

  it("keeps an over-budget image queue head intact before automatic dispatch", async () => {
    const document = documentWithModel();
    document.globalSettings.apiProviders[0].models[0] = {
      ...document.globalSettings.apiProviders[0].models[0],
      capabilities: ["image_recognition"]
    };
    const historicalImages = Array.from({ length: 20 }, (_, index) => ({
      id: `queued-history-${index}`,
      name: `history-${index}.png`,
      mime: "image/png",
      width: 1,
      height: 1,
      bytes: 1
    }));
    document.workspaces[0].conversations[0].contexts = [{
      id: "queued-budget-history",
      kind: "user",
      content: "",
      images: historicalImages,
      createdAt: "2026-07-24T00:00:00Z"
    }];
    document.workspaces[0].conversations[0].queuedMessages = [{
      id: "over-budget-head",
      content: "",
      images: [{
        id: "queued-extra",
        name: "extra.png",
        mime: "image/png",
        width: 1,
        height: 1,
        bytes: 1
      }],
      createdAt: "2026-07-24T00:00:01Z"
    }];
    runtimeMocks.loadDocument.mockResolvedValue(document);

    render(<App />);

    const queue = await screen.findByRole("region", { name: "排队消息" });
    expect(within(queue).getByText("1 张图片")).toBeInTheDocument();
    // A budget-blocked queue head is neither promoted nor discarded.
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    expect(runtimeMocks.runModel).not.toHaveBeenCalled();
    expect(runtimeMocks.saveDocument.mock.calls.every(([saved]) => (
      (saved as AppDocument).workspaces[0].conversations[0].queuedMessages.some(
        (message) => message.id === "over-budget-head"
      )
    ))).toBe(true);
  });

  it("keeps new messages behind a blocked queue head and lets that head be deleted", async () => {
    const document = documentWithModel();
    document.workspaces[0].conversations[0].queuedMessages = [{
      id: "blocked-image-head",
      content: "",
      images: [{
        id: "blocked-image",
        name: "blocked.png",
        mime: "image/png",
        width: 20,
        height: 20,
        bytes: 80
      }],
      createdAt: "2026-07-24T00:00:00Z"
    }];
    runtimeMocks.loadDocument.mockResolvedValue(document);
    runtimeMocks.runModel.mockResolvedValue({
      contexts: [{
        id: "after-blocked-head",
        kind: "assistant",
        content: "队列已继续",
        createdAt: "2026-07-24T00:00:02Z"
      }],
      usage: {},
      model: model.id,
      providerName: "OpenAI Responses",
      durationMs: 2
    });

    const user = userEvent.setup();
    render(<App />);
    const composer = await screen.findByLabelText("向 Agent 发送消息");
    // Confirm the blocked head remains queued before adding a later message.
    expect(within(await screen.findByRole("region", { name: "排队消息" }))
      .getByText("1 张图片")).toBeInTheDocument();

    await user.type(composer, "排在图片之后");
    await user.click(screen.getByRole("button", { name: "加入排队消息" }));
    const queue = await screen.findByRole("region", { name: "排队消息" });
    expect(within(queue).getAllByRole("listitem")).toHaveLength(2);
    expect(runtimeMocks.runModel).not.toHaveBeenCalled();

    await user.click(within(queue).getByRole("button", {
      name: "删除排队消息“blocked.png”"
    }));

    await waitFor(() => expect(runtimeMocks.runModel).toHaveBeenCalledTimes(1));
    const request = runtimeMocks.runModel.mock.calls[0][0] as ModelRunRequest;
    expect(request.contexts.at(-1)).toEqual(expect.objectContaining({
      kind: "user",
      content: "排在图片之后"
    }));
    expect(await screen.findByText("队列已继续")).toBeInTheDocument();
  });
});
