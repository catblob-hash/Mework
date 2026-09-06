import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import App from "./App";
import { configureI18n } from "./i18n";
import type { AppDocument, Conversation, PendingForkRequest } from "./types";
import { documentWithModel, model, resetAppMocks, runtimeMocks } from "./test/appMocks";
import { emitAppPushEvent } from "./test/appMockInstances";

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

function forkRequest(document: AppDocument, overrides: Partial<PendingForkRequest> = {}): PendingForkRequest {
  const source = document.workspaces[0].conversations[0];
  return {
    forkId: "fork-1",
    workspaceId: document.workspaces[0].id,
    sourceConversationId: source.id,
    sourceTitle: source.title,
    prompt: "把测试也跑一遍",
    inheritContext: true,
    requestedAt: "2026-09-05T00:00:00Z",
    ...overrides
  };
}

/** The child exactly as the host would hand it back: the copied history under
 * fresh ids, then the prompt as the last user message, parented to the source. */
function forkedChild(document: AppDocument, prompt: string): Conversation {
  const source = document.workspaces[0].conversations[0];
  return {
    ...source,
    id: "conv_forked_child",
    title: prompt,
    contexts: [
      { id: "copied-u1", kind: "user", content: "第一问", createdAt: "2026-09-05T00:00:00Z" },
      { id: "copied-a1", kind: "assistant", content: "第一答", createdAt: "2026-09-05T00:00:01Z" },
      { id: "ctx_fork_prompt", kind: "user", content: prompt, createdAt: "2026-09-05T00:00:02Z" }
    ],
    parentConversationId: source.id
  };
}

