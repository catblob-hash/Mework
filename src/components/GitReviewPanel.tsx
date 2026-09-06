import {
  Archive,
  ArchiveRestore,
  ArrowDownToLine,
  ArrowUpFromLine,
  Check,
  ChevronLeft,
  CircleAlert,
  ExternalLink,
  FileDiff,
  GitBranch,
  GitCommitHorizontal,
  GitCompareArrows,
  GitMerge,
  Github,
  History,
  LoaderCircle,
  Plus,
  RefreshCw,
  RotateCcw,
  Trash2
} from "lucide-react";
import { useCallback, useEffect, useId, useRef, useState } from "react";
import { useI18n } from "../i18n";
import { externalHttpUrl } from "../lib/externalLinks";
import {
  executeGitAction,
  executeGitHubAction,
  getGitBranches,
  getGitChangePage,
  getGitDiff,
  getGitHistory,
  getGitHubPullRequestDetail,
  getGitHubPullRequestDiff,
  getGitHubPullRequestReadiness,
  getGitHubPullRequestReviewThreadComments,
  getGitHubPullRequestReviewThreads,
  getGitHubPullRequests,
  getGitHubRepository,
  getGitWorkspaceSummary,
  gitFileHasStagedChange,
  gitFileHasUnstagedChange,
  gitTargetKey,
  prepareGitCommit,
  prepareGitDiscard,
  prepareGitStageAll,
  summaryToGitWorkspaceSnapshot,
  type GitAction,
  type GitBranch as GitBranchModel,
  type GitBranchesResult,
  type GitChangePageResult,
  type GitCommit,
  type GitDiffRequest,
  type GitDiffResult,
  type GitFileChange,
  type GitRemote,
  type GitUpstream,
  type GitHubAction,
  type GitHubActionIdentity,
  type GitHubPullRequestReviewDraftComment,
  type GitHubPullRequestReviewEvent,
  type GitHubPullRequestReviewThread,
  type GitHubPullRequest,
  type GitHubPullRequestCheckState,
  type GitHubPullRequestDetail,
  type GitHubPullRequestDiff,
  type GitHubPullRequestReadiness,
  type GitHubPullRequestReadinessCheck,
  type GitHubRepository,
  type GitRepositoryOperation,
  type GitReviewView,
  type GitTarget,
  type GitWorkspaceSnapshot
} from "../lib/git";
import { IconButton } from "./Common";
import {
  DiffOutput,
  type DiffLineSelection
} from "./DiffOutput";
import "./GitReviewPanel.css";

export type { GitReviewView };

export interface GitReviewPanelProps {
  target: GitTarget;
  snapshot: GitWorkspaceSnapshot;
  initialView?: GitReviewView;
  active: boolean;
  mutationDisabledReason?: string | null;
  onViewChange?: (view: GitReviewView) => void;
  onSnapshotChange?: (snapshot: GitWorkspaceSnapshot | null) => void;
  onMutationStart?: () => boolean;
  onMutationEnd?: () => void;
}

type Translate = ReturnType<typeof useI18n>["t"];
type RequestState = "idle" | "loading" | "ready" | "error";
type DiffScope = "working" | "staged" | "unstaged";
type PullRequestMergeMethod = "merge" | "squash" | "rebase";
type ChangeCompositionGroupId =
  | "conflicted"
  | "partiallyStaged"
  | "staged"
  | "unstaged"
  | "untracked";
const GITHUB_PULL_REQUEST_PAGE_SIZE = 30;
const GITHUB_REVIEW_THREAD_PAGE_SIZE = 30;
const GITHUB_REVIEW_THREAD_COMMENT_PAGE_SIZE = 50;
const CHANGE_FILE_BATCH_SIZE = 200;

interface PendingDiscard {
  key: string;
  scopeKey: string;
  snapshotKey: string;
  path: string;
  includeUntracked: boolean;
  contentRevision: string;
  targetRevision: string;
}

interface PendingStageAll {
  key: string;
  scopeKey: string;
  snapshotKey: string;
  snapshot: GitWorkspaceSnapshot;
  contentRevision: string;
  targetRevision: string;
  candidateTreeOid: string;
}

interface PendingCommit {
  scopeKey: string;
  snapshotKey: string;
  message: string;
  snapshot: GitWorkspaceSnapshot;
  targetRevision: string;
  candidateTreeOid: string;
  messageDigest: string;
}

interface PullRequestReview {
  detail: GitHubPullRequestDetail;
  diff: GitHubPullRequestDiff;
}

interface PullRequestLineDraft extends GitHubPullRequestReviewDraftComment {
  key: string;
  lineText: string;
  kind: DiffLineSelection["kind"];
}

interface PullRequestThreadCommentsPage {
  loading: boolean;
  error: string | null;
  nextCursor: string | null;
  seenCursors: string[];
}

const VIEWS: Array<{
  id: GitReviewView;
  icon: typeof FileDiff;
  label: (t: Translate) => string;
}> = [
  { id: "changes", icon: FileDiff, label: (t) => t("变更", "Changes") },
  { id: "history", icon: History, label: (t) => t("历史", "History") },
  { id: "branches", icon: GitBranch, label: (t) => t("分支", "Branches") },
  { id: "compare", icon: GitCompareArrows, label: (t) => t("比较", "Compare") },
  { id: "pullRequests", icon: Github, label: (t) => t("拉取请求", "Pull requests") }
];

function failureMessage(reason: unknown, fallback: string): string {
  if (reason instanceof Error && reason.message.trim()) return reason.message;
  if (typeof reason === "string" && reason.trim()) return reason;
  return fallback;
}

function repositoryOperationLabel(operation: GitRepositoryOperation, t: Translate): string {
  if (operation === "merge") return t("合并", "merge");
  if (operation === "rebase") return t("变基", "rebase");
  if (operation === "cherryPick") return t("拣选提交", "cherry-pick");
  if (operation === "revert") return t("还原提交", "revert");
  return t("二分查找", "bisect");
}

function operationSupportsContinue(operation: GitRepositoryOperation): boolean {
  return operation !== "bisect";
}

function operationSupportsSkip(operation: GitRepositoryOperation): boolean {
  return operation === "rebase" || operation === "cherryPick" || operation === "revert";
}

function gitActionChangesBranches(action: GitAction): boolean {
  return action.type === "checkout"
    || action.type === "create_branch"
    || action.type === "delete_branch"
    || action.type === "merge"
    || action.type === "commit"
    || action.type === "fetch"
    || action.type === "pull"
    || action.type === "push"
    || action.type === "continue_operation"
    || action.type === "skip_operation"
    || action.type === "abort_operation"
    || action.type === "bisect_step";
}

function gitActionChangesHistory(action: GitAction): boolean {
  return action.type === "checkout"
    || action.type === "create_branch"
    || action.type === "merge"
    || action.type === "commit"
    || action.type === "pull"
    || action.type === "continue_operation"
    || action.type === "skip_operation"
    || action.type === "abort_operation"
    || action.type === "bisect_step";
}

function githubActionKeepsOpenReview(action: GitHubAction): boolean {
  return action.type === "submit_pull_request_review"
    || action.type === "reply_review_thread"
    || action.type === "resolve_review_thread"
    || action.type === "unresolve_review_thread";
}

function changeStatusLabel(change: GitFileChange, t: Translate): string {
  if (change.conflicted || change.status === "unmerged") return t("冲突", "Conflict");
  if (change.submodule) return t("子模块", "Submodule");
  if (change.untracked || change.status === "untracked") return t("未跟踪", "Untracked");
  if (change.status === "added") return t("新增", "Added");
  if (change.status === "deleted") return t("删除", "Deleted");
  if (change.status === "renamed") return t("重命名", "Renamed");
  if (change.status === "copied") return t("复制", "Copied");
  if (change.status === "typeChanged") return t("类型", "Type");
  return t("修改", "Modified");
}

function changeTone(change: GitFileChange): string {
  if (change.conflicted || change.status === "unmerged") return "conflict";
  if (change.untracked || change.status === "untracked" || change.status === "added") return "addition";
  if (change.status === "deleted") return "deletion";
  return "modified";
}

function changeCompositionGroup(change: GitFileChange): ChangeCompositionGroupId {
  if (change.conflicted || change.status === "unmerged") return "conflicted";
  if (change.untracked || change.status === "untracked") return "untracked";
  const staged = gitFileHasStagedChange(change);
  const unstaged = gitFileHasUnstagedChange(change);
  if (staged && unstaged) return "partiallyStaged";
  if (staged) return "staged";
  return "unstaged";
}

function checkTone(state: GitHubPullRequestCheckState): "success" | "failure" | "pending" {
  if (state === "success" || state === "neutral" || state === "skipped") return "success";
  if (state === "failure" || state === "cancelled") return "failure";
  return "pending";
}

function checkStateLabel(state: GitHubPullRequestCheckState, t: Translate): string {
  if (state === "success") return t("通过", "Passed");
  if (state === "failure") return t("失败", "Failed");
  if (state === "neutral") return t("中立", "Neutral");
  if (state === "skipped") return t("已跳过", "Skipped");
  if (state === "cancelled") return t("已取消", "Cancelled");
  return t("进行中", "Pending");
}

function readinessCheckTone(
  check: GitHubPullRequestReadinessCheck
): "success" | "failure" | "pending" {
  const conclusion = check.conclusion?.trim().toUpperCase() ?? "";
  const state = check.state.trim().toUpperCase();
  if (
    conclusion === "SUCCESS"
    || conclusion === "NEUTRAL"
    || conclusion === "SKIPPED"
    || (!conclusion && (state === "SUCCESS" || state === "NEUTRAL" || state === "SKIPPED"))
  ) return "success";
  if (
    conclusion === "ACTION_REQUIRED"
    || conclusion === "CANCELLED"
    || conclusion === "FAILURE"
    || conclusion === "STALE"
    || conclusion === "STARTUP_FAILURE"
    || conclusion === "TIMED_OUT"
    || (!conclusion && (state === "ERROR" || state === "FAILURE"))
  ) return "failure";
  return "pending";
}

function readinessCheckStateLabel(
  check: GitHubPullRequestReadinessCheck,
  t: Translate
): string {
  const conclusion = check.conclusion?.trim().toUpperCase() ?? "";
  if (conclusion === "NEUTRAL") return t("中立", "Neutral");
  if (conclusion === "SKIPPED") return t("已跳过", "Skipped");
  if (conclusion === "CANCELLED") return t("已取消", "Cancelled");
  const tone = readinessCheckTone(check);
  if (tone === "success") return t("通过", "Passed");
  if (tone === "failure") return t("失败", "Failed");
  return t("进行中", "Pending");
}

function readinessCheckIsSuccessfulTerminal(
  check: GitHubPullRequestReadinessCheck
): boolean {
  const state = check.state.trim().toUpperCase();
  const conclusion = check.conclusion?.trim().toUpperCase() ?? "";
  if (check.kind.trim().toUpperCase() === "CHECKRUN") {
    return state === "COMPLETED"
      && (conclusion === "SUCCESS" || conclusion === "NEUTRAL" || conclusion === "SKIPPED");
  }
  return !conclusion
    && (state === "SUCCESS" || state === "NEUTRAL" || state === "SKIPPED");
}

function mergeStateTone(status: string): "success" | "failure" | "pending" {
  const normalized = status.trim().toUpperCase();
  if (normalized === "CLEAN" || normalized === "HAS_HOOKS") return "success";
  if (normalized === "BLOCKED" || normalized === "DIRTY") return "failure";
  return "pending";
}

function mergeStateLabel(status: string, t: Translate): string {
  const normalized = status.trim().toUpperCase();
  if (normalized === "CLEAN") return t("合并状态正常", "Merge state clean");
  if (normalized === "HAS_HOOKS") return t("等待服务端钩子", "Server hooks apply");
  if (normalized === "UNSTABLE") return t("非必需检查未通过", "Optional checks are not passing");
  if (normalized === "BEHIND") return t("分支落后于基线", "Branch is behind");
  if (normalized === "BLOCKED") return t("合并被规则阻止", "Merge is blocked");
  if (normalized === "DIRTY") return t("存在合并冲突", "Merge conflicts");
  if (normalized === "DRAFT") return t("仍为草稿", "Draft pull request");
  return t("合并状态未知", "Merge state unknown");
}

function allowedMergeMethods(
  readiness: GitHubPullRequestReadiness | null
): PullRequestMergeMethod[] {
  if (!readiness) return [];
  const methods: PullRequestMergeMethod[] = [];
  if (readiness.mergePolicy.mergeCommitAllowed) methods.push("merge");
  if (readiness.mergePolicy.squashMergeAllowed) methods.push("squash");
  if (readiness.mergePolicy.rebaseMergeAllowed) methods.push("rebase");
  return methods;
}

function normalizeMergeMethod(value: string | null | undefined): PullRequestMergeMethod | null {
  const normalized = value?.trim().toLowerCase();
  if (normalized === "merge" || normalized === "squash" || normalized === "rebase") {
    return normalized;
  }
  return null;
}

function mergeMethodLabel(method: PullRequestMergeMethod, t: Translate): string {
  if (method === "merge") return t("创建合并提交", "Create merge commit");
  if (method === "rebase") return t("变基合并", "Rebase and merge");
  return t("压缩合并", "Squash and merge");
}

function repositoryIdentityMatches(
  actual: { host: string; nameWithOwner: string },
  expected: GitHubRepository
): boolean {
  return actual.host.toLowerCase() === expected.host.toLowerCase()
    && actual.nameWithOwner.toLowerCase() === expected.nameWithOwner.toLowerCase();
}

function readinessMatchesPullRequest(
  readiness: GitHubPullRequestReadiness,
  repository: GitHubRepository,
  summary: GitHubPullRequest,
  detail?: GitHubPullRequestDetail | null
): boolean {
  const expected = detail ?? summary;
  return repositoryIdentityMatches(readiness.identity.repository, repository)
    && repositoryIdentityMatches(readiness.identity.baseRepository, repository)
    && (!repository.viewerLogin
      || readiness.viewer.login.toLowerCase() === repository.viewerLogin.toLowerCase())
    && readiness.identity.number === expected.number
    && readiness.identity.headRefName === expected.headRefName
    && readiness.identity.baseRefName === expected.baseRefName
    && readiness.identity.state.toLowerCase() === expected.state.toLowerCase()
    && readiness.identity.draft === expected.draft
    && (!detail || readiness.identity.headRefOid === detail.headRefOid);
}

function readinessSelectionKey(
  target: GitTarget,
  workspaceIdentityKey: string,
  repository: GitHubRepository,
  pullRequest: GitHubPullRequest
): string {
  return JSON.stringify([
    gitTargetKey(target),
    workspaceIdentityKey,
    repository.host.toLowerCase(),
    repository.nameWithOwner.toLowerCase(),
    pullRequest.number,
    pullRequest.headRefName,
    pullRequest.baseRefName,
    pullRequest.state,
    pullRequest.draft
  ]);
}

function shortOid(commit: Pick<GitCommit, "oid" | "shortOid">): string {
  return commit.shortOid || commit.oid.slice(0, 8);
}

function mergePullRequests(
  current: GitHubPullRequest[],
  incoming: GitHubPullRequest[]
): GitHubPullRequest[] {
  const merged = [...current];
  const indexes = new Map(merged.map((pullRequest, index) => [pullRequest.number, index]));
  for (const pullRequest of incoming) {
    const existing = indexes.get(pullRequest.number);
    if (existing === undefined) {
      indexes.set(pullRequest.number, merged.length);
      merged.push(pullRequest);
    } else {
      merged[existing] = pullRequest;
    }
  }
  return merged;
}

function mergeReviewThreads(
  current: GitHubPullRequestReviewThread[],
  incoming: GitHubPullRequestReviewThread[]
): GitHubPullRequestReviewThread[] {
  const merged = [...current];
  const indexes = new Map(merged.map((thread, index) => [thread.id, index]));
  for (const thread of incoming) {
    const existing = indexes.get(thread.id);
    if (existing === undefined) {
      indexes.set(thread.id, merged.length);
      merged.push(thread);
    } else {
      merged[existing] = thread;
    }
  }
  return merged;
}

function mergeReviewComments(
  current: GitHubPullRequestReviewThread["comments"],
  incoming: GitHubPullRequestReviewThread["comments"]
): GitHubPullRequestReviewThread["comments"] {
  const merged = [...current];
  const indexes = new Map(merged.map((comment, index) => [comment.id, index]));
  for (const comment of incoming) {
    const existing = indexes.get(comment.id);
    if (existing === undefined) {
      indexes.set(comment.id, merged.length);
      merged.push(comment);
    } else {
      merged[existing] = comment;
    }
  }
  return merged;
}

function lineDraftKey(selection: Pick<DiffLineSelection, "path" | "line" | "side">): string {
  return JSON.stringify([selection.path, selection.line, selection.side]);
}

function diffSnapshotFingerprint(snapshot: GitWorkspaceSnapshot): string {
  return JSON.stringify([
    snapshot.repositoryId,
    snapshot.worktreeId,
    snapshot.summaryRevision ?? null,
    snapshot.contentRevision,
    snapshot.branch,
    snapshot.head,
    snapshot.staged,
    snapshot.unstaged,
    snapshot.untracked,
    snapshot.conflicted,
    snapshot.operation,
    snapshot.operationRevision
  ]);
}

function gitRemoteProofKey(remote: GitRemote | null | undefined): string | null {
  if (!remote) return null;
  return JSON.stringify([
    remote.name,
    remote.fetchRevision,
    remote.pushRevision,
    remote.url
  ]);
}

function gitUpstreamProofKey(upstream: GitUpstream | null | undefined): string | null {
  if (!upstream) return null;
  return JSON.stringify([
    upstream.remoteName,
    upstream.remoteBranch,
    upstream.mergeRef,
    upstream.trackingRef,
    upstream.trackingOid,
    upstream.isLocal,
    gitRemoteProofKey(upstream.remote)
  ]);
}

function preferredRemoteName(snapshot: GitWorkspaceSnapshot): string {
  const upstream = snapshot.upstreamTarget;
  if (
    upstream
    && !upstream.isLocal
    && upstream.remoteName !== "."
    && snapshot.remotes.some((remote) => remote.name === upstream.remoteName)
  ) {
    return upstream.remoteName;
  }
  if (
    snapshot.remote
    && snapshot.remotes.some((remote) => remote.name === snapshot.remote?.name)
  ) {
    return snapshot.remote.name;
  }
  return snapshot.remotes[0]?.name ?? "";
}

function remoteByName(
  snapshot: GitWorkspaceSnapshot,
  remoteName: string
): GitRemote | null {
  return snapshot.remotes.find((remote) => remote.name === remoteName) ?? null;
}

function gitNetworkScopeKey(
  snapshot: GitWorkspaceSnapshot,
  selectedRemoteName: string
): string {
  return JSON.stringify([
    snapshot.repositoryId,
    snapshot.worktreeId,
    selectedRemoteName,
    snapshot.remotes.map((remote) => gitRemoteProofKey(remote)),
    gitRemoteProofKey(snapshot.remote),
    gitUpstreamProofKey(snapshot.upstreamTarget)
  ]);
}

function gitActionUsesRemoteProof(action: GitAction): boolean {
  return action.type === "fetch" || action.type === "pull" || action.type === "push";
}

function discardPreparationSnapshotKey(snapshot: GitWorkspaceSnapshot): string {
  return JSON.stringify([
    diffSnapshotFingerprint(snapshot),
    snapshot.repositoryId,
    snapshot.worktreeId,
    snapshot.repositoryRoot ?? null,
    snapshot.worktreeRoot ?? null,
    snapshot.upstream,
    gitUpstreamProofKey(snapshot.upstreamTarget),
    snapshot.ahead,
    snapshot.behind,
    snapshot.additions,
    snapshot.deletions,
    snapshot.stash,
    gitRemoteProofKey(snapshot.remote),
    snapshot.remotes.map((remote) => gitRemoteProofKey(remote)),
    snapshot.gitVersion,
    snapshot.detached,
    snapshot.unborn,
    snapshot.isClean,
    snapshot.binaryFiles,
    snapshot.changedFiles ?? snapshot.files.length,
    snapshot.stageable ?? snapshot.unstaged,
    snapshot.unstageable ?? snapshot.staged,
    snapshot.warnings
  ]);
}

function snapshotHasInlineChanges(snapshot: GitWorkspaceSnapshot): boolean {
  return snapshot.filesComplete === true || !snapshot.summaryRevision;
}

function dateLabel(value: string, language: string): string {
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? value : date.toLocaleString(language, {
    year: "numeric",
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit"
  });
}

function DiffPreview({
  diff,
  loading,
  error,
  emptyLabel,
  selectedLine,
  onLineSelect
}: {
  diff: GitDiffResult | null;
  loading: boolean;
  error: string | null;
  emptyLabel: string;
  selectedLine?: DiffLineSelection | null;
  onLineSelect?: (selection: DiffLineSelection) => void;
}) {
  const { t } = useI18n();
  if (loading) {
    return <div className="git-review__empty" role="status"><LoaderCircle className="spin" size={18} />{t("正在读取差异", "Loading diff")}</div>;
  }
  if (error) return <div className="git-review__error" role="alert"><CircleAlert size={15} />{error}</div>;
  return (
    <>
      {diff?.truncated && (
        <div className="git-review__warning" role="alert">
          <CircleAlert size={15} />
          {t(
            "差异内容超过安全读取上限，当前审阅不完整；刷新或缩小范围后再执行合并。",
            "The diff exceeded the safe read limit, so this review is incomplete. Refresh or narrow the scope before merging."
          )}
        </div>
      )}
      {!diff?.patch ? (
        <div className="git-review__empty">{emptyLabel}</div>
      ) : diff.path || diff.files.length <= 1 ? (
        <DiffOutput
          value={diff.patch}
          path={diff.path ?? diff.files[0]?.path}
          selectedLine={selectedLine}
          onLineSelect={
            onLineSelect && (Boolean(diff.path) || diff.files.length === 1)
              ? onLineSelect
              : undefined
          }
        />
      ) : (
        <pre className="git-review__raw-diff" tabIndex={0} aria-label={t("多文件差异", "Multi-file diff")}>
          {diff.patch}
        </pre>
      )}
    </>
  );
}

function BusyLabel({ children }: { children: string }) {
  return <><LoaderCircle className="spin" size={12} />{children}</>;
}

function GitHubWriteIdentity({
  repository,
  headOid
}: {
  repository: GitHubRepository;
  headOid?: string | null;
}) {
  const { t } = useI18n();
  return (
    <div
      className="git-review__github-write-identity"
      role="group"
      aria-label={t("GitHub 写入身份", "GitHub write identity")}
    >
      <Github size={13} aria-hidden="true" />
      <span>
        <strong>{repository.host}/{repository.nameWithOwner}</strong>
        <small>
          @{repository.viewerLogin ?? t("未识别账号", "Unknown account")}
          {headOid && <>{" · "}{t("提交", "commit")} {headOid.slice(0, 8)}</>}
        </small>
      </span>
    </div>
  );
}

