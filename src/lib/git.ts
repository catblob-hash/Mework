import { t } from "../i18n";
import { hasBackendRuntime, invoke } from "./backend";
import type { ConversationWorktree } from "../types";

export type GitFileStatus =
  | "added"
  | "modified"
  | "deleted"
  | "renamed"
  | "copied"
  | "typeChanged"
  | "unmerged"
  | "untracked"
  | "ignored"
  | "unknown";

export interface GitFileChange {
  /** Repository-relative path, always using `/` separators. */
  path: string;
  originalPath?: string | null;
  status: GitFileStatus;
  /** Raw porcelain-v2 index/worktree status codes when available. */
  indexStatus?: string | null;
  worktreeStatus?: string | null;
  staged?: boolean;
  unstaged?: boolean;
  untracked?: boolean;
  conflicted?: boolean;
  additions?: number | null;
  deletions?: number | null;
  binary?: boolean;
  submodule?: boolean;
  submoduleCommitChanged?: boolean;
  submoduleModified?: boolean;
  submoduleUntracked?: boolean;
}

export interface GitRemote {
  name: string;
  fetchRevision: string;
  pushRevision: string;
  url: string | null;
}

export interface GitUpstream {
  remoteName: string;
  remoteBranch: string;
  mergeRef: string;
  trackingRef: string;
  trackingOid: string | null;
  isLocal: boolean;
  remote: GitRemote;
}

export type GitRepositoryOperation = "merge" | "rebase" | "cherryPick" | "revert" | "bisect";
export type GitBisectOutcome = "old" | "new" | "skip";

/**
 * Derived, non-persistent state for the trusted working directory of one conversation.
 * A null command result means that working directory is not a Git worktree.
 */
export interface GitWorkspaceSnapshot {
  /** Opaque identity of the repository common directory; never infer it from a path. */
  repositoryId: string;
  /** Opaque identity of this exact worktree; linked worktrees have distinct values. */
  worktreeId: string;
  branch: string | null;
  head: string | null;
  contentRevision: string;
  upstream: string | null;
  upstreamTarget: GitUpstream | null;
  ahead: number;
  behind: number;
  additions: number;
  deletions: number;
  staged: number;
  unstaged: number;
  untracked: number;
  conflicted: number;
  stash: number;
  files: GitFileChange[];
  remote: GitRemote | null;
  remotes: GitRemote[];
  gitVersion: string;
  repositoryRoot?: string;
  worktreeRoot?: string;
  detached: boolean;
  unborn: boolean;
  operation: GitRepositoryOperation | null;
  /**
   * Opaque identity for the exact in-progress Git operation. Operation controls
   * echo it so stale UI cannot act on an aborted and restarted operation.
   */
  operationRevision: string | null;
  isClean: boolean;
  binaryFiles: number;
  warnings: string[];
  /**
   * Opaque identity for the bounded summary/page protocol. Older full
   * snapshots omit it and are treated as complete compatibility responses.
   */
  summaryRevision?: string;
  /** Total changed paths, including paths not loaded into `files` yet. */
  changedFiles?: number;
  /** Paths that can be staged by the repository-wide stage action. */
  stageable?: number;
  /** Paths that can be unstaged by the repository-wide unstage action. */
  unstageable?: number;
  /** False when `files` is only a locally loaded page rather than the full status. */
  filesComplete?: boolean;
}

export interface GitWorkspaceSummary
  extends Omit<
    GitWorkspaceSnapshot,
    "files" | "summaryRevision" | "changedFiles" | "stageable" | "unstageable" | "filesComplete"
  > {
  summaryRevision: string;
  changedFiles: number;
  stageable: number;
  unstageable: number;
}

export type GitWorkspaceSummaryResult =
  | { kind: "notRepository" }
  | { kind: "unchanged"; revision: string }
  | { kind: "snapshot"; summary: GitWorkspaceSummary };

export interface GitChangePageRequest {
  expectedRevision: string;
  cursor?: string;
  query?: string;
  limit?: number;
  selectedPath?: string;
  /**
   * List the changes since this commit — committed on the branch and not yet committed alike —
   * instead of the uncommitted ones: what a worktree has done since it was forked.
   */
  base?: string;
}

