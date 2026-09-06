import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import App from "./App";
import { configureI18n } from "./i18n";
import type { GitTarget, GitWorkspaceSnapshot } from "./lib/git";
import { gitConversationTarget, gitWorkspaceTarget } from "./lib/git";
import { documentWithModel, expandGitStatus, gitMocks, resetAppMocks, runtimeMocks } from "./test/appMocks";

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

function snapshot(branch: string): GitWorkspaceSnapshot {
  return {
    repositoryId: "repository-id-workspace",
    worktreeId: "worktree-id-workspace",
    repositoryRoot: "C:/workspace",
    worktreeRoot: "C:/workspace",
    branch,
    head: "head-oid",
    contentRevision: `revision-${branch}`,
    upstream: null,
    ahead: 0,
    behind: 0,
    additions: 0,
    deletions: 0,
    staged: 0,
    unstaged: 0,
    untracked: 0,
    conflicted: 0,
    stash: 0,
    files: [],
    remote: null,
    remotes: [],
    gitVersion: "git version 2.50.0",
    detached: false,
    unborn: false,
    operation: null,
    operationRevision: null,
    isClean: true,
    binaryFiles: 0,
    warnings: []
  } as unknown as GitWorkspaceSnapshot;
}

function summaryResult(value: GitWorkspaceSnapshot) {
  const { files, ...summary } = value;
  return {
    kind: "snapshot" as const,
    summary: {
      ...summary,
      summaryRevision: `summary-${value.contentRevision}`,
      changedFiles: 0,
      stageable: 0,
      unstageable: 0
    }
  };
}

/**
 * Git behavior for draft conversations. Drafts have no host conversation row, so
 * Git requests target their persisted workspace rather than a conversation ID.
 */