export function GitReviewPanel({
  target,
  snapshot,
  initialView = "changes",
  active,
  mutationDisabledReason = null,
  onViewChange = () => undefined,
  onSnapshotChange = () => undefined,
  onMutationStart = () => true,
  onMutationEnd = () => undefined
}: GitReviewPanelProps) {
  const { t, resolvedLanguage } = useI18n();
  const tabsId = useId();
  const [view, setView] = useState<GitReviewView>(initialView);
  const [currentSnapshot, setCurrentSnapshot] = useState(snapshot);
  const [selectedRemoteName, setSelectedRemoteName] = useState(
    () => preferredRemoteName(snapshot)
  );
  const [busyAction, setBusyAction] = useState<string | null>(null);
  const [operationError, setOperationError] = useState<string | null>(null);
  const [operationMessage, setOperationMessage] = useState<string | null>(null);
  const [selectedPath, setSelectedPath] = useState<string | null>(snapshot.files[0]?.path ?? null);
  const [selectedChangeFile, setSelectedChangeFile] = useState<GitFileChange | null>(
    snapshotHasInlineChanges(snapshot) ? snapshot.files[0] ?? null : null
  );
  const [changeFilter, setChangeFilter] = useState("");
  const [changeFiles, setChangeFiles] = useState<GitFileChange[]>(
    snapshotHasInlineChanges(snapshot) ? snapshot.files : []
  );
  const [changeMatchedCount, setChangeMatchedCount] = useState(
    snapshotHasInlineChanges(snapshot)
      ? snapshot.files.length
      : snapshot.changedFiles ?? 0
  );
  const [changeNextCursor, setChangeNextCursor] = useState<string | null>(null);
  const [changePageState, setChangePageState] = useState<RequestState>(
    snapshotHasInlineChanges(snapshot) ? "ready" : "idle"
  );
  const [changePageError, setChangePageError] = useState<string | null>(null);
  const [changeLoadingMore, setChangeLoadingMore] = useState(false);
  const [changePageStageAllProof, setChangePageStageAllProof] = useState<{
    targetRevision: string;
    candidateTreeOid: string;
  } | null>(null);
  const [diffScope, setDiffScope] = useState<DiffScope>("working");
  const [diff, setDiff] = useState<GitDiffResult | null>(null);
  const [diffState, setDiffState] = useState<RequestState>("idle");
  const [diffError, setDiffError] = useState<string | null>(null);
  const [diffRevision, setDiffRevision] = useState(0);
  const [pendingDiscard, setPendingDiscard] = useState<PendingDiscard | null>(null);
  const [pendingStageAll, setPendingStageAll] = useState<PendingStageAll | null>(null);
  const [commitMessage, setCommitMessage] = useState("");
  const [pendingCommit, setPendingCommit] = useState<PendingCommit | null>(null);
  const [branches, setBranches] = useState<GitBranchesResult | null>(null);
  const [branchesState, setBranchesState] = useState<RequestState>("idle");
  const [branchesError, setBranchesError] = useState<string | null>(null);
  const [newBranchName, setNewBranchName] = useState("");
  const [pendingGitAction, setPendingGitAction] = useState<string | null>(null);
  const [history, setHistory] = useState<GitCommit[]>([]);
  const [historyState, setHistoryState] = useState<RequestState>("idle");
  const [historyError, setHistoryError] = useState<string | null>(null);
  const [historyNextCursor, setHistoryNextCursor] = useState<string | null>(null);
  const [historyLoadingMore, setHistoryLoadingMore] = useState(false);
  const [compareBase, setCompareBase] = useState(snapshot.upstream ?? "");
  const [compareHead, setCompareHead] = useState(snapshot.branch ?? "HEAD");
  const [compareDiff, setCompareDiff] = useState<GitDiffResult | null>(null);
  const [compareSelectedDiff, setCompareSelectedDiff] = useState<GitDiffResult | null>(null);
  const [compareState, setCompareState] = useState<RequestState>("idle");
  const [compareError, setCompareError] = useState<string | null>(null);
  const [selectedComparePath, setSelectedComparePath] = useState<string | null>(null);
  const [githubRepository, setGithubRepository] = useState<GitHubRepository | null>(null);
  const [pullRequests, setPullRequests] = useState<GitHubPullRequest[]>([]);
  const [pullRequestNextPage, setPullRequestNextPage] = useState<number | null>(null);
  const [pullRequestsLoadingMore, setPullRequestsLoadingMore] = useState(false);
  const [githubState, setGithubState] = useState<RequestState>("idle");
  const [githubError, setGithubError] = useState<string | null>(null);
  const [selectedPullRequest, setSelectedPullRequest] = useState<GitHubPullRequest | null>(null);
  const [pullRequestReview, setPullRequestReview] = useState<PullRequestReview | null>(null);
  const [pullRequestState, setPullRequestState] = useState<RequestState>("idle");
  const [pullRequestReadiness, setPullRequestReadiness] =
    useState<GitHubPullRequestReadiness | null>(null);
  const [pullRequestReadinessState, setPullRequestReadinessState] =
    useState<RequestState>("idle");
  const [pullRequestReadinessError, setPullRequestReadinessError] =
    useState<string | null>(null);
  const [pullRequestMergeMethod, setPullRequestMergeMethod] =
    useState<PullRequestMergeMethod>("squash");
  const [pullRequestSelectedPath, setPullRequestSelectedPath] = useState<string | null>(null);
  const [pullRequestSelectedDiff, setPullRequestSelectedDiff] = useState<GitHubPullRequestDiff | null>(null);
  const [pullRequestDiffState, setPullRequestDiffState] = useState<RequestState>("idle");
  const [pullRequestDiffError, setPullRequestDiffError] = useState<string | null>(null);
  const [pullRequestThreads, setPullRequestThreads] = useState<GitHubPullRequestReviewThread[]>([]);
  const [pullRequestThreadsHeadOid, setPullRequestThreadsHeadOid] = useState<string | null>(null);
  const [pullRequestThreadsTotalCount, setPullRequestThreadsTotalCount] = useState(0);
  const [pullRequestThreadsNextCursor, setPullRequestThreadsNextCursor] = useState<string | null>(null);
  const [pullRequestThreadsState, setPullRequestThreadsState] = useState<RequestState>("idle");
  const [pullRequestThreadsError, setPullRequestThreadsError] = useState<string | null>(null);
  const [pullRequestThreadsLoadingMore, setPullRequestThreadsLoadingMore] = useState(false);
  const [pullRequestSelectedLine, setPullRequestSelectedLine] = useState<DiffLineSelection | null>(null);
  const [pullRequestLineComment, setPullRequestLineComment] = useState("");
  const [pullRequestLineDrafts, setPullRequestLineDrafts] = useState<PullRequestLineDraft[]>([]);
  const [pullRequestReviewBody, setPullRequestReviewBody] = useState("");
  const [pullRequestReviewEvent, setPullRequestReviewEvent] = useState<GitHubPullRequestReviewEvent>("comment");
  const [pullRequestThreadReplies, setPullRequestThreadReplies] = useState<Record<string, string>>({});
  const [pullRequestThreadCommentsPages, setPullRequestThreadCommentsPages] = useState<
    Record<string, PullRequestThreadCommentsPage>
  >({});
  const [createPullRequestOpen, setCreatePullRequestOpen] = useState(false);
  const [pullRequestTitle, setPullRequestTitle] = useState("");
  const [pullRequestBody, setPullRequestBody] = useState("");
  const [pullRequestBase, setPullRequestBase] = useState("");
  const [pullRequestDraft, setPullRequestDraft] = useState(false);
  const [pendingGitHubAction, setPendingGitHubAction] = useState<string | null>(null);
  const changeDiffRequestRef = useRef(0);
  const changePageRequestRef = useRef(0);
  const selectedPathRef = useRef(selectedPath);
  selectedPathRef.current = selectedPath;
  const branchesRequestRef = useRef(0);
  const historyRequestRef = useRef(0);
  const githubRequestRef = useRef(0);
  const compareRequestRef = useRef(0);
  const pullRequestRef = useRef(0);
  const pullRequestReadinessRef = useRef(0);
  const pullRequestDiffRef = useRef(0);
  const pullRequestThreadsRef = useRef(0);
  const pullRequestThreadCommentsScopeRef = useRef(0);
  const pullRequestThreadCommentsRequestsRef = useRef<Record<string, number>>({});
  const discardPreparationRequestRef = useRef(0);
  const stageAllPreparationRequestRef = useRef(0);
  const commitPreparationRequestRef = useRef(0);
  const gitActionRequestRef = useRef(0);
  const selectedPullRequestRef = useRef(selectedPullRequest);
  selectedPullRequestRef.current = selectedPullRequest;
  const pullRequestReviewRef = useRef(pullRequestReview);
  pullRequestReviewRef.current = pullRequestReview;
  const githubRepositoryRef = useRef(githubRepository);
  githubRepositoryRef.current = githubRepository;
  const workspaceIdentityKey = JSON.stringify([snapshot.repositoryId, snapshot.worktreeId]);
  const workspaceIdentityKeyRef = useRef(workspaceIdentityKey);
  workspaceIdentityKeyRef.current = workspaceIdentityKey;
  const incomingNetworkScopeKey = gitNetworkScopeKey(snapshot, selectedRemoteName);
  const networkScopeKeyRef = useRef(incomingNetworkScopeKey);
  networkScopeKeyRef.current = incomingNetworkScopeKey;
  const diffSnapshotFingerprintRef = useRef(diffSnapshotFingerprint(snapshot));
  const discardSnapshotKey = discardPreparationSnapshotKey(snapshot);
  const stageAllSnapshotKey = discardPreparationSnapshotKey(snapshot);
  const acceptedDiscardPreparationSnapshotKeyRef = useRef<string | null>(null);
  const acceptedStageAllPreparationSnapshotKeyRef = useRef<string | null>(null);
  const discardPreparationSnapshotKeyRef = useRef(discardSnapshotKey);
  const stageAllPreparationSnapshotKeyRef = useRef(stageAllSnapshotKey);
  const committedDiscardSnapshotKeyRef = useRef(discardSnapshotKey);
  const committedStageAllSnapshotKeyRef = useRef(stageAllSnapshotKey);
  const observedDiscardSnapshotRef = useRef(snapshot);
  const observedStageAllSnapshotRef = useRef(snapshot);
  const discardScopeKey = JSON.stringify([
    active,
    target,
    snapshot.repositoryId,
    snapshot.worktreeId,
    snapshot.repositoryRoot ?? null,
    snapshot.worktreeRoot ?? null,
    mutationDisabledReason
  ]);
  const discardPreparationScopeRef = useRef(discardScopeKey);
  discardPreparationScopeRef.current = discardScopeKey;
  if (observedDiscardSnapshotRef.current !== snapshot) {
    observedDiscardSnapshotRef.current = snapshot;
    if (discardPreparationSnapshotKeyRef.current !== discardSnapshotKey) {
      if (acceptedDiscardPreparationSnapshotKeyRef.current !== discardSnapshotKey) {
        discardPreparationRequestRef.current += 1;
      }
      discardPreparationSnapshotKeyRef.current = discardSnapshotKey;
    }
  }
  const stageAllScopeKey = discardScopeKey;
  const stageAllPreparationScopeRef = useRef(stageAllScopeKey);
  const observedStageAllScopeRef = useRef(stageAllScopeKey);
  if (observedStageAllScopeRef.current !== stageAllScopeKey) {
    observedStageAllScopeRef.current = stageAllScopeKey;
    stageAllPreparationRequestRef.current += 1;
  }
  stageAllPreparationScopeRef.current = stageAllScopeKey;
  if (observedStageAllSnapshotRef.current !== snapshot) {
    observedStageAllSnapshotRef.current = snapshot;
    if (stageAllPreparationSnapshotKeyRef.current !== stageAllSnapshotKey) {
      if (acceptedStageAllPreparationSnapshotKeyRef.current !== stageAllSnapshotKey) {
        stageAllPreparationRequestRef.current += 1;
      }
      stageAllPreparationSnapshotKeyRef.current = stageAllSnapshotKey;
    }
  }
  const commitScopeKey = discardScopeKey;
  const commitSnapshotKey = discardPreparationSnapshotKey(currentSnapshot);
  const commitPreparationScopeRef = useRef(commitScopeKey);
  commitPreparationScopeRef.current = commitScopeKey;
  const commitPreparationSnapshotKeyRef = useRef(commitSnapshotKey);
  commitPreparationSnapshotKeyRef.current = commitSnapshotKey;
  const acceptedCommitPreparationSnapshotKeyRef = useRef<string | null>(null);
  const commitMessageRef = useRef(commitMessage);
  commitMessageRef.current = commitMessage;
  const githubScopeKey = JSON.stringify([
    target,
    snapshot.repositoryId,
    snapshot.worktreeId,
    snapshot.repositoryRoot ?? null,
    snapshot.worktreeRoot ?? null,
    gitRemoteProofKey(snapshot.remote),
    snapshot.remotes.map((remote) => gitRemoteProofKey(remote)),
    gitUpstreamProofKey(snapshot.upstreamTarget)
  ]);
  const normalizedChangeFilter = changeFilter.trim().toLowerCase();
  const changesAreInline = snapshotHasInlineChanges(currentSnapshot);
  const changePageRevision = currentSnapshot.summaryRevision ?? currentSnapshot.contentRevision;
  const stageAllReviewTargetRevision = pendingStageAll?.targetRevision ?? null;
  const stageAllReviewCandidateTreeOid = pendingStageAll?.candidateTreeOid ?? null;
  const changePageScopeKey = JSON.stringify([
    target,
    currentSnapshot.repositoryId,
    currentSnapshot.worktreeId,
    currentSnapshot.repositoryRoot ?? null,
    currentSnapshot.worktreeRoot ?? null,
    changePageRevision,
    normalizedChangeFilter,
    changesAreInline,
    stageAllReviewTargetRevision,
    stageAllReviewCandidateTreeOid
  ]);
  const changePageScopeRef = useRef(changePageScopeKey);
  changePageScopeRef.current = changePageScopeKey;

  const invalidatePullRequestThreadCommentRequests = useCallback(() => {
    pullRequestThreadCommentsScopeRef.current += 1;
    pullRequestThreadCommentsRequestsRef.current = {};
    setPullRequestThreadCommentsPages({});
  }, []);

  const clearComparison = useCallback(() => {
    compareRequestRef.current += 1;
    setCompareDiff(null);
    setCompareSelectedDiff(null);
    setCompareState("idle");
    setCompareError(null);
    setSelectedComparePath(null);
  }, []);

  const invalidateRepositoryReads = useCallback((
    branchesChanged: boolean,
    historyChanged: boolean
  ) => {
    changeDiffRequestRef.current += 1;
    setDiff(null);
    setDiffState("idle");
    setDiffError(null);
    if (branchesChanged) {
      branchesRequestRef.current += 1;
      setBranchesState("idle");
      setBranchesError(null);
      clearComparison();
    }
    if (historyChanged) {
      historyRequestRef.current += 1;
      setHistoryState("idle");
      setHistoryLoadingMore(false);
      setHistoryError(null);
    }
  }, [clearComparison]);

  useEffect(() => () => {
    gitActionRequestRef.current += 1;
  }, []);

  useEffect(() => {
    setView(initialView);
  }, [initialView]);

  const incomingRemoteNamesKey = JSON.stringify(snapshot.remotes.map((remote) => remote.name));

  useEffect(() => {
    setSelectedRemoteName((current) => (
      current && snapshot.remotes.some((remote) => remote.name === current)
        ? current
        : preferredRemoteName(snapshot)
    ));
  }, [incomingRemoteNamesKey, snapshot]);

  useEffect(() => {
    setPendingGitAction((current) => (
      current?.startsWith("pull:") || current?.startsWith("push:")
        ? null
        : current
    ));
  }, [incomingNetworkScopeKey]);

  useEffect(() => {
    githubRequestRef.current += 1;
    pullRequestRef.current += 1;
    pullRequestReadinessRef.current += 1;
    pullRequestDiffRef.current += 1;
    pullRequestThreadsRef.current += 1;
    invalidatePullRequestThreadCommentRequests();
    setGithubRepository(null);
    setPullRequests([]);
    setPullRequestNextPage(null);
    setPullRequestsLoadingMore(false);
    setGithubState("idle");
    setGithubError(null);
    setSelectedPullRequest(null);
    setPullRequestReview(null);
    setPullRequestState("idle");
    setPullRequestReadiness(null);
    setPullRequestReadinessState("idle");
    setPullRequestReadinessError(null);
    setPullRequestMergeMethod("squash");
    setPullRequestSelectedPath(null);
    setPullRequestSelectedDiff(null);
    setPullRequestDiffState("idle");
    setPullRequestDiffError(null);
    setPullRequestThreads([]);
    setPullRequestThreadsHeadOid(null);
    setPullRequestThreadsTotalCount(0);
    setPullRequestThreadsNextCursor(null);
    setPullRequestThreadsState("idle");
    setPullRequestThreadsError(null);
    setPullRequestThreadsLoadingMore(false);
    setPullRequestSelectedLine(null);
    setPullRequestLineComment("");
    setPullRequestLineDrafts([]);
    setPullRequestReviewBody("");
    setPullRequestReviewEvent("comment");
    setPullRequestThreadReplies({});
    setCreatePullRequestOpen(false);
    setPullRequestTitle("");
    setPullRequestBody("");
    setPullRequestBase("");
    setPullRequestDraft(false);
    setPendingGitHubAction(null);
  }, [githubScopeKey, invalidatePullRequestThreadCommentRequests]);

  useEffect(() => {
    discardPreparationRequestRef.current += 1;
    acceptedDiscardPreparationSnapshotKeyRef.current = null;
    setPendingDiscard(null);
    setBusyAction((current) => (
      current?.startsWith("prepare-discard:") ? null : current
    ));
  }, [discardScopeKey]);

  useEffect(() => {
    if (committedDiscardSnapshotKeyRef.current === discardSnapshotKey) return;
    committedDiscardSnapshotKeyRef.current = discardSnapshotKey;
    if (acceptedDiscardPreparationSnapshotKeyRef.current === discardSnapshotKey) {
      acceptedDiscardPreparationSnapshotKeyRef.current = null;
      return;
    }
    acceptedDiscardPreparationSnapshotKeyRef.current = null;
    setPendingDiscard(null);
    setBusyAction((current) => (
      current?.startsWith("prepare-discard:") ? null : current
    ));
  }, [discardSnapshotKey]);

  useEffect(() => {
    stageAllPreparationRequestRef.current += 1;
    acceptedStageAllPreparationSnapshotKeyRef.current = null;
    setPendingStageAll(null);
    setChangePageStageAllProof(null);
    setBusyAction((current) => (
      current === "prepare-stage-all" ? null : current
    ));
  }, [stageAllScopeKey]);

  useEffect(() => {
    if (committedStageAllSnapshotKeyRef.current === stageAllSnapshotKey) return;
    committedStageAllSnapshotKeyRef.current = stageAllSnapshotKey;
    if (acceptedStageAllPreparationSnapshotKeyRef.current === stageAllSnapshotKey) {
      acceptedStageAllPreparationSnapshotKeyRef.current = null;
      return;
    }
    acceptedStageAllPreparationSnapshotKeyRef.current = null;
    setPendingStageAll(null);
    setChangePageStageAllProof(null);
    setBusyAction((current) => (
      current === "prepare-stage-all" ? null : current
    ));
  }, [stageAllSnapshotKey]);

  useEffect(() => {
    commitPreparationRequestRef.current += 1;
    acceptedCommitPreparationSnapshotKeyRef.current = null;
    setPendingCommit(null);
    setBusyAction((current) => current?.startsWith("prepare-commit:") ? null : current);
  }, [commitScopeKey]);

  useEffect(() => {
    if (acceptedCommitPreparationSnapshotKeyRef.current === commitSnapshotKey) {
      acceptedCommitPreparationSnapshotKeyRef.current = null;
      return;
    }
    commitPreparationRequestRef.current += 1;
    setPendingCommit(null);
    setBusyAction((current) => current?.startsWith("prepare-commit:") ? null : current);
  }, [commitSnapshotKey]);

  useEffect(() => {
    const nextDiffFingerprint = diffSnapshotFingerprint(snapshot);
    const diffChanged = diffSnapshotFingerprintRef.current !== nextDiffFingerprint;
    diffSnapshotFingerprintRef.current = nextDiffFingerprint;
    setCurrentSnapshot(snapshot);
    setCompareHead((current) => current || snapshot.branch || "HEAD");
    setCompareBase((current) => current || snapshot.upstream || "");
    if (diffChanged) setDiffRevision((revision) => revision + 1);
  }, [snapshot]);

  useEffect(() => {
    // The same filesystem path can be replaced with another repository, and a
    // linked worktree shares repository metadata while having different HEAD
    // and index state. Never let either reuse an old page, patch, or armed
    // write confirmation.
    changeDiffRequestRef.current += 1;
    changePageRequestRef.current += 1;
    branchesRequestRef.current += 1;
    historyRequestRef.current += 1;
    githubRequestRef.current += 1;
    compareRequestRef.current += 1;
    pullRequestRef.current += 1;
    pullRequestReadinessRef.current += 1;
    pullRequestDiffRef.current += 1;
    pullRequestThreadsRef.current += 1;
    invalidatePullRequestThreadCommentRequests();
    setChangeFilter("");
    setChangeFiles([]);
    setChangeNextCursor(null);
    setChangeLoadingMore(false);
    setChangePageState("idle");
    setChangePageError(null);
    setChangePageStageAllProof(null);
    setSelectedPath(null);
    setSelectedChangeFile(null);
    setDiff(null);
    setDiffState("idle");
    setDiffError(null);
    setBranches(null);
    setBranchesState("idle");
    setBranchesError(null);
    setHistory([]);
    setHistoryState("idle");
    setHistoryError(null);
    setHistoryNextCursor(null);
    setHistoryLoadingMore(false);
    setCompareDiff(null);
    setCompareSelectedDiff(null);
    setCompareState("idle");
    setCompareError(null);
    setSelectedComparePath(null);
    setGithubRepository(null);
    setPullRequests([]);
    setPullRequestNextPage(null);
    setPullRequestsLoadingMore(false);
    setGithubState("idle");
    setGithubError(null);
    setSelectedPullRequest(null);
    setPullRequestReview(null);
    setPullRequestState("idle");
    setPullRequestReadiness(null);
    setPullRequestReadinessState("idle");
    setPullRequestReadinessError(null);
    setPullRequestMergeMethod("squash");
    setPullRequestSelectedPath(null);
    setPullRequestSelectedDiff(null);
    setPullRequestDiffState("idle");
    setPullRequestDiffError(null);
    setPullRequestThreads([]);
    setPullRequestThreadsHeadOid(null);
    setPullRequestThreadsTotalCount(0);
    setPullRequestThreadsNextCursor(null);
    setPullRequestThreadsState("idle");
    setPullRequestThreadsError(null);
    setPullRequestThreadsLoadingMore(false);
    setPullRequestSelectedLine(null);
    setPullRequestLineComment("");
    setPullRequestLineDrafts([]);
    setPullRequestReviewBody("");
    setPullRequestReviewEvent("comment");
    setPullRequestThreadReplies({});
    setCreatePullRequestOpen(false);
    setPullRequestTitle("");
    setPullRequestBody("");
    setPullRequestBase("");
    setPullRequestDraft(false);
    setPendingGitAction(null);
    setPendingGitHubAction(null);
    setSelectedRemoteName(preferredRemoteName(snapshot));
  }, [invalidatePullRequestThreadCommentRequests, workspaceIdentityKey]);

  useEffect(() => {
    if (active) return;
    gitActionRequestRef.current += 1;
    changeDiffRequestRef.current += 1;
    changePageRequestRef.current += 1;
    branchesRequestRef.current += 1;
    historyRequestRef.current += 1;
    githubRequestRef.current += 1;
    compareRequestRef.current += 1;
    pullRequestRef.current += 1;
    pullRequestReadinessRef.current += 1;
    pullRequestDiffRef.current += 1;
    pullRequestThreadsRef.current += 1;
    invalidatePullRequestThreadCommentRequests();
    setDiffState((current) => current === "loading" ? "idle" : current);
    setChangePageState((current) => current === "loading" ? "idle" : current);
    setChangeLoadingMore(false);
    setBranchesState((current) => current === "loading" ? "idle" : current);
    setHistoryState((current) => current === "loading" ? "idle" : current);
    setHistoryLoadingMore(false);
    setPullRequestsLoadingMore(false);
    setGithubState((current) => current === "loading" ? "idle" : current);
    setCompareState((current) => current === "loading" ? "idle" : current);
    setPullRequestState((current) => current === "loading" ? "idle" : current);
    setPullRequestReadiness(null);
    setPullRequestReadinessState("idle");
    setPullRequestReadinessError(null);
    setPullRequestDiffState((current) => current === "loading" ? "idle" : current);
    setPullRequestThreadsState((current) => current === "loading" ? "idle" : current);
    setPullRequestThreadsLoadingMore(false);
    setPendingDiscard(null);
    setPendingStageAll(null);
    commitPreparationRequestRef.current += 1;
    acceptedCommitPreparationSnapshotKeyRef.current = null;
    setPendingCommit(null);
    setChangePageStageAllProof(null);
    setPendingGitAction(null);
    setPendingGitHubAction(null);
  }, [active, invalidatePullRequestThreadCommentRequests]);

  useEffect(() => {
    if (!mutationDisabledReason) return;
    setPendingDiscard(null);
    setPendingStageAll(null);
    commitPreparationRequestRef.current += 1;
    acceptedCommitPreparationSnapshotKeyRef.current = null;
    setPendingCommit(null);
    setChangePageStageAllProof(null);
    setPendingGitAction(null);
    setPendingGitHubAction(null);
  }, [mutationDisabledReason]);

  useEffect(() => {
    setPendingGitAction(null);
  }, [currentSnapshot.operation, currentSnapshot.operationRevision]);

  const publishSnapshot = useCallback((next: GitWorkspaceSnapshot | null) => {
    if (next) {
      diffSnapshotFingerprintRef.current = diffSnapshotFingerprint(next);
      setCurrentSnapshot(next);
    }
    onSnapshotChange(next);
  }, [onSnapshotChange]);

  const reconcileWorkspaceSummary = useCallback(async (
    knownRevision?: string,
    expectedNetworkScope?: string
  ) => {
    const requestedWorkspaceIdentity = workspaceIdentityKeyRef.current;
    const result = await getGitWorkspaceSummary(target, knownRevision);
    if (
      requestedWorkspaceIdentity !== workspaceIdentityKeyRef.current
      || (
        expectedNetworkScope !== undefined
        && expectedNetworkScope !== networkScopeKeyRef.current
      )
    ) {
      return { result, accepted: false };
    }
    if (result.kind === "notRepository") {
      publishSnapshot(null);
    } else if (result.kind === "snapshot") {
      publishSnapshot(summaryToGitWorkspaceSnapshot(result.summary));
    }
    return { result, accepted: true };
  }, [target, publishSnapshot]);

  const refreshSnapshot = useCallback(async () => {
    if (!active || busyAction) return;
    commitPreparationRequestRef.current += 1;
    acceptedCommitPreparationSnapshotKeyRef.current = null;
    setPendingCommit(null);
    setPendingStageAll(null);
    setChangePageStageAllProof(null);
    setBusyAction("refresh");
    setOperationError(null);
    setOperationMessage(null);
    try {
      await reconcileWorkspaceSummary(currentSnapshot.summaryRevision);
      setDiffRevision((revision) => revision + 1);
    } catch (reason) {
      setOperationError(failureMessage(reason, t("无法刷新 Git 状态", "Unable to refresh Git status")));
    } finally {
      setBusyAction(null);
    }
  }, [
    active,
    busyAction,
    currentSnapshot.summaryRevision,
    reconcileWorkspaceSummary,
    t
  ]);

  const cancelStageAllReview = useCallback(() => {
    stageAllPreparationRequestRef.current += 1;
    changePageRequestRef.current += 1;
    changeDiffRequestRef.current += 1;
    setPendingStageAll(null);
    setChangePageStageAllProof(null);
    setDiff(null);
    setDiffState("idle");
    setDiffError(null);
    setOperationError(null);
    setOperationMessage(t(
      "已取消全部暂存候选",
      "The stage-all candidate was cancelled"
    ));
  }, [t]);

  const loadChangePage = useCallback(async (cursor?: string) => {
    if (
      !active
      || busyAction
      || view !== "changes"
      || changesAreInline
      || !currentSnapshot.summaryRevision
    ) return;
    const requestId = ++changePageRequestRef.current;
    const requestScopeKey = changePageScopeKey;
    const requestedStageAllProof = pendingStageAll ? {
      key: pendingStageAll.key,
      targetRevision: pendingStageAll.targetRevision,
      candidateTreeOid: pendingStageAll.candidateTreeOid
    } : null;
    const loadingMore = Boolean(cursor);
    if (loadingMore) {
      setChangeLoadingMore(true);
    } else {
      setChangePageState("loading");
      setChangePageError(null);
    }
    try {
      const result: GitChangePageResult = await getGitChangePage(target, {
        expectedRevision: currentSnapshot.summaryRevision,
        ...(cursor ? { cursor } : {}),
        ...(normalizedChangeFilter ? { query: normalizedChangeFilter } : {}),
        limit: CHANGE_FILE_BATCH_SIZE,
        ...(selectedPath ? { selectedPath } : {}),
        ...(requestedStageAllProof ? {
          expectedStageAllTargetRevision: requestedStageAllProof.targetRevision
        } : {})
      });
      if (
        requestId !== changePageRequestRef.current
        || requestScopeKey !== changePageScopeRef.current
      ) return;
      if (result.kind === "stale") {
        if (requestedStageAllProof) {
          setPendingStageAll((current) => (
            current?.key === requestedStageAllProof.key ? null : current
          ));
          setChangePageStageAllProof(null);
        }
        setChangeFiles([]);
        setChangeMatchedCount(0);
        setChangeNextCursor(null);
        setSelectedChangeFile(null);
        setChangePageState("idle");
        publishSnapshot(summaryToGitWorkspaceSnapshot(result.summary));
        setDiffRevision((revision) => revision + 1);
        return;
      }
      if (result.revision !== currentSnapshot.summaryRevision) {
        throw new Error(t(
          "Git 变更页来自其他仓库修订；请刷新后重试",
          "The Git change page belongs to another repository revision. Refresh and try again."
        ));
      }
      if (requestedStageAllProof) {
        if (
          result.stageAllTargetRevision !== requestedStageAllProof.targetRevision
          || result.candidateTreeOid !== requestedStageAllProof.candidateTreeOid
        ) {
          throw new Error(t(
            "Git 变更页不属于当前全部暂存证明；请重新审阅",
            "The Git change page does not belong to the current stage-all proof. Review it again."
          ));
        }
        setChangePageStageAllProof({
          targetRevision: result.stageAllTargetRevision,
          candidateTreeOid: result.candidateTreeOid
        });
      } else {
        if (result.stageAllTargetRevision || result.candidateTreeOid) {
          throw new Error(t(
            "Git 变更页携带了意外的全部暂存证明",
            "The Git change page carried an unexpected stage-all proof."
          ));
        }
        setChangePageStageAllProof(null);
      }
      const nextFiles = cursor ? [...changeFiles, ...result.files] : result.files;
      setChangeFiles(nextFiles);
      setChangeMatchedCount(result.matchedCount);
      setChangeNextCursor(result.nextCursor);
      if (result.selection?.state === "present") {
        setSelectedPath(result.selection.file.path);
        setSelectedChangeFile(result.selection.file);
      } else if (result.selection?.state === "filteredOut") {
        // Keep the latent path so clearing the query can restore the user's
        // selection, but do not review a file hidden by the active filter.
        setSelectedChangeFile(null);
      } else if (result.selection?.state === "missing") {
        const fallback = nextFiles[0] ?? null;
        setSelectedPath(fallback?.path ?? null);
        setSelectedChangeFile(fallback);
      } else if (!selectedPath) {
        const fallback = nextFiles[0] ?? null;
        setSelectedPath(fallback?.path ?? null);
        setSelectedChangeFile(fallback);
      }
      setChangePageState("ready");
      setChangePageError(null);
    } catch (reason) {
      if (
        requestId !== changePageRequestRef.current
        || requestScopeKey !== changePageScopeRef.current
      ) return;
      const message = failureMessage(
        reason,
        t("无法读取 Git 变更文件", "Unable to load Git changes")
      );
      if (requestedStageAllProof) {
        setPendingStageAll((current) => (
          current?.key === requestedStageAllProof.key ? null : current
        ));
        setChangePageStageAllProof(null);
        setOperationError(t(
          "全部暂存证明已失效：{message}",
          "The stage-all proof is no longer valid: {message}",
          { message }
        ));
      }
      setChangePageError(message);
      setChangePageState("error");
    } finally {
      if (
        requestId === changePageRequestRef.current
        && requestScopeKey === changePageScopeRef.current
      ) {
        setChangeLoadingMore(false);
      }
    }
  }, [
    active,
    busyAction,
    changeFiles,
    changePageScopeKey,
    changesAreInline,
    target,
    currentSnapshot.summaryRevision,
    normalizedChangeFilter,
    pendingStageAll,
    publishSnapshot,
    selectedPath,
    t,
    view
  ]);

  useEffect(() => {
    changePageRequestRef.current += 1;
    setChangeNextCursor(null);
    setChangeLoadingMore(false);
    setChangePageError(null);
    setChangePageStageAllProof(null);
    if (changesAreInline) {
      const nextFiles = normalizedChangeFilter
        ? currentSnapshot.files.filter((file) => (
          file.path.toLowerCase().includes(normalizedChangeFilter)
          || file.originalPath?.toLowerCase().includes(normalizedChangeFilter)
        ))
        : currentSnapshot.files;
      setChangeFiles(nextFiles);
      setChangeMatchedCount(nextFiles.length);
      const selected = selectedPathRef.current
        ? nextFiles.find((file) => file.path === selectedPathRef.current) ?? null
        : null;
      const fallback = selected ?? nextFiles[0] ?? null;
      setSelectedPath(fallback?.path ?? null);
      setSelectedChangeFile(fallback);
      setChangePageState("ready");
      return;
    }
    setChangeFiles([]);
    setChangeMatchedCount(normalizedChangeFilter ? 0 : currentSnapshot.changedFiles ?? 0);
    setSelectedChangeFile(null);
    setChangePageState("idle");
  }, [
    changePageScopeKey,
    changesAreInline,
    currentSnapshot.changedFiles,
    currentSnapshot.files,
    normalizedChangeFilter
  ]);

  useEffect(() => {
    if (
      !active
      || busyAction
      || view !== "changes"
      || changesAreInline
      || changePageState !== "idle"
      || (currentSnapshot.changedFiles ?? 0) === 0
    ) return;
    void loadChangePage();
  }, [
    active,
    busyAction,
    changePageScopeKey,
    changePageState,
    changesAreInline,
    currentSnapshot.changedFiles,
    loadChangePage,
    view
  ]);

  const runGitAction = useCallback(async (action: GitAction, key: string) => {
    if (!active || busyAction || mutationDisabledReason) return false;
    if (!onMutationStart()) {
      setOperationError(t(
        "另一个任务或工作区操作已经开始，请稍后重试",
        "Another task or workspace operation has started. Try again shortly."
      ));
      return false;
    }
    const requestId = ++gitActionRequestRef.current;
    const actionWorkspaceIdentity = workspaceIdentityKeyRef.current;
    const actionNetworkScope = gitActionUsesRemoteProof(action)
      ? networkScopeKeyRef.current
      : undefined;
    const requestIsCurrent = () => (
      requestId === gitActionRequestRef.current
      && actionWorkspaceIdentity === workspaceIdentityKeyRef.current
      && (
        actionNetworkScope === undefined
        || actionNetworkScope === networkScopeKeyRef.current
      )
    );
    // Any repository mutation abandons the reviewed candidate first. This
    // prevents file-level actions (or a fetch/pull/checkout) from leaving a
    // proof-bound page mounted against state that the action may change.
    stageAllPreparationRequestRef.current += 1;
    setPendingStageAll(null);
    setChangePageStageAllProof(null);
    commitPreparationRequestRef.current += 1;
    acceptedCommitPreparationSnapshotKeyRef.current = null;
    setPendingCommit(null);
    invalidateRepositoryReads(
      gitActionChangesBranches(action),
      gitActionChangesHistory(action)
    );
    setBusyAction(key);
    setOperationError(null);
    setOperationMessage(null);
    try {
      const result = await executeGitAction(target, action);
      if (!requestIsCurrent()) return false;
      publishSnapshot(result.snapshot);
      setDiffRevision((revision) => revision + 1);
      if (action.type === "commit") setCommitMessage("");
      if (
        (action.type === "checkout" || action.type === "create_branch")
        && result.snapshot?.branch
      ) {
        clearComparison();
        setCompareHead(result.snapshot.branch);
      }
      if (gitActionChangesBranches(action)) setBranchesState("idle");
      if (gitActionChangesHistory(action)) setHistoryState("idle");
      if (!["stage", "unstage", "discard"].includes(action.type)) {
        setOperationMessage(
          result.message?.trim()
          || t("Git 操作已完成", "Git operation completed")
        );
      }
      return true;
    } catch (reason) {
      if (!requestIsCurrent()) return false;
      const message = failureMessage(reason, t("Git 操作失败", "Git operation failed"));
      try {
        const reconciliation = await reconcileWorkspaceSummary(
          undefined,
          actionNetworkScope
        );
        if (!reconciliation.accepted) return false;
        setDiffRevision((revision) => revision + 1);
      } catch {
        // Preserve the action failure: it is the primary error, while the regular
        // polling path can retry a snapshot that temporarily could not be read.
      }
      if (gitActionChangesBranches(action)) setBranchesState("idle");
      if (gitActionChangesHistory(action)) setHistoryState("idle");
      setOperationError(message);
      return false;
    } finally {
      if (requestId === gitActionRequestRef.current) {
        setBusyAction((current) => current === key ? null : current);
      }
      onMutationEnd();
    }
  }, [
    active,
    busyAction,
    clearComparison,
    target,
    invalidateRepositoryReads,
    mutationDisabledReason,
    onMutationEnd,
    onMutationStart,
    publishSnapshot,
    reconcileWorkspaceSummary,
    t
  ]);

  const prepareDiscardConfirmation = useCallback(async (
    path: string,
    includeUntracked: boolean
  ): Promise<PendingDiscard | null> => {
    if (!active || busyAction || mutationDisabledReason) return null;
    const requestId = ++discardPreparationRequestRef.current;
    const requestScopeKey = discardScopeKey;
    const requestSnapshotKey = discardPreparationSnapshotKeyRef.current;
    let acceptedSnapshotKey: string | null = null;
    const busyKey = `prepare-discard:${path}`;
    setBusyAction(busyKey);
    setOperationError(null);
    setOperationMessage(null);
    try {
      const preparation = await prepareGitDiscard(
        target,
        [path],
        includeUntracked
      );
      if (
        requestId !== discardPreparationRequestRef.current
        || requestScopeKey !== discardPreparationScopeRef.current
        || requestSnapshotKey !== discardPreparationSnapshotKeyRef.current
      ) return null;
      acceptedSnapshotKey = discardPreparationSnapshotKey(preparation.snapshot);
      acceptedDiscardPreparationSnapshotKeyRef.current = acceptedSnapshotKey;
      discardPreparationSnapshotKeyRef.current = acceptedSnapshotKey;
      publishSnapshot(preparation.snapshot);
      return {
        key: JSON.stringify([
          requestScopeKey,
          acceptedSnapshotKey,
          path,
          includeUntracked,
          preparation.snapshot.contentRevision,
          preparation.targetRevision
        ]),
        scopeKey: requestScopeKey,
        snapshotKey: acceptedSnapshotKey,
        path,
        includeUntracked,
        contentRevision: preparation.snapshot.contentRevision,
        targetRevision: preparation.targetRevision
      };
    } catch (reason) {
      if (
        requestId !== discardPreparationRequestRef.current
        || requestScopeKey !== discardPreparationScopeRef.current
        || (
          requestSnapshotKey !== discardPreparationSnapshotKeyRef.current
          && acceptedSnapshotKey !== discardPreparationSnapshotKeyRef.current
        )
      ) return null;
      setPendingDiscard(null);
      setOperationError(failureMessage(
        reason,
        t("无法准备安全丢弃", "Unable to prepare a safe discard")
      ));
      return null;
    } finally {
      if (
        requestId === discardPreparationRequestRef.current
        && requestScopeKey === discardPreparationScopeRef.current
        && (
          requestSnapshotKey === discardPreparationSnapshotKeyRef.current
          || acceptedSnapshotKey === discardPreparationSnapshotKeyRef.current
        )
      ) {
        setBusyAction((current) => current === busyKey ? null : current);
      }
    }
  }, [
    active,
    busyAction,
    target,
    discardScopeKey,
    mutationDisabledReason,
    publishSnapshot,
    t
  ]);

  const requestDiscard = useCallback((
    path: string,
    includeUntracked: boolean,
    confirmation: PendingDiscard | null
  ) => {
    setPendingDiscard(null);
    void prepareDiscardConfirmation(path, includeUntracked).then((preparation) => {
      if (
        !preparation
        || preparation.scopeKey !== discardPreparationScopeRef.current
        || preparation.snapshotKey !== discardPreparationSnapshotKeyRef.current
      ) return;
      if (
        !confirmation
        || confirmation.scopeKey !== preparation.scopeKey
        || confirmation.snapshotKey !== preparation.snapshotKey
        || confirmation.path !== preparation.path
        || confirmation.includeUntracked !== preparation.includeUntracked
        || confirmation.contentRevision !== preparation.contentRevision
        || confirmation.targetRevision !== preparation.targetRevision
      ) {
        setPendingDiscard(preparation);
        return;
      }
      void runGitAction({
        type: "discard",
        paths: [preparation.path],
        includeUntracked: preparation.includeUntracked,
        expectedContentRevision: preparation.contentRevision,
        expectedTargetRevision: preparation.targetRevision
      }, `discard:${preparation.contentRevision}:${preparation.path}`);
    });
  }, [prepareDiscardConfirmation, runGitAction]);

  const prepareStageAllConfirmation = useCallback(async (): Promise<PendingStageAll | null> => {
    if (!active || busyAction || mutationDisabledReason) return null;
    const requestId = ++stageAllPreparationRequestRef.current;
    const requestScopeKey = stageAllScopeKey;
    const requestSnapshotKey = stageAllPreparationSnapshotKeyRef.current;
    let acceptedSnapshotKey: string | null = null;
    const busyKey = "prepare-stage-all";
    setBusyAction(busyKey);
    setOperationError(null);
    setOperationMessage(null);
    try {
      const preparation = await prepareGitStageAll(target);
      if (
        requestId !== stageAllPreparationRequestRef.current
        || requestScopeKey !== stageAllPreparationScopeRef.current
        || requestSnapshotKey !== stageAllPreparationSnapshotKeyRef.current
      ) return null;
      acceptedSnapshotKey = discardPreparationSnapshotKey(preparation.snapshot);
      acceptedStageAllPreparationSnapshotKeyRef.current = acceptedSnapshotKey;
      stageAllPreparationSnapshotKeyRef.current = acceptedSnapshotKey;
      return {
        key: JSON.stringify([
          requestScopeKey,
          acceptedSnapshotKey,
          preparation.snapshot.contentRevision,
          preparation.targetRevision,
          preparation.candidateTreeOid
        ]),
        scopeKey: requestScopeKey,
        snapshotKey: acceptedSnapshotKey,
        snapshot: preparation.snapshot,
        contentRevision: preparation.snapshot.contentRevision,
        targetRevision: preparation.targetRevision,
        candidateTreeOid: preparation.candidateTreeOid
      };
    } catch (reason) {
      if (
        requestId !== stageAllPreparationRequestRef.current
        || requestScopeKey !== stageAllPreparationScopeRef.current
        || (
          requestSnapshotKey !== stageAllPreparationSnapshotKeyRef.current
          && acceptedSnapshotKey !== stageAllPreparationSnapshotKeyRef.current
        )
      ) return null;
      setPendingStageAll(null);
      setOperationError(failureMessage(
        reason,
        t("无法准备安全批量暂存", "Unable to prepare safe bulk staging")
      ));
      return null;
    } finally {
      if (
        requestId === stageAllPreparationRequestRef.current
        && requestScopeKey === stageAllPreparationScopeRef.current
        && (
          requestSnapshotKey === stageAllPreparationSnapshotKeyRef.current
          || acceptedSnapshotKey === stageAllPreparationSnapshotKeyRef.current
        )
      ) {
        setBusyAction((current) => current === busyKey ? null : current);
      }
    }
  }, [
    active,
    busyAction,
    target,
    mutationDisabledReason,
    stageAllScopeKey,
    t
  ]);

  const requestStageAll = useCallback((confirmation: PendingStageAll | null) => {
    void prepareStageAllConfirmation().then((preparation) => {
      if (
        !preparation
        || preparation.scopeKey !== stageAllPreparationScopeRef.current
        || preparation.snapshotKey !== stageAllPreparationSnapshotKeyRef.current
      ) return;
      if (
        !confirmation
        || confirmation.scopeKey !== preparation.scopeKey
        || confirmation.snapshotKey !== preparation.snapshotKey
        || confirmation.contentRevision !== preparation.contentRevision
        || confirmation.targetRevision !== preparation.targetRevision
        || confirmation.candidateTreeOid !== preparation.candidateTreeOid
      ) {
        const drifted = Boolean(confirmation);
        changePageRequestRef.current += 1;
        changeDiffRequestRef.current += 1;
        setChangeFilter("");
        setChangeFiles([]);
        setChangeMatchedCount(preparation.snapshot.changedFiles ?? 0);
        setChangeNextCursor(null);
        setChangeLoadingMore(false);
        setChangePageState("idle");
        setChangePageError(null);
        setChangePageStageAllProof(null);
        setSelectedChangeFile(null);
        setDiffScope("working");
        setDiff(null);
        setDiffState("idle");
        setDiffError(null);
        setDiffRevision((revision) => revision + 1);
        publishSnapshot(preparation.snapshot);
        setPendingStageAll(preparation);
        setOperationMessage(drifted
          ? t(
              "待暂存内容已变化；已生成新的候选，请重新审阅后确认",
              "The content to stage changed. A new candidate was prepared; review it before confirming."
            )
          : t(
              "已生成不可变暂存候选；请审阅差异后再次确认",
              "An immutable staging candidate is ready. Review the diff, then confirm again."
            ));
        return;
      }
      setPendingStageAll(null);
      setChangePageStageAllProof(null);
      void runGitAction({
        type: "stage_all",
        expectedContentRevision: preparation.contentRevision,
        expectedTargetRevision: preparation.targetRevision
      }, `stage-all:${preparation.contentRevision}`);
    });
  }, [prepareStageAllConfirmation, publishSnapshot, runGitAction, t]);

  const prepareCommitConfirmation = useCallback(async (
    message: string
  ): Promise<PendingCommit | null> => {
    if (!active || busyAction || mutationDisabledReason || !message) return null;
    const requestId = ++commitPreparationRequestRef.current;
    const requestScopeKey = commitPreparationScopeRef.current;
    const requestSnapshotKey = commitPreparationSnapshotKeyRef.current;
    const busyKey = `prepare-commit:${requestId}`;
    stageAllPreparationRequestRef.current += 1;
    setPendingStageAll(null);
    setChangePageStageAllProof(null);
    setBusyAction(busyKey);
    setOperationError(null);
    setOperationMessage(null);
    try {
      const preparation = await prepareGitCommit(target, message);
      if (
        requestId !== commitPreparationRequestRef.current
        || requestScopeKey !== commitPreparationScopeRef.current
        || requestSnapshotKey !== commitPreparationSnapshotKeyRef.current
        || message !== commitMessageRef.current.trim()
      ) return null;
      const acceptedSnapshotKey = discardPreparationSnapshotKey(preparation.snapshot);
      acceptedCommitPreparationSnapshotKeyRef.current = acceptedSnapshotKey;
      commitPreparationSnapshotKeyRef.current = acceptedSnapshotKey;
      const pending: PendingCommit = {
        scopeKey: requestScopeKey,
        snapshotKey: acceptedSnapshotKey,
        message,
        snapshot: preparation.snapshot,
        targetRevision: preparation.targetRevision,
        candidateTreeOid: preparation.candidateTreeOid,
        messageDigest: preparation.messageDigest
      };
      publishSnapshot(preparation.snapshot);
      setPendingCommit(pending);
      setOperationMessage(t(
        "提交候选已锁定；核对树和文件数后再次确认",
        "The commit candidate is locked. Verify the tree and file count, then confirm again."
      ));
      return pending;
    } catch (reason) {
      if (
        requestId !== commitPreparationRequestRef.current
        || requestScopeKey !== commitPreparationScopeRef.current
        || requestSnapshotKey !== commitPreparationSnapshotKeyRef.current
        || message !== commitMessageRef.current.trim()
      ) return null;
      setPendingCommit(null);
      setOperationError(failureMessage(
        reason,
        t("无法准备安全提交", "Unable to prepare a safe commit")
      ));
      return null;
    } finally {
      setBusyAction((current) => current === busyKey ? null : current);
    }
  }, [
    active,
    busyAction,
    target,
    mutationDisabledReason,
    publishSnapshot,
    t
  ]);

  const requestCommit = useCallback(() => {
    const message = commitMessageRef.current.trim();
    if (!message) return;
    const snapshotKey = commitPreparationSnapshotKeyRef.current;
    const confirmation = pendingCommit
      && pendingCommit.scopeKey === commitPreparationScopeRef.current
      && pendingCommit.snapshotKey === snapshotKey
      && pendingCommit.message === message
      ? pendingCommit
      : null;
    setPendingCommit(null);
    void prepareCommitConfirmation(message).then((preparation) => {
      if (!preparation || !confirmation) return;
      if (
        confirmation.scopeKey !== preparation.scopeKey
        || confirmation.snapshotKey !== preparation.snapshotKey
        || confirmation.message !== preparation.message
        || confirmation.targetRevision !== preparation.targetRevision
        || confirmation.candidateTreeOid !== preparation.candidateTreeOid
        || confirmation.messageDigest !== preparation.messageDigest
      ) {
        setOperationMessage(t(
          "提交候选已变化；已刷新证明，请重新核对后确认",
          "The commit candidate changed. The proof was refreshed; verify it before confirming again."
        ));
        return;
      }
      void runGitAction({
        type: "commit",
        message,
        expectedTargetRevision: preparation.targetRevision,
        expectedTreeOid: preparation.candidateTreeOid
      }, `commit:${preparation.targetRevision}:${preparation.candidateTreeOid}`);
    });
  }, [pendingCommit, prepareCommitConfirmation, runGitAction, t]);

  const updateCommitMessage = useCallback((message: string) => {
    commitPreparationRequestRef.current += 1;
    acceptedCommitPreparationSnapshotKeyRef.current = null;
    setPendingCommit(null);
    setBusyAction((current) => current?.startsWith("prepare-commit:") ? null : current);
    setCommitMessage(message);
  }, []);

  const runGitHubAction = useCallback(async (action: GitHubAction, key: string) => {
    if (
      !active
      || busyAction
      || mutationDisabledReason
      || githubState === "loading"
      || pullRequestsLoadingMore
    ) return false;
    if (!onMutationStart()) {
      setOperationError(t(
        "另一个任务或工作区操作已经开始，请稍后重试",
        "Another task or workspace operation has started. Try again shortly."
      ));
      return false;
    }
    const actionWorkspaceIdentity = workspaceIdentityKeyRef.current;
    githubRequestRef.current += 1;
    pullRequestRef.current += 1;
    pullRequestReadinessRef.current += 1;
    pullRequestDiffRef.current += 1;
    pullRequestThreadsRef.current += 1;
    commitPreparationRequestRef.current += 1;
    acceptedCommitPreparationSnapshotKeyRef.current = null;
    setPendingCommit(null);
    if (action.type === "checkout_pull_request") {
      invalidateRepositoryReads(true, true);
    }
    setPullRequestsLoadingMore(false);
    setPullRequestThreadsLoadingMore(false);
    setPullRequestReadiness(null);
    setPullRequestReadinessState("idle");
    setPullRequestReadinessError(null);
    setBusyAction(key);
    setOperationError(null);
    setOperationMessage(null);
    if (action.type === "create_pull_request") {
      pullRequestRef.current += 1;
      pullRequestDiffRef.current += 1;
      setSelectedPullRequest(null);
      setPullRequestReview(null);
      setPullRequestState("idle");
      setPullRequestSelectedPath(null);
      setPullRequestSelectedDiff(null);
      setPullRequestDiffState("idle");
      setPullRequestDiffError(null);
      setPullRequestThreads([]);
      setPullRequestThreadsHeadOid(null);
      setPullRequestThreadsTotalCount(0);
      setPullRequestThreadsNextCursor(null);
      setPullRequestThreadsState("idle");
      setPullRequestThreadsError(null);
      setPullRequestThreadsLoadingMore(false);
      setPullRequestSelectedLine(null);
      setPullRequestLineComment("");
      setPullRequestLineDrafts([]);
      setPullRequestReviewBody("");
      setPullRequestReviewEvent("comment");
      setPullRequestThreadReplies({});
    }
    try {
      const result = await executeGitHubAction(target, action);
      if (actionWorkspaceIdentity !== workspaceIdentityKeyRef.current) return false;
      if (result.snapshot !== undefined) publishSnapshot(result.snapshot);
      if (action.type === "checkout_pull_request" && result.snapshot?.branch) {
        clearComparison();
        setCompareHead(result.snapshot.branch);
      }
      if (result.repository !== undefined) setGithubRepository(result.repository);
      if (action.type !== "create_pull_request") {
        const nextDetail = result.pullRequest;
        if (
          nextDetail
          && pullRequestReview?.detail.number === nextDetail.number
          && pullRequestReview.detail.headRefOid === nextDetail.headRefOid
        ) {
          setPullRequestReview({ detail: nextDetail, diff: pullRequestReview.diff });
          setSelectedPullRequest((current) => current ? {
            ...current,
            title: nextDetail.title,
            state: nextDetail.state,
            headRefName: nextDetail.headRefName,
            baseRefName: nextDetail.baseRefName,
            author: nextDetail.author,
            updatedAt: nextDetail.updatedAt,
            url: nextDetail.url,
            draft: nextDetail.draft,
            mergeable: nextDetail.mergeable
          } : current);
          setPullRequestState("ready");
        } else if (!githubActionKeepsOpenReview(action)) {
          pullRequestDiffRef.current += 1;
          pullRequestThreadsRef.current += 1;
          setSelectedPullRequest(null);
          setPullRequestReview(null);
          setPullRequestState("idle");
          setPullRequestSelectedPath(null);
          setPullRequestSelectedDiff(null);
          setPullRequestDiffState("idle");
          setPullRequestDiffError(null);
          setPullRequestThreads([]);
          setPullRequestThreadsHeadOid(null);
          setPullRequestThreadsTotalCount(0);
          setPullRequestThreadsNextCursor(null);
          setPullRequestThreadsState("idle");
          setPullRequestThreadsError(null);
          setPullRequestThreadsLoadingMore(false);
          setPullRequestSelectedLine(null);
          setPullRequestLineComment("");
          setPullRequestLineDrafts([]);
          setPullRequestReviewBody("");
          setPullRequestReviewEvent("comment");
          setPullRequestThreadReplies({});
        }
      }
      setGithubState("idle");
      setOperationMessage(
        result.message?.trim()
        || t("GitHub 操作已完成", "GitHub operation completed")
      );
      return true;
    } catch (reason) {
      if (actionWorkspaceIdentity !== workspaceIdentityKeyRef.current) return false;
      const message = failureMessage(reason, t("GitHub 操作失败", "GitHub operation failed"));
      try {
        await reconcileWorkspaceSummary();
        setDiffRevision((revision) => revision + 1);
      } catch {
        // A failed `gh pr checkout` may still have changed local refs. Keep the
        // original error visible and let the normal polling path retry later.
      }
      if (action.type === "checkout_pull_request") {
        setBranchesState("idle");
        setHistoryState("idle");
      }
      setOperationError(message);
      return false;
    } finally {
      setBusyAction((current) => current === key ? null : current);
      onMutationEnd();
    }
  }, [
    active,
    busyAction,
    clearComparison,
    target,
    githubState,
    invalidateRepositoryReads,
    mutationDisabledReason,
    onMutationEnd,
    onMutationStart,
    pullRequestReview,
    pullRequestsLoadingMore,
    publishSnapshot,
    reconcileWorkspaceSummary,
    t
  ]);

  useEffect(() => {
    if (
      !active
      || busyAction
      || view !== "changes"
      || !selectedPath
      || selectedChangeFile?.path !== selectedPath
    ) {
      if (!selectedPath || selectedChangeFile?.path !== selectedPath) {
        changeDiffRequestRef.current += 1;
        setDiff(null);
        setDiffState("idle");
      }
      return;
    }
    const requestId = ++changeDiffRequestRef.current;
    const requestedStageAllProof = pendingStageAll ? {
      key: pendingStageAll.key,
      targetRevision: pendingStageAll.targetRevision,
      candidateTreeOid: pendingStageAll.candidateTreeOid
    } : null;
    setDiffState("loading");
    setDiffError(null);
    const request: GitDiffRequest = diffScope === "staged"
      ? { type: "staged", path: selectedPath }
      : {
          type: diffScope,
          path: selectedPath,
          ...(requestedStageAllProof ? {
            expectedStageAllTargetRevision: requestedStageAllProof.targetRevision
          } : {})
        };
    void getGitDiff(target, request)
      .then((result) => {
        if (requestId !== changeDiffRequestRef.current) return;
        if (requestedStageAllProof) {
          if (
            result.stageAllTargetRevision !== requestedStageAllProof.targetRevision
            || result.candidateTreeOid !== requestedStageAllProof.candidateTreeOid
          ) {
            throw new Error(t(
              "文件差异不属于当前全部暂存证明；请重新审阅",
              "The file diff does not belong to the current stage-all proof. Review it again."
            ));
          }
        } else if (result.stageAllTargetRevision || result.candidateTreeOid) {
          throw new Error(t(
            "文件差异携带了意外的全部暂存证明",
            "The file diff carried an unexpected stage-all proof."
          ));
        }
        setDiff(result);
        setDiffState("ready");
      })
      .catch((reason) => {
        if (requestId !== changeDiffRequestRef.current) return;
        const message = failureMessage(reason, t("无法读取文件差异", "Unable to load file diff"));
        if (requestedStageAllProof) {
          setPendingStageAll((current) => (
            current?.key === requestedStageAllProof.key ? null : current
          ));
          setChangePageStageAllProof(null);
          setOperationError(t(
            "全部暂存证明已失效：{message}",
            "The stage-all proof is no longer valid: {message}",
            { message }
          ));
        }
        setDiffError(message);
        setDiffState("error");
      });
    return () => {
      if (requestId === changeDiffRequestRef.current) changeDiffRequestRef.current += 1;
    };
  }, [
    active,
    busyAction,
    target,
    diffRevision,
    diffScope,
    pendingStageAll,
    selectedChangeFile?.path,
    selectedPath,
    t,
    view
  ]);

  const loadBranches = useCallback(async (force = false) => {
    if (!active || busyAction || (!force && branchesState !== "idle")) return;
    const requestId = ++branchesRequestRef.current;
    setBranchesState("loading");
    setBranchesError(null);
    try {
      const result = await getGitBranches(target);
      if (requestId !== branchesRequestRef.current) return;
      setBranches(result);
      setBranchesState("ready");
      const locals = result.branches.filter((branch) => branch.kind === "local");
      setCompareBase((current) => current || result.defaultBranch || locals.find((branch) => !branch.current)?.name || "");
      setCompareHead((current) => current || locals.find((branch) => branch.current)?.name || "HEAD");
    } catch (reason) {
      if (requestId !== branchesRequestRef.current) return;
      setBranchesError(failureMessage(reason, t("无法读取分支", "Unable to load branches")));
      setBranchesState("error");
    }
  }, [active, branchesState, busyAction, target, t]);

  const loadHistory = useCallback(async (force = false, cursor?: string) => {
    const append = Boolean(cursor);
    if (
      !active
      || busyAction
      || (append && historyLoadingMore)
      || (!append && !force && historyState !== "idle")
    ) return;
    const requestId = ++historyRequestRef.current;
    if (append) {
      setHistoryLoadingMore(true);
    } else {
      setHistoryLoadingMore(false);
      setHistoryNextCursor(null);
      setHistoryState("loading");
    }
    setHistoryError(null);
    try {
      const result = await getGitHistory(target, {
        limit: 100,
        ...(cursor ? { cursor } : {})
      });
      if (requestId !== historyRequestRef.current) return;
      setHistory((current) => {
        if (!append) return result.commits;
        const seen = new Set(current.map((commit) => commit.oid));
        return [...current, ...result.commits.filter((commit) => !seen.has(commit.oid))];
      });
      setHistoryNextCursor(result.nextCursor);
      setHistoryState("ready");
    } catch (reason) {
      if (requestId !== historyRequestRef.current) return;
      setHistoryError(failureMessage(reason, t("无法读取提交历史", "Unable to load commit history")));
      setHistoryState(append ? "ready" : "error");
    } finally {
      if (requestId === historyRequestRef.current) setHistoryLoadingMore(false);
    }
  }, [active, busyAction, target, historyLoadingMore, historyState, t]);

  const loadGitHub = useCallback(async (force = false) => {
    if (
      !active
      || busyAction
      || pullRequestsLoadingMore
      || githubState === "loading"
      || (!force && githubState !== "idle")
    ) return;
    const requestId = ++githubRequestRef.current;
    setPullRequestsLoadingMore(false);
    setGithubState("loading");
    setGithubError(null);
    let resolvedRepository = githubRepository;
    try {
      const repository = await getGitHubRepository(target);
      resolvedRepository = repository;
      if (requestId !== githubRequestRef.current) return;
      const repositoryChanged = Boolean(
        repository
        && githubRepository
        && (
          repository.host !== githubRepository.host
          || repository.nameWithOwner !== githubRepository.nameWithOwner
        )
      );
      setGithubRepository(repository);
      if (!repository) {
        setPullRequests([]);
        setPullRequestNextPage(null);
        setGithubState("ready");
        return;
      }
      if (repositoryChanged) {
        setPullRequests([]);
        setPullRequestNextPage(null);
      }
      const result = await getGitHubPullRequests(target, {
        page: 1,
        pageSize: GITHUB_PULL_REQUEST_PAGE_SIZE
      });
      if (requestId !== githubRequestRef.current) return;
      setPullRequests(mergePullRequests([], result.pullRequests));
      setPullRequestNextPage(result.hasMore ? result.nextPage : null);
      setPullRequestBase((current) => current || repository.defaultBranch || "");
      setGithubState("ready");
    } catch (reason) {
      if (requestId !== githubRequestRef.current) return;
      setGithubError(failureMessage(reason, t("无法读取 GitHub 仓库", "Unable to load GitHub repository")));
      setGithubState(resolvedRepository ? "ready" : "error");
    }
  }, [
    active,
    busyAction,
    target,
    githubRepository,
    githubState,
    pullRequestsLoadingMore,
    t
  ]);

  const loadMorePullRequests = useCallback(async () => {
    const page = pullRequestNextPage;
    if (
      !active
      || !githubRepository
      || !page
      || busyAction
      || mutationDisabledReason
      || githubState !== "ready"
      || pullRequestsLoadingMore
    ) return;
    const requestId = ++githubRequestRef.current;
    setPullRequestsLoadingMore(true);
    setGithubError(null);
    try {
      const result = await getGitHubPullRequests(target, {
        page,
        pageSize: GITHUB_PULL_REQUEST_PAGE_SIZE
      });
      if (requestId !== githubRequestRef.current) return;
      setPullRequests((current) => mergePullRequests(current, result.pullRequests));
      setPullRequestNextPage(result.hasMore ? result.nextPage : null);
    } catch (reason) {
      if (requestId !== githubRequestRef.current) return;
      setGithubError(failureMessage(
        reason,
        t("无法加载更多拉取请求", "Unable to load more pull requests")
      ));
    } finally {
      if (requestId === githubRequestRef.current) setPullRequestsLoadingMore(false);
    }
  }, [
    active,
    busyAction,
    target,
    githubRepository,
    githubState,
    mutationDisabledReason,
    pullRequestNextPage,
    pullRequestsLoadingMore,
    t
  ]);

  const loadPullRequestReadiness = useCallback(async (
    pullRequest: GitHubPullRequest,
    repository: GitHubRepository
  ) => {
    if (!active) return;
    const selectionKey = readinessSelectionKey(
      target,
      workspaceIdentityKey,
      repository,
      pullRequest
    );
    const requestId = ++pullRequestReadinessRef.current;
    setPullRequestReadiness(null);
    setPullRequestReadinessState("loading");
    setPullRequestReadinessError(null);
    try {
      const readiness = await getGitHubPullRequestReadiness(
        target,
        pullRequest.number
      );
      if (requestId !== pullRequestReadinessRef.current) return;
      const currentRepository = githubRepositoryRef.current;
      const currentPullRequest = selectedPullRequestRef.current;
      if (
        !currentRepository
        || !currentPullRequest
        || readinessSelectionKey(
          target,
          workspaceIdentityKeyRef.current,
          currentRepository,
          currentPullRequest
        )
          !== selectionKey
      ) return;
      const currentDetail = pullRequestReviewRef.current?.detail;
      if (!readinessMatchesPullRequest(
        readiness,
        currentRepository,
        currentPullRequest,
        currentDetail?.number === currentPullRequest.number ? currentDetail : null
      )) {
        throw new Error(t(
          "GitHub 就绪度来自其他仓库、分支或提交；已忽略该响应。",
          "GitHub readiness belongs to another repository, branch, or commit and was ignored."
        ));
      }
      setPullRequestReadiness(readiness);
      setPullRequestReadinessState("ready");
    } catch (reason) {
      if (requestId !== pullRequestReadinessRef.current) return;
      setPullRequestReadiness(null);
      setPullRequestReadinessError(failureMessage(
        reason,
        t(
          "无法读取合并就绪度；差异仍可审阅，但内置合并不可用。",
          "Merge readiness could not be loaded. The diff remains reviewable, but built-in merge is unavailable."
        )
      ));
      setPullRequestReadinessState("error");
    }
  }, [active, target, t, workspaceIdentityKey]);

  useEffect(() => {
    if (!active) return;
    if (view === "branches" || view === "compare") void loadBranches();
    if (view === "history") void loadHistory();
    if (view === "pullRequests") void loadGitHub();
  }, [active, loadBranches, loadGitHub, loadHistory, view]);

  useEffect(() => {
    if (
      !active
      || view !== "pullRequests"
      || busyAction
      || !githubRepository
      || !selectedPullRequest
      || pullRequestReadinessState !== "idle"
    ) return;
    void loadPullRequestReadiness(selectedPullRequest, githubRepository);
  }, [
    active,
    busyAction,
    githubRepository,
    loadPullRequestReadiness,
    pullRequestReadinessState,
    selectedPullRequest,
    view
  ]);

  useEffect(() => {
    if (
      pullRequestReadinessState !== "ready"
      || !pullRequestReadiness
      || !githubRepository
      || !selectedPullRequest
      || !pullRequestReview
      || readinessMatchesPullRequest(
        pullRequestReadiness,
        githubRepository,
        selectedPullRequest,
        pullRequestReview.detail
      )
    ) return;
    pullRequestReadinessRef.current += 1;
    setPullRequestReadiness(null);
    setPullRequestReadinessState("error");
    setPullRequestReadinessError(t(
      "拉取请求的仓库、分支或提交在读取期间发生变化；请重试就绪度检查。",
      "The pull request repository, branch, or commit changed while readiness was loading. Retry the readiness check."
    ));
  }, [
    githubRepository,
    pullRequestReadiness,
    pullRequestReadinessState,
    pullRequestReview,
    selectedPullRequest,
    t
  ]);

  useEffect(() => {
    if (!pullRequestReadiness) return;
    const methods = allowedMergeMethods(pullRequestReadiness);
    const preferred = pullRequestReadiness.viewerDefault.availability === "available"
      ? normalizeMergeMethod(pullRequestReadiness.viewerDefault.value?.mergeMethod)
      : null;
    setPullRequestMergeMethod(
      preferred && methods.includes(preferred)
        ? preferred
        : methods[0] ?? "squash"
    );
    setPendingGitHubAction((current) => (
      current?.startsWith("pr-merge:") ? null : current
    ));
  }, [pullRequestReadiness]);

  const loadComparison = useCallback(async (path?: string) => {
    const requestedBase = compareBase.trim();
    const requestedHead = compareHead.trim();
    if (!active || !requestedBase || !requestedHead || requestedBase === requestedHead) return;
    const requestId = ++compareRequestRef.current;
    setCompareState("loading");
    setCompareError(null);
    if (path) {
      setSelectedComparePath(path);
    } else {
      setCompareDiff(null);
      setCompareSelectedDiff(null);
      setSelectedComparePath(null);
    }
    try {
      const result = await getGitDiff(target, {
        type: "compare",
        base: requestedBase,
        head: requestedHead,
        ...(path ? { path } : {})
      });
      if (requestId !== compareRequestRef.current) return;
      if (path) {
        setCompareSelectedDiff(result);
      } else {
        setCompareDiff(result);
        setCompareSelectedDiff(null);
      }
      setCompareState("ready");
      if (!path) setSelectedComparePath(result.files[0]?.path ?? null);
    } catch (reason) {
      if (requestId !== compareRequestRef.current) return;
      setCompareError(failureMessage(reason, t("无法比较分支", "Unable to compare branches")));
      setCompareState("error");
    }
  }, [active, compareBase, compareHead, target, t]);

  const closePullRequestReview = useCallback(() => {
    pullRequestRef.current += 1;
    pullRequestReadinessRef.current += 1;
    pullRequestDiffRef.current += 1;
    pullRequestThreadsRef.current += 1;
    invalidatePullRequestThreadCommentRequests();
    setPendingGitHubAction(null);
    setSelectedPullRequest(null);
    setPullRequestReview(null);
    setPullRequestState("idle");
    setPullRequestReadiness(null);
    setPullRequestReadinessState("idle");
    setPullRequestReadinessError(null);
    setPullRequestSelectedPath(null);
    setPullRequestSelectedDiff(null);
    setPullRequestDiffState("idle");
    setPullRequestDiffError(null);
    setPullRequestThreads([]);
    setPullRequestThreadsHeadOid(null);
    setPullRequestThreadsTotalCount(0);
    setPullRequestThreadsNextCursor(null);
    setPullRequestThreadsState("idle");
    setPullRequestThreadsError(null);
    setPullRequestThreadsLoadingMore(false);
    setPullRequestSelectedLine(null);
    setPullRequestLineComment("");
    setPullRequestLineDrafts([]);
    setPullRequestReviewBody("");
    setPullRequestReviewEvent("comment");
    setPullRequestThreadReplies({});
    setGithubError(null);
  }, [invalidatePullRequestThreadCommentRequests]);

  const loadPullRequestFile = useCallback(async (
    review: PullRequestReview,
    path: string
  ) => {
    if (!active || !review.diff.files.some((file) => file.path === path)) return;
    const requestId = ++pullRequestDiffRef.current;
    const expectedHeadOid = review.detail.headRefOid;
    setPullRequestSelectedPath(path);
    setPullRequestSelectedDiff(null);
    setPullRequestSelectedLine(null);
    setPullRequestLineComment("");
    setPullRequestDiffState("loading");
    setPullRequestDiffError(null);
    try {
      const result = await getGitHubPullRequestDiff(
        target,
        review.detail.number,
        path
      );
      if (requestId !== pullRequestDiffRef.current) return;
      if (result.headRefOid !== expectedHeadOid) {
        setPullRequestLineDrafts([]);
        setPullRequestSelectedLine(null);
        setPullRequestLineComment("");
        setPullRequestReviewBody("");
        throw new Error(t(
          "拉取请求在读取文件差异时已更新；请重新打开以审阅最新提交。",
          "The pull request changed while its file diff was loading. Reopen it to review the latest commit."
        ));
      }
      setPullRequestSelectedDiff(result);
      setPullRequestDiffState("ready");
    } catch (reason) {
      if (requestId !== pullRequestDiffRef.current) return;
      setPullRequestDiffError(failureMessage(
        reason,
        t("无法读取拉取请求文件差异", "Unable to load pull request file diff")
      ));
      setPullRequestDiffState("error");
    }
  }, [active, target, t]);

  const openPullRequest = useCallback(async (pullRequest: GitHubPullRequest) => {
    if (!active) return;
    const requestId = ++pullRequestRef.current;
    pullRequestReadinessRef.current += 1;
    pullRequestDiffRef.current += 1;
    const threadsRequestId = ++pullRequestThreadsRef.current;
    invalidatePullRequestThreadCommentRequests();
    setSelectedPullRequest(pullRequest);
    setPullRequestReview(null);
    setPullRequestReadiness(null);
    setPullRequestReadinessState("idle");
    setPullRequestReadinessError(null);
    setPullRequestSelectedPath(null);
    setPullRequestSelectedDiff(null);
    setPullRequestDiffState("idle");
    setPullRequestDiffError(null);
    setPullRequestThreads([]);
    setPullRequestThreadsHeadOid(null);
    setPullRequestThreadsTotalCount(0);
    setPullRequestThreadsNextCursor(null);
    setPullRequestThreadsState("loading");
    setPullRequestThreadsError(null);
    setPullRequestThreadsLoadingMore(false);
    setPullRequestSelectedLine(null);
    setPullRequestLineComment("");
    setPullRequestLineDrafts([]);
    setPullRequestReviewBody("");
    setPullRequestReviewEvent("comment");
    setPullRequestThreadReplies({});
    setPendingGitHubAction(null);
    setPullRequestState("loading");
    setGithubError(null);
    try {
      const detail = await getGitHubPullRequestDetail(target, pullRequest.number);
      if (requestId !== pullRequestRef.current) return;
      if (detail.number !== pullRequest.number) {
        throw new Error(t(
          "GitHub 返回了其他拉取请求；请重试。",
          "GitHub returned a different pull request. Try again."
        ));
      }
      const [nextDiff, threadsOutcome] = await Promise.all([
        getGitHubPullRequestDiff(target, pullRequest.number),
        getGitHubPullRequestReviewThreads(target, {
          number: pullRequest.number,
          expectedHeadOid: detail.headRefOid,
          pageSize: GITHUB_REVIEW_THREAD_PAGE_SIZE
        }).then(
          (result) => ({ result }),
          (reason: unknown) => ({ reason })
        )
      ]);
      if (
        requestId !== pullRequestRef.current
        || threadsRequestId !== pullRequestThreadsRef.current
      ) return;
      if (
        detail.headRefOid !== nextDiff.headRefOid
      ) {
        throw new Error(t(
          "拉取请求在读取期间已更新；请重新打开以审阅最新提交。",
          "The pull request changed while it was loading. Reopen it to review the latest commit."
        ));
      }
      const review = { detail, diff: nextDiff };
      const firstPath = nextDiff.files[0]?.path ?? null;
      if (
        detail.headRefName !== pullRequest.headRefName
        || detail.baseRefName !== pullRequest.baseRefName
        || detail.state !== pullRequest.state
        || detail.draft !== pullRequest.draft
      ) {
        pullRequestReadinessRef.current += 1;
        setPullRequestReadiness(null);
        setPullRequestReadinessState("idle");
        setPullRequestReadinessError(null);
      }
      setPullRequestReview(review);
      setSelectedPullRequest({
        ...pullRequest,
        title: detail.title,
        state: detail.state,
        headRefName: detail.headRefName,
        baseRefName: detail.baseRefName,
        author: detail.author,
        updatedAt: detail.updatedAt,
        url: detail.url,
        draft: detail.draft,
        mergeable: detail.mergeable
      });
      if ("result" in threadsOutcome) {
        const threadsResult = threadsOutcome.result;
        if (
          threadsResult.number === detail.number
          && threadsResult.headRefOid === detail.headRefOid
        ) {
          setPullRequestThreads(threadsResult.threads);
          setPullRequestThreadsHeadOid(threadsResult.headRefOid);
          setPullRequestThreadsTotalCount(threadsResult.totalCount);
          setPullRequestThreadsNextCursor(threadsResult.nextCursor);
          setPullRequestThreadsState("ready");
        } else {
          setPullRequestThreads([]);
          setPullRequestThreadsHeadOid(null);
          setPullRequestThreadsTotalCount(0);
          setPullRequestThreadsNextCursor(null);
          setPullRequestThreadsState("error");
          setPullRequestThreadsError(t(
            "审阅线程来自其他提交；已禁用审阅写入。",
            "Review threads belong to another head, so review writes are disabled."
          ));
        }
      } else {
        setPullRequestThreads([]);
        setPullRequestThreadsHeadOid(null);
        setPullRequestThreadsTotalCount(0);
        setPullRequestThreadsNextCursor(null);
        setPullRequestThreadsState("error");
        setPullRequestThreadsError(failureMessage(
          threadsOutcome.reason,
          t(
            "无法读取审阅线程；仍可浏览差异，但审阅写入已禁用。",
            "Review threads could not be loaded. The diff remains readable, but review writes are disabled."
          )
        ));
      }
      setPullRequestSelectedPath(firstPath);
      setPullRequestState("ready");
      if (firstPath) {
        void loadPullRequestFile(review, firstPath);
      } else {
        setPullRequestDiffState("ready");
      }
    } catch (reason) {
      if (requestId !== pullRequestRef.current) return;
      setPullRequestReview(null);
      setPullRequestThreadsState("error");
      setPullRequestThreadsError(failureMessage(
        reason,
        t("无法读取审阅线程", "Unable to load review threads")
      ));
      setGithubError(failureMessage(reason, t("无法读取拉取请求", "Unable to load pull request")));
      setPullRequestState("error");
    }
  }, [
    active,
    target,
    invalidatePullRequestThreadCommentRequests,
    loadPullRequestFile,
    t
  ]);

  const loadMorePullRequestThreads = useCallback(async () => {
    const review = pullRequestReview;
    const cursor = pullRequestThreadsNextCursor;
    if (
      !active
      || !review
      || !cursor
      || pullRequestThreadsLoadingMore
      || busyAction
    ) return;
    const requestId = ++pullRequestThreadsRef.current;
    setPullRequestThreadsLoadingMore(true);
    setPullRequestThreadsError(null);
    try {
      const result = await getGitHubPullRequestReviewThreads(target, {
        number: review.detail.number,
        expectedHeadOid: review.detail.headRefOid,
        cursor,
        pageSize: GITHUB_REVIEW_THREAD_PAGE_SIZE
      });
      if (requestId !== pullRequestThreadsRef.current) return;
      if (
        result.number !== review.detail.number
        || result.headRefOid !== review.detail.headRefOid
      ) {
        invalidatePullRequestThreadCommentRequests();
        setPullRequestLineDrafts([]);
        setPullRequestSelectedLine(null);
        setPullRequestLineComment("");
        setPullRequestReviewBody("");
        setPullRequestThreadReplies({});
        setPullRequestThreadsHeadOid(null);
        setPullRequestThreadsNextCursor(null);
        throw new Error(t(
          "拉取请求提交已变化，审阅草稿已清除；请重新打开审阅。",
          "The pull request head changed, so review drafts were cleared. Reopen the review."
        ));
      }
      setPullRequestThreads((current) => mergeReviewThreads(current, result.threads));
      setPullRequestThreadsHeadOid(result.headRefOid);
      setPullRequestThreadsTotalCount(result.totalCount);
      setPullRequestThreadsNextCursor(result.nextCursor);
      setPullRequestThreadsState("ready");
    } catch (reason) {
      if (requestId !== pullRequestThreadsRef.current) return;
      setPullRequestThreadsError(failureMessage(
        reason,
        t("无法加载更多审阅线程", "Unable to load more review threads")
      ));
      setPullRequestThreadsState("error");
    } finally {
      if (requestId === pullRequestThreadsRef.current) {
        setPullRequestThreadsLoadingMore(false);
      }
    }
  }, [
    active,
    busyAction,
    target,
    pullRequestReview,
    pullRequestThreadsLoadingMore,
    pullRequestThreadsNextCursor,
    invalidatePullRequestThreadCommentRequests,
    t
  ]);

  const refreshPullRequestReviewContext = useCallback(async (
    review: PullRequestReview
  ): Promise<boolean> => {
    const requestId = ++pullRequestRef.current;
    const threadsRequestId = ++pullRequestThreadsRef.current;
    invalidatePullRequestThreadCommentRequests();
    setPullRequestThreadsState("loading");
    setPullRequestThreadsError(null);
    try {
      const detail = await getGitHubPullRequestDetail(target, review.detail.number);
      if (requestId !== pullRequestRef.current) return false;
      if (
        detail.number !== review.detail.number
        || detail.headRefOid !== review.detail.headRefOid
      ) {
        setPullRequestLineDrafts([]);
        setPullRequestSelectedLine(null);
        setPullRequestLineComment("");
        setPullRequestReviewBody("");
        setPullRequestThreadReplies({});
        setPullRequestThreads([]);
        setPullRequestThreadsHeadOid(null);
        setPullRequestThreadsTotalCount(0);
        setPullRequestThreadsNextCursor(null);
        setPullRequestThreadsState("error");
        setPullRequestThreadsError(t(
          "拉取请求提交已变化，审阅草稿已清除。",
          "The pull request head changed, so review drafts were cleared."
        ));
        setPullRequestReview(null);
        setSelectedPullRequest(detail);
        setPullRequestState("error");
        setGithubError(t(
          "拉取请求提交已变化；已拒绝旧提交上的审阅，请重新打开。",
          "The pull request head changed. The review against the old head was rejected; reopen it."
        ));
        return false;
      }
      const result = await getGitHubPullRequestReviewThreads(target, {
        number: detail.number,
        expectedHeadOid: detail.headRefOid,
        pageSize: GITHUB_REVIEW_THREAD_PAGE_SIZE
      });
      if (
        requestId !== pullRequestRef.current
        || threadsRequestId !== pullRequestThreadsRef.current
      ) return false;
      if (result.number !== detail.number || result.headRefOid !== detail.headRefOid) {
        throw new Error(t(
          "审阅线程来自其他提交；请重新打开拉取请求。",
          "Review threads belong to another head. Reopen the pull request."
        ));
      }
      setPullRequestReview({ detail, diff: review.diff });
      setSelectedPullRequest(detail);
      setPullRequestState("ready");
      setPullRequestThreads(result.threads);
      setPullRequestThreadsHeadOid(result.headRefOid);
      setPullRequestThreadsTotalCount(result.totalCount);
      setPullRequestThreadsNextCursor(result.nextCursor);
      setPullRequestThreadsState("ready");
      setGithubError(null);
      return true;
    } catch (reason) {
      if (requestId !== pullRequestRef.current) return false;
      setPullRequestThreadsError(failureMessage(
        reason,
        t("无法刷新审阅线程", "Unable to refresh review threads")
      ));
      setPullRequestThreadsState("error");
      return false;
    }
  }, [target, invalidatePullRequestThreadCommentRequests, t]);

  const totalChangedFiles = currentSnapshot.changedFiles ?? currentSnapshot.files.length;
  const stageableFiles = currentSnapshot.stageable ?? currentSnapshot.unstaged;
  const unstageableFiles = currentSnapshot.unstageable ?? currentSnapshot.staged;
  const remainingChanges = Math.max(0, changeMatchedCount - changeFiles.length);
  const nextChangePageCount = Math.min(CHANGE_FILE_BATCH_SIZE, remainingChanges);
  const groupedChangeFiles: Record<ChangeCompositionGroupId, GitFileChange[]> = {
    conflicted: [],
    partiallyStaged: [],
    staged: [],
    unstaged: [],
    untracked: []
  };
  for (const file of changeFiles) {
    groupedChangeFiles[changeCompositionGroup(file)].push(file);
  }
  const changeGroups: Array<{
    id: ChangeCompositionGroupId;
    label: string;
    description: string;
    files: GitFileChange[];
  }> = [{
    id: "conflicted",
    label: t("冲突", "Conflicted"),
    description: t("解决后重新暂存", "Resolve, then stage again"),
    files: groupedChangeFiles.conflicted
  }, {
    id: "partiallyStaged",
    label: t("部分暂存", "Partially staged"),
    description: t("索引 + 工作区", "Index + working tree"),
    files: groupedChangeFiles.partiallyStaged
  }, {
    id: "staged",
    label: t("已暂存", "Staged"),
    description: t("将进入下次提交", "Included in the next commit"),
    files: groupedChangeFiles.staged
  }, {
    id: "unstaged",
    label: t("未暂存", "Unstaged"),
    description: t("尚未加入提交", "Not yet included"),
    files: groupedChangeFiles.unstaged
  }, {
    id: "untracked",
    label: t("未跟踪", "Untracked"),
    description: t("尚未纳入版本控制", "Not yet under version control"),
    files: groupedChangeFiles.untracked
  }];
  const visibleChangeGroups = changeGroups.filter((group) => group.files.length > 0);

  const comparisonBranches = branches?.branches ?? [];
  const selectedChange = selectedChangeFile?.path === selectedPath ? selectedChangeFile : null;
  const currentDiscardSnapshotKey = discardPreparationSnapshotKey(currentSnapshot);
  const currentStageAllSnapshotKey = discardPreparationSnapshotKey(currentSnapshot);
  const stageAllArmed = Boolean(
    pendingStageAll
    && pendingStageAll.scopeKey === stageAllScopeKey
    && pendingStageAll.snapshotKey === currentStageAllSnapshotKey
    && pendingStageAll.contentRevision === currentSnapshot.contentRevision
  );
  const stageAllReviewReady = Boolean(
    stageAllArmed
    && pendingStageAll
    && changePageState === "ready"
    && changePageStageAllProof?.targetRevision === pendingStageAll.targetRevision
    && changePageStageAllProof.candidateTreeOid === pendingStageAll.candidateTreeOid
    && !normalizedChangeFilter
    && selectedPath
    && selectedChange?.path === selectedPath
    && diffScope === "working"
    && diffState === "ready"
    && diff?.path === selectedPath
    && diff.stageAllTargetRevision === pendingStageAll.targetRevision
    && diff.candidateTreeOid === pendingStageAll.candidateTreeOid
    && !diff.truncated
  );
  const commitArmed = Boolean(
    pendingCommit
    && pendingCommit.scopeKey === commitScopeKey
    && pendingCommit.snapshotKey === commitSnapshotKey
    && pendingCommit.message === commitMessage.trim()
  );
  const operationBusy = busyAction !== null;
  const mutationsDisabled = operationBusy || Boolean(mutationDisabledReason);
  const repositoryOperation = currentSnapshot.operation;
  const repositoryTransitionDisabled = mutationsDisabled || repositoryOperation !== null;
  const selectedRemote = remoteByName(currentSnapshot, selectedRemoteName);
  const selectedRemoteIsLocal = selectedRemote?.name === ".";
  const currentUpstream = currentSnapshot.upstreamTarget;
  const currentUpstreamRemote = currentUpstream
    ? remoteByName(currentSnapshot, currentUpstream.remoteName)
    : null;
  const upstreamRemoteProofMatches = Boolean(
    currentUpstream
    && currentUpstreamRemote
    && gitRemoteProofKey(currentUpstream.remote) === gitRemoteProofKey(currentUpstreamRemote)
  );
  const pullTargetLabel = currentUpstream
    ? `${currentUpstream.remoteName}/${currentUpstream.remoteBranch}`
    : null;
  const pushRemoteBranch = (
    currentUpstream
    && currentUpstream.remoteName === selectedRemote?.name
  )
    ? currentUpstream.remoteBranch
    : currentSnapshot.branch;
  const localRemoteDisabledReason = selectedRemoteIsLocal
    ? t(
        "“.” 是本地仓库；获取、拉取和推送不可用。",
        "\".\" is the local repository; fetch, pull, and push are unavailable."
      )
    : null;
  const repositoryOperationScope = repositoryOperation
    ? `${workspaceIdentityKey}:${repositoryOperation}:${currentSnapshot.head ?? "unborn"}:${currentSnapshot.operationRevision ?? "unavailable"}`
    : null;
  const skipOperationKey = repositoryOperationScope ? `skip-operation:${repositoryOperationScope}` : "";
  const abortOperationKey = repositoryOperationScope ? `abort-operation:${repositoryOperationScope}` : "";
  const bisectOldOperationKey = repositoryOperationScope ? `bisect-old:${repositoryOperationScope}` : "";
  const bisectNewOperationKey = repositoryOperationScope ? `bisect-new:${repositoryOperationScope}` : "";
  const bisectSkipOperationKey = repositoryOperationScope ? `bisect-skip:${repositoryOperationScope}` : "";
  const stashKey = `stash:${workspaceIdentityKey}:${currentSnapshot.contentRevision}`;
  const stashPopKey = `stash-pop:${workspaceIdentityKey}:${currentSnapshot.contentRevision}:${currentSnapshot.stash}`;
  const githubListBusy = githubState === "loading" || pullRequestsLoadingMore;
  const githubActionIdentity: GitHubActionIdentity | null = (
    githubRepository?.viewerLogin
      ? {
          expectedRepository: {
            host: githubRepository.host,
            owner: githubRepository.owner,
            name: githubRepository.name
          },
          expectedViewerLogin: githubRepository.viewerLogin
        }
      : null
  );
  const githubWriteScopeKey = JSON.stringify([
    currentSnapshot.repositoryId,
    currentSnapshot.worktreeId,
    githubRepository?.host ?? null,
    githubRepository?.owner ?? null,
    githubRepository?.name ?? null,
    githubRepository?.viewerLogin ?? null
  ]);
  const githubActionsDisabled = repositoryTransitionDisabled
    || githubListBusy
    || !githubActionIdentity;
  const createPullRequestBase = pullRequestBase.trim() || githubRepository?.defaultBranch || "";
  const createPullRequestHead = currentSnapshot.branch ?? "";
  const createPullRequestConfirmationKey = `pr-create:${JSON.stringify([
    githubWriteScopeKey,
    pullRequestTitle.trim(),
    pullRequestBody.trim(),
    createPullRequestBase,
    createPullRequestHead,
    currentSnapshot.head,
    currentSnapshot.contentRevision,
    pullRequestDraft
  ])}`;

  const loadMorePullRequestThreadComments = useCallback(async (
    thread: GitHubPullRequestReviewThread
  ) => {
    const review = pullRequestReview;
    const identity = githubActionIdentity;
    const page = pullRequestThreadCommentsPages[thread.id];
    const cursor = page
      ? page.nextCursor
      : thread.commentsNextCursor ?? null;
    if (
      !active
      || !review
      || !identity
      || !cursor
      || page?.loading
    ) return;

    const scopeId = pullRequestThreadCommentsScopeRef.current;
    const requestId = (pullRequestThreadCommentsRequestsRef.current[thread.id] ?? 0) + 1;
    const seenCursors = page?.seenCursors ?? [];
    const requestedSeenCursors = seenCursors.includes(cursor)
      ? seenCursors
      : [...seenCursors, cursor];
    pullRequestThreadCommentsRequestsRef.current[thread.id] = requestId;
    setPullRequestThreadCommentsPages((current) => ({
      ...current,
      [thread.id]: {
        loading: true,
        error: null,
        nextCursor: cursor,
        seenCursors: requestedSeenCursors
      }
    }));

    try {
      const result = await getGitHubPullRequestReviewThreadComments(target, {
        ...identity,
        number: review.detail.number,
        expectedState: review.detail.state,
        expectedHeadOid: review.detail.headRefOid,
        threadId: thread.id,
        cursor,
        pageSize: GITHUB_REVIEW_THREAD_COMMENT_PAGE_SIZE
      });
      if (
        scopeId !== pullRequestThreadCommentsScopeRef.current
        || requestId !== pullRequestThreadCommentsRequestsRef.current[thread.id]
      ) return;
      if (
        result.number !== review.detail.number
        || result.headRefOid !== review.detail.headRefOid
        || result.threadId !== thread.id
      ) {
        throw new Error(t(
          "回复页来自其他拉取请求、提交或线程；已忽略该结果。",
          "The reply page belongs to another pull request, head, or thread and was ignored."
        ));
      }
      const cursorRepeated = Boolean(
        result.nextCursor
        && requestedSeenCursors.includes(result.nextCursor)
      );
      const nextSeenCursors = result.nextCursor && !cursorRepeated
        ? [...requestedSeenCursors, result.nextCursor]
        : requestedSeenCursors;
      setPullRequestThreads((current) => current.map((item) => (
        item.id === thread.id
          ? {
              ...item,
              comments: mergeReviewComments(item.comments, result.comments),
              commentsTotalCount: result.totalCount,
              commentsNextCursor: cursorRepeated ? null : result.nextCursor
            }
          : item
      )));
      setPullRequestThreadCommentsPages((current) => ({
        ...current,
        [thread.id]: {
          loading: false,
          error: cursorRepeated
            ? t(
                "GitHub 返回了重复的回复游标；已停止加载以避免循环。",
                "GitHub returned a repeated reply cursor. Loading stopped to avoid a loop."
              )
            : null,
          nextCursor: cursorRepeated ? null : result.nextCursor,
          seenCursors: nextSeenCursors
        }
      }));
    } catch (reason) {
      if (
        scopeId !== pullRequestThreadCommentsScopeRef.current
        || requestId !== pullRequestThreadCommentsRequestsRef.current[thread.id]
      ) return;
      setPullRequestThreadCommentsPages((current) => ({
        ...current,
        [thread.id]: {
          loading: false,
          error: failureMessage(
            reason,
            t("无法加载更多回复", "Unable to load more replies")
          ),
          nextCursor: current[thread.id]?.nextCursor ?? cursor,
          seenCursors: current[thread.id]?.seenCursors ?? requestedSeenCursors
        }
      }));
    }
  }, [
    active,
    target,
    githubActionIdentity,
    pullRequestReview,
    pullRequestThreadCommentsPages,
    t
  ]);

  const selectView = (next: GitReviewView) => {
    setPendingDiscard(null);
    setPendingGitAction(null);
    setPendingGitHubAction(null);
    setView(next);
    onViewChange(next);
  };

  const confirmGitHubAction = (
    key: string,
    action: GitHubAction,
    afterSuccess?: () => void
  ) => {
    if (pendingGitHubAction !== key) {
      setPendingGitHubAction(key);
      return;
    }
    setPendingGitHubAction(null);
    void runGitHubAction(action, key).then((success) => {
      if (success) afterSuccess?.();
    });
  };

  const confirmPullRequestReviewAction = (
    key: string,
    action: GitHubAction,
    afterSuccess?: () => void
  ) => {
    const review = pullRequestReview;
    if (!review) return;
    if (pendingGitHubAction !== key) {
      setPendingGitHubAction(key);
      return;
    }
    setPendingGitHubAction(null);
    void runGitHubAction(action, key).then(async (success) => {
      if (success) afterSuccess?.();
      await refreshPullRequestReviewContext(review);
    });
  };

  const selectPullRequestLine = (selection: DiffLineSelection) => {
    const review = pullRequestReview;
    const selectedDiff = pullRequestSelectedDiff ?? (
      review?.diff.path ? review.diff : null
    );
    if (
      !review
      || !selectedDiff
      || selectedDiff.headRefOid !== review.detail.headRefOid
      || pullRequestThreadsHeadOid !== review.detail.headRefOid
      || selection.path !== selectedDiff.path
    ) return;
    const existing = pullRequestLineDrafts.find(
      (draft) => draft.key === lineDraftKey(selection)
    );
    setPullRequestSelectedLine(selection);
    setPullRequestLineComment(existing?.body ?? "");
    setPendingGitHubAction(null);
  };

  const savePullRequestLineDraft = () => {
    const selection = pullRequestSelectedLine;
    const body = pullRequestLineComment.trim();
    const review = pullRequestReview;
    if (
      !selection
      || !body
      || !review
      || (
        pullRequestSelectedDiff?.headRefOid
        ?? (review.diff.path ? review.diff.headRefOid : null)
      ) !== review.detail.headRefOid
      || pullRequestThreadsHeadOid !== review.detail.headRefOid
    ) return;
    const draft: PullRequestLineDraft = {
      key: lineDraftKey(selection),
      path: selection.path,
      line: selection.line,
      side: selection.side,
      body,
      lineText: selection.text,
      kind: selection.kind
    };
    setPullRequestLineDrafts((current) => {
      const index = current.findIndex((item) => item.key === draft.key);
      if (index < 0) return [...current, draft];
      const next = [...current];
      next[index] = draft;
      return next;
    });
    setPullRequestLineComment(body);
    setPendingGitHubAction(null);
  };

  const confirmLocalGitAction = (
    key: string,
    action: GitAction,
    afterSuccess?: () => void
  ) => {
    if (pendingGitAction !== key) {
      setPendingGitAction(key);
      return;
    }
    setPendingGitAction(null);
    void runGitAction(action, key).then((success) => {
      if (success) afterSuccess?.();
    });
  };

  const handleReviewTabKeyDown = (
    event: React.KeyboardEvent<HTMLButtonElement>,
    current: GitReviewView
  ) => {
    if (!["ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key)) return;
    event.preventDefault();
    const currentIndex = VIEWS.findIndex((item) => item.id === current);
    const nextIndex = event.key === "Home" ? 0
      : event.key === "End" ? VIEWS.length - 1
        : event.key === "ArrowRight" ? (currentIndex + 1) % VIEWS.length
          : (currentIndex - 1 + VIEWS.length) % VIEWS.length;
    const next = VIEWS[nextIndex]?.id;
    if (!next) return;
    selectView(next);
    window.requestAnimationFrame(() => document.getElementById(`${tabsId}-tab-${next}`)?.focus());
  };

  const pullConfirmationKey = `pull:${JSON.stringify([
    currentSnapshot.repositoryId,
    currentSnapshot.worktreeId,
    currentSnapshot.branch,
    currentSnapshot.head,
    currentSnapshot.contentRevision,
    selectedRemoteName,
    gitRemoteProofKey(selectedRemote),
    gitUpstreamProofKey(currentUpstream)
  ])}`;
  const pullConfirmationArmed = pendingGitAction === pullConfirmationKey;
  const pushConfirmationKey = `push:${JSON.stringify([
    currentSnapshot.repositoryId,
    currentSnapshot.worktreeId,
    currentSnapshot.branch,
    currentSnapshot.head,
    selectedRemoteName,
    pushRemoteBranch,
    gitRemoteProofKey(selectedRemote),
    gitUpstreamProofKey(currentUpstream)
  ])}`;
  const pushConfirmationArmed = pendingGitAction === pushConfirmationKey;
  const fetchActionKey = `fetch:${JSON.stringify([
    currentSnapshot.repositoryId,
    currentSnapshot.worktreeId,
    gitRemoteProofKey(selectedRemote)
  ])}`;

  const renderHeader = () => (
    <header className="git-review__repository-bar">
      <span className="git-review__repository-mark" aria-hidden="true"><GitBranch size={14} /></span>
      <span className="git-review__repository-copy">
        <strong title={currentSnapshot.branch ?? currentSnapshot.head ?? ""}>
          {currentSnapshot.branch ?? currentSnapshot.head ?? t("尚无提交", "No commits yet")}
        </strong>
        <small>
          {currentUpstream
            ? currentUpstream.isLocal
              ? t("本地/{branch}", "local/{branch}", { branch: currentUpstream.remoteBranch })
              : pullTargetLabel
            : selectedRemote?.name ?? t("仅本地", "Local only")}
          {(currentSnapshot.ahead || currentSnapshot.behind)
            ? ` · ↑${currentSnapshot.ahead} ↓${currentSnapshot.behind}`
            : ""}
        </small>
        {localRemoteDisabledReason && (
          <small className="git-review__remote-note" role="status">
            {localRemoteDisabledReason}
          </small>
        )}
      </span>
      <div className="git-review__repository-actions">
        <label className="git-review__remote-select">
          <span>{t("远端", "Remote")}</span>
          <select
            aria-label={t("Git 远端", "Git remote")}
            value={selectedRemoteName}
            disabled={operationBusy || currentSnapshot.remotes.length === 0}
            onChange={(event) => setSelectedRemoteName(event.target.value)}
          >
            {currentSnapshot.remotes.map((remote) => (
              <option value={remote.name} key={remote.name}>
                {remote.name}
              </option>
            ))}
          </select>
        </label>
        <button
          type="button"
          disabled={repositoryTransitionDisabled || !selectedRemote || selectedRemoteIsLocal}
          title={localRemoteDisabledReason ?? undefined}
          onClick={() => {
            if (!selectedRemote) return;
            void runGitAction({
              type: "fetch",
              expectedRepositoryId: currentSnapshot.repositoryId,
              expectedWorktreeId: currentSnapshot.worktreeId,
              remote: selectedRemote
            }, fetchActionKey);
          }}
        >
          {busyAction === fetchActionKey ? <BusyLabel>{t("获取中", "Fetching")}</BusyLabel> : <><RefreshCw size={13} />{t("获取", "Fetch")}</>}
        </button>
        <button
          type="button"
          disabled={
            repositoryTransitionDisabled
            || selectedRemoteIsLocal
            || !currentUpstream
            || currentUpstream.isLocal
            || !upstreamRemoteProofMatches
            || !currentSnapshot.branch
            || !currentSnapshot.head
          }
          className={pullConfirmationArmed ? "git-review__confirm--armed" : undefined}
          title={localRemoteDisabledReason ?? t(
            "仅允许从结构化上游 {target} 快进拉取",
            "Fast-forward-only pull from the structured upstream {target}",
            { target: pullTargetLabel ?? "—" }
          )}
          onBlur={() => {
            setPendingGitAction((current) => current === pullConfirmationKey ? null : current);
          }}
          onClick={() => {
            if (
              !currentUpstream
              || currentUpstream.isLocal
              || !upstreamRemoteProofMatches
              || !currentSnapshot.branch
              || !currentSnapshot.head
            ) return;
            confirmLocalGitAction(pullConfirmationKey, {
              type: "pull",
              expectedRepositoryId: currentSnapshot.repositoryId,
              expectedWorktreeId: currentSnapshot.worktreeId,
              expectedLocalBranch: currentSnapshot.branch,
              expectedHead: currentSnapshot.head,
              expectedContentRevision: currentSnapshot.contentRevision,
              upstream: currentUpstream,
              ffOnly: true
            });
          }}
        >
          {busyAction === pullConfirmationKey
            ? <BusyLabel>{t("拉取中", "Pulling")}</BusyLabel>
            : pullConfirmationArmed
              ? <><ArrowDownToLine size={13} />{t(
                "确认拉取 {target}",
                "Confirm pull {target}",
                { target: pullTargetLabel ?? "" }
              )}</>
              : <><ArrowDownToLine size={13} />{t("拉取", "Pull")}</>}
        </button>
        <button
          type="button"
          disabled={
            repositoryTransitionDisabled
            || !selectedRemote
            || selectedRemoteIsLocal
            || !currentSnapshot.branch
            || !currentSnapshot.head
            || !pushRemoteBranch
          }
          title={localRemoteDisabledReason ?? (
            selectedRemote && pushRemoteBranch && currentSnapshot.head
              ? t(
                  "推送到 {target}，提交 {head}",
                  "Push to {target}, commit {head}",
                  {
                    target: `${selectedRemote.name}/${pushRemoteBranch}`,
                    head: currentSnapshot.head.slice(0, 8)
                  }
                )
              : undefined
          )}
          className={pushConfirmationArmed ? "git-review__confirm--armed" : undefined}
          onBlur={() => {
            setPendingGitAction((current) => current === pushConfirmationKey ? null : current);
          }}
          onClick={() => {
            if (
              !selectedRemote
              || !currentSnapshot.branch
              || !currentSnapshot.head
              || !pushRemoteBranch
            ) return;
            confirmLocalGitAction(pushConfirmationKey, {
              type: "push",
              expectedRepositoryId: currentSnapshot.repositoryId,
              expectedWorktreeId: currentSnapshot.worktreeId,
              remote: selectedRemote,
              expectedLocalBranch: currentSnapshot.branch,
              remoteBranch: pushRemoteBranch,
              expectedHead: currentSnapshot.head,
              expectedUpstream: currentUpstream,
              setUpstream: currentUpstream === null
            });
          }}
        >
          {busyAction === pushConfirmationKey
            ? <BusyLabel>{t("推送中", "Pushing")}</BusyLabel>
            : pushConfirmationArmed
              ? <><ArrowUpFromLine size={13} />{t(
                "确认推送 {target} · {head}",
                "Confirm push {target} · {head}",
                {
                  target: `${selectedRemote?.name ?? ""}/${pushRemoteBranch ?? ""}`,
                  head: currentSnapshot.head?.slice(0, 8) ?? ""
                }
              )}</>
              : <><ArrowUpFromLine size={13} />{t("推送", "Push")}</>}
        </button>
        <IconButton
          label={t("刷新 Git 状态", "Refresh Git status")}
          disabled={operationBusy}
          onClick={() => void refreshSnapshot()}
        >
          {busyAction === "refresh" ? <LoaderCircle className="spin" size={14} /> : <RefreshCw size={14} />}
        </IconButton>
      </div>
    </header>
  );

  const renderChanges = () => (
    <section
      id={`${tabsId}-panel-changes`}
      className="git-review__view git-review__changes"
      role="tabpanel"
      aria-labelledby={`${tabsId}-tab-changes`}
    >
      <div className="git-review__change-toolbar">
        <span>
          <b>+{currentSnapshot.additions}</b>
          <em>−{currentSnapshot.deletions}</em>
          <small>{t("{count} 个文件", "{count} files", { count: totalChangedFiles })}</small>
        </span>
        <button
          type="button"
          className={stageAllArmed ? "git-review__confirm--armed" : undefined}
          disabled={
            mutationsDisabled
            || stageableFiles === 0
            || (stageAllArmed && !stageAllReviewReady)
          }
          onClick={() => requestStageAll(stageAllArmed ? pendingStageAll : null)}
        >
          {busyAction === "prepare-stage-all"
            ? <BusyLabel>{t("准备中", "Preparing")}</BusyLabel>
            : stageAllArmed
              ? stageAllReviewReady
                ? t("确认全部暂存", "Confirm stage all")
                : <BusyLabel>{t("校验候选差异", "Reviewing candidate diff")}</BusyLabel>
              : t("全部暂存", "Stage all")}
        </button>
        {stageAllArmed && (
          <button
            type="button"
            disabled={operationBusy}
            onClick={cancelStageAllReview}
          >
            {t("取消候选", "Cancel candidate")}
          </button>
        )}
        {!stageAllArmed && (
          <button
            type="button"
            disabled={mutationsDisabled || unstageableFiles === 0}
            onClick={() => void runGitAction({
              type: "unstage_all",
              expectedContentRevision: currentSnapshot.contentRevision
            }, `unstage-all:${currentSnapshot.contentRevision}`)}
          >{t("全部取消", "Unstage all")}</button>
        )}
      </div>
      <div className={`git-review__change-layout${totalChangedFiles ? "" : " git-review__change-layout--empty"}`}>
        {totalChangedFiles ? (
          <nav className="git-review__file-list" aria-label={t("变更文件", "Changed files")}>
            <label className="git-review__file-filter">
              <input
                type="search"
                value={changeFilter}
                aria-label={t("筛选变更文件", "Filter changed files")}
                placeholder={t("筛选文件", "Filter files")}
                onChange={(event) => setChangeFilter(event.target.value)}
              />
              <span role="status">
                {t(
                  "显示 {shown}/{total} 个变更文件",
                  "Showing {shown}/{total} changed files",
                  { shown: changeFiles.length, total: changeMatchedCount }
                )}
              </span>
            </label>
            {visibleChangeGroups.map((group) => (
              <section
                className={`git-review__file-group git-review__file-group--${group.id}`}
                role="group"
                aria-labelledby={`${tabsId}-changes-${group.id}`}
                key={group.id}
              >
                <header className="git-review__file-group-heading">
                  <h3 id={`${tabsId}-changes-${group.id}`}>{group.label}</h3>
                  <span>{group.description}</span>
                  <b>{group.files.length}</b>
                </header>
                {group.files.map((file) => {
                  const staged = gitFileHasStagedChange(file);
                  const unstaged = gitFileHasUnstagedChange(file);
                  const untracked = Boolean(file.untracked || file.status === "untracked");
                  const partiallyStaged = group.id === "partiallyStaged";
                  const stageable = unstaged && (!file.submodule || Boolean(file.submoduleCommitChanged));
                  const discardable = unstaged && !file.submodule;
                  const selected = selectedPath === file.path;
                  const discardArmed = Boolean(
                    pendingDiscard
                    && pendingDiscard.scopeKey === discardScopeKey
                    && pendingDiscard.snapshotKey === currentDiscardSnapshotKey
                    && pendingDiscard.path === file.path
                    && pendingDiscard.includeUntracked === untracked
                    && pendingDiscard.contentRevision === currentSnapshot.contentRevision
                  );
                  const armedDiscardKey = discardArmed ? pendingDiscard?.key ?? null : null;
                  const discardPreparing = busyAction === `prepare-discard:${file.path}`;
                  return (
                    <div className={`git-review__file-row${selected ? " git-review__file-row--selected" : ""}`} key={file.path}>
                      <button
                        type="button"
                        className="git-review__file-main"
                        aria-pressed={selected}
                        onClick={() => {
                          setSelectedPath(file.path);
                          setSelectedChangeFile(file);
                        }}
                      >
                        <span className={`git-review__status git-review__status--${changeTone(file)}`}>
                          {changeStatusLabel(file, t)}
                        </span>
                        <span className="git-review__file-copy">
                          <span className="git-review__file-path" title={file.path}>{file.path}</span>
                          {(partiallyStaged || file.additions !== undefined || file.deletions !== undefined) && (
                            <small className="git-review__file-meta">
                              {partiallyStaged && (
                                <span className="git-review__file-composition">
                                  {t("已暂存 + 未暂存", "Staged + unstaged")}
                                </span>
                              )}
                              {(file.additions !== undefined || file.deletions !== undefined) && (
                                <span className="git-review__file-stats">
                                  <b>+{file.additions ?? 0}</b>
                                  <em>−{file.deletions ?? 0}</em>
                                </span>
                              )}
                            </small>
                          )}
                        </span>
                      </button>
                      <div className="git-review__file-actions">
                        {!stageAllArmed && stageable && (
                          <IconButton
                            label={t("暂存 {path}", "Stage {path}", { path: file.path })}
                            disabled={mutationsDisabled}
                            onClick={() => void runGitAction({ type: "stage", paths: [file.path] }, `stage:${file.path}`)}
                          ><Plus size={13} /></IconButton>
                        )}
                        {!stageAllArmed && staged && (
                          <IconButton
                            label={t("取消暂存 {path}", "Unstage {path}", { path: file.path })}
                            disabled={mutationsDisabled}
                            onClick={() => void runGitAction({ type: "unstage", paths: [file.path] }, `unstage:${file.path}`)}
                          ><RotateCcw size={12} /></IconButton>
                        )}
                        {!stageAllArmed && discardable && (
                          <IconButton
                            label={discardArmed
                              ? untracked
                                ? t("确认永久删除未跟踪文件 {path}", "Confirm permanent deletion of untracked file {path}", { path: file.path })
                                : t("确认丢弃 {path}", "Confirm discard {path}", { path: file.path })
                              : untracked
                                ? t("永久删除未跟踪文件 {path}", "Permanently delete untracked file {path}", { path: file.path })
                                : t("丢弃 {path} 的未暂存更改", "Discard unstaged changes in {path}", { path: file.path })}
                            className={discardArmed ? "git-review__discard--armed" : undefined}
                            disabled={mutationsDisabled}
                            onBlur={() => setPendingDiscard((current) => (
                              current?.key === armedDiscardKey ? null : current
                            ))}
                            onClick={() => {
                              requestDiscard(
                                file.path,
                                untracked,
                                discardArmed ? pendingDiscard : null
                              );
                            }}
                          >{
                            discardPreparing
                              ? <LoaderCircle className="spin" size={12} />
                              : discardArmed
                                ? <Check size={12} />
                                : <Trash2 size={12} />
                          }</IconButton>
                        )}
                      </div>
                    </div>
                  );
                })}
              </section>
            ))}
            {changePageState === "loading" && changeFiles.length === 0 && (
              <div className="git-review__file-list-state" role="status">
                <LoaderCircle className="spin" size={14} />
                {t("正在读取变更文件", "Loading changed files")}
              </div>
            )}
            {changePageError && (
              <div className="git-review__file-list-state git-review__file-list-state--error" role="alert">
                <CircleAlert size={13} />
                {changePageError}
              </div>
            )}
            {changePageState !== "loading" && !changePageError && changeMatchedCount === 0 && (
              <div className="git-review__file-filter-empty">
                {t("没有匹配的变更文件", "No changed files match")}
              </div>
            )}
            {changeNextCursor && (
              <button
                type="button"
                className="git-review__file-list-more"
                disabled={changeLoadingMore}
                onClick={() => void loadChangePage(changeNextCursor)}
              >
                {changeLoadingMore
                  ? <BusyLabel>{t("正在加载", "Loading")}</BusyLabel>
                  : t(
                    "再显示 {count} 个文件",
                    "Show {count} more files",
                    { count: nextChangePageCount }
                  )}
              </button>
            )}
          </nav>
        ) : (
          <div className="git-review__empty"><Check size={20} />{t("工作区没有变更", "Working tree is clean")}</div>
        )}
        {selectedChange && (
          <article className="git-review__diff-pane" aria-label={t("{path} 差异", "{path} diff", { path: selectedChange.path })}>
            <header>
              <span title={selectedChange.path}>{selectedChange.path}</span>
              <div role="group" aria-label={t("差异范围", "Diff scope")}>
                {(["working", "staged", "unstaged"] as const).map((scope) => (
                  <button
                    key={scope}
                    type="button"
                    className={diffScope === scope ? "is-active" : undefined}
                    aria-pressed={diffScope === scope}
                    disabled={(stageAllArmed && scope !== "working")
                      || (scope === "staged" && !gitFileHasStagedChange(selectedChange))
                      || (scope === "unstaged" && !gitFileHasUnstagedChange(selectedChange))}
                    onClick={() => setDiffScope(scope)}
                  >
                    {scope === "working" ? t("全部", "All") : scope === "staged" ? t("已暂存", "Staged") : t("未暂存", "Unstaged")}
                  </button>
                ))}
              </div>
            </header>
            <DiffPreview
              diff={diff}
              loading={diffState === "loading"}
              error={diffError}
              emptyLabel={t("这个范围没有行级差异", "No line-level diff in this scope")}
            />
          </article>
        )}
      </div>
      <footer className="git-review__commit-bar">
        <div className="git-review__commit-input">
          <textarea
            rows={2}
            value={commitMessage}
            placeholder={t("提交说明", "Commit message")}
            aria-label={t("提交说明", "Commit message")}
            onChange={(event) => updateCommitMessage(event.target.value)}
          />
          {commitArmed && pendingCommit && (
            <span className="git-review__commit-proof" role="status">
              {t(
                "{count} 个文件 · 树 {oid}",
                "{count} files · tree {oid}",
                {
                  count: pendingCommit.snapshot.staged,
                  oid: pendingCommit.candidateTreeOid.slice(0, 8)
                }
              )}
            </span>
          )}
        </div>
        <button
          type="button"
          className={`git-review__primary-action${commitArmed ? " git-review__primary-action--armed" : ""}`}
          aria-pressed={commitArmed}
          disabled={repositoryTransitionDisabled || currentSnapshot.staged === 0 || !commitMessage.trim()}
          onClick={requestCommit}
        >
          {busyAction?.startsWith("prepare-commit:")
            ? <BusyLabel>{t("校验中", "Verifying")}</BusyLabel>
            : busyAction?.startsWith("commit:")
            ? <BusyLabel>{t("提交中", "Committing")}</BusyLabel>
            : commitArmed
              ? <><GitCommitHorizontal size={14} />{t(
                  "确认提交 {count} 个文件",
                  "Confirm commit of {count} files",
                  { count: pendingCommit?.snapshot.staged ?? currentSnapshot.staged }
                )}</>
              : <><GitCommitHorizontal size={14} />{t("提交 {count} 个已暂存文件", "Commit {count} staged files", { count: currentSnapshot.staged })}</>}
        </button>
      </footer>
    </section>
  );

  const renderHistory = () => (
    <section
      id={`${tabsId}-panel-history`}
      className="git-review__view"
      role="tabpanel"
      aria-labelledby={`${tabsId}-tab-history`}
    >
      <div className="git-review__section-heading">
        <span><History size={14} /><strong>{t("提交历史", "Commit history")}</strong></span>
        <IconButton label={t("刷新提交历史", "Refresh commit history")} onClick={() => void loadHistory(true)}>
          <RefreshCw size={13} />
        </IconButton>
      </div>
      {historyState === "loading" && history.length === 0 ? (
        <div className="git-review__empty" role="status"><LoaderCircle className="spin" size={18} />{t("正在读取提交", "Loading commits")}</div>
      ) : history.length ? (
        <>
          <ol className="git-review__history-list">
            {history.map((commit) => (
              <li key={commit.oid}>
                <code>{shortOid(commit)}</code>
                <span>
                  <strong>{commit.subject}</strong>
                  <small>{commit.authorName} · {dateLabel(commit.authoredAt, resolvedLanguage)}</small>
                </span>
                {commit.refs?.length ? <em>{commit.refs.join(", ")}</em> : null}
              </li>
            ))}
          </ol>
          {historyError && <div className="git-review__error" role="alert"><CircleAlert size={15} />{historyError}</div>}
          {historyNextCursor && (
            <button
              type="button"
              className="git-review__load-more"
              disabled={historyLoadingMore}
              onClick={() => void loadHistory(false, historyNextCursor)}
            >
              {historyLoadingMore
                ? <BusyLabel>{t("正在加载", "Loading")}</BusyLabel>
                : t("加载更多提交", "Load more commits")}
            </button>
          )}
        </>
      ) : historyError ? (
        <div className="git-review__error" role="alert"><CircleAlert size={15} />{historyError}</div>
      ) : (
        <div className="git-review__empty">{t("这个仓库还没有提交", "This repository has no commits")}</div>
      )}
    </section>
  );

  const renderBranches = () => (
    <section
      id={`${tabsId}-panel-branches`}
      className="git-review__view"
      role="tabpanel"
      aria-labelledby={`${tabsId}-tab-branches`}
    >
      <div className="git-review__stash-bar">
        <span>
          <Archive size={14} />
          <strong>{t("储藏", "Stashes")}</strong>
          <small>{currentSnapshot.stash}</small>
        </span>
        <button
          type="button"
          disabled={
            repositoryTransitionDisabled
            || currentSnapshot.staged + currentSnapshot.unstaged + currentSnapshot.untracked === 0
          }
          className={pendingGitAction === stashKey ? "git-review__confirm--armed" : undefined}
          onBlur={() => setPendingGitAction((current) => current === stashKey ? null : current)}
          onClick={() => confirmLocalGitAction(
            stashKey,
            currentSnapshot.untracked > 0
              ? { type: "stash", includeUntracked: true }
              : { type: "stash" }
          )}
        >
          <Archive size={13} />
          {pendingGitAction === stashKey
            ? currentSnapshot.untracked > 0
              ? t("确认储藏更改与未跟踪文件", "Confirm stash changes and untracked files")
              : t("确认储藏更改", "Confirm stash changes")
            : currentSnapshot.untracked > 0
              ? t("储藏更改与未跟踪文件", "Stash changes and untracked files")
              : t("储藏更改", "Stash changes")}
        </button>
        <button
          type="button"
          disabled={repositoryTransitionDisabled || currentSnapshot.stash === 0}
          className={pendingGitAction === stashPopKey ? "git-review__confirm--armed" : undefined}
          onBlur={() => setPendingGitAction((current) => current === stashPopKey ? null : current)}
          onClick={() => confirmLocalGitAction(stashPopKey, { type: "stash_pop", index: 0 })}
        >
          <ArchiveRestore size={13} />
          {pendingGitAction === stashPopKey
            ? t("确认弹出最近储藏", "Confirm pop latest stash")
            : t("弹出最近储藏", "Pop latest stash")}
        </button>
      </div>
      <form
        className="git-review__create-branch"
        onSubmit={(event) => {
          event.preventDefault();
          const name = newBranchName.trim();
          if (!name) return;
          void runGitAction({ type: "create_branch", name, checkout: true }, "create-branch")
            .then((success) => {
              if (success) {
                setNewBranchName("");
                void loadBranches(true);
              }
            });
        }}
      >
        <input
          value={newBranchName}
          aria-label={t("新分支名称", "New branch name")}
          placeholder={t("新分支名称", "New branch name")}
          onChange={(event) => setNewBranchName(event.target.value)}
        />
        <button type="submit" disabled={repositoryTransitionDisabled || !newBranchName.trim()}>
          <Plus size={13} />{t("新建并切换", "Create and switch")}
        </button>
      </form>
      {branchesState === "loading" ? (
        <div className="git-review__empty" role="status"><LoaderCircle className="spin" size={18} />{t("正在读取分支", "Loading branches")}</div>
      ) : branchesError ? (
        <div className="git-review__error" role="alert">
          <CircleAlert size={15} />
          <span>{branchesError}</span>
          <button type="button" onClick={() => void loadBranches(true)}>{t("重试", "Retry")}</button>
        </div>
      ) : (
        <div className="git-review__branch-groups">
          {(["local", "remote"] as const).map((kind) => {
            const items = branches?.branches.filter((branch) => branch.kind === kind) ?? [];
            if (!items.length) return null;
            return (
              <section key={kind}>
                <h3>{kind === "local" ? t("本地", "Local") : t("远程", "Remote")}</h3>
                {items.map((branch) => {
                  const mergeKey = `merge:${workspaceIdentityKey}:${currentSnapshot.head ?? "unborn"}:${branch.name}:${branch.head}`;
                  const deleteKey = `delete-branch:${workspaceIdentityKey}:${currentSnapshot.head ?? "unborn"}:${branch.name}:${branch.head}`;
                  return (
                    <div
                      className={`git-review__branch-row${branch.current ? " is-current" : ""}`}
                      key={branch.fullName ?? `${kind}:${branch.name}`}
                    >
                      <button
                        type="button"
                        className="git-review__branch-main"
                        disabled={repositoryTransitionDisabled || branch.current || kind === "remote"}
                        onClick={() => void runGitAction({ type: "checkout", branch: branch.name }, `checkout:${branch.name}`)
                          .then((success) => success && void loadBranches(true))}
                      >
                        <GitBranch size={14} />
                        <span>
                          <strong>{branch.name}</strong>
                          <small>{branch.upstream ?? branch.head ?? ""}</small>
                        </span>
                        {(branch.ahead || branch.behind) ? <em>↑{branch.ahead} ↓{branch.behind}</em> : null}
                        {branch.current && <Check size={13} />}
                      </button>
                      {kind === "local" && !branch.current && (
                        <div className="git-review__branch-actions">
                          <IconButton
                            label={pendingGitAction === mergeKey
                              ? t("确认合并 {branch}", "Confirm merge {branch}", { branch: branch.name })
                              : t("合并 {branch}", "Merge {branch}", { branch: branch.name })}
                            className={pendingGitAction === mergeKey ? "git-review__confirm--armed" : undefined}
                            disabled={repositoryTransitionDisabled || !currentSnapshot.head || !branch.head}
                            onBlur={() => setPendingGitAction((current) => current === mergeKey ? null : current)}
                            onClick={() => confirmLocalGitAction(
                              mergeKey,
                              {
                                type: "merge",
                                branch: branch.name,
                                expectedHead: currentSnapshot.head ?? "",
                                expectedBranchOid: branch.head ?? ""
                              },
                              () => void loadBranches(true)
                            )}
                          >
                            <GitMerge size={13} />
                          </IconButton>
                          <IconButton
                            label={pendingGitAction === deleteKey
                              ? t("确认删除分支 {branch}", "Confirm delete branch {branch}", { branch: branch.name })
                              : t("删除分支 {branch}", "Delete branch {branch}", { branch: branch.name })}
                            className={pendingGitAction === deleteKey ? "git-review__confirm--armed" : undefined}
                            disabled={repositoryTransitionDisabled || !branch.head}
                            onBlur={() => setPendingGitAction((current) => current === deleteKey ? null : current)}
                            onClick={() => confirmLocalGitAction(
                              deleteKey,
                              {
                                type: "delete_branch",
                                name: branch.name,
                                expectedHead: currentSnapshot.head ?? "",
                                expectedOid: branch.head ?? ""
                              },
                              () => void loadBranches(true)
                            )}
                          >
                            <Trash2 size={12} />
                          </IconButton>
                        </div>
                      )}
                    </div>
                  );
                })}
              </section>
            );
          })}
        </div>
      )}
    </section>
  );

  const renderCompare = () => (
    <section
      id={`${tabsId}-panel-compare`}
      className="git-review__view"
      role="tabpanel"
      aria-labelledby={`${tabsId}-tab-compare`}
    >
      <form
        className="git-review__compare-controls"
        onSubmit={(event) => {
          event.preventDefault();
          void loadComparison();
        }}
      >
        <label>
          <span>{t("基准", "Base")}</span>
          <select
            value={compareBase}
            onChange={(event) => {
              clearComparison();
              setCompareBase(event.target.value);
            }}
          >
            {!comparisonBranches.some((branch) => branch.name === compareBase) && compareBase && <option value={compareBase}>{compareBase}</option>}
            {comparisonBranches.map((branch) => <option key={branch.fullName ?? `${branch.kind}:${branch.name}`} value={branch.name}>{branch.name}</option>)}
          </select>
        </label>
        <GitCompareArrows size={15} aria-hidden="true" />
        <label>
          <span>{t("目标", "Head")}</span>
          <select
            value={compareHead}
            onChange={(event) => {
              clearComparison();
              setCompareHead(event.target.value);
            }}
          >
            {!comparisonBranches.some((branch) => branch.name === compareHead) && compareHead && <option value={compareHead}>{compareHead}</option>}
            {comparisonBranches.map((branch) => <option key={branch.fullName ?? `${branch.kind}:${branch.name}`} value={branch.name}>{branch.name}</option>)}
          </select>
        </label>
        <button
          type="submit"
          disabled={!compareBase || !compareHead || compareBase === compareHead || compareState === "loading"}
        >
          {compareState === "loading" ? <BusyLabel>{t("比较中", "Comparing")}</BusyLabel> : t("比较", "Compare")}
        </button>
      </form>
      <div className="git-review__comparison">
        {compareDiff?.files.length ? (
          <nav aria-label={t("比较文件", "Compared files")}>
            {compareDiff.files.map((file) => (
              <button
                type="button"
                className={selectedComparePath === file.path ? "is-selected" : undefined}
                key={file.path}
                onClick={() => {
                  void loadComparison(file.path);
                }}
              >
                <span className={`git-review__status git-review__status--${changeTone(file)}`}>{changeStatusLabel(file, t)}</span>
                <span>{file.path}</span>
              </button>
            ))}
          </nav>
        ) : null}
        <article>
          <DiffPreview
            diff={compareSelectedDiff ?? compareDiff}
            loading={compareState === "loading"}
            error={compareError}
            emptyLabel={compareBase && compareHead && compareBase === compareHead
              ? t("请选择两个不同的分支", "Choose two different branches")
              : compareState === "ready" && compareDiff
                ? t("两个分支没有差异", "There are no differences between these branches")
                : t("选择两个分支开始比较", "Choose two branches to compare")}
          />
        </article>
      </div>
    </section>
  );

  /**
   * Browser entry for the pull request currently on screen.
   *
   * The address comes from the API response rather than being assembled here, so a
   * GitHub Enterprise host stays intact. An address the app cannot open produces no
   * anchor at all, because a dead link is worse than none. The document-level
   * external-link interceptor turns the click into `open_external_url`.
   */
  const renderPullRequestBrowserLink = (pullRequest: GitHubPullRequest) => {
    const href = externalHttpUrl(pullRequest.url);
    if (!href) return null;
    const label = t("在浏览器中打开 PR", "Open the PR in a browser");
    return (
      <a
        className="git-review__pr-open"
        href={href}
        aria-label={label}
        title={label}
        rel="noreferrer"
        target="_blank"
      >
        <ExternalLink size={13} aria-hidden="true" />
      </a>
    );
  };

  const renderPullRequestDetail = () => {
    const summary = selectedPullRequest;
    if (!pullRequestReview) {
      if (!summary) {
        return <div className="git-review__empty"><Github size={20} />{t("选择拉取请求以审阅", "Select a pull request to review")}</div>;
      }
      return (
        <article className="git-review__pr-detail">
          <header>
            <button type="button" onClick={closePullRequestReview}>
              <ChevronLeft size={14} />{t("返回", "Back")}
            </button>
            <span>
              <strong>#{summary.number} {summary.title}</strong>
              <small>{summary.headRefName} → {summary.baseRefName} · {summary.author ?? t("未知作者", "Unknown author")}</small>
            </span>
            {renderPullRequestBrowserLink(summary)}
          </header>
          {pullRequestState === "loading" ? (
            <div className="git-review__empty" role="status">
              <LoaderCircle className="spin" size={18} />
              {t("正在读取拉取请求", "Loading pull request")}
            </div>
          ) : (
            <div className="git-review__error" role="alert">
              <CircleAlert size={15} />
              <span>{githubError ?? t("无法读取拉取请求", "Unable to load pull request")}</span>
              <button type="button" onClick={() => void openPullRequest(summary)}>
                {t("重试", "Retry")}
              </button>
            </div>
          )}
        </article>
      );
    }
    const { detail: pullRequest, diff: pullRequestDiff } = pullRequestReview;
    const reviewDecision = pullRequest.reviewDecision?.toUpperCase() ?? "";
    const pullRequestChecks = pullRequest.checks ?? [];
    const readinessContextMatches = Boolean(
      pullRequestReadiness
      && githubRepository
      && readinessMatchesPullRequest(
        pullRequestReadiness,
        githubRepository,
        selectedPullRequest ?? pullRequest,
        pullRequest
      )
    );
    const readiness = pullRequestReadinessState === "ready" && readinessContextMatches
      ? pullRequestReadiness
      : null;
    const readinessChecksAvailable = Boolean(
      readiness?.checks.availability === "available"
      && readiness.checks.value
    );
    const readinessChecks = readinessChecksAvailable
      ? readiness!.checks.value!.checks
      : null;
    const requiredChecks = readinessChecks?.filter((check) => check.required) ?? [];
    const optionalChecks = readinessChecks?.filter((check) => !check.required) ?? [];
    const requiredChecksBlocking = requiredChecks.filter(
      (check) => !readinessCheckIsSuccessfulTerminal(check)
    );
    const unclassifiedChecks = readinessChecks === null ? pullRequestChecks : [];
    const mergeMethods = allowedMergeMethods(readiness);
    const selectedMergeMethod = mergeMethods.includes(pullRequestMergeMethod)
      ? pullRequestMergeMethod
      : mergeMethods[0] ?? "squash";
    const activeAutoMerge = readiness?.autoMerge.availability === "available"
      ? readiness.autoMerge.value
      : null;
    const activeAutoMergeMethod = normalizeMergeMethod(activeAutoMerge?.mergeMethod);
    const mergeQueueState = readiness?.mergeQueue.availability === "available"
      ? readiness.mergeQueue.value
      : null;
    const mergeQueueStateIsConsistent = Boolean(
      mergeQueueState
      && (
        (!mergeQueueState.isInQueue && mergeQueueState.entry === null)
        || (
          mergeQueueState.enabled
          && mergeQueueState.isInQueue
          && mergeQueueState.entry !== null
        )
      )
    );
    const mergeQueueAllowsImmediate = Boolean(
      mergeQueueStateIsConsistent
      && mergeQueueState
      && !mergeQueueState.enabled
      && !mergeQueueState.isInQueue
      && mergeQueueState.entry === null
    );
    const mergeQueueIsInQueue = Boolean(
      mergeQueueStateIsConsistent
      && mergeQueueState?.enabled
      && mergeQueueState.isInQueue
      && mergeQueueState.entry
    );
    const readinessRoute = readiness
      ? `${readiness.identity.headRepository?.nameWithOwner
        ?? t("已删除的来源仓库", "Deleted head repository")}:${readiness.identity.headRefName}`
        + ` → ${readiness.identity.baseRepository.nameWithOwner}:${readiness.identity.baseRefName}`
      : null;
    const reviewedHeadMatches = pullRequestDiff.headRefOid === pullRequest.headRefOid;
    const reviewThreadsHeadMatches = pullRequestThreadsHeadOid === pullRequest.headRefOid;
    const reviewContextMatches = reviewedHeadMatches
      && reviewThreadsHeadMatches
      && pullRequestThreadsState === "ready";
    const displayedPullRequestDiff = pullRequestDiff.files.length > 0
      ? pullRequestSelectedDiff
      : pullRequestDiff;
    const reviewedPath = pullRequestSelectedPath ?? displayedPullRequestDiff?.path ?? null;
    const selectedFileThreads = reviewedPath
      ? pullRequestThreads.filter((thread) => thread.path === reviewedPath)
      : [];
    const reviewBody = pullRequestReviewBody.trim();
    const reviewComments = pullRequestLineDrafts.map((draft) => ({
      path: draft.path,
      line: draft.line,
      side: draft.side,
      body: draft.body
    }));
    const pullRequestActionScope = JSON.stringify([
      githubWriteScopeKey,
      pullRequest.number,
      pullRequest.headRefOid,
      pullRequest.state
    ]);
    const mergeConfirmationKey = readiness
      ? `pr-merge:${JSON.stringify([
          githubWriteScopeKey,
          readiness.identity.repository.host,
          readiness.identity.repository.nodeId,
          readiness.identity.repository.nameWithOwner,
          readiness.identity.pullRequestNodeId,
          readiness.identity.number,
          readiness.identity.state,
          readiness.identity.draft,
          readiness.identity.baseRepository.nodeId,
          readiness.identity.baseRepository.nameWithOwner,
          readiness.identity.baseRefName,
          readiness.identity.baseRefOid,
          readiness.identity.headRepository?.nodeId ?? null,
          readiness.identity.headRepository?.nameWithOwner ?? null,
          readiness.identity.headRefName,
          readiness.identity.headRefOid,
          readiness.identityRevision,
          readiness.readinessRevision,
          readiness.mergeQueue.availability,
          mergeQueueState?.enabled ?? null,
          mergeQueueState?.isInQueue ?? null,
          mergeQueueState?.entry?.entryId ?? null,
          mergeQueueState?.entry?.position ?? null,
          mergeQueueState?.entry?.state ?? null,
          selectedMergeMethod
        ])}`
      : "pr-merge:unbound";
    const mergeSafetyDescriptionId = `${tabsId}-merge-safety-${pullRequest.number}`;
    const checkoutConfirmationKey = `pr-checkout:${JSON.stringify([
      pullRequestActionScope,
      currentSnapshot.head,
      currentSnapshot.contentRevision,
      currentSnapshot.isClean,
      currentSnapshot.operation
    ])}`;
    const closeConfirmationKey = `pr-close:${pullRequestActionScope}`;
    const reopenConfirmationKey = `pr-reopen:${pullRequestActionScope}`;
    const submitReviewConfirmationKey = `pr-review-submit:${JSON.stringify([
      pullRequestActionScope,
      pullRequestReviewEvent,
      reviewBody,
      reviewComments
    ])}`;
    const reviewNeedsBody = pullRequestReviewEvent !== "approve";
    const reviewHasContent = pullRequestReviewEvent !== "comment"
      || Boolean(reviewBody)
      || reviewComments.length > 0;
    const reviewActionsDisabled = githubActionsDisabled
      || pullRequestThreadsLoadingMore
      || !reviewContextMatches;
    const approvalBlockedByDiff = pullRequestReviewEvent === "approve" && (
      pullRequestDiff.truncated
      || pullRequestDiffState === "error"
      || !reviewedHeadMatches
    );
    const approvalBlockReason = !approvalBlockedByDiff
      ? null
      : !reviewedHeadMatches
        ? t(
            "拉取请求提交与差异 HEAD 不一致，无法通过审阅。",
            "The pull request and diff heads do not match, so approval is unavailable."
          )
        : pullRequestDiffState === "error"
          ? t(
              "差异读取失败，无法确认完整变更，因此不能通过审阅。",
              "The diff could not be read, so the review cannot be approved."
            )
          : t(
              "完整差异已截断，无法确认全部变更，因此不能通过审阅。",
              "The full diff is truncated, so the review cannot be approved."
            );
    const reviewSubmitDisabled = reviewActionsDisabled
      || pullRequest.state !== "open"
      || !reviewHasContent
      || (reviewNeedsBody && !reviewBody)
      || approvalBlockedByDiff;
    const mergeStateStatus = readiness?.mergePolicy.mergeStateStatus.trim().toUpperCase() ?? "";
    const mergeStateAllowsProof = (
      (mergeStateStatus === "CLEAN" || mergeStateStatus === "HAS_HOOKS")
      && (!readinessChecksAvailable || requiredChecksBlocking.length === 0)
    ) || (
      mergeStateStatus === "UNSTABLE"
      && readinessChecksAvailable
      && requiredChecksBlocking.length === 0
    );
    const mergeReviewComplete = !pullRequestDiff.truncated
      && pullRequestDiffState !== "error"
      && reviewedHeadMatches;
    const mergeQueueBlockedReason = !mergeQueueStateIsConsistent
      ? t("无法确认队列要求。", "Unable to confirm merge queue requirements.")
      : mergeQueueIsInQueue
        ? t(
            "已在合并队列中（位置 {position}，状态 {state}）。",
            "Already in the merge queue (position {position}, state {state}).",
            {
              position: mergeQueueState!.entry!.position,
              state: mergeQueueState!.entry!.state
            }
          )
        : mergeQueueState!.enabled
          ? t(
              "目标分支要求合并队列；即时合并不可用。",
              "The target branch requires a merge queue; immediate merge is unavailable."
            )
          : null;
    const mergeProofReady = Boolean(
      readiness
      && pullRequest.state === "open"
      && readiness.identity.state.toLowerCase() === "open"
      && !pullRequest.draft
      && !readiness.identity.draft
      && readiness.mergePolicy.mergeable.trim().toUpperCase() === "MERGEABLE"
      && mergeQueueAllowsImmediate
      && mergeStateAllowsProof
      && mergeMethods.length > 0
      && mergeReviewComplete
    );
    const proofBoundMergeAction: GitHubAction | null = (
      readiness
      && githubActionIdentity
      && pullRequest.state === "open"
    ) ? {
        ...githubActionIdentity,
        type: "merge_pull_request",
        number: readiness.identity.number,
        expectedHeadOid: readiness.identity.headRefOid,
        expectedBaseOid: readiness.identity.baseRefOid,
        expectedState: pullRequest.state,
        expectedIdentityRevision: readiness.identityRevision,
        expectedReadinessRevision: readiness.readinessRevision,
        method: selectedMergeMethod
      } : null;
    const mergeActionDisabled = githubActionsDisabled
      || !mergeProofReady
      || !proofBoundMergeAction;
    const mergeBlockedReason = pullRequestReadinessState === "loading"
      ? t("正在读取合并就绪度。", "Loading merge readiness.")
      : pullRequestReadinessState === "error"
        ? t("合并就绪度不可用，无法证明本次写入。", "Merge readiness is unavailable, so this write cannot be proven.")
        : !readiness
          ? t("就绪度与当前仓库、分支或提交不匹配。", "Readiness does not match the current repository, branch, or commit.")
          : pullRequest.state !== "open" || readiness.identity.state.toLowerCase() !== "open"
            ? t("只有打开的拉取请求可以合并。", "Only open pull requests can be merged.")
            : pullRequest.draft || readiness.identity.draft
              ? t("草稿拉取请求不能合并。", "Draft pull requests cannot be merged.")
              : readiness.mergePolicy.mergeable.trim().toUpperCase() !== "MERGEABLE"
                ? t("GitHub 尚未确认该拉取请求可合并。", "GitHub has not confirmed that this pull request is mergeable.")
                : mergeQueueBlockedReason
                  ? mergeQueueBlockedReason
                  : readinessChecksAvailable && requiredChecksBlocking.length > 0
                    ? t(
                        "{count} 项必需检查尚未成功完成。",
                        "{count} required checks have not completed successfully.",
                        { count: requiredChecksBlocking.length }
                      )
                    : mergeStateStatus === "UNSTABLE" && !readinessChecksAvailable
                      ? t(
                          "合并状态不稳定且必需检查分类不可用，无法安全合并。",
                          "The merge state is unstable and required-check classification is unavailable, so merge is disabled."
                        )
                    : !mergeStateAllowsProof
                      ? mergeStateLabel(readiness.mergePolicy.mergeStateStatus, t)
                      : mergeMethods.length === 0
                        ? t("仓库没有启用可用的合并方式。", "The repository has no enabled merge method.")
                        : !mergeReviewComplete
                          ? t("当前差异不完整或与 readiness HEAD 不一致。", "The current diff is incomplete or does not match the readiness head.")
                          : githubActionsDisabled
                            ? mutationDisabledReason
                              ?? t("GitHub 写入身份或工作区当前不可用。", "The GitHub write identity or workspace is currently unavailable.")
                            : null;
    const mergeConfirmationArmed = !mergeActionDisabled
      && pendingGitHubAction === mergeConfirmationKey;
    const mergeSafetyText = mergeActionDisabled
      ? mergeBlockedReason
      : mergeConfirmationArmed
        ? t(
            "再次点击将按已确认的 base {base}、head {head} 与 readiness revision 执行。",
            "Click again to execute against the confirmed base {base}, head {head}, and readiness revision.",
            {
              base: readiness!.identity.baseRefOid.slice(0, 8),
              head: readiness!.identity.headRefOid.slice(0, 8)
            }
          )
        : t(
            "首次点击只锁定 base/head、身份与就绪证明；再次点击才会合并。",
            "The first click only locks the base/head, identity, and readiness proof; the second click merges."
          );
    return (
      <article className="git-review__pr-detail">
        <header>
          <button type="button" onClick={closePullRequestReview}>
            <ChevronLeft size={14} />{t("返回", "Back")}
          </button>
          <span>
            <strong>#{pullRequest.number} {pullRequest.title}</strong>
            <small>{pullRequest.headRefName} → {pullRequest.baseRefName} · {pullRequest.author ?? t("未知作者", "Unknown author")}</small>
          </span>
          {renderPullRequestBrowserLink(pullRequest)}
        </header>
        {readinessRoute && readiness && (
          <div
            className="git-review__pr-route"
            role="group"
            aria-label={t("拉取请求仓库路径", "Pull request repository route")}
            title={readinessRoute}
          >
            <GitCompareArrows size={12} aria-hidden="true" />
            <code>{readinessRoute}</code>
            <small>
              {readiness.identity.headRefOid.slice(0, 8)}
              {" → "}
              {readiness.identity.baseRefOid.slice(0, 8)}
            </small>
          </div>
        )}
        {githubRepository && (
          <GitHubWriteIdentity
            repository={githubRepository}
            headOid={pullRequest.headRefOid}
          />
        )}
        {pullRequest.body && <p>{pullRequest.body}</p>}
        <div className="git-review__pr-guardrails" aria-label={t("合并检查", "Merge checks")}>
          {readiness ? (
            <span className={`is-${mergeStateTone(readiness.mergePolicy.mergeStateStatus)}`}>
              {mergeStateLabel(readiness.mergePolicy.mergeStateStatus, t)}
            </span>
          ) : pullRequest.mergeable !== undefined && pullRequest.mergeable !== null ? (
            <span className={pullRequest.mergeable ? "is-success" : "is-failure"}>
              {pullRequest.mergeable ? t("可合并", "Mergeable") : t("存在冲突", "Conflicts")}
            </span>
          ) : null}
          {reviewDecision && (
            <span className={reviewDecision === "APPROVED" ? "is-success" : reviewDecision === "CHANGES_REQUESTED" ? "is-failure" : "is-pending"}>
              {reviewDecision === "APPROVED"
                ? t("审阅通过", "Approved")
                : reviewDecision === "CHANGES_REQUESTED"
                  ? t("要求修改", "Changes requested")
                  : t("等待审阅", "Review pending")}
            </span>
          )}
          {readinessChecks !== null ? (
            <span className={requiredChecksBlocking.length === 0
              ? "is-success"
              : requiredChecksBlocking.some((check) => readinessCheckTone(check) === "failure")
                ? "is-failure"
                : "is-pending"}
            >
              {requiredChecksBlocking.length === 0
                ? t("必需检查通过", "Required checks passed")
                : t(
                    "{count} 项必需检查未通过",
                    "{count} required checks not passing",
                    { count: requiredChecksBlocking.length }
                  )}
            </span>
          ) : pullRequestReadinessState === "loading" ? (
            <span className="is-pending">{t("正在读取就绪度", "Loading readiness")}</span>
          ) : (
            <span className="is-pending">{t("检查未分类", "Checks unclassified")}</span>
          )}
          {activeAutoMerge && (
            <span className="is-pending">
              {t("自动合并", "Auto-merge")}
              {" · "}
              {activeAutoMergeMethod
                ? mergeMethodLabel(activeAutoMergeMethod, t)
                : activeAutoMerge.mergeMethod}
              {activeAutoMerge.enabledBy ? ` · @${activeAutoMerge.enabledBy}` : ""}
            </span>
          )}
          {mergeQueueStateIsConsistent && mergeQueueState?.enabled ? (
            <span className="is-pending">
              {t("目标分支要求合并队列", "Target branch requires a merge queue")}
            </span>
          ) : mergeQueueAllowsImmediate ? (
            <span className="is-success">
              {t("无需合并队列", "Merge queue not required")}
            </span>
          ) : readiness ? (
            <span className="is-pending">
              {t("无法确认队列要求", "Unable to confirm merge queue requirements")}
            </span>
          ) : null}
          {mergeQueueIsInQueue && (
            <span className="is-pending">
              {t(
                "合并队列 · #{position} · {state}",
                "Merge queue · #{position} · {state}",
                {
                  position: mergeQueueState!.entry!.position,
                  state: mergeQueueState!.entry!.state
                }
              )}
            </span>
          )}
        </div>
        {pullRequestReadinessState === "error" && (
          <div className="git-review__pr-readiness-status" role="status">
            <CircleAlert size={12} aria-hidden="true" />
            <span>{pullRequestReadinessError}</span>
            <button
              type="button"
              onClick={() => {
                pullRequestReadinessRef.current += 1;
                setPullRequestReadiness(null);
                setPullRequestReadinessError(null);
                setPullRequestReadinessState("idle");
              }}
            >
              {t("重试", "Retry")}
            </button>
          </div>
        )}
        {(readinessChecks !== null || unclassifiedChecks.length > 0) && (
          <details className="git-review__pr-checks">
            <summary>
              <span>{t("检查详情", "Check details")}</span>
              <small>
                {readinessChecks !== null
                  ? t(
                      "必需 {required} · 其他 {optional}",
                      "{required} required · {optional} optional",
                      { required: requiredChecks.length, optional: optionalChecks.length }
                    )
                  : t(
                      "{count} 项未分类",
                      "{count} unclassified",
                      { count: unclassifiedChecks.length }
                    )}
              </small>
            </summary>
            <div>
              {readinessChecks !== null ? (
                <>
                  <section aria-label={t("必需检查", "Required checks")}>
                    <header>
                      <strong>{t("必需", "Required")}</strong>
                      <small>
                        {requiredChecksBlocking.length === 0
                          ? t("不阻断", "Not blocking")
                          : t(
                              "{count} 项阻断",
                              "{count} blocking",
                              { count: requiredChecksBlocking.length }
                            )}
                      </small>
                    </header>
                    {requiredChecks.length > 0 ? (
                      <div role="list">
                        {requiredChecks.map((check) => (
                          <div
                            role="listitem"
                            className={`git-review__pr-check git-review__pr-check--${readinessCheckTone(check)}`}
                            key={check.nodeId}
                            title={check.link ?? undefined}
                          >
                            <span>
                              <strong>{check.name}</strong>
                              {(check.workflow || check.description) && (
                                <small>
                                  {[check.workflow, check.description].filter(Boolean).join(" · ")}
                                </small>
                              )}
                            </span>
                            <em>{readinessCheckStateLabel(check, t)}</em>
                          </div>
                        ))}
                      </div>
                    ) : (
                      <p>{t("仓库没有为此分支声明必需检查", "No required checks are declared for this branch")}</p>
                    )}
                  </section>
                  <section aria-label={t("其他检查", "Optional checks")}>
                    <header>
                      <strong>{t("其他", "Optional")}</strong>
                      <small>{t("不作为阻断依据", "Never used as a blocker")}</small>
                    </header>
                    {optionalChecks.length > 0 ? (
                      <div role="list">
                        {optionalChecks.map((check) => (
                          <div
                            role="listitem"
                            className={`git-review__pr-check git-review__pr-check--${readinessCheckTone(check)}`}
                            key={check.nodeId}
                            title={check.link ?? undefined}
                          >
                            <span>
                              <strong>{check.name}</strong>
                              {(check.workflow || check.description) && (
                                <small>
                                  {[check.workflow, check.description].filter(Boolean).join(" · ")}
                                </small>
                              )}
                            </span>
                            <em>{readinessCheckStateLabel(check, t)}</em>
                          </div>
                        ))}
                      </div>
                    ) : (
                      <p>{t("没有其他检查", "No optional checks")}</p>
                    )}
                  </section>
                </>
              ) : (
                <section aria-label={t("未分类的旧版检查", "Unclassified legacy checks")}>
                  <header>
                    <strong>{t("未分类", "Unclassified")}</strong>
                    <small>{t("不作为阻断依据", "Never used as a blocker")}</small>
                  </header>
                  <div role="list">
                    {unclassifiedChecks.map((check, index) => (
                      <div
                        role="listitem"
                        className={`git-review__pr-check git-review__pr-check--${checkTone(check.state)}`}
                        key={`${check.name}:${check.workflow ?? ""}:${check.link ?? ""}:${index}`}
                        title={check.link ?? undefined}
                      >
                        <span>
                          <strong>{check.name}</strong>
                          {(check.workflow || check.description) && (
                            <small>
                              {[check.workflow, check.description].filter(Boolean).join(" · ")}
                            </small>
                          )}
                        </span>
                        <em>{checkStateLabel(check.state, t)}</em>
                      </div>
                    ))}
                  </div>
                </section>
              )}
            </div>
          </details>
        )}
        <div className="git-review__pr-actions">
          <button
            type="button"
            disabled={
              githubActionsDisabled
              || !currentSnapshot.isClean
              || !currentSnapshot.head
              || currentSnapshot.operation !== null
            }
            className={pendingGitHubAction === checkoutConfirmationKey ? "git-review__confirm--armed" : undefined}
            onBlur={() => setPendingGitHubAction((current) => current === checkoutConfirmationKey ? null : current)}
            onClick={() => confirmGitHubAction(
              checkoutConfirmationKey,
              {
                ...githubActionIdentity!,
                type: "checkout_pull_request",
                number: pullRequest.number,
                expectedHeadOid: pullRequest.headRefOid,
                expectedState: pullRequest.state,
                expectedLocalHeadOid: currentSnapshot.head!,
                expectedContentRevision: currentSnapshot.contentRevision
              }
            )}
          >{pendingGitHubAction === checkoutConfirmationKey ? t("确认检出", "Confirm checkout") : t("检出", "Checkout")}</button>
          {pullRequest.state === "open" && (
            <>
              <div className="git-review__pr-merge-control">
                {mergeMethods.length > 0 && (
                  <select
                    aria-label={t("合并方式", "Merge method")}
                    value={selectedMergeMethod}
                    disabled={Boolean(busyAction)}
                    onChange={(event) => {
                      setPullRequestMergeMethod(event.target.value as PullRequestMergeMethod);
                      setPendingGitHubAction((current) => (
                        current?.startsWith("pr-merge:") ? null : current
                      ));
                    }}
                  >
                    {mergeMethods.map((method) => (
                      <option value={method} key={method}>
                        {mergeMethodLabel(method, t)}
                      </option>
                    ))}
                  </select>
                )}
                <button
                  key={mergeConfirmationKey}
                  type="button"
                  disabled={mergeActionDisabled}
                  className={mergeConfirmationArmed ? "git-review__confirm--armed" : undefined}
                  aria-describedby={mergeSafetyDescriptionId}
                  title={mergeSafetyText ?? undefined}
                  onBlur={() => setPendingGitHubAction((current) => (
                    current === mergeConfirmationKey ? null : current
                  ))}
                  onClick={() => {
                    if (!proofBoundMergeAction) return;
                    confirmGitHubAction(
                      mergeConfirmationKey,
                      proofBoundMergeAction,
                      () => {
                        void refreshPullRequestReviewContext(pullRequestReview);
                      }
                    );
                  }}
                >
                  {mergeConfirmationArmed
                    ? t(
                        "确认{method}",
                        "Confirm {method}",
                        { method: mergeMethodLabel(selectedMergeMethod, t) }
                      )
                    : mergeMethods.length > 0
                      ? mergeMethodLabel(selectedMergeMethod, t)
                    : t("合并不可用", "Merge unavailable")}
                </button>
              </div>
              <small
                className="git-review__pr-merge-safety"
                id={mergeSafetyDescriptionId}
              >
                {mergeSafetyText}
              </small>
              <button
                type="button"
                disabled={githubActionsDisabled}
                className={pendingGitHubAction === closeConfirmationKey ? "git-review__confirm--armed" : undefined}
                onBlur={() => setPendingGitHubAction((current) => current === closeConfirmationKey ? null : current)}
                onClick={() => confirmGitHubAction(
                  closeConfirmationKey,
                  {
                    ...githubActionIdentity!,
                    type: "close_pull_request",
                    number: pullRequest.number,
                    expectedHeadOid: pullRequest.headRefOid,
                    expectedState: pullRequest.state
                  }
                )}
              >{pendingGitHubAction === closeConfirmationKey ? t("确认关闭", "Confirm close") : t("关闭", "Close")}</button>
            </>
          )}
          {pullRequest.state === "closed" && (
            <button
              type="button"
              disabled={githubActionsDisabled}
              className={pendingGitHubAction === reopenConfirmationKey ? "git-review__confirm--armed" : undefined}
              onBlur={() => setPendingGitHubAction((current) => current === reopenConfirmationKey ? null : current)}
              onClick={() => confirmGitHubAction(
                reopenConfirmationKey,
                {
                  ...githubActionIdentity!,
                  type: "reopen_pull_request",
                  number: pullRequest.number,
                  expectedHeadOid: pullRequest.headRefOid,
                  expectedState: pullRequest.state
                }
              )}
            >{pendingGitHubAction === reopenConfirmationKey ? t("确认重新打开", "Confirm reopen") : t("重新打开", "Reopen")}</button>
          )}
        </div>
        <div className="git-review__pr-stats">
          <b>+{pullRequest.additions}</b><em>−{pullRequest.deletions}</em>
          <span>{t("{files} 个文件 · {commits} 个提交", "{files} files · {commits} commits", {
            files: pullRequest.changedFiles,
            commits: pullRequest.commits
          })}</span>
        </div>
        {pullRequestDiff.truncated && pullRequestDiff.files.length > 0 && (
          <div className="git-review__warning" role="alert">
            <CircleAlert size={15} />
            {t(
              "完整差异超过安全读取上限，当前审阅不完整；请逐个文件审阅，合并保持禁用。",
              "The full diff exceeded the safe read limit, so this review is incomplete. Review files individually; merging remains disabled."
            )}
          </div>
        )}
        <div className="git-review__pr-diff">
          {pullRequestDiff.files.length > 0 && (
            <nav aria-label={t("拉取请求文件", "Pull request files")}>
              {pullRequestDiff.files.map((file) => (
                <button
                  type="button"
                  className={pullRequestSelectedPath === file.path ? "is-selected" : undefined}
                  key={file.path}
                  onClick={() => void loadPullRequestFile(pullRequestReview, file.path)}
                >
                  <span className={`git-review__status git-review__status--${changeTone(file)}`}>
                    {changeStatusLabel(file, t)}
                  </span>
                  <span>{file.path}</span>
                  <small><b>+{file.additions ?? 0}</b><em>−{file.deletions ?? 0}</em></small>
                </button>
              ))}
            </nav>
          )}
          <article>
            <DiffPreview
              diff={displayedPullRequestDiff}
              loading={pullRequestDiff.files.length > 0 && pullRequestDiffState === "loading"}
              error={pullRequestDiff.files.length > 0 ? pullRequestDiffError : githubError}
              emptyLabel={pullRequestDiff.files.length > 0
                ? t("选择文件以读取差异", "Select a file to load its diff")
                : t("没有可显示的拉取请求差异", "No pull request diff to show")}
              selectedLine={pullRequestSelectedLine}
              onLineSelect={
                pullRequest.state === "open"
                && reviewContextMatches
                && displayedPullRequestDiff?.path
                  ? selectPullRequestLine
                  : undefined
              }
            />
            {pullRequestSelectedLine && (
              <form
                className="git-review__pr-line-editor"
                onSubmit={(event) => {
                  event.preventDefault();
                  savePullRequestLineDraft();
                }}
              >
                <header>
                  <code>
                    {pullRequestSelectedLine.path}:{pullRequestSelectedLine.line}
                    {" · "}
                    {pullRequestSelectedLine.side}
                  </code>
                  <button
                    type="button"
                    onClick={() => {
                      setPullRequestSelectedLine(null);
                      setPullRequestLineComment("");
                    }}
                  >
                    {t("取消", "Cancel")}
                  </button>
                </header>
                <textarea
                  rows={2}
                  value={pullRequestLineComment}
                  aria-label={t(
                    "第 {line} 行评论",
                    "Comment for line {line}",
                    { line: pullRequestSelectedLine.line }
                  )}
                  placeholder={t("添加行评论", "Add an inline comment")}
                  onChange={(event) => setPullRequestLineComment(event.target.value)}
                />
                <button
                  type="submit"
                  disabled={!pullRequestLineComment.trim() || !reviewContextMatches}
                >
                  {pullRequestLineDrafts.some(
                    (draft) => draft.key === lineDraftKey(pullRequestSelectedLine)
                  )
                    ? t("更新草稿", "Update draft")
                    : t("保存草稿", "Save draft")}
                </button>
              </form>
            )}
          </article>
        </div>
        <section
          className="git-review__pr-review-panel"
          aria-label={t("拉取请求审阅", "Pull request review")}
        >
          <div className="git-review__pr-thread-list">
            <header>
              <strong>{t("当前文件讨论", "Current file discussions")}</strong>
              <small>
                {t(
                  "{shown}/{total} 个线程",
                  "{shown}/{total} threads",
                  {
                    shown: selectedFileThreads.length,
                    total: pullRequestThreadsTotalCount
                  }
                )}
              </small>
            </header>
            {pullRequestThreadsState === "loading" && pullRequestThreads.length === 0 && (
              <div className="git-review__pr-review-status" role="status">
                <LoaderCircle className="spin" size={13} />
                {t("正在读取审阅线程", "Loading review threads")}
              </div>
            )}
            {pullRequestThreadsError && (
              <div className="git-review__pr-review-status is-error" role="alert">
                <CircleAlert size={13} />
                {pullRequestThreadsError}
              </div>
            )}
            {pullRequestThreadsState !== "loading"
              && !pullRequestThreadsError
              && selectedFileThreads.length === 0 && (
                <div className="git-review__pr-review-status">
                  {reviewedPath
                    ? t("当前文件没有审阅线程", "No review threads for this file")
                    : t("选择文件以查看审阅线程", "Select a file to view review threads")}
                </div>
              )}
            {selectedFileThreads.map((thread) => {
              const threadLine = thread.line
                ?? thread.originalLine
                ?? thread.startLine
                ?? thread.originalStartLine
                ?? null;
              const replyBody = pullRequestThreadReplies[thread.id] ?? "";
              const replyKey = `pr-thread-reply:${JSON.stringify([
                pullRequestActionScope,
                thread.id,
                replyBody.trim()
              ])}`;
              const resolutionKey = `pr-thread-${thread.isResolved ? "unresolve" : "resolve"}:${JSON.stringify([
                pullRequestActionScope,
                thread.id,
                thread.isResolved
              ])}`;
              const commentsPage = pullRequestThreadCommentsPages[thread.id];
              const commentsNextCursor = commentsPage
                ? commentsPage.nextCursor
                : thread.commentsNextCursor ?? null;
              const remainingComments = Math.max(
                0,
                thread.commentsTotalCount - thread.comments.length
              );
              return (
                <article
                  className={`git-review__pr-thread${thread.isResolved ? " is-resolved" : ""}${thread.isOutdated ? " is-outdated" : ""}`}
                  key={thread.id}
                >
                  <header>
                    <code>
                      {thread.path}
                      {threadLine ? `:${threadLine}` : ""}
                      {" · "}
                      {thread.diffSide}
                    </code>
                    <span>
                      {thread.isOutdated && <small>{t("已过时", "Outdated")}</small>}
                      {thread.isResolved && <small>{t("已解决", "Resolved")}</small>}
                    </span>
                  </header>
                  <div className="git-review__pr-thread-comments">
                    {thread.comments.map((comment) => (
                      <div key={comment.id}>
                        <span>
                          <strong>{comment.author ?? t("未知作者", "Unknown author")}</strong>
                          <small>{dateLabel(comment.updatedAt || comment.createdAt, resolvedLanguage)}</small>
                        </span>
                        <p>{comment.body}</p>
                      </div>
                    ))}
                    {commentsPage?.error && (
                      <small role="alert">{commentsPage.error}</small>
                    )}
                    {commentsNextCursor ? (
                      <button
                        type="button"
                        className="git-review__pr-thread-more"
                        disabled={commentsPage?.loading}
                        onClick={() => void loadMorePullRequestThreadComments(thread)}
                      >
                        {commentsPage?.loading
                          ? <BusyLabel>{t("正在加载回复", "Loading replies")}</BusyLabel>
                          : t(
                              "加载更多回复{count}",
                              "Load more replies{count}",
                              {
                                count: remainingComments > 0
                                  ? ` · ${remainingComments}`
                                  : ""
                              }
                            )}
                      </button>
                    ) : remainingComments > 0 && (
                      <small>
                        {t(
                          "另有 {count} 条回复未在当前页显示",
                          "{count} more replies are not shown on this page",
                          { count: remainingComments }
                        )}
                      </small>
                    )}
                  </div>
                  {(thread.viewerCanReply || thread.viewerCanResolve || thread.viewerCanUnresolve) && (
                    <footer>
                      {thread.viewerCanReply && (
                        <>
                          <textarea
                            rows={1}
                            value={replyBody}
                            aria-label={t(
                              "回复线程 {id}",
                              "Reply to thread {id}",
                              { id: thread.id }
                            )}
                            placeholder={t("回复讨论", "Reply to discussion")}
                            onChange={(event) => {
                              const value = event.target.value;
                              setPullRequestThreadReplies((current) => ({
                                ...current,
                                [thread.id]: value
                              }));
                            }}
                          />
                          <button
                            type="button"
                            disabled={
                              reviewActionsDisabled
                              || !replyBody.trim()
                            }
                            className={pendingGitHubAction === replyKey ? "git-review__confirm--armed" : undefined}
                            onBlur={() => setPendingGitHubAction((current) => current === replyKey ? null : current)}
                            onClick={() => confirmPullRequestReviewAction(
                              replyKey,
                              {
                                ...githubActionIdentity!,
                                type: "reply_review_thread",
                                number: pullRequest.number,
                                expectedHeadOid: pullRequest.headRefOid,
                                expectedState: pullRequest.state,
                                threadId: thread.id,
                                body: replyBody.trim()
                              },
                              () => setPullRequestThreadReplies((current) => ({
                                ...current,
                                [thread.id]: ""
                              }))
                            )}
                          >
                            {pendingGitHubAction === replyKey
                              ? t("确认回复", "Confirm reply")
                              : t("回复", "Reply")}
                          </button>
                        </>
                      )}
                      {(
                        (!thread.isResolved && thread.viewerCanResolve)
                        || (thread.isResolved && thread.viewerCanUnresolve)
                      ) && (
                        <button
                          type="button"
                          disabled={reviewActionsDisabled}
                          className={pendingGitHubAction === resolutionKey ? "git-review__confirm--armed" : undefined}
                          onBlur={() => setPendingGitHubAction((current) => current === resolutionKey ? null : current)}
                          onClick={() => confirmPullRequestReviewAction(
                            resolutionKey,
                            {
                              ...githubActionIdentity!,
                              type: thread.isResolved
                                ? "unresolve_review_thread"
                                : "resolve_review_thread",
                              number: pullRequest.number,
                              expectedHeadOid: pullRequest.headRefOid,
                              expectedState: pullRequest.state,
                              threadId: thread.id
                            }
                          )}
                        >
                          {pendingGitHubAction === resolutionKey
                            ? thread.isResolved
                              ? t("确认重新打开", "Confirm unresolve")
                              : t("确认解决", "Confirm resolve")
                            : thread.isResolved
                              ? t("重新打开", "Unresolve")
                              : t("解决", "Resolve")}
                        </button>
                      )}
                    </footer>
                  )}
                </article>
              );
            })}
            {pullRequestThreadsNextCursor && (
              <button
                type="button"
                className="git-review__pr-thread-more"
                disabled={pullRequestThreadsLoadingMore || Boolean(busyAction)}
                onClick={() => void loadMorePullRequestThreads()}
              >
                {pullRequestThreadsLoadingMore
                  ? <BusyLabel>{t("正在加载", "Loading")}</BusyLabel>
                  : t("加载更多线程", "Load more threads")}
              </button>
            )}
          </div>
          <div className="git-review__pr-drafts">
            <header>
              <strong>{t("行评论草稿", "Inline drafts")}</strong>
              <small>{pullRequestLineDrafts.length}</small>
            </header>
            {pullRequestLineDrafts.map((draft) => (
              <div key={draft.key}>
                <span>
                  <code>{draft.path}:{draft.line} · {draft.side}</code>
                  <small>{draft.body}</small>
                </span>
                <button
                  type="button"
                  aria-label={t(
                    "移除 {path} 第 {line} 行草稿",
                    "Remove draft for {path} line {line}",
                    { path: draft.path, line: draft.line }
                  )}
                  onClick={() => {
                    setPullRequestLineDrafts((current) => (
                      current.filter((item) => item.key !== draft.key)
                    ));
                    if (pullRequestSelectedLine && lineDraftKey(pullRequestSelectedLine) === draft.key) {
                      setPullRequestLineComment("");
                    }
                    setPendingGitHubAction(null);
                  }}
                >
                  <Trash2 size={12} />
                </button>
              </div>
            ))}
            {pullRequestLineDrafts.length === 0 && (
              <p>{t("在差异行号上选择位置以添加评论", "Select a diff line number to add a comment")}</p>
            )}
          </div>
          <form
            className="git-review__pr-submit-review"
            onSubmit={(event) => {
              event.preventDefault();
              if (reviewSubmitDisabled || !githubActionIdentity) return;
              confirmPullRequestReviewAction(
                submitReviewConfirmationKey,
                {
                  ...githubActionIdentity,
                  type: "submit_pull_request_review",
                  number: pullRequest.number,
                  expectedHeadOid: pullRequest.headRefOid,
                  expectedState: pullRequest.state,
                  event: pullRequestReviewEvent,
                  ...(reviewBody ? { body: reviewBody } : {}),
                  comments: reviewComments
                },
                () => {
                  setPullRequestLineDrafts([]);
                  setPullRequestSelectedLine(null);
                  setPullRequestLineComment("");
                  setPullRequestReviewBody("");
                }
              );
            }}
          >
            <textarea
              rows={2}
              value={pullRequestReviewBody}
              aria-label={t("审阅总结", "Review summary")}
              required={reviewNeedsBody}
              placeholder={
                pullRequestReviewEvent === "comment"
                  ? t("填写本次评论的总结（必填）", "Summarize this comment review (required)")
                  : pullRequestReviewEvent === "request_changes"
                    ? t("说明需要修改的内容（必填）", "Explain the requested changes (required)")
                    : t("审阅总结（可选）", "Review summary (optional)")
              }
              onChange={(event) => setPullRequestReviewBody(event.target.value)}
            />
            <div role="group" aria-label={t("审阅结论", "Review event")}>
              {([
                ["comment", t("评论", "Comment")],
                ["approve", t("通过", "Approve")],
                ["request_changes", t("要求修改", "Request changes")]
              ] as Array<[GitHubPullRequestReviewEvent, string]>).map(([event, label]) => (
                <button
                  type="button"
                  key={event}
                  aria-pressed={pullRequestReviewEvent === event}
                  className={pullRequestReviewEvent === event ? "is-selected" : undefined}
                  onClick={() => {
                    setPullRequestReviewEvent(event);
                    setPendingGitHubAction(null);
                  }}
                >
                  {label}
                </button>
              ))}
            </div>
            {reviewNeedsBody && !reviewBody && (
              <p className="git-review__pr-approval-warning" role="status">
                <CircleAlert size={12} />
                {pullRequestReviewEvent === "comment"
                  ? t(
                      "评论审阅必须填写总结，行评论草稿不能替代总结。",
                      "A comment review requires a summary; inline drafts do not replace it."
                    )
                  : t(
                      "要求修改必须填写审阅总结。",
                      "A request-changes review requires a summary."
                    )}
              </p>
            )}
            {approvalBlockReason && (
              <p className="git-review__pr-approval-warning" role="status">
                <CircleAlert size={12} />
                {approvalBlockReason}
              </p>
            )}
            <button
              type="submit"
              disabled={reviewSubmitDisabled}
              className={pendingGitHubAction === submitReviewConfirmationKey
                ? "git-review__confirm--armed"
                : "git-review__primary-action"}
              onBlur={() => setPendingGitHubAction((current) => (
                current === submitReviewConfirmationKey ? null : current
              ))}
            >
              {pendingGitHubAction === submitReviewConfirmationKey
                ? t("确认提交审阅", "Confirm submit review")
                : t("提交审阅", "Submit review")}
            </button>
          </form>
        </section>
      </article>
    );
  };

  const renderPullRequests = () => (
    <section
      id={`${tabsId}-panel-pullRequests`}
      className="git-review__view"
      role="tabpanel"
      aria-labelledby={`${tabsId}-tab-pullRequests`}
    >
      {githubState === "loading" && !githubRepository ? (
        <div className="git-review__empty" role="status"><LoaderCircle className="spin" size={18} />{t("正在连接 GitHub CLI", "Connecting to GitHub CLI")}</div>
      ) : githubError && !githubRepository && !selectedPullRequest ? (
        <div className="git-review__error" role="alert">
          <CircleAlert size={15} />
          <span>{githubError}</span>
          <button type="button" onClick={() => void loadGitHub(true)}>{t("重试", "Retry")}</button>
        </div>
      ) : !githubRepository ? (
        <div className="git-review__empty"><Github size={20} />{t("当前远程仓库未连接 GitHub CLI", "This remote is not connected to GitHub CLI")}</div>
      ) : selectedPullRequest || pullRequestReview ? renderPullRequestDetail() : (
        <>
          <div className="git-review__github-heading">
            <span><Github size={15} /><strong>{githubRepository.nameWithOwner}</strong><small>{githubRepository.viewerLogin ?? ""}</small></span>
            <button
              type="button"
              disabled={githubActionsDisabled}
              onClick={() => setCreatePullRequestOpen((open) => !open)}
            >
              <Plus size={13} />{t("新建拉取请求", "New pull request")}
            </button>
            <IconButton
              label={t("刷新拉取请求", "Refresh pull requests")}
              disabled={githubActionsDisabled}
              onClick={() => void loadGitHub(true)}
            >
              <RefreshCw className={githubState === "loading" ? "spin" : undefined} size={13} />
            </IconButton>
          </div>
          {githubError && (
            <div className="git-review__error" role="alert">
              <CircleAlert size={15} />
              <span>{githubError}</span>
            </div>
          )}
          {createPullRequestOpen && (
            <form
              className="git-review__pr-create"
              onSubmit={(event) => {
                event.preventDefault();
                const title = pullRequestTitle.trim();
                if (
                  !title
                  || !createPullRequestBase
                  || !createPullRequestHead
                  || !currentSnapshot.head
                  || !githubActionIdentity
                ) return;
                confirmGitHubAction(
                  createPullRequestConfirmationKey,
                  {
                    ...githubActionIdentity,
                    type: "create_pull_request",
                    title,
                    body: pullRequestBody.trim(),
                    base: createPullRequestBase,
                    head: createPullRequestHead,
                    draft: pullRequestDraft,
                    expectedLocalHeadOid: currentSnapshot.head,
                    expectedContentRevision: currentSnapshot.contentRevision
                  },
                  () => {
                    setCreatePullRequestOpen(false);
                    setPullRequestTitle("");
                    setPullRequestBody("");
                    void loadGitHub(true);
                  }
                );
              }}
            >
              {githubRepository && (
                <GitHubWriteIdentity
                  repository={githubRepository}
                  headOid={currentSnapshot.head}
                />
              )}
              <input
                value={pullRequestTitle}
                aria-label={t("拉取请求标题", "Pull request title")}
                placeholder={t("拉取请求标题", "Pull request title")}
                onChange={(event) => setPullRequestTitle(event.target.value)}
              />
              <textarea
                rows={3}
                value={pullRequestBody}
                aria-label={t("拉取请求说明", "Pull request description")}
                placeholder={t("说明（可选）", "Description (optional)")}
                onChange={(event) => setPullRequestBody(event.target.value)}
              />
              <label><span>{t("目标分支", "Base branch")}</span><input value={pullRequestBase} onChange={(event) => setPullRequestBase(event.target.value)} /></label>
              <label className="git-review__checkbox"><input type="checkbox" checked={pullRequestDraft} onChange={(event) => setPullRequestDraft(event.target.checked)} />{t("草稿", "Draft")}</label>
              <span className="git-review__pr-create-target">
                <strong>{createPullRequestHead || t("无当前分支", "No current branch")}</strong>
                <span>→</span>
                <strong>{createPullRequestBase || t("无目标分支", "No base branch")}</strong>
              </span>
              <button
                type="submit"
                className={pendingGitHubAction === createPullRequestConfirmationKey ? "git-review__confirm--armed" : undefined}
                disabled={
                  githubActionsDisabled
                  || !pullRequestTitle.trim()
                  || !createPullRequestBase
                  || !createPullRequestHead
                  || !currentSnapshot.head
                }
                onBlur={() => setPendingGitHubAction((current) => (
                  current === createPullRequestConfirmationKey ? null : current
                ))}
              >
                {busyAction === createPullRequestConfirmationKey
                  ? <BusyLabel>{t("创建中", "Creating")}</BusyLabel>
                  : pendingGitHubAction === createPullRequestConfirmationKey
                    ? t("确认创建", "Confirm create")
                    : t("创建", "Create")}
              </button>
            </form>
          )}
          {pullRequests.length ? (
            <div className="git-review__pr-list">
              {pullRequests.map((pullRequest) => (
                <button type="button" key={pullRequest.number} onClick={() => void openPullRequest(pullRequest)}>
                  <span className={`git-review__pr-state git-review__pr-state--${pullRequest.state}`}>{pullRequest.state}</span>
                  <span>
                    <strong>#{pullRequest.number} {pullRequest.title}</strong>
                    <small>{pullRequest.headRefName} → {pullRequest.baseRefName} · {pullRequest.author ?? ""}</small>
                  </span>
                  {pullRequest.draft && <em>{t("草稿", "Draft")}</em>}
                </button>
              ))}
            </div>
          ) : (
            <div className="git-review__empty">{t("没有拉取请求", "No pull requests")}</div>
          )}
          {pullRequestNextPage && (
            <button
              type="button"
              className="git-review__load-more"
              aria-label={t("加载更多拉取请求", "Load more pull requests")}
              disabled={githubActionsDisabled}
              onClick={() => void loadMorePullRequests()}
            >
              {pullRequestsLoadingMore
                ? <BusyLabel>{t("正在加载", "Loading")}</BusyLabel>
                : t("加载更多", "Load more")}
            </button>
          )}
        </>
      )}
    </section>
  );

  const renderedView = view === "changes" ? renderChanges()
    : view === "history" ? renderHistory()
      : view === "branches" ? renderBranches()
        : view === "compare" ? renderCompare()
          : renderPullRequests();

  return (
    <section className="git-review-panel" aria-label={t("Git 审阅", "Git review")}>
      {renderHeader()}
      <div className="git-review__tabs" role="tablist" aria-label={t("Git 审阅页面", "Git review views")}>
        {VIEWS.map((item) => {
          const Icon = item.icon;
          const selected = view === item.id;
          return (
            <button
              type="button"
              role="tab"
              id={`${tabsId}-tab-${item.id}`}
              aria-controls={`${tabsId}-panel-${item.id}`}
              aria-selected={selected}
              aria-label={item.label(t)}
              tabIndex={selected ? 0 : -1}
              className={selected ? "is-active" : undefined}
              key={item.id}
              onClick={() => selectView(item.id)}
              onKeyDown={(event) => handleReviewTabKeyDown(event, item.id)}
            >
              <Icon size={14} aria-hidden="true" />
              <span>{item.label(t)}</span>
            </button>
          );
        })}
      </div>
      {repositoryOperation && (
        <section
          className="git-review__repository-operation"
          aria-label={t("进行中的 Git 操作", "Git operation in progress")}
        >
          <CircleAlert size={15} aria-hidden="true" />
          <span>
            <strong>{t(
              "Git {operation} 正在进行",
              "Git {operation} in progress",
              { operation: repositoryOperationLabel(repositoryOperation, t) }
            )}</strong>
            <small>
              {currentSnapshot.conflicted > 0
                ? t(
                  "仍有 {count} 个冲突；解决并暂存后再继续",
                  "{count} conflicts remain; resolve and stage them before continuing",
                  { count: currentSnapshot.conflicted }
                )
                : repositoryOperation === "bisect"
                  ? currentSnapshot.isClean
                    ? t(
                      "测试当前提交，然后标记旧状态、新状态或跳过；自定义术语由 Git 自动映射",
                      "Test this commit, then mark it old, new, or skip it. Git maps custom terms automatically."
                    )
                    : t(
                      "先提交或储藏当前变更，才能继续二分查找",
                      "Commit or stash the current changes before advancing the bisect."
                    )
                  : t(
                    "没有检测到未合并文件，可以继续当前操作",
                    "No unmerged files were detected; the operation can continue"
                  )}
            </small>
          </span>
          <div>
            {repositoryOperation === "bisect" && (
              <>
                <button
                  type="button"
                  className={pendingGitAction === bisectOldOperationKey ? "git-review__confirm--armed" : undefined}
                  disabled={
                    mutationsDisabled
                    || !currentSnapshot.isClean
                    || !currentSnapshot.head
                    || !currentSnapshot.operationRevision
                  }
                  onBlur={() => setPendingGitAction((current) => current === bisectOldOperationKey ? null : current)}
                  onClick={() => confirmLocalGitAction(bisectOldOperationKey, {
                    type: "bisect_step",
                    outcome: "old",
                    expectedHead: currentSnapshot.head ?? "",
                    expectedOperationRevision: currentSnapshot.operationRevision ?? "",
                    expectedContentRevision: currentSnapshot.contentRevision
                  })}
                >
                  {busyAction === bisectOldOperationKey
                    ? <BusyLabel>{t("标记中", "Marking")}</BusyLabel>
                    : pendingGitAction === bisectOldOperationKey
                      ? t("确认旧状态", "Confirm old")
                      : t("标为旧状态", "Mark old")}
                </button>
                <button
                  type="button"
                  className={pendingGitAction === bisectNewOperationKey ? "git-review__confirm--armed" : undefined}
                  disabled={
                    mutationsDisabled
                    || !currentSnapshot.isClean
                    || !currentSnapshot.head
                    || !currentSnapshot.operationRevision
                  }
                  onBlur={() => setPendingGitAction((current) => current === bisectNewOperationKey ? null : current)}
                  onClick={() => confirmLocalGitAction(bisectNewOperationKey, {
                    type: "bisect_step",
                    outcome: "new",
                    expectedHead: currentSnapshot.head ?? "",
                    expectedOperationRevision: currentSnapshot.operationRevision ?? "",
                    expectedContentRevision: currentSnapshot.contentRevision
                  })}
                >
                  {busyAction === bisectNewOperationKey
                    ? <BusyLabel>{t("标记中", "Marking")}</BusyLabel>
                    : pendingGitAction === bisectNewOperationKey
                      ? t("确认新状态", "Confirm new")
                      : t("标为新状态", "Mark new")}
                </button>
                <button
                  type="button"
                  className={pendingGitAction === bisectSkipOperationKey ? "git-review__confirm--armed" : undefined}
                  disabled={
                    mutationsDisabled
                    || !currentSnapshot.isClean
                    || !currentSnapshot.head
                    || !currentSnapshot.operationRevision
                  }
                  onBlur={() => setPendingGitAction((current) => current === bisectSkipOperationKey ? null : current)}
                  onClick={() => confirmLocalGitAction(bisectSkipOperationKey, {
                    type: "bisect_step",
                    outcome: "skip",
                    expectedHead: currentSnapshot.head ?? "",
                    expectedOperationRevision: currentSnapshot.operationRevision ?? "",
                    expectedContentRevision: currentSnapshot.contentRevision
                  })}
                >
                  {busyAction === bisectSkipOperationKey
                    ? <BusyLabel>{t("跳过中", "Skipping")}</BusyLabel>
                    : pendingGitAction === bisectSkipOperationKey
                      ? t("确认跳过", "Confirm skip")
                      : t("跳过", "Skip")}
                </button>
              </>
            )}
            {operationSupportsContinue(repositoryOperation) && (
              <button
                type="button"
                disabled={
                  mutationsDisabled
                  || currentSnapshot.conflicted > 0
                  || !currentSnapshot.head
                  || !currentSnapshot.operationRevision
                }
                onClick={() => void runGitAction({
                  type: "continue_operation",
                  operation: repositoryOperation,
                  expectedHead: currentSnapshot.head ?? "",
                  expectedOperationRevision: currentSnapshot.operationRevision ?? ""
                }, "continue-operation")}
              >
                {busyAction === "continue-operation"
                  ? <BusyLabel>{t("继续中", "Continuing")}</BusyLabel>
                  : t("继续", "Continue")}
              </button>
            )}
            {operationSupportsSkip(repositoryOperation) && (
              <button
                type="button"
                className={pendingGitAction === skipOperationKey ? "git-review__confirm--armed" : undefined}
                disabled={
                  mutationsDisabled
                  || !currentSnapshot.head
                  || !currentSnapshot.operationRevision
                }
                onBlur={() => setPendingGitAction((current) => current === skipOperationKey ? null : current)}
                onClick={() => confirmLocalGitAction(skipOperationKey, {
                  type: "skip_operation",
                  operation: repositoryOperation,
                  expectedHead: currentSnapshot.head ?? "",
                  expectedOperationRevision: currentSnapshot.operationRevision ?? ""
                })}
              >
                {pendingGitAction === skipOperationKey
                  ? t("确认跳过", "Confirm skip")
                  : t("跳过", "Skip")}
              </button>
            )}
            <button
              type="button"
              className={pendingGitAction === abortOperationKey ? "git-review__confirm--armed" : undefined}
              disabled={
                mutationsDisabled
                || !currentSnapshot.head
                || !currentSnapshot.operationRevision
              }
              onBlur={() => setPendingGitAction((current) => current === abortOperationKey ? null : current)}
              onClick={() => confirmLocalGitAction(abortOperationKey, {
                type: "abort_operation",
                operation: repositoryOperation,
                expectedHead: currentSnapshot.head ?? "",
                expectedOperationRevision: currentSnapshot.operationRevision ?? ""
              })}
            >
              {pendingGitAction === abortOperationKey
                ? repositoryOperation === "bisect"
                  ? t("确认结束", "Confirm stop")
                  : t("确认中止", "Confirm abort")
                : repositoryOperation === "bisect"
                  ? t("结束", "Stop")
                  : t("中止", "Abort")}
            </button>
          </div>
        </section>
      )}
      {mutationDisabledReason && (
        <div className="git-review__operation-notice" role="status">
          <CircleAlert size={14} />
          {mutationDisabledReason}
        </div>
      )}
      {operationError && <div className="git-review__operation-error" role="alert"><CircleAlert size={14} />{operationError}</div>}
      {operationMessage && (
        <div className="git-review__operation-success" role="status">
          <Check size={14} />
          {operationMessage}
        </div>
      )}
      {currentSnapshot.warnings.length > 0 && (
        <div className="git-review__warning" role="status">
          <CircleAlert size={14} />
          <span>{currentSnapshot.warnings.join(" · ")}</span>
        </div>
      )}
      <div className="git-review__content">{renderedView}</div>
    </section>
  );
}