export type GitChangeSelection =
  | { state: "present"; file: GitFileChange }
  | { state: "filteredOut" }
  | { state: "missing" };

/**
 * One side of a Git write conflict test.
 *
 * `snapshot` is what the host last answered for that conversation's checkout in
 * the workspace being written: `undefined` when the renderer has never seen one,
 * `null` when the directory is not a repository. `isolated` says the conversation
 * still records its own worktree; a draft is always `false`, because it addresses
 * the workspace root and a ticked worktree checkbox is intent the host has not
 * honoured yet.
 */
export interface GitCheckoutRef {
  snapshot: GitWorkspaceSnapshot | null | undefined;
  isolated: boolean;
}

function gitCheckoutWorktreeId(snapshot: GitWorkspaceSnapshot | null | undefined): string | null {
  if (!snapshot?.repositoryId || !snapshot.worktreeId) return null;
  return snapshot.worktreeId;
}

/**
 * Whether two conversations are known to work in different checkouts.
 *
 * The judgement belongs to the host and is read back from the identities in its
 * snapshots. A recorded worktree on its own proves nothing, because the host
 * falls back to the workspace root when that directory is gone. Anything the
 * renderer has not observed — a conversation never polled, a directory that is
 * not a repository, an identity without both halves — reads as the same checkout,
 * so an unknown peer keeps blocking.
 */
export function gitCheckoutsAreDistinct(acting: GitCheckoutRef, peer: GitCheckoutRef): boolean {
  // Neither side asked for isolation, so both address the workspace root no
  // matter what an older snapshot may still say.
  if (!acting.isolated && !peer.isolated) return false;
  const actingWorktreeId = gitCheckoutWorktreeId(acting.snapshot);
  const peerWorktreeId = gitCheckoutWorktreeId(peer.snapshot);
  if (!actingWorktreeId || !peerWorktreeId) return false;
  return actingWorktreeId !== peerWorktreeId;
}

/**
 * Whether one workspace peer blocks a Git write by the acting conversation.
 *
 * A peer holding a Git mutation lease always blocks: linked worktrees share the
 * repository, so two writes still interleave. A peer merely running a model
 * blocks only while it may be working in the same checkout, which is the rule the
 * host itself applies when it decides who a workspace write must wait for.
 */
export function gitPeerBlocksMutation(input: {
  acting: GitCheckoutRef;
  peer: GitCheckoutRef;
  peerModelRunActive: boolean;
  peerGitMutationActive: boolean;
}): boolean {
  if (input.peerGitMutationActive) return true;
  if (!input.peerModelRunActive) return false;
  return !gitCheckoutsAreDistinct(input.acting, input.peer);
}

export type GitChangePageResult =
  | { kind: "stale"; summary: GitWorkspaceSummary }
  | {
      kind: "page";
      revision: string;
      files: GitFileChange[];
      matchedCount: number;
      nextCursor: string | null;
      selection: GitChangeSelection | null;
    };

export type GitDiffRequest =
  | { type: "working"; path?: string; context?: number }
  | { type: "staged"; path?: string; context?: number }
  | { type: "unstaged"; path?: string; context?: number }
  | { type: "compare"; base: string; head: string; path?: string; context?: number }
  /** From the commit `base` to the working tree: what a branch has done since it was forked. */
  | { type: "branch"; base: string; path?: string; context?: number };

export interface GitDiffResult {
  patch: string;
  path: string | null;
  additions: number;
  deletions: number;
  binary: boolean;
  truncated: boolean;
  /** Present for summary requests; callers should lazily request the selected path's patch. */
  files: GitFileChange[];
}

export type GitBranchKind = "local" | "remote";

export interface GitBranch {
  name: string;
  fullName?: string;
  kind: GitBranchKind;
  current: boolean;
  head: string | null;
  upstream: string | null;
  ahead: number;
  behind: number;
  merged?: boolean;
}

export interface GitBranchesResult {
  branches: GitBranch[];
  defaultBranch: string | null;
}

