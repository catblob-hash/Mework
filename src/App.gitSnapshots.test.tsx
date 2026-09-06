import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import App, {
  gitReviewSnapshotCacheKey,
  gitSnapshotForWorkspace,
  gitSnapshotsAfterRefresh,
  gitSnapshotsAfterWorkspaceMutation
} from "./App";
import type { GitTarget, GitWorkspaceSnapshot } from "./lib/git";
import { gitConversationTarget } from "./lib/git";
import { configureI18n } from "./i18n";
import { resetAppMocks, documentWithModel, expandGitStatus, gitMocks, runtimeMocks } from "./test/appMocks";

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

describe("App model run flow — gitSnapshots", () => {
  beforeEach(resetAppMocks);

  it("does not reuse a Git snapshot after a task moves to another workspace", () => {
    const snapshot = { branch: "main" } as GitWorkspaceSnapshot;

    expect(gitSnapshotForWorkspace({ workspaceId: "workspace-a", snapshot }, "workspace-a"))
      .toBe(snapshot);
    expect(gitSnapshotForWorkspace({ workspaceId: "workspace-a", snapshot }, "workspace-b"))
      .toBeUndefined();
    expect(gitSnapshotForWorkspace({ workspaceId: "workspace-b", snapshot: null }, "workspace-b"))
      .toBeNull();
  });

  it("preserves Git snapshot identity for failed and unchanged summary refreshes", () => {
    const snapshot = { branch: "main", head: "abc123" } as GitWorkspaceSnapshot;
    const current = {
      "conversation-1": {
        workspaceId: "workspace-a",
        snapshot
      }
    };

    expect(gitSnapshotsAfterRefresh(
      current,
      "conversation-1",
      "workspace-a",
      { status: "failed" }
    )).toBe(current);
    expect(gitSnapshotsAfterRefresh(
      current,
      "conversation-1",
      "workspace-a",
      { status: "unchanged", revision: "summary-abc123" }
    )).toBe(current);

    const resolvedNonRepository = gitSnapshotsAfterRefresh(
      current,
      "conversation-1",
      "workspace-a",
      { status: "resolved", snapshot: null }
    );
    expect(resolvedNonRepository).not.toBe(current);
    expect(resolvedNonRepository["conversation-1"]).toEqual({
      workspaceId: "workspace-a",
      snapshot: null
    });
    expect(gitSnapshotsAfterRefresh(
      resolvedNonRepository,
      "conversation-1",
      "workspace-a",
      { status: "resolved", snapshot: null }
    )).toBe(resolvedNonRepository);
  });

  it("removes the Git task-card state when a bounded refresh reports notRepository", async () => {
    const appDocument = documentWithModel();
    const conversation = appDocument.workspaces[0].conversations[0];
    runtimeMocks.loadDocument.mockResolvedValue(appDocument);
    Object.defineProperty(window, "__TAURI_INTERNALS__", { configurable: true, value: {} });
    const summary = {
      repositoryId: "repository-id-workspace",
      worktreeId: "worktree-id-workspace",
      repositoryRoot: "C:/workspace",
      worktreeRoot: "C:/workspace",
      branch: "main",
      head: "abc123",
      contentRevision: "content-abc123",
      summaryRevision: "summary-abc123",
      upstream: "origin/main",
      upstreamTarget: {
        remoteName: "origin",
        remoteBranch: "main",
        mergeRef: "refs/heads/main",
        trackingRef: "refs/remotes/origin/main",
        trackingOid: "abc123abc123abc123abc123abc123abc123abc1",
        isLocal: false,
        remote: {
          name: "origin",
          fetchRevision: "origin-fetch-revision-1",
          pushRevision: "origin-push-revision-1",
          url: "https://github.com/example-org/Mework.git"
        }
      },
      ahead: 0,
      behind: 0,
      additions: 3,
      deletions: 1,
      staged: 0,
      unstaged: 1,
      untracked: 0,
      conflicted: 0,
      stash: 0,
      changedFiles: 1,
      stageable: 1,
      unstageable: 0,
      remote: {
        name: "origin",
        fetchRevision: "origin-fetch-revision-1",
        pushRevision: "origin-push-revision-1",
        url: "https://github.com/example-org/Mework.git"
      },
      remotes: [{
        name: "origin",
        fetchRevision: "origin-fetch-revision-1",
        pushRevision: "origin-push-revision-1",
        url: "https://github.com/example-org/Mework.git"
      }],
      gitVersion: "git version 2.50.0",
      detached: false,
      unborn: false,
      operation: null,
      operationRevision: null,
      isClean: false,
      binaryFiles: 0,
      warnings: []
    };
    gitMocks.getGitWorkspaceSummary
      .mockResolvedValueOnce({ kind: "snapshot", summary })
      .mockResolvedValue({ kind: "notRepository" });

    const user = userEvent.setup();
    render(<App />);
    const gitStatus = await expandGitStatus(user);
    expect(gitStatus).toHaveTextContent("main");
    expect(gitMocks.getGitWorkspaceSummary).toHaveBeenNthCalledWith(
      1,
      gitConversationTarget(conversation.id),
      undefined
    );

    await act(async () => {
      window.dispatchEvent(new Event("focus"));
    });

    // The card exists only to report a repository, so losing the repository
    // removes the card outright rather than emptying it.
    await waitFor(() => expect(
      screen.queryByRole("complementary", { name: "Git 状态" })
    ).not.toBeInTheDocument());
    expect(gitMocks.getGitWorkspaceSummary).toHaveBeenNthCalledWith(
      2,
      gitConversationTarget(conversation.id),
      summary.summaryRevision
    );
  });

  it("publishes a Git mutation snapshot to every task in the same workspace", () => {
    const previous = { branch: "main", head: "old" } as GitWorkspaceSnapshot;
    const next = { branch: "feature", head: "new" } as GitWorkspaceSnapshot;
    const current = {
      "conversation-a": { workspaceId: "workspace-a", snapshot: previous },
      "conversation-b": { workspaceId: "workspace-a", snapshot: previous },
      "conversation-c": { workspaceId: "workspace-b", snapshot: previous }
    };

    expect(gitSnapshotsAfterWorkspaceMutation(
      current,
      ["conversation-a", "conversation-b"],
      "workspace-a",
      next
    )).toEqual({
      "conversation-a": { workspaceId: "workspace-a", snapshot: next },
      "conversation-b": { workspaceId: "workspace-a", snapshot: next },
      "conversation-c": { workspaceId: "workspace-b", snapshot: previous }
    });
  });

  it("does not let task A's stale Git poll overwrite task B's workspace mutation snapshot", async () => {
    const appDocument = documentWithModel();
    const conversationA = {
      ...appDocument.workspaces[0].conversations[0],
      id: "conversation-git-a",
      title: "Git 任务 A"
    };
    const conversationB = {
      ...conversationA,
      id: "conversation-git-b",
      title: "Git 任务 B"
    };
    appDocument.workspaces[0].conversations = [conversationA, conversationB];
    runtimeMocks.loadDocument.mockResolvedValue(appDocument);
    Object.defineProperty(window, "__TAURI_INTERNALS__", { configurable: true, value: {} });

    const snapshot = (
      branch: string,
      head: string,
      contentRevision: string
    ): GitWorkspaceSnapshot => ({
      repositoryId: "repository-id-workspace",
      worktreeId: "worktree-id-workspace",
      repositoryRoot: "C:/workspace",
      worktreeRoot: "C:/workspace",
      branch,
      head,
      contentRevision,
      upstream: `origin/${branch}`,
      upstreamTarget: {
        remoteName: "origin",
        remoteBranch: branch,
        mergeRef: `refs/heads/${branch}`,
        trackingRef: `refs/remotes/origin/${branch}`,
        trackingOid: head,
        isLocal: false,
        remote: {
          name: "origin",
          fetchRevision: "origin-fetch-revision-1",
          pushRevision: "origin-push-revision-1",
          url: "https://github.com/example-org/Mework.git"
        }
      },
      ahead: 0,
      behind: 0,
      additions: 1,
      deletions: 0,
      staged: 0,
      unstaged: 1,
      untracked: 0,
      conflicted: 0,
      stash: 0,
      files: [{
        path: "src/App.tsx",
        status: "modified",
        staged: false,
        unstaged: true,
        additions: 1,
        deletions: 0
      }],
      remote: {
        name: "origin",
        fetchRevision: "origin-fetch-revision-1",
        pushRevision: "origin-push-revision-1",
        url: "https://github.com/example-org/Mework.git"
      },
      remotes: [{
        name: "origin",
        fetchRevision: "origin-fetch-revision-1",
        pushRevision: "origin-push-revision-1",
        url: "https://github.com/example-org/Mework.git"
      }],
      gitVersion: "git version 2.50.0",
      detached: false,
      unborn: false,
      operation: null,
      operationRevision: null,
      isClean: false,
      binaryFiles: 0,
      warnings: []
    });
    const staleSnapshot = snapshot("stale/a", "old-a", "revision-old-a");
    const initialSnapshot = snapshot("main", "initial", "revision-initial");
    const mutationSnapshot = snapshot("feature/new", "new-b", "revision-new-b");
    const summaryResult = (value: GitWorkspaceSnapshot) => {
      const { files, ...summary } = value;
      return {
        kind: "snapshot" as const,
        summary: {
          ...summary,
          summaryRevision: `summary-${value.contentRevision}`,
          changedFiles: files.length,
          stageable: files.filter((file) => file.unstaged || file.untracked).length,
          unstageable: files.filter((file) => file.staged).length
        }
      };
    };
    let resolveStalePoll!: (value: ReturnType<typeof summaryResult>) => void;
    const stalePoll = new Promise<ReturnType<typeof summaryResult>>((resolve) => {
      resolveStalePoll = resolve;
    });
    let conversationAPolls = 0;
    gitMocks.getGitWorkspaceSummary.mockImplementation((target: GitTarget) => {
      if (target.kind === "conversation" && target.conversationId === conversationA.id) {
        conversationAPolls += 1;
        return conversationAPolls === 1
          ? stalePoll
          : new Promise<ReturnType<typeof summaryResult>>(() => undefined);
      }
      return Promise.resolve(summaryResult(initialSnapshot));
    });
    gitMocks.getGitChangePage.mockImplementation((
      _target: GitTarget,
      request: { expectedRevision: string }
    ) => {
      return Promise.resolve({
        kind: "page",
        revision: request.expectedRevision,
        files: initialSnapshot.files,
        matchedCount: initialSnapshot.files.length,
        nextCursor: null,
        selection: null
      });
    });
    gitMocks.executeGitAction.mockResolvedValue({ snapshot: mutationSnapshot });
    gitMocks.getGitDiff.mockResolvedValue({
      patch: "@@ -0,0 +1 @@\n+change",
      path: "src/App.tsx",
      additions: 1,
      deletions: 0,
      binary: false,
      truncated: false,
      files: []
    });

    const user = userEvent.setup();
    render(<App />);
    await waitFor(() => expect(gitMocks.getGitWorkspaceSummary).toHaveBeenCalledWith(
      gitConversationTarget(conversationA.id),
      undefined
    ));

    const navigation = await screen.findByRole("complementary", { name: "工作区和对话" });
    await user.click(within(navigation).getByText(conversationB.title).closest("button")!);
    await waitFor(() => expect(gitMocks.getGitWorkspaceSummary).toHaveBeenCalledWith(
      gitConversationTarget(conversationB.id),
      undefined
    ));
    await expandGitStatus(user);
    await user.click(await screen.findByRole("button", { name: /变更.*新增 1 行/ }));
    await user.click(await screen.findByRole("button", { name: "暂存 src/App.tsx" }));
    await waitFor(() => expect(gitMocks.executeGitAction).toHaveBeenCalledWith(
      gitConversationTarget(conversationB.id),
      { type: "stage", paths: ["src/App.tsx"] }
    ));
    // The review page took over the message area, so the status card it was opened from is inside
    // a hidden subtree until we walk back.
    await user.click(screen.getByRole("button", { name: "返回对话" }));
    const taskStatusAfterMutation = screen.getByRole("complementary", { name: "Git 状态" });
    await waitFor(() => expect(taskStatusAfterMutation).toHaveTextContent("feature/new"));

    await act(async () => {
      resolveStalePoll(summaryResult(staleSnapshot));
      await stalePoll;
    });
    await user.click(within(navigation).getByText(conversationA.title).closest("button")!);
    const taskStatusAfterReturn = await expandGitStatus(user);

    // Scope assertions to the Git status card: the branch chip in the input row
    // also includes the branch name, so a global button query would match both.
    expect(await within(taskStatusAfterReturn).findByRole("button", { name: /feature\/new/ }))
      .toBeInTheDocument();
    expect(within(taskStatusAfterReturn).queryByRole("button", { name: /stale\/a/ }))
      .not.toBeInTheDocument();
  });

  it("invalidates the Git review cache when repository identity or HEAD changes", () => {
    const snapshot = {
      repositoryId: "repository-id-1",
      worktreeId: "worktree-id-1",
      repositoryRoot: "C:/repo",
      worktreeRoot: "C:/repo",
      branch: "main",
      head: "abc123",
      upstream: "origin/main",
      upstreamTarget: {
        remoteName: "origin",
        remoteBranch: "main",
        mergeRef: "refs/heads/main",
        trackingRef: "refs/remotes/origin/main",
        trackingOid: "abc123",
        isLocal: false,
        remote: {
          name: "origin",
          fetchRevision: "origin-fetch-revision-1",
          pushRevision: "origin-push-revision-1",
          url: "https://github.com/example/repo.git"
        }
      },
      remote: {
        name: "origin",
        fetchRevision: "origin-fetch-revision-1",
        pushRevision: "origin-push-revision-1",
        url: "https://github.com/example/repo.git"
      },
      remotes: [{
        name: "origin",
        fetchRevision: "origin-fetch-revision-1",
        pushRevision: "origin-push-revision-1",
        url: "https://github.com/example/repo.git"
      }],
      additions: 1
    } as GitWorkspaceSnapshot;
    const cacheKey = gitReviewSnapshotCacheKey(snapshot);

    expect(gitReviewSnapshotCacheKey({ ...snapshot, additions: 9 })).toBe(cacheKey);
    expect(gitReviewSnapshotCacheKey({ ...snapshot, branch: "feature" })).not.toBe(cacheKey);
    expect(gitReviewSnapshotCacheKey({ ...snapshot, head: "def456" })).not.toBe(cacheKey);
    expect(gitReviewSnapshotCacheKey({ ...snapshot, upstream: "origin/next" })).toBe(cacheKey);
    expect(gitReviewSnapshotCacheKey({
      ...snapshot,
      upstreamTarget: {
        ...snapshot.upstreamTarget!,
        trackingOid: "def456"
      }
    })).not.toBe(cacheKey);
    expect(gitReviewSnapshotCacheKey({ ...snapshot, worktreeRoot: "C:/repo-worktree" })).not.toBe(cacheKey);
    expect(gitReviewSnapshotCacheKey({ ...snapshot, repositoryId: "repository-id-2" })).not.toBe(cacheKey);
    expect(gitReviewSnapshotCacheKey({ ...snapshot, worktreeId: "worktree-id-2" })).not.toBe(cacheKey);
    expect(gitReviewSnapshotCacheKey({
      ...snapshot,
      remote: {
        ...snapshot.remote!,
        fetchRevision: "origin-fetch-revision-2",
        url: "https://github.com/example/other.git"
      },
      remotes: [{
        ...snapshot.remotes[0],
        fetchRevision: "origin-fetch-revision-2",
        url: "https://github.com/example/other.git"
      }]
    })).not.toBe(cacheKey);
  });
});

