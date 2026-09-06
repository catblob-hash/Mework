import { hasBackendRuntime, invoke } from "./backend";
import type { ConversationWorktree } from "../types";

export type GitReviewView = "changes" | "history" | "branches" | "compare" | "pullRequests";

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
  expectedStageAllTargetRevision?: string;
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
      stageAllTargetRevision?: string;
      candidateTreeOid?: string;
    };

export type GitDiffRequest =
  | { type: "working"; path?: string; expectedStageAllTargetRevision?: string }
  | { type: "staged"; path?: string }
  | { type: "unstaged"; path?: string; expectedStageAllTargetRevision?: string }
  | { type: "compare"; base: string; head: string; path?: string };

export interface GitDiffResult {
  patch: string;
  path: string | null;
  additions: number;
  deletions: number;
  binary: boolean;
  truncated: boolean;
  /** Present for summary requests; callers should lazily request the selected path's patch. */
  files: GitFileChange[];
  stageAllTargetRevision?: string;
  candidateTreeOid?: string;
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

export interface GitHistoryRequest {
  limit?: number;
  cursor?: string;
  branch?: string;
  path?: string;
}

export interface GitCommit {
  oid: string;
  shortOid: string;
  subject: string;
  body?: string;
  authorName: string;
  authorEmail?: string;
  authoredAt: string;
  committedAt?: string;
  parents: string[];
  refs?: string[];
}

export interface GitHistoryResult {
  commits: GitCommit[];
  nextCursor: string | null;
}

export type GitAction =
  | { type: "stage"; paths: string[] }
  | {
      type: "stage_all";
      expectedContentRevision: string;
      expectedTargetRevision: string;
    }
  | { type: "unstage"; paths: string[] }
  | { type: "unstage_all"; expectedContentRevision: string }
  | {
      type: "discard";
      paths: string[];
      includeUntracked?: boolean;
      expectedContentRevision: string;
      expectedTargetRevision: string;
    }
  | {
      type: "commit";
      message: string;
      expectedTargetRevision: string;
      expectedTreeOid: string;
      amend?: boolean;
    }
  | {
      type: "fetch";
      expectedRepositoryId: string;
      expectedWorktreeId: string;
      remote: GitRemote;
    }
  | {
      type: "pull";
      expectedRepositoryId: string;
      expectedWorktreeId: string;
      expectedLocalBranch: string;
      expectedHead: string;
      expectedContentRevision: string;
      upstream: GitUpstream;
      rebase?: boolean;
      ffOnly?: boolean;
    }
  | {
      type: "push";
      expectedRepositoryId: string;
      expectedWorktreeId: string;
      remote: GitRemote;
      expectedLocalBranch: string;
      remoteBranch: string;
      expectedHead: string;
      expectedUpstream: GitUpstream | null;
      setUpstream?: boolean;
      forceWithLease?: boolean;
    }
  | { type: "checkout"; branch: string }
  | { type: "create_branch"; name: string; startPoint?: string; checkout?: boolean }
  | {
      type: "delete_branch";
      name: string;
      force?: boolean;
      expectedHead: string;
      expectedOid: string;
    }
  | {
      type: "merge";
      branch: string;
      expectedHead: string;
      expectedBranchOid: string;
    }
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
    }
  | { type: "stash"; message?: string; includeUntracked?: boolean }
  | { type: "stash_pop"; index?: number };

export interface GitActionResult {
  snapshot: GitWorkspaceSnapshot | null;
  message?: string;
  committedOid?: string;
}

export interface GitDiscardPreparation {
  snapshot: GitWorkspaceSnapshot;
  targetRevision: string;
}

export interface GitStageAllPreparation {
  snapshot: GitWorkspaceSnapshot;
  targetRevision: string;
  candidateTreeOid: string;
}

export interface GitCommitPreparation {
  snapshot: GitWorkspaceSnapshot;
  targetRevision: string;
  candidateTreeOid: string;
  messageDigest: string;
}

export interface GitHubRepository {
  host: string;
  owner: string;
  name: string;
  nameWithOwner: string;
  url: string;
  defaultBranch: string | null;
  viewerLogin: string | null;
  authenticated: boolean;
  ghVersion: string;
}

export type GitHubPullRequestState = "open" | "closed" | "merged";

export interface GitHubPullRequest {
  number: number;
  title: string;
  state: GitHubPullRequestState;
  url: string;
  author: string | null;
  headRefName: string;
  baseRefName: string;
  draft: boolean;
  mergeable?: boolean | null;
  updatedAt: string;
}

export interface GitHubPullRequestDetail extends GitHubPullRequest {
  headRefOid: string;
  body: string;
  additions: number;
  deletions: number;
  changedFiles: number;
  commits: number;
  reviewDecision?: string | null;
  statusCheckRollup?: string | null;
  checks?: GitHubPullRequestCheck[];
}

