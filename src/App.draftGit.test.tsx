import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import App from "./App";
import { configureI18n } from "./i18n";
import type { GitTarget, GitWorkspaceSnapshot } from "./lib/git";
import { gitConversationTarget } from "./lib/git";
import { documentWithModel, expandGitStatus, gitMocks, resetAppMocks, runtimeMocks } from "./test/appMocks";

/**
 * Staging one file now lives in that file's `⋮` in the review pane's file column.
 * The column's menu is the one driven here: it is present whether or not the
 * file's diff card is open.
 */
async function openReviewFileMenu(user: ReturnType<typeof userEvent.setup>, path: string): Promise<void> {
  const list = await screen.findByRole("navigation", { name: "变更文件" });
  const name = path.slice(path.lastIndexOf("/") + 1);
  await user.click(within(list).getByRole("button", { name: `${name} 的操作` }));
}

async function clickReviewFileAction(
  user: ReturnType<typeof userEvent.setup>,
  path: string,
  label: string
): Promise<void> {
  await openReviewFileMenu(user, path);
  await user.click(await screen.findByRole("menuitem", { name: new RegExp(`^${label}`) }));
}


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
 * Workspace-backed drafts have host conversation rows, so Git reads and writes
 * use their persisted conversation IDs even before the first message.
 */
