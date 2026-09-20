import {
  getGitWorkspaceSummary,
  summaryToGitWorkspaceSnapshot
} from "./git";
import type { GitTarget, GitWorkspaceSnapshot, GitWorkspaceSummaryResult } from "./git";

export type GitSnapshotEntry = {
  workspaceId: string;
  snapshot: GitWorkspaceSnapshot | null;
};
export type GitSnapshots = Partial<Record<string, GitSnapshotEntry>>;
export type GitSnapshotRefreshResult =
  | { status: "resolved"; snapshot: GitWorkspaceSnapshot | null }
  | { status: "unchanged"; revision: string }
  | { status: "failed" };

export function gitSnapshotForWorkspace(
  entry: GitSnapshotEntry | undefined,
  workspaceId: string | undefined
): GitWorkspaceSnapshot | null | undefined {
  if (!entry || entry.workspaceId !== workspaceId) return undefined;
  return entry.snapshot;
}

export function gitSnapshotsAfterRefresh(
  current: GitSnapshots,
  conversationId: string,
  workspaceId: string,
  result: GitSnapshotRefreshResult
): GitSnapshots {
  if (result.status === "failed" || result.status === "unchanged") return current;
  const existing = current[conversationId];
  if (
    existing?.workspaceId === workspaceId
    && existing.snapshot === result.snapshot
  ) return current;
  return {
    ...current,
    [conversationId]: {
      workspaceId,
      snapshot: result.snapshot
    }
  };
}

export function gitSnapshotRefreshResultFromSummary(
  result: GitWorkspaceSummaryResult
): GitSnapshotRefreshResult {
  if (result.kind === "notRepository") {
    return { status: "resolved", snapshot: null };
  }
  if (result.kind === "unchanged") {
    return { status: "unchanged", revision: result.revision };
  }
  return {
    status: "resolved",
    snapshot: summaryToGitWorkspaceSnapshot(result.summary)
  };
}

export function gitSnapshotsAfterWorkspaceMutation(
  current: GitSnapshots,
  conversationIds: readonly string[],
  workspaceId: string,
  snapshot: GitWorkspaceSnapshot | null
): GitSnapshots {
  const next = { ...current };
  for (const conversationId of conversationIds) {
    next[conversationId] = { workspaceId, snapshot };
  }
  return next;
}

/**
 * Moves a draft's snapshot to the real conversation key on redemption.
 *
 * Both keys refer to the same checkout. Moving the entry prevents the Git
 * status card from disappearing until the next poll. If the destination
 * workspace differs, `gitSnapshotForWorkspace` returns `undefined` normally.
 */
export function gitSnapshotsAfterDraftRedemption(
  current: GitSnapshots,
  draftConversationId: string,
  conversationId: string
): GitSnapshots {
  const entry = current[draftConversationId];
  if (!entry) return current;
  const next = { ...current, [conversationId]: entry };
  delete next[draftConversationId];
  return next;
}

/**
 * Selects recipients for a snapshot returned by a write operation.
 *
 * Share only conversations running against the same checkout, not merely the
 * same workspace. An isolated worktree has different content and its own Git
 * actions; broadcasting the root snapshot to it would display unrelated changes.
 *
 * A recorded worktree is a conservative criterion. A missing directory may have
 * fallen back to the root and miss one broadcast, but that is safer than sending
 * a snapshot to a different checkout.
 */
export function gitSnapshotBroadcastIds(
  conversations: readonly { id: string; worktree: unknown }[],
  actingConversationId: string,
  actingRunsInOwnWorktree: boolean
): string[] {
  if (actingRunsInOwnWorktree) return [actingConversationId];
  const shared = conversations
    .filter((conversation) => !conversation.worktree)
    .map((conversation) => conversation.id);
  // Drafts are absent from workspace conversation lists but read and write the
  // workspace root.
  return shared.includes(actingConversationId) ? shared : [...shared, actingConversationId];
}

export function gitReviewSnapshotCacheKey(snapshot: GitWorkspaceSnapshot): string {
  return JSON.stringify([
    snapshot.repositoryId,
    snapshot.worktreeId,
    snapshot.repositoryRoot ?? "",
    snapshot.worktreeRoot ?? "",
    snapshot.branch ?? "",
    snapshot.head ?? "",
    snapshot.upstreamTarget
      ? [
          snapshot.upstreamTarget.remoteName,
          snapshot.upstreamTarget.remoteBranch,
          snapshot.upstreamTarget.mergeRef,
          snapshot.upstreamTarget.trackingRef,
          snapshot.upstreamTarget.trackingOid,
          snapshot.upstreamTarget.isLocal,
          snapshot.upstreamTarget.remote.fetchRevision,
          snapshot.upstreamTarget.remote.pushRevision
        ]
      : null,
    snapshot.remote
      ? [
          snapshot.remote.name,
          snapshot.remote.fetchRevision,
          snapshot.remote.pushRevision
        ]
      : null,
    snapshot.remotes.map((remote) => [
      remote.name,
      remote.fetchRevision,
      remote.pushRevision
    ])
  ]);
}