export type GitAction =
  | { type: "stage"; paths: string[] }
  | { type: "unstage"; paths: string[] }
  | {
      type: "discard";
      paths: string[];
      includeUntracked?: boolean;
      expectedContentRevision: string;
      expectedTargetRevision: string;
    }
  | { type: "checkout"; branch: string }
  | {
      type: "continue_operation";
      operation: GitRepositoryOperation;
      expectedHead: string;
      expectedOperationRevision: string;
    }
  | {
      type: "skip_operation";
      operation: GitRepositoryOperation;
      expectedHead: string;
      expectedOperationRevision: string;
    }
  | {
      type: "abort_operation";
      operation: GitRepositoryOperation;
      expectedHead: string;
      expectedOperationRevision: string;
    }
  | {
      type: "bisect_step";
      outcome: GitBisectOutcome;
      expectedHead: string;
      expectedOperationRevision: string;
      expectedContentRevision: string;
    };

export interface GitActionResult {
  snapshot: GitWorkspaceSnapshot | null;
  message?: string;
}

export interface GitDiscardPreparation {
  snapshot: GitWorkspaceSnapshot;
  targetRevision: string;
}

function requireGitRuntime(): void {
  if (!hasBackendRuntime()) throw new Error(t("Git 功能仅可在连接 Rust 后端时使用", "Git is available only with the Rust backend connected"));
}

function normalizeDiffResult(value: GitDiffResult | string): GitDiffResult {
  if (typeof value !== "string") return value;
  return {
    patch: value,
    path: null,
    additions: 0,
    deletions: 0,
    binary: false,
    truncated: false,
    files: []
  };
}

function normalizeBranchesResult(value: GitBranchesResult | GitBranch[]): GitBranchesResult {
  return Array.isArray(value) ? { branches: value, defaultBranch: null } : value;
}

export function gitFileHasStagedChange(file: GitFileChange): boolean {
  if (file.staged !== undefined) return file.staged;
  const status = file.indexStatus?.trim();
  return Boolean(status && status !== "." && status !== "?");
}

export function gitFileHasUnstagedChange(file: GitFileChange): boolean {
  if (file.unstaged !== undefined) return file.unstaged;
  if (file.untracked) return true;
  const status = file.worktreeStatus?.trim();
  return Boolean(status && status !== ".");
}

/**
 * Checkout targeted by a Git request.
 *
 * Git status belongs to a working directory, not a conversation. Both target
 * forms send only IDs to the host, which resolves paths from persisted documents;
 * the renderer must never supply a repository or Git-directory path. Workspace
 * targets resolve to the registered directory because worktrees belong to
 * conversations; a conversation target resolves to its worktree of the workspace
 * when it has one.
 */
export type GitTarget =
  | {
    kind: "conversation";
    conversationId: string;
    /**
     * Which of the project's workspaces, 1-based, as for a workspace target: absent or 1 is
     * workspace 1. The conversation's checkout of it — its worktree when it has one.
     */
    member?: number;
  }
  | {
    kind: "workspace";
    workspaceId: string;
    /**
     * Which of the project's workspaces, 1-based: absent or 1 is its first
     * directory, 2 and on the ones added after it. Still an index rather than a
     * path — the host looks the directory up in its own saved project.
     */
    member?: number;
  };

export function gitConversationTarget(conversationId: string, member?: number): GitTarget {
  return member && member > 1
    ? { kind: "conversation", conversationId, member }
    : { kind: "conversation", conversationId };
}

export function gitWorkspaceTarget(workspaceId: string, member?: number): GitTarget {
  return member && member > 1
    ? { kind: "workspace", workspaceId, member }
    : { kind: "workspace", workspaceId };
}

/**
 * The key a conversation's Git snapshot is stored under: the project id for the project's first
 * workspace, and `<project id>#<member>` for another of its workspaces. Two directories of one
 * project are two checkouts, and a snapshot of one must never be shown for the other.
 */
export function gitSurfaceKey(workspaceId: string, member = 1): string {
  return member > 1 ? `${workspaceId}#${member}` : workspaceId;
}

/** The project a {@link gitSurfaceKey} belongs to. */
export function gitSurfaceProjectId(surfaceKey: string): string {
  const separator = surfaceKey.indexOf("#");
  return separator < 0 ? surfaceKey : surfaceKey.slice(0, separator);
}