export type GitHubPullRequestCheckState =
  | "success"
  | "failure"
  | "pending"
  | "neutral"
  | "skipped"
  | "cancelled";

export interface GitHubPullRequestCheck {
  name: string;
  state: GitHubPullRequestCheckState;
  workflow?: string | null;
  description?: string | null;
  link?: string | null;
  startedAt?: string | null;
  completedAt?: string | null;
}

export type GitHubReadinessAvailability = "available" | "unsupported" | "error";

export interface GitHubReadinessPhase<T> {
  availability: GitHubReadinessAvailability;
  value: T | null;
  error: string | null;
}

export interface GitHubReadinessRepositoryIdentity {
  host: string;
  nodeId: string;
  nameWithOwner: string;
}

export interface GitHubPullRequestCoreIdentity {
  repository: GitHubReadinessRepositoryIdentity;
  pullRequestNodeId: string;
  number: number;
  state: string;
  draft: boolean;
  baseRepository: GitHubReadinessRepositoryIdentity;
  headRepository: GitHubReadinessRepositoryIdentity | null;
  baseRefName: string;
  baseRefOid: string;
  headRefName: string;
  headRefOid: string;
}

export interface GitHubPullRequestMergePolicy {
  mergeStateStatus: string;
  mergeable: string;
  mergeCommitAllowed: boolean;
  squashMergeAllowed: boolean;
  rebaseMergeAllowed: boolean;
}

export interface GitHubPullRequestViewer {
  login: string;
  canUpdate: boolean;
  canMergeAsAdmin: boolean;
}

export interface GitHubPullRequestReadinessCheck {
  nodeId: string;
  kind: string;
  name: string;
  state: string;
  conclusion: string | null;
  workflow: string | null;
  description: string | null;
  link: string | null;
  startedAt: string | null;
  completedAt: string | null;
  required: boolean;
}

export interface GitHubPullRequestChecks {
  totalCount: number;
  checks: GitHubPullRequestReadinessCheck[];
}

export interface GitHubPullRequestViewerDefault {
  mergeMethod: string;
}

export interface GitHubPullRequestAutoMerge {
  enabledAt: string | null;
  mergeMethod: string;
  commitHeadline: string | null;
  commitBody: string | null;
  enabledBy: string | null;
}

export interface GitHubPullRequestMergeQueueEntry {
  entryId: string;
  position: number;
  state: string;
  enqueuedAt: string;
  estimatedTimeToMerge: number | null;
}

export interface GitHubPullRequestMergeQueueState {
  enabled: boolean;
  isInQueue: boolean;
  entry: GitHubPullRequestMergeQueueEntry | null;
}

export interface GitHubPullRequestReadiness {
  identity: GitHubPullRequestCoreIdentity;
  mergePolicy: GitHubPullRequestMergePolicy;
  viewer: GitHubPullRequestViewer;
  checks: GitHubReadinessPhase<GitHubPullRequestChecks>;
  viewerDefault: GitHubReadinessPhase<GitHubPullRequestViewerDefault>;
  autoMerge: GitHubReadinessPhase<GitHubPullRequestAutoMerge>;
  mergeQueue: GitHubReadinessPhase<GitHubPullRequestMergeQueueState>;
  identityRevision: string;
  readinessRevision: string;
}

export interface GitHubPullRequestsResult {
  pullRequests: GitHubPullRequest[];
  page: number;
  pageSize: number;
  hasMore: boolean;
  nextPage: number | null;
}

export interface GitHubPullRequestsRequest {
  page: number;
  pageSize: number;
}

export interface GitHubPullRequestDiff extends GitDiffResult {
  headRefOid: string;
}

export type GitHubDiffSide = "LEFT" | "RIGHT";

export interface GitHubPullRequestReviewComment {
  id: string;
  author?: string | null;
  body: string;
  createdAt: string;
  updatedAt: string;
  url: string;
  replyToId?: string | null;
}

export interface GitHubPullRequestReviewThread {
  id: string;
  path: string;
  line?: number | null;
  startLine?: number | null;
  diffSide: GitHubDiffSide;
  startDiffSide?: GitHubDiffSide | null;
  originalLine?: number | null;
  originalStartLine?: number | null;
  isResolved: boolean;
  isOutdated: boolean;
  viewerCanReply: boolean;
  viewerCanResolve: boolean;
  viewerCanUnresolve: boolean;
  comments: GitHubPullRequestReviewComment[];
  commentsTotalCount: number;
  commentsNextCursor?: string | null;
}

export interface GitHubPullRequestReviewThreadsRequest {
  number: number;
  expectedHeadOid: string;
  cursor?: string;
  pageSize?: number;
}

