import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  executeGitAction,
  getGitBranches,
  getGitDiff,
  getGitChangePage,
  getGitWorkspaceSummary,
  gitConversationTarget,
  gitCheckoutsAreDistinct,
  gitFileHasStagedChange,
  gitFileHasUnstagedChange,
  gitPeerBlocksMutation,
  gitTargetKey,
  gitWorkspaceTarget,
  prepareGitDiscard,
  summaryToGitWorkspaceSnapshot,
  type GitCheckoutRef,
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

  it("normalizes collection-only branch responses", async () => {
    backend.invoke
      .mockResolvedValueOnce([{
        name: "main",
        kind: "local",
        current: true,
        head: "abc",
        upstream: "origin/main",
        ahead: 0,
        behind: 0
      }]);

    await expect(getGitBranches(conversationTarget)).resolves.toMatchObject({
      defaultBranch: null,
      branches: [{ name: "main" }]
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
      type: "checkout",
      branch: "main"
    })).resolves.toEqual({ snapshot });
    expect(backend.invoke).toHaveBeenCalledWith("execute_git_action", {
      target: conversationTarget,
      action: {
        type: "checkout",
        branch: "main"
      }
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

    await expect(getGitWorkspaceSummary(conversationTarget)).rejects.toThrow(
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