export interface GitControllerState {
  snapshots: GitSnapshots;
  /** Conversations currently holding a Git mutation lease. */
  mutationConversationIds: ReadonlySet<string>;
}

export interface GitRefreshHandlers {
  /** Runs only when this refresh is still the newest one for the conversation. */
  onError?: (error: unknown) => void;
}

export interface GitController {
  subscribe(listener: () => void): () => void;
  current(): GitControllerState;
  updateSnapshots(update: (current: GitSnapshots) => GitSnapshots): void;
  mutationIsActive(conversationId: string): boolean;
  /**
   * Grants the mutation lease and invalidates every in-flight poll for the
   * workspace: a poll that started before the lease was acquired must never
   * overwrite the mutation result with its older repository snapshot.
   */
  acquireMutationLease(
    conversationId: string,
    workspaceConversationIds: readonly string[]
  ): void;
  releaseMutationLease(conversationId: string): void;
  /**
   * Polls the workspace summary. A newer refresh or mutation lease for the
   * same conversation invalidates this poll's commit (last-token-wins).
   *
   * `conversationId` is the cache key, `target` is the addressing. For a real
   * conversation the two say the same thing; the draft conversation caches under
   * its own renderer-only key while asking Git about the workspace it has
   * selected, because the host has never heard of that key.
   */
  refresh(
    conversationId: string,
    workspaceId: string,
    target: GitTarget,
    handlers?: GitRefreshHandlers
  ): Promise<GitWorkspaceSnapshot | null | undefined>;
}

export function createGitController(): GitController {
  let state: GitControllerState = {
    snapshots: {},
    mutationConversationIds: new Set<string>()
  };
  const listeners = new Set<() => void>();
  const refreshTokens = new Map<string, number>();

  const notify = () => {
    for (const listener of [...listeners]) listener();
  };

  const commitSnapshots = (next: GitSnapshots) => {
    if (next === state.snapshots) return;
    state = { ...state, snapshots: next };
    notify();
  };

  return {
    subscribe(listener) {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    current() {
      return state;
    },
    updateSnapshots(update) {
      commitSnapshots(update(state.snapshots));
    },
    mutationIsActive(conversationId) {
      return state.mutationConversationIds.has(conversationId);
    },
    acquireMutationLease(conversationId, workspaceConversationIds) {
      for (const id of workspaceConversationIds) {
        refreshTokens.set(id, (refreshTokens.get(id) ?? 0) + 1);
      }
      if (state.mutationConversationIds.has(conversationId)) return;
      state = {
        ...state,
        mutationConversationIds: new Set(state.mutationConversationIds).add(conversationId)
      };
      notify();
    },
    releaseMutationLease(conversationId) {
      if (!state.mutationConversationIds.has(conversationId)) return;
      const next = new Set(state.mutationConversationIds);
      next.delete(conversationId);
      state = { ...state, mutationConversationIds: next };
      notify();
    },
    async refresh(conversationId, workspaceId, target, handlers) {
      const token = (refreshTokens.get(conversationId) ?? 0) + 1;
      refreshTokens.set(conversationId, token);
      const knownRevision = gitSnapshotForWorkspace(
        state.snapshots[conversationId],
        workspaceId
      )?.summaryRevision;
      try {
        const result = await getGitWorkspaceSummary(target, knownRevision);
        const refreshResult = gitSnapshotRefreshResultFromSummary(result);
        if (refreshTokens.get(conversationId) === token) {
          commitSnapshots(gitSnapshotsAfterRefresh(
            state.snapshots,
            conversationId,
            workspaceId,
            refreshResult
          ));
        }
        if (refreshResult.status === "resolved") return refreshResult.snapshot;
        return gitSnapshotForWorkspace(state.snapshots[conversationId], workspaceId);
      } catch (error) {
        if (refreshTokens.get(conversationId) === token) {
          handlers?.onError?.(error);
        }
        return undefined;
      }
    }
  };
}