export interface GitHubPullRequestReviewThreadsResult {
  number: number;
  headRefOid: string;
  threads: GitHubPullRequestReviewThread[];
  totalCount: number;
  nextCursor: string | null;
}

export interface GitHubPullRequestReviewThreadCommentsRequest {
  expectedRepository: GitHubExpectedRepository;
  expectedViewerLogin: string;
  number: number;
  expectedState: GitHubPullRequestState;
  expectedHeadOid: string;
  threadId: string;
  cursor: string;
  pageSize: number;
}

export interface GitHubPullRequestReviewThreadCommentsResult {
  number: number;
  headRefOid: string;
  threadId: string;
  comments: GitHubPullRequestReviewComment[];
  totalCount: number;
  nextCursor: string | null;
}

export interface GitHubPullRequestReviewDraftComment {
  path: string;
  line: number;
  side: GitHubDiffSide;
  body: string;
}

export type GitHubPullRequestReviewEvent = "comment" | "approve" | "request_changes";

export interface GitHubExpectedRepository {
  host: string;
  owner: string;
  name: string;
}

export interface GitHubActionIdentity {
  expectedRepository: GitHubExpectedRepository;
  expectedViewerLogin: string;
}

export type GitHubAction = GitHubActionIdentity & (
  | {
      type: "create_pull_request";
      title: string;
      body?: string;
      base?: string;
      head?: string;
      draft?: boolean;
      expectedLocalHeadOid: string;
      expectedContentRevision: string;
    }
  | {
      type: "checkout_pull_request";
      number: number;
      expectedHeadOid: string;
      expectedState: GitHubPullRequestState;
      expectedLocalHeadOid: string;
      expectedContentRevision: string;
    }
  | {
      type: "merge_pull_request";
      number: number;
      expectedHeadOid: string;
      expectedBaseOid: string;
      expectedState: GitHubPullRequestState;
      expectedIdentityRevision: string;
      expectedReadinessRevision: string;
      method: "merge" | "squash" | "rebase";
    }
  | {
      type: "close_pull_request";
      number: number;
      expectedHeadOid: string;
      expectedState: GitHubPullRequestState;
    }
  | {
      type: "reopen_pull_request";
      number: number;
      expectedHeadOid: string;
      expectedState: GitHubPullRequestState;
    }
  | {
      type: "submit_pull_request_review";
      number: number;
      expectedHeadOid: string;
      expectedState: GitHubPullRequestState;
      event: GitHubPullRequestReviewEvent;
      body?: string;
      comments: GitHubPullRequestReviewDraftComment[];
    }
  | {
      type: "reply_review_thread";
      number: number;
      expectedHeadOid: string;
      expectedState: GitHubPullRequestState;
      threadId: string;
      body: string;
    }
  | {
      type: "resolve_review_thread";
      number: number;
      expectedHeadOid: string;
      expectedState: GitHubPullRequestState;
      threadId: string;
    }
  | {
      type: "unresolve_review_thread";
      number: number;
      expectedHeadOid: string;
      expectedState: GitHubPullRequestState;
      threadId: string;
    }
);

export interface GitHubActionResult {
  repository: GitHubRepository | null;
  pullRequest?: GitHubPullRequestDetail | null;
  snapshot?: GitWorkspaceSnapshot | null;
  message?: string;
}

