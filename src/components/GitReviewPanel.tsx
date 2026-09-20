import {
  ArrowRight,
  Check,
  CircleAlert,
  LoaderCircle,
  PanelLeft,
  Plus,
  RotateCcw,
  Trash2
} from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useI18n } from "../i18n";
import {
  executeGitAction,
  getGitChangePage,
  getGitDiff,
  getGitWorkspaceSummary,
  gitFileHasStagedChange,
  gitFileHasUnstagedChange,
  prepareGitDiscard,
  summaryToGitWorkspaceSnapshot,
  type GitAction,
  type GitChangePageResult,
  type GitDiffRequest,
  type GitDiffResult,
  type GitFileChange,
  type GitRemote,
  type GitUpstream,
  type GitRepositoryOperation,
  type GitTarget,
  type GitWorkspaceSnapshot
} from "../lib/git";
import { IconButton } from "./Common";
import { DiffViewer, DIFF_LARGE_LINE_COUNT } from "./DiffViewer";
import type { DiffEntry, DiffEntryStatus } from "./DiffViewer";
import { PopoverMenu } from "./PopoverMenu";
import type { PopoverMenuItem, PopoverMenuSection } from "./PopoverMenu";
import { SidePane } from "./SidePane";
import { parseUnifiedDiff } from "../lib/unifiedDiff";
import type { DiffFile } from "../lib/unifiedDiff";
import {
  readStoredChoice,
  readStoredFlag,
  writeStoredChoice,
  writeStoredFlag
} from "../lib/paneSettings";
import type { SidePaneId } from "../lib/sidePanes";
import "./GitReviewPanel.css";

export interface GitReviewPanelProps {
  paneId: SidePaneId;
  target: GitTarget;
  snapshot: GitWorkspaceSnapshot;
  active: boolean;
  mutationDisabledReason?: string | null;
  onSnapshotChange?: (snapshot: GitWorkspaceSnapshot | null) => void;
  onMutationStart?: () => boolean;
  onMutationEnd?: () => void;
  paneExpanded: boolean;
  onPaneToggleExpand: () => void;
  onPaneFocus: () => void;
  onPaneClose: () => void;
}

type Translate = ReturnType<typeof useI18n>["t"];
type RequestState = "idle" | "loading" | "ready" | "error";
type DiffScope = "working" | "staged" | "unstaged";
const CHANGE_FILE_BATCH_SIZE = 200;
/** Display settings the pane's `⋮` menu owns; they outlive the conversation. */
const DIFF_TREE_STORAGE_KEY = "mework.review.showFiles";
const DIFF_STYLE_STORAGE_KEY = "mework.review.diffStyle";
const DIFF_WORD_WRAP_STORAGE_KEY = "mework.review.wordWrap";
const DIFF_WORD_DIFF_STORAGE_KEY = "mework.review.wordDiff";
const DIFF_HIDE_WHITESPACE_STORAGE_KEY = "mework.review.hideWhitespace";
const DIFF_STYLES = ["unified", "split"] as const;
/**
 * Changed files past which the scope is read one file at a time.
 *
 * A scope-wide `git diff` is one command holding the repository lock for as long
 * as it takes to write every hunk of every file; past this many files the wait
 * is long enough to stall the change list, and every file opens folded anyway.
 */
const DIFF_LAZY_FILE_COUNT = 60;
/**
 * Context lines asked for when a hunk boundary is expanded.
 *
 * Each step re-reads that one file at a wider `-U`; the last is large enough to
 * be the whole of any file a reviewer will read in a pane.
 */
const DIFF_CONTEXT_STEPS = [25, 100, 100000] as const;

interface PendingDiscard {
  key: string;
  scopeKey: string;
  snapshotKey: string;
  path: string;
  includeUntracked: boolean;
  contentRevision: string;
  targetRevision: string;
}

/**
 * The review panel lists tracked changes only.
 *
 * `git status` enumerates untracked files and the workspace snapshot carries
 * them for the `git_status` tool, discard and stash; the host filters them out
 * of `get_git_change_page`, and this is the same rule for the inline path that
 * reads `snapshot.files` without asking the host for a page.
 */
function isReviewableChange(file: GitFileChange): boolean {
  return !file.untracked && file.status !== "untracked";
}

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

/**
 * Files this panel will list.
 *
 * A snapshot that carries no `files` still knows how many paths changed and how
 * many of those are untracked, so the tracked count is exact either way.
 */
function reviewableFileCount(snapshot: GitWorkspaceSnapshot): number {
  if (snapshotHasInlineChanges(snapshot)) {
    return snapshot.files.filter(isReviewableChange).length;
  }
  const changed = snapshot.changedFiles ?? snapshot.files.length;
  return Math.max(0, changed - snapshot.untracked);
}

function BusyLabel({ children }: { children: string }) {
  return <><LoaderCircle className="spin" size={12} />{children}</>;
}