/**
 * The key a conversation's snapshot of its project workspace `member` is cached under: the
 * conversation id itself for workspace 1, and `<conversation id>#<member>` for the others, so
 * every workspace is polled, fenced and invalidated on its own.
 */
export function gitSnapshotKey(conversationId: string, member = 1): string {
  return member > 1 ? `${conversationId}#${member}` : conversationId;
}

/** The conversation a {@link gitSnapshotKey} belongs to. */
export function gitSnapshotKeyConversation(key: string): string {
  const separator = key.indexOf("#");
  return separator < 0 ? key : key.slice(0, separator);
}

/** Stable string suitable for React keys, cache keys, and dependency arrays. */
export function gitTargetKey(target: GitTarget): string {
  if (target.kind === "conversation") {
    return target.member && target.member > 1
      ? `conversation:${target.conversationId}#${target.member}`
      : `conversation:${target.conversationId}`;
  }
  return target.member && target.member > 1
    ? `workspace:${target.workspaceId}#${target.member}`
    : `workspace:${target.workspaceId}`;
}

export async function getGitWorkspaceSummary(
  target: GitTarget,
  knownRevision?: string
): Promise<GitWorkspaceSummaryResult> {
  requireGitRuntime();
  return invoke<GitWorkspaceSummaryResult>("get_git_workspace_summary", {
    target,
    knownRevision
  });
}

export function summaryToGitWorkspaceSnapshot(
  summary: GitWorkspaceSummary,
  files: GitFileChange[] = []
): GitWorkspaceSnapshot {
  return {
    ...summary,
    files,
    filesComplete: false
  };
}

export async function getGitChangePage(
  target: GitTarget,
  request: GitChangePageRequest
): Promise<GitChangePageResult> {
  requireGitRuntime();
  return invoke<GitChangePageResult>("get_git_change_page", { target, request });
}

export async function getGitDiff(
  target: GitTarget,
  request: GitDiffRequest
): Promise<GitDiffResult> {
  requireGitRuntime();
  const value = await invoke<GitDiffResult | string>("get_git_diff", { target, request });
  return normalizeDiffResult(value);
}

export async function getGitBranches(target: GitTarget): Promise<GitBranchesResult> {
  requireGitRuntime();
  const value = await invoke<GitBranchesResult | GitBranch[]>("get_git_branches", { target });
  return normalizeBranchesResult(value);
}

/**
 * Create an isolated worktree of the conversation's project workspace `member` (1-based) as the
 * conversation's trusted directory for that workspace, on whichever machine it is.
 *
 * `fromBranch` is the baseline branch, defaulting to the workspace HEAD. Persist
 * the returned record in `Conversation.worktrees` so future host resolution uses it.
 */
export async function createConversationWorktree(
  conversationId: string,
  member = 1,
  fromBranch?: string
): Promise<ConversationWorktree> {
  requireGitRuntime();
  return invoke<ConversationWorktree>("create_conversation_worktree", {
    conversationId,
    member,
    fromBranch: fromBranch ?? null
  });
}

/**
 * Release a conversation's isolated worktree of its project workspace `member`.
 *
 * Returns `true` when the worktree and branch were deleted. Returns `false`
 * when uncommitted work or additional commits require retaining it on disk;
 * either result removes the conversation's association.
 */
export async function releaseConversationWorktree(conversationId: string, member = 1): Promise<boolean> {
  requireGitRuntime();
  return invoke<boolean>("release_conversation_worktree", { conversationId, member });
}

export async function executeGitAction(
  target: GitTarget,
  action: GitAction
): Promise<GitActionResult> {
  requireGitRuntime();
  const value = await invoke<GitActionResult | GitWorkspaceSnapshot | null>("execute_git_action", {
    target,
    action
  });
  if (value === null || !("snapshot" in value)) return { snapshot: value };
  return value;
}

export async function prepareGitDiscard(
  target: GitTarget,
  paths: string[],
  includeUntracked = false
): Promise<GitDiscardPreparation> {
  requireGitRuntime();
  return invoke<GitDiscardPreparation>("prepare_git_discard", {
    target,
    paths,
    includeUntracked
  });
}