function requireGitRuntime(): void {
  if (!hasBackendRuntime()) throw new Error("Git 功能仅可在连接 Rust 后端时使用");
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

function normalizeHistoryResult(value: GitHistoryResult | GitCommit[]): GitHistoryResult {
  return Array.isArray(value) ? { commits: value, nextCursor: null } : value;
}

function normalizePullRequestsResult(
  value: GitHubPullRequestsResult | GitHubPullRequest[],
  request: GitHubPullRequestsRequest
): GitHubPullRequestsResult {
  if (Array.isArray(value)) {
    return {
      pullRequests: value,
      page: request.page,
      pageSize: request.pageSize,
      hasMore: false,
      nextPage: null
    };
  }
  const hasMore = value.hasMore ?? false;
  return {
    pullRequests: value.pullRequests,
    page: value.page ?? request.page,
    pageSize: value.pageSize ?? request.pageSize,
    hasMore,
    nextPage: value.nextPage ?? (hasMore ? request.page + 1 : null)
  };
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
 * targets resolve to the workspace root because worktrees belong to conversations.
 */
export type GitTarget =
  | { kind: "conversation"; conversationId: string }
  | { kind: "workspace"; workspaceId: string };

export function gitConversationTarget(conversationId: string): GitTarget {
  return { kind: "conversation", conversationId };
}

export function gitWorkspaceTarget(workspaceId: string): GitTarget {
  return { kind: "workspace", workspaceId };
}

/** Stable string suitable for React keys, cache keys, and dependency arrays. */
export function gitTargetKey(target: GitTarget): string {
  return target.kind === "conversation"
    ? `conversation:${target.conversationId}`
    : `workspace:${target.workspaceId}`;
}

export async function getGitWorkspaceSnapshot(
  target: GitTarget
): Promise<GitWorkspaceSnapshot | null> {
  requireGitRuntime();
  return invoke<GitWorkspaceSnapshot | null>("get_git_workspace_snapshot", { target });
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
 * Create an isolated worktree as the conversation's trusted working directory.
 *
 * `fromBranch` is the baseline branch, defaulting to the workspace HEAD. Persist
 * the returned record to `Conversation.worktree` so future host resolution uses it.
 */
export async function createConversationWorktree(
  conversationId: string,
  fromBranch?: string
): Promise<ConversationWorktree> {
  requireGitRuntime();
  return invoke<ConversationWorktree>("create_conversation_worktree", {
    conversationId,
    fromBranch: fromBranch ?? null
  });
}

/**
 * Release a conversation's isolated worktree.
 *
 * Returns `true` when the worktree and branch were deleted. Returns `false`
 * when uncommitted work or additional commits require retaining it on disk;
 * either result removes the conversation's association.
 */
export async function releaseConversationWorktree(conversationId: string): Promise<boolean> {
  requireGitRuntime();
  return invoke<boolean>("release_conversation_worktree", { conversationId });
}

export async function getGitHistory(
  target: GitTarget,
  request: GitHistoryRequest = {}
): Promise<GitHistoryResult> {
  requireGitRuntime();
  const value = await invoke<GitHistoryResult | GitCommit[]>("get_git_history", {
    target,
    request
  });
  return normalizeHistoryResult(value);
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

export async function prepareGitStageAll(
  target: GitTarget
): Promise<GitStageAllPreparation> {
  requireGitRuntime();
  return invoke<GitStageAllPreparation>("prepare_git_stage_all", { target });
}

export async function prepareGitCommit(
  target: GitTarget,
  message: string
): Promise<GitCommitPreparation> {
  requireGitRuntime();
  return invoke<GitCommitPreparation>("prepare_git_commit", { target, message });
}

export async function getGitHubRepository(
  target: GitTarget
): Promise<GitHubRepository | null> {
  requireGitRuntime();
  return invoke<GitHubRepository | null>("get_github_repository", { target });
}

export async function getGitHubPullRequests(
  target: GitTarget,
  request: GitHubPullRequestsRequest = { page: 1, pageSize: 30 }
): Promise<GitHubPullRequestsResult> {
  requireGitRuntime();
  const value = await invoke<GitHubPullRequestsResult | GitHubPullRequest[]>(
    "get_github_pull_requests",
    {
      target,
      page: request.page,
      pageSize: request.pageSize
    }
  );
  return normalizePullRequestsResult(value, request);
}

export async function getGitHubPullRequestDetail(
  target: GitTarget,
  number: number
): Promise<GitHubPullRequestDetail> {
  requireGitRuntime();
  return invoke<GitHubPullRequestDetail>("get_github_pull_request_detail", {
    target,
    number
  });
}

export async function getGitHubPullRequestReadiness(
  target: GitTarget,
  number: number
): Promise<GitHubPullRequestReadiness> {
  requireGitRuntime();
  return invoke<GitHubPullRequestReadiness>("get_github_pull_request_readiness", {
    target,
    number
  });
}

export async function getGitHubPullRequestDiff(
  target: GitTarget,
  number: number,
  path?: string
): Promise<GitHubPullRequestDiff> {
  requireGitRuntime();
  return invoke<GitHubPullRequestDiff>("get_github_pull_request_diff", {
    target,
    number,
    path
  });
}

export async function getGitHubPullRequestReviewThreads(
  target: GitTarget,
  request: GitHubPullRequestReviewThreadsRequest
): Promise<GitHubPullRequestReviewThreadsResult> {
  requireGitRuntime();
  return invoke<GitHubPullRequestReviewThreadsResult>(
    "get_github_pull_request_review_threads",
    { target, request }
  );
}

export async function getGitHubPullRequestReviewThreadComments(
  target: GitTarget,
  request: GitHubPullRequestReviewThreadCommentsRequest
): Promise<GitHubPullRequestReviewThreadCommentsResult> {
  requireGitRuntime();
  return invoke<GitHubPullRequestReviewThreadCommentsResult>(
    "get_github_pull_request_review_thread_comments",
    { target, request }
  );
}

export async function executeGitHubAction(
  target: GitTarget,
  action: GitHubAction
): Promise<GitHubActionResult> {
  requireGitRuntime();
  return invoke<GitHubActionResult>("execute_github_action", { target, action });
}
