import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  createGitController,
  gitSnapshotBroadcastIds,
  gitSnapshotForWorkspace,
  gitSnapshotRefreshResultFromSummary,
  gitSnapshotsAfterDraftRedemption,
  gitSnapshotsAfterRefresh,
  gitSnapshotsAfterWorkspaceMutation
} from "./gitController";
import {
  gitConversationTarget,
  gitWorkspaceTarget,
  type GitWorkspaceSnapshot
} from "./git";

const gitMocks = vi.hoisted(() => ({
  getGitWorkspaceSummary: vi.fn()
}));
vi.mock("./git", async (importOriginal) => ({
  ...await importOriginal<typeof import("./git")>(),
  ...gitMocks
}));

const conversationTarget = gitConversationTarget("conversation-1");

function snapshot(overrides: Partial<GitWorkspaceSnapshot> = {}): GitWorkspaceSnapshot {
  return {
    summaryRevision: "revision-1",
    ...overrides
  } as GitWorkspaceSnapshot;
}

beforeEach(() => {
  gitMocks.getGitWorkspaceSummary.mockReset();
});

describe("pure snapshot helpers", () => {
  it("gitSnapshotForWorkspace only matches the entry's workspace", () => {
    const entry = { workspaceId: "workspace-1", snapshot: null };
    expect(gitSnapshotForWorkspace(undefined, "workspace-1")).toBeUndefined();
    expect(gitSnapshotForWorkspace(entry, "workspace-2")).toBeUndefined();
    expect(gitSnapshotForWorkspace(entry, "workspace-1")).toBeNull();
  });

  it("gitSnapshotsAfterRefresh keeps identity for failed, unchanged, and identical results", () => {
    const resolved = snapshot();
    const current = {
      "conversation-1": { workspaceId: "workspace-1", snapshot: resolved }
    };
    expect(gitSnapshotsAfterRefresh(current, "conversation-1", "workspace-1", { status: "failed" }))
      .toBe(current);
    expect(gitSnapshotsAfterRefresh(
      current,
      "conversation-1",
      "workspace-1",
      { status: "unchanged", revision: "revision-1" }
    )).toBe(current);
    expect(gitSnapshotsAfterRefresh(
      current,
      "conversation-1",
      "workspace-1",
      { status: "resolved", snapshot: resolved }
    )).toBe(current);
    const next = gitSnapshotsAfterRefresh(
      current,
      "conversation-1",
      "workspace-1",
      { status: "resolved", snapshot: null }
    );
    expect(next).not.toBe(current);
    expect(next["conversation-1"]?.snapshot).toBeNull();
  });

  it("gitSnapshotsAfterWorkspaceMutation stamps every conversation", () => {
    const stamped = snapshot();
    const next = gitSnapshotsAfterWorkspaceMutation({}, ["a", "b"], "workspace-1", stamped);
    expect(next.a).toEqual({ workspaceId: "workspace-1", snapshot: stamped });
    expect(next.b).toEqual({ workspaceId: "workspace-1", snapshot: stamped });
  });

  /** Broadcast root mutations only to conversations sharing the root checkout. */
  it("gitSnapshotBroadcastIds keeps a root mutation off isolated worktrees", () => {
    const conversations = [
      { id: "root-a", worktree: null },
      { id: "isolated", worktree: { path: "C:/ws/.mework/worktrees/x" } },
      { id: "root-b", worktree: null }
    ];
    expect(gitSnapshotBroadcastIds(conversations, "root-a", false)).toEqual(["root-a", "root-b"]);
  });

  it("gitSnapshotBroadcastIds includes the draft, which is not a workspace row", () => {
    const conversations = [{ id: "root-a", worktree: null }];
    expect(gitSnapshotBroadcastIds(conversations, "__draft__", false))
      .toEqual(["root-a", "__draft__"]);
  });

  it("gitSnapshotBroadcastIds keeps a worktree mutation to itself", () => {
    const conversations = [
      { id: "root-a", worktree: null },
      { id: "isolated", worktree: { path: "C:/ws/.mework/worktrees/x" } }
    ];
    expect(gitSnapshotBroadcastIds(conversations, "isolated", true)).toEqual(["isolated"]);
  });

  it("gitSnapshotsAfterDraftRedemption moves the draft entry to the conversation key", () => {
    const entry = { workspaceId: "workspace-1", snapshot: snapshot() };
    const current = {
      "draft-conversation": entry,
      "conversation-other": { workspaceId: "workspace-2", snapshot: null }
    };

    const next = gitSnapshotsAfterDraftRedemption(
      current,
      "draft-conversation",
      "conversation-1"
    );
    expect(next).not.toBe(current);
    expect(next["conversation-1"]).toBe(entry);
    expect(next["draft-conversation"]).toBeUndefined();
    expect(next["conversation-other"]).toBe(current["conversation-other"]);
  });

  it("gitSnapshotsAfterDraftRedemption keeps identity without a draft entry", () => {
    const current = {
      "conversation-1": { workspaceId: "workspace-1", snapshot: snapshot() }
    };

    expect(gitSnapshotsAfterDraftRedemption(
      current,
      "draft-conversation",
      "conversation-2"
    )).toBe(current);
  });

  it("gitSnapshotRefreshResultFromSummary maps the three summary kinds", () => {
    expect(gitSnapshotRefreshResultFromSummary({ kind: "notRepository" }))
      .toEqual({ status: "resolved", snapshot: null });
    expect(gitSnapshotRefreshResultFromSummary({ kind: "unchanged", revision: "revision-9" }))
      .toEqual({ status: "unchanged", revision: "revision-9" });
  });
});