describe("draft conversation Git surface", () => {
  beforeEach(() => {
    resetAppMocks();
    // Git controls require a host runtime.
    Object.defineProperty(window, "__TAURI_INTERNALS__", { configurable: true, value: {} });
    runtimeMocks.loadDocument.mockResolvedValue(documentWithModel());
    gitMocks.getGitWorkspaceSummary.mockResolvedValue(summaryResult(snapshot("main")));
  });

  it("asks Git about the persisted draft in its selected workspace", async () => {
    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");

    await user.click(screen.getByRole("button", { name: "新建任务" }));

    // The existing unsent slot is reused in the inherited workspace.
    await waitFor(() => expect(gitMocks.getGitWorkspaceSummary).toHaveBeenCalledWith(
      gitConversationTarget("conv_agent_gui"),
      undefined
    ));
    expect(await screen.findByRole("complementary", { name: "Git 状态" })).toBeInTheDocument();
    expect(await screen.findByRole("button", { name: "分支：main" })).toBeInTheDocument();
    expect(screen.getByRole("checkbox", { name: /工作树/ })).not.toBeChecked();
  });

  it("stages a draft change through its conversation target", async () => {
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
    await clickReviewFileAction(user, "src/App.tsx", "暂存");

    // Reads and writes address the same persisted unsent conversation.
    await waitFor(() => expect(gitMocks.executeGitAction).toHaveBeenCalledWith(
      gitConversationTarget("conv_agent_gui"),
      { type: "stage", paths: ["src/App.tsx"] }
    ));
    expect(gitMocks.getGitChangePage).toHaveBeenCalledWith(
      gitConversationTarget("conv_agent_gui"),
      expect.anything()
    );
  });

  it("has no Git surface when the draft has not chosen a workspace", async () => {
    const document = documentWithModel();
    document.workspaces.forEach((workspace) => { workspace.conversations = []; });
    runtimeMocks.loadDocument.mockResolvedValue(document);

    render(<App />);
    // A draft in the temporary workspace has no repository coordinate.
    await screen.findByRole("button", { name: "项目：选择项目" });
    await waitFor(() => expect(runtimeMocks.loadDocument).toHaveBeenCalled());
    expect(gitMocks.getGitWorkspaceSummary).not.toHaveBeenCalled();
    expect(screen.queryByRole("complementary", { name: "Git 状态" })).not.toBeInTheDocument();
  });

  it("keeps the status card across the persisted draft’s first send", async () => {
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

    // Sending preserves the persisted conversation and its checkout snapshot.
    await user.type(screen.getByLabelText("向 Agent 发送消息"), "第一句话");
    await user.click(screen.getByRole("button", { name: "发送" }));

    await waitFor(() => expect(runtimeMocks.runModel).toHaveBeenCalledTimes(1));
    expect(screen.getByRole("button", { name: "分支：main" })).toBeInTheDocument();
  });

  it("creates the persisted draft worktree immediately before its first send", async () => {
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
    // A workspace-backed draft already has the host row needed for creation.
    await waitFor(() => expect(gitMocks.createConversationWorktree).toHaveBeenCalledExactlyOnceWith("conv_agent_gui"));
    await waitFor(() => expect(screen.getByRole("checkbox", { name: /工作树/ })).toBeChecked());
    expect(runtimeMocks.runModel).not.toHaveBeenCalled();

    await user.type(screen.getByLabelText("向 Agent 发送消息"), "在隔离检出上开始");
    await user.click(screen.getByRole("button", { name: "发送" }));

    // Create the worktree before the first message so every tool call in that
    // turn uses the isolated checkout.
    await waitFor(() => expect(gitMocks.createConversationWorktree).toHaveBeenCalledTimes(1));
    expect(gitMocks.createConversationWorktree.mock.calls[0][0]).not.toBe("__draft__");
    await waitFor(() => expect(runtimeMocks.runModel).toHaveBeenCalledTimes(1));
    expect(gitMocks.createConversationWorktree.mock.invocationCallOrder[0])
      .toBeLessThan(runtimeMocks.runModel.mock.invocationCallOrder[0]);
    // Persist the workspace-backed draft before creating its worktree because the
    // host resolves conversations from its saved document.
    expect(runtimeMocks.saveDocument).toHaveBeenCalled();
    expect(runtimeMocks.saveDocument.mock.invocationCallOrder[0])
      .toBeLessThan(gitMocks.createConversationWorktree.mock.invocationCallOrder[0]);
    // The persisted draft's initial checkout read has no cached revision.
    await waitFor(() => expect(gitMocks.getGitWorkspaceSummary).toHaveBeenCalledWith(
      gitConversationTarget("conv_agent_gui"),
      undefined
    ));
    const firstConversationRead = gitMocks.getGitWorkspaceSummary.mock.calls.find(
      (call) => (call[0] as GitTarget).kind === "conversation"
    );
    expect(firstConversationRead?.[1]).toBeUndefined();

  });

  it("reports immediate worktree creation failure without enabling isolation", async () => {
    const user = userEvent.setup();
    gitMocks.createConversationWorktree.mockRejectedValue(new Error("仓库还没有任何提交"));
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");
    await user.click(screen.getByRole("button", { name: "新建任务" }));
    await user.click(await screen.findByRole("checkbox", { name: /工作树/ }));

    // Failure belongs to the checkbox operation, not a deferred send.
    // No message has been submitted and isolation must remain disabled.
    await waitFor(() => expect(gitMocks.createConversationWorktree).toHaveBeenCalled());
    expect(runtimeMocks.runModel).not.toHaveBeenCalled();
    const alert = await screen.findByRole("alert");
    expect(screen.getByRole("checkbox", { name: /工作树/ })).not.toBeChecked();
    expect(alert).toHaveTextContent("仓库还没有任何提交");
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
      target.kind === "conversation" && target.conversationId === "conv_agent_gui"
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
      expect.objectContaining({ kind: "conversation", conversationId: expect.not.stringMatching(`^${peerConversationId}$`) }),
      undefined
    ));
    await expandGitStatus(user);
    await user.click(await screen.findByRole("button", { name: /变更.*新增 1 行/ }));
    const draftTarget = gitMocks.getGitWorkspaceSummary.mock.calls.map(([target]) => target)
      .find((target) => target.kind === "conversation" && target.conversationId !== peerConversationId);
    expect(draftTarget).toBeDefined();
    await openReviewFileMenu(user, "src/App.tsx");
    const stage = await screen.findByRole("menuitem", { name: /^暂存/ });
    return { stage, draftTarget };
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
    const { stage, draftTarget } = await runPeerThenOpenDraft(user, peer.id);

    expect(stage).toBeEnabled();
    await user.click(stage);

    await waitFor(() => expect(gitMocks.executeGitAction).toHaveBeenCalledWith(
      draftTarget,
      { type: "stage", paths: ["src/App.tsx"] }
    ));
    expect(screen.queryByText(
      "同一项目中的另一项任务正在运行，暂不能执行 Git 写操作"
    )).not.toBeInTheDocument();
  });

  it("refuses a draft Git write while a peer runs in the workspace root", async () => {
    runtimeMocks.loadDocument.mockResolvedValue(documentWithModel());
    // A conversation without a worktree answers from the root the draft writes to.
    mockPeerCheckout(snapshot("main"));

    const user = userEvent.setup();
    const { stage } = await runPeerThenOpenDraft(user, "conv_agent_gui");

    expect(stage).toBeDisabled();
    await user.click(stage);

    expect(await screen.findByText(
      "同一项目中的另一项任务正在运行，暂不能执行 Git 写操作"
    )).toBeInTheDocument();
    expect(gitMocks.executeGitAction).not.toHaveBeenCalled();
  });
});