describe("App fork requests", () => {
  beforeEach(resetAppMocks);

  it("draws a pushed fork request in the tray, approves it through the host, and starts the child's run nested under its source", async () => {
    const document = documentWithModel();
    const source = document.workspaces[0].conversations[0];
    source.contexts = [
      { id: "u1", kind: "user", content: "第一问", createdAt: "2026-09-05T00:00:00Z" },
      { id: "a1", kind: "assistant", content: "第一答", createdAt: "2026-09-05T00:00:01Z" }
    ];
    runtimeMocks.loadDocument.mockResolvedValue(document);
    const child = forkedChild(document, "把测试也跑一遍");
    runtimeMocks.loadConversationRemote.mockResolvedValue(child);
    runtimeMocks.runModel.mockResolvedValue({
      contexts: [{ id: "ctx_child_reply", kind: "assistant", content: "子会话已回答", createdAt: "2026-09-05T00:00:03Z" }],
      usage: {},
      model: model.id,
      providerName: "Test Provider",
      durationMs: 5
    });

    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");

    await act(async () => {
      emitAppPushEvent({ type: "forkRequested", ...forkRequest(document) });
    });
    const tray = await screen.findByRole("region", { name: "分叉请求" });
    expect(within(tray).getByText("把测试也跑一遍")).toBeInTheDocument();
    expect(within(tray).getByText(/继承上下文：是/)).toBeInTheDocument();
    // The card is not modal: the composer stays usable underneath it.
    expect(screen.getByLabelText("向 Agent 发送消息")).toBeEnabled();
    expect(runtimeMocks.runModel).not.toHaveBeenCalled();

    await user.click(within(tray).getByRole("button", { name: "批准" }));
    await waitFor(() => expect(runtimeMocks.resolveForkRequest).toHaveBeenCalledWith("fork-1", true));
    // The card comes down on the click; the host's answer is what creates the child.
    expect(screen.queryByRole("region", { name: "分叉请求" })).not.toBeInTheDocument();
    expect(runtimeMocks.runModel).not.toHaveBeenCalled();

    await act(async () => {
      emitAppPushEvent({
        type: "forkResolved",
        forkId: "fork-1",
        workspaceId: document.workspaces[0].id,
        sourceConversationId: source.id,
        approved: true,
        childConversationId: child.id
      });
    });

    // The child is loaded from the host, filed under its parent, and run once,
    // anchored at the prompt the host persisted as its last user message.
    await waitFor(() => expect(runtimeMocks.loadConversationRemote).toHaveBeenCalledWith(child.id));
    await waitFor(() => expect(runtimeMocks.runModel).toHaveBeenCalledTimes(1));
    const request = runtimeMocks.runModel.mock.calls[0][0];
    expect(request.conversationId).toBe(child.id);
    expect(request.forkPromptContextId).toBe("ctx_fork_prompt");
    expect(request.contexts.map((context: { id: string }) => context.id))
      .toEqual(["copied-u1", "copied-a1", "ctx_fork_prompt"]);
    // No renderer-side create command: the host already owns the child.
    expect(runtimeMocks.createConversationRemote).not.toHaveBeenCalled();

    const sidebar = within(window.document.querySelector(".workspace-list") as HTMLElement);
    const parentRow = sidebar.getByText(source.title).closest(".conversation-row") as HTMLElement;
    expect(within(parentRow).getByRole("button", { name: "收起子会话" })).toBeInTheDocument();
    expect(sidebar.getByText("把测试也跑一遍").closest(".conversation-row")).toHaveClass("conversation-row--nested");
    // The user is not yanked into the child; the source stays active.
    expect(sidebar.getByText(source.title).closest(".conversation-row")).toHaveClass("conversation-row--active");

    // Opening the child shows the run's reply.
    await user.click(sidebar.getByText("把测试也跑一遍"));
    expect(await screen.findByText("子会话已回答")).toBeInTheDocument();
  });

  it("takes a denied card down without loading or running anything", async () => {
    const document = documentWithModel();
    runtimeMocks.loadDocument.mockResolvedValue(document);
    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");

    await act(async () => {
      emitAppPushEvent({ type: "forkRequested", ...forkRequest(document, { forkId: "fork-deny" }) });
    });
    const tray = await screen.findByRole("region", { name: "分叉请求" });
    await user.click(within(tray).getByRole("button", { name: "拒绝" }));
    await waitFor(() => expect(runtimeMocks.resolveForkRequest).toHaveBeenCalledWith("fork-deny", false));
    await act(async () => {
      emitAppPushEvent({
        type: "forkResolved",
        forkId: "fork-deny",
        workspaceId: document.workspaces[0].id,
        sourceConversationId: document.workspaces[0].conversations[0].id,
        approved: false,
        childConversationId: null
      });
    });
    expect(screen.queryByRole("region", { name: "分叉请求" })).not.toBeInTheDocument();
    expect(runtimeMocks.loadConversationRemote).not.toHaveBeenCalled();
    expect(runtimeMocks.runModel).not.toHaveBeenCalled();
  });

  it("re-lists open fork requests after a reload and ignores a duplicate push of the same card", async () => {
    const document = documentWithModel();
    runtimeMocks.loadDocument.mockResolvedValue(document);
    const request = forkRequest(document, { forkId: "fork-relist", prompt: "重载后仍在" });
    runtimeMocks.listPendingForkRequests.mockResolvedValue([request]);

    render(<App />);
    const tray = await screen.findByRole("region", { name: "分叉请求" });
    expect(within(tray).getByText("重载后仍在")).toBeInTheDocument();

    await act(async () => {
      emitAppPushEvent({ type: "forkRequested", ...request });
    });
    expect(within(tray).getAllByText("重载后仍在")).toHaveLength(1);
  });

  it("starts an auto-approved fork's run from the resolved event alone", async () => {
    const document = documentWithModel();
    const source = document.workspaces[0].conversations[0];
    runtimeMocks.loadDocument.mockResolvedValue(document);
    const child = forkedChild(document, "完全访问下自动分叉");
    runtimeMocks.loadConversationRemote.mockResolvedValue(child);
    runtimeMocks.runModel.mockResolvedValue({
      contexts: [],
      usage: {},
      model: model.id,
      providerName: "Test Provider",
      durationMs: 5
    });

    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");
    // Full access never showed a card; the only signal is the resolution.
    await act(async () => {
      emitAppPushEvent({
        type: "forkResolved",
        forkId: "fork-auto",
        workspaceId: document.workspaces[0].id,
        sourceConversationId: source.id,
        approved: true,
        childConversationId: child.id
      });
    });
    await waitFor(() => expect(runtimeMocks.runModel).toHaveBeenCalledTimes(1));
    expect(runtimeMocks.runModel.mock.calls[0][0].conversationId).toBe(child.id);
    expect(screen.queryByRole("region", { name: "分叉请求" })).not.toBeInTheDocument();
  });

  it("recovers a delivered fork intent after reload before run adoption without replaying the event", async () => {
    const document = documentWithModel();
    const child = forkedChild(document, "重载窗口的首轮");
    runtimeMocks.loadDocument.mockResolvedValue(document);
    runtimeMocks.loadConversationRemote.mockResolvedValue(child);
    let release!: (rows: never[]) => void;
    runtimeMocks.listResumableRuns.mockImplementationOnce(() => new Promise((resolve) => { release = resolve; }));
    runtimeMocks.listPendingForkStarts.mockResolvedValue([]);
    const old = render(<App />);
    await waitFor(() => expect(runtimeMocks.listResumableRuns).toHaveBeenCalledTimes(1));
    await act(async () => {
      emitAppPushEvent({ type: "forkResolved", forkId: "lost-edge", workspaceId: document.workspaces[0].id,
        sourceConversationId: document.workspaces[0].conversations[0].id, approved: true, childConversationId: child.id });
    });
    expect(runtimeMocks.runModel).not.toHaveBeenCalled();
    old.unmount();
    document.workspaces[0].conversations.push(child);
    runtimeMocks.listPendingForkStarts.mockResolvedValue([{ workspaceId: document.workspaces[0].id,
      conversationId: child.id, promptContextId: "ctx_fork_prompt" }]);
    runtimeMocks.runModel.mockResolvedValue({ contexts: [], usage: {}, model: model.id, providerName: "Test Provider", durationMs: 5 });
    render(<App />);
    await waitFor(() => expect(runtimeMocks.runModel).toHaveBeenCalledTimes(1));
    expect(runtimeMocks.runModel.mock.calls[0][0].contexts.filter((item: { id: string }) => item.id === "ctx_fork_prompt")).toHaveLength(1);
    expect(runtimeMocks.createConversationRemote).not.toHaveBeenCalled();
    await act(async () => release([]));
  });

  it("deduplicates the durable list and a live event and does not restart a consumed intent", async () => {
    const document = documentWithModel();
    const child = forkedChild(document, "只运行一次");
    document.workspaces[0].conversations.push(child);
    runtimeMocks.loadDocument.mockResolvedValue(document);
    runtimeMocks.loadConversationRemote.mockResolvedValue(child);
    runtimeMocks.listPendingForkStarts.mockResolvedValue([{ workspaceId: document.workspaces[0].id,
      conversationId: child.id, promptContextId: "ctx_fork_prompt" }]);
    runtimeMocks.runModel.mockResolvedValue({ contexts: [], usage: {}, model: model.id, providerName: "Test Provider", durationMs: 5 });
    const mounted = render(<App />);
    await waitFor(() => expect(runtimeMocks.runModel).toHaveBeenCalledTimes(1));
    await act(async () => {
      emitAppPushEvent({ type: "forkResolved", forkId: "duplicate", workspaceId: document.workspaces[0].id,
        sourceConversationId: document.workspaces[0].conversations[0].id, approved: true, childConversationId: child.id });
    });
    expect(runtimeMocks.runModel).toHaveBeenCalledTimes(1);
    expect(runtimeMocks.loadConversationRemote).toHaveBeenCalledTimes(1);
    mounted.unmount();
    runtimeMocks.listPendingForkStarts.mockResolvedValue([]);
    render(<App />);
    await waitFor(() => expect(runtimeMocks.listPendingForkStarts).toHaveBeenCalledTimes(2));
    expect(runtimeMocks.runModel).toHaveBeenCalledTimes(1);
  });

  it("keeps a failed child load visible and retries only on user intent", async () => {
    const document = documentWithModel();
    const child = forkedChild(document, "重试首轮");
    runtimeMocks.loadDocument.mockResolvedValue(document);
    runtimeMocks.loadConversationRemote.mockRejectedValueOnce(new Error("injected load failure")).mockResolvedValue(child);
    runtimeMocks.listPendingForkStarts.mockResolvedValue([{ workspaceId: document.workspaces[0].id,
      conversationId: child.id, promptContextId: "ctx_fork_prompt" }]);
    runtimeMocks.runModel.mockResolvedValue({ contexts: [], usage: {}, model: model.id, providerName: "Test Provider", durationMs: 5 });
    const user = userEvent.setup();
    render(<App />);
    expect(await screen.findByText("injected load failure")).toBeInTheDocument();
    expect(runtimeMocks.runModel).not.toHaveBeenCalled();
    expect(runtimeMocks.loadConversationRemote).toHaveBeenCalledTimes(1);
    await user.click(screen.getByRole("button", { name: "重试分叉首轮" }));
    await waitFor(() => expect(runtimeMocks.runModel).toHaveBeenCalledTimes(1));
    expect(runtimeMocks.createConversationRemote).not.toHaveBeenCalled();
  });

  it("re-parents a deleted conversation's children to its own parent", async () => {
    const document = documentWithModel();
    const root = document.workspaces[0].conversations[0];
    const middle: Conversation = {
      ...root,
      id: "conv_middle",
      title: "中间层",
      contexts: [],
      parentConversationId: root.id
    };
    const leaf: Conversation = {
      ...root,
      id: "conv_leaf",
      title: "叶子",
      contexts: [],
      parentConversationId: middle.id
    };
    document.workspaces[0].conversations = [root, middle, leaf];
    runtimeMocks.loadDocument.mockResolvedValue(document);

    const user = userEvent.setup();
    render(<App />);
    const sidebar = within(await screen.findByRole("navigation") as HTMLElement);
    const middleRow = sidebar.getByText("中间层").closest(".conversation-row") as HTMLElement;
    expect(sidebar.getByText("叶子").closest(".conversation-list--nested")).not.toBeNull();

    // Two-click delete: arm, then confirm.
    await user.click(within(middleRow).getByRole("button", { name: "删除 中间层" }));
    await user.click(within(middleRow).getByRole("button", { name: "确认删除 中间层" }));

    await waitFor(() => expect(sidebar.queryByText("中间层")).not.toBeInTheDocument());
    // The leaf now hangs directly under the root instead of vanishing with the
    // middle layer.
    const rootRow = sidebar.getByText(root.title).closest(".conversation-row") as HTMLElement;
    expect(within(rootRow).getByRole("button", { name: "收起子会话" })).toBeInTheDocument();
    expect(sidebar.getByText("叶子").closest(".conversation-row")).toHaveClass("conversation-row--nested");
    await waitFor(() => expect(runtimeMocks.saveDocument.mock.calls.some(([snapshot]) => (
      (snapshot as AppDocument).workspaces[0].conversations
        .find((candidate) => candidate.id === leaf.id)?.parentConversationId === root.id
    ))).toBe(true));
  });
});