describe("createGitController", () => {
  it("tracks mutation leases and notifies subscribers", () => {
    const controller = createGitController();
    const listener = vi.fn();
    controller.subscribe(listener);

    controller.acquireMutationLease("conversation-1", ["conversation-1", "conversation-2"]);
    expect(controller.mutationIsActive("conversation-1")).toBe(true);
    expect(controller.mutationIsActive("conversation-2")).toBe(false);
    expect(listener).toHaveBeenCalledTimes(1);

    controller.releaseMutationLease("conversation-1");
    expect(controller.mutationIsActive("conversation-1")).toBe(false);
    expect(listener).toHaveBeenCalledTimes(2);

    controller.releaseMutationLease("conversation-1");
    expect(listener).toHaveBeenCalledTimes(2);
  });

  it("commits a resolved refresh and passes the known revision", async () => {
    const controller = createGitController();
    const resolved = snapshot();
    gitMocks.getGitWorkspaceSummary.mockResolvedValue({ kind: "notRepository" });
    await controller.refresh("conversation-1", "workspace-1", conversationTarget);
    expect(gitMocks.getGitWorkspaceSummary).toHaveBeenCalledWith(conversationTarget, undefined);

    controller.updateSnapshots(() => ({
      "conversation-1": { workspaceId: "workspace-1", snapshot: resolved }
    }));
    gitMocks.getGitWorkspaceSummary.mockResolvedValue({ kind: "unchanged", revision: "revision-1" });
    const result = await controller.refresh("conversation-1", "workspace-1", conversationTarget);
    expect(gitMocks.getGitWorkspaceSummary).toHaveBeenLastCalledWith(conversationTarget, "revision-1");
    expect(result).toBe(resolved);
  });

  it("passes a workspace target through while caching under the draft conversation", async () => {
    const controller = createGitController();
    const target = gitWorkspaceTarget("workspace-1");
    gitMocks.getGitWorkspaceSummary.mockResolvedValue({ kind: "notRepository" });

    await expect(controller.refresh(
      "draft-conversation",
      "workspace-1",
      target
    )).resolves.toBeNull();
    expect(gitMocks.getGitWorkspaceSummary).toHaveBeenCalledWith(target, undefined);
    expect(controller.current().snapshots["draft-conversation"]).toEqual({
      workspaceId: "workspace-1",
      snapshot: null
    });
    expect(controller.current().snapshots["workspace-1"]).toBeUndefined();
  });

  it("drops a stale refresh commit when a newer lease bumped the token", async () => {
    const controller = createGitController();
    let release!: (value: { kind: "notRepository" }) => void;
    gitMocks.getGitWorkspaceSummary.mockImplementation(() => new Promise((resolve) => {
      release = resolve;
    }));
    const pending = controller.refresh("conversation-1", "workspace-1", conversationTarget);
    controller.acquireMutationLease("conversation-1", ["conversation-1"]);
    release({ kind: "notRepository" });
    await pending;
    expect(controller.current().snapshots["conversation-1"]).toBeUndefined();
  });

  it("reports errors only for the newest refresh and resolves undefined", async () => {
    const controller = createGitController();
    gitMocks.getGitWorkspaceSummary.mockRejectedValue(new Error("summary failed"));
    const onError = vi.fn();
    const result = await controller.refresh(
      "conversation-1",
      "workspace-1",
      conversationTarget,
      { onError }
    );
    expect(result).toBeUndefined();
    expect(onError).toHaveBeenCalledTimes(1);

    let rejectStale!: (error: Error) => void;
    gitMocks.getGitWorkspaceSummary.mockImplementation(() => new Promise((_, reject) => {
      rejectStale = reject;
    }));
    const staleError = vi.fn();
    const stale = controller.refresh(
      "conversation-1",
      "workspace-1",
      conversationTarget,
      { onError: staleError }
    );
    controller.acquireMutationLease("conversation-1", ["conversation-1"]);
    rejectStale(new Error("stale failure"));
    await stale;
    expect(staleError).not.toHaveBeenCalled();
  });
});