export function GitReviewPanel({
  paneId,
  target,
  snapshot,
  active,
  mutationDisabledReason = null,
  onSnapshotChange = () => undefined,
  onMutationStart = () => true,
  onMutationEnd = () => undefined,
  paneExpanded,
  onPaneToggleExpand,
  onPaneFocus,
  onPaneClose
}: GitReviewPanelProps) {
  const { t } = useI18n();
  const [currentSnapshot, setCurrentSnapshot] = useState(snapshot);
  const [busyAction, setBusyAction] = useState<string | null>(null);
  const [operationError, setOperationError] = useState<string | null>(null);
  const [operationMessage, setOperationMessage] = useState<string | null>(null);
  const [selectedPath, setSelectedPath] = useState<string | null>(
    snapshot.files.find(isReviewableChange)?.path ?? null
  );
  const [changeFilter, setChangeFilter] = useState("");
  const [changeFiles, setChangeFiles] = useState<GitFileChange[]>(
    snapshotHasInlineChanges(snapshot) ? snapshot.files.filter(isReviewableChange) : []
  );
  const [changeMatchedCount, setChangeMatchedCount] = useState(
    reviewableFileCount(snapshot)
  );
  const [changeNextCursor, setChangeNextCursor] = useState<string | null>(null);
  const [changePageState, setChangePageState] = useState<RequestState>(
    snapshotHasInlineChanges(snapshot) ? "ready" : "idle"
  );
  const [changePageError, setChangePageError] = useState<string | null>(null);
  const [changeLoadingMore, setChangeLoadingMore] = useState(false);
  const [diffScope, setDiffScope] = useState<DiffScope>("working");
  const [diff, setDiff] = useState<GitDiffResult | null>(null);
  const [diffState, setDiffState] = useState<RequestState>("idle");
  const [diffError, setDiffError] = useState<string | null>(null);
  const [diffRevision, setDiffRevision] = useState(0);
  // The reference shell keeps these four in the pane's own `⋮` menu and remembers
  // them across sessions; they describe how a diff is read, not which one.
  const [showDiffFiles, setShowDiffFiles] = useState(
    () => readStoredFlag(DIFF_TREE_STORAGE_KEY, true)
  );
  const [diffStyle, setDiffStyle] = useState<"unified" | "split">(
    () => readStoredChoice(DIFF_STYLE_STORAGE_KEY, DIFF_STYLES, "unified")
  );
  const [diffWordWrap, setDiffWordWrap] = useState(
    () => readStoredFlag(DIFF_WORD_WRAP_STORAGE_KEY, true)
  );
  const [diffWordDiff, setDiffWordDiff] = useState(
    () => readStoredFlag(DIFF_WORD_DIFF_STORAGE_KEY, true)
  );
  const [diffHideWhitespace, setDiffHideWhitespace] = useState(
    () => readStoredFlag(DIFF_HIDE_WHITESPACE_STORAGE_KEY, false)
  );
  const [diffFoldAll, setDiffFoldAll] = useState<{ collapsed: boolean; seq: number } | undefined>(undefined);
  const [diffCanFitFiles, setDiffCanFitFiles] = useState(true);
  const [diffCanFitSplit, setDiffCanFitSplit] = useState(true);
  const [diffIsLarge, setDiffIsLarge] = useState(false);
  /** Patches parsed out of the scope-wide diff, keyed by repository-relative path. */
  const [scopePatches, setScopePatches] = useState<ReadonlyMap<string, DiffFile>>(() => new Map());
  /** Paths whose patch had to be fetched on its own, and how that went. */
  const [filePatches, setFilePatches] = useState<ReadonlyMap<string, DiffFile>>(() => new Map());
  const [filePatchStates, setFilePatchStates] = useState<ReadonlyMap<string, "loading" | string>>(
    () => new Map()
  );
  /** How much context each file has been expanded to, as an index into the steps. */
  const [contextSteps, setContextSteps] = useState<ReadonlyMap<string, number>>(() => new Map());
  const totalChangedFiles = reviewableFileCount(currentSnapshot);
  /**
   * Whether the scope is past the size where one patch is worth asking for.
   *
   * Read from the live snapshot, not the mounted prop: the prop is whatever the
   * workspace looked like when the panel was keyed, and its counts start at zero,
   * so gating on it would let exactly the request this guards against through.
   *
   * The file count is a second gate because line counts alone understate the cost
   * — three hundred small files is still one `git diff` holding the repository
   * lock while every other Git read in the pane queues behind it.
   */
  const scopeIsTooLargeToReadAtOnce =
    currentSnapshot.additions + currentSnapshot.deletions > DIFF_LARGE_LINE_COUNT
    || totalChangedFiles > DIFF_LAZY_FILE_COUNT;
  const [pendingDiscard, setPendingDiscard] = useState<PendingDiscard | null>(null);
  const [pendingGitAction, setPendingGitAction] = useState<string | null>(null);
  const changeDiffRequestRef = useRef(0);
  /** Per-path request ids: several files can be in flight and none fences the others. */
  const changeFilePatchRequestRef = useRef(new Map<string, number>());
  const filePatchesRef = useRef<ReadonlyMap<string, DiffFile>>(new Map());
  const filePatchStatesRef = useRef<ReadonlyMap<string, "loading" | string>>(new Map());
  // Written during render rather than in an effect: the viewer asks for a missing
  // patch from its own effect, and a child's effects run before its parent's, so a
  // ref synced in an effect would still be one render behind when the ask arrives.
  filePatchesRef.current = filePatches;
  filePatchStatesRef.current = filePatchStates;
  const changePageRequestRef = useRef(0);
  const selectedPathRef = useRef(selectedPath);
  selectedPathRef.current = selectedPath;
  const discardPreparationRequestRef = useRef(0);
  const gitActionRequestRef = useRef(0);
  const workspaceIdentityKey = JSON.stringify([snapshot.repositoryId, snapshot.worktreeId]);
  const workspaceIdentityKeyRef = useRef(workspaceIdentityKey);
  workspaceIdentityKeyRef.current = workspaceIdentityKey;
  const diffSnapshotFingerprintRef = useRef(diffSnapshotFingerprint(snapshot));
  const discardSnapshotKey = discardPreparationSnapshotKey(snapshot);
  const acceptedDiscardPreparationSnapshotKeyRef = useRef<string | null>(null);
  const discardPreparationSnapshotKeyRef = useRef(discardSnapshotKey);
  const committedDiscardSnapshotKeyRef = useRef(discardSnapshotKey);
  const observedDiscardSnapshotRef = useRef(snapshot);
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
  const normalizedChangeFilter = changeFilter.trim().toLowerCase();
  const changesAreInline = snapshotHasInlineChanges(currentSnapshot);
  const changePageRevision = currentSnapshot.summaryRevision ?? currentSnapshot.contentRevision;
  const changePageScopeKey = JSON.stringify([
    target,
    currentSnapshot.repositoryId,
    currentSnapshot.worktreeId,
    currentSnapshot.repositoryRoot ?? null,
    currentSnapshot.worktreeRoot ?? null,
    changePageRevision,
    normalizedChangeFilter,
    changesAreInline
  ]);
  const changePageScopeRef = useRef(changePageScopeKey);
  changePageScopeRef.current = changePageScopeKey;

  /** Drops the cached scope diff, so the next read goes back to the repository. */
  const invalidateRepositoryReads = useCallback(() => {
    changeDiffRequestRef.current += 1;
    setDiff(null);
    setDiffState("idle");
    setDiffError(null);
  }, []);

  useEffect(() => () => {
    gitActionRequestRef.current += 1;
  }, []);

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
    const nextDiffFingerprint = diffSnapshotFingerprint(snapshot);
    const diffChanged = diffSnapshotFingerprintRef.current !== nextDiffFingerprint;
    diffSnapshotFingerprintRef.current = nextDiffFingerprint;
    setCurrentSnapshot(snapshot);
    if (diffChanged) setDiffRevision((revision) => revision + 1);
  }, [snapshot]);

  useEffect(() => {
    // The same filesystem path can be replaced with another repository, and a
    // linked worktree shares repository metadata while having different HEAD
    // and index state. Never let either reuse an old page, patch, or armed
    // write confirmation.
    changeDiffRequestRef.current += 1;
    changePageRequestRef.current += 1;
    setChangeFilter("");
    setChangeFiles([]);
    setChangeNextCursor(null);
    setChangeLoadingMore(false);
    setChangePageState("idle");
    setChangePageError(null);
    setSelectedPath(null);
    setDiff(null);
    setDiffState("idle");
    setDiffError(null);
    setPendingGitAction(null);
  }, [workspaceIdentityKey]);

  useEffect(() => {
    if (active) return;
    gitActionRequestRef.current += 1;
    changeDiffRequestRef.current += 1;
    changePageRequestRef.current += 1;
    setDiffState((current) => current === "loading" ? "idle" : current);
    setChangePageState((current) => current === "loading" ? "idle" : current);
    setChangeLoadingMore(false);
    setPendingDiscard(null);
    setPendingGitAction(null);
  }, [active]);

  useEffect(() => {
    if (!mutationDisabledReason) return;
    setPendingDiscard(null);
    setPendingGitAction(null);
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

  const reconcileWorkspaceSummary = useCallback(async (knownRevision?: string) => {
    const requestedWorkspaceIdentity = workspaceIdentityKeyRef.current;
    const result = await getGitWorkspaceSummary(target, knownRevision);
    if (requestedWorkspaceIdentity !== workspaceIdentityKeyRef.current) {
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

  const loadChangePage = useCallback(async (cursor?: string) => {
    if (
      !active
      || busyAction
      || changesAreInline
      || !currentSnapshot.summaryRevision
    ) return;
    const requestId = ++changePageRequestRef.current;
    const requestScopeKey = changePageScopeKey;
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
        ...(selectedPath ? { selectedPath } : {})
      });
      if (
        requestId !== changePageRequestRef.current
        || requestScopeKey !== changePageScopeRef.current
      ) return;
      if (result.kind === "stale") {
        setChangeFiles([]);
        setChangeMatchedCount(0);
        setChangeNextCursor(null);
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
      const nextFiles = cursor ? [...changeFiles, ...result.files] : result.files;
      setChangeFiles(nextFiles);
      setChangeMatchedCount(result.matchedCount ?? nextFiles.length);
      setChangeNextCursor(result.nextCursor);
      if (result.selection?.state === "present") {
        setSelectedPath(result.selection.file.path);
      } else if (result.selection?.state === "filteredOut") {
        // Deliberately nothing: the latent path is kept so clearing the query can
        // restore the user's selection, and a file the filter is hiding must not
        // become the selected one in the meantime.
      } else if (result.selection?.state === "missing") {
        const fallback = nextFiles[0] ?? null;
        setSelectedPath(fallback?.path ?? null);
      } else if (!selectedPath) {
        const fallback = nextFiles[0] ?? null;
        setSelectedPath(fallback?.path ?? null);
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
    publishSnapshot,
    selectedPath,
    t
  ]);

  useEffect(() => {
    changePageRequestRef.current += 1;
    setChangeNextCursor(null);
    setChangeLoadingMore(false);
    setChangePageError(null);
    if (changesAreInline) {
      const reviewable = currentSnapshot.files.filter(isReviewableChange);
      const nextFiles = normalizedChangeFilter
        ? reviewable.filter((file) => (
          file.path.toLowerCase().includes(normalizedChangeFilter)
          || file.originalPath?.toLowerCase().includes(normalizedChangeFilter)
        ))
        : reviewable;
      setChangeFiles(nextFiles);
      setChangeMatchedCount(nextFiles.length);
      const selected = selectedPathRef.current
        ? nextFiles.find((file) => file.path === selectedPathRef.current) ?? null
        : null;
      const fallback = selected ?? nextFiles[0] ?? null;
      setSelectedPath(fallback?.path ?? null);
      setChangePageState("ready");
      return;
    }
    setChangeFiles([]);
    setChangeMatchedCount(normalizedChangeFilter ? 0 : totalChangedFiles);
    setChangePageState("idle");
  }, [
    changePageScopeKey,
    changesAreInline,
    currentSnapshot.files,
    normalizedChangeFilter,
    totalChangedFiles
  ]);

  useEffect(() => {
    if (
      !active
      || busyAction
      || changesAreInline
      || changePageState !== "idle"
      || totalChangedFiles === 0
    ) return;
    void loadChangePage();
  }, [
    active,
    busyAction,
    changePageScopeKey,
    changePageState,
    changesAreInline,
    loadChangePage,
    totalChangedFiles
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
    const requestIsCurrent = () => (
      requestId === gitActionRequestRef.current
      && actionWorkspaceIdentity === workspaceIdentityKeyRef.current
    );
    invalidateRepositoryReads();
    setBusyAction(key);
    setOperationError(null);
    setOperationMessage(null);
    try {
      const result = await executeGitAction(target, action);
      if (!requestIsCurrent()) return false;
      publishSnapshot(result.snapshot);
      setDiffRevision((revision) => revision + 1);
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
        const reconciliation = await reconcileWorkspaceSummary();
        if (!reconciliation.accepted) return false;
        setDiffRevision((revision) => revision + 1);
      } catch {
        // Preserve the action failure: it is the primary error, while the regular
        // polling path can retry a snapshot that temporarily could not be read.
      }
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

  /**
   * Reads the whole scope as one patch.
   *
   * The reference shell shows every changed file in one scroller, so the request
   * is scope-wide rather than per file. It is skipped once the change set passes
   * the size at which every file opens folded anyway: one `git diff` over a
   * hundred thousand changed lines holds the repository lock long enough to
   * starve the change list beside it, and nothing it returns would be drawn.
   * Those files arrive one at a time instead, as they are opened. It is skipped
   * outright when nothing in the scope is reviewable — an untracked-only working
   * tree has no tracked change for `git diff` to report.
   */
  useEffect(() => {
    if (!active || busyAction) return;
    if (scopeIsTooLargeToReadAtOnce || totalChangedFiles === 0) {
      changeDiffRequestRef.current += 1;
      // Idempotent on purpose: this branch runs on every render the effect is
      // re-created for, and handing React a fresh Map each time would make the
      // state change that re-runs it.
      setScopePatches((current) => (current.size === 0 ? current : new Map()));
      setDiffState("ready");
      setDiffError(null);
      return;
    }
    const requestId = ++changeDiffRequestRef.current;
    setDiffState("loading");
    setDiffError(null);
    const request: GitDiffRequest = { type: diffScope };
    void getGitDiff(target, request)
      .then((result) => {
        if (requestId !== changeDiffRequestRef.current) return;
        setDiff(result);
        setScopePatches(new Map(parseUnifiedDiff(result.patch).map((file) => [file.path, file])));
        // Anything fetched on its own belonged to the previous revision of this
        // scope; keeping it would show one file from before the change beside the
        // rest from after it.
        setFilePatches(new Map());
        setFilePatchStates(new Map());
        setContextSteps(new Map());
        setDiffState("ready");
      })
      .catch((reason) => {
        if (requestId !== changeDiffRequestRef.current) return;
        const message = failureMessage(reason, t("无法读取文件差异", "Unable to load file diff"));
        setScopePatches(new Map());
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
    scopeIsTooLargeToReadAtOnce,
    totalChangedFiles,
    t
  ]);

  /**
   * Fetches one file's patch on its own.
   *
   * Two reasons a file is not in the scope-wide patch: an untracked file has to be
   * diffed against nothing, one path at a time, and a file whose context has been
   * expanded needs re-reading at a wider `-U`. Both land here. The call is
   * idempotent for the plain case — the viewer asks again on every render until an
   * answer arrives — and the fence is per path, because several files can be in
   * flight at once and the slow one's answer is not stale just for being late.
   */
  const requestFilePatch = useCallback((path: string, step?: number) => {
    // An inactive panel is still mounted and still renders its viewer; reading a
    // patch for a pane nobody is looking at is exactly the work `active` exists
    // to stop. Waiting for the scope-wide read to settle matters for the same
    // reason: until it answers, every file looks missing.
    if (!active || diffState !== "ready") return;
    if (step === undefined && filePatchStatesRef.current.get(path) !== undefined) return;
    if (step === undefined && filePatchesRef.current.has(path)) return;
    const requestId = (changeFilePatchRequestRef.current.get(path) ?? 0) + 1;
    changeFilePatchRequestRef.current.set(path, requestId);
    const context = step === undefined ? undefined : DIFF_CONTEXT_STEPS[step];
    setFilePatchStates((current) => new Map(current).set(path, "loading"));
    const request: GitDiffRequest = diffScope === "staged"
      ? { type: "staged", path, ...(context !== undefined ? { context } : {}) }
      : { type: diffScope, path, ...(context !== undefined ? { context } : {}) };
    void getGitDiff(target, request)
      .then((result) => {
        if (changeFilePatchRequestRef.current.get(path) !== requestId) return;
        const parsed = parseUnifiedDiff(result.patch, { fallbackPath: path })[0];
        if (!parsed) {
          // An answer carrying no patch is still an answer. Recording it is what
          // stops the viewer asking again on the next render, and every render
          // after that, for a file that has no line-level diff to give.
          setFilePatchStates((current) => new Map(current).set(
            path,
            t("这个范围没有行级差异", "No line-level diff in this scope")
          ));
          return;
        }
        setFilePatchStates((current) => {
          const next = new Map(current);
          next.delete(path);
          return next;
        });
        setFilePatches((current) => new Map(current).set(path, parsed));
        if (step !== undefined) setContextSteps((current) => new Map(current).set(path, step));
      })
      .catch((reason) => {
        if (changeFilePatchRequestRef.current.get(path) !== requestId) return;
        setFilePatchStates((current) => new Map(current).set(
          path,
          failureMessage(reason, t("无法读取文件差异", "Unable to load file diff"))
        ));
      });
  }, [active, diffScope, diffState, t, target]);

  /** Asks for the next wider `-U` for one file, if there is one left. */
  const expandFileContext = useCallback((path: string) => {
    const current = contextSteps.get(path);
    const next = current === undefined ? 0 : current + 1;
    if (next >= DIFF_CONTEXT_STEPS.length) return;
    requestFilePatch(path, next);
  }, [contextSteps, requestFilePatch]);

  const remainingChanges = Math.max(0, changeMatchedCount - changeFiles.length);
  const nextChangePageCount = Math.min(CHANGE_FILE_BATCH_SIZE, remainingChanges);

  const currentDiscardSnapshotKey = discardPreparationSnapshotKey(currentSnapshot);
  const operationBusy = busyAction !== null;
  const mutationsDisabled = operationBusy || Boolean(mutationDisabledReason);
  const repositoryOperation = currentSnapshot.operation;
  const repositoryOperationScope = repositoryOperation
    ? `${workspaceIdentityKey}:${repositoryOperation}:${currentSnapshot.head ?? "unborn"}:${currentSnapshot.operationRevision ?? "unavailable"}`
    : null;
  const skipOperationKey = repositoryOperationScope ? `skip-operation:${repositoryOperationScope}` : "";
  const abortOperationKey = repositoryOperationScope ? `abort-operation:${repositoryOperationScope}` : "";
  const bisectOldOperationKey = repositoryOperationScope ? `bisect-old:${repositoryOperationScope}` : "";
  const bisectNewOperationKey = repositoryOperationScope ? `bisect-new:${repositoryOperationScope}` : "";
  const bisectSkipOperationKey = repositoryOperationScope ? `bisect-skip:${repositoryOperationScope}` : "";

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

  /**
   * The change list as rows of the diff viewer's file column.
   *
   * The reference shell's column is a directory tree of everything in the scope,
   * so staging composition rides along as a tone and a note rather than splitting
   * the list into groups.
   */
  const diffEntries = useMemo<DiffEntry[]>(() => changeFiles.map((file) => {
    const staged = gitFileHasStagedChange(file);
    const unstaged = gitFileHasUnstagedChange(file);
    const untracked = Boolean(file.untracked || file.status === "untracked");
    const status: DiffEntryStatus = file.conflicted || file.status === "unmerged"
      ? "conflicted"
      : untracked
        ? "untracked"
        : file.status === "added"
          ? "added"
          : file.status === "deleted"
            ? "deleted"
            : file.status === "renamed"
              ? "renamed"
              : file.status === "copied"
                ? "copied"
                : file.status === "typeChanged"
                  ? "typeChanged"
                  : "modified";
    const patchState = filePatchStates.get(file.path);
    const failure = typeof patchState === "string" && patchState !== "loading" ? patchState : undefined;
    return {
      path: file.path,
      oldPath: file.originalPath ?? null,
      status,
      additions: file.additions ?? 0,
      deletions: file.deletions ?? 0,
      binary: Boolean(file.binary),
      note: staged && unstaged ? t("已暂存 + 未暂存", "Staged + unstaged") : undefined,
      loading: patchState === "loading",
      unavailable: failure
        ?? (file.submodule
          ? t("子模块变更没有行级差异", "A submodule change has no line-level diff")
          : undefined)
    } satisfies DiffEntry;
  }), [changeFiles, filePatchStates, t]);

  // A file fetched on its own wins: it is either the only place its patch exists
  // (untracked) or a wider read of the same file (expanded context).
  const diffPatches = useMemo(() => {
    const merged = new Map(scopePatches);
    for (const [path, file] of filePatches) merged.set(path, file);
    return merged;
  }, [filePatches, scopePatches]);

  const expandingPaths = useMemo(
    () => new Set([...filePatchStates].filter(([, state]) => state === "loading").map(([path]) => path)),
    [filePatchStates]
  );
  const fullyExpandedPaths = useMemo(
    () => new Set(
      [...contextSteps]
        .filter(([, step]) => step >= DIFF_CONTEXT_STEPS.length - 1)
        .map(([path]) => path)
    ),
    [contextSteps]
  );


  /**
   * The `⋮` on a file row and on its sticky header.
   *
   * Discard keeps its two-click proof here: the first selection arms it and the
   * row's label becomes the confirmation, so the menu is a different affordance
   * for the same guarded action rather than a way around it.
   */
  const changeFileMenu = (path: string): PopoverMenuSection[] => {
    const file = changeFiles.find((candidate) => candidate.path === path);
    if (!file) return [];
    const staged = gitFileHasStagedChange(file);
    const unstaged = gitFileHasUnstagedChange(file);
    const untracked = Boolean(file.untracked || file.status === "untracked");
    const stageable = unstaged && (!file.submodule || Boolean(file.submoduleCommitChanged));
    const discardable = unstaged && !file.submodule;
    const discardArmed = Boolean(
      pendingDiscard
      && pendingDiscard.scopeKey === discardScopeKey
      && pendingDiscard.snapshotKey === currentDiscardSnapshotKey
      && pendingDiscard.path === path
      && pendingDiscard.includeUntracked === untracked
      && pendingDiscard.contentRevision === currentSnapshot.contentRevision
    );
    const items: PopoverMenuItem[] = [];
    if (stageable) {
      items.push({
        id: "stage",
        label: t("暂存", "Stage"),
        icon: <Plus size={13} aria-hidden="true" />,
        disabled: mutationsDisabled,
        onSelect: () => void runGitAction({ type: "stage", paths: [path] }, `stage:${path}`)
      });
    }
    if (staged) {
      items.push({
        id: "unstage",
        label: t("取消暂存", "Unstage"),
        icon: <RotateCcw size={12} aria-hidden="true" />,
        disabled: mutationsDisabled,
        onSelect: () => void runGitAction({ type: "unstage", paths: [path] }, `unstage:${path}`)
      });
    }
    if (discardable) {
      items.push({
        id: "discard",
        label: discardArmed
          ? untracked
            ? t("确认永久删除", "Confirm permanent deletion")
            : t("确认丢弃未暂存更改", "Confirm discard of unstaged changes")
          : untracked
            ? t("永久删除未跟踪文件", "Permanently delete untracked file")
            : t("丢弃未暂存更改", "Discard unstaged changes"),
        icon: discardArmed
          ? <Check size={12} aria-hidden="true" />
          : <Trash2 size={12} aria-hidden="true" />,
        disabled: mutationsDisabled,
        description: discardArmed
          ? undefined
          : untracked
            ? t("这个文件不在 Git 里，删除无法撤销。", "This file is not in Git; deleting it cannot be undone.")
            : undefined,
        onSelect: () => requestDiscard(path, untracked, discardArmed ? pendingDiscard : null)
      });
    }
    const sections: PopoverMenuSection[] = [];
    if (items.length) sections.push({ id: "file-actions", items });
    sections.push({
      id: "file-path",
      items: [{
        id: "copy-path",
        label: t("复制路径", "Copy path"),
        onSelect: () => void navigator.clipboard?.writeText(path)
      }]
    });
    return sections;
  };

  const setShowDiffFilesPreference = (next: boolean) => {
    setShowDiffFiles(next);
    writeStoredFlag(DIFF_TREE_STORAGE_KEY, next);
  };

  const diffFilesVisible = showDiffFiles && diffCanFitFiles;
  const scopeHeadLabel = diffScope === "staged"
    ? t("已暂存", "Staged")
    : diffScope === "unstaged"
      ? t("未暂存", "Unstaged")
      : t("工作树", "working tree");

  /**
   * The pane's title bar.
   *
   * Left to right: the file-column toggle, then one dropdown that answers "what am
   * I looking at" — the reference shell's shape, where the refs being compared are
   * themselves the control that changes them.
   */
  const paneHeader = (
    <div className="git-review__pane-header">
      <IconButton
        className="git-review__files-toggle"
        label={diffFilesVisible ? t("隐藏文件", "Hide files") : t("显示文件", "Show files")}
        aria-pressed={diffFilesVisible}
        disabled={!diffCanFitFiles}
        onClick={() => setShowDiffFilesPreference(!showDiffFiles)}
      >
        <PanelLeft size={14} aria-hidden="true" />
      </IconButton>
      <PopoverMenu
        rootClassName="git-review__scope"
        triggerClassName="git-review__scope-trigger"
        trigger={(
          <>
            <span className="git-review__scope-ref">
              {currentSnapshot.branch ?? currentSnapshot.head?.slice(0, 8) ?? t("尚无提交", "No commits yet")}
            </span>
            <ArrowRight size={11} aria-hidden="true" className="git-review__scope-arrow" />
            <span className="git-review__scope-ref">{scopeHeadLabel}</span>
          </>
        )}
        triggerLabel={t("审阅范围", "Review scope")}
        menuLabel={t("审阅范围", "Review scope")}
        sections={[
          {
            id: "diff-scope",
            label: t("差异范围", "Diff scope"),
            items: ([
              ["working", t("全部变更", "All changes")],
              ["staged", t("已暂存的变更", "Staged changes")],
              ["unstaged", t("未暂存的变更", "Unstaged changes")]
            ] as const).map(([scope, label]) => ({
              id: `scope-${scope}`,
              label,
              checked: diffScope === scope,
              onSelect: () => setDiffScope(scope)
            }))
          }
        ]}
        align="start"
        dense
      />
    </div>
  );

  const paneMenuSections = useMemo<PopoverMenuSection[]>(() => {
    const sections: PopoverMenuSection[] = [];
    sections.push({
      id: "diff-file-list",
      items: [{
        id: "show-files",
        label: t("显示文件", "Show files"),
        checked: diffFilesVisible,
        checkedRole: "checkbox",
        disabled: !diffCanFitFiles,
        description: diffCanFitFiles
          ? undefined
          : t("面板太窄，放不下文件列表", "The pane is too narrow for the file list"),
        onSelect: () => setShowDiffFilesPreference(!showDiffFiles)
      }]
    });
    sections.push({
      id: "diff-fold",
      items: [
        {
          id: "collapse-all",
          label: t("折叠所有文件", "Collapse all files"),
          onSelect: () => setDiffFoldAll((current) => ({ collapsed: true, seq: (current?.seq ?? 0) + 1 }))
        },
        {
          id: "expand-all",
          label: t("展开所有文件", "Expand all files"),
          disabled: diffIsLarge,
          description: diffIsLarge
            ? t("这次变更太大，无法一次展开。选择一个文件来展开它。", "This diff is too large to expand at once. Select a file to expand it.")
            : undefined,
          onSelect: () => setDiffFoldAll((current) => ({ collapsed: false, seq: (current?.seq ?? 0) + 1 }))
        }
      ]
    });
    sections.push({
      id: "diff-display",
      items: [
        {
          id: "side-by-side",
          label: t("并排显示", "Side by side"),
          checked: diffStyle === "split",
          checkedRole: "checkbox",
          hint: diffStyle === "split" && !diffCanFitSplit
            ? t("面板需要更宽", "Needs a wider pane")
            : undefined,
          onSelect: () => {
            const next = diffStyle === "split" ? "unified" : "split";
            setDiffStyle(next);
            writeStoredChoice(DIFF_STYLE_STORAGE_KEY, next);
          }
        },
        {
          id: "word-wrap",
          label: t("自动换行", "Word wrap"),
          checked: diffWordWrap,
          checkedRole: "checkbox",
          onSelect: () => {
            setDiffWordWrap((current) => {
              writeStoredFlag(DIFF_WORD_WRAP_STORAGE_KEY, !current);
              return !current;
            });
          }
        },
        {
          id: "word-diff",
          label: t("高亮改动的词", "Highlight changed words"),
          checked: diffWordDiff,
          checkedRole: "checkbox",
          onSelect: () => {
            setDiffWordDiff((current) => {
              writeStoredFlag(DIFF_WORD_DIFF_STORAGE_KEY, !current);
              return !current;
            });
          }
        },
        {
          id: "hide-whitespace",
          label: t("隐藏空白改动", "Hide whitespace changes"),
          checked: diffHideWhitespace,
          checkedRole: "checkbox",
          onSelect: () => {
            setDiffHideWhitespace((current) => {
              writeStoredFlag(DIFF_HIDE_WHITESPACE_STORAGE_KEY, !current);
              return !current;
            });
          }
        }
      ]
    });
    sections.push({
      id: "refresh",
      items: [{
        id: "refresh",
        label: t("刷新", "Refresh"),
        disabled: operationBusy,
        onSelect: () => void refreshSnapshot()
      }]
    });
    return sections;
    // `setShowDiffFilesPreference` and `refreshSnapshot` are stable enough for this
    // menu; it is rebuilt whenever anything it displays changes.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [
    diffCanFitFiles,
    diffCanFitSplit,
    diffFilesVisible,
    diffHideWhitespace,
    diffIsLarge,
    diffStyle,
    diffWordDiff,
    diffWordWrap,
    operationBusy,
    showDiffFiles,
    t
  ]);

  /**
   * The changes view.
   *
   * One scroller holding every changed file behind a sticky header, with the file
   * column beside it — the reference shell's shape. What used to be a per-file
   * request is now one scope-wide patch; the per-file requests that remain are for
   * what a scope-wide `git diff` cannot carry.
   */
  const renderChanges = () => (
    <section
      className="git-review__view git-review__changes"
      aria-label={t("变更", "Changes")}
    >
      {diffError && (
        <div className="git-review__operation-error" role="alert">
          <CircleAlert size={14} />
          {diffError}
        </div>
      )}
      {diff?.truncated && (
        <div className="git-review__warning" role="alert">
          <CircleAlert size={15} />
          {t(
            "差异内容超过安全读取上限，当前审阅不完整；刷新或缩小范围后再查看。",
            "The diff exceeded the safe read limit, so this review is incomplete. Refresh or narrow the scope."
          )}
        </div>
      )}
      {totalChangedFiles ? (
        <DiffViewer
          entries={diffEntries}
          patches={diffPatches}
          showTree={showDiffFiles}
          onCanFitTreeChange={setDiffCanFitFiles}
          diffStyle={diffStyle}
          onCanFitSplitChange={setDiffCanFitSplit}
          wordWrap={diffWordWrap}
          wordDiff={diffWordDiff}
          hideWhitespace={diffHideWhitespace}
          onIsLargeChange={setDiffIsLarge}
          foldAllRequest={diffFoldAll}
          activePath={selectedPath}
          onSelectFile={setSelectedPath}
          renderFileMenu={changeFileMenu}
          onNeedPatch={requestFilePatch}
          onExpandContext={expandFileContext}
          expandingPaths={expandingPaths}
          fullyExpandedPaths={fullyExpandedPaths}
          emptyLabel={diffState === "loading"
            ? t("正在读取差异", "Loading the diff")
            : t("这个范围没有行级差异", "No line-level diff in this scope")}
          listHeader={(
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
          )}
          listFooter={(
            <>
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
            </>
          )}
        />
      ) : (
        <div className="git-review__empty">
          <Check size={20} />
          {currentSnapshot.untracked > 0
            ? t(
              "没有已跟踪的变更；{count} 个未跟踪文件不在审阅范围内。",
              "No tracked changes. {count} untracked files are out of review scope.",
              { count: currentSnapshot.untracked }
            )
            : t("工作区没有变更", "Working tree is clean")}
        </div>
      )}
    </section>
  );

  return (
    <SidePane
      id={paneId}
      title={t("审阅", "Review")}
      header={paneHeader}
      menuSections={paneMenuSections}
      expanded={paneExpanded}
      onToggleExpand={onPaneToggleExpand}
      onFocus={onPaneFocus}
      onClose={onPaneClose}
    >
    <section className="git-review-panel" aria-label={t("Git 审阅", "Git review")}>
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
      <div className="git-review__content">{renderChanges()}</div>
    </section>
    </SidePane>
  );
}