describe("draft conversation Git surface", () => {
  beforeEach(() => {
    resetAppMocks();
    // Git controls require a host runtime.
    Object.defineProperty(window, "__TAURI_INTERNALS__", { configurable: true, value: {} });
    runtimeMocks.loadDocument.mockResolvedValue(documentWithModel());
    gitMocks.getGitWorkspaceSummary.mockResolvedValue(summaryResult(snapshot("main")));
  });

  it("asks Git about the workspace the draft has selected", async () => {
    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");

    await user.click(screen.getByRole("button", { name: "新建任务" }));

    // Drafts inherit the prior workspace, so Git queries its root rather than an
    // unpersisted conversation ID.
    await waitFor(() => expect(gitMocks.getGitWorkspaceSummary).toHaveBeenCalledWith(
      gitWorkspaceTarget("ws_mework"),
      undefined
    ));
    expect(await screen.findByRole("complementary", { name: "Git 状态" })).toBeInTheDocument();
    expect(await screen.findByRole("button", { name: "分支：main" })).toBeInTheDocument();
    expect(screen.getByRole("checkbox", { name: /工作树/ })).not.toBeChecked();
  });

  it("stages a draft change through its workspace target", async () => {
    const dirtySnapshot: GitWorkspaceSnapshot = {
      ...snapshot("main"),
      additions: 1,
      unstaged: 1,
      files: [{
        path: "src/App.tsx",
        status: "modified",
        staged: false,
        unstaged: true,
        additions: 1,
        deletions: 0
      }],
      isClean: false
    };
    gitMocks.getGitWorkspaceSummary.mockResolvedValue({
      kind: "snapshot",
      summary: {
        ...summaryResult(dirtySnapshot).summary,
        changedFiles: 1,
        stageable: 1
      }
    });
    gitMocks.getGitChangePage.mockImplementation((_target, request) => Promise.resolve({
      kind: "page",
      revision: request.expectedRevision,
      files: dirtySnapshot.files,
      matchedCount: dirtySnapshot.files.length,
      nextCursor: null,
      selection: null
    }));
    gitMocks.getGitDiff.mockResolvedValue({
      patch: "@@ -0,0 +1 @@\n+change",
      path: "src/App.tsx",
      additions: 1,
      deletions: 0,
      binary: false,
      truncated: false,
      files: []
    });
    gitMocks.executeGitAction.mockResolvedValue({ snapshot: dirtySnapshot });

    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");
    await user.click(screen.getByRole("button", { name: "新建任务" }));

    await expandGitStatus(user);
    await user.click(await screen.findByRole("button", { name: /变更.*新增 1 行/ }));
    await user.click(await screen.findByRole("button", { name: "暂存 src/App.tsx" }));

    // Read and write requests must retain the workspace coordinate; a draft
    // conversation cannot be downgraded to an unpersisted conversation target.
    await waitFor(() => expect(gitMocks.executeGitAction).toHaveBeenCalledWith(
      gitWorkspaceTarget("ws_mework"),
      { type: "stage", paths: ["src/App.tsx"] }
    ));
    expect(gitMocks.getGitChangePage).toHaveBeenCalledWith(
      gitWorkspaceTarget("ws_mework"),
      expect.anything()
    );
  });

  it("has no Git surface when the draft has not chosen a workspace", async () => {
    const document = documentWithModel();
    document.workspaces.forEach((workspace) => { workspace.conversations = []; });
    runtimeMocks.loadDocument.mockResolvedValue(document);

    render(<App />);
    // A draft in the temporary workspace has no repository coordinate.
    await screen.findByRole("button", { name: "工作区：选择工作区" });
    await waitFor(() => expect(runtimeMocks.loadDocument).toHaveBeenCalled());
    expect(gitMocks.getGitWorkspaceSummary).not.toHaveBeenCalled();
    expect(screen.queryByRole("complementary", { name: "Git 状态" })).not.toBeInTheDocument();
  });

  it("keeps the status card across the send that redeems the draft", async () => {
    const user = userEvent.setup();
    runtimeMocks.runModel.mockResolvedValue({
      contexts: [],
      usage: {},
      model: "test-model",
      providerName: "",
      durationMs: 1
    });
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");
    await user.click(screen.getByRole("button", { name: "新建任务" }));
    await screen.findByRole("complementary", { name: "Git 状态" });

    // Redemption changes the key, not the checkout; the snapshot moves without
    // making the status card flash away.
    await user.type(screen.getByLabelText("向 Agent 发送消息"), "第一句话");
    await user.click(screen.getByRole("button", { name: "发送" }));

    await waitFor(() => expect(runtimeMocks.runModel).toHaveBeenCalledTimes(1));
    expect(screen.getByRole("button", { name: "分支：main" })).toBeInTheDocument();
  });

  it("records the worktree checkbox as intent and creates it on the first send", async () => {
    const user = userEvent.setup();
    runtimeMocks.runModel.mockResolvedValue({
      contexts: [],
      usage: {},
      model: "test-model",
      providerName: "",
      durationMs: 1
    });
    gitMocks.createConversationWorktree.mockResolvedValue({
      path: "C:/workspace/.mework/worktrees/conversations/conv-new",
      branch: "mework/conv-new",
      baseOid: "head-oid"
    });
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");
    await user.click(screen.getByRole("button", { name: "新建任务" }));

    await user.click(await screen.findByRole("checkbox", { name: /工作树/ }));
    // The checkbox is only intent on a draft: the host needs a conversation ID
    // to create the worktree, and no row exists yet.
    expect(screen.getByRole("checkbox", { name: /工作树/ })).toBeChecked();
    expect(gitMocks.createConversationWorktree).not.toHaveBeenCalled();

    await user.type(screen.getByLabelText("向 Agent 发送消息"), "在隔离检出上开始");
    await user.click(screen.getByRole("button", { name: "发送" }));

    // Create the worktree before the first message so every tool call in that
    // turn uses the isolated checkout.
    await waitFor(() => expect(gitMocks.createConversationWorktree).toHaveBeenCalledTimes(1));
    expect(gitMocks.createConversationWorktree.mock.calls[0][0]).not.toBe("__draft__");
    await waitFor(() => expect(runtimeMocks.runModel).toHaveBeenCalledTimes(1));
    expect(gitMocks.createConversationWorktree.mock.invocationCallOrder[0])
      .toBeLessThan(runtimeMocks.runModel.mock.invocationCallOrder[0]);
    // Persist the redeemed conversation before creating its worktree because the
    // host resolves conversations from its saved document.
    expect(runtimeMocks.saveDocument).toHaveBeenCalled();
    expect(runtimeMocks.saveDocument.mock.invocationCallOrder[0])
      .toBeLessThan(gitMocks.createConversationWorktree.mock.invocationCallOrder[0]);
    // A workspace-root snapshot belongs to a different checkout. The new
    // conversation's first summary must not carry its known revision.
    await waitFor(() => expect(gitMocks.getGitWorkspaceSummary).toHaveBeenCalledWith(
      expect.objectContaining({ kind: "conversation" }),
      undefined
    ));
    const firstConversationRead = gitMocks.getGitWorkspaceSummary.mock.calls.find(
      (call) => (call[0] as GitTarget).kind === "conversation"
    );
    expect(firstConversationRead?.[1]).toBeUndefined();
  });

  it("does not send when the requested worktree cannot be created", async () => {
    const user = userEvent.setup();
    gitMocks.createConversationWorktree.mockRejectedValue(new Error("仓库还没有任何提交"));
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");
    await user.click(screen.getByRole("button", { name: "新建任务" }));
    await user.click(await screen.findByRole("checkbox", { name: /工作树/ }));

    await user.type(screen.getByLabelText("向 Agent 发送消息"), "在隔离检出上开始");
    await user.click(screen.getByRole("button", { name: "发送" }));

    // A requested worktree must not silently fall back to the workspace root.
    await waitFor(() => expect(gitMocks.createConversationWorktree).toHaveBeenCalled());
    expect(runtimeMocks.runModel).not.toHaveBeenCalled();
    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("消息还没有发出");
    expect(alert).toHaveTextContent("仓库还没有任何提交");
  });

  it("drops a worktree intent the draft can no longer honour", async () => {
    const user = userEvent.setup();
    runtimeMocks.runModel.mockResolvedValue({
      contexts: [],
      usage: {},
      model: "test-model",
      providerName: "",
      durationMs: 1
    });
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");
    await user.click(screen.getByRole("button", { name: "新建任务" }));
    await user.click(await screen.findByRole("checkbox", { name: /工作树/ }));

    // A temporary workspace cannot create a conversation worktree.
    await user.click(screen.getByRole("button", { name: /^工作区：/ }));
    await user.click(await screen.findByRole("menuitemradio", { name: "临时工作区" }));
    await waitFor(() => expect(
      screen.queryByRole("complementary", { name: "Git 状态" })
    ).not.toBeInTheDocument());

    await user.type(screen.getByLabelText("向 Agent 发送消息"), "换到临时工作区");
    await user.click(screen.getByRole("button", { name: "发送" }));

    // Drop an intent that can no longer be fulfilled so it cannot block sending.
    await waitFor(() => expect(runtimeMocks.runModel).toHaveBeenCalledTimes(1));
    expect(gitMocks.createConversationWorktree).not.toHaveBeenCalled();
  });

  it("addresses a persisted conversation by conversation, not by workspace", async () => {
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");

    // Persisted conversations retain their own target so registered worktrees
    // are included instead of using the workspace root.
    await waitFor(() => expect(gitMocks.getGitWorkspaceSummary).toHaveBeenCalledWith(
      gitConversationTarget("conv_agent_gui"),
      undefined
    ));
  });

  /**
   * A peer's model run only blocks the draft's Git writes while the two may share
   * a checkout. The renderer decides that from the identities the host returned,
   * which it learns for a conversation while that conversation is on screen.
   */
  const dirtyRootSnapshot = (): GitWorkspaceSnapshot => ({
    ...snapshot("main"),
    additions: 1,
    unstaged: 1,
    files: [{
      path: "src/App.tsx",
      status: "modified",
      staged: false,
      unstaged: true,
      additions: 1,
      deletions: 0
    }],
    isClean: false
  });

  /** Answers the workspace root with a stageable change and the peer with its own checkout. */
  function mockPeerCheckout(peerSnapshot: GitWorkspaceSnapshot) {
    const dirty = dirtyRootSnapshot();
    gitMocks.getGitWorkspaceSummary.mockImplementation((target: GitTarget) => Promise.resolve(
      target.kind === "conversation"
        ? summaryResult(peerSnapshot)
        : {
          kind: "snapshot" as const,
          summary: {
            ...summaryResult(dirty).summary,
            changedFiles: 1,
            stageable: 1
          }
        }
    ));
    gitMocks.getGitChangePage.mockImplementation((
      _target: GitTarget,
      request: { expectedRevision: string }
    ) => Promise.resolve({
      kind: "page",
      revision: request.expectedRevision,
      files: dirty.files,
      matchedCount: dirty.files.length,
      nextCursor: null,
      selection: null
    }));
    gitMocks.getGitDiff.mockResolvedValue({
      patch: "@@ -0,0 +1 @@\n+change",
      path: "src/App.tsx",
      additions: 1,
      deletions: 0,
      binary: false,
      truncated: false,
      files: []
    });
    gitMocks.executeGitAction.mockResolvedValue({ snapshot: dirty });
  }

  /** Leaves the workspace conversation running, then opens a draft on the same workspace. */
  async function runPeerThenOpenDraft(
    user: ReturnType<typeof userEvent.setup>,
    peerConversationId: string
  ) {
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");
    // The peer's checkout identity has to reach the renderer before it leaves the screen.
    await waitFor(() => expect(gitMocks.getGitWorkspaceSummary).toHaveBeenCalledWith(
      gitConversationTarget(peerConversationId),
      undefined
    ));
    runtimeMocks.runModel.mockImplementation(() => new Promise(() => undefined));

    await user.type(screen.getByLabelText("向 Agent 发送消息"), "开始一段长时间的工作");
    await user.click(screen.getByRole("button", { name: "发送" }));
    await waitFor(() => expect(runtimeMocks.runModel).toHaveBeenCalledTimes(1));

    await user.click(screen.getByRole("button", { name: "新建任务" }));
    await waitFor(() => expect(gitMocks.getGitWorkspaceSummary).toHaveBeenCalledWith(
      gitWorkspaceTarget("ws_mework"),
      undefined
    ));
    await expandGitStatus(user);
    await user.click(await screen.findByRole("button", { name: /变更.*新增 1 行/ }));
    return screen.findByRole("button", { name: "暂存 src/App.tsx" });
  }

  it("stages from the draft while a peer runs in its own worktree", async () => {
    const appDocument = documentWithModel();
    const workspace = appDocument.workspaces[0];
    const peer = {
      ...workspace.conversations[0],
      worktree: {
        path: "C:/workspace/.mework/worktrees/conversations/peer",
        branch: "mework/peer",
        baseOid: "head-oid"
      }
    };
    workspace.conversations = [peer];
    runtimeMocks.loadDocument.mockResolvedValue(appDocument);
    // Same repository, its own worktree: an isolated checkout, not another repository.
    mockPeerCheckout({
      ...snapshot("mework/peer"),
      worktreeId: "worktree-id-peer",
      worktreeRoot: "C:/workspace/.mework/worktrees/conversations/peer"
    });

    const user = userEvent.setup();
    const stage = await runPeerThenOpenDraft(user, peer.id);

    expect(stage).toBeEnabled();
    await user.click(stage);

    await waitFor(() => expect(gitMocks.executeGitAction).toHaveBeenCalledWith(
      gitWorkspaceTarget("ws_mework"),
      { type: "stage", paths: ["src/App.tsx"] }
    ));
    expect(screen.queryByText(
      "同一工作区中的另一项任务正在运行，暂不能执行 Git 写操作"
    )).not.toBeInTheDocument();
  });

  it("refuses a draft Git write while a peer runs in the workspace root", async () => {
    runtimeMocks.loadDocument.mockResolvedValue(documentWithModel());
    // A conversation without a worktree answers from the root the draft writes to.
    mockPeerCheckout(snapshot("main"));

    const user = userEvent.setup();
    const stage = await runPeerThenOpenDraft(user, "conv_agent_gui");

    expect(stage).toBeDisabled();
    await user.click(stage);

    expect(await screen.findByText(
      "同一工作区中的另一项任务正在运行，暂不能执行 Git 写操作"
    )).toBeInTheDocument();
    expect(gitMocks.executeGitAction).not.toHaveBeenCalled();
  });
});