/**
 * A peer's model run blocks a Git write only while the two conversations may share
 * a checkout. The renderer follows the host's own rule, using the checkout identity
 * the host returned rather than the recorded worktree, which can fall back to the root.
 */
describe("App Git writes across checkouts", () => {
  beforeEach(() => {
    resetAppMocks();
    Object.defineProperty(window, "__TAURI_INTERNALS__", { configurable: true, value: {} });
  });

  const ISOLATED_ID = "conversation-isolated";
  const ROOT_ID = "conversation-root";
  const changedFile = {
    path: "src/App.tsx",
    status: "modified" as const,
    staged: false,
    unstaged: true,
    additions: 1,
    deletions: 0
  };

  const checkoutSnapshot = (
    worktreeId: string,
    worktreeRoot: string,
    branch: string,
    dirty: boolean
  ): GitWorkspaceSnapshot => ({
    // One repository throughout: two repositories would look distinct for the wrong reason.
    repositoryId: "repository-id-workspace",
    worktreeId,
    repositoryRoot: "C:/workspace",
    worktreeRoot,
    branch,
    head: `head-${worktreeId}`,
    contentRevision: `revision-${worktreeId}`,
    upstream: null,
    upstreamTarget: null,
    ahead: 0,
    behind: 0,
    additions: dirty ? 1 : 0,
    deletions: 0,
    staged: 0,
    unstaged: dirty ? 1 : 0,
    untracked: 0,
    conflicted: 0,
    stash: 0,
    files: dirty ? [changedFile] : [],
    remote: null,
    remotes: [],
    gitVersion: "git version 2.50.0",
    detached: false,
    unborn: false,
    operation: null,
    operationRevision: null,
    isClean: !dirty,
    binaryFiles: 0,
    warnings: []
  });

  const summaryOf = (value: GitWorkspaceSnapshot) => {
    const { files, ...summary } = value;
    return {
      kind: "snapshot" as const,
      summary: {
        ...summary,
        summaryRevision: `summary-${value.contentRevision}`,
        changedFiles: files.length,
        stageable: files.filter((file) => file.unstaged).length,
        unstageable: 0
      }
    };
  };

  /**
   * Two conversations of one workspace: the first owns a worktree and has a change
   * to stage, the second works in the workspace root. `rootCheckoutObserved` decides
   * whether the renderer ever gets an identity for the root peer.
   */
  async function stageButtonWithRunningRootPeer(
    user: ReturnType<typeof userEvent.setup>,
    rootCheckoutObserved: boolean
  ) {
    const appDocument = documentWithModel();
    const workspace = appDocument.workspaces[0];
    const template = workspace.conversations[0];
    workspace.conversations = [
      {
        ...template,
        id: ISOLATED_ID,
        title: "隔离检出任务",
        worktree: {
          path: "C:/workspace/.mework/worktrees/conversations/isolated",
          branch: "mework/isolated",
          baseOid: "head-oid"
        }
      },
      { ...template, id: ROOT_ID, title: "根检出任务", worktree: null }
    ];
    runtimeMocks.loadDocument.mockResolvedValue(appDocument);

    const isolatedSnapshot = checkoutSnapshot(
      "worktree-id-isolated",
      "C:/workspace/.mework/worktrees/conversations/isolated",
      "mework/isolated",
      true
    );
    const rootSnapshot = checkoutSnapshot(
      "worktree-id-workspace",
      "C:/workspace",
      "main",
      false
    );
    gitMocks.getGitWorkspaceSummary.mockImplementation((target: GitTarget) => {
      if (target.kind === "conversation" && target.conversationId === ROOT_ID) {
        return rootCheckoutObserved
          ? Promise.resolve(summaryOf(rootSnapshot))
          : new Promise<ReturnType<typeof summaryOf>>(() => undefined);
      }
      return Promise.resolve(summaryOf(isolatedSnapshot));
    });
    gitMocks.getGitChangePage.mockImplementation((
      _target: GitTarget,
      request: { expectedRevision: string }
    ) => Promise.resolve({
      kind: "page",
      revision: request.expectedRevision,
      files: [changedFile],
      matchedCount: 1,
      nextCursor: null,
      selection: null
    }));
    gitMocks.getGitDiff.mockResolvedValue({
      patch: "@@ -0,0 +1 @@\n+change",
      path: changedFile.path,
      additions: 1,
      deletions: 0,
      binary: false,
      truncated: false,
      files: []
    });
    gitMocks.executeGitAction.mockResolvedValue({ snapshot: isolatedSnapshot });

    render(<App />);
    await waitFor(() => expect(gitMocks.getGitWorkspaceSummary).toHaveBeenCalledWith(
      gitConversationTarget(ISOLATED_ID),
      undefined
    ));
    const navigation = await screen.findByRole("complementary", { name: "工作区和对话" });

    // The peer starts running after the first render and before any Git click, so the
    // synchronous gate and the button state are judged on the same facts.
    await user.click(within(navigation).getByText("根检出任务").closest("button")!);
    await waitFor(() => expect(gitMocks.getGitWorkspaceSummary).toHaveBeenCalledWith(
      gitConversationTarget(ROOT_ID),
      undefined
    ));
    runtimeMocks.runModel.mockImplementation(() => new Promise(() => undefined));
    await user.type(screen.getByLabelText("向 Agent 发送消息"), "在根检出上跑一会儿");
    await user.click(screen.getByRole("button", { name: "发送" }));
    await waitFor(() => expect(runtimeMocks.runModel).toHaveBeenCalledTimes(1));

    await user.click(within(navigation).getByText("隔离检出任务").closest("button")!);
    await expandGitStatus(user);
    await user.click(await screen.findByRole("button", { name: /变更.*新增 1 行/ }));
    return screen.findByRole("button", { name: `暂存 ${changedFile.path}` });
  }

  it("stages in an isolated worktree while a peer runs in the workspace root", async () => {
    const user = userEvent.setup();
    const stage = await stageButtonWithRunningRootPeer(user, true);

    expect(stage).toBeEnabled();
    await user.click(stage);

    await waitFor(() => expect(gitMocks.executeGitAction).toHaveBeenCalledWith(
      gitConversationTarget(ISOLATED_ID),
      { type: "stage", paths: [changedFile.path] }
    ));
    expect(screen.queryByText(
      "同一工作区中的另一项任务正在运行，暂不能执行 Git 写操作"
    )).not.toBeInTheDocument();
  });

  it("keeps blocking while the running peer's checkout is unknown", async () => {
    const user = userEvent.setup();
    const stage = await stageButtonWithRunningRootPeer(user, false);

    // No identity came back for the peer, so it may be sharing this checkout.
    expect(stage).toBeDisabled();
    await user.click(stage);

    expect(await screen.findByText(
      "同一工作区中的另一项任务正在运行，暂不能执行 Git 写操作"
    )).toBeInTheDocument();
    expect(gitMocks.executeGitAction).not.toHaveBeenCalled();
  });
});
