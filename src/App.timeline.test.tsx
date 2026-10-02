import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import App from "./App";
import { configureI18n } from "./i18n";
import type {
  AppDocument,
  ContextItem,
  ModelRunRequest,
  ToolContext
} from "./types";
import { resetAppMocks, answeredQuestionPair, documentWithModel, model, runtimeMocks } from "./test/appMocks";
import { EMPTY_TOOL_LOCK } from "./lib/toolLock";

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

/** The main conversation's timeline, where the undo keys answer. */
const mainTimeline = () => window.document.querySelector<HTMLElement>(".conversation-pane__main .context-scroll")!;

describe("App model run flow — timeline", () => {
  beforeEach(resetAppMocks);

  it("exposes the complete tool catalog in temporary-workspace model requests", async () => {
    const document = documentWithModel();
    // Tool descriptions are loaded from trusted files at runtime; a selection is only a resource ID.
    // Requests therefore contain built-in text, not the selected file's content.
    for (const workspace of document.workspaces) {
      for (const conversation of workspace.conversations) {
        conversation.settings.toolDescriptionFileId = "tooldesc_user_main_0f0f0f0f";
      }
    }
    for (const preset of document.globalSettings.conversationPresets) {
      preset.settings.toolDescriptionFileId = "tooldesc_user_main_0f0f0f0f";
    }
    runtimeMocks.loadDocument.mockResolvedValue(document);
    runtimeMocks.runModel.mockResolvedValue({
      contexts: [{ id: "ctx_temporary", kind: "assistant", content: "临时工作区回复", createdAt: "2026-07-11T00:00:00Z" }],
      usage: { inputTokens: 1, outputTokens: 1, totalTokens: 2 },
      model: model.id,
      providerName: "OpenAI Responses",
      durationMs: 1,
      stopReason: "completed"
    });
    const user = userEvent.setup();

    render(<App />);
    await user.click(await screen.findByRole("button", { name: `项目：${document.workspaces[0].name}` }));
    await user.click(within(screen.getByRole("menu", { name: "选择项目" })).getByRole("menuitemradio", { name: "临时项目" }));
    await user.type(screen.getByLabelText("向 Agent 发送消息"), "检查临时目录");
    await user.click(screen.getByRole("button", { name: "发送" }));

    await waitFor(() => expect(runtimeMocks.runModel).toHaveBeenCalledTimes(1));
    const request = runtimeMocks.runModel.mock.calls[0][0] as ModelRunRequest;
    expect(request).toEqual(expect.objectContaining({
      workspacePath: "",
      enabledTools: document.tools.map((tool) => tool.name)
    }));
    expect(request.tools.map((tool) => tool.name)).toEqual(document.tools.map((tool) => tool.name));
    expect(request.tools.map((tool) => tool.name)).toEqual(expect.arrayContaining(["ls", "powershell", "bash"]));
    expect(request.tools.find((tool) => tool.name === "ls")?.parameters[0].defaultValue).toBe(".");
    // The renderer never produces tool descriptions. Assert an empty string rather than equality with the seed, which could make matching defects pass.
    expect(request.tools.find((tool) => tool.name === "ls")?.description).toBe("");
  });

  it("sends the conversation snapshot and appends the generated response", async () => {
    const document = documentWithModel();
    runtimeMocks.loadDocument.mockResolvedValue(document);
    runtimeMocks.runModel.mockResolvedValue({
      contexts: [{ id: "ctx_generated", kind: "assistant", content: "模型已经回复", createdAt: "2026-07-11T00:00:00Z" }],
      usage: { inputTokens: 8, outputTokens: 6, totalTokens: 14 },
      model: model.id,
      providerName: "OpenAI Responses",
      durationMs: 42,
      stopReason: "completed"
    });

    const user = userEvent.setup();
    render(<App />);
    const composer = await screen.findByLabelText("向 Agent 发送消息");
    await user.type(composer, "请检查项目");
    await user.click(screen.getByRole("button", { name: "发送" }));

    await waitFor(() => expect(runtimeMocks.runModel).toHaveBeenCalledTimes(1));
    expect(runtimeMocks.runModel).toHaveBeenCalledWith(expect.objectContaining({
      conversationId: document.workspaces[0].conversations[0].id,
      workspacePath: document.workspaces[0].path,
      model,
      reasoningEffort: "low",
      contexts: [expect.objectContaining({ kind: "user", content: "请检查项目" })]
    }), expect.any(Function), expect.any(String));
    expect(await screen.findByText("模型已经回复")).toBeInTheDocument();
    expect(composer).toHaveValue("");
  });

  it("deletes an unanswered question from before questions blocked, and restores it with one undo", async () => {
    const document = documentWithModel();
    const { ask } = answeredQuestionPair();
    document.workspaces[0].conversations[0].contexts = [ask];
    runtimeMocks.loadDocument.mockResolvedValue(document);
    const user = userEvent.setup();

    render(<App />);
    // An old paused question has no card to answer: it sits in the timeline.
    expect(await screen.findByText("采用哪个方案？")).toBeInTheDocument();
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "删除提问" }));

    await waitFor(() => expect(screen.queryByText("采用哪个方案？")).not.toBeInTheDocument());
    expect(runtimeMocks.runModel).not.toHaveBeenCalled();
    // No undo control below the timeline: one line says what happened and which key takes it back.
    expect(screen.queryByRole("button", { name: /撤销删除/ })).not.toBeInTheDocument();
    const notice = screen.getByText("已删除 1 条消息").closest(".timeline-notice") as HTMLElement;
    expect(within(notice).getByText("Ctrl+Z")).toBeInTheDocument();

    await waitFor(() => expect(mainTimeline()).toHaveFocus());
    await user.keyboard("{Control>}z{/Control}");
    expect(await screen.findByText("采用哪个方案？")).toBeInTheDocument();
    expect(screen.getByText("已撤回：删除提问消息")).toBeInTheDocument();
  });

  it("answers the undo keys only while focus is in the timeline, and redoes with Ctrl+X", async () => {
    const document = documentWithModel();
    const { ask } = answeredQuestionPair();
    document.workspaces[0].conversations[0].contexts = [ask];
    runtimeMocks.loadDocument.mockResolvedValue(document);
    const user = userEvent.setup();

    render(<App />);
    expect(await screen.findByText("采用哪个方案？")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "删除提问" }));
    await waitFor(() => expect(screen.queryByText("采用哪个方案？")).not.toBeInTheDocument());

    // The composer's own Ctrl+Z belongs to its text.
    await user.click(screen.getByLabelText("向 Agent 发送消息"));
    await user.keyboard("{Control>}z{/Control}");
    expect(screen.queryByText("采用哪个方案？")).not.toBeInTheDocument();

    mainTimeline().focus();
    await user.keyboard("{Control>}z{/Control}");
    expect(await screen.findByText("采用哪个方案？")).toBeInTheDocument();
    await user.keyboard("{Control>}x{/Control}");
    await waitFor(() => expect(screen.queryByText("采用哪个方案？")).not.toBeInTheDocument());
    expect(screen.getByText("已重做：删除提问消息")).toBeInTheDocument();
    // Both directions are spent until something else happens.
    await user.keyboard("{Control>}x{/Control}");
    expect(screen.getByText("没有可重做的修改")).toBeInTheDocument();
  });

  it("keeps each conversation's undo history to itself", async () => {
    const document = documentWithModel();
    const [first] = document.workspaces[0].conversations;
    first.contexts = [{ id: "first-reply", kind: "assistant", content: "第一段对话的回复", createdAt: "2026-07-24T00:00:00Z" }];
    const second = {
      ...structuredClone(first),
      id: "conv_second",
      title: "第二段对话",
      contexts: [{ id: "second-reply", kind: "assistant" as const, content: "第二段对话的回复", createdAt: "2026-07-24T00:00:00Z" }]
    };
    document.workspaces[0].conversations.push(second);
    runtimeMocks.loadDocument.mockResolvedValue(document);
    const user = userEvent.setup();

    render(<App />);
    const firstReply = (await screen.findByText("第一段对话的回复")).closest("article") as HTMLElement;
    await user.click(within(firstReply).getByRole("button", { name: "删除上下文" }));
    await waitFor(() => expect(screen.queryByText("第一段对话的回复")).not.toBeInTheDocument());

    await user.click(screen.getByText("第二段对话"));
    expect(await screen.findByText("第二段对话的回复")).toBeInTheDocument();
    mainTimeline().focus();
    await user.keyboard("{Control>}z{/Control}");
    expect(screen.getByText("没有可撤回的修改")).toBeInTheDocument();
    expect(screen.getByText("第二段对话的回复")).toBeInTheDocument();
  });

  it("deletes and restores an answered question and answer as one timeline message", async () => {
    const document = documentWithModel();
    const { ask, answer } = answeredQuestionPair();
    document.workspaces[0].conversations[0].contexts = [ask, answer];
    runtimeMocks.loadDocument.mockResolvedValue(document);
    const user = userEvent.setup();

    render(<App />);
    const card = (await screen.findByText("采用哪个方案？")).closest(".question-history") as HTMLElement;
    expect(card).toBeInTheDocument();
    expect(within(card).getByText("方案 A")).toBeInTheDocument();
    expect(within(card).getAllByRole("button")).toHaveLength(2);
    await user.click(within(card).getByRole("button", { name: "删除整条提问消息" }));

    await waitFor(() => expect(screen.queryByText("采用哪个方案？")).not.toBeInTheDocument());
    expect(screen.queryByText("方案 A")).not.toBeInTheDocument();

    await waitFor(() => expect(mainTimeline()).toHaveFocus());
    await user.keyboard("{Control>}z{/Control}");
    const restored = (await screen.findByText("采用哪个方案？")).closest(".question-history") as HTMLElement;
    expect(within(restored).getByText("方案 A")).toBeInTheDocument();
    expect(screen.queryByRole("dialog", { name: "需要你的回答" })).not.toBeInTheDocument();
  });

  it("deletes a tool call row and restores it from the undo entry", async () => {
    const appDocument = documentWithModel();
    const conversation = appDocument.workspaces[0].conversations[0];
    const readCall = (id: string, path: string): ToolContext => ({
      id,
      kind: "tool",
      toolName: "read",
      round: 1,
      input: { path },
      result: { success: true, output: `${path} 的内容`, executedAt: "2026-07-24T00:00:00Z", durationMs: 1 },
      createdAt: "2026-07-24T00:00:00Z"
    });
    conversation.contexts = [readCall("read-kept", "src/kept.ts"), readCall("read-deleted", "src/deleted.ts")];
    runtimeMocks.loadDocument.mockResolvedValue(appDocument);
    const user = userEvent.setup();

    render(<App />);
    const deletedRow = () => window.document.querySelector<HTMLElement>('[data-context-id="read-deleted"]');
    const row = await waitFor(() => {
      expect(deletedRow()).not.toBeNull();
      return deletedRow()!;
    });

    runtimeMocks.saveDocument.mockClear();
    await user.click(within(row).getByRole("button", { name: "删除工具调用 已读取：deleted.ts" }));

    await waitFor(() => expect(deletedRow()).toBeNull());
    await waitFor(() => expect(runtimeMocks.saveDocument.mock.calls.some(([saved]) => (
      saved.workspaces[0].conversations[0].contexts.map((context: { id: string }) => context.id).join(",")
      === "read-kept"
    ))).toBe(true));

    await waitFor(() => expect(mainTimeline()).toHaveFocus());
    await user.keyboard("{Control>}z{/Control}");
    await waitFor(() => expect(deletedRow()).not.toBeNull());
    await waitFor(() => expect(runtimeMocks.saveDocument.mock.calls.some(([saved]) => (
      saved.workspaces[0].conversations[0].contexts.map((context: { id: string }) => context.id).join(",")
      === "read-kept,read-deleted"
    ))).toBe(true));
  });

  it("edits an answered question and its mapped answer in one fixed-count editor", async () => {
    const document = documentWithModel();
    const { ask, answer } = answeredQuestionPair();
    document.workspaces[0].conversations[0].contexts = [ask, answer];
    runtimeMocks.loadDocument.mockResolvedValue(document);
    const user = userEvent.setup();

    render(<App />);
    const card = (await screen.findByText("采用哪个方案？")).closest(".question-history") as HTMLElement;
    await user.click(within(card).getByRole("button", { name: "编辑提问与回答" }));
    // The editor replaces the card's own body rather than opening over it.
    const dialog = within(card).getByRole("form", { name: "编辑提问与回答" });
    expect(within(dialog).getAllByRole("heading", { level: 3 })).toHaveLength(1);
    expect(within(dialog).queryByRole("button", { name: /增加问题|删除问题/ })).not.toBeInTheDocument();

    const prompt = within(dialog).getByLabelText("第 1 题问题内容");
    const response = within(dialog).getByLabelText("第 1 题回答");
    await user.clear(prompt);
    await user.type(prompt, "最终采用哪个方案？");
    await user.clear(response);
    await user.type(response, "方案 B，并补齐回归测试");
    await user.click(within(dialog).getByRole("button", { name: "保存" }));

    expect(await screen.findByText("最终采用哪个方案？")).toBeInTheDocument();
    const updatedAnswer = screen.getByText("方案 B，并补齐回归测试");
    expect(updatedAnswer.closest(".question-history__output")).toBeInTheDocument();
    expect(screen.queryByText("采用哪个方案？")).not.toBeInTheDocument();
    expect(runtimeMocks.executeTool).not.toHaveBeenCalled();
  });

  it("branches a user message into an independent conversation carrying its own copy of the history", async () => {
    const document = documentWithModel();
    const conversation = document.workspaces[0].conversations[0];
    conversation.settings.securityLevel = "full_access";
    const remembered = document.workspaces[0].lastConversationSettings;
    conversation.contexts = [
      { id: "branch-sys", kind: "system", content: "系统", createdAt: "2026-07-20T00:00:00Z" },
      { id: "branch-u1", kind: "user", content: "第一问", createdAt: "2026-07-20T00:00:01Z" },
      { id: "branch-a1", kind: "assistant", content: "旧的第一答", createdAt: "2026-07-20T00:00:02Z" },
      { id: "branch-u2", kind: "user", content: "第二问", createdAt: "2026-07-20T00:00:03Z" }
    ];
    runtimeMocks.loadDocument.mockResolvedValue(document);
    // The host returns a copy with its own ids; nothing is shared by identity.
    runtimeMocks.forkConversationContexts.mockResolvedValue([
      { id: "copied-sys", kind: "system", content: "系统", createdAt: "2026-07-20T00:00:00Z" },
      { id: "copied-u1", kind: "user", content: "第一问", createdAt: "2026-07-20T00:00:01Z" },
      { id: "copied-a1", kind: "assistant", content: "旧的第一答", createdAt: "2026-07-20T00:00:02Z" }
    ]);

    const user = userEvent.setup();
    render(<App />);
    const secondQuestion = (await screen.findByText("第二问")).closest("article")!;
    await user.click(within(secondQuestion).getByRole("button", { name: "从此消息分支" }));

    // The branched message itself lands in the composer so it can be edited
    // before it is sent. Nothing runs on its own.
    const composer = await screen.findByRole("textbox", { name: "向 Agent 发送消息" });
    await waitFor(() => expect(composer).toHaveValue("第二问"));
    expect(runtimeMocks.runModel).not.toHaveBeenCalled();

    // Everything strictly before the branch point is copied by the host.
    await waitFor(() => expect(runtimeMocks.forkConversationContexts).toHaveBeenCalledTimes(1));
    expect(runtimeMocks.forkConversationContexts.mock.calls[0][0]).toMatchObject({
      sourceConversationId: conversation.id,
      throughContextId: "branch-a1"
    });

    await waitFor(() => expect(runtimeMocks.saveDocument.mock.calls.some(([snapshot]) => (
      (snapshot as AppDocument).workspaces[0].conversations.some(
        (candidate) => candidate.contexts.some((context) => context.id === "copied-a1")
      )
    ))).toBe(true));
    const saved = runtimeMocks.saveDocument.mock.calls
      .map(([snapshot]) => snapshot as AppDocument)
      .at(-1)!;
    const conversations = saved.workspaces[0].conversations;
    expect(conversations).toHaveLength(2);

    // The branch owns a full, separately persisted copy...
    const branch = conversations.find((candidate) => candidate.id !== conversation.id)!;
    expect(branch.contexts.map((context) => context.id))
      .toEqual(["copied-sys", "copied-u1", "copied-a1"]);
    // ...retries the source's work under the source's settings, without making them what the
    // project's next new task starts from...
    expect(branch.settings.securityLevel).toBe("full_access");
    expect(saved.workspaces[0].lastConversationSettings).toEqual(remembered);
    // ...remembers where it came from, yet the sidebar lists it as an ordinary conversation...
    expect(branch.parentConversationId).toBe(conversation.id);
    const parentRow = window.document.querySelector(`[data-conversation-id="${conversation.id}"]`) as HTMLElement;
    const branchRow = window.document.querySelector(`[data-conversation-id="${branch.id}"]`) as HTMLElement;
    expect(branchRow.parentElement).toBe(parentRow.parentElement);
    expect(within(parentRow).queryByRole("button", { name: "收起子会话" })).toBeNull();

    // ...and the original is untouched.
    const original = conversations.find((candidate) => candidate.id === conversation.id)!;
    expect(original.contexts.map((context) => context.id))
      .toEqual(["branch-sys", "branch-u1", "branch-a1", "branch-u2"]);
  });

  it("flushes the branch before the host copy, and the host accepts that flushed target", async () => {
    const document = documentWithModel();
    const conversation = document.workspaces[0].conversations[0];
    conversation.contexts = [
      { id: "order-u1", kind: "user", content: "第一问", createdAt: "2026-07-20T00:00:01Z" },
      { id: "order-a1", kind: "assistant", content: "第一答", createdAt: "2026-07-20T00:00:02Z" },
      { id: "order-u2", kind: "user", content: "第二问", createdAt: "2026-07-20T00:00:03Z" }
    ];
    runtimeMocks.loadDocument.mockResolvedValue(document);

    // Stand in for the host guard. The renderer flushes the new conversation to
    // disk *because* the host copies from its own committed document, so by the
    // time the host runs, the target already exists and is empty. Rejecting that
    // — as the host used to — made every fork past the first message fail.
    runtimeMocks.forkConversationContexts.mockImplementation(async ({ targetConversationId }) => {
      const flushed = runtimeMocks.saveDocument.mock.calls
        .map(([snapshot]) => snapshot as AppDocument)
        .at(-1);
      const target = flushed?.workspaces[0].conversations
        .find((candidate) => candidate.id === targetConversationId);
      if (!target) throw new Error(`分支目标对话 ${targetConversationId} 尚未落盘`);
      if (target.contexts.length > 0) {
        throw new Error(`分支目标对话 ${targetConversationId} 已有历史，无法作为分支目标`);
      }
      return [
        { id: "order-copied-u1", kind: "user", content: "第一问", createdAt: "2026-07-20T00:00:01Z" },
        { id: "order-copied-a1", kind: "assistant", content: "第一答", createdAt: "2026-07-20T00:00:02Z" }
      ];
    });

    const user = userEvent.setup();
    render(<App />);
    const secondQuestion = (await screen.findByText("第二问")).closest("article")!;
    await user.click(within(secondQuestion).getByRole("button", { name: "从此消息分支" }));

    await waitFor(() => expect(runtimeMocks.forkConversationContexts).toHaveBeenCalledTimes(1));
    await waitFor(() => expect(runtimeMocks.saveDocument.mock.calls.some(([snapshot]) => (
      (snapshot as AppDocument).workspaces[0].conversations.some(
        (candidate) => candidate.contexts.some((context) => context.id === "order-copied-a1")
      )
    ))).toBe(true));
  });

  it("forks the timeline above the insertion line into a conversation named after its origin", async () => {
    const document = documentWithModel();
    const conversation = document.workspaces[0].conversations[0];
    conversation.title = "修复登录";
    conversation.settings.enabledTools = ["read"];
    const sentAt = new Date(Date.now() - 10 * 60_000).toISOString();
    const lastRequest = { providerId: document.globalSettings.apiProviders[0].id, modelId: "test-model", at: sentAt };
    conversation.settings.toolLock = {
      ...EMPTY_TOOL_LOCK,
      tools: ["read"],
      promptSkillIds: [],
      lastRequest,
      modelRequests: [lastRequest]
    };
    conversation.contexts = [
      { id: "fork-u1", kind: "user", content: "第一问", createdAt: "2026-07-20T00:00:01Z" },
      { id: "fork-a1", kind: "assistant", content: "第一答", createdAt: "2026-07-20T00:00:02Z" },
      { id: "fork-u2", kind: "user", content: "第二问", createdAt: "2026-07-20T00:00:03Z" }
    ];
    runtimeMocks.loadDocument.mockResolvedValue(document);
    runtimeMocks.forkConversationContexts.mockImplementation(async ({ sourceContexts, throughContextId }: {
      sourceContexts: ContextItem[];
      throughContextId: string;
    }) => {
      const cut = sourceContexts.findIndex((context) => context.id === throughContextId);
      const copy = runtimeMocks.forkConversationContexts.mock.calls.length;
      return sourceContexts.slice(0, cut + 1).map((context) => ({ ...context, id: `${context.id}-copy${copy}` }));
    });
    const saved = () => runtimeMocks.saveDocument.mock.calls
      .map(([snapshot]) => snapshot as AppDocument)
      .at(-1)?.workspaces[0].conversations ?? [];
    const savedTitled = (title: string) => saved().find((candidate) => candidate.title === title);

    const user = userEvent.setup();
    render(<App />);
    const answer = (await screen.findByText("第一答")).closest("article")!;
    const header = window.document.querySelector<HTMLElement>(".topbar")!;
    // The lower half of the answer puts the line under it.
    fireEvent.contextMenu(answer, { clientX: 40, clientY: 1 });
    await user.click(within(screen.getByRole("menu")).getByRole("menuitem", { name: "分叉会话" }));

    expect(await within(header).findByRole("button", { name: "对话标题：修复登录-fork-1，点击重命名" })).toBeInTheDocument();
    await waitFor(() => expect(runtimeMocks.forkConversationContexts).toHaveBeenCalledTimes(1));
    expect(runtimeMocks.forkConversationContexts.mock.calls[0][0]).toMatchObject({
      sourceConversationId: conversation.id,
      throughContextId: "fork-a1"
    });
    await waitFor(() => expect(savedTitled("修复登录-fork-1")?.contexts.map((context) => context.id))
      .toEqual(["fork-u1-copy1", "fork-a1-copy1"]));
    const first = savedTitled("修复登录-fork-1")!;
    expect(first.forkOf).toEqual({ conversationId: conversation.id, number: 1 });
    // It carries on the source's work, so it keeps the source's settings.
    expect(first.settings.enabledTools).toEqual(["read"]);
    // And the source's cache: the same lock, down to the moment it was sent,
    // so it runs out when the source's does rather than starting over.
    expect(first.settings.toolLock?.lastRequest).toEqual(lastRequest);
    expect(first.settings.toolLock?.modelRequests).toEqual([lastRequest]);
    expect(first.settings.toolLock?.tools).toEqual(["read"]);
    // Nothing is handed to the composer, and the source keeps its whole timeline.
    expect(screen.getByRole("textbox", { name: "向 Agent 发送消息" })).toHaveValue("");
    expect(saved().find((candidate) => candidate.id === conversation.id)?.contexts).toHaveLength(3);

    // A fork of the fork is numbered and named under the same origin.
    fireEvent.contextMenu(window.document.querySelector(".context-stream")!, { clientX: 40, clientY: 400 });
    await user.click(within(screen.getByRole("menu")).getByRole("menuitem", { name: "分叉会话" }));
    expect(await within(header).findByRole("button", { name: "对话标题：修复登录-fork-2，点击重命名" })).toBeInTheDocument();
    await waitFor(() => expect(runtimeMocks.forkConversationContexts).toHaveBeenCalledTimes(2));
    expect(runtimeMocks.forkConversationContexts.mock.calls[1][0]).toMatchObject({
      sourceConversationId: first.id,
      throughContextId: "fork-a1-copy1"
    });
    await waitFor(() => expect(savedTitled("修复登录-fork-2")?.forkOf)
      .toEqual({ conversationId: conversation.id, number: 2 }));

    // Renaming the origin renames its forks.
    const originRow = window.document.querySelector<HTMLElement>(`[data-conversation-id="${conversation.id}"]`)!;
    await user.click(within(originRow).getByText("修复登录"));
    await user.click(within(header).getByRole("button", { name: "对话标题：修复登录，点击重命名" }));
    await user.clear(within(header).getByRole("textbox", { name: "对话标题" }));
    await user.keyboard("修复注册{Enter}");
    await waitFor(() => expect(saved().map((candidate) => candidate.title))
      .toEqual(expect.arrayContaining(["修复注册", "修复注册-fork-1", "修复注册-fork-2"])));
  });

  it("branches the first message into an empty conversation without copying history", async () => {
    const document = documentWithModel();
    const conversation = document.workspaces[0].conversations[0];
    conversation.contexts = [
      { id: "only-u1", kind: "user", content: "唯一的问题", createdAt: "2026-07-20T00:00:00Z" }
    ];
    runtimeMocks.loadDocument.mockResolvedValue(document);

    const user = userEvent.setup();
    render(<App />);
    const onlyQuestion = (await screen.findByText("唯一的问题")).closest("article")!;
    await user.click(within(onlyQuestion).getByRole("button", { name: "从此消息分支" }));

    const composer = await screen.findByRole("textbox", { name: "向 Agent 发送消息" });
    await waitFor(() => expect(composer).toHaveValue("唯一的问题"));
    // There is nothing before the branch point, so the host is never asked.
    expect(runtimeMocks.forkConversationContexts).not.toHaveBeenCalled();
  });

  it("carries the source's lock into a branch that copies history, and none into one that does not", async () => {
    const document = documentWithModel();
    const conversation = document.workspaces[0].conversations[0];
    const lastRequest = {
      providerId: document.globalSettings.apiProviders[0].id,
      modelId: "test-model",
      at: new Date(Date.now() - 5 * 60_000).toISOString()
    };
    conversation.settings.toolLock = { ...EMPTY_TOOL_LOCK, promptSkillIds: [], lastRequest, modelRequests: [lastRequest] };
    conversation.contexts = [
      { id: "lock-u1", kind: "user", content: "第一问", createdAt: "2026-07-20T00:00:01Z" },
      { id: "lock-a1", kind: "assistant", content: "第一答", createdAt: "2026-07-20T00:00:02Z" },
      { id: "lock-u2", kind: "user", content: "第二问", createdAt: "2026-07-20T00:00:03Z" }
    ];
    runtimeMocks.loadDocument.mockResolvedValue(document);
    runtimeMocks.forkConversationContexts.mockImplementation(async ({ sourceContexts, throughContextId }: {
      sourceContexts: ContextItem[];
      throughContextId: string;
    }) => sourceContexts
      .slice(0, sourceContexts.findIndex((context) => context.id === throughContextId) + 1)
      .map((context) => ({ ...context, id: `${context.id}-copy` })));
    const branches = () => (runtimeMocks.saveDocument.mock.calls.at(-1)?.[0] as AppDocument | undefined)
      ?.workspaces[0].conversations.filter((candidate) => candidate.parentConversationId === conversation.id) ?? [];

    const user = userEvent.setup();
    render(<App />);
    const secondQuestion = (await screen.findByText("第二问")).closest("article")!;
    await user.click(within(secondQuestion).getByRole("button", { name: "从此消息分支" }));
    await waitFor(() => expect(branches()).toHaveLength(1));
    expect(branches()[0].settings.toolLock?.lastRequest).toEqual(lastRequest);

    const originRow = window.document.querySelector<HTMLElement>(`[data-conversation-id="${conversation.id}"]`)!;
    await user.click(within(originRow).getByText(conversation.title));
    const firstQuestion = (await screen.findByText("第一问")).closest("article")!;
    await user.click(within(firstQuestion).getByRole("button", { name: "从此消息分支" }));
    await waitFor(() => expect(branches()).toHaveLength(2));
    const empty = branches().find((candidate) => candidate.contexts.length === 0);
    expect(empty?.settings.toolLock).toBeUndefined();
  });
});
