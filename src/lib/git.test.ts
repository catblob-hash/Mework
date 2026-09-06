import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  executeGitAction,
  executeGitHubAction,
  getGitBranches,
  getGitDiff,
  getGitHistory,
  getGitChangePage,
  getGitHubPullRequestDiff,
  getGitHubPullRequestReadiness,
  getGitHubPullRequestReviewThreadComments,
  getGitHubPullRequestReviewThreads,
  getGitHubPullRequests,
  getGitWorkspaceSummary,
  getGitWorkspaceSnapshot,
  gitConversationTarget,
  gitCheckoutsAreDistinct,
  gitFileHasStagedChange,
  gitFileHasUnstagedChange,
  gitPeerBlocksMutation,
  gitTargetKey,
  gitWorkspaceTarget,
  prepareGitDiscard,
  prepareGitStageAll,
  summaryToGitWorkspaceSnapshot,
  type GitAction,
  type GitCheckoutRef,
  type GitHubAction,
  type GitWorkspaceSnapshot
} from "./git";

const backend = vi.hoisted(() => ({
  hasBackendRuntime: vi.fn(() => true),
  invoke: vi.fn()
}));

vi.mock("./backend", () => backend);

const conversationTarget = gitConversationTarget("conversation-1");

describe("Git backend client", () => {
  beforeEach(() => {
    backend.hasBackendRuntime.mockReturnValue(true);
    backend.invoke.mockReset();
  });

  it("resolves snapshots through the trusted conversation id", async () => {
    backend.invoke.mockResolvedValueOnce(null);

    await expect(getGitWorkspaceSnapshot(conversationTarget)).resolves.toBeNull();
    expect(backend.invoke).toHaveBeenCalledWith("get_git_workspace_snapshot", {
      target: conversationTarget
    });
  });

  it("uses revision-bound bounded summary and change-page commands", async () => {
    const summary = {
      repositoryId: "repository-id-1",
      worktreeId: "worktree-id-1",
      branch: "main",
      head: "abc",
      contentRevision: "content",
      summaryRevision: "summary",
      upstream: null,
      upstreamTarget: null,
      ahead: 0,
      behind: 0,
      additions: 0,
      deletions: 0,
      staged: 0,
      unstaged: 1,
      untracked: 1,
      conflicted: 0,
      stash: 0,
      changedFiles: 2,
      stageable: 2,
      unstageable: 0,
      remote: null,
      remotes: [],
      gitVersion: "2.50.0",
      repositoryRoot: "C:/repo",
      worktreeRoot: "C:/repo",
      detached: false,
      unborn: false,
      operation: null,
      operationRevision: null,
      isClean: false,
      binaryFiles: 0,
      warnings: []
    };
    backend.invoke
      .mockResolvedValueOnce({ kind: "snapshot", summary })
      .mockResolvedValueOnce({
        kind: "page",
        revision: "summary",
        files: [],
        matchedCount: 2,
        nextCursor: null,
        selection: null
      });

    await expect(getGitWorkspaceSummary(conversationTarget, "known")).resolves.toEqual({
      kind: "snapshot",
      summary
    });
    const request = {
      expectedRevision: "summary",
      query: "src",
      limit: 200
    };
    await expect(getGitChangePage(conversationTarget, request)).resolves.toMatchObject({
      kind: "page",
      matchedCount: 2
    });
    expect(backend.invoke).toHaveBeenNthCalledWith(1, "get_git_workspace_summary", {
      target: conversationTarget,
      knownRevision: "known"
    });
    expect(backend.invoke).toHaveBeenNthCalledWith(2, "get_git_change_page", {
      target: conversationTarget,
      request
    });
    expect(summaryToGitWorkspaceSnapshot(summary)).toMatchObject({
      summaryRevision: "summary",
      files: [],
      filesComplete: false
    });
  });

  it("prepares a path-scoped discard proof before the mutation", async () => {
    const preparation = {
      snapshot: { branch: "main", contentRevision: "revision-1" },
      targetRevision: "target-revision-1"
    };
    backend.invoke.mockResolvedValueOnce(preparation);

    await expect(prepareGitDiscard(
      conversationTarget,
      ["src/App.tsx"],
      false
    )).resolves.toBe(preparation);
    expect(backend.invoke).toHaveBeenCalledWith("prepare_git_discard", {
      target: conversationTarget,
      paths: ["src/App.tsx"],
      includeUntracked: false
    });
  });

  it("prepares a repository-scoped stage-all proof before the mutation", async () => {
    const preparation = {
      snapshot: { branch: "main", contentRevision: "revision-1" },
      targetRevision: "target-revision-1",
      candidateTreeOid: "a".repeat(40)
    };
    backend.invoke.mockResolvedValueOnce(preparation);

    await expect(prepareGitStageAll(conversationTarget)).resolves.toBe(preparation);
    expect(backend.invoke).toHaveBeenCalledWith("prepare_git_stage_all", {
      target: conversationTarget
    });
  });

  it("normalizes plain patch responses while preserving structured diffs", async () => {
    backend.invoke
      .mockResolvedValueOnce("--- a/a.ts\n+++ b/a.ts\n")
      .mockResolvedValueOnce({
        patch: "@@ -1 +1 @@\n-a\n+b",
        path: "a.ts",
        additions: 1,
        deletions: 1,
        binary: false,
        truncated: false,
        files: []
      });

    await expect(getGitDiff(conversationTarget, { type: "unstaged", path: "a.ts" })).resolves.toMatchObject({
      patch: "--- a/a.ts\n+++ b/a.ts\n",
      path: null,
      files: []
    });
    await expect(getGitDiff(conversationTarget, { type: "staged", path: "a.ts" })).resolves.toMatchObject({
      path: "a.ts",
      additions: 1,
      deletions: 1
    });
    expect(backend.invoke).toHaveBeenNthCalledWith(1, "get_git_diff", {
      target: conversationTarget,
      request: { type: "unstaged", path: "a.ts" }
    });
  });

  it("normalizes collection-only branch, history, and pull request responses", async () => {
    backend.invoke
      .mockResolvedValueOnce([{
        name: "main",
        kind: "local",
        current: true,
        head: "abc",
        upstream: "origin/main",
        ahead: 0,
        behind: 0
      }])
      .mockResolvedValueOnce([{
        oid: "abcdef",
        shortOid: "abcdef",
        subject: "Initial",
        authorName: "Cat",
        authoredAt: "2026-07-24T00:00:00Z",
        parents: []
      }])
      .mockResolvedValueOnce([{
        number: 12,
        title: "Review",
        state: "open",
        url: "https://github.com/example/repo/pull/12",
        author: "cat",
        headRefName: "feature",
        baseRefName: "main",
        draft: false,
        updatedAt: "2026-07-24T00:00:00Z"
      }]);

    await expect(getGitBranches(conversationTarget)).resolves.toMatchObject({
      defaultBranch: null,
      branches: [{ name: "main" }]
    });
    await expect(getGitHistory(conversationTarget)).resolves.toMatchObject({
      nextCursor: null,
      commits: [{ subject: "Initial" }]
    });
    await expect(getGitHubPullRequests(conversationTarget, {
      page: 3,
      pageSize: 25
    })).resolves.toMatchObject({
      pullRequests: [{ number: 12 }],
      page: 3,
      pageSize: 25,
      hasMore: false,
      nextPage: null
    });
    expect(backend.invoke).toHaveBeenLastCalledWith("get_github_pull_requests", {
      target: conversationTarget,
      page: 3,
      pageSize: 25
    });
  });

  it("sends workspace targets for read and write commands", async () => {
    const target = gitWorkspaceTarget("workspace-1");
    const action = { type: "stage" as const, paths: ["src/App.tsx"] };
    backend.invoke
      .mockResolvedValueOnce([])
      .mockResolvedValueOnce(null);

    await expect(getGitBranches(target)).resolves.toMatchObject({ branches: [] });
    await expect(executeGitAction(target, action)).resolves.toEqual({ snapshot: null });
    expect(backend.invoke).toHaveBeenNthCalledWith(1, "get_git_branches", {
      target
    });
    expect(backend.invoke).toHaveBeenNthCalledWith(2, "execute_git_action", {
      target,
      action
    });
  });

  it("gives each Git target kind a distinct stable key", () => {
    const conversation = gitConversationTarget("shared-id");
    const workspace = gitWorkspaceTarget("shared-id");

    expect(gitTargetKey(conversation)).toBe("conversation:shared-id");
    expect(gitTargetKey(workspace)).toBe("workspace:shared-id");
    expect(gitTargetKey(gitConversationTarget("shared-id"))).toBe(gitTargetKey(conversation));
    expect(gitTargetKey(gitWorkspaceTarget("shared-id"))).toBe(gitTargetKey(workspace));
    expect(gitTargetKey(conversation)).not.toBe(gitTargetKey(workspace));
  });

  it("requests pull request readiness through the trusted conversation and number", async () => {
    const readiness = {
      identity: { number: 42 },
      identityRevision: "identity-revision",
      readinessRevision: "readiness-revision"
    };
    backend.invoke.mockResolvedValueOnce(readiness);

    await expect(getGitHubPullRequestReadiness(conversationTarget, 42))
      .resolves.toBe(readiness);
    expect(backend.invoke).toHaveBeenCalledWith(
      "get_github_pull_request_readiness",
      {
        target: conversationTarget,
        number: 42
      }
    );
  });

  it("normalizes action snapshots and forwards tagged actions", async () => {
    const snapshot = {
      branch: "main",
      head: "abc",
      contentRevision: "revision-1",
      upstream: "origin/main",
      upstreamTarget: null,
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
      gitVersion: "git version 2.50.0"
    };
    backend.invoke.mockResolvedValueOnce(snapshot);

    await expect(executeGitAction(conversationTarget, {
      type: "push",
      expectedRepositoryId: "repository-id-1",
      expectedWorktreeId: "worktree-id-1",
      remote: {
        name: "origin",
        fetchRevision: "origin-fetch-revision-1",
        pushRevision: "origin-push-revision-1",
        url: "https://github.com/example/repo.git"
      },
      expectedLocalBranch: "main",
      remoteBranch: "main",
      expectedHead: "0123456789abcdef0123456789abcdef01234567",
      expectedUpstream: null,
      setUpstream: true
    })).resolves.toEqual({ snapshot });
    expect(backend.invoke).toHaveBeenCalledWith("execute_git_action", {
      target: conversationTarget,
      action: {
        type: "push",
        expectedRepositoryId: "repository-id-1",
        expectedWorktreeId: "worktree-id-1",
        remote: {
          name: "origin",
          fetchRevision: "origin-fetch-revision-1",
          pushRevision: "origin-push-revision-1",
          url: "https://github.com/example/repo.git"
        },
        expectedLocalBranch: "main",
        remoteBranch: "main",
        expectedHead: "0123456789abcdef0123456789abcdef01234567",
        expectedUpstream: null,
        setUpstream: true
      }
    });
  });

  it("does not accept legacy string-only remote or upstream action shapes", () => {
    type FetchAction = Extract<GitAction, { type: "fetch" }>;
    type PullAction = Extract<GitAction, { type: "pull" }>;
    type PushAction = Extract<GitAction, { type: "push" }>;
    type LegacyFetch = { type: "fetch"; remote: string };
    type LegacyPull = { type: "pull"; remote: string; branch: string };
    type LegacyPush = {
      type: "push";
      remote: string;
      branch: string;
      expectedHead: string;
    };
    const legacyFetchIsAssignable: LegacyFetch extends FetchAction ? true : false = false;
    const legacyPullIsAssignable: LegacyPull extends PullAction ? true : false = false;
    const legacyPushIsAssignable: LegacyPush extends PushAction ? true : false = false;

    expect([
      legacyFetchIsAssignable,
      legacyPullIsAssignable,
      legacyPushIsAssignable
    ]).toEqual([false, false, false]);
  });

  it("passes an optional PR path to the GitHub diff command", async () => {
    const diff = {
      headRefOid: "0123456789abcdef0123456789abcdef01234567",
      patch: "patch",
      path: "src/App.tsx",
      additions: 1,
      deletions: 0,
      binary: false,
      truncated: false,
      files: []
    };
    backend.invoke.mockResolvedValueOnce(diff);

    await expect(
      getGitHubPullRequestDiff(conversationTarget, 42, "src/App.tsx")
    ).resolves.toEqual(diff);
    expect(backend.invoke).toHaveBeenCalledWith("get_github_pull_request_diff", {
      target: conversationTarget,
      number: 42,
      path: "src/App.tsx"
    });
  });

  it("forwards the reviewed head and cursor for pull request review threads", async () => {
    const result = {
      number: 42,
      headRefOid: "0123456789abcdef0123456789abcdef01234567",
      threads: [],
      totalCount: 0,
      nextCursor: null
    };
    backend.invoke.mockResolvedValueOnce(result);
    const request = {
      number: 42,
      expectedHeadOid: result.headRefOid,
      cursor: "cursor-1",
      pageSize: 30
    };

    await expect(
      getGitHubPullRequestReviewThreads(conversationTarget, request)
    ).resolves.toBe(result);
    expect(backend.invoke).toHaveBeenCalledWith(
      "get_github_pull_request_review_threads",
      { target: conversationTarget, request }
    );
  });

  it("forwards the exact identity, thread, head, state, and cursor for review replies", async () => {
    const result = {
      number: 42,
      headRefOid: "0123456789abcdef0123456789abcdef01234567",
      threadId: "PRRT_thread_1",
      comments: [],
      totalCount: 50,
      nextCursor: null
    };
    backend.invoke.mockResolvedValueOnce(result);
    const request = {
      expectedRepository: {
        host: "github.example",
        owner: "example",
        name: "repo"
      },
      expectedViewerLogin: "cat",
      number: 42,
      expectedState: "open" as const,
      expectedHeadOid: result.headRefOid,
      threadId: result.threadId,
      cursor: "comment-cursor-50",
      pageSize: 50
    };

    await expect(
      getGitHubPullRequestReviewThreadComments(conversationTarget, request)
    ).resolves.toBe(result);
    expect(backend.invoke).toHaveBeenCalledWith(
      "get_github_pull_request_review_thread_comments",
      { target: conversationTarget, request }
    );
  });

  it("forwards the exact proof-bound pull request merge contract", async () => {
    backend.invoke.mockResolvedValueOnce({
      repository: null,
      pullRequest: null,
      snapshot: null
    });
    const action = {
      type: "merge_pull_request" as const,
      number: 42,
      expectedRepository: {
        host: "github.com",
        owner: "example",
        name: "repo"
      },
      expectedViewerLogin: "cat",
      expectedHeadOid: "0123456789abcdef0123456789abcdef01234567",
      expectedBaseOid: "89abcdef0123456789abcdef0123456789abcdef",
      expectedState: "open" as const,
      expectedIdentityRevision: "identity-revision-42",
      expectedReadinessRevision: "readiness-revision-42",
      method: "squash" as const
    };

    await executeGitHubAction(conversationTarget, action);
    expect(backend.invoke).toHaveBeenCalledWith("execute_github_action", {
      target: conversationTarget,
      action
    });
  });

  it("does not accept the legacy merge shape without base and readiness proofs", () => {
    type MergeAction = Extract<GitHubAction, { type: "merge_pull_request" }>;
    type LegacyMergeAction = {
      type: "merge_pull_request";
      expectedRepository: { host: string; owner: string; name: string };
      expectedViewerLogin: string;
      number: number;
      expectedHeadOid: string;
      expectedState: "open";
      method: "squash";
    };
    const legacyShapeIsAssignable: LegacyMergeAction extends MergeAction ? true : false = false;

    expect(legacyShapeIsAssignable).toBe(false);
  });

  it("forwards exact identity, head, state, and line sides when submitting a review", async () => {
    backend.invoke.mockResolvedValueOnce({
      repository: null,
      pullRequest: null
    });
    const action = {
      type: "submit_pull_request_review" as const,
      expectedRepository: {
        host: "github.com",
        owner: "example",
        name: "repo"
      },
      expectedViewerLogin: "cat",
      number: 42,
      expectedHeadOid: "0123456789abcdef0123456789abcdef01234567",
      expectedState: "open" as const,
      event: "request_changes" as const,
      body: "Please address the inline notes.",
      comments: [{
        path: "src/App.tsx",
        line: 17,
        side: "RIGHT" as const,
        body: "Handle the empty state here."
      }]
    };

    await executeGitHubAction(conversationTarget, action);
    expect(backend.invoke).toHaveBeenCalledWith("execute_github_action", {
      target: conversationTarget,
      action
    });
  });

  it("derives staged and unstaged state from explicit flags or porcelain codes", () => {
    expect(gitFileHasStagedChange({ path: "a", status: "modified", staged: true })).toBe(true);
    expect(gitFileHasStagedChange({ path: "a", status: "modified", indexStatus: "M" })).toBe(true);
    expect(gitFileHasStagedChange({ path: "a", status: "modified", indexStatus: "." })).toBe(false);
    expect(gitFileHasUnstagedChange({ path: "a", status: "untracked", untracked: true })).toBe(true);
    expect(gitFileHasUnstagedChange({ path: "a", status: "modified", worktreeStatus: "M" })).toBe(true);
    expect(gitFileHasUnstagedChange({ path: "a", status: "modified", worktreeStatus: "." })).toBe(false);
  });

  it("refuses to synthesize Git state without the Rust runtime", async () => {
    backend.hasBackendRuntime.mockReturnValue(false);

    await expect(getGitWorkspaceSnapshot(conversationTarget)).rejects.toThrow(
      "Git 功能仅可在连接 Rust 后端时使用"
    );
    expect(backend.invoke).not.toHaveBeenCalled();
  });
});

