import {
  getGitChangePage,
  getGitWorkspaceSummary,
  gitSnapshotKeyConversation,
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
 * Moves a draft's snapshots — one per project workspace — to the real conversation's keys on
 * redemption.
 *
 * Both keys refer to the same checkout. Moving the entries prevents the Git
 * status card from disappearing until the next poll. If the destination
 * workspace differs, `gitSnapshotForWorkspace` returns `undefined` normally.
 */
export function gitSnapshotsAfterDraftRedemption(
  current: GitSnapshots,
  draftConversationId: string,
  conversationId: string
): GitSnapshots {
  const moved = Object.keys(current).filter((key) => gitSnapshotKeyConversation(key) === draftConversationId);
  if (moved.length === 0) return current;
  const next = { ...current };
  for (const key of moved) {
    next[`${conversationId}${key.slice(draftConversationId.length)}`] = current[key];
    delete next[key];
  }
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

/**
 * Whether the review pane lists `path` — a tracked file with a change — in the
 * checkout `snapshot` describes.
 *
 * A summary snapshot carries no file list, so the host is asked for that one path
 * by name, the way the pane itself asks for its selection. A summary gone stale
 * in the meantime is answered with the current one, which is asked once more.
 *
 * `base` is a worktree's fork point: its page opens on the branch's whole change
 * since then, which lists files already committed there as well, and only the
 * host knows that listing.
 */
export async function gitReviewListsPath(
  target: GitTarget,
  snapshot: GitWorkspaceSnapshot,
  path: string,
  base: string | null = null
): Promise<boolean> {
  if (!snapshot.summaryRevision || (!base && snapshot.filesComplete === true)) {
    return snapshot.files.some((file) => (
      file.path === path && !file.untracked && file.status !== "untracked"
    ));
  }
  let revision = snapshot.summaryRevision;
  for (let attempt = 0; attempt < 2; attempt += 1) {
    const result = await getGitChangePage(target, {
      expectedRevision: revision,
      selectedPath: path,
      limit: 1,
      ...(base ? { base } : {})
    });
    if (result.kind === "page") return result.selection?.state === "present";
    revision = result.summary.summaryRevision;
  }
  return false;
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
   * Polls the workspace summary. A newer refresh of the same key, or a mutation
   * lease for its conversation, invalidates this poll's commit (last-token-wins).
   *
   * `conversationId` is the cache key — a `gitSnapshotKey`, one per project
   * workspace of the conversation — and `target` is the addressing. For a real
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
      // Every workspace of every conversation named: a poll of any of them started before
      // the lease must not land after the write.
      const invalidated = new Set(workspaceConversationIds);
      for (const key of refreshTokens.keys()) {
        if (invalidated.has(gitSnapshotKeyConversation(key))) {
          refreshTokens.set(key, (refreshTokens.get(key) ?? 0) + 1);
        }
      }
      for (const id of workspaceConversationIds) {
        if (!refreshTokens.has(id)) refreshTokens.set(id, 1);
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