/**
 * Which peers of one workspace block a Git write.
 *
 * Only the identities matter here, so the snapshots carry nothing else. Every
 * case keeps one repository: two different repositories would make an isolated
 * checkout look distinct for the wrong reason.
 */
describe("Git write conflicts between workspace peers", () => {
  const checkout = (
    worktreeId: string,
    repositoryId = "repository-id-1"
  ): GitWorkspaceSnapshot => ({ repositoryId, worktreeId } as GitWorkspaceSnapshot);
  const root = checkout("worktree-id-root");
  const isolated = checkout("worktree-id-conversation");

  it("calls two checkouts distinct only when the host has identified both", () => {
    const cases: { name: string; acting: GitCheckoutRef; peer: GitCheckoutRef; distinct: boolean }[] = [
      {
        name: "both conversations sit in the workspace root",
        acting: { snapshot: root, isolated: false },
        peer: { snapshot: root, isolated: false },
        distinct: false
      },
      {
        name: "neither records a worktree, so a disagreeing snapshot is stale",
        acting: { snapshot: root, isolated: false },
        peer: { snapshot: isolated, isolated: false },
        distinct: false
      },
      {
        name: "the peer runs in a worktree the host gave its own identity",
        acting: { snapshot: root, isolated: false },
        peer: { snapshot: isolated, isolated: true },
        distinct: true
      },
      {
        name: "the acting conversation is the isolated one",
        acting: { snapshot: isolated, isolated: true },
        peer: { snapshot: root, isolated: false },
        distinct: true
      },
      {
        name: "both are isolated in worktrees of their own",
        acting: { snapshot: checkout("worktree-id-a"), isolated: true },
        peer: { snapshot: checkout("worktree-id-b"), isolated: true },
        distinct: true
      },
      {
        name: "a recorded worktree the host resolved back to the root",
        acting: { snapshot: root, isolated: false },
        peer: { snapshot: root, isolated: true },
        distinct: false
      },
      {
        name: "the peer has never been polled",
        acting: { snapshot: root, isolated: false },
        peer: { snapshot: undefined, isolated: true },
        distinct: false
      },
      {
        name: "the peer's directory is not a repository",
        acting: { snapshot: root, isolated: false },
        peer: { snapshot: null, isolated: true },
        distinct: false
      },
      {
        name: "the acting conversation has never been polled",
        acting: { snapshot: undefined, isolated: false },
        peer: { snapshot: isolated, isolated: true },
        distinct: false
      },
      {
        name: "an identity without a worktree half",
        acting: { snapshot: root, isolated: false },
        peer: { snapshot: checkout(""), isolated: true },
        distinct: false
      },
      {
        name: "an identity without a repository half",
        acting: { snapshot: root, isolated: false },
        peer: { snapshot: checkout("worktree-id-conversation", ""), isolated: true },
        distinct: false
      }
    ];

    for (const item of cases) {
      expect(
        { name: item.name, distinct: gitCheckoutsAreDistinct(item.acting, item.peer) }
      ).toEqual({ name: item.name, distinct: item.distinct });
    }
  });

  it("blocks a write for every peer that may be working in the same checkout", () => {
    const sameCheckout = {
      acting: { snapshot: root, isolated: false },
      peer: { snapshot: root, isolated: false }
    };
    const otherCheckout = {
      acting: { snapshot: root, isolated: false },
      peer: { snapshot: isolated, isolated: true }
    };

    expect(gitPeerBlocksMutation({
      ...sameCheckout,
      peerModelRunActive: false,
      peerGitMutationActive: false
    })).toBe(false);
    expect(gitPeerBlocksMutation({
      ...sameCheckout,
      peerModelRunActive: true,
      peerGitMutationActive: false
    })).toBe(true);
    expect(gitPeerBlocksMutation({
      ...otherCheckout,
      peerModelRunActive: true,
      peerGitMutationActive: false
    })).toBe(false);
    // A linked worktree shares the repository, so a peer's Git write still blocks.
    expect(gitPeerBlocksMutation({
      ...otherCheckout,
      peerModelRunActive: false,
      peerGitMutationActive: true
    })).toBe(true);
    expect(gitPeerBlocksMutation({
      ...sameCheckout,
      peerModelRunActive: false,
      peerGitMutationActive: true
    })).toBe(true);
  });
});
