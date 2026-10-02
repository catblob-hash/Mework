import {
  ArrowUp,
  ChevronDown,
  CircleAlert,
  FileText,
  Folder,
  FolderPlus,
  GitBranch,
  GitCompareArrows,
  Globe,
  History,
  LoaderCircle,
  ListChecks,
  PanelRight,
  RotateCcw,
  Server,
  Settings,
  SlidersHorizontal,
  Square,
  SquareTerminal,
  X
} from "lucide-react";
import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";
import { createPortal } from "react-dom";
import type { CSSProperties, ReactNode } from "react";
import { CommonErrorBoundary } from "./components/ErrorBoundary";
import { MeworkMark } from "./components/MeworkIcon";
import { ComposerAddFiles } from "./components/ComposerAddMenu";
import { AttachmentDropOverlay, AttachmentNotice } from "./components/AttachmentFeedback";
import { ComposerCat } from "./components/ComposerCat";
import { PopoverMenu, type PopoverMenuItem, type PopoverMenuSection } from "./components/PopoverMenu";
import { ModelCacheMark } from "./components/LockTone";
import { ContextUsageMeter } from "./components/ContextUsageMeter";
import { ImageStrip } from "./components/ImageStrip";
import { usePastedTextTags } from "./components/PastedTextTags";
import { SelectedElementChips } from "./components/SelectedElementChips";
import { imagesWithoutElementCrops, selectedElementImageFile } from "./lib/selectedElement";
import { ConversationSettings } from "./components/ConversationSettings";
import {
  StreamedConversationView,
  StreamedSubagentPanel,
  subagentViewMessages
} from "./components/StreamedConversationView";
import { QuestionDock } from "./components/QuestionDock";
import { SubagentTabBar } from "./components/SubagentPanel";
import { TasksPaneTabBar, tasksPaneTabLabel } from "./components/TasksPaneTabBar";
import { ToolApprovalDock } from "./components/ToolApprovalDock";
import { QueuedMessageList } from "./components/QueuedMessageList";
import { Dialog, IconButton } from "./components/Common";
import { GlobalSettings } from "./components/GlobalSettings";
import { WindowLayerOutlet, WindowLayerProvider } from "./components/WindowLayer";
import {
  clampSidebarWidth,
  Sidebar,
  SIDEBAR_DEFAULT_WIDTH
} from "./components/Sidebar";
import type { ConversationStatus } from "./components/Sidebar";
import { ShellNav, useWindowFullscreen, WindowFrame } from "./components/WindowChrome";
import { ConversationSearch } from "./components/ConversationSearch";
import { windowChromeKind } from "./lib/windowChrome";
import {
  conversationHistoryTarget,
  EMPTY_CONVERSATION_HISTORY,
  visitConversation
} from "./lib/conversationHistory";
import type { ConversationHistory } from "./lib/conversationHistory";
import { RemoteDirectoryPicker } from "./components/RemoteDirectoryPicker";
import {
  machineIcon,
  previewWorkspaceMenuItems,
  ProjectSelector,
  terminalShellMenuItems,
  terminalWorkspaceMenuItems,
  WorkspaceMemberSelector
} from "./components/ProjectChips";
import { ProjectDialog } from "./components/ProjectDialog";
import {
  MachineSettingsDialog,
  type MachineShellsControl
} from "./components/MachineDialogs";
import { WorkspaceSettingsDialog } from "./components/WorkspaceSettings";
import { ForkRequestTray } from "./components/ForkRequestTray";
import { detachAbsentParents, reparentChildren } from "./lib/conversationTree";
import { GitStatusCard } from "./components/GitStatusCard";
import {
  TasksPane,
  taskContainerMessages,
  type TaskItem
} from "./components/TaskContainer";
import { deriveWorkflowProgress } from "./lib/workflowProgress";
import type { WorkflowProgressView } from "./lib/workflowProgress";
import { countRunningTasks, deriveTaskItems, shellTaskDirectory, shellTaskTitle } from "./lib/taskContainer";
import type { TaskSources } from "./lib/taskContainer";
import { reorderItems } from "./components/usePointerDrag";
import { createId } from "./lib/id";
import { estimateContextsTokens, liveContextTokens } from "./lib/contextTokens";
import { appendsTools, hasUsableBaseUrl, isEncryptedReasoning, supportsVision } from "./lib/modelCapabilities";
import {
  imageShortIdsInUse,
  reserveQueuedMessageIds
} from "./lib/imageShortIds";
import { messageAttachmentAdder } from "./lib/imagePaste";
import { useAttachmentDropZone } from "./lib/attachmentDrop";
import type { PastedText } from "./lib/pastedText";
import { contextsContainProjectedImages } from "./lib/imageBudget";
import {
  applyStreamedRunContexts,
  browserAutomationToolForRun,
  contextsFromInterruptedRun,
  contextsFromModelRun,
  mergeContextsWithStreamingRun,
  mergeUniqueContexts
} from "./lib/runContexts";
import { createModelRunController } from "./lib/modelRunController";
import { useStoreSelector } from "./lib/useStoreSelector";
import {
  createSendPipeline,
  type ContextUsage,
  type ModelRunErrorState,
  type SendPipelineHost
} from "./lib/sendPipeline";
import {
  availableShellBackends,
  knownShells,
  listMachineShells,
  probeMachineShells,
  sshEndpoint,
  terminalShellsFor,
  toolsForShellBackends,
  withAgentShell,
  withDefaultAgentShell,
  withoutProbe
} from "./lib/machineShells";
import {
  conversationWorkspaces,
  hostIsWindows,
  isReservedWorkspace,
  isTemporaryWorkspace,
  machineUsage,
  projectWorkspaces,
  registeredProjectWorkspaces,
  terminalShellLabel,
  runEnvKey,
  sameMachine,
  TEMPORARY_WORKSPACE_ID,
  withWorkspaceArgument,
  workspaceDirectoryLabel,
  workspaceEnvKey,
  workspaceLocationTitle,
  withConversationWorktree,
  worktreeFor,
  worktreeName
} from "./lib/workspaces";
import {
  modelCacheWarmUntil,
  planModeLocked,
  refreshedToolLock,
  restoreLockedSettings,
  toolLockModelOf,
  toolLockOf,
  toolLockState,
  withRunToolLock
} from "./lib/toolLock";
import { grantsWebFetch } from "./lib/webSearch";
import type { NewConversationSource, TerminalShell } from "./lib/workspaces";
import {
  draftAsConversation,
  DRAFT_CONVERSATION_ID,
  isDraftConversationId,
  isUnsentConversation,
  visibleConversations
} from "./lib/draftConversation";
import type { DraftConversationState } from "./lib/draftConversation";
import {
  conversationHasNoContexts,
  createConversationBodyCache,
  hollowConversation,
  isBodyUnloaded,
  keepUnloadedBody
} from "./lib/conversationBodies";
import { memoryPool } from "./lib/memoryPool";
import { hasConversationCommands, listWslDistros, settleConversationTitle, startedOnFreshInstall } from "./lib/runtime";
import { setUpClaudeAgentOnFirstLaunch } from "./lib/claudeAgentFirstLaunch";
import { DEFAULT_REASONING_EFFORT, REASONING_EFFORTS } from "./lib/reasoningEffort";
import { loadToolExplanations } from "./lib/localModel";
import { applyConversationTemplate, attestEditedToolContext, attestInsertedToolContext, cancelConversationRun, cancelModelRun, defaultConversationWebSearchSettings, deleteConversationTemplate, deleteHook, deleteMcpServer, deleteSkill, executeTool, forkConversationContexts, captureConversationTemplate, listConversationTemplates, listForkDecisions, listPendingForkStarts, listPendingForkRequests, listPendingToolPrompts, listWakePendingConversations, loadConversationPlan, loadConversationRemote, loadDocument, previewConversationTemplate, probeMcpServer, refreshCapabilities, revealCapabilityLocation, updateConversationTemplate, requestToolApproval, resetDocument, resolveForkRequest, resolveToolPrompt, skipWorkflowStep, workflowStepRecord } from "./lib/runtime";
import { SECURITY_LEVEL_OPTIONS, securityLevelLabel } from "./lib/securityLevels";
import {
  answersFromFormattedContent,
  isClaudeQuestionInput,
  questionsFromInput
} from "./lib/orchestration";
import {
  approvalPromptSubagentView,
  deriveSubagentViews,
  findExternalStepBodyRef,
  findOpenableSubagentView,
  graftExternalStepBodies
} from "./lib/subagents";
import type { SubagentView } from "./lib/subagents";
import { BrowserPanel, splitBrowserAddress } from "./components/BrowserPanel";
import { PreviewPageTabs } from "./components/PreviewPageTabs";
import { GitReviewPageLabel, GitReviewPanel } from "./components/GitReviewPanel";
import { PageTabs } from "./components/PageTabs";
import type { GitReviewRevealRequest } from "./components/GitReviewPanel";
import { ShellTaskPanel } from "./components/ShellTaskPanel";
import type { ContextMenuAnchor, ContextMenuSection } from "./components/ContextMenu";
import { PathChoiceMenu, closePathChoice, showPathChoice } from "./components/PathChoiceMenu";
import { PathText } from "./components/PathText";
import { FilesPane } from "./components/FilesPane";
import type { FilesPaneOpenRequest, FilesPaneWorkspace } from "./components/FilesPane";
import {
  browseMachineLabel,
  browsePath,
  browseProbePaths,
  formatAddress,
  isWindowsPath,
  locationKey,
  resolveBrowsePath,
  sameBrowseMachine
} from "./lib/fileBrowser";
import type { BrowseMachine, BrowseProbeResult } from "./lib/fileBrowser";
import { setPathOpenHandler, setPathPrefetchHandler } from "./lib/pathLinks";
import type { PathOpenRequest } from "./lib/pathLinks";
import { workspaceRelativePath } from "./lib/workspaceFiles";
import { HistoryPane } from "./components/HistoryPane";
import { PaneTiles } from "./components/PaneTiles";
import { PaneToolbar } from "./components/PaneToolbar";
import { innerBottomRadius, SidePane, type SidePaneBounds } from "./components/SidePane";
import { PlanPane, planStatusLabel, planTitle } from "./components/PlanPage";
import { TerminalPanel, terminalPanelId } from "./components/TerminalPanel";
import type { TerminalPanelHandle } from "./components/TerminalPanel";
import { TerminalTabBar } from "./components/TerminalTabBar";
import {
  hasBackendRuntime,
  isBrowserDevRuntime,
  isTauriRuntime,
  onBrowserDevReconnected
} from "./lib/backend";
import { onAppPushEvent } from "./lib/appEvents";
import { hasNativeWorkspacePicker, pickWorkspaceDirectory } from "./lib/workspacePicker";
import { createTerminalController, terminalSessionIsLive, terminalSessionKey } from "./lib/terminalController";
import { liveTerminalCount } from "./lib/terminal";
import type { TerminalLaunchChoice, TerminalSessionState } from "./lib/terminal";
import {
  initialTerminalTabsState,
  READ_ONLY_TERMINAL_TAB_ID,
  terminalStripIds,
  terminalTabsFor,
  terminalTabsReducer
} from "./lib/terminalTabs";
import type { TerminalTab, TerminalTabsAction } from "./lib/terminalTabs";
import { listShellTasks, stopConversationTask, stopShellTask } from "./lib/shellTasks";
import type { ShellTaskSnapshot } from "./lib/shellTasks";
import { isPreviewPageToolName } from "./lib/taskTools";
import {
  closeBrowserSession,
  getBrowserStatus,
  navigateBrowser,
  openBrowser,
  performBrowserAction,
  setBrowserPageNetwork,
  setBrowserPanelBounds
} from "./lib/browser";
import {
  startBrowserRendererMountHeartbeat,
  stopBrowserRendererMountHeartbeat
} from "./lib/browserRendererMount";
import type { BrowserCloseDisposition, BrowserStatus } from "./lib/browser";
import {
  listPreviewServers,
  previewServerAddress,
  previewUrlIsServedAt,
  stopPreviewServer,
  type PreviewServerSnapshot,
  type PreviewTarget
} from "./lib/preview";
import type {
  GitBranch as GitBranchInfo,
  GitCheckoutRef,
  GitTarget,
  GitWorkspaceSnapshot
} from "./lib/git";
import {
  createConversationWorktree,
  executeGitAction,
  getGitBranches,
  gitConversationTarget,
  gitPeerBlocksMutation,
  gitSnapshotKey,
  gitSnapshotKeyConversation,
  gitSurfaceKey,
  gitSurfaceProjectId,
  gitWorkspaceTarget,
  releaseConversationWorktree
} from "./lib/git";
import { useGitSurfacePolling } from "./lib/gitPolling";
import type { GitPollSurface } from "./lib/gitPolling";
import type { GitRefreshHandlers } from "./lib/gitController";
import {
  createGitController,
  gitReviewListsPath,
  gitReviewSnapshotCacheKey,
  gitSnapshotBroadcastIds,
  gitSnapshotForWorkspace,
  gitSnapshotsAfterDraftRedemption,
  gitSnapshotsAfterWorkspaceMutation
} from "./lib/gitController";
import {
  expandedPane,
  focusedPane,
  isPrimaryPreviewSession,
  loadSidePanesState,
  newPreviewPageSessionId,
  openPreviewSession,
  paneIsOpen,
  paneKind,
  paneTarget,
  persistSidePanesState,
  previewPaneId,
  previewSessionBelongsToConversation,
  previewSessionsFor,
  previewWorkspaceOf,
  shownSubagent,
  shownTasksTab,
  sidePaneDomId,
  sidePaneLayoutFor,
  sidePanesReducer,
  subagentHistoryPaneId,
  type SidePaneId,
  type SidePanesAction,
  type TasksPaneTab
} from "./lib/sidePanes";
import { resolveApplicationLanguage, translate, useI18n } from "./i18n";
import { configureApplicationAppearance, getResolvedTheme, useResolvedTheme } from "./theme";
import type { ApplicationTheme } from "./theme";
import { backgroundAfterThemeChange } from "./lib/background";
import { ZOOM_STEP, clampZoom, defaultAppearancePreferences } from "./lib/appearance";
import {
  SHORTCUT_COMMANDS,
  isImeKeyEvent,
  matchesEvent,
  resolveShortcut,
  shouldSuppressForFocus
} from "./lib/shortcuts";
import { localizeToolDescriptor } from "./lib/toolDefaults";
import {
  applyGlobalSettingsChange,
  applyQuarantinedContextReplacements,
  findConversation,
  type GlobalSettingsChange
} from "./lib/documentUpdates";
import { createDocumentStore } from "./lib/documentStore";
import type { DocumentStore } from "./lib/documentStore";
import { createConversationSync } from "./lib/conversationSync";
import { createComposerController } from "./lib/composerController";
import { createBrowserController } from "./lib/browserController";
import {
  cumulativeModelRunUsage,
  type ModelRuns,
  type ModelRunState
} from "./lib/modelStream";
import {
  applyConversationPresetSettings,
  captureConversationPresetSettings,
  cloneConversationSettings,
  conversationPresetById,
  defaultConversationPreset,
  isBuiltinConversationPreset,
  sameConversationPresetSettings
} from "./lib/conversationPresets";
import {
  contextBranchNavigations,
  isConversationBranchFork,
  switchConversationBranch
} from "./lib/conversationBranches";
import {
  EMPTY_TIMELINE_PATCH,
  TimelineHistory,
  applyTimelinePatch,
  insertionPatch,
  invertTimelinePatch,
  placedContexts,
  removedIds,
  timelineHistoryShortcut
} from "./lib/timelineHistory";
import type { TimelinePatch } from "./lib/timelineHistory";
import { forkOrigin, forkTitle, nextForkNumber, staleForkTitles } from "./lib/conversationForks";
import {
  TIMELINE_START_ANCHOR,
  annotateTurnFailure,
  clearTurnFailures,
  dropEmptyConversationTurns,
  findResumableTurn,
  loadConversationTurns,
  materializeRunTurnContexts,
  modelUsageEqual,
  resumeConversationTurn,
  saveConversationTurns,
  subtractModelUsage,
  sumModelUsage
} from "./lib/conversationTurns";
import type { ConversationTurn, ConversationTurnError, ConversationTurns } from "./lib/conversationTurns";
import type {
  ApiProvider,
  AppDocument,
  AttachedWorkspace,
  ConversationTemplateSummary,
  ContextItem,
  Conversation,
  ConversationForkOrigin,
  ConversationPlan,
  ConversationPreset,
  ConversationPresetSettings,
  ConversationSettings as ConversationSettingsType,
  ConversationToolLock,
  FileAttachment,
  ImageAttachment,
  InsertableContextKind,
  JsonObject,
  ModelProfile,
  ModelUsage,
  PendingForkRequest,
  ForkDecisionRecord,
  PendingToolPrompt,
  QuestionResponse,
  ResourceDescriptor,
  RunTarget as RunTargetType,
  SandboxSettings,
  SecurityLevel,
  SshMachineConfig as SshMachineConfigType,
  MachineShells,
  ShellBackend,
  SettingsView,
  ShortcutCommandId,
  SubagentRunRecord,
  ToolApprovalGrant,
  ToolResult,
  ToolContext,
  ToolPromptDecision,
  UserContext,
  WorkflowProgressEntry,
  Workspace,
  WslDistro
} from "./types";

/** Stable identity for the empty projection so selectors return the same array without an active conversation. */
const NO_PROJECTED_CONTEXTS: ContextItem[] = [];

/** Stable identity for empty workflow entries when no run is active. */
const EMPTY_WORKFLOW_ENTRIES: Record<string, WorkflowProgressEntry[]> = {};
const NO_PASTED_TEXTS: PastedText[] = [];
const EMPTY_WORKFLOW_RUN_IDS: Record<string, string> = {};

/**
 * How long a probe of where a transcript path is stays good for: long enough that the hover's
 * answer serves the click and a second click, short enough that a file just written is found.
 */
const PATH_PROBE_REUSE_MS = 8_000;
/** How long a click on a path waits for its probe before the menu opens to say it is looking. */
const PATH_CHOICE_PATIENCE_MS = 150;

/**
 * Compare subagent chrome fields only. Live contexts are intentionally excluded because
 * App chrome does not consume them; StreamedSubagentPanel subscribes to live text itself.
 *
 * Usage, toolCount, role, and stepIndex must be compared because task and workflow panels
 * read them. Omitting them keeps a stale array in `useStoreSelector` and hides usage-only
 * updates for externalized workflow steps.
 */
export function subagentViewsEqualForChrome(previous: SubagentView[], next: SubagentView[]): boolean {
  if (previous === next) return true;
  if (previous.length !== next.length) return false;
  return previous.every((view, index) => {
    const other = next[index];
    return view.id === other.id
      && view.name === other.name
      && view.ledgerOwner === other.ledgerOwner
      && view.kind === other.kind
      && view.workflowRun === other.workflowRun
      && view.label === other.label
      && view.task === other.task
      && view.status === other.status
      && view.summary === other.summary
      && view.parentId === other.parentId
      && view.depth === other.depth
      && view.phase === other.phase
      && view.phaseIndex === other.phaseIndex
      && view.stepIndex === other.stepIndex
      && view.toolCount === other.toolCount
      && view.role?.name === other.role?.name
      && view.role?.modelId === other.role?.modelId
      && modelUsageEqual(view.usage, other.usage)
      && view.createdAt === other.createdAt
      && view.completedAt === other.completedAt
      && view.callIds.length === other.callIds.length
      && view.callIds.every((id, callIndex) => other.callIds[callIndex] === id)
      && view.childIds.length === other.childIds.length
      && view.childIds.every((id, childIndex) => other.childIds[childIndex] === id)
      && view.updates.length === other.updates.length;
  });
}

const SIDEBAR_WIDTH_STORAGE_KEY = "mework.sidebar-width";
const LEGACY_SIDEBAR_WIDTH_STORAGE_KEY = "naiword.sidebar-width";

function loadSidebarWidth(): number {
  try {
    const storedWidth = Number(
      window.localStorage.getItem(SIDEBAR_WIDTH_STORAGE_KEY)
      ?? window.localStorage.getItem(LEGACY_SIDEBAR_WIDTH_STORAGE_KEY)
    );
    return Number.isFinite(storedWidth) && storedWidth > 0
      ? clampSidebarWidth(storedWidth)
      : SIDEBAR_DEFAULT_WIDTH;
  } catch {
    return SIDEBAR_DEFAULT_WIDTH;
  }
}

type AppShellStyle = CSSProperties & {
  "--sidebar-width": string;
};

type EditorState =
  | { mode: "insert"; kind: InsertableContextKind; index: number; toolName?: string }
  | { mode: "edit"; kind: InsertableContextKind; item: ContextItem; index: number };

type QuestionEditorState = {
  conversationId: string;
  item: ToolContext;
  answer?: UserContext;
};

/**
 * The one line saying what an edit on the timeline just did, and which key takes
 * it back. It belongs to the timeline it was said about.
 */
type TimelineNoticeState = {
  id: string;
  conversationId: string;
  message: string;
  hint?: { keys: string; action: string };
};

/** How long a timeline notice stays up. */
const TIMELINE_NOTICE_MS = 3200;

export {
  gitReviewSnapshotCacheKey,
  gitSnapshotForWorkspace,
  gitSnapshotsAfterRefresh,
  gitSnapshotsAfterWorkspaceMutation
} from "./lib/gitController";

function failureMessage(reason: unknown, fallback: string): string {
  if (reason instanceof Error && reason.message.trim()) return reason.message;
  if (typeof reason === "string" && reason.trim()) return reason;
  return fallback;
}

function activeTurnSegmentDuration(turn: ConversationTurn, endedAt: string): number {
  return Math.max(
    0,
    new Date(endedAt).getTime() - new Date(turn.startedAt).getTime()
  );
}

function isAbsoluteWorkspacePath(value: string): boolean {
  const path = value.trim();
  return path.startsWith("/") || path.startsWith("\\\\") || /^[a-zA-Z]:[\\/]/.test(path);
}

function normalizedWorkspacePath(value: string): string {
  const path = value.trim().replace(/[\\/]+$/, "");
  return /^[a-zA-Z]:[\\/]/.test(path) || path.startsWith("\\\\") ? path.toLocaleLowerCase() : path;
}

const COMPOSER_TEXTAREA_MAX_HEIGHT = 180;

/**
 * The chip label for a directory: its last segment, since a chip cannot hold an
 * absolute path. The full path stays in the chip's `title`, which is what the
 * user checks when two directories share a name.
 */
function directoryLabel(path: string): string {
  return workspaceDirectoryLabel(path);
}

/**
 * The icon that says which machine a workspace is on.
 *
 * Machine and directory are one fact on a chip this small, so the icon carries
 * the machine and the label carries the directory; the full pair is in the
 * chip's `title`, where a user who needs both can read them.
 */
function WorkspaceMachineIcon({ machine }: { machine?: RunTargetType | null }) {
  if (!machine) return <Folder size={13} />;
  return machine.kind === "wsl" ? <SquareTerminal size={13} /> : <Server size={13} />;
}

/** The chip's hover text: the path and, when it is not this machine, where it is. */
function workspaceChipTitle(
  workspace: AttachedWorkspace,
  sshMachines: SshMachineConfigType[]
): string {
  return workspaceLocationTitle(workspace.path, workspace.machine, sshMachines);
}

function allowsNativeContextMenu(target: EventTarget | null): boolean {
  if (!(target instanceof Element)) return false;
  // An xterm surface counts as an input: on right-click xterm has already moved its helper
  // textarea under the pointer with the selection in it, so the native menu's Copy and Paste
  // act on the terminal — the only way a terminal built on a canvas can offer them.
  return target.closest(
    'input, textarea, [contenteditable]:not([contenteditable="false"]), .xterm'
  ) !== null;
}

function resizeComposerTextarea(textarea: HTMLTextAreaElement): void {
  // Collapse first so scrollHeight reflects the current value instead of the
  // textarea's previously expanded height.
  textarea.style.height = "0px";
  textarea.style.height = `${Math.min(COMPOSER_TEXTAREA_MAX_HEIGHT, textarea.scrollHeight)}px`;
}

/**
 * Resolves once the host's document holds `workspaceId`, sending the pending document save first
 * when it does not. A project added in the same event reaches the host through the debounced
 * save, later than a command that names it would; the host refuses what it cannot resolve.
 */
async function awaitWorkspaceAtHost(store: DocumentStore, workspaceId: string): Promise<void> {
  const hostHolds = store.persisted()?.workspaces.some((workspace) => workspace.id === workspaceId);
  if (!hostHolds) await store.flush();
}

function ErrorView({ message, onReset }: { message: string; onReset: () => void }) {
  const { t } = useI18n();
  return (
    <div className="fatal-state">
      <div><CircleAlert size={24} /></div>
      <h1>{t("无法载入 Mework", "Unable to load Mework")}</h1>
      <p>{message}</p>
      <button type="button" className="button button--primary" onClick={onReset}>
        <RotateCcw size={15} />{t("恢复示例数据", "Restore sample data")}
      </button>
    </div>
  );
}

function App() {
  const { resolvedLanguage, t } = useI18n();
  const platform = typeof navigator === "undefined" ? "" : navigator.platform;
  /** Who draws the window's frame (see `lib/windowChrome.ts`); fixed for the process's life. */
  const [windowChrome] = useState(windowChromeKind);
  const windowFullscreen = useWindowFullscreen(windowChrome === "mac");
  // On the root, so the stylesheet can place the top row and portaled layers alike.
  useLayoutEffect(() => {
    const root = window.document.documentElement;
    root.dataset.windowChrome = windowChrome;
    if (windowFullscreen) root.dataset.windowFullscreen = "true";
    else delete root.dataset.windowFullscreen;
  }, [windowChrome, windowFullscreen]);
  useEffect(() => {
    const suppressNativeContextMenu = (event: MouseEvent) => {
      if (event.defaultPrevented || allowsNativeContextMenu(event.target)) return;
      event.preventDefault();
    };
    window.document.addEventListener("contextmenu", suppressNativeContextMenu);
    return () => window.document.removeEventListener("contextmenu", suppressNativeContextMenu);
  }, []);
  /** Authoritative document store; React only subscribes to its publication and save pipeline. */
  const [documentStore] = useState(createDocumentStore);
  /** Bumped as conversation-body fetches start and end, so the loading state redraws. */
  const [, setBodyLoadTick] = useState(0);
  /**
   * Replace local conversation read models with authoritative host-written bodies by id. A
   * conversation whose body is unloaded takes only the metadata: loading a body back is the body
   * cache's call, made when the conversation is opened, not a side effect of a write reply.
   */
  const applyAuthoritativeConversation = useCallback(
    (workspaceId: string, conversation: Conversation) => {
      documentStore.update((current) => current ? {
        ...current,
        workspaces: current.workspaces.map((workspace) => (
          workspace.id === workspaceId
            ? {
              ...workspace,
              conversations: workspace.conversations.map((candidate) => {
                if (candidate.id !== conversation.id) return candidate;
                return isBodyUnloaded(candidate) ? hollowConversation(conversation) : conversation;
              })
            }
            : workspace
        ))
      } : current);
    },
    [documentStore]
  );
  /**
   * Conversation bodies as pooled data (`lib/conversationBodies.ts`): installing a fetched body
   * and dropping an unloaded one are local only — the host already holds both — so neither goes
   * through the conversation command channel.
   */
  const [conversationBodies] = useState(() => createConversationBodyCache({
    pool: memoryPool,
    current: () => documentStore.current(),
    install: (conversation) => documentStore.update((current) => current ? {
      ...current,
      workspaces: current.workspaces.map((workspace) => (
        workspace.conversations.some((candidate) => candidate.id === conversation.id)
          ? {
            ...workspace,
            conversations: workspace.conversations.map((candidate) => (
              candidate.id === conversation.id && isBodyUnloaded(candidate) ? conversation : candidate
            ))
          }
          : workspace
      ))
    } : current),
    unload: (conversationIds) => {
      const unloading = new Set(conversationIds);
      documentStore.update((current) => current ? {
        ...current,
        workspaces: current.workspaces.map((workspace) => (
          workspace.conversations.some((candidate) => unloading.has(candidate.id))
            ? {
              ...workspace,
              conversations: workspace.conversations.map((candidate) => (
                unloading.has(candidate.id) && !isBodyUnloaded(candidate)
                  ? hollowConversation(candidate)
                  : candidate
              ))
            }
            : workspace
        ))
      } : current);
    },
    load: loadConversationRemote,
    enabled: hasConversationCommands,
    onLoadingChange: () => setBodyLoadTick((tick) => tick + 1)
  }));
  useEffect(() => () => conversationBodies.dispose(), [conversationBodies]);
  /** The host exclusively writes conversation bodies; the renderer sends intents through this command channel. */
  const [conversationSync] = useState(() => createConversationSync(
    applyAuthoritativeConversation,
    // A project added in this same event — whose empty task slot is created right after it — is
    // still waiting on the debounced document save. Send that save ahead of the conversation, or
    // the host refuses a conversation in a workspace it has never heard of and the renderer is
    // left holding one the host cannot find.
    (workspaceId) => awaitWorkspaceAtHost(documentStore, workspaceId)
  ));
  const document = useSyncExternalStore(documentStore.subscribe, documentStore.getSnapshot);
  /**
   * Every machine's last shell probe, keyed by environment key: this machine
   * from startup, SSH machines from the first time this session reaches them,
   * WSL distributions from first use, and any machine from its settings. The
   * shell tools a conversation lists and the agent-shell choices a machine's
   * settings offer are read from here.
   */
  const [machineShells, setMachineShells] = useState<Record<string, MachineShells>>({});
  useEffect(() => {
    if (!hasBackendRuntime()) return;
    let cancelled = false;
    void listMachineShells()
      .then((probes) => {
        if (!cancelled && probes && typeof probes === "object") {
          setMachineShells((current) => ({ ...probes, ...current }));
        }
      })
      .catch(() => undefined);
    const unsubscribe = onAppPushEvent((event) => {
      if (event.type !== "machineShellsChanged") return;
      setMachineShells((current) => ({ ...current, [event.key]: event.shells }));
    });
    return () => {
      cancelled = true;
      unsubscribe();
    };
  }, []);
  /**
   * A machine's first probe records its agent shell: the first backend of its
   * OS's priority list that it has. That is the priority list's only use — a
   * machine that already has a choice keeps it whatever the list says later.
   */
  const executionEnvironments = document?.globalSettings.executionEnvironments;
  useEffect(() => {
    if (!executionEnvironments) return;
    const withDefaults = (environments: typeof executionEnvironments) => Object.entries(machineShells)
      .reduce((next, [key, shells]) => withDefaultAgentShell(next, key, shells), environments);
    if (withDefaults(executionEnvironments) === executionEnvironments) return;
    documentStore.update((current) => {
      if (!current) return current;
      const environments = current.globalSettings.executionEnvironments;
      const next = withDefaults(environments);
      return next === environments
        ? current
        : { ...current, globalSettings: { ...current.globalSettings, executionEnvironments: next } };
    });
  }, [documentStore, executionEnvironments, machineShells]);
  /** Use factory preferences before the document loads so consumers always receive valid settings. */
  const appearance = document?.globalSettings.appearance ?? defaultAppearancePreferences();
  // StrictMode unmounts the shared store once during development; resume must pair with
  // dispose so that cleanup cannot permanently disable subsequent edits and saves.
  useEffect(() => {
    documentStore.resume();
    return () => documentStore.dispose();
  }, [documentStore]);
  /**
   * Re-reads the capability catalog from disk and publishes it.
   *
   * Skills, MCP servers and hooks are files the user owns, so the catalog mirrors
   * disk rather than document state and every surface that can have invalidated
   * it calls this: startup, the conversation-settings pane opening, each delete,
   * adding a directory workspace, and the per-page rescan button. An incomplete
   * catalog is discarded rather than published — a missing section would empty a
   * page that in fact has entries — and a failure keeps the previous snapshot,
   * because a temporarily unreadable directory is not a reason to drop rows the
   * user is looking at.
   */
  const rescanCapabilities = useCallback(async () => {
    const discovered = await refreshCapabilities();
    if (!discovered?.skills || !discovered.mcps || !discovered.hooks) return;
    documentStore.update((current) => (
      current ? { ...current, capabilities: discovered } : current
    ));
  }, [documentStore]);
  /** Authoritative terminal sessions; App orchestrates guards, pages, and prompts only. */
  const [terminalController] = useState(createTerminalController);
  const terminalSessions = useSyncExternalStore(terminalController.subscribe, terminalController.current);
  /**
   * The terminal pane's tabs, one per shell. Separate from the session store because a tab
   * outlives its session: a shell that failed keeps its tab, and its verdict, until the user
   * retries it or closes it.
   */
  const [terminalTabsState, setTerminalTabsState] = useState(initialTerminalTabsState);
  /** Authoritative Git snapshots and mutation leases, including last-token-wins polling. */
  const [gitController] = useState(createGitController);
  const gitState = useSyncExternalStore(gitController.subscribe, gitController.current);
  const gitSnapshots = gitState.snapshots;
  const gitMutationConversationIds = gitState.mutationConversationIds;
  /** Authoritative composer drafts, image queue, queued-message save fence, and guides. */
  const [composerController] = useState(createComposerController);
  const composerState = useSyncExternalStore(composerController.subscribe, composerController.current);
  const composerDrafts = composerState.drafts;
  const composerPastedTexts = composerState.pastedTexts;
  const composerImageDrafts = composerState.imageDrafts;
  const composerFileDrafts = composerState.fileDrafts;
  const composerAttachmentNotices = composerState.attachmentNotices;
  const composerElementPicks = composerState.elementPicks;
  const composerImageLoadingIds = composerState.imageLoadingIds;
  const steeringMessageIds = composerState.steeringMessageIds;
  const failedQueuedPromotionIds = composerState.failedQueuedPromotionIds;
  /** Authoritative browser lifecycle state: intent epochs, open/close promises, visible sessions, and tab status. */
  const [browserController] = useState(createBrowserController);
  const browserState = useSyncExternalStore(browserController.subscribe, browserController.current);
  const browserStatuses = browserState.statuses;
  const browserRuntimeReady = browserState.runtimeReady;
  const [loadError, setLoadError] = useState<string | null>(null);
  const [activeWorkspaceId, setActiveWorkspaceIdState] = useState<string | null>(null);
  const [activeConversationId, setActiveConversationIdState] = useState<string | null>(null);
  /**
   * A draft conversation exists only in the renderer until its first sent message materializes it.
   * It does not persist, appear in the sidebar, or occupy a host conversation row, so it retains
   * its own settings until `materializeDraft` converts it at the send boundary. `workspaceId` may
   * be null; sending then targets the temporary workspace.
   */
  const [draftConversation, setDraftConversation] = useState<DraftConversationState | null>(null);
  /**
   * Aiming the draft at another project would end shells and dev servers it still has running, so
   * the move waits for the user. `ownerId` is the draft it was asked about: a draft that has since
   * moved or been sent is no longer the one the answer is for.
   */
  const [draftRetargetPrompt, setDraftRetargetPrompt] = useState<{
    ownerId: string;
    workspaceId: string | null;
    shells: number;
    servers: number;
    proceed: () => void;
  } | null>(null);
  const [sidebarOpen, setSidebarOpen] = useState(true);
  const [sidebarWidth, setSidebarWidth] = useState(loadSidebarWidth);
  const [sidebarResizing, setSidebarResizing] = useState(false);
  const [paneResizing, setPaneResizing] = useState(false);
  /**
   * Which side panes are open beside the conversation, per conversation, plus the live preview
   * sessions each one owns and the side-column widths the user last settled on. One state, not
   * several: "what is on screen" has a single answer instead of flags that had to be kept from
   * contradicting each other.
   */
  const [sidePanesState, setSidePanesState] = useState(loadSidePanesState);
  /**
   * On-demand loaded workflow step bodies, keyed
   * `${conversationId}/${runId}/${stepIndex}`. A timeline record only carries a
   * preview and these retrieval coordinates; the full record is fetched the
   * first time the drawer opens that step and grafted back before view
   * derivation. `null` means the backend definitively answered "no body"
   * (run directory deleted or the write never landed) — the drawer then shows
   * the thin shell instead of retrying forever.
   */
  const [externalStepBodies, setExternalStepBodies] = useState<Record<string, SubagentRunRecord | null>>({});
  /** Coordinates already in flight, so a re-render doesn't double-fetch. */
  const externalStepBodyLoads = useRef<Set<string>>(new Set());
  /** Task ids whose abort was requested and whose run has not yet ended. */
  const [stoppingTaskIds, setStoppingTaskIds] = useState<string[]>([]);
  /**
   * Shell commands running across every conversation, keyed only by their own
   * id. Push events from the backend are the source of truth; the sidebar
   * filters this down to the conversation it is showing.
   */
  const [shellTasks, setShellTasks] = useState<ShellTaskSnapshot[]>([]);
  /** Ids the host evicted; a task list still in flight when the eviction arrived must not restore them. */
  const evictedShellTaskIdsRef = useRef(new Set<string>());
  const [modelStoppingIds, setModelStoppingIds] = useState<Set<string>>(() => new Set());
  const [browserAutomationStoppingIds, setBrowserAutomationStoppingIds] = useState<Set<string>>(() => new Set());
  const [globalSettingsView, setGlobalSettingsView] = useState<SettingsView | null>(null);
  const [editor, setEditor] = useState<EditorState | null>(null);
  const [questionEditor, setQuestionEditor] = useState<QuestionEditorState | null>(null);
  const [workspaceDialogOpen, setWorkspaceDialogOpen] = useState(false);
  const [assignWorkspaceAfterAdd, setAssignWorkspaceAfterAdd] = useState(false);
  /**
   * Which of its project's workspaces each conversation's composer is looking at, 1-based.
   * Absent is the first. Choosing one changes nothing in the conversation: it only decides
   * which directory the Git chip, the status card and the review pane describe, and where the
   * composer's terminal button opens a shell. Kept per conversation for the session only.
   */
  const [selectedWorkspaceMembers, setSelectedWorkspaceMembers] = useState<Record<string, number>>({});
  /**
   * The review pane's open page for each conversation — the project workspace it reviews,
   * 1-based — when the reader chose one. Absent follows the composer's workspace chip.
   */
  const [reviewPageMembers, setReviewPageMembers] = useState<Record<string, number>>({});
  /**
   * The order the reader dragged each conversation's review pages into, by workspace number.
   * Workspaces missing from it keep the project's order after the ones it names; reordering a
   * page never renumbers a workspace, which is how the model addresses it.
   */
  const [reviewPageOrders, setReviewPageOrders] = useState<Record<string, number[]>>({});
  /**
   * Why the last read of each checkout failed, by its `gitSnapshotKey`, until one succeeds. The
   * snapshot it had stays on screen — a machine that went to sleep is still the checkout it was —
   * and its review page says the state is not current.
   */
  const [gitSurfaceFailures, setGitSurfaceFailures] = useState<Record<string, string>>({});
  /** The project whose edit dialog is open, from the sidebar's project menu. */
  const [projectEditor, setProjectEditor] = useState<string | null>(null);
  const editedProject = projectEditor
    ? document?.workspaces.find((workspace) => (
      workspace.id === projectEditor && workspace.kind === "directory"
    )) ?? null
    : null;
  /** Reload branch lists whenever their conversation is revisited because repository state is live. */
  const [branchPicker, setBranchPicker] = useState<{
    conversationId: string;
    status: "loading" | "ready" | "error";
    branches: GitBranchInfo[];
    message?: string;
  } | null>(null);
  const [branchChipError, setBranchChipError] = useState<string | null>(null);
  /** Authoritative model-run state: fine-grained stream commits, summary projection, and send guards. */
  const [modelRunController] = useState(createModelRunController);
  /** Stream deltas do not change summaries, avoiding App-wide re-renders; fine-grained subscriptions live in streamed views. */
  const modelRunSummaries = useSyncExternalStore(
    modelRunController.subscribeSummaries,
    modelRunController.summaries
  );
  const [conversationTurns, setConversationTurns] = useState<ConversationTurns>(loadConversationTurns);
  const [modelRunErrors, setModelRunErrors] = useState<Partial<Record<string, ModelRunErrorState>>>({});
  const [contextUsage, setContextUsage] = useState<Record<string, ContextUsage>>({});
  /** Tool calls waiting on an approval card, oldest first per conversation. A
   * queue rather than a single slot: a subagent can raise its own card while
   * the main session's is still up. */
  const [toolPrompts, setToolPrompts] = useState<Record<string, PendingToolPrompt[]>>({});
  /** The plan document each conversation owns, by conversation id. `null` means
   * "loaded, and there is none"; a missing key means "not loaded yet". Host
   * state rather than document state, so it is fetched and pushed, never saved. */
  const [plans, setPlans] = useState<Record<string, ConversationPlan | null>>({});
  /** Bumped by every plan push. A load answered after a push is stale, however
   * fresh it looked when it was issued. */
  const planPushCountRef = useRef(0);
  /** The model's fork requests awaiting the user, oldest first; drawn by the top-right tray. */
  const [forkRequests, setForkRequests] = useState<PendingForkRequest[]>([]);
  /** Answered fork requests per source conversation, oldest first. Task-bar rows only: the host
   * keeps them, the model never sees them. */
  const [forkDecisions, setForkDecisions] = useState<Record<string, ForkDecisionRecord[]>>({});
  const appendForkDecision = useCallback((decision: ForkDecisionRecord) => {
    setForkDecisions((current) => {
      const existing = current[decision.sourceConversationId] ?? [];
      // A reload and the live event can both carry the same decision.
      if (existing.some((record) => record.forkId === decision.forkId)) return current;
      return { ...current, [decision.sourceConversationId]: [...existing, decision] };
    });
  }, []);
  /** Which card of the stack is shown, per conversation. Raw and unclamped —
   * the derivation below clamps against the live queue, so answering a card
   * needs no bookkeeping here. */
  const [toolPromptCursors, setToolPromptCursors] = useState<Record<string, number>>({});
  /** Settlers for cards the renderer raised itself, keyed by prompt id. Only a
   * manual call has one: a model card's answer goes to the blocked worker. */
  const manualToolPromptsRef = useRef(new Map<string, {
    resolve: (grant: ToolApprovalGrant) => void;
    reject: (error: Error) => void;
  }>());
  const saveStatus = useSyncExternalStore(documentStore.subscribeSaveStatus, documentStore.getSaveStatus);
  /** Preserve the host's specific save-rejection reason so a failed document is diagnosable. */
  const [saveFailureReason, setSaveFailureReason] = useState<string | null>(null);
  useEffect(() => documentStore.subscribeSaveFailures((failure) => {
    const message = failureMessage(failure.error, "");
    console.error("[mework] document save rejected", failure.phase, failure.error);
    setSaveFailureReason(message || null);
  }), [documentStore]);
  useEffect(() => {
    if (saveStatus === "saved") setSaveFailureReason(null);
  }, [saveStatus]);
  const saveStatusChip = saveStatus === "saved" ? null : (
    <span
      className={`save-status save-status--${saveStatus}`}
      aria-live="polite"
      title={saveStatus === "error" && saveFailureReason ? saveFailureReason : undefined}
    >
      {saveStatus === "saving" && <LoaderCircle size={12} className="spin" />}
      {saveStatus === "error" && <CircleAlert size={12} />}
      {saveStatus === "saving" ? t("保存中", "Saving") : t("保存失败", "Save failed")}
      {saveStatus === "error" && saveFailureReason && (
        <span className="save-status__reason">{saveFailureReason}</span>
      )}
    </span>
  );
  const [deletingWorkspaceIds, setDeletingWorkspaceIds] = useState<Set<string>>(() => new Set());
  const [deletingConversationIds, setDeletingConversationIds] = useState<Set<string>>(() => new Set());
  /* What each timeline remembers of its own edits, for its undo keys. In memory
     only, one budget shared by every conversation (`timelineHistory.ts`). */
  const timelineHistoryRef = useRef<TimelineHistory | null>(null);
  if (!timelineHistoryRef.current) timelineHistoryRef.current = new TimelineHistory();
  const [timelineNotice, setTimelineNotice] = useState<TimelineNoticeState | null>(null);
  const timelineNoticeTimerRef = useRef<number | null>(null);
  const contextScrollRef = useRef<Record<string, number>>({});
  const conversationTurnsRef = useRef<ConversationTurns>(conversationTurns);
  const deletingWorkspaceIdsRef = useRef(new Set<string>());
  const deletingConversationIdsRef = useRef(new Set<string>());
  const activeWorkspaceIdRef = useRef<string | null>(activeWorkspaceId);
  const activeConversationIdRef = useRef<string | null>(activeConversationId);
  /** Event handlers read the draft ref because they cannot wait for the next render. */
  const draftConversationRef = useRef<DraftConversationState | null>(draftConversation);
  draftConversationRef.current = draftConversation;
  /* The draft owns its settings from the moment it is opened, the way any conversation does, so
   * they are kept with the document: a restart brings back what the user set on it rather than
   * rebuilding it from the preset it was copied from. Only the settings and their preset trace are
   * kept; its text, grants and shells belong to this run of the app. */
  const draftSettings = draftConversation?.settings ?? null;
  const draftPresetId = draftConversation?.presetId ?? "";
  useEffect(() => {
    if (!draftSettings) return;
    documentStore.update((current) => {
      if (!current) return current;
      const stored = current.globalSettings.draftConversation;
      if (stored?.settings === draftSettings && stored.presetId === draftPresetId) return current;
      return {
        ...current,
        globalSettings: {
          ...current.globalSettings,
          draftConversation: { settings: draftSettings, presetId: draftPresetId }
        }
      };
    });
  }, [documentStore, draftPresetId, draftSettings]);
  /**
   * The id the host knows a conversation's terminals and preview page by: its own, or for the
   * draft the id it will materialize as. Terminal tabs, terminal sessions and browser sessions are
   * keyed by it rather than by the draft's placeholder id, so the draft becoming real moves none
   * of them. Pane layout is the exception: it follows the placeholder and is adopted on send.
   */
  const hostConversationId = useCallback((conversationId: string): string => (
    isDraftConversationId(conversationId)
      ? draftConversationRef.current?.materializesAs ?? conversationId
      : conversationId
  ), []);
  /**
   * Whether `ownerId` — a conversation, or the draft by the id it will materialize as — works in
   * `workspace`. The draft is in no workspace's conversation list, yet its shells sit in the
   * project it is aimed at like any conversation's, and Git has to see them there.
   */
  const ownerWorksIn = useCallback((ownerId: string, workspace: Workspace): boolean => {
    if (workspace.conversations.some((conversation) => conversation.id === ownerId)) return true;
    const draft = draftConversationRef.current;
    return draft?.materializesAs === ownerId
      && (draft.workspaceId ?? TEMPORARY_WORKSPACE_ID) === workspace.id;
  }, []);
  /** Whether the draft is aimed at `workspace`, where it shares the root checkout with its conversations. */
  const draftWorksIn = useCallback((workspace: Workspace): boolean => {
    const draft = draftConversationRef.current;
    return Boolean(draft && (draft.workspaceId ?? TEMPORARY_WORKSPACE_ID) === workspace.id);
  }, []);
  /** Keep this changing callback in a ref so startup effects do not rerun when the document changes. */
  const openDraftConversationRef = useRef<(
    workspaceId?: string,
    source?: NewConversationSource
  ) => void>(() => {});
  const composerTextareaRef = useRef<HTMLTextAreaElement>(null);
  /** One handle per mounted terminal tab; the tab's close control drives the panel through it. */
  const terminalPanelHandlesRef = useRef(new Map<string, TerminalPanelHandle>());
  const sidePanesStateRef = useRef(sidePanesState);
  const terminalTabsStateRef = useRef(terminalTabsState);
  /**
   * What a terminal nobody chose a shell for starts: the workspace the composer has selected, in
   * the most preferred shell its machine was probed to have. Kept current every render, so the
   * pane opening with nothing in it reads the selection as it is now.
   */
  const defaultTerminalLaunchRef = useRef<TerminalLaunchChoice | null>(null);
  const modelStoppingIdsRef = useRef(new Set<string>());
  const browserAutomationStoppingIdsRef = useRef(new Set<string>());
  /** Update refs and state together so handlers immediately observe the current value. */
  const setActiveWorkspaceId = useCallback((workspaceId: string | null) => {
    activeWorkspaceIdRef.current = workspaceId;
    setActiveWorkspaceIdState(workspaceId);
  }, []);
  const setActiveConversationId = useCallback((conversationId: string | null) => {
    activeConversationIdRef.current = conversationId;
    setActiveConversationIdState(conversationId);
  }, []);
  /** Reduce from the ref so consecutive dispatches in one tick observe each other. */
  const dispatchSidePanes = useCallback((action: SidePanesAction) => {
    const next = sidePanesReducer(sidePanesStateRef.current, action);
    if (next === sidePanesStateRef.current) return;
    if (next.lastSideFlexByKind !== sidePanesStateRef.current.lastSideFlexByKind) persistSidePanesState(next);
    sidePanesStateRef.current = next;
    setSidePanesState(next);
  }, []);
  /** Reduce from the ref so consecutive dispatches in one tick observe each other. */
  const dispatchTerminalTabs = useCallback((action: TerminalTabsAction) => {
    const next = terminalTabsReducer(terminalTabsStateRef.current, action);
    if (next === terminalTabsStateRef.current) return;
    terminalTabsStateRef.current = next;
    setTerminalTabsState(next);
  }, []);
  const updateGitSnapshots = gitController.updateSnapshots;
  const conversationOperationIsActive = useCallback((conversationId: string) => (
    modelRunController.hasRunToken(conversationId)
    || modelRunController.hasPerformingRun(conversationId)
    || modelRunController.hasPreparingRun(conversationId)
    || gitController.mutationIsActive(conversationId)
  ), [gitController]);
  const conversationIsInLockedWorkspace = useCallback((conversationId: string) => {
    return Boolean(documentStore.current()?.workspaces.some((workspace) => (
      deletingWorkspaceIdsRef.current.has(workspace.id)
      && workspace.conversations.some((conversation) => conversation.id === conversationId)
    )));
  }, []);
  const contextMutationIsBlocked = useCallback((conversationId: string) => (
    conversationOperationIsActive(conversationId)
    || conversationIsInLockedWorkspace(conversationId)
  ), [conversationIsInLockedWorkspace, conversationOperationIsActive]);
  const conversationIsBusy = useCallback((conversationId: string) => (
    Boolean(modelRunController.current()[conversationId]) || contextMutationIsBlocked(conversationId)
  ), [contextMutationIsBlocked]);
  /**
   * The activity indicator must derive from React state. It includes model streams, unfinished
   * background commands, and busy running terminals, but excludes idle terminals and browser pages.
   */
  const conversationHasLiveActivity = useCallback((conversationId: string) => (
    Boolean(modelRunSummaries[conversationId])
    || shellTasks.some((task) => task.conversationId === conversationId && !task.outcome)
    || Object.values(terminalSessions).some((session) => (
      session.conversationId === conversationId && session.phase === "running" && session.busy
    ))
  ), [modelRunSummaries, shellTasks, terminalSessions]);
  const conversationModelRunIsActive = useCallback((conversationId: string) => (
    Boolean(modelRunController.current()[conversationId])
    || modelRunController.hasRunToken(conversationId)
    || modelRunController.hasPerformingRun(conversationId)
    || modelRunController.hasPreparingRun(conversationId)
  ), []);
  /**
   * Takes the Git mutation lease for a write to the conversation's project workspace `member`
   * (1-based). Another conversation blocks it only while it may be working in the same checkout
   * of that workspace — not in a worktree of its own — which is how the host decides too.
   */
  const beginGitMutation = useCallback((conversationId: string, member = 1): boolean => {
    if (modelRunController.current()[conversationId] || contextMutationIsBlocked(conversationId)) return false;
    // Drafts are absent from workspace conversations, so resolve their selected workspace directly.
    const draftWorkspaceId = isDraftConversationId(conversationId)
      ? draftConversationRef.current?.workspaceId ?? null
      : null;
    const workspace = draftWorkspaceId
      ? documentStore.current()?.workspaces.find((candidate) => candidate.id === draftWorkspaceId)
      : documentStore.current()?.workspaces.find((candidate) => (
        candidate.conversations.some((conversation) => conversation.id === conversationId)
      ));
    if (!workspace) return false;
    const registered = registeredProjectWorkspaces(workspace)[member - 1];
    const surface = gitSurfaceKey(workspace.id, member);
    const snapshots = gitController.current().snapshots;
    const isolated = (conversation: Pick<Conversation, "worktrees"> | undefined) => Boolean(
      registered && worktreeFor(conversation, member, registered)
    );
    // A peer running a model in its own checkout is not writing here, which is
    // also how the host decides. The draft has no worktree of its own.
    const acting: GitCheckoutRef = {
      snapshot: gitSnapshotForWorkspace(snapshots[gitSnapshotKey(conversationId, member)], surface),
      isolated: isolated(workspace.conversations.find((conversation) => conversation.id === conversationId))
    };
    // The draft aimed here is a peer at the root checkout too, though no list holds it.
    const peers: Array<Pick<Conversation, "id" | "worktrees">> = [
      ...workspace.conversations,
      ...(draftWorksIn(workspace) ? [{ id: DRAFT_CONVERSATION_ID, worktrees: [] }] : [])
    ];
    if (
      peers.some((peer) => (
        peer.id !== conversationId
        && gitPeerBlocksMutation({
          acting,
          peer: {
            snapshot: gitSnapshotForWorkspace(snapshots[gitSnapshotKey(peer.id, member)], surface),
            isolated: isolated(peer)
          },
          peerModelRunActive: conversationModelRunIsActive(peer.id),
          peerGitMutationActive: gitController.mutationIsActive(peer.id)
        })
      ))
      || Object.values(terminalController.current()).some((session) => (
        session.busy && ownerWorksIn(session.conversationId, workspace)
      ))
    ) return false;
    gitController.acquireMutationLease(
      conversationId,
      // Every poll of this workspace must be invalidated, the draft's included.
      [...new Set([...peers.map((peer) => peer.id), conversationId])]
    );
    return true;
  }, [
    contextMutationIsBlocked,
    conversationModelRunIsActive,
    draftWorksIn,
    gitController,
    ownerWorksIn,
    terminalController
  ]);
  const endGitMutation = useCallback((conversationId: string) => {
    gitController.releaseMutationLease(conversationId);
  }, [gitController]);
  const activeLayout = sidePaneLayoutFor(sidePanesState, activeConversationId);
  const currentPreviewSessions = previewSessionsFor(sidePanesState, activeConversationId);
  const activeFocusedPane = focusedPane(activeLayout);
  const activeExpandedPane = expandedPane(activeLayout);
  // Agents share one pane as its tabs; the shown tab is the transcript the conversation's own
  // docks and body-loading effects follow.
  const selectedSubagentId = shownSubagent(activeLayout);
  const openPreviewSessionId = openPreviewSession(activeLayout);
  // The native page is only presented while this conversation's preview pane is open. Anything
  // else — no pane, another conversation — means the surface has to be released.
  const browserPanelOpen = openPreviewSessionId !== null;
  // Expanding another pane leaves the preview pane with a full-size box it no longer occupies.
  // The native page positions itself from that box, so it has to be withdrawn rather than moved.
  const previewPaneCovered = activeExpandedPane !== null
    && (openPreviewSessionId === null || activeExpandedPane !== previewPaneId(openPreviewSessionId));
  // Only a conversation that actually owns a preview needs its native status polled. Without this
  // the poll runs forever for every task that never opened one, and each tick is a native IPC
  // round trip.
  const openBrowserSessionKey = currentPreviewSessions.join("\n");
  // Joined first so the polling effect only restarts when the set of sessions really changes.
  const openBrowserSessionIds = useMemo(
    () => (openBrowserSessionKey ? openBrowserSessionKey.split("\n") : []),
    [openBrowserSessionKey]
  );
  const gitReviewPanelOpen = paneIsOpen(activeLayout, "review");
  const planPageOpen = paneIsOpen(activeLayout, "plan");
  const terminalPaneOpen = paneIsOpen(activeLayout, "terminal");

  const updateSidebarWidth = useCallback((width: number) => {
    const nextWidth = clampSidebarWidth(width);
    setSidebarWidth(nextWidth);
    try {
      window.localStorage.setItem(SIDEBAR_WIDTH_STORAGE_KEY, String(nextWidth));
    } catch {
      // Resizing should continue to work when persistent browser storage is unavailable.
    }
  }, []);

  useEffect(() => {
    if (!isTauriRuntime()) return;
    let cancelled = false;
    let retryTimer: number | null = null;
    const startHeartbeat = async (mayRetry: boolean) => {
      try {
        await startBrowserRendererMountHeartbeat();
      } catch {
        if (!cancelled && mayRetry) {
          retryTimer = window.setTimeout(() => {
            retryTimer = null;
            void startHeartbeat(false);
          }, 750);
        }
      }
    };
    void startHeartbeat(true);
    return () => {
      cancelled = true;
      if (retryTimer !== null) window.clearTimeout(retryTimer);
      stopBrowserRendererMountHeartbeat();
    };
  }, []);

  useEffect(() => {
    setQuestionEditor((current) => (
      current && current.conversationId !== activeConversationId ? null : current
    ));
  }, [activeConversationId]);

  // Shell command explanations are stored beside the cards; read them once per conversation.
  useEffect(() => {
    if (activeConversationId) void loadToolExplanations(activeConversationId);
  }, [activeConversationId]);

  const updateConversationTurns = useCallback((updater: (current: ConversationTurns) => ConversationTurns) => {
    // Every writer funnels through here, so this is where a round that produced
    // nothing stops being a record. Sweeping it centrally is what keeps the next
    // run from continuing a round the user never saw and inheriting its elapsed
    // time and token counters.
    const next = dropEmptyConversationTurns(updater(conversationTurnsRef.current));
    conversationTurnsRef.current = next;
    setConversationTurns(next);
  }, []);

  const invalidateComposerImages = composerController.invalidateImages;

  useEffect(() => {
    saveConversationTurns(conversationTurns);
  }, [conversationTurns]);

  useEffect(() => {
    if (!document) return;
    configureApplicationAppearance({
      appLanguage: document.globalSettings.appLanguage,
      theme: document.globalSettings.theme,
      appearance: document.globalSettings.appearance
    });
  }, [
    document?.globalSettings.appLanguage,
    document?.globalSettings.theme,
    document?.globalSettings.appearance
  ]);

  // A solid background is a theme's own ground: when the theme on screen changes, one picked
  // from the other theme goes back to following it (`lib/background.ts`). The scheme is read
  // after the effect above has applied the loaded preferences, so startup is not a change.
  const resolvedTheme = useResolvedTheme();
  const followedThemeRef = useRef<ApplicationTheme | null>(null);
  const documentLoaded = Boolean(document);
  useEffect(() => {
    if (!documentLoaded) return;
    const theme = getResolvedTheme();
    const previous = followedThemeRef.current;
    followedThemeRef.current = theme;
    if (previous === null || previous === theme) return;
    documentStore.update((current) => {
      if (!current) return current;
      const appearance = current.globalSettings.appearance;
      const background = backgroundAfterThemeChange(appearance.background);
      if (background === appearance.background) return current;
      return {
        ...current,
        globalSettings: { ...current.globalSettings, appearance: { ...appearance, background } }
      };
    });
  }, [documentLoaded, resolvedTheme]);

  // The host cannot resolve `auto` without an OS language library, so mirror the renderer-resolved
  // application language into the document for built-in tool descriptions.
  useEffect(() => {
    if (!document) return;
    const expectedLanguage = resolveApplicationLanguage(document.globalSettings.appLanguage);
    if (resolvedLanguage !== expectedLanguage) return;
    if (document.globalSettings.resolvedAppLanguage === resolvedLanguage) return;
    documentStore.update((current) => {
      if (!current || current.globalSettings.resolvedAppLanguage === resolvedLanguage) return current;
      return {
        ...current,
        globalSettings: {
          ...current.globalSettings,
          resolvedAppLanguage: resolvedLanguage
        }
      };
    });
  }, [
    document?.globalSettings.appLanguage,
    document?.globalSettings.resolvedAppLanguage,
    resolvedLanguage
  ]);

  useEffect(() => {
    if (!document) return;
    const expectedLanguage = resolveApplicationLanguage(document.globalSettings.appLanguage);
    if (resolvedLanguage !== expectedLanguage) return;
    // Only the untouched default title is localized. A title the user typed never matches the
    // default marker, so it is left alone without needing a separate provenance signal.
    documentStore.update((current) => {
      if (!current) return current;
      let changed = false;
      const workspaces = current.workspaces.map((workspace) => ({
        ...workspace,
        conversations: workspace.conversations.map((conversation) => {
          const title = !conversation.title.trim()
            || conversation.title === "新任务" // i18n-audit-ignore: recognizes the localized default-title marker
            || conversation.title === "New task"
            ? translate(resolvedLanguage, "新任务", "New task")
            : conversation.title;
          if (title === conversation.title) return conversation;
          changed = true;
          return { ...conversation, title };
        })
      }));
      return changed ? { ...current, workspaces } : current;
    });
  }, [document?.globalSettings.appLanguage, document?.tools, platform, resolvedLanguage]);

  const openToolPrompt = useCallback((conversationId: string, prompt: PendingToolPrompt) => {
    setToolPrompts((current) => {
      const queue = current[conversationId] ?? [];
      if (queue.some((pending) => pending.promptId === prompt.promptId)) return current;
      return { ...current, [conversationId]: [...queue, prompt] };
    });
  }, []);

  const closeToolPrompt = useCallback((conversationId: string, promptId: string) => {
    const waiter = manualToolPromptsRef.current.get(promptId);
    if (waiter) {
      // The card went away without this renderer answering it — a cancelled
      // run, a backend timeout. The caller is still awaiting the nonce, so it
      // has to be told rather than left hanging.
      manualToolPromptsRef.current.delete(promptId);
      waiter.reject(new Error(t("工具确认已取消", "The approval was cancelled")));
    }
    setToolPrompts((current) => {
      const queue = current[conversationId];
      if (!queue?.some((pending) => pending.promptId === promptId)) return current;
      const next = queue.filter((pending) => pending.promptId !== promptId);
      const updated = { ...current };
      if (next.length) updated[conversationId] = next;
      else delete updated[conversationId];
      return updated;
    });
  }, [t]);

  /** Draws the card for a call the renderer started itself and resolves with
   * the grant the backend mints once the user answers. */
  const awaitManualToolApproval = useCallback((
    conversationId: string,
    prompt: PendingToolPrompt
  ): Promise<ToolApprovalGrant> => new Promise((resolve, reject) => {
    manualToolPromptsRef.current.set(prompt.promptId, { resolve, reject });
    openToolPrompt(conversationId, prompt);
  }), [openToolPrompt]);

  /** Answers one card. A card raised inside a model run resumes the blocked
   * worker and returns nothing; a card the renderer raised itself hands the
   * minted grant back to whoever is awaiting it. */
  const decideToolPrompt = useCallback((
    conversationId: string,
    promptId: string,
    decision: ToolPromptDecision,
    /** Only a denied plan-exit card carries one: what the model should change. */
    feedback?: string
  ) => {
    const waiter = manualToolPromptsRef.current.get(promptId);
    manualToolPromptsRef.current.delete(promptId);
    const answered = resolveToolPrompt(promptId, decision, feedback);
    if (waiter) {
      answered.then(waiter.resolve, waiter.reject);
    } else {
      // The backend retracts a model card itself with `tool_approval_resolved`,
      // so a rejected submission has no renderer-side compensation left; the
      // catch exists only to keep the rejection from going unhandled.
      void answered.catch(() => {});
    }
    closeToolPrompt(conversationId, promptId);
  }, [closeToolPrompt]);

  /** What each open question card would hand back if a composer message were sent now. */
  const questionDraftsRef = useRef(new Map<string, QuestionResponse>());
  const rememberQuestionDraft = useCallback((promptId: string, response: QuestionResponse) => {
    questionDraftsRef.current.set(promptId, response);
  }, []);
  /** Answers a question card. The card comes down only once the host took the
   * answer; the host's own `tool_approval_resolved` would close it anyway. */
  const answerQuestionPrompt = useCallback(async (
    conversationId: string,
    promptId: string,
    response: QuestionResponse
  ): Promise<boolean> => {
    try {
      await resolveToolPrompt(promptId, "deny", undefined, response);
    } catch {
      return false;
    }
    questionDraftsRef.current.delete(promptId);
    closeToolPrompt(conversationId, promptId);
    return true;
  }, [closeToolPrompt]);

  const openForkRequest = useCallback((request: PendingForkRequest) => {
    setForkRequests((current) => (
      current.some((pending) => pending.forkId === request.forkId)
        ? current
        : [...current, request]
    ));
  }, []);

  const closeForkRequest = useCallback((forkId: string) => {
    setForkRequests((current) => (
      current.some((pending) => pending.forkId === forkId)
        ? current.filter((pending) => pending.forkId !== forkId)
        : current
    ));
  }, []);

  /** Answers one fork card. The card comes down at once; the host publishes
   * `forkResolved` for the outcome, and that event — shared with the
   * auto-approved path — is what starts the child's run. */
  const decideForkRequest = useCallback((forkId: string, approved: boolean) => {
    closeForkRequest(forkId);
    resolveForkRequest(forkId, approved).catch((error) => {
      console.error(t("分叉请求未能提交给宿主", "The fork decision could not be delivered to the host"), error);
    });
  }, [closeForkRequest, t]);

  /** Flush the authoritative snapshot through the store's serial queue; durable writes also await conversation commands. */
  const flushLatestDocument = useCallback(
    async (options: { durable?: boolean } = {}) => {
      await documentStore.flush(options);
      await conversationSync.flush();
    },
    [conversationSync, documentStore]
  );

  /**
   * `snapshotKey` is the conversation's `gitSnapshotKey` for the workspace; `surfaceKey` is what
   * the snapshot is stored under: a project id, or a project id with the member number when the
   * snapshot is of another of the project's workspaces.
   */
  const refreshGitSnapshot = useCallback(async (
    snapshotKey: string,
    surfaceKey: string,
    target: GitTarget,
    handlers?: GitRefreshHandlers
  ): Promise<GitWorkspaceSnapshot | null | undefined> => {
    const projectId = gitSurfaceProjectId(surfaceKey);
    const workspace = documentStore.current()?.workspaces.find((candidate) => candidate.id === projectId);
    if (
      !hasBackendRuntime()
      // Drafts can hold mutation leases despite being absent from workspace conversation lists.
      || gitController.mutationIsActive(gitSnapshotKeyConversation(snapshotKey))
      || workspace?.conversations.some((conversation) => (
        gitController.mutationIsActive(conversation.id)
      ))
    ) return undefined;
    return gitController.refresh(snapshotKey, surfaceKey, target, handlers);
  }, [gitController]);

  const browserSessionDeletionIsActive = useCallback((conversationId: string): boolean => (
    deletingConversationIdsRef.current.has(conversationId)
    || Boolean(documentStore.current()?.workspaces.some((workspace) => (
      deletingWorkspaceIdsRef.current.has(workspace.id)
      && workspace.conversations.some((conversation) => conversation.id === conversationId)
    )))
  ), []);

  /** Every native browser session of one conversation: its primary page plus any extra tabs. */
  const conversationBrowserSessionIds = useCallback((conversationId: string): string[] => {
    const sessionIds = new Set<string>([hostConversationId(conversationId)]);
    for (const sessionId of previewSessionsFor(sidePanesStateRef.current, conversationId)) {
      sessionIds.add(sessionId);
    }
    return [...sessionIds];
  }, [hostConversationId]);

  const issueBrowserIntent = browserController.issueIntent;
  const browserIntentIsCurrent = browserController.intentIsCurrent;

  const commitBrowserSessionClosed = useCallback((sessionId: string) => {
    if (browserController.visibleSession() === sessionId) {
      browserController.setVisibleSession(null);
      browserController.setRuntimeReady(false);
    }
    browserController.updateStatuses((current) => {
      if (!current[sessionId]) return current;
      const next = { ...current };
      delete next[sessionId];
      return next;
    });
    // The session id carries its owning conversation, so a page closed while the user is
    // somewhere else still retires from the right roster. The draft's roster is filed under its
    // placeholder id while its page is named after the id it will materialize as, so a roster
    // that lists the page is its owner too.
    for (const [conversationId, sessions] of Object.entries(sidePanesStateRef.current.previewSessions)) {
      if (
        !previewSessionBelongsToConversation(sessionId, conversationId)
        && !sessions.includes(sessionId)
      ) continue;
      dispatchSidePanes({ type: "forget_preview", conversationId, sessionId });
    }
  }, [browserController, dispatchSidePanes]);

  const openBuiltInBrowser = useCallback(async (
    conversationId: string,
    sessionId: string,
    intentEpoch: number
  ): Promise<boolean> => {
    if (
      browserSessionDeletionIsActive(conversationId)
      || browserController.closeInFlight(sessionId)
      || !browserIntentIsCurrent(sessionId, intentEpoch, "open")
      // The host keys a page by its session id alone, so the draft's page needs no row of its own.
      || !(
        (isDraftConversationId(conversationId) && draftConversationRef.current)
        || documentStore.current()?.workspaces.some((workspace) => (
          workspace.conversations.some((conversation) => conversation.id === conversationId)
        ))
      )
    ) return false;

    const rawOpen = openBrowser(sessionId, null, intentEpoch);
    browserController.trackOpen(sessionId, rawOpen);

    try {
      const status = await rawOpen;
      if (
        browserSessionDeletionIsActive(conversationId)
        || !browserIntentIsCurrent(sessionId, intentEpoch, "open")
      ) return false;
      const stillVisible = (
        activeConversationIdRef.current === conversationId
        && browserController.visibleSession() === sessionId
      );
      if (stillVisible) {
        browserController.updateStatuses((current) => ({ ...current, [sessionId]: status }));
        browserController.setRuntimeReady(status.open);
      }
      return status.open;
    } catch {
      if (
        browserSessionDeletionIsActive(conversationId)
        || !browserIntentIsCurrent(sessionId, intentEpoch, "open")
      ) return false;
      browserController.clearVisibleSessionIf(sessionId);
      if (activeConversationIdRef.current === conversationId) {
        browserController.setRuntimeReady(false);
      }
      return false;
    }
  }, [browserController, browserIntentIsCurrent, browserSessionDeletionIsActive]);

  const hideBuiltInBrowser = useCallback(async () => {
    browserController.setRuntimeReady(false);
    const sessionId = browserController.takeVisibleSession();
    if (!sessionId) return;
    if (browserController.currentIntent(sessionId)?.desired === "closed") {
      // A structured close may retain its tab while native cleanup is pending. The accepted
      // Closed intent already hid (or truthfully reported failure to hide) the exact surface;
      // never reinterpret that generation as Hidden while the user switches trusted UI pages.
      return;
    }
    const intentEpoch = issueBrowserIntent(sessionId, "hidden");
    if (!hasBackendRuntime()) {
      browserController.updateStatuses((current) => {
        const status = current[sessionId];
        return status
          ? { ...current, [sessionId]: { ...status, open: false } }
          : current;
      });
      return;
    }
    try {
      let status: BrowserStatus;
      try {
        status = await performBrowserAction(
          sessionId,
          "hide",
          null,
          intentEpoch
        );
      } catch (firstError) {
        if (!browserIntentIsCurrent(sessionId, intentEpoch, "hidden")) return;
        // Hidden is an idempotent native compensation point. A transient WebView failure after
        // the backend published this exact epoch must be retried with the same epoch.
        try {
          status = await performBrowserAction(
            sessionId,
            "hide",
            null,
            intentEpoch
          );
        } catch {
          throw firstError;
        }
      }
      if (browserIntentIsCurrent(sessionId, intentEpoch, "hidden")) {
        browserController.updateStatuses((current) => ({ ...current, [sessionId]: status }));
      }
    } catch {
      // A failed hide is non-fatal; the next intent reconciles the surface with its own epoch.
    }
  }, [browserController, browserIntentIsCurrent, issueBrowserIntent]);

  const requestBrowserSessionClose = useCallback((sessionId: string): Promise<void> => (
    browserController.dedupClose(sessionId, () => {
      // Declare the Closed intent synchronously before any competing open intent can be issued.
      const previousIntent = browserController.currentIntent(sessionId);
      let intentEpoch = issueBrowserIntent(sessionId, "closed");
      const pendingOpens = browserController.pendingOpens(sessionId);
      return Promise.resolve()
        .then(async () => {
          let disposition: BrowserCloseDisposition = hasBackendRuntime()
            ? await closeBrowserSession(sessionId, intentEpoch)
            : {
                status: "closed" as const,
                intentAccepted: true,
                cleanupComplete: true,
                surfaceHidden: true
              };

          if (
            hasBackendRuntime()
            && !disposition.intentAccepted
            && (disposition.errorCode === "staleIntent" || disposition.errorCode === "intentCollision")
            && browserIntentIsCurrent(sessionId, intentEpoch, "closed")
          ) {
            // A renderer reload may reveal that native authority has already observed a later
            // generation. Retry once with a freshly issued Close; rejected dispositions are
            // explicitly zero-side-effect, so this cannot duplicate import cancellation.
            intentEpoch = issueBrowserIntent(sessionId, "closed");
            disposition = await closeBrowserSession(sessionId, intentEpoch);
          }

          if (!disposition.intentAccepted) {
            if (browserIntentIsCurrent(sessionId, intentEpoch, "closed")) {
              browserController.restoreIntent(sessionId, previousIntent);
            }
            throw new Error(disposition.message ?? t(
              "内置浏览器关闭请求未被接受",
              "The built-in browser close request was not accepted"
            ));
          }

          if (disposition.surfaceHidden && browserIntentIsCurrent(sessionId, intentEpoch, "closed")) {
            browserController.setRuntimeReady(false);
          }

          if (!disposition.cleanupComplete) {
            throw new Error(disposition.message ?? t(
              "内置浏览器关闭清理尚未完成，请重试",
              "The built-in browser cleanup is not complete; please retry"
            ));
          }

          if (pendingOpens.length > 0) {
            await Promise.allSettled(pendingOpens);
            if (hasBackendRuntime()) {
              // An older open may have completed after the first close. The second
              // close is the compensating fence that makes this intent terminal.
              disposition = await closeBrowserSession(sessionId, intentEpoch);
              if (!disposition.intentAccepted || !disposition.cleanupComplete) {
                throw new Error(disposition.message ?? t(
                  "内置浏览器关闭清理尚未完成，请重试",
                  "The built-in browser cleanup is not complete; please retry"
                ));
              }
            }
          }
          if (browserIntentIsCurrent(sessionId, intentEpoch, "closed")) {
            commitBrowserSessionClosed(sessionId);
          }
        });
    })
  ), [browserController, browserIntentIsCurrent, commitBrowserSessionClosed, issueBrowserIntent, t]);

  /** Closes every native browser session a conversation owns, including tabs already removed. */
  const requestConversationBrowserClose = useCallback((conversationId: string): Promise<void> => (
    Promise.all(conversationBrowserSessionIds(conversationId).map(requestBrowserSessionClose))
      .then(() => undefined)
  ), [conversationBrowserSessionIds, requestBrowserSessionClose]);

  const openPane = useCallback((pane: SidePaneId) => {
    const conversationId = activeConversationIdRef.current;
    if (!conversationId) return;
    // A terminal pane with no tabs has nothing to show — nothing was ever opened, or the last
    // tab leaving is what closed it — so asking for the pane asks for a terminal to put in it.
    if (pane === "terminal") {
      dispatchTerminalTabs({
        type: "ensure",
        conversationId: hostConversationId(conversationId),
        launch: defaultTerminalLaunchRef.current
      });
    }
    dispatchSidePanes({ type: "open", conversationId, pane });
  }, [dispatchSidePanes, dispatchTerminalTabs, hostConversationId]);

  /** Closing a preview pane releases the native surface; the session itself keeps running. */
  const closePane = useCallback((pane: SidePaneId) => {
    const conversationId = activeConversationIdRef.current;
    if (!conversationId) return;
    dispatchSidePanes({ type: "close", conversationId, pane });
    if (paneKind(pane) === "preview") void hideBuiltInBrowser();
  }, [dispatchSidePanes, hideBuiltInBrowser]);

  /** Reads the ref, not the render's layout, so two toggles in one tick observe each other. */
  const togglePane = useCallback((pane: SidePaneId) => {
    const conversationId = activeConversationIdRef.current;
    if (!conversationId) return;
    if (paneIsOpen(sidePaneLayoutFor(sidePanesStateRef.current, conversationId), pane)) closePane(pane);
    else openPane(pane);
  }, [closePane, openPane]);

  /** Brings the tasks pane forward on one of its tabs; the conversation's history is the other one. */
  const showTasksTab = useCallback((tab: TasksPaneTab) => {
    const conversationId = activeConversationIdRef.current;
    if (!conversationId) return;
    dispatchSidePanes({ type: "show_tasks_tab", conversationId, tab });
  }, [dispatchSidePanes]);

  /** A tab's menu row: closes the pane when it is that tab on show, and otherwise shows that tab. */
  const toggleTasksTab = useCallback((tab: TasksPaneTab) => {
    const conversationId = activeConversationIdRef.current;
    if (!conversationId) return;
    if (shownTasksTab(sidePaneLayoutFor(sidePanesStateRef.current, conversationId)) === tab) closePane("tasks");
    else showTasksTab(tab);
  }, [closePane, showTasksTab]);

  /**
   * Height the native page may occupy inside the pane.
   *
   * The host sizes the page from the pane rectangle alone and knows nothing about renderer chrome
   * below it, so the preview's log drawer would be painted over by a full-height page. Subtracting
   * what the drawer reserved makes the page shrink rather than sit behind the drawer. Nothing
   * is reserved at the top any more: the browser's toolbar is the pane's own title bar, outside
   * this rectangle.
   */
  const previewReservedBottomRef = useRef<Record<string, number>>({});
  const lastPreviewBoundsRef = useRef<Record<string, SidePaneBounds>>({});
  const previewPageHeight = useCallback((sessionId: string, height: number) => (
    Math.max(1, height - (previewReservedBottomRef.current[sessionId] ?? 0))
  ), []);
  /**
   * The page rectangle the host is given for a measured pane body.
   *
   * Spelled out field by field: a measured `DOMRect` keeps its coordinates on the prototype, so
   * spreading one into the wire payload would silently drop every one of them. The page only
   * reaches the pane's rounded bottom when the log drawer is not taking the bottom from it.
   */
  const previewPageBounds = useCallback((sessionId: string, bounds: SidePaneBounds) => {
    const reserved = previewReservedBottomRef.current[sessionId] ?? 0;
    return {
      x: bounds.x,
      y: bounds.y,
      width: bounds.width,
      height: previewPageHeight(sessionId, bounds.height),
      visible: true,
      bottomCornerRadius: reserved > 0 ? 0 : bounds.bottomCornerRadius
    };
  }, [previewPageHeight]);
  /**
   * One bounds request in flight per page, and only the newest rectangle waiting behind it.
   *
   * The page follows the pane while a divider is dragged, which reports a rectangle every frame.
   * Sent as they come, requests that each take longer than a frame queue up behind one another,
   * and the page falls further behind the pane the longer the drag goes on; holding only the
   * newest bounds the lag to a single round trip.
   */
  const previewBoundsQueueRef = useRef<Record<string, { busy: boolean; next: (() => Promise<void>) | null }>>({});
  const sendLatestPreviewBounds = useCallback((sessionId: string, send: () => Promise<void>) => {
    const queues = previewBoundsQueueRef.current;
    const queue = queues[sessionId] ?? { busy: false, next: null };
    queues[sessionId] = queue;
    if (queue.busy) {
      queue.next = send;
      return;
    }
    queue.busy = true;
    const run = (task: () => Promise<void>) => {
      void task().catch(() => undefined).finally(() => {
        const next = queue.next;
        queue.next = null;
        if (next) run(next);
        else queue.busy = false;
      });
    };
    run(send);
  }, []);
  /**
   * The page each conversation last had on screen, so bringing the pane back brings back the page
   * the user left rather than whichever happens to be first in the strip.
   */
  const lastPreviewPageRef = useRef<Record<string, string>>({});
  /** Read by callbacks that must see the statuses as they are now, not as their closure saw them. */
  const browserStatusesRef = useRef(browserStatuses);
  browserStatusesRef.current = browserStatuses;
  /**
   * The workspace the composer's chip has selected, for the one preview action that has no
   * workspace of its own to go by: showing the pane when there is no page yet. Assigned where the
   * selection is resolved, further down the render.
   */
  const activeWorkspaceMemberRef = useRef(1);
  /**
   * The conversation as the preview commands address it (see `activePreviewOwnerTarget`), for the
   * callbacks declared before it is resolved.
   */
  const previewOwnerTargetRef = useRef<PreviewTarget | null>(null);

  /**
   * Publishes a preview page's rectangle *before* its native page is created.
   *
   * `browser_open` runs behind the host's lifecycle lock, and the page's own ResizeObserver
   * cannot fire until React has committed and that await has returned. Without this the first
   * native layout falls through to the host's compatibility fallback — a 560px panel pinned to
   * the right edge — and the page visibly flies in from the corner on every open. The host
   * accepts and retains geometry for a session that has no page yet, which is what makes the
   * barrier possible at all.
   */
  const publishPreviewBounds = useCallback(async (sessionId: string, epoch: number) => {
    if (!isTauriRuntime()) return;
    const domId = sidePaneDomId(previewPaneId(sessionId));
    for (let attempt = 0; attempt < 4; attempt += 1) {
      await new Promise<void>((resolve) => window.requestAnimationFrame(() => resolve()));
      const rect = window.document
        .getElementById(domId)
        ?.querySelector<HTMLElement>(".side-pane__body")
        ?.getBoundingClientRect();
      // A page that has not been laid out yet measures zero, which the host would reject.
      if (!rect || rect.width < 1 || rect.height < 1) continue;
      const section = window.document.getElementById(domId);
      const bounds: SidePaneBounds = {
        x: rect.x,
        y: rect.y,
        width: rect.width,
        height: rect.height,
        bottomCornerRadius: section ? innerBottomRadius(section) : 0
      };
      lastPreviewBoundsRef.current = { ...lastPreviewBoundsRef.current, [sessionId]: bounds };
      await setBrowserPanelBounds(sessionId, previewPageBounds(sessionId, bounds), epoch)
        .catch(() => undefined);
      return;
    }
  }, [previewPageBounds]);

  /**
   * Presents this conversation's preview page.
   *
   * There is exactly one, and the host keys it by the conversation id, so `sessionId` only ever
   * names the page that already exists; omitting it opens that same page. The tab roster the Agent
   * used to publish is gone with the tools that published it. The draft's page is keyed by the id
   * it will materialize as, which is what lets the page stay open across its first send.
   */
  const openBrowserTab = useCallback(async (sessionId?: string) => {
    if (!activeConversationId) return;
    const conversationId = activeConversationId;
    const targetSessionId = sessionId ?? hostConversationId(conversationId);
    if (
      browserSessionDeletionIsActive(conversationId)
      || browserController.closeInFlight(targetSessionId)
    ) return;
    if (
      browserController.visibleSession()
      && browserController.visibleSession() !== targetSessionId
    ) {
      // Two native pages share one panel rectangle, so the outgoing page must be hidden before
      // the incoming one is shown; otherwise the old surface stays on top of the new one.
      await hideBuiltInBrowser();
      if (activeConversationIdRef.current !== conversationId) return;
    }
    const intentEpoch = issueBrowserIntent(targetSessionId, "open");
    browserController.setVisibleSession(targetSessionId);
    lastPreviewPageRef.current = { ...lastPreviewPageRef.current, [conversationId]: targetSessionId };
    dispatchSidePanes({ type: "open", conversationId, pane: previewPaneId(targetSessionId) });
    if (hasBackendRuntime()) {
      await publishPreviewBounds(targetSessionId, intentEpoch);
      if (
        activeConversationIdRef.current !== conversationId
        || !browserIntentIsCurrent(targetSessionId, intentEpoch, "open")
      ) {
        return;
      }
      const opened = await openBuiltInBrowser(conversationId, targetSessionId, intentEpoch);
      if (
        activeConversationIdRef.current !== conversationId
        || !browserIntentIsCurrent(targetSessionId, intentEpoch, "open")
      ) {
        return;
      }
      const visible = browserController.visibleSession();
      if (visible === null && !opened) {
        // The host refused the open, and the refusal took the page off the visible slot. The
        // page itself is still this conversation's, so only the pane goes: left open it drew
        // nothing, showed the button pressed, and took two more clicks to open.
        dispatchSidePanes({ type: "close", conversationId, pane: previewPaneId(targetSessionId) });
        return;
      }
      if (visible !== targetSessionId) return;
      if (!opened) {
        dispatchSidePanes({ type: "forget_preview", conversationId, sessionId: targetSessionId });
      }
    } else {
      if (!browserIntentIsCurrent(targetSessionId, intentEpoch, "open")) return;
      browserController.updateStatuses((current) => ({
        ...current,
        [targetSessionId]: current[targetSessionId]
          ? { ...current[targetSessionId], hasPage: true, open: true }
          : {
              hasPage: true,
              open: true,
              loading: false,
              url: "about:blank",
              title: t("浏览器预览", "Browser preview"),
              canGoBack: false,
              canGoForward: false,
              zoom: 1,
              viewport: { width: 560, height: 720 }
            }
      }));
      browserController.setRuntimeReady(true);
    }
  }, [
    activeConversationId,
    browserController,
    browserIntentIsCurrent,
    browserSessionDeletionIsActive,
    dispatchSidePanes,
    hideBuiltInBrowser,
    hostConversationId,
    issueBrowserIntent,
    openBuiltInBrowser,
    publishPreviewBounds,
    t
  ]);

  /**
   * Registers this conversation's preview session after a preview tool that owns the page.
   *
   * The host mints the native page on `preview_start` and on every page tool, and with no manual
   * "open a browser" affordance left, a call that produced no row would leave a live Chromium page
   * the user could neither see nor close. There is exactly one page per conversation and it is
   * keyed by the conversation id, so nothing has to be parsed out of the result. A session the
   * host does not actually have is self-correcting: status polling reports `hasPage: false` and the
   * row never renders.
   *
   * The model still never gets to steal the surface: the session is registered without becoming
   * the shown page.
   */
  const registerPreviewSession = useCallback((conversationId: string) => {
    dispatchSidePanes({ type: "register_preview", conversationId, sessionId: conversationId });
  }, [dispatchSidePanes]);

  /**
   * Shows one agent's read-only transcript as a tab of the subagent pane. The conversation stays
   * on screen beside it, and the agents opened before it stay a tab away.
   */
  const openSubagentPanel = useCallback((subagentId: string) => {
    const conversationId = activeConversationIdRef.current;
    if (!conversationId) return;
    setEditor(null);
    dispatchSidePanes({ type: "open_subagent", conversationId, subagentId });
  }, [dispatchSidePanes]);

  /** Leaves the focused pane, which is what the `panel.close` shortcut addresses. */
  const closeLastPane = useCallback(() => {
    const conversationId = activeConversationIdRef.current;
    if (!conversationId) return;
    const pane = focusedPane(sidePaneLayoutFor(sidePanesStateRef.current, conversationId));
    if (pane) closePane(pane);
  }, [closePane]);

  const openConversationSettings = useCallback(() => {
    openPane("settings");
  }, [openPane]);

  /**
   * Keeps the one native surface pointed at the pane that is asking for it. Switching
   * conversations releases a page whose pane is not on screen here, and re-presents this
   * conversation's own preview pane, whose session the host is not currently showing.
   * Conversation settings is a pane in the same column now, so it takes a track beside
   * the preview instead of covering it: only `previewPaneCovered` still withdraws the page.
   * Modal surfaces do not appear here at all — a dialog's backdrop registers itself as a
   * floating surface, and the host cuts the page out under it.
   */
  useEffect(() => {
    const wanted = previewPaneCovered ? null : openPreviewSessionId;
    const presented = browserController.visibleSession();
    if (presented && presented !== wanted) void hideBuiltInBrowser();
    if (wanted && presented !== wanted && !browserController.closeInFlight(wanted)) {
      void openBrowserTab(wanted);
    }
  }, [
    activeConversationId,
    browserController,
    hideBuiltInBrowser,
    openBrowserTab,
    openPreviewSessionId,
    previewPaneCovered
  ]);

  useEffect(() => {
    if (!activeConversationId || !hasBackendRuntime() || !openBrowserSessionIds.length) return;
    const sessionIds = openBrowserSessionIds;
    let cancelled = false;
    let timer: number | null = null;
    const pollSession = async (sessionId: string) => {
      const status = await getBrowserStatus(sessionId);
      if (cancelled) return;
      if (browserController.currentIntent(sessionId)?.desired === "closed") return;
      browserController.updateStatuses((current) => ({ ...current, [sessionId]: status }));
      if (
        isTauriRuntime()
        && browserPanelOpen
        && browserRuntimeReady
        // A page withdrawn because another pane was expanded over it is still the pane's page.
        // Only a page that stopped being shown on its own means the user closed the preview.
        && !previewPaneCovered
        && browserController.visibleSession() === sessionId
        && !status.open
      ) {
        const conversationId = activeConversationIdRef.current;
        if (conversationId) {
          dispatchSidePanes({ type: "close", conversationId, pane: previewPaneId(sessionId) });
        }
        browserController.setRuntimeReady(false);
      }
    };
    const poll = async () => {
      // Every open session is polled so each preview row keeps showing its own page's title, not
      // just the presented one's.
      for (const sessionId of sessionIds) {
        if (cancelled) return;
        try {
          await pollSession(sessionId);
        } catch {
          // A transient IPC failure must not tear down a page or hide its card entry.
        }
      }
      if (!cancelled) timer = window.setTimeout(() => void poll(), 700);
    };
    void poll();
    return () => {
      cancelled = true;
      if (timer !== null) window.clearTimeout(timer);
    };
  }, [
    activeConversationId,
    browserPanelOpen,
    browserRuntimeReady,
    dispatchSidePanes,
    openBrowserSessionIds,
    previewPaneCovered
  ]);

  const updateTerminalSession = useCallback((next: TerminalSessionState) => {
    if (browserSessionDeletionIsActive(next.conversationId)) return;
    terminalController.update(next);
  }, [browserSessionDeletionIsActive, terminalController]);

  const beginTerminalCommand = useCallback((conversationId: string, terminalId: string): boolean => {
    const session = terminalController.current()[terminalSessionKey(conversationId, terminalId)];
    if (!session) return false;
    const workspace = documentStore.current()?.workspaces.find((candidate) => (
      ownerWorksIn(session.conversationId, candidate)
    ));
    if (
      !workspace
      || browserSessionDeletionIsActive(session.conversationId)
      || workspace.conversations.some((conversation) => (
        gitController.mutationIsActive(conversation.id)
      ))
      || (draftWorksIn(workspace) && gitController.mutationIsActive(DRAFT_CONVERSATION_ID))
    ) return false;
    return terminalController.markCommandStarted(conversationId, terminalId);
  }, [browserSessionDeletionIsActive, draftWorksIn, gitController, ownerWorksIn, terminalController]);

  const requestTerminalSessionClose = useCallback((
    conversationId: string,
    terminalId: string
  ): Promise<void> => (
    terminalController.requestClose(conversationId, terminalId)
  ), [terminalController]);

  const destroyTerminalSession = useCallback(async (
    conversationId: string,
    terminalId: string
  ): Promise<boolean> => {
    try {
      await requestTerminalSessionClose(conversationId, terminalId);
      return true;
    } catch {
      return false;
    }
  }, [requestTerminalSessionClose]);

  /**
   * Ends one terminal and takes its tab with it.
   *
   * A tab is its shell's only place on screen, so the two leave together, and the pane goes with
   * the last of them the way a browser window closes with its last page. The pane's own × is the
   * opposite act: it puts the window away and leaves every shell running behind it.
   *
   * The mounted panel owns the kill so input stays gated and the body stays veiled until the
   * host confirms it; only a tab with no panel — one belonging to another conversation — goes
   * straight to the session store.
   */
  const closeTerminalTab = useCallback((conversationId: string, terminalId: string) => {
    // The pane belongs to the conversation on screen, the terminal to its host identity; for the
    // draft the two differ.
    const ownerId = hostConversationId(conversationId);
    const wasLast = terminalStripIds(terminalTabsFor(terminalTabsStateRef.current, ownerId)).length <= 1;
    // The read-only page has no shell behind it: closing it only takes the page away, and the
    // command it showed runs on.
    const handle = terminalId === READ_ONLY_TERMINAL_TAB_ID ? null : terminalPanelHandlesRef.current.get(
      terminalSessionKey(ownerId, terminalId)
    );
    const ended = terminalId === READ_ONLY_TERMINAL_TAB_ID
      ? undefined
      : handle
        ? handle.close()
        : requestTerminalSessionClose(ownerId, terminalId).catch(() => undefined);
    void Promise.resolve(ended).then(() => {
      dispatchTerminalTabs({ type: "close", conversationId: ownerId, terminalId });
      if (!wasLast) return;
      dispatchSidePanes({ type: "close", conversationId, pane: "terminal" });
      if (activeConversationIdRef.current === conversationId) {
        composerTextareaRef.current?.focus({ preventScroll: true });
      }
    });
  }, [dispatchSidePanes, dispatchTerminalTabs, hostConversationId, requestTerminalSessionClose]);

  /**
   * Opens the pane behind a task row. Subagents and workflows show a transcript instead and never
   * reach here.
   *
   * A terminal row has no pane of its own: the conversation's single PTY lives in the terminal
   * pane, so its row opens that pane. A command the model ran has none either: its output is the
   * terminal pane's read-only page, which the row points at it.
   */
  const openTaskItemPage = useCallback(async (item: TaskItem) => {
    const conversationId = activeConversationIdRef.current;
    if (!conversationId) return;
    if (item.kind === "terminal") {
      openPane("terminal");
      return;
    }
    if (item.kind === "shell") {
      // Shown first, so that opening the pane finds a page to show and starts no shell for it.
      dispatchTerminalTabs({
        type: "show_read_only",
        conversationId: hostConversationId(conversationId),
        shellTaskId: item.shell.shellTaskId
      });
      openPane("terminal");
      return;
    }
    if (item.kind === "plan") {
      openPane("plan");
      return;
    }
    if (item.kind === "fork") {
      // Only an approved row is openable, and the child it points at may since
      // have been deleted: then the row stays a record and opens nothing.
      const childId = item.decision.childConversationId;
      if (!childId) return;
      const exists = documentStore.current()?.workspaces.some((workspace) => (
        workspace.id === item.decision.workspaceId
        && workspace.conversations.some((candidate) => candidate.id === childId)
      ));
      if (!exists) return;
      setActiveWorkspaceId(item.decision.workspaceId);
      setActiveConversationId(childId);
      return;
    }
    if (item.kind === "browser") await openBrowserTab(item.sessionId);
    // A dev server has no pane of its own either: what there is to look at is the
    // page it serves, so the row opens the conversation's preview. When that page
    // is showing something else, the pane's own start page lists this server with
    // the button that points it here.
    if (item.kind === "preview") await openBrowserTab();
  }, [
    dispatchTerminalTabs, documentStore, hostConversationId, openBrowserTab, openPane,
    setActiveConversationId, setActiveWorkspaceId
  ]);

  const openGlobalSettings = useCallback((view: SettingsView) => {
    setGlobalSettingsView(view);
  }, []);

  /**
   * Closes one preview session and its native page.
   *
   * With no tab strip there is no × on a page any more, so this is reached from the preview row's
   * stop control in the task bar. The automation guard survives the move: only the primary
   * session is the surface Agent tools drive, so only it has to wait for automation to stop.
   */
  const closePreviewSession = useCallback(async (conversationId: string, sessionId: string) => {
    if (
      isPrimaryPreviewSession(sessionId, conversationId)
      && (
        browserAutomationStoppingIdsRef.current.has(conversationId)
        || Boolean(browserAutomationToolForRun(modelRunController.current()[conversationId]))
      )
    ) {
      return;
    }
    try {
      await requestBrowserSessionClose(sessionId);
    } catch {
      return;
    }
    browserController.clearVisibleSessionIf(sessionId);
    if (activeConversationIdRef.current === conversationId) {
      browserController.setRuntimeReady(false);
    }
    browserController.updateStatuses((current) => {
      const next = { ...current };
      delete next[sessionId];
      return next;
    });
    // The reducer returns the message area to the conversation when the closed session was the
    // one on screen — there is no neighbouring tab left to fall back along.
    dispatchSidePanes({ type: "forget_preview", conversationId, sessionId });
  }, [browserController, dispatchSidePanes, modelRunController, requestBrowserSessionClose]);

  /**
   * Opens a page for the conversation's workspace `member` (1-based) and shows it.
   *
   * A page with nothing loaded yet is that workspace's start page, which is exactly what a new page
   * would be, so an idle one is brought forward rather than joined by a second copy of itself. The
   * first page a conversation gets is its own page — the one the model's preview tools drive — and
   * every further one is a tab of its own, with a Chromium profile of its own.
   */
  const openPreviewPage = useCallback((member: number) => {
    const conversationId = activeConversationIdRef.current;
    if (!conversationId) return;
    const state = sidePanesStateRef.current;
    const roster = previewSessionsFor(state, conversationId);
    const statuses = browserStatusesRef.current;
    const idle = roster.find((sessionId) => {
      if (previewWorkspaceOf(state, sessionId) !== member) return false;
      const url = statuses[sessionId]?.url;
      return !url || url === "about:blank";
    });
    if (idle) {
      void openBrowserTab(idle);
      return;
    }
    const token = typeof crypto !== "undefined" && "randomUUID" in crypto
      ? crypto.randomUUID()
      : `${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`;
    const sessionId = newPreviewPageSessionId(roster, hostConversationId(conversationId), `tab_${token}`);
    dispatchSidePanes({ type: "register_preview", conversationId, sessionId, workspace: member });
    const owner = previewOwnerTargetRef.current;
    if (!owner || !hasBackendRuntime()) {
      void openBrowserTab(sessionId);
      return;
    }
    // The page's network is its workspace's machine, and it has to be that before the page
    // exists: its first request must already leave from there. A binding that fails leaves the
    // page on this computer, and the pane says why its machine cannot be reached.
    void setBrowserPageNetwork(sessionId, member > 1 ? { ...owner, workspace: member } : owner)
      .catch(() => undefined)
      .then(() => {
        if (activeConversationIdRef.current === conversationId) void openBrowserTab(sessionId);
      });
  }, [dispatchSidePanes, hostConversationId, openBrowserTab]);

  /**
   * Brings the preview pane up on the page the user last had there. With no page at all, the pane
   * opens on the start page of the workspace the composer's chip has selected — the one place the
   * conversation has already said it is working in.
   */
  const showPreviewPanel = useCallback(() => {
    const conversationId = activeConversationIdRef.current;
    if (!conversationId) return;
    const roster = previewSessionsFor(sidePanesStateRef.current, conversationId);
    if (!roster.length) {
      openPreviewPage(activeWorkspaceMemberRef.current);
      return;
    }
    const last = lastPreviewPageRef.current[conversationId];
    void openBrowserTab(last && roster.includes(last) ? last : roster[0]);
  }, [openBrowserTab, openPreviewPage]);

  /**
   * Closes one page from its tab. The neighbour takes its place first — to the right, as a
   * browser's tabs do, and to the left off the end — so the pane moves to another page instead of
   * blinking shut and open again; the last page closing is what takes the pane with it.
   */
  const closePreviewPage = useCallback(async (sessionId: string) => {
    const conversationId = activeConversationIdRef.current;
    if (!conversationId) return;
    const state = sidePanesStateRef.current;
    const roster = previewSessionsFor(state, conversationId);
    const index = roster.indexOf(sessionId);
    const remaining = roster.filter((candidate) => candidate !== sessionId);
    if (openPreviewSession(sidePaneLayoutFor(state, conversationId)) === sessionId && remaining.length) {
      await openBrowserTab(remaining[Math.min(Math.max(index, 0), remaining.length - 1)]);
    }
    await closePreviewSession(conversationId, sessionId);
  }, [closePreviewSession, openBrowserTab]);


  /** Publish synchronously before awaiting persistence. Durable writes also await conversation commands so user messages reach disk before a run. */
  const persistDocumentImmediately = useCallback(
    async (next: AppDocument, options: { durable?: boolean } = {}) => {
      const previous = documentStore.current();
      const published = documentStore.publish(next, options);
      conversationSync.syncDocument(previous, next);
      await published;
      // Immediate persistence includes conversation commands so callers can safely read the host's committed document.
      await conversationSync.flush();
    },
    [conversationSync, documentStore]
  );


  useEffect(() => {
    let cancelled = false;
    loadDocument()
      .then(async (loaded) => {
        if (cancelled) return;
        documentStore.load(loaded);
        // In the background: the first draft opens on the seed model and moves to
        // the CLI's Opus once it answers, unless the user picked a model first.
        if (startedOnFreshInstall()) {
          void setUpClaudeAgentOnFirstLaunch(loaded, (updater) => documentStore.update(updater));
        }
        const firstWorkspace = loaded.workspaces[0];
        const firstConversationId = firstWorkspace?.conversations[0]?.id ?? null;
        if (firstConversationId) {
          setActiveWorkspaceId(firstWorkspace?.id ?? null);
          setActiveConversationId(firstConversationId);
        } else {
          // A fresh installation opens a draft without selecting a workspace; its first message targets the temporary workspace.
          setActiveWorkspaceId(null);
          openDraftConversationRef.current();
        }
        // Capabilities mirror disk rather than document state, so rescan at startup for external changes.
        try {
          await rescanCapabilities();
        } catch {
          // Retain the document snapshot when the catalog is temporarily unreadable.
        }
      })
      .catch((error) => !cancelled && setLoadError(error instanceof Error ? error.message : String(error)));
    return () => { cancelled = true; };
  }, []);

  // `taskSettled` means an idle conversation has a deliverable task result. Wake only the active
  // conversation immediately; record inactive ones until they become active.
  const pendingWakeConversationsRef = useRef(new Set<string>());
  const [wakeSignal, setWakeSignal] = useState(0);
  // Children the host just forked and whose first run this renderer still has to start.
  const pendingForkStartsRef = useRef<{ workspaceId: string; conversationId: string }[]>([]);
  const [forkSignal, setForkSignal] = useState(0);
  const forkStartsAttemptedRef = useRef(new Set<string>());
  // Handoff continuations, child → source. When the source is still the
  // conversation on screen once the child is loaded, the page follows it there.
  const handoffJumpsRef = useRef(new Map<string, string>());
  const [forkStartError, setForkStartError] = useState<string | null>(null);
  const [forkRetrySignal, setForkRetrySignal] = useState(0);

  // Surface background document-write failures immediately; recovery returns only error status to saved.
  useEffect(() => onAppPushEvent((event) => {
    if (event.type === "documentWriteFailure") {
      documentStore.reportBackendSaveResult("failure");
      return;
    }
    // Record wake notifications here; a dedicated effect performs the wake with active-conversation guards.
    if (event.type === "taskSettled") {
      pendingWakeConversationsRef.current.add(event.conversationId);
      setWakeSignal((current) => current + 1);
      return;
    }
    // Background task approvals use push delivery because no running stream can carry their card; they outlive turn boundaries.
    if (event.type === "toolApprovalRequested") {
      const { type: _type, conversationId, ...prompt } = event;
      openToolPrompt(conversationId, prompt);
      return;
    }
    if (event.type === "toolApprovalResolved") {
      closeToolPrompt(event.conversationId, event.promptId);
      return;
    }
    if (event.type === "conversationTitleChanged") {
      // The host wrote it (placeholder, then the local helper model's title);
      // mirrored without writing back, like the security level above.
      documentStore.update((current) => current ? {
        ...current,
        workspaces: current.workspaces.map((workspace) => ({
          ...workspace,
          conversations: workspace.conversations.map((conversation) => (
            conversation.id === event.conversationId && conversation.title !== event.title
              ? { ...conversation, title: event.title }
              : conversation
          ))
        }))
      } : current);
      return;
    }
    if (event.type === "conversationPlanUpdated") {
      planPushCountRef.current += 1;
      setPlans((current) => ({ ...current, [event.conversationId]: event.plan }));
      return;
    }
    if (event.type === "conversationPlanModeChanged") {
      // The host already wrote it: an approved plan turned plan mode off. It is
      // written back all the same, because a settings write still waiting in the
      // commit queue was built before this and carries the old value, and the
      // latest write for a conversation is the one that lands.
      const current = documentStore.current();
      const workspace = current?.workspaces.find((candidate) => (
        candidate.conversations.some((conversation) => conversation.id === event.conversationId)
      ));
      const base = workspace?.conversations.find((conversation) => conversation.id === event.conversationId);
      if (!workspace || !base || Boolean(base.settings.planModeEnabled) === event.enabled) return;
      const next = { ...base, settings: { ...base.settings, planModeEnabled: event.enabled } };
      documentStore.update((document) => document ? {
        ...document,
        workspaces: document.workspaces.map((candidate) => candidate.id !== workspace.id ? candidate : {
          ...candidate,
          conversations: candidate.conversations.map((conversation) => (
            conversation.id === event.conversationId ? next : conversation
          ))
        })
      } : document);
      conversationSync.changed(workspace.id, base, next);
      return;
    }
    // Fork cards are global: the tool call that raised one has already returned, so the
    // card belongs to no run and the tray draws it whichever conversation is open.
    if (event.type === "forkRequested") {
      const { type: _type, ...request } = event;
      openForkRequest(request);
      return;
    }
    if (event.type === "forkResolved") {
      closeForkRequest(event.forkId);
      // A retraction carries no decision; an answered card carries the row the task bar shows.
      if (event.decision) appendForkDecision(event.decision);
      // The child's first run is started from an effect rather than here, so it
      // waits for run adoption exactly as a wake does and cannot race it.
      if (event.childConversationId) {
        pendingForkStartsRef.current.push({
          workspaceId: event.workspaceId,
          conversationId: event.childConversationId
        });
        setForkSignal((current) => current + 1);
      }
      return;
    }
    // The model handed its conversation off and the host committed the
    // continuation, armed on its opening message. It is not a fork, but its
    // first run starts the way a host-made fork's does.
    if (event.type === "conversationHandedOff") {
      handoffJumpsRef.current.set(event.childConversationId, event.sourceConversationId);
      pendingForkStartsRef.current.push({
        workspaceId: event.workspaceId,
        conversationId: event.childConversationId
      });
      setForkSignal((current) => current + 1);
      return;
    }
    // Shell lifecycle events are model behavior, not document-save results.
    if (event.type === "shellTaskStarted") {
      // A host that restarted mints ids from one again, so a start is what
      // clears an old tombstone for the same id.
      evictedShellTaskIdsRef.current.delete(event.task.shellTaskId);
      setShellTasks((current) => {
        const others = current.filter(
          (task) => task.shellTaskId !== event.task.shellTaskId
        );
        return [...others, event.task];
      });
      return;
    }
    if (event.type === "shellTaskEnded") {
      // Replaced, never removed: the row carries how the command went, which is
      // the answer the user is waiting for, and it belongs in the finish list
      // rather than gone. Rows the host has already evicted simply stop arriving.
      setShellTasks((current) => {
        const others = current.filter(
          (task) => task.shellTaskId !== event.task.shellTaskId
        );
        return [...others, event.task];
      });
      return;
    }
    if (event.type === "shellTaskEvicted") {
      // The host let go of the row and its transcript together. A row kept here
      // would still open, onto a pane that can no longer read anything, so it
      // goes — and a pane already showing it goes with it. The id is remembered
      // so a task list requested before the eviction cannot bring the row back.
      evictedShellTaskIdsRef.current.add(event.shellTaskId);
      setShellTasks((current) => current.filter(
        (task) => task.shellTaskId !== event.shellTaskId
      ));
      // The read-only page closes like any other tab, and the pane goes with its last tab.
      const terminals = terminalTabsFor(terminalTabsStateRef.current, event.conversationId);
      if (terminals.readOnly === event.shellTaskId) {
        dispatchTerminalTabs({
          type: "forget_read_only",
          conversationId: event.conversationId,
          shellTaskId: event.shellTaskId
        });
        if (terminalStripIds(terminals).length <= 1) {
          dispatchSidePanes({ type: "close", conversationId: event.conversationId, pane: "terminal" });
        }
      }
      return;
    }
    if (event.type === "toolContextsQuarantined") {
      // The host saved exact local-only markers in place of these cards. The
      // renderer still holds the originals, so install the committed markers;
      // deleting them would erase the user-visible evidence of quarantine.
      documentStore.update((current) =>
        current ? applyQuarantinedContextReplacements(current, event.contexts) : current
      );
      return;
    }
    if (event.type === "documentWriteRecovered") {
      documentStore.reportBackendSaveResult("success");
    }
  }), [documentStore, conversationSync, openToolPrompt, closeToolPrompt, openForkRequest, closeForkRequest, appendForkDecision, dispatchSidePanes, dispatchTerminalTabs]);

  /** Merge persisted and draft branches into one active workspace/conversation pair; only document writes, sending, and real-workspace panels need to distinguish drafts. */
  const draftActive = isDraftConversationId(activeConversationId) && draftConversation !== null;
  const draftConversationView = useMemo(
    () => (draftConversation ? draftAsConversation(draftConversation, t("新任务", "New task")) : null),
    [draftConversation, t]
  );
  const persisted = useMemo(
    () => findConversation(document, activeWorkspaceId, activeConversationId),
    [document, activeWorkspaceId, activeConversationId]
  );
  const activeConversation = draftActive ? draftConversationView : persisted.conversation;
  /** See `hostConversationId`: what the active conversation's terminals and preview page are keyed by. */
  const activeHostConversationId = draftActive
    ? draftConversation?.materializesAs ?? null
    : activeConversation?.id ?? null;
  const activeWorkspace = draftActive
    ? document?.workspaces.find((workspace) => workspace.id === draftConversation?.workspaceId) ?? null
    : persisted.workspace;
  /**
   * Whether the composer belongs to a task that has never been sent, which is where the cat
   * loafs. That is mostly the renderer draft, but a real conversation can be just as empty — one
   * whose first tool call was refused right after it materialized, or one left from before the
   * draft was global — and it is still withheld from the sidebar and still the same empty desk.
   */
  const composerIsUnsentTask = draftActive || Boolean(
    activeConversation
    && activeWorkspace
    && isUnsentConversation(
      activeConversation,
      activeWorkspace.conversations.some((conversation) => (
        conversation.parentConversationId === activeConversation.id
      ))
    )
  );
  /**
   * Conversation bodies that must stay loaded: the one on screen, and every one with a run in
   * flight — its settlement splices into the body it started from.
   */
  const pinnedBodyIds = useMemo(() => {
    const ids = new Set<string>(Object.keys(modelRunSummaries));
    if (!draftActive && activeConversationId) ids.add(activeConversationId);
    return ids;
  }, [activeConversationId, draftActive, modelRunSummaries]);
  useEffect(() => {
    conversationBodies.sync(document, pinnedBodyIds);
  }, [conversationBodies, document, pinnedBodyIds]);
  /** Why the on-screen conversation's body last failed to load, by conversation. */
  const [bodyLoadFailures, setBodyLoadFailures] = useState<Record<string, string>>({});
  const activeBodyUnloaded = Boolean(
    !draftActive && activeConversation && isBodyUnloaded(activeConversation)
  );
  const loadActiveBody = useCallback((conversationId: string) => {
    setBodyLoadFailures((current) => {
      if (!(conversationId in current)) return current;
      const { [conversationId]: _cleared, ...rest } = current;
      return rest;
    });
    void conversationBodies.ensureLoaded(conversationId).catch((error: unknown) => {
      setBodyLoadFailures((current) => ({
        ...current,
        [conversationId]: error instanceof Error ? error.message : String(error)
      }));
    });
  }, [conversationBodies]);
  // Opening a conversation is what counts as using its body.
  useEffect(() => {
    if (!draftActive && activeConversationId) conversationBodies.touch(activeConversationId);
  }, [activeConversationId, conversationBodies, draftActive]);
  useEffect(() => {
    if (activeBodyUnloaded && activeConversationId) loadActiveBody(activeConversationId);
  }, [activeBodyUnloaded, activeConversationId, loadActiveBody]);
  /** The project's own workspaces as this conversation uses them: its worktree of each stands in for it. */
  const activeProjectWorkspaces = useMemo(
    () => projectWorkspaces(activeWorkspace, activeConversation),
    [activeWorkspace, activeConversation]
  );
  /** The project's workspaces as registered: what worktrees are made from, and named after. */
  const activeRegisteredWorkspaces = useMemo(
    () => registeredProjectWorkspaces(activeWorkspace),
    [activeWorkspace]
  );
  /**
   * The project workspace the composer's workspace chip has selected, 1-based. A selection
   * past the end — a workspace removed from the project since — falls back to the first.
   */
  const activeWorkspaceMember = (() => {
    const selected = activeConversation ? selectedWorkspaceMembers[activeConversation.id] ?? 1 : 1;
    return selected >= 1 && selected <= activeProjectWorkspaces.length ? selected : 1;
  })();
  activeWorkspaceMemberRef.current = activeWorkspaceMember;
  const activeSelectedWorkspace = activeProjectWorkspaces[activeWorkspaceMember - 1] ?? null;
  /**
   * The key the Git snapshot of the selected workspace is stored under: the project id for its
   * first workspace, and the id with the member number for the others, so a snapshot of one
   * directory is never read as another's.
   */
  const activeGitSurfaceKey = activeWorkspace
    ? gitSurfaceKey(activeWorkspace.id, activeWorkspaceMember)
    : undefined;
  const activeGitSnapshotEntry = activeConversation
    ? gitSnapshots[gitSnapshotKey(activeConversation.id, activeWorkspaceMember)]
    : undefined;
  const activeGitSnapshotState = gitSnapshotForWorkspace(
    activeGitSnapshotEntry,
    activeGitSurfaceKey
  );
  const activeGitSnapshot = activeGitSnapshotState ?? null;
  /**
   * Git requests for persisted conversations target their own checkout, including an isolated
   * worktree. Drafts target their selected directory workspace root; temporary-workspace drafts
   * have no shared root and therefore no Git surface.
   */
  const activeGitConversationId = activeConversation?.id ?? null;
  const activeGitWorkspaceId = activeWorkspace?.id ?? null;
  const activeGitWorkspaceKind = activeWorkspace?.kind ?? null;
  /**
   * Whether the active project's first workspace is on another machine. The file pane and the
   * browser's file picker read this computer's filesystem, so they have nothing to show for one;
   * Git works there all the same, on its machine.
   */
  const activeWorkspaceIsRemote = Boolean(activeWorkspace?.machine);
  /**
   * The Git target of the conversation's checkout of its project workspace `member` (1-based),
   * wherever it is: its worktree of that workspace when it has one. A draft has no worktrees yet
   * and addresses the registered directory. Another workspace is addressed by its number within
   * the project, never by path.
   */
  const gitTargetForMember = useCallback((member: number): GitTarget | null => {
    if (!activeGitConversationId) return null;
    if (!draftActive) return gitConversationTarget(activeGitConversationId, member);
    if (!activeGitWorkspaceId || activeGitWorkspaceKind !== "directory") return null;
    return gitWorkspaceTarget(activeGitWorkspaceId, member);
  }, [activeGitConversationId, activeGitWorkspaceId, activeGitWorkspaceKind, draftActive]);
  /** The conversation's checkout of its first workspace, wherever it is. */
  const activePrimaryGitCheckout = useMemo(() => gitTargetForMember(1), [gitTargetForMember]);
  /**
   * The conversation's own checkout when it is on this host. The file pane and the browser's file
   * picker act on this one whatever the chip has selected.
   */
  const activePrimaryGitTarget = activeWorkspaceIsRemote ? null : activePrimaryGitCheckout;
  /**
   * The checkout the Git chip, the status card, the branch menu and the Git writes act on: the
   * selected workspace, on whichever machine it is — the host runs a remote workspace's Git there
   * through its agent, the way it runs a local one here.
   */
  const activeGitStatusTarget = useMemo(
    () => gitTargetForMember(activeWorkspaceMember),
    [activeWorkspaceMember, gitTargetForMember]
  );
  const activeGitTarget = activeGitStatusTarget;
  /**
   * Every project workspace the conversation has a Git surface for, with where its snapshot is
   * kept and the worktree standing in for it. A temporary project's conversation has its scratch
   * directory as workspace 1; a draft aimed at no project has none.
   */
  const activeGitMembers = useMemo(() => {
    if (!activeConversation || !activeWorkspace) return [];
    const registered = activeWorkspace.kind === "directory"
      ? activeRegisteredWorkspaces
      : draftActive ? [] : [{ machine: null, path: activeWorkspace.path }];
    return registered.flatMap((workspace, index) => {
      const member = index + 1;
      const target = gitTargetForMember(member);
      if (!target) return [];
      return [{
        member,
        registered: workspace,
        worktree: draftActive ? null : worktreeFor(activeConversation, member, workspace),
        key: gitSnapshotKey(activeConversation.id, member),
        surfaceKey: gitSurfaceKey(activeWorkspace.id, member),
        target
      }];
    });
  }, [activeConversation, activeRegisteredWorkspaces, activeWorkspace, draftActive, gitTargetForMember]);
  /**
   * Whether the conversation has started. The project it belongs to is settled from then on,
   * so the composer stops offering to change it.
   */
  const activeConversationStarted = Boolean(
    !draftActive && activeConversation && !conversationHasNoContexts(activeConversation)
  );
  /**
   * How many of the conversation's workspace numbers the project takes. A temporary project
   * still takes one — the host gives it a scratch directory — so attached workspaces always
   * start after it.
   */
  const activeProjectWorkspaceCount = Math.max(1, activeProjectWorkspaces.length);
  /** The shells a terminal on `machine` can start, as its probe found them, most preferred first. */
  const terminalShellsOn = useCallback(
    (machine: RunTargetType | null | undefined) => terminalShellsFor(machine, machineShells, platform),
    [machineShells, platform]
  );
  /** The shells a terminal in the selected workspace can start, which follow its machine. */
  const activeTerminalShells = useMemo(
    () => terminalShellsOn(activeSelectedWorkspace?.machine ?? activeWorkspace?.machine),
    [activeSelectedWorkspace?.machine, activeWorkspace?.machine, terminalShellsOn]
  );
  defaultTerminalLaunchRef.current = {
    workspace: activeWorkspaceMember,
    shell: activeTerminalShells[0] ?? null
  };
  /** Opens a new terminal in the conversation's workspace `member` (1-based), in `shell`. */
  const openTerminalIn = useCallback((member: number, shell: TerminalShell) => {
    const activeId = activeConversationIdRef.current;
    if (!activeId) return;
    dispatchTerminalTabs({
      type: "add",
      conversationId: hostConversationId(activeId),
      launch: { workspace: member, shell }
    });
    openPane("terminal");
  }, [dispatchTerminalTabs, hostConversationId, openPane]);
  /**
   * The project workspace the review pane shows, 1-based: the page the reader opened, or the one
   * the composer's workspace chip has selected. A page past the end falls back to the chip's.
   */
  const activeReviewMember = (() => {
    const chosen = activeConversation ? reviewPageMembers[activeConversation.id] : undefined;
    return chosen && activeGitMembers.some((entry) => entry.member === chosen)
      ? chosen
      : activeWorkspaceMember;
  })();
  /**
   * Every checkout of the conversation is kept fresh, each on its own loop: the review pane's
   * pages all show where they stand, and a slow machine holds up nobody else's. The one the chip
   * shows, and the review pane's open page, poll fast; the rest at a background cadence.
   */
  const gitPollSurfaces = useMemo((): GitPollSurface[] => activeGitMembers.map((entry) => ({
    key: entry.key,
    surfaceKey: entry.surfaceKey,
    target: entry.target,
    foreground: entry.member === activeWorkspaceMember
      || (gitReviewPanelOpen && entry.member === activeReviewMember)
  })), [activeGitMembers, activeReviewMember, activeWorkspaceMember, gitReviewPanelOpen]);
  useGitSurfacePolling(
    gitPollSurfaces,
    async (surface) => {
      let failure: string | null = null;
      const snapshot = await refreshGitSnapshot(surface.key, surface.surfaceKey, surface.target, {
        onError: (reason) => {
          failure = failureMessage(reason, t("无法读取 Git 状态", "Could not read the Git status"));
        }
      });
      // A read skipped for a write in flight says nothing either way.
      if (failure === null && snapshot === undefined) return true;
      setGitSurfaceFailures((current) => {
        if ((current[surface.key] ?? null) === failure) return current;
        const next = { ...current };
        if (failure === null) delete next[surface.key];
        else next[surface.key] = failure;
        return next;
      });
      return failure === null;
    },
    hasBackendRuntime()
  );
  /**
   * The review pane's pages: every project workspace that is a Git repository, in the order the
   * reader dragged them into, with the project's order for the rest.
   */
  const activeReviewPages = useMemo(() => {
    const pages = activeGitMembers.flatMap((entry) => {
      const snapshot = gitSnapshotForWorkspace(gitSnapshots[entry.key], entry.surfaceKey);
      return snapshot ? [{ ...entry, snapshot }] : [];
    });
    const order = activeConversation ? reviewPageOrders[activeConversation.id] ?? [] : [];
    const rank = (member: number) => {
      const position = order.indexOf(member);
      return position < 0 ? order.length + member : position;
    };
    return [...pages].sort((left, right) => rank(left.member) - rank(right.member));
  }, [activeConversation, activeGitMembers, gitSnapshots, reviewPageOrders]);
  /**
   * Whether Git has answered for every workspace of the conversation and none of them is a
   * repository — as opposed to answers still on their way.
   */
  const activeGitHasNoRepository = activeGitMembers.every((entry) => (
    gitSnapshotForWorkspace(gitSnapshots[entry.key], entry.surfaceKey) === null
  ));
  // A conversation none of whose workspaces is a Git repository has no review pane to show.
  // Leaving it up would strand the user on a pane whose panel has nothing to render.
  useEffect(() => {
    if (!activeConversation || !activeGitHasNoRepository) return;
    if (!paneIsOpen(sidePaneLayoutFor(sidePanesStateRef.current, activeConversation.id), "review")) return;
    dispatchSidePanes({ type: "close", conversationId: activeConversation.id, pane: "review" });
  }, [activeConversation, activeGitHasNoRepository, dispatchSidePanes]);
  /**
   * Shows the review pane.
   *
   * Deliberately unconditional: the status card's rows all point at the same pane, and returning
   * early when it was already open used to make a second click do nothing at all.
   */
  const openGitReview = useCallback((member?: number) => {
    if (!activeConversation) return;
    const page = member ?? activeReviewMember;
    if (!activeReviewPages.some((entry) => entry.member === page)) return;
    setReviewPageMembers((current) => (
      current[activeConversation.id] === page ? current : { ...current, [activeConversation.id]: page }
    ));
    openPane("review");
  }, [activeConversation, activeReviewMember, activeReviewPages, openPane]);
  const activeComposerDraft = activeConversation ? composerDrafts[activeConversation.id] ?? "" : "";
  const activeComposerConversationId = activeConversation?.id;
  const updateActiveComposerPastedTexts = useCallback((
    update: (current: readonly PastedText[]) => PastedText[]
  ) => {
    if (!activeComposerConversationId) return;
    composerController.updatePastedTexts((current) => ({
      ...current,
      [activeComposerConversationId]: update(current[activeComposerConversationId] ?? [])
    }));
  }, [activeComposerConversationId, composerController]);
  const composerPasteTags = usePastedTextTags({
    textareaRef: composerTextareaRef,
    value: activeComposerDraft,
    pastes: (activeComposerConversationId && composerPastedTexts[activeComposerConversationId]) || NO_PASTED_TEXTS,
    onPastesChange: updateActiveComposerPastedTexts
  });
  const activeComposerImages = activeConversation ? composerImageDrafts[activeConversation.id] ?? [] : [];
  const activeComposerFiles = activeConversation ? composerFileDrafts[activeConversation.id] ?? [] : [];
  const activeComposerAttachmentNotice = activeConversation
    ? composerAttachmentNotices[activeConversation.id] ?? []
    : [];
  const activeElementPicks = activeConversation ? composerElementPicks[activeConversation.id] ?? [] : [];
  /* Every draft image except the element crops, which a chip already stands for.
     The gates above still count them: they are attachments on this message like
     any other, and hiding one from the strip must not hide it from the budget. */
  const activeComposerVisibleImages = useMemo(
    () => imagesWithoutElementCrops(activeComposerImages, activeElementPicks),
    [activeComposerImages, activeElementPicks]
  );
  const activeModelRunning = Boolean(activeConversation && modelRunSummaries[activeConversation.id]);
  const activeComposerHasText = Boolean(activeComposerDraft.trim());
  const activeComposerHasPayload = activeComposerHasText
    || activeComposerImages.length > 0
    || activeComposerFiles.length > 0;
  const activeComposerQueuesMessage = Boolean(
    activeComposerHasPayload
    && (activeModelRunning || activeConversation?.queuedMessages.length)
  );
  const activeComposerImageLoading = Boolean(activeConversation && composerImageLoadingIds.has(activeConversation.id));
  const visibleQueuedMessages = useMemo(() => {
    if (!activeConversation) return [];
    const summary = modelRunSummaries[activeConversation.id];
    if (!summary) return activeConversation.queuedMessages;
    const delivered = new Set(summary.steeredMessageIds);
    return activeConversation.queuedMessages.filter((message) => !delivered.has(message.id));
  }, [activeConversation, modelRunSummaries]);
  useLayoutEffect(() => {
    if (composerTextareaRef.current) resizeComposerTextarea(composerTextareaRef.current);
  }, [activeConversation?.id, activeComposerDraft]);
  const activeWorkspaceDeletionRunning = Boolean(
    activeWorkspace && deletingWorkspaceIds.has(activeWorkspace.id)
  );
  /**
   * The draft, when it is aimed at the active workspace: it works at that root checkout like the
   * workspace's own conversations, with shells and Git writes of its own, though no list holds it.
   */
  const activeWorkspaceDraft = activeWorkspace && draftConversation
    && (draftConversation.workspaceId ?? TEMPORARY_WORKSPACE_ID) === activeWorkspace.id
    ? draftConversation
    : null;
  /** The workspace's conversations and the draft aimed at it, as Git sees the root checkout. */
  const activeWorkspacePeers = useMemo(
    (): Array<Pick<Conversation, "id" | "worktrees">> => [
      ...(activeWorkspace?.conversations ?? []),
      ...(activeWorkspaceDraft ? [{ id: DRAFT_CONVERSATION_ID, worktrees: [] }] : [])
    ],
    [activeWorkspace, activeWorkspaceDraft]
  );
  const activeWorkspaceGitMutationRunning = activeWorkspacePeers.some((peer) => (
    gitMutationConversationIds.has(peer.id)
  ));
  /**
   * Whether another task of the project may be writing the conversation's checkout of project
   * workspace `member`, which holds a Git write to it back. Mirrors `beginGitMutation` through the
   * same predicate, so a button never offers a write the synchronous gate then refuses.
   */
  const gitPeerOperationRunningFor = (member: number) => {
    if (!activeConversation || !activeWorkspace) return false;
    const registered = activeRegisteredWorkspaces[member - 1];
    const surface = gitSurfaceKey(activeWorkspace.id, member);
    // The draft has no worktrees of its own; a peer's count whichever conversation is acting.
    const isolated = (conversation: Pick<Conversation, "worktrees">) => Boolean(
      registered && worktreeFor(conversation, member, registered)
    );
    return activeWorkspacePeers.some((peer) => (
      peer.id !== activeConversation.id
      && gitPeerBlocksMutation({
        acting: {
          snapshot: gitSnapshotForWorkspace(gitSnapshots[gitSnapshotKey(activeConversation.id, member)], surface),
          isolated: isolated(activeConversation)
        },
        peer: {
          snapshot: gitSnapshotForWorkspace(gitSnapshots[gitSnapshotKey(peer.id, member)], surface),
          isolated: isolated(peer)
        },
        peerModelRunActive: Boolean(modelRunSummaries[peer.id]),
        peerGitMutationActive: gitMutationConversationIds.has(peer.id)
      })
    ));
  };
  const activeWorkspaceTerminalBusy = Boolean(
    activeWorkspace && Object.values(terminalSessions).some((session) => (
      session.busy
      && (
        activeWorkspace.conversations.some((conversation) => (
          conversation.id === session.conversationId
        ))
        || session.conversationId === activeWorkspaceDraft?.materializesAs
      )
    ))
  );
  const activeWorkspaceLifecycleOperationRunning = activeWorkspaceDeletionRunning
    || activeWorkspaceGitMutationRunning;
  // Runtime selection uses the global activeProviderId and that provider's activeModelId; the host revalidates the same selection from its trusted snapshot.
  const modelChoiceForConversation = useCallback((
    latest: AppDocument
  ): { provider: ApiProvider | undefined; model: ModelProfile | undefined } => {
    const provider = latest.globalSettings.apiProviders.find((item) => (
      item.id === latest.globalSettings.activeProviderId
    ));
    const model = provider?.models.find((item) => item.id === provider.activeModelId);
    return { provider, model };
  }, []);
  const enabledModelChoices = useMemo(
    () => document?.globalSettings.apiProviders.flatMap((provider) => provider.enabled
      ? provider.models
          .map((model) => ({
            value: JSON.stringify([provider.id, model.id]),
            provider,
            model
          }))
      : []) ?? [],
    [document]
  );
  const activeModelChoice = useMemo(() => {
    if (!document) return null;
    const resolved = modelChoiceForConversation(document);
    if (!resolved.provider || !resolved.model) return null;
    return enabledModelChoices.find(({ provider, model }) => (
      provider.id === resolved.provider!.id && model.id === resolved.model!.id
    )) ?? null;
  }, [document, enabledModelChoices, modelChoiceForConversation]);
  /** Puts back what a conversation's lock holds once its model is picked again
   * (`restoreLockedSettings`). A ref, because the conversation writer it needs
   * is declared further down than the model menu that calls it. */
  const restoreLocksForModelRef = useRef<(providerId: string, modelId: string) => void>(() => {});
  /* Which models this conversation still holds a warm prompt cache on, and until
     when (`modelCacheWarmUntil`). A fork carries its source's lock, so it marks
     the same models until the same moment. */
  const [modelCacheClock, setModelCacheClock] = useState(0);
  const activeConversationSettings = activeConversation?.settings;
  /* Plan mode cannot go on where the model cannot take the plan pair
     mid-conversation and its last request did not carry it (`planModeLocked`). */
  const activePlanModeLocked = useMemo(() => {
    if (!activeConversationSettings || !activeModelChoice) return false;
    const lockModel = toolLockModelOf(activeModelChoice.provider, activeModelChoice.model);
    return planModeLocked(toolLockState(activeConversationSettings, lockModel, Date.now()));
  }, [activeConversationSettings, activeModelChoice]);
  // biome-ignore lint/correctness/useExhaustiveDependencies: `modelCacheClock` is the moment to read the clock again, though nothing here reads it.
  const modelCacheMarks = useMemo(() => {
    const marks = new Map<string, number>();
    if (!activeConversationSettings) return marks;
    const now = Date.now();
    for (const choice of enabledModelChoices) {
      const until = modelCacheWarmUntil(activeConversationSettings, {
        providerId: choice.provider.id,
        modelId: choice.model.id,
        cacheTtlMinutes: choice.model.cacheTtlMinutes
      }, now);
      if (until !== null) marks.set(choice.value, until);
    }
    return marks;
  }, [activeConversationSettings, enabledModelChoices, modelCacheClock]);
  useEffect(() => {
    if (!modelCacheMarks.size) return;
    const soonest = Math.min(...modelCacheMarks.values());
    const timer = window.setTimeout(
      () => setModelCacheClock((tick) => tick + 1),
      Math.max(0, soonest - Date.now()) + 250
    );
    return () => window.clearTimeout(timer);
  }, [modelCacheMarks]);
  /** One section per provider, so the provider reads as a heading rather than a repeated subtitle. */
  const enabledModelSections = useMemo(() => {
    const selectModel = (providerId: string, modelId: string) => {
      documentStore.update((current) => {
        if (!current) return current;
        const provider = current.globalSettings.apiProviders.find((item) => (
          item.id === providerId && item.enabled
        ));
        if (!provider?.models.some((item) => item.id === modelId)) return current;
        return {
          ...current,
          globalSettings: {
            ...current.globalSettings,
            activeProviderId: provider.id,
            apiProviders: current.globalSettings.apiProviders.map((item) => item.id === provider.id
              ? { ...item, activeModelId: modelId }
              : item)
          }
        };
      });
      restoreLocksForModelRef.current(providerId, modelId);
    };
    const sections: PopoverMenuSection[] = [];
    for (const choice of enabledModelChoices) {
      const cachedUntil = modelCacheMarks.get(choice.value);
      const item: PopoverMenuItem = {
        id: choice.value,
        label: choice.model.id,
        checked: choice.value === activeModelChoice?.value,
        badge: cachedUntil === undefined ? undefined : <ModelCacheMark until={cachedUntil} />,
        onSelect: () => selectModel(choice.provider.id, choice.model.id)
      };
      const existing = sections.find((section) => section.id === choice.provider.id);
      if (existing) existing.items.push(item);
      else sections.push({ id: choice.provider.id, label: choice.provider.name, items: [item] });
    }
    return sections;
  }, [activeModelChoice, documentStore, enabledModelChoices, modelCacheMarks]);
  /** Whether the composer's model can see images, which decides how pictures in a paste or drop are taken. */
  const activeComposerImageInput = Boolean(activeModelChoice && supportsVision(activeModelChoice.model));
  const composerDrop = useAttachmentDropZone({
    imageInput: activeComposerImageInput,
    disabled: !activeConversation || activeWorkspaceLifecycleOperationRunning,
    onDrop: (files, preRejected) => {
      if (!activeConversation) return;
      void addComposerAttachments(activeConversation.id, files, preRejected);
    }
  });
  // Images can outlive the model that accepted them: attach under a vision model,
  // switch to a text-only one, and the request still carries them. Sending then
  // refuses deep in the pipeline, which used to happen with no visible reason at
  // all — the composer simply did nothing.
  const activeComposerImagesUnsupported = Boolean(
    activeConversation
    && activeModelChoice
    && !supportsVision(activeModelChoice.model)
    && (activeComposerImages.length > 0
      || contextsContainProjectedImages(activeConversation.contexts))
  );
  const activeProjectionTarget = useMemo(() => activeModelChoice ? {
    providerId: activeModelChoice.provider.id,
    family: activeModelChoice.provider.family
  } : null, [activeModelChoice]);
  const estimateActiveContextUsage = useCallback((contexts: ContextItem[]): ContextUsage => {
    const tokens = estimateContextsTokens(contexts);
    return { tokens, estimated: true, ...activeProjectionTarget };
  }, [activeProjectionTarget]);
  const securityLevelLabelFor = (level: SecurityLevel): string => securityLevelLabel(level, t);
  const activeModelLabel = activeModelChoice
    ? `${activeModelChoice.provider.name} · ${activeModelChoice.model.id}`
    : enabledModelChoices.length
      ? t("选择模型", "Select a model")
      : t("没有已启用的模型", "No enabled models");
  const cachedActiveContextUsage = activeConversation ? contextUsage[activeConversation.id] : undefined;
  /** Authoritative usage reported at the prior turn boundary, projected for the current provider and protocol. */
  const projectedCachedContextUsage = activeConversation && cachedActiveContextUsage
    && cachedActiveContextUsage.providerId === activeProjectionTarget?.providerId
    && cachedActiveContextUsage.family === activeProjectionTarget?.family
    ? cachedActiveContextUsage
    : null;
  /** Use the larger of local estimation and prior authoritative usage as a stable lower bound; contexts can only grow during a turn. */
  const fallbackContextTokens = useMemo(() => Math.max(
    activeConversation ? estimateContextsTokens(activeConversation.contexts) : 0,
    projectedCachedContextUsage?.tokens ?? 0
  ), [activeConversation, projectedCachedContextUsage]);
  /** Live usage combines the latest provider snapshot with estimated streamed content so it changes within a turn without rerendering for equivalent values. */
  const liveContextUsage = useStoreSelector(
    modelRunController.subscribe,
    modelRunController.current,
    (runs) => {
      const run = activeConversation ? runs[activeConversation.id] : undefined;
      return run ? liveContextTokens(run, fallbackContextTokens) : null;
    },
    (previous, next) => (
      previous === next
      || (previous !== null && next !== null
        && previous.tokens === next.tokens
        && previous.estimated === next.estimated)
    )
  );
  const activeContextUsage = useMemo(() => {
    if (!activeConversation) return null;
    if (liveContextUsage) return { ...liveContextUsage, ...activeProjectionTarget };
    return projectedCachedContextUsage
      ?? { tokens: fallbackContextTokens, estimated: true, ...activeProjectionTarget };
  }, [
    activeConversation,
    activeProjectionTarget,
    fallbackContextTokens,
    liveContextUsage,
    projectedCachedContextUsage
  ]);
  const activeComposerStopsRun = !activeComposerQueuesMessage
    && activeModelRunning;
  const activeSecurityLevelLabel = securityLevelLabelFor(
    activeConversation?.settings.securityLevel ?? SECURITY_LEVEL_OPTIONS[0]
  );
  const activeReasoningEffort = activeConversation?.settings.reasoningEffort ?? DEFAULT_REASONING_EFFORT;
  // The levels are named the same in every language: they are the providers' own words.
  const activeReasoningEffortLabel = activeReasoningEffort;
  /** The conversation's isolated worktree of its first workspace; null runs at the registered root. */
  const activePrimaryWorktree = activeGitMembers.find((entry) => entry.member === 1)?.worktree ?? null;
  /** The worktree standing in for the workspace the chip has selected; null runs at its registered root. */
  const activeWorktree = activeGitMembers.find((entry) => entry.member === activeWorkspaceMember)?.worktree ?? null;
  /** A draft checkbox records an intent; its worktree cannot exist until the draft materializes. */
  const activeWorktreeChecked = draftActive
    ? Boolean(draftConversation?.worktreeMembers.includes(activeWorkspaceMember))
    : Boolean(activeWorktree);
  /** A worktree displays its own branch because that is where the Agent writes, not the workspace-root HEAD. */
  const activeBranchLabel = activeWorktree?.branch
    ?? activeGitSnapshot?.branch
    ?? null;
  /** The directory a path written in this conversation's transcript is written against. */
  const timelinePathBaseDir = activePrimaryWorktree?.path
    ?? (!activeWorkspaceIsRemote
      ? gitSnapshotForWorkspace(
        activeConversation ? gitSnapshots[gitSnapshotKey(activeConversation.id, 1)] : undefined,
        activeWorkspace ? gitSurfaceKey(activeWorkspace.id, 1) : undefined
      )?.worktreeRoot
      : undefined)
    ?? activeWorkspace?.path
    ?? null;
  /**
   * Whether the file pane can open. It browses any machine the user can reach, so every
   * conversation has it; one with no workspace yet — a draft aimed at no project — starts at this
   * computer's home.
   */
  const filesPaneAvailable = Boolean(activeConversation);
  const [filesPaneRequest, setFilesPaneRequest] = useState<
    (FilesPaneOpenRequest & { conversationId: string }) | null
  >(null);
  const filesPaneRequestNonce = useRef(0);

  const [reviewPaneRequest, setReviewPaneRequest] = useState<
    (GitReviewRevealRequest & { conversationId: string; member: number }) | null
  >(null);
  const reviewPaneRequestNonce = useRef(0);
  /**
   * A request is acted on once: without this, closing the pane and opening it
   * again from the toolbar would replay the last file the timeline asked for.
   */
  const onFilesPaneRequestHandled = useCallback((nonce: number) => {
    setFilesPaneRequest((current) => (current?.nonce === nonce ? null : current));
  }, []);
  const onReviewPaneRequestHandled = useCallback((nonce: number) => {
    setReviewPaneRequest((current) => (current?.nonce === nonce ? null : current));
  }, []);
  const branchChipDisabled = Boolean(
    !activeGitSnapshot
    || !activeGitTarget
    || activeModelRunning
    || activeWorkspaceLifecycleOperationRunning
    || (activeConversation && gitMutationConversationIds.has(activeConversation.id))
  );
  // Tool-description overrides are assembled by the backend from a trusted snapshot at runtime.
  /**
   * Every workspace the active conversation can address, project members
   * included, in the host's numbering. Keyed on the two conversation fields
   * that decide it, so a streaming turn does not rebuild it.
   */
  const activeAttachedWorkspaces = activeConversation?.attachedWorkspaces ?? null;
  const activeWorktrees = activeConversation?.worktrees ?? null;
  const activeConversationWorkspaces = useMemo(
    () => conversationWorkspaces(
      activeWorkspace,
      activeAttachedWorkspaces
        ? { worktrees: activeWorktrees ?? [], attachedWorkspaces: activeAttachedWorkspaces }
        : null
    ),
    [activeWorkspace, activeAttachedWorkspaces, activeWorktrees]
  );
  /**
   * The conversation's workspaces as the file pane offers them, numbered the way the model
   * addresses them. A draft knows its project's workspaces and nothing it attached, as its
   * terminals do; one aimed at no project has none yet.
   */
  const filesPaneWorkspaces = useMemo((): FilesPaneWorkspace[] => {
    if (draftActive && activePrimaryGitCheckout === null) return [];
    const list = draftActive
      ? activeConversationWorkspaces.slice(0, activeProjectWorkspaceCount)
      : activeConversationWorkspaces;
    // A temporary project's scratch directory is the host's to name; the renderer has no path for it.
    return list.flatMap((workspace, index) => (workspace.path.trim()
      ? [{
          number: index + 1,
          machine: workspace.machine ?? null,
          path: workspace.path,
          label: workspaceDirectoryLabel(workspace.path)
        }]
      : []));
  }, [activeConversationWorkspaces, activePrimaryGitCheckout, activeProjectWorkspaceCount, draftActive]);
  /**
   * Recent probes, by the targets they asked about. A path is probed when the pointer reaches it
   * and again when it is clicked; the click finds the hover's answer here, already on its way.
   */
  const pathProbes = useRef(new Map<string, { at: number; answer: Promise<BrowseProbeResult[]> }>());
  // The menu belongs to the conversation whose transcript it was opened from.
  // biome-ignore lint/correctness/useExhaustiveDependencies: another conversation is the moment to close it, though nothing here reads which.
  useEffect(() => closePathChoice(), [activeConversation?.id]);
  /**
   * A file path clicked anywhere in a transcript opens in the file pane, and a
   * file a turn changed opens on its diff in the review pane when Git tracks the
   * change.
   *
   * The interceptor that catches the click lives outside React, so this is where
   * the two meet: the handler is what knows the conversation's workspaces and their
   * machines, and so where a path written against them can be. A path that can only
   * be one place opens there at once. One that could be in several — a relative path
   * in a conversation with several workspaces, an absolute one in a conversation on
   * several machines — is looked up on all of them in one request, started when the
   * pointer reaches the link so the answer is usually in by the click; found in one,
   * it opens there, found in several, a menu at the link asks which.
   */
  useEffect(() => {
    const conversationId = activeConversation?.id ?? null;
    if (conversationId === null) return;
    const workspaces = filesPaneWorkspaces;
    // Each project workspace that is a repository has a review page, on whichever machine it is;
    // Git reads it at its root, which is the conversation's checkout of that workspace.
    const reviewPages = activeReviewPages;
    const addressContext = {
      sshMachines: document?.globalSettings.executionEnvironments.sshMachines ?? [],
      hostWindows: hostIsWindows(platform)
    };
    // A pane opened only to show this file opens on the file, its file column folded; one
    // already open keeps its column the way the reader left it.
    const paneWasOpen = (pane: SidePaneId) => (
      paneIsOpen(sidePaneLayoutFor(sidePanesStateRef.current, conversationId), pane)
    );
    const showInFiles = (machine: BrowseMachine, path: string, line: number | null, newPage = false) => {
      filesPaneRequestNonce.current += 1;
      setFilesPaneRequest({
        conversationId,
        machine,
        path,
        line,
        nonce: filesPaneRequestNonce.current,
        collapseTree: !paneWasOpen("files"),
        ...(newPage ? { newPage } : {})
      });
      openPane("files");
    };
    const showInReview = (member: number, relative: string) => {
      reviewPaneRequestNonce.current += 1;
      setReviewPageMembers((current) => (
        current[conversationId] === member ? current : { ...current, [conversationId]: member }
      ));
      setReviewPaneRequest({
        conversationId,
        member,
        path: relative,
        nonce: reviewPaneRequestNonce.current,
        collapseTree: !paneWasOpen("review")
      });
      openPane("review");
    };

    interface Candidate {
      /** The workspace the path was resolved in; null for an absolute path, which names a machine. */
      workspace: FilesPaneWorkspace | null;
      machine: BrowseMachine;
      path: string;
    }
    const absolute = (path: string) => (
      path.startsWith("/") || path.startsWith("~") || isWindowsPath(path)
    );
    /** Everywhere a path the transcript wrote could be. */
    const candidatesFor = ({ path, baseDir, workspace }: PathOpenRequest): Candidate[] => {
      const number = typeof workspace === "number" && workspace >= 1 ? workspace : null;
      if (number !== null) {
        // A tool call that named its workspace wrote its path against that workspace.
        const named = workspaces.find((entry) => entry.number === number);
        if (!named) return [];
        const base = number === 1 ? baseDir ?? named.path : named.path;
        return [{ workspace: named, machine: named.machine, path: resolveBrowsePath(browsePath(base), path) }];
      }
      if (absolute(path)) {
        const machines: BrowseMachine[] = [];
        for (const machine of [...workspaces.map((entry) => entry.machine), null]) {
          if (!machines.some((known) => sameBrowseMachine(known, machine))) machines.push(machine);
        }
        return machines.map((machine) => ({ workspace: null, machine, path: browsePath(path) }));
      }
      if (!workspaces.length) {
        return baseDir ? [{ workspace: null, machine: null, path: resolveBrowsePath(browsePath(baseDir), path) }] : [];
      }
      // Workspace 1 is the one the transcript's working directory belongs to.
      return workspaces.map((entry) => ({
        workspace: entry,
        machine: entry.machine,
        path: resolveBrowsePath(browsePath(entry.number === 1 ? baseDir ?? entry.path : entry.path), path)
      }));
    };
    const probe = (candidates: Candidate[]): Promise<BrowseProbeResult[]> => {
      const key = candidates.map((candidate) => locationKey(candidate.machine, candidate.path)).join("\n");
      const now = Date.now();
      for (const [cachedKey, cached] of pathProbes.current) {
        if (now - cached.at > PATH_PROBE_REUSE_MS) pathProbes.current.delete(cachedKey);
      }
      const cached = pathProbes.current.get(key);
      if (cached) return cached.answer;
      const answer = browseProbePaths(candidates.map(({ machine, path }) => ({ machine, path })))
        .catch(() => candidates.map((candidate) => ({ path: candidate.path, kind: null, reached: false })));
      pathProbes.current.set(key, { at: now, answer });
      return answer;
    };
    const choiceSections = (
      hits: { candidate: Candidate; result: BrowseProbeResult }[],
      line: number | null,
      newPage: boolean
    ): ContextMenuSection[] => [{
      id: "targets",
      label: hits.some(({ candidate }) => candidate.workspace) ? t("在哪个工作区打开", "Open in which workspace") : t("在哪台机器上打开", "Open on which machine"),
      items: hits.map(({ candidate, result }) => {
        const machineName = browseMachineLabel(candidate.machine, addressContext.sshMachines, t("本机", "This computer"));
        return {
          id: locationKey(candidate.machine, result.path),
          label: candidate.workspace ? candidate.workspace.label : machineName,
          hint: candidate.workspace ? `${candidate.workspace.number}` : undefined,
          description: formatAddress(candidate.machine, result.path, addressContext),
          descriptionIsPath: true,
          icon: result.kind === "directory"
            ? <Folder size={13} aria-hidden="true" />
            : <FileText size={13} aria-hidden="true" />,
          onSelect: () => showInFiles(candidate.machine, result.path, line, newPage)
        };
      })
    }];

    const removePrefetch = setPathPrefetchHandler((request) => {
      if (request.machine !== undefined || request.review) return;
      const candidates = candidatesFor(request);
      if (candidates.length > 1) void probe(candidates);
    });
    const removeOpen = setPathOpenHandler((request) => {
      const { path, baseDir, line, workspace, review, machine, anchor } = request;
      const newPage = request.newPage === true;
      // A path in a document the file pane shows: on that document's machine.
      if (machine !== undefined) {
        showInFiles(machine, baseDir ? resolveBrowsePath(browsePath(baseDir), path) : browsePath(path), line, newPage);
        return true;
      }
      const number = typeof workspace === "number" && workspace >= 1 ? workspace : 1;
      // A workspace has a review page once Git has answered for it, which a conversation just
      // opened (or one whose reads failed) is still waiting on; the click asks Git itself.
      const member = review ? activeGitMembers.find((entry) => entry.member === number) : undefined;
      const named = workspaces.find((entry) => entry.number === number) ?? null;
      if (review && member && named) {
        const base = number > 1 ? named.path : baseDir ?? timelinePathBaseDir;
        const reviewRelative = workspaceRelativePath(path, base, named.path);
        if (reviewRelative !== null) {
          const target = { machine: named.machine, path: resolveBrowsePath(browsePath(base ?? named.path), path) };
          // Only Git knows whether it tracks the file, so the pane is picked once it has answered.
          const page = reviewPages.find((entry) => entry.member === number);
          void (page
            ? Promise.resolve(page.snapshot)
            : refreshGitSnapshot(member.key, member.surfaceKey, member.target))
            .then((snapshot) => snapshot
              ? gitReviewListsPath(member.target, snapshot, reviewRelative, member.worktree?.baseOid ?? null)
              : false)
            .catch(() => false)
            .then((listed) => {
              if (activeConversationIdRef.current !== conversationId) return;
              if (listed) showInReview(member.member, reviewRelative);
              else showInFiles(target.machine, target.path, line, newPage);
            });
          return true;
        }
      }
      const candidates = candidatesFor(request);
      if (!candidates.length) return false;
      if (candidates.length === 1) {
        showInFiles(candidates[0]!.machine, candidates[0]!.path, line, newPage);
        return true;
      }
      let settled = false;
      let shown = false;
      const menuAnchor: ContextMenuAnchor = anchor
        ? { rect: anchor, align: "start" }
        : { x: window.innerWidth / 2, y: window.innerHeight / 3 };
      // Most answers are in before anyone could see a menu; one that is not gets the menu at
      // once, saying it is still looking, rather than a click that seems to do nothing.
      const pending = window.setTimeout(() => {
        if (settled) return;
        shown = true;
        showPathChoice(menuAnchor, []);
      }, PATH_CHOICE_PATIENCE_MS);
      void probe(candidates).then((results) => {
        settled = true;
        window.clearTimeout(pending);
        if (activeConversationIdRef.current !== conversationId) return;
        const hits = candidates.flatMap((candidate, index) => {
          const result = results[index];
          return result?.kind ? [{ candidate, result }] : [];
        });
        // Two workspaces on one machine can be the same directory under two names.
        const distinct = hits.filter((hit, index) => hits.findIndex((other) => (
          locationKey(other.candidate.machine, other.result.path) === locationKey(hit.candidate.machine, hit.result.path)
        )) === index);
        if (distinct.length >= 2) {
          showPathChoice(menuAnchor, choiceSections(distinct, line, newPage));
          return;
        }
        if (shown) closePathChoice();
        // Nowhere: the first place it could have been, where the pane says what it found — on a
        // machine that answered, since one that is switched off would only keep the pane waiting.
        const target = distinct[0]
          ? { machine: distinct[0].candidate.machine, path: distinct[0].result.path }
          : candidates.find((_, index) => results[index]?.reached) ?? candidates[0]!;
        showInFiles(target.machine, target.path, line, newPage);
      });
      return true;
    });
    return () => {
      removeOpen();
      removePrefetch();
    };
  }, [
    activeConversation?.id,
    activeGitMembers,
    activeReviewPages,
    document?.globalSettings.executionEnvironments.sshMachines,
    filesPaneWorkspaces,
    openPane,
    platform,
    refreshGitSnapshot,
    t,
    timelinePathBaseDir
  ]);
  /**
   * The workspaces a terminal can be opened in, in the conversation's numbering. The host knows
   * a draft's project workspaces and nothing it attached, so those wait until it is sent.
   */
  const activeTerminalWorkspaces = useMemo(
    () => (draftActive
      ? activeConversationWorkspaces.slice(0, activeProjectWorkspaceCount)
      : activeConversationWorkspaces),
    [activeConversationWorkspaces, activeProjectWorkspaceCount, draftActive]
  );
  /**
   * The conversation as the preview commands address it, before a workspace is picked: the host
   * identity its pages and servers are filed under, and for the draft the project it is aimed at.
   * A page adds the number of the workspace it belongs to; the task bar leaves it off to list the
   * servers of every workspace. Preview addresses workspaces exactly as the terminal does, so the
   * draft reaches only its project's.
   */
  const activePreviewDraftWorkspaceId = draftActive
    ? draftConversation?.workspaceId ?? TEMPORARY_WORKSPACE_ID
    : null;
  const activePreviewOwnerTarget = useMemo((): PreviewTarget | null => {
    if (!activeHostConversationId) return null;
    return activePreviewDraftWorkspaceId
      ? { conversationId: activeHostConversationId, draftWorkspaceId: activePreviewDraftWorkspaceId }
      : { conversationId: activeHostConversationId };
  }, [activeHostConversationId, activePreviewDraftWorkspaceId]);
  previewOwnerTargetRef.current = activePreviewOwnerTarget;
  /**
   * Which machine a page is on, by environment key: its workspace's. A page reaches that machine's
   * `localhost`, so a server and a page only belong together when their machines match.
   */
  const previewPageMachineKeyRef = useRef<(sessionId: string) => string>(() => runEnvKey(null));
  previewPageMachineKeyRef.current = (sessionId) => runEnvKey(
    activeTerminalWorkspaces[previewWorkspaceOf(sidePanesStateRef.current, sessionId) - 1]?.machine ?? null
  );
  /** Which machine a server runs on, by the same key. */
  const previewServerMachineKeyRef = useRef<(server: PreviewServerSnapshot) => string>(() => runEnvKey(null));
  previewServerMachineKeyRef.current = (server) => (
    server.workspace
      ? runEnvKey(activeTerminalWorkspaces[server.workspace - 1]?.machine ?? null)
      : server.machine ? `machine:${server.machine}` : runEnvKey(null)
  );
  /** Where a page's start page reads its servers from and runs them: its own workspace. */
  const previewTargetForPage = useCallback((sessionId: string): PreviewTarget | null => {
    if (!activePreviewOwnerTarget) return null;
    const workspace = previewWorkspaceOf(sidePanesState, sessionId);
    return workspace > 1 ? { ...activePreviewOwnerTarget, workspace } : activePreviewOwnerTarget;
  }, [activePreviewOwnerTarget, sidePanesState]);
  /**
   * The catalog as this conversation can use it: a shell tool is listed only
   * when one of its machines has that shell, so the shell tools are the union
   * of the backends its machines' probes found.
   */
  const activeConversationTools = useMemo(() => {
    if (!document) return [];
    const backends = availableShellBackends(activeConversationWorkspaces, machineShells, platform);
    return toolsForShellBackends(document.tools, backends)
      .map((tool) => localizeToolDescriptor(tool, resolvedLanguage));
  }, [document, activeConversationWorkspaces, machineShells, platform, resolvedLanguage]);
  /**
   * What the timeline's manual tool cards are edited against: the catalog plus
   * the `workspace` argument the host puts on the wire once the conversation
   * has more than one workspace. Only the timeline sees it — the tool picker
   * and the subagent panel name tools, they do not fill in calls. Keyed on the
   * two conversation fields that decide the numbers, not the conversation,
   * so a streaming turn does not hand the timeline a fresh descriptor list on
   * every context row.
   */
  const activeTimelineTools = useMemo(
    () => withWorkspaceArgument(
      activeConversationTools,
      activeConversationWorkspaces,
      (machine) => knownShells(machine, machineShells, platform).backends,
      t("工作区编号", "Workspace number")
    ),
    [activeConversationTools, activeConversationWorkspaces, machineShells, platform, t]
  );
  const activeEnabledTools = useMemo(() => {
    const available = new Set(activeConversationTools.map((tool) => tool.name));
    return activeConversation?.settings.enabledTools.filter((name) => available.has(name)) ?? [];
  }, [activeConversation, activeConversationTools]);
  /** Fine-grained timeline, agent, and issue slices retain identity for equivalent values; StreamedConversationView owns the full timeline projection. */
  const projectRenderedContexts = useCallback((runs: ModelRuns): ContextItem[] => {
    if (!activeConversation) return NO_PROJECTED_CONTEXTS;
    const run = runs[activeConversation.id];
    if (!run) return activeConversation.contexts;
    return mergeContextsWithStreamingRun(activeConversation.contexts, run);
  }, [activeConversation]);
  const activeBranchNavigations = useMemo(
    () => activeConversation ? contextBranchNavigations(activeConversation) : {},
    [activeConversation]
  );
  /** Subagent views for workspace cards, task containers, and guards compare chrome fields only; StreamedSubagentPanel derives live text itself. */
  const subagents = useStoreSelector(
    modelRunController.subscribe,
    modelRunController.current,
    (runs) => {
      const conversationId = activeConversation?.id;
      const rendered = projectRenderedContexts(runs);
      const source = conversationId
        ? graftExternalStepBodies(
          rendered,
          (ref) => externalStepBodies[`${conversationId}/${ref.runId}/${ref.stepIndex}`]
        )
        : rendered;
      return deriveSubagentViews(source, subagentViewMessages(t));
    },
    subagentViewsEqualForChrome
  );
  /** Navigate once to the source agent or workflow step for an approval card; retry unresolved sources on later view derivations. */
  const navigatedToolPromptIdsRef = useRef(new Set<string>());
  useEffect(() => {
    const conversationId = activeConversation?.id;
    if (!conversationId) return;
    const queue = toolPrompts[conversationId] ?? [];
    for (let index = queue.length - 1; index >= 0; index -= 1) {
      const prompt = queue[index];
      if (navigatedToolPromptIdsRef.current.has(prompt.promptId)) continue;
      // A plan-exit card is answered in the plan pane, because the answer is
      // "is this plan right" and the plan is not in the timeline.
      if (prompt.kind === "plan_exit") {
        navigatedToolPromptIdsRef.current.add(prompt.promptId);
        setToolPromptCursors((current) => ({ ...current, [conversationId]: index }));
        openPane("plan");
        break;
      }
      if (!prompt.sourceAgent && !prompt.sourceCallId) {
        navigatedToolPromptIdsRef.current.add(prompt.promptId);
        continue;
      }
      const target = approvalPromptSubagentView(subagents, prompt);
      if (!target) continue;
      navigatedToolPromptIdsRef.current.add(prompt.promptId);
      setToolPromptCursors((current) => ({ ...current, [conversationId]: index }));
      openSubagentPanel(target.id);
      break;
    }
  }, [activeConversation?.id, openPane, openSubagentPanel, subagents, toolPrompts]);
  /**
   * Sends the panel's Skip to the run that owns the step.
   *
   * The host validates (requestId, runId) against its live registry, so a panel
   * left on screen after its turn ended is told the run is over rather than
   * silently swallowing the click.
   */
  const handleWorkflowStepControl = useCallback((runId: string, stepIndex: number) => {
    if (!activeConversation) return;
    const running = modelRunController.current()[activeConversation.id];
    if (!running) return;
    void (async () => {
      try {
        await skipWorkflowStep(running.requestId, runId, stepIndex);
      } catch {
        // The workflow may already have ended; skip failures are non-fatal.
      }
    })();
  }, [activeConversation, modelRunController]);
  /**
   * Live workflow ledgers for the task panel, keyed by run *view* id.
   *
   * The selector returns the raw per-call entry record, which the stream layer
   * only replaces when a progress event actually arrives — so a text delta does
   * not re-render App on the way past. Folding happens below, off that identity.
   */
  const workflowLedgerEntriesByCall = useStoreSelector(
    modelRunController.subscribe,
    modelRunController.current,
    (runs) => (activeConversationId
      ? runs[activeConversationId]?.workflowProgressByCall ?? EMPTY_WORKFLOW_ENTRIES
      : EMPTY_WORKFLOW_ENTRIES)
  );
  const workflowRunIdEntriesByCall = useStoreSelector(
    modelRunController.subscribe,
    modelRunController.current,
    (runs) => (activeConversationId
      ? runs[activeConversationId]?.workflowRunIdByCall ?? EMPTY_WORKFLOW_RUN_IDS
      : EMPTY_WORKFLOW_RUN_IDS)
  );
  const workflowProgressByRun = useMemo<Record<string, WorkflowProgressView>>(() => {
    const messages = {
      fallbackStepLabel: (index: number) => t("步骤 {number}", "Step {number}", { number: index + 1 }),
      unphasedHeading: t("未分组", "Ungrouped"),
      cachedBadge: t("已缓存", "Cached"),
      blockedBadge: t("等待中", "Waiting"),
      skippedBadge: t("已跳过", "Skipped"),
      progressLabel: (done: number, total: number) => t(
        "工作流进度：{total} 步中已完成 {done} 步",
        "Workflow progress: {done} of {total} steps completed",
        { done, total }
      )
    };
    const byRun: Record<string, WorkflowProgressView> = {};
    for (const agent of subagents) {
      for (const callId of agent.callIds) {
        const entries = workflowLedgerEntriesByCall[callId];
        if (entries) byRun[agent.id] = deriveWorkflowProgress(entries, messages);
      }
    }
    return byRun;
  }, [subagents, t, workflowLedgerEntriesByCall]);
  const workflowRunIdsByRun = useMemo<Record<string, string>>(() => {
    const byRun: Record<string, string> = {};
    for (const agent of subagents) {
      for (const callId of agent.callIds) {
        const runId = workflowRunIdEntriesByCall[callId];
        if (runId) byRun[agent.id] = runId;
      }
    }
    return byRun;
  }, [subagents, workflowRunIdEntriesByCall]);
  const taskMessages = useMemo(() => taskContainerMessages(t), [t]);
  /**
   * Brings one run's panel forward, which is what the compact card in the
   * message stream offers instead of the transcript the old row navigated to.
   * The panel may not be mounted yet when the tasks pane was closed, so the
   * scroll waits for the frame that mounts it.
   */
  const focusWorkflowRunPanel = useCallback((runId: string) => {
    showTasksTab("tasks");
    window.requestAnimationFrame(() => {
      window.document
        .querySelector(`[data-workflow-run="${CSS.escape(runId)}"]`)
        ?.scrollIntoView({ block: "nearest" });
    });
  }, [showTasksTab]);
  /** Oldest outstanding card for this conversation shows by default. One at a
   * time: the card sits above the composer, and rendering several would bury
   * the composer — the stack is flipped through with the pager in the card's
   * top-right corner instead. */
  const activeToolPromptQueue = activeConversation
    ? toolPrompts[activeConversation.id] ?? []
    : [];
  const activeToolPromptCursor = Math.max(0, Math.min(
    (activeConversation ? toolPromptCursors[activeConversation.id] : 0) ?? 0,
    activeToolPromptQueue.length - 1
  ));
  const activeToolPrompt = activeToolPromptQueue[activeToolPromptCursor] ?? null;
  /** The run is blocked on an `ask_user` card: a composer message answers it first. */
  const activeQuestionPending = activeToolPromptQueue.some((prompt) => prompt.kind === "question");
  /** Pager state for the card's top-right corner; absent for a single card. */
  const activeToolPromptStack = activeConversation && activeToolPromptQueue.length > 1
    ? {
        index: activeToolPromptCursor,
        total: activeToolPromptQueue.length,
        onNavigate: (delta: number) => {
          const conversationId = activeConversation.id;
          const next = activeToolPromptCursor + delta;
          setToolPromptCursors((current) => ({ ...current, [conversationId]: next }));
        }
      }
    : undefined;
  /**
   * Exactly one surface owns the pending card. Several panes can be open at once now, so the
   * dock has to be addressed to a single one or the same prompt would be answerable from the
   * plan, the subagent pane and the composer at the same time. The subagent pane takes it only
   * while the requesting agent is its shown tab: a card behind another tab is one nobody sees.
   */
  const approvalDockOwner: SidePaneId | "composer" = (() => {
    if (!activeToolPrompt) return "composer";
    if (activeToolPrompt.kind === "plan_exit" && planPageOpen) return "plan";
    if (activeToolPrompt.sourceAgent || activeToolPrompt.sourceCallId) {
      const target = approvalPromptSubagentView(subagents, activeToolPrompt);
      if (target && shownSubagent(activeLayout) === target.id) return "subagent";
    }
    return "composer";
  })();
  const activeModelRunBusy = Boolean(activeConversation && (
    modelRunSummaries[activeConversation.id]
    || modelRunController.hasRunToken(activeConversation.id)
    || modelRunController.hasPreparingRun(activeConversation.id)
  ));
  /** The preview tool driving this conversation's page right now, if any. */
  const activePreviewPageTool = activeConversation
    ? modelRunSummaries[activeConversation.id]?.browserAutomationTool ?? null
    : null;
  const activeBrowserAutomationStopping = Boolean(
    activeConversation && browserAutomationStoppingIds.has(activeConversation.id)
  );
  /**
   * Every live preview session of this conversation, one task row each.
   *
   * A session with no row of its own would be a Chromium process the user can neither reach nor
   * close, which is exactly what the tab strip used to prevent.
   */
  const activeBrowserSessions = useMemo(() => (
    currentPreviewSessions.flatMap((sessionId) => {
      const status = browserStatuses[sessionId];
      return status ? [{ sessionId, status }] : [];
    })
  ), [browserStatuses, currentPreviewSessions]);
  // Read from the reconciliation below, which runs on a host event rather than on a render, so it
  // must see the sessions as they are now instead of the ones its closure was built with.
  const activeBrowserSessionsRef = useRef(activeBrowserSessions);
  activeBrowserSessionsRef.current = activeBrowserSessions;
  /**
   * Every dev server this conversation may address — its own, plus the ones no conversation owns,
   * exactly as `preview_list` narrows them for the model.
   *
   * These are the preview task rows. Read on a push event rather than polled: the model starts and
   * stops servers without the renderer asking, and the registry says so the moment it happens.
   */
  const [previewServers, setPreviewServers] = useState<PreviewServerSnapshot[]>([]);
  const previewServersRef = useRef<PreviewServerSnapshot[]>([]);
  /**
   * Each SSH machine's link to its agent, by host, as the host last reported it. A page served
   * from a machine whose link is down keeps what it had on screen; this is what lets the pane say
   * why nothing on it is answering, instead of leaving the user to read a timeout as a bug.
   */
  const [remoteLinks, setRemoteLinks] = useState<Record<string, { state: string; detail: string | null }>>({});
  useEffect(() => onAppPushEvent((event) => {
    if (event.type !== "remoteLinkChanged") return;
    setRemoteLinks((current) => (
      current[event.host]?.state === event.state && current[event.host]?.detail === event.detail
        ? current
        : { ...current, [event.host]: { state: event.state, detail: event.detail } }
    ));
  }), []);
  const remoteLinkNotice = useCallback((machine: RunTargetType | null | undefined): string | null => {
    if (machine?.kind !== "ssh") return null;
    const config = document?.globalSettings.executionEnvironments.sshMachines.find((candidate) => (
      candidate.id === machine.machineId
    ));
    if (!config) return null;
    const link = remoteLinks[config.host];
    const name = config.name || config.host;
    switch (link?.state) {
      case "reconnecting":
        return t(
          "与 {name} 的连接中断，正在重新连接；页面和服务器会在连接恢复后继续",
          "Lost the connection to {name}; reconnecting. The page and its servers carry on once it is back",
          { name }
        );
      case "lost":
        return link.detail
          ? t("无法连接到 {name}：{detail}", "Cannot reach {name}: {detail}", { name, detail: link.detail })
          : t("无法连接到 {name}", "Cannot reach {name}", { name });
      case "unavailable":
        return t(
          "{name} 上无法运行 Mework 的远程代理，这台机器上的预览不可用",
          "Mework's remote agent cannot run on {name}, so previews there are unavailable",
          { name }
        );
      default:
        return null;
    }
  }, [document, remoteLinks, t]);
  /**
   * Takes down every page of this conversation that `address` was serving.
   *
   * The page carries no binding to a server, so the committed origin is what says which pages a
   * dead server takes with it. A page showing something else — a docs site the user navigated to
   * in the address bar — is nobody's dependent and survives.
   *
   * What "taking it down" means depends on whether the user can still see it. A page in an open
   * pane goes back to `about:blank`, which puts the pane on its own start page with the server
   * listed and a button to run it again: closing the pane instead would make stopping a server an
   * act that also dismantles the surface you were watching it in, and leave nothing on screen
   * saying why. A page with no pane behind it has no such surface to return to and is closed — it
   * is a Chromium process the user would otherwise have no way to reach or shut down.
   */
  const closePagesServedAt = useCallback(async (
    conversationId: string,
    server: PreviewServerSnapshot
  ): Promise<void> => {
    const address = previewServerAddress(server);
    const serverMachine = previewServerMachineKeyRef.current(server);
    const doomed = activeBrowserSessionsRef.current.filter(({ sessionId, status }) => (
      previewUrlIsServedAt(status.url, address)
      // A page's `localhost` is its own machine's: a page of a workspace on an SSH machine reaches
      // that machine's servers, so the same address on two machines is two different servers.
      && previewPageMachineKeyRef.current(sessionId) === serverMachine
    ));
    const layout = sidePaneLayoutFor(sidePanesStateRef.current, conversationId);
    for (const { sessionId } of doomed) {
      // Only the page on screen goes back to its start page. A page in another tab is still
      // reachable from the strip, and waking it just to blank it would spend a live page slot on
      // a page nobody is looking at; it shows the server gone when it is next opened.
      if (!paneIsOpen(layout, previewPaneId(sessionId))) continue;
      // A refusal is not worth reporting: the page may already be gone, and the pane's own body
      // reads the committed URL either way.
      await navigateBrowser(sessionId, "about:blank").catch(() => undefined);
    }
  }, []);
  const closePagesServedAtRef = useRef(closePagesServedAt);
  closePagesServedAtRef.current = closePagesServedAt;
  /**
   * Re-reads the dev-server list and takes down the page of every server that has just gone.
   *
   * One function for both halves because they are one fact: the list the task bar draws and the
   * set of pages that still have a server behind them are read from the same snapshot, and doing
   * them separately is how a page outlives the process it was showing.
   */
  const refreshPreviewServers = useCallback(async (
    conversationId: string,
    ownerId: string,
    target: PreviewTarget,
    stoppedIds: readonly string[]
  ): Promise<void> => {
    let listed: PreviewServerSnapshot[];
    try {
      listed = await listPreviewServers(target);
    } catch {
      // A workspace that has gone away answers again on the next event; the bar keeps what it has.
      return;
    }
    const mine = listed.filter((server) => (
      !server.sessionId || server.sessionId === ownerId
    ));
    const previous = previewServersRef.current;
    previewServersRef.current = mine;
    // Replace the state only on a real change: this runs on a host event, and a fresh array every
    // time would re-render the whole shell for a list that says the same thing.
    if (
      previous.length !== mine.length
      || mine.some((server, index) => (
        previous[index].handle !== server.handle || previous[index].status !== server.status
      ))
    ) {
      setPreviewServers(mine);
    }
    // Only the servers somebody stopped take their page with them. A server that exited on its
    // own is gone from the list too, and its page stays: what it shows then — a connection
    // refused, a stack trace the framework printed on the way down — is the whole diagnosis, and
    // the pane's own card offers the restart.
    const stopped = previous.filter((server) => (
      stoppedIds.includes(server.handle) && !mine.some((live) => live.handle === server.handle)
    ));
    for (const server of stopped) {
      await closePagesServedAtRef.current(conversationId, server);
    }
  }, []);
  useEffect(() => {
    const conversationId = activeConversation?.id;
    if (!conversationId || !activePreviewOwnerTarget || !hasBackendRuntime()) {
      previewServersRef.current = [];
      setPreviewServers([]);
      return undefined;
    }
    // Every workspace's servers, each carrying its number: a conversation that works on several
    // machines has a dev server list on each, and the task bar is one list of all of them.
    const target = activePreviewOwnerTarget;
    // The host files the draft's servers under the id it will be sent as, not its placeholder.
    const ownerId = activeHostConversationId ?? conversationId;
    // A conversation switch starts from nothing rather than from the previous conversation's list:
    // a server missing from *this* conversation's list was never this conversation's to close.
    previewServersRef.current = [];
    setPreviewServers([]);
    void refreshPreviewServers(conversationId, ownerId, target, []);
    return onAppPushEvent((event) => {
      if (event.type !== "previewServersChanged") return;
      void refreshPreviewServers(conversationId, ownerId, target, event.stopped);
    });
  }, [
    activeConversation?.id,
    activeHostConversationId,
    JSON.stringify(activePreviewOwnerTarget),
    refreshPreviewServers
  ]);
  /**
   * Keeps each page filed under the workspace it is actually showing.
   *
   * The model's `preview_start` can point the conversation's own page at a server in any of its
   * workspaces, and when that workspace is on another machine the host moves the page onto that
   * machine's network as it does so. The page's network is the fact to go by: a page is moved to a
   * workspace on the machine it now uses — the one whose server it is showing when one matches,
   * the first on that machine otherwise. A page the user opened is already where it belongs and
   * is left alone.
   */
  useEffect(() => {
    const conversationId = activeConversationId;
    if (!conversationId || activeTerminalWorkspaces.length < 2) return;
    const machineOf = (member: number) => runEnvKey(activeTerminalWorkspaces[member - 1]?.machine ?? null);
    for (const { sessionId, status } of activeBrowserSessions) {
      if (!status.hasPage) continue;
      const current = previewWorkspaceOf(sidePanesState, sessionId);
      // Only another machine is ever reported; a page with none named is on this computer's
      // network, or on one the host has not said — which is no reason to move it anywhere.
      const reported = status.networkMachine ?? null;
      const network = reported ?? runEnvKey(null);
      const serving = previewServers.filter((server) => (
        server.workspace
        && machineOf(server.workspace) === network
        && previewUrlIsServedAt(status.url, previewServerAddress(server))
      ));
      let next = current;
      if (serving.length && !serving.some((server) => server.workspace === current)) {
        next = serving[0].workspace ?? current;
      } else if (reported && machineOf(current) !== reported) {
        const index = activeTerminalWorkspaces.findIndex((workspace) => (
          runEnvKey(workspace.machine ?? null) === network
        ));
        if (index >= 0) next = index + 1;
      }
      if (next !== current) {
        dispatchSidePanes({ type: "set_preview_workspace", conversationId, sessionId, workspace: next });
      }
    }
  }, [activeBrowserSessions, activeConversationId, activeTerminalWorkspaces, dispatchSidePanes, previewServers, sidePanesState]);
  const activeModelStopping = Boolean(
    activeConversation && modelStoppingIds.has(activeConversation.id)
  );
  const activeTaskTerminals = useMemo(() => {
    if (!activeHostConversationId) return [];
    return Object.values(terminalSessions).filter((session) => (
      session.conversationId === activeHostConversationId
      && session.phase === "running"
      && (session.busy || session.hasHistory)
    ));
  }, [activeHostConversationId, terminalSessions]);
  // Push delivery is incremental, so reconcile the full task list when a conversation becomes active and after browser-dev reconnection.
  useEffect(() => {
    const conversationId = activeConversation?.id;
    // Drafts do not exist at the host, so querying their background tasks would waste an IPC call.
    if (!conversationId || draftActive || !hasBackendRuntime()) return undefined;
    let cancelled = false;
    const reconcile = () => {
      listShellTasks(conversationId).then((tasks) => {
        if (cancelled) return;
        const evicted = evictedShellTaskIdsRef.current;
        setShellTasks((current) => [
          // Reconcile only this conversation's rows. A list answered before an
          // eviction that has since arrived must not resurrect the evicted row.
          ...current.filter((task) => task.conversationId !== conversationId),
          ...tasks.filter((task) => !evicted.has(task.shellTaskId))
        ]);
      }).catch((error) => console.error("Failed to list shell tasks", error));
    };
    reconcile();
    const stopListening = isBrowserDevRuntime()
      ? onBrowserDevReconnected(reconcile)
      : undefined;
    return () => {
      cancelled = true;
      stopListening?.();
    };
  }, [activeConversation?.id, draftActive]);

  // The plan lives at the host, so it has to be fetched when a conversation
  // becomes active; `conversationPlanUpdated` keeps it current from then on.
  useEffect(() => {
    const conversationId = activeConversation?.id;
    if (!conversationId || draftActive || !hasBackendRuntime()) return undefined;
    let cancelled = false;
    const requestedAt = planPushCountRef.current;
    loadConversationPlan(conversationId)
      .then((plan) => {
        if (cancelled) return;
        // A push that landed while this was in flight is newer than the answer.
        if (planPushCountRef.current !== requestedAt) return;
        setPlans((current) => ({ ...current, [conversationId]: plan }));
      })
      .catch((error) => console.error("Failed to load the conversation plan", error));
    return () => {
      cancelled = true;
    };
  }, [activeConversation?.id, draftActive]);

  const activePlan = activeConversationId ? plans[activeConversationId] ?? null : null;
  const activeShellTasks = useMemo(() => {
    if (!activeConversation) return [];
    return shellTasks.filter((task) => task.conversationId === activeConversation.id);
  }, [activeConversation, shellTasks]);
  /**
   * Task row the focused pane is currently showing, so the task list marks it as current.
   *
   * An agent row's id is its agent id, which is why one field covers transcripts and other panes
   * alike. The review pane has no task row — it is reached from the Git status card.
   */
  const activeReadOnlyShellTaskId = (() => {
    const terminals = terminalTabsFor(terminalTabsState, activeHostConversationId);
    return terminals.activeId === READ_ONLY_TERMINAL_TAB_ID ? terminals.readOnly : null;
  })();
  const selectedTaskRowId = useMemo(() => {
    if (!activeFocusedPane) return null;
    const kind = paneKind(activeFocusedPane);
    if (kind === "subagent") return selectedSubagentId;
    if (kind === "preview") return activeFocusedPane;
    // A command's row is marked while the terminal pane shows its output.
    if (kind === "terminal") return activeReadOnlyShellTaskId;
    if (kind === "plan") return "plan";
    // A targeted history pane is that agent's surface too, so reading what it
    // did keeps its row marked rather than clearing the mark the transcript set.
    if (kind === "history") return paneTarget(activeFocusedPane);
    return null;
  }, [activeFocusedPane, activeReadOnlyShellTaskId, selectedSubagentId]);
  /** True while a plan-exit card is waiting on this conversation's plan. */
  const planAwaitingApproval = activeToolPrompt?.kind === "plan_exit";
  /** Everything the conversation currently has running, as one set of inputs. */
  const taskSources = useMemo<TaskSources>(() => ({
    conversationId: activeConversation?.id,
    agents: subagents,
    terminals: activeTaskTerminals,
    shellTasks: activeShellTasks,
    previewServers,
    browserSessions: activeBrowserSessions,
    browserSessionId: activeConversation?.id ?? null,
    browserAutomationTool: activePreviewPageTool,
    browserAutomationStopping: activeBrowserAutomationStopping,
    modelRequestId: activeConversation ? modelRunController.current()[activeConversation.id]?.requestId ?? null : null,
    userAbortedTasks: activeConversation?.userAbortedTasks ?? [],
    forkDecisions: activeConversation ? forkDecisions[activeConversation.id] ?? [] : [],
    inheritedModelId: activeModelChoice?.model.id ?? null,
    plan: activePlan,
    planAwaitingApproval
  }), [
    activeBrowserAutomationStopping,
    activeBrowserSessions,
    activeConversation,
    activeModelChoice,
    activePlan,
    activePreviewPageTool,
    activeShellTasks,
    activeTaskTerminals,
    forkDecisions,
    planAwaitingApproval,
    previewServers,
    subagents
  ]);
  /** What the waiting line beside a live round says is still running, read off the rows the tasks pane draws. */
  const runningTaskCount = useMemo(
    () => countRunningTasks(deriveTaskItems(taskSources, taskMessages)),
    [taskMessages, taskSources]
  );
  const openTasksPane = useCallback(() => showTasksTab("tasks"), [showTasksTab]);

  /** The agent whose read-only transcript the focused subagent pane is showing, if any. */
  const selectedSubagentView = useMemo(() => (
    selectedSubagentId ? findOpenableSubagentView(subagents, selectedSubagentId) : null
  ), [selectedSubagentId, subagents]);
  // Fetch an externalized workflow step's full record the first time the
  // drawer opens it. Searching the grafted tree keeps this idempotent: once a
  // body is grafted the context carries a nested record again and yields no
  // coordinates. An IPC failure is treated as transient — nothing is cached,
  // so re-selecting the step retries; only a definitive backend answer
  // (record or null) lands in the cache.
  useEffect(() => {
    if (!activeConversation || selectedSubagentView?.kind !== "workflowStep") return;
    const conversationId = activeConversation.id;
    const run = modelRunController.current()[conversationId];
    const rendered = run
      ? mergeContextsWithStreamingRun(activeConversation.contexts, run)
      : activeConversation.contexts;
    const source = graftExternalStepBodies(
      rendered,
      (bodyRef) => externalStepBodies[`${conversationId}/${bodyRef.runId}/${bodyRef.stepIndex}`]
    );
    const ref = findExternalStepBodyRef(source, selectedSubagentView.callIds);
    if (!ref) return;
    const key = `${conversationId}/${ref.runId}/${ref.stepIndex}`;
    if (key in externalStepBodies || externalStepBodyLoads.current.has(key)) return;
    externalStepBodyLoads.current.add(key);
    void workflowStepRecord(conversationId, ref.runId, ref.stepIndex)
      .then((record) => {
        setExternalStepBodies((previous) => ({ ...previous, [key]: record }));
      })
      .catch(() => {})
      .finally(() => {
        externalStepBodyLoads.current.delete(key);
      });
  }, [activeConversation, selectedSubagentView, externalStepBodies, modelRunController]);
  const activeTimelineMutationBlocked = activeModelRunBusy
    || activeWorkspaceLifecycleOperationRunning;
  /** Why a Git write to the conversation's checkout of project workspace `member` cannot run now. */
  const gitMutationDisabledReasonFor = (member: number) => activeModelRunBusy
    ? t(
      "模型或子代理正在使用工作区，结束后才能执行 Git 写操作",
      "A model or subagent is using the workspace. Wait for it to finish before changing Git state."
    )
      : gitPeerOperationRunningFor(member)
          ? t(
            "同一项目中的另一项任务正在运行，暂不能执行 Git 写操作",
            "Another task in this project is running, so Git changes are temporarily unavailable."
          )
          : activeWorkspaceTerminalBusy
            ? t(
              "内置终端正在执行命令，结束后才能执行 Git 写操作",
              "The built-in terminal is running a command. Wait before changing Git state."
            )
        : activeWorkspaceDeletionRunning
          ? t(
            "项目正在删除，无法执行 Git 写操作",
            "Git changes are unavailable while the project is being deleted."
          )
          : null;
  // The mirror of `gitMutationDisabledReasonFor`: a Git write is rewriting the very checkout the
  // shell is sitting in, so the terminal stops taking input until it lands. The terminal→Git
  // direction is the branch above; both have to hold or the two can still interleave.
  const terminalInputDisabledReason = activeWorkspaceGitMutationRunning
    ? t(
      "Git 写操作进行中，完成后才能在终端输入",
      "A Git write is running. Wait for it to finish before typing in the terminal."
    )
    : null;
  const branchSwitchDisabledReason = activeWorkspaceGitMutationRunning
    ? t(
      "Git 写操作进行中，完成后才能切换对话分支",
      "Wait for the Git operation to finish before switching conversation branches."
    )
    : activeModelRunBusy
    ? t(
      "模型回合进行中，完成或停止后才能切换分支",
      "Wait for the model turn to finish or stop before switching branches"
    )
      : activeWorkspaceDeletionRunning
        ? t("项目正在删除，无法切换分支", "Cannot switch branches while the project is being deleted")
        : null;
  // Branching only opens a new conversation with this message as a draft, so
  // it needs neither a configured model nor an idle turn — only a workspace
  // that is not going away underneath it.
  const branchFromDisabledReason = activeWorkspaceDeletionRunning
    ? t("项目正在删除，无法创建分支", "Cannot create a branch while the project is being deleted")
    : null;
  const forkDisabledReason = activeWorkspaceDeletionRunning
    ? t("项目正在删除，无法分叉会话", "Cannot fork the conversation while the project is being deleted")
    : null;

  const syncBrowserPanelBounds = useCallback((bounds: SidePaneBounds) => {
    const sessionId = browserController.visibleSession();
    if (!isTauriRuntime() || !browserPanelOpen || previewPaneCovered || !sessionId) return;
    const intent = browserController.currentIntent(sessionId);
    if (intent?.desired !== "open") return;
    lastPreviewBoundsRef.current = { ...lastPreviewBoundsRef.current, [sessionId]: { ...bounds } };
    const payload = previewPageBounds(sessionId, bounds);
    sendLatestPreviewBounds(sessionId, () => setBrowserPanelBounds(sessionId, payload, intent.epoch)
      .then((status) => {
        if (
          !browserIntentIsCurrent(sessionId, intent.epoch, "open")
          || browserController.visibleSession() !== sessionId
        ) return;
        browserController.updateStatuses((current) => ({ ...current, [sessionId]: status }));
      }));
  }, [
    browserController,
    browserIntentIsCurrent,
    browserPanelOpen,
    previewPageBounds,
    previewPaneCovered,
    sendLatestPreviewBounds
  ]);

  /**
   * Records the pane height the preview's log drawer took and republishes the page rectangle.
   *
   * The drawer opens without the pane resizing, so nothing else would tell the host that the page
   * has less room than the pane does.
   */
  const reservePreviewBottom = useCallback((sessionId: string, reservedBottom: number) => {
    const rounded = Math.max(0, Math.round(reservedBottom));
    if ((previewReservedBottomRef.current[sessionId] ?? 0) === rounded) return;
    previewReservedBottomRef.current = {
      ...previewReservedBottomRef.current,
      [sessionId]: rounded
    };
    const bounds = lastPreviewBoundsRef.current[sessionId];
    if (bounds && browserController.visibleSession() === sessionId) syncBrowserPanelBounds(bounds);
  }, [browserController, syncBrowserPanelBounds]);

  // The subagent pane only keeps tabs for agents that still exist. A tab opened
  // while the call was streaming follows the agent to its persisted record,
  // whose view id switches from the call id to the stable name id. A tab that
  // lands on a workflow run is dropped outright: a run is a script and has no
  // transcript, so there is nothing for it to show. An agent's history pane is
  // addressed by the same id and follows it the same way.
  useEffect(() => {
    const conversationId = activeConversationId;
    if (!conversationId) return;
    const layout = sidePaneLayoutFor(sidePanesStateRef.current, conversationId);
    for (const subagentId of layout.subagentTabs) {
      const matched = findOpenableSubagentView(subagents, subagentId);
      if (!matched) {
        dispatchSidePanes({ type: "close_subagent", conversationId, subagentId });
      } else if (matched.id !== subagentId) {
        dispatchSidePanes({ type: "retarget_subagent", conversationId, from: subagentId, to: matched.id });
      }
    }
    for (const pane of layout.panes) {
      // The conversation's own history is a tab of the tasks pane; a history
      // pane is always an agent's.
      const subagentId = paneKind(pane) === "history" ? paneTarget(pane) : null;
      if (!subagentId) continue;
      const matched = findOpenableSubagentView(subagents, subagentId);
      if (!matched) {
        dispatchSidePanes({ type: "close", conversationId, pane });
      } else if (matched.id !== subagentId) {
        dispatchSidePanes({
          type: "replace", conversationId, pane, replacement: subagentHistoryPaneId(matched.id)
        });
      }
    }
  }, [activeConversationId, dispatchSidePanes, sidePanesState, subagents]);

  // The plan pane only stays up while there is a plan. Clearing the plan — the
  // host discarding it once implementation starts — would otherwise leave an
  // empty pane with no row left in the task bar to explain it. A plan that has
  // not been fetched yet is unknown, not absent: a card re-opened after a
  // reload navigates here before the fetch answers.
  const activePlanKnownAbsent = activeConversationId
    ? plans[activeConversationId] === null
    : false;
  useEffect(() => {
    const conversationId = activeConversationId;
    if (!conversationId || !planPageOpen || !activePlanKnownAbsent) return;
    dispatchSidePanes({ type: "close", conversationId, pane: "plan" });
  }, [activeConversationId, activePlanKnownAbsent, dispatchSidePanes, planPageOpen]);

  const updateConversation = useCallback(
    (
      workspaceId: string,
      conversationId: string,
      updater: (conversation: Conversation) => Conversation,
      options: { persist?: boolean } = {}
    ) => {
      let base: Conversation | null = null;
      let next: Conversation | null = null;
      documentStore.update((current) => current ? {
        ...current,
        workspaces: current.workspaces.map((workspace) => {
          if (workspace.id !== workspaceId) return workspace;
          // Update the workspace's last settings only when the settings reference changes; streaming context updates preserve it.
          let lastConversationSettings = workspace.lastConversationSettings;
          const conversations = workspace.conversations.map((conversation) => {
            if (conversation.id !== conversationId) return conversation;
            // An unloaded body is an empty stand-in: an updater that read it keeps its metadata
            // changes only, never a body derived from nothing.
            const updated = keepUnloadedBody(conversation, updater(conversation));
            if (updated.settings !== conversation.settings) lastConversationSettings = updated.settings;
            base = conversation;
            next = updated;
            return updated;
          });
          return { ...workspace, conversations, lastConversationSettings };
        })
      } : current);
      // Host-produced contexts are read-model updates only; user-initiated changes require write-back.
      if (options.persist === false) return;
      if (base && next && base !== next) {
        conversationSync.changed(workspaceId, base, next);
      }
    },
    [conversationSync, documentStore]
  );

  const startConversationTurn = useCallback((
    conversationId: string,
    requestId: string,
    anchorContextId: string,
    modelId: string,
    startedAt: string,
    contexts: ContextItem[],
    usageBaseline: ModelUsage = {},
    usageRevisionAtStart = 0
  ) => {
    updateConversationTurns((current) => {
      const existing = current[conversationId] ?? [];
      if (existing.some((turn) => turn.requestId === requestId && turn.anchorContextId === anchorContextId)) {
        return current;
      }
      const anchorIndex = contexts.findIndex((context) => context.id === anchorContextId);
      const previousContexts = anchorIndex < 0 ? contexts : contexts.slice(0, anchorIndex);
      const openRequestIds = new Set(existing
        .filter((turn) => turn.status === "running")
        .map((turn) => turn.requestId));
      let reconciled = existing;
      openRequestIds.forEach((openRequestId) => {
        reconciled = materializeRunTurnContexts(reconciled, previousContexts, openRequestId);
      });
      reconciled = reconciled.map((turn) => (
        turn.status === "running"
          ? {
            ...turn,
            status: "interrupted" as const,
            endedAt: startedAt,
            durationMs: (turn.durationMs ?? 0) + activeTurnSegmentDuration(turn, startedAt)
          }
          : turn
      ));
      const turn: ConversationTurn = {
        id: createId("ui-turn"),
        requestId,
        anchorContextId,
        modelId,
        startedAt,
        durationMs: 0,
        status: "running",
        contextIds: [],
        usage: {},
        usageOffset: {},
        usageBaseline,
        usageRevisionAtStart,
        segmentCount: 1
      };
      return { ...current, [conversationId]: [...reconciled, turn] };
    });
  }, [updateConversationTurns]);

  const updateRunningTurnUsage = useCallback((
    conversationId: string,
    requestId: string,
    cumulativeUsage: ModelUsage,
    usageRevision: number
  ) => {
    updateConversationTurns((current) => {
      const existing = current[conversationId] ?? [];
      let changed = false;
      const next = existing.map((turn) => {
        if (
          turn.requestId !== requestId
          || turn.status !== "running"
          || usageRevision <= turn.usageRevisionAtStart
        ) return turn;
        changed = true;
        return {
          ...turn,
          usage: sumModelUsage([
            turn.usageOffset,
            subtractModelUsage(cumulativeUsage, turn.usageBaseline)
          ])
        };
      });
      return changed ? { ...current, [conversationId]: next } : current;
    });
  }, [updateConversationTurns]);

  /** Resume an adopted host run as running. Reset startedAt because load conversion already accounted for its prior duration, preventing double-counting. */
  const resumeAdoptedConversationTurn = useCallback((
    conversationId: string,
    requestId: string,
    contexts: ContextItem[],
    modelId: string
  ) => {
    const resumedAt = new Date().toISOString();
    if (!(conversationTurnsRef.current[conversationId] ?? []).some((turn) => turn.requestId === requestId)) {
      startConversationTurn(conversationId, requestId,
        contexts[contexts.length - 1]?.id ?? TIMELINE_START_ANCHOR, modelId, resumedAt, contexts);
      return;
    }
    updateConversationTurns((current) => {
      const existing = current[conversationId] ?? [];
      const adopted = [...existing].reverse().find((turn) => (
        turn.requestId === requestId && turn.status === "interrupted"
      ));
      if (!adopted) return current;
      const next = existing.map((turn) => {
        if (turn.id !== adopted.id) return turn;
        // The run is live again, so the failure recorded when loading marked it
        // interrupted is stale. Its settlement re-records one if it still applies.
        const { endedAt: _endedAt, error: _error, ...rest } = turn;
        return {
          ...rest,
          status: "running" as const,
          startedAt: resumedAt
        };
      });
      return { ...current, [conversationId]: next };
    });
  }, [startConversationTurn, updateConversationTurns]);

  /**
   * Attach a run that carries no new user message to a round.
   *
   * A round ends on a new user message or on a normal final reply, so a bare
   * Send after a stop, a retry, and a task wake all belong to the round they
   * follow: the resumed turn keeps accumulating its elapsed time and its input,
   * cached-input and output counts. When there is nothing to continue the run
   * still opens a turn of its own, anchored at the timeline tail — a round with
   * no header is a round whose cost the user cannot read.
   */
  const continueConversationTurn = useCallback((
    conversationId: string,
    requestId: string,
    modelId: string,
    contexts: ContextItem[]
  ) => {
    const resumedAt = new Date().toISOString();
    let resumed = false;
    updateConversationTurns((current) => {
      const existing = current[conversationId] ?? [];
      const target = findResumableTurn(existing, contexts);
      if (!target) return current;
      resumed = true;
      return {
        ...current,
        [conversationId]: resumeConversationTurn(existing, contexts, target, {
          requestId,
          modelId,
          startedAt: resumedAt
        })
      };
    });
    if (resumed) return;
    startConversationTurn(
      conversationId,
      requestId,
      contexts[contexts.length - 1]?.id ?? TIMELINE_START_ANCHOR,
      modelId,
      resumedAt,
      contexts
    );
  }, [startConversationTurn, updateConversationTurns]);

  const splitConversationTurn = useCallback((
    workspaceId: string,
    conversationId: string,
    run: ModelRunState,
    nextAnchor: UserContext
  ) => {
    const persisted = findConversation(documentStore.current(), workspaceId, conversationId).conversation?.contexts ?? [];
    const projected = mergeUniqueContexts(
      run.request.contexts,
      persisted,
      contextsFromModelRun(run, true)
    );
    const endedAt = new Date().toISOString();
    const cumulativeUsage = cumulativeModelRunUsage(run);
    updateConversationTurns((current) => {
      const existing = current[conversationId] ?? [];
      const materialized = materializeRunTurnContexts(existing, projected, run.requestId);
      const finalized = materialized.map((turn) => {
        if (turn.requestId !== run.requestId || turn.status !== "running") return turn;
        const usage = run.usageRevision > turn.usageRevisionAtStart
          ? sumModelUsage([
            turn.usageOffset,
            subtractModelUsage(cumulativeUsage, turn.usageBaseline)
          ])
          : turn.usage;
        return {
          ...turn,
          status: "interrupted" as const,
          endedAt,
          durationMs: (turn.durationMs ?? 0) + activeTurnSegmentDuration(turn, endedAt),
          usage
        };
      });
      const nextTurn: ConversationTurn = {
        id: createId("ui-turn"),
        requestId: run.requestId,
        anchorContextId: nextAnchor.id,
        modelId: run.modelName,
        startedAt: endedAt,
        durationMs: 0,
        status: "running",
        contextIds: [],
        usage: {},
        usageOffset: {},
        usageBaseline: cumulativeUsage,
        usageRevisionAtStart: run.usageRevision,
        segmentCount: 1
      };
      return { ...current, [conversationId]: [...finalized, nextTurn] };
    });
  }, [updateConversationTurns]);

  const finishConversationTurns = useCallback((
    conversationId: string,
    requestId: string,
    contexts: ContextItem[],
    status: "completed" | "interrupted",
    options: { modelId?: string; usage?: ModelUsage; durationMs?: number } = {}
  ) => {
    const endedAt = new Date().toISOString();
    updateConversationTurns((current) => {
      const existing = current[conversationId] ?? [];
      const materialized = materializeRunTurnContexts(existing, contexts, requestId);
      const requestTurns = materialized.filter((turn) => turn.requestId === requestId);
      const runningTurn = [...requestTurns].reverse().find((turn) => turn.status === "running");
      if (!runningTurn) return current;
      const previousRunUsage = sumModelUsage(requestTurns
        .filter((turn) => turn.id !== runningTurn.id)
        .map((turn) => subtractModelUsage(turn.usage, turn.usageOffset)));
      const currentSegmentUsage = options.usage
        ? subtractModelUsage(options.usage, previousRunUsage)
        : subtractModelUsage(runningTurn.usage, runningTurn.usageOffset);
      const terminalUsage = sumModelUsage([runningTurn.usageOffset, currentSegmentUsage]);
      const next = materialized.map((turn) => {
        if (turn.id !== runningTurn.id) return turn;
        const segmentDuration = options.durationMs !== undefined && requestTurns.length === 1
          ? options.durationMs
          : activeTurnSegmentDuration(turn, endedAt);
        const durationMs = (turn.durationMs ?? 0) + segmentDuration;
        return {
          ...turn,
          ...(options.modelId ? { modelId: options.modelId } : {}),
          status,
          endedAt,
          durationMs,
          usage: terminalUsage
        };
      });
      return { ...current, [conversationId]: next };
    });
  }, [updateConversationTurns]);

  /**
   * Attach a run failure to the turn it happened in. Called after the turn is
   * already finalized, so the notice lands on a terminal turn and stays readable
   * after a reload — unlike the composer notice, which only lives in memory.
   */
  const failConversationTurn = useCallback((
    conversationId: string,
    requestId: string,
    error: { message: string; providerName: string; modelName: string }
  ) => {
    const turnError: ConversationTurnError = { ...error, at: new Date().toISOString() };
    updateConversationTurns((current) => {
      const existing = current[conversationId] ?? [];
      const next = annotateTurnFailure(existing, requestId, turnError);
      return next === existing ? current : { ...current, [conversationId]: next };
    });
  }, [updateConversationTurns]);

  /**
   * Retract this conversation's last failure. `"notice"` clears only the
   * composer copy; `"failure"` also strips the turn-level notices and drops the
   * header-only turns that carried them, so a retracted failure can never leave
   * a bare "stopped after 12s" header behind.
   */
  const clearModelRunError = useCallback((
    conversationId: string,
    scope: "notice" | "failure" = "failure"
  ) => {
    setModelRunErrors((current) => {
      if (!(conversationId in current)) return current;
      const next = { ...current };
      delete next[conversationId];
      return next;
    });
    if (scope === "notice") return;
    updateConversationTurns((current) => {
      const existing = current[conversationId];
      if (!existing) return current;
      const next = clearTurnFailures(existing);
      return next === existing ? current : { ...current, [conversationId]: next };
    });
  }, [updateConversationTurns]);

  const persistInterruptedRun = useCallback((workspaceId: string, conversationId: string, run: ModelRunState) => {
    // Callers capture their snapshot before awaiting the backend's cancel
    // acknowledgement, and stream state publishes on a fixed 100 ms cadence, so
    // that snapshot can be a commit behind — or the last events may still be
    // buffered. Flush, then re-read: losing the final tenth of a second of a
    // turn is losing exactly the content the user was looking at when they
    // chose to stop it.
    modelRunController.flushPendingEvents(conversationId);
    const current = modelRunController.current()[conversationId];
    const latest = current?.requestId === run.requestId ? current : run;
    const interrupted = contextsFromInterruptedRun(latest);
    const persisted = findConversation(documentStore.current(), workspaceId, conversationId).conversation?.contexts ?? [];
    const knownIds = new Set(persisted.map((context) => context.id));
    const generated = interrupted.filter((context) => {
      if (knownIds.has(context.id)) return false;
      knownIds.add(context.id);
      return true;
    });
    finishConversationTurns(
      conversationId,
      latest.requestId,
      mergeUniqueContexts(latest.request.contexts, persisted, generated),
      "interrupted"
    );
    if (!interrupted.length) return;
    updateConversation(workspaceId, conversationId, (conversation) =>
      applyStreamedRunContexts(conversation, interrupted, latest.requestId), { persist: false });
    conversationSync.refresh(conversationId).then((authoritative) => {
      if (authoritative) applyAuthoritativeConversation(workspaceId, authoritative);
    }).catch(() => undefined);
  }, [
    applyAuthoritativeConversation,
    conversationSync,
    finishConversationTurns,
    modelRunController,
    updateConversation
  ]);

  /** After settlement, replace the read model from the host conversation store so UI and disk match exactly. */
  const refreshConversation = useCallback((workspaceId: string, conversationId: string) => {
    conversationSync.refresh(conversationId).then((authoritative) => {
      if (authoritative) applyAuthoritativeConversation(workspaceId, authoritative);
    }).catch(() => undefined);
  }, [applyAuthoritativeConversation, conversationSync]);

  /** Replace settled signed tool cards by id and debounce-persist them so finalized subagent records survive a mid-run process death. */
  const applySettledToolContext = useCallback((
    _workspaceId: string,
    conversationId: string,
    context: Extract<ContextItem, { kind: "tool" }>
  ) => {
    documentStore.update((current) => (
      current
        ? applyQuarantinedContextReplacements(current, [{
            conversationId,
            contextId: context.id,
            replacement: context
          }])
        : current
    ));
  }, [documentStore]);

  const updateActiveConversation = useCallback(
    (updater: (conversation: Conversation) => Conversation) => {
      // A preset-owned edit turns the conversation into an unnamed draft. Derive it here so
      // every surface that edits settings agrees, and skip it when the updater set `presetId`
      // itself, which is how applying a preset re-establishes the trace.
      const withPresetTrace = (conversation: Conversation): Conversation => {
        const updated = updater(conversation);
        if (
          !conversation.presetId
          || updated.presetId !== conversation.presetId
          || updated.settings === conversation.settings
        ) return updated;
        return sameConversationPresetSettings(
          captureConversationPresetSettings(conversation.settings),
          captureConversationPresetSettings(updated.settings)
        ) ? updated : { ...updated, presetId: "" };
      };
      // Apply the same updater to draft projections so settings and timeline surfaces need not
      // special-case drafts. Settings and content are the draft's own; `worktree` is not, because a
      // worktree belongs to a persisted conversation id and the draft carries `worktreeMembers`.
      const draft = draftConversationRef.current;
      if (draft && isDraftConversationId(activeConversationIdRef.current)) {
        setDraftConversation((current) => {
          if (!current) return current;
          const updated = withPresetTrace(draftAsConversation(current, ""));
          return {
            ...current,
            settings: updated.settings,
            contexts: updated.contexts,
            presetId: updated.presetId,
            templateId: updated.templateId
          };
        });
        return;
      }
      if (!activeWorkspaceId || !activeConversationId) return;
      updateConversation(activeWorkspaceId, activeConversationId, (conversation) => ({
        ...withPresetTrace(conversation),
        updatedAt: new Date().toISOString()
      }));
    },
    [activeWorkspaceId, activeConversationId, updateConversation]
  );

  /** Presets are templates, so composition edits are written directly to conversation settings. */
  const saveActiveConversationComposition = useCallback(
    (settings: ConversationSettingsType) => {
      if (!document) return;
      updateActiveConversation((conversation) => ({ ...conversation, settings }));
    },
    [document, updateActiveConversation]
  );

  const updateActiveConversationSettingsOnly = useCallback(
    (patch: Partial<ConversationSettingsType>) => {
      updateActiveConversation((conversation) => ({
        ...conversation,
        settings: { ...conversation.settings, ...patch }
      }));
    },
    [updateActiveConversation]
  );

  /** Reload branches whenever the menu opens; show read failures in place without interrupting composition. */
  const loadBranchPicker = useCallback(() => {
    const conversationId = activeConversationId;
    const workspaceId = activeGitSurfaceKey;
    if (!conversationId || !workspaceId || !activeGitTarget || !hasBackendRuntime()) return;
    setBranchPicker({ conversationId, status: "loading", branches: [] });
    void (async () => {
      try {
        const result = await getGitBranches(activeGitTarget);
        setBranchPicker((current) => current?.conversationId === conversationId
          ? {
            conversationId,
            status: "ready",
            // Exclude remote branches because checking out a remote ref creates a detached HEAD.
            branches: result.branches.filter((branch) => branch.kind === "local")
          }
          : current);
      } catch (reason) {
        const message = failureMessage(reason, t("无法读取分支列表", "Could not read the branch list"));
        setBranchPicker((current) => current?.conversationId === conversationId
          ? { conversationId, status: "error", branches: [], message }
          : current);
      }
    })();
  }, [activeConversationId, activeGitTarget, activeGitSurfaceKey, t]);

  /** Switch branches through the shared GitAction channel and workspace mutation lease; preserve Git's own checkout errors rather than discarding changes. */
  const checkoutComposerBranch = useCallback(async (branch: string) => {
    const conversationId = activeConversationId;
    const workspaceId = activeGitSurfaceKey;
    const target = activeGitTarget;
    const member = activeWorkspaceMember;
    if (!conversationId || !workspaceId || !target) return;
    if (!beginGitMutation(conversationId, member)) {
      setBranchChipError(t(
        "工作区里还有别的操作在进行，请稍后再切换分支",
        "Another operation is running in this workspace; try switching branches later"
      ));
      return;
    }
    setBranchChipError(null);
    try {
      await executeGitAction(target, { type: "checkout", branch });
      setBranchPicker(null);
    } catch (reason) {
      setBranchChipError(failureMessage(reason, t("切换分支失败", "Could not switch branches")));
    } finally {
      endGitMutation(conversationId);
      await refreshGitSnapshot(gitSnapshotKey(conversationId, member), workspaceId, target);
    }
  }, [
    activeConversationId,
    activeGitTarget,
    activeGitSurfaceKey,
    activeWorkspaceMember,
    beginGitMutation,
    endGitMutation,
    refreshGitSnapshot,
    t
  ]);


  /**
   * WSL distributions offered by the attach-workspace menu, or `null` before the
   * menu has been opened. Enumerated on each open rather than cached: a
   * distribution can be installed or removed while the app is running, and the
   * menu is the moment the answer matters.
   */
  const [machineMenuDistros, setMachineMenuDistros] = useState<WslDistro[] | null>(null);
  /**
   * The machine whose remote directory browser is open, with its display name
   * and what the chosen directory becomes: another numbered workspace of this
   * conversation, or the workspace the conversation moves to.
   */
  const [remoteWorkspacePicker, setRemoteWorkspacePicker] = useState<
    { machine: RunTargetType; name: string; purpose: "attach" } | null
  >(null);
  /** The machine whose settings a gear opened; `{ machine: null }` is this one. */
  const [machineSettings, setMachineSettings] = useState<{ machine: RunTargetType | null } | null>(null);
  /**
   * The workspace whose settings a gear opened — its sandbox and variables: its machine and
   * the directory it is registered at — never a worktree standing in for it — with its
   * display name.
   */
  const [workspaceSettings, setWorkspaceSettings] = useState<
    { machine: RunTargetType | null; path: string; name: string } | null
  >(null);

  const loadMachineMenu = useCallback(() => {
    void listWslDistros().then(setMachineMenuDistros).catch(() => setMachineMenuDistros([]));
  }, []);

  /**
   * Replace the conversation's attached workspaces, on the same terms as the
   * run target: a draft keeps them in its own state until materialization.
   */
  const setConversationAttachedWorkspaces = useCallback((
    next: (current: AttachedWorkspace[]) => AttachedWorkspace[]
  ) => {
    if (isDraftConversationId(activeConversationIdRef.current)) {
      setDraftConversation((current) => (
        current ? { ...current, attachedWorkspaces: next(current.attachedWorkspaces) } : current
      ));
      return;
    }
    if (!activeWorkspaceId || !activeConversationId) return;
    updateConversation(activeWorkspaceId, activeConversationId, (conversation) => ({
      ...conversation,
      attachedWorkspaces: next(conversation.attachedWorkspaces),
      updatedAt: new Date().toISOString()
    }));
  }, [activeWorkspaceId, activeConversationId, updateConversation]);

  /** Append one workspace, ignoring a machine-and-path pair already attached. */
  const attachWorkspace = useCallback((machine: RunTargetType | null, path: string) => {
    setConversationAttachedWorkspaces((current) => (
      current.some((entry) => (
        sameMachine(entry.machine, machine) && entry.path === path
      ))
        ? current
        : [...current, machine ? { machine, path } : { path }]
    ));
  }, [setConversationAttachedWorkspaces]);

  /**
   * Attach one workspace on the host machine through the native picker.
   *
   * The picker is what authorizes the path — the host will refuse to save a
   * document naming a directory it never returned — so there is no text-entry
   * path here, and nothing to do when the user cancels. A workspace on another
   * machine goes through {@link RemoteDirectoryPicker} instead, which is the
   * same rule served by a different dialog.
   */
  const attachLocalWorkspace = useCallback(async () => {
    if (!hasNativeWorkspacePicker()) return;
    try {
      const path = await pickWorkspaceDirectory();
      if (!path) return;
      attachWorkspace(null, path);
    } catch {
      // Cancelling or failing to pick a directory is not an error worth reporting; the button retries.
    }
  }, [attachWorkspace]);

  const detachWorkspace = useCallback((workspace: AttachedWorkspace) => {
    setConversationAttachedWorkspaces((current) => current.filter((entry) => !(
      sameMachine(entry.machine, workspace.machine) && entry.path === workspace.path
    )));
  }, [setConversationAttachedWorkspaces]);

  /**
   * Sets one workspace's sandbox. The entry stays when it is switched off: it records that
   * answer, which the host's hand-over of sandboxes conversations used to carry respects.
   */
  const saveWorkspaceSandbox = useCallback((
    machine: RunTargetType | null,
    path: string,
    sandbox: SandboxSettings
  ) => {
    const key = workspaceEnvKey(machine, path);
    documentStore.update((current) => {
      if (!current) return current;
      const environments = current.globalSettings.executionEnvironments;
      return {
        ...current,
        globalSettings: {
          ...current.globalSettings,
          executionEnvironments: {
            ...environments,
            sandboxes: { ...environments.sandboxes, [key]: sandbox }
          }
        }
      };
    });
  }, [documentStore]);

  /** Sets one workspace's environment variables; an empty table removes its entry. */
  const saveWorkspaceEnvVars = useCallback((
    machine: RunTargetType | null,
    path: string,
    vars: Record<string, string>
  ) => {
    const envKey = workspaceEnvKey(machine, path);
    documentStore.update((current) => {
      if (!current) return current;
      const envVars = { ...current.globalSettings.executionEnvironments.envVars };
      if (Object.keys(vars).length) envVars[envKey] = vars;
      else delete envVars[envKey];
      return {
        ...current,
        globalSettings: {
          ...current.globalSettings,
          executionEnvironments: {
            ...current.globalSettings.executionEnvironments,
            envVars
          }
        }
      };
    });
  }, [documentStore]);

  /** Probes a machine now and keeps the answer; the saved catalog is what the host reads, so pending saves land first. */
  const probeMachine = useCallback(async (machine: RunTargetType | null): Promise<MachineShells> => {
    await documentStore.flush();
    const endpoint = () => sshEndpoint(documentStore.current()?.globalSettings.executionEnvironments, machine);
    const probed = endpoint();
    const shells = await probeMachineShells(machine);
    // A machine moved to another address while it was being probed is not
    // described by the old address's answer; the host does not keep using it either.
    if (endpoint() === probed) {
      setMachineShells((current) => ({ ...current, [runEnvKey(machine)]: shells }));
    }
    return shells;
  }, [documentStore]);

  const setMachineAgentShell = useCallback((machine: RunTargetType, backend: ShellBackend) => {
    documentStore.update((current) => {
      if (!current) return current;
      const environments = current.globalSettings.executionEnvironments;
      const next = withAgentShell(environments, machine, backend);
      return next === environments
        ? current
        : { ...current, globalSettings: { ...current.globalSettings, executionEnvironments: next } };
    });
  }, [documentStore]);

  const machineShellsControl = useMemo<MachineShellsControl | null>(() => document ? {
    probes: machineShells,
    environments: document.globalSettings.executionEnvironments,
    probe: probeMachine,
    setAgentShell: setMachineAgentShell
  } : null, [document, machineShells, probeMachine, setMachineAgentShell]);

  const saveSshMachine = useCallback((machine: SshMachineConfigType) => {
    const previous = documentStore.current()?.globalSettings.executionEnvironments.sshMachines
      .find((item) => item.id === machine.id);
    // A machine is probed as soon as it is added, which is when its first
    // probe records its agent shell; and again when it now names another
    // endpoint, whose shells the old answer says nothing about.
    const endpointChanged = !previous
      || previous.host !== machine.host
      || previous.port !== machine.port
      || previous.identityFile !== machine.identityFile;
    if (endpointChanged) {
      // Until the new endpoint answers, the machine reads as never probed:
      // should that probe fail, the old answer would otherwise stay in force.
      setMachineShells((current) => withoutProbe(current, runEnvKey({ kind: "ssh", machineId: machine.id })));
      queueMicrotask(() => {
        void probeMachine({ kind: "ssh", machineId: machine.id }).catch(() => undefined);
      });
    }
    documentStore.update((current) => {
      if (!current) return current;
      const machines = current.globalSettings.executionEnvironments.sshMachines;
      const exists = machines.some((item) => item.id === machine.id);
      return {
        ...current,
        globalSettings: {
          ...current.globalSettings,
          executionEnvironments: {
            ...current.globalSettings.executionEnvironments,
            sshMachines: exists
              ? machines.map((item) => (item.id === machine.id ? machine : item))
              : [...machines, machine]
          }
        }
      };
    });
  }, [documentStore, probeMachine]);

  /**
   * Removes an SSH machine from the catalog, and the variables and sandbox of every workspace
   * on it with it: a machine registered again gets a new id, so those entries could never be
   * reached. Workspaces on it stay where they are and read as a deleted machine until chosen
   * again.
   */
  const deleteSshMachine = useCallback((machineId: string) => {
    const prefix = `${runEnvKey({ kind: "ssh", machineId })}|`;
    setMachineShells((current) => withoutProbe(current, runEnvKey({ kind: "ssh", machineId })));
    documentStore.update((current) => {
      if (!current) return current;
      const environments = current.globalSettings.executionEnvironments;
      return {
        ...current,
        globalSettings: {
          ...current.globalSettings,
          executionEnvironments: {
            ...environments,
            sshMachines: environments.sshMachines.filter((machine) => machine.id !== machineId),
            envVars: Object.fromEntries(
              Object.entries(environments.envVars).filter(([key]) => !key.startsWith(prefix))
            ),
            ...(environments.sandboxes
              ? {
                sandboxes: Object.fromEntries(
                  Object.entries(environments.sandboxes).filter(([key]) => !key.startsWith(prefix))
                )
              }
              : {})
          }
        }
      };
    });
  }, [documentStore]);

  /**
   * Creates or releases the conversation's worktree of the workspace the chip has selected, on
   * whichever machine it is, and persists the record the host resolves that workspace through.
   * On disable, release before clearing the record because the host uses it to find the tree.
   * Drafts retain only the request until they materialize before their first send.
   */
  const toggleConversationWorktree = useCallback(async (enabled: boolean) => {
    const conversationId = activeConversationId;
    const member = activeWorkspaceMember;
    const entry = activeGitMembers.find((candidate) => candidate.member === member);
    if (!conversationId || !entry) return;
    if (draftConversationRef.current && isDraftConversationId(conversationId)) {
      setBranchChipError(null);
      setDraftConversation((current) => {
        if (!current) return current;
        const others = current.worktreeMembers.filter((candidate) => candidate !== member);
        return { ...current, worktreeMembers: enabled ? [...others, member].sort((a, b) => a - b) : others };
      });
      setBranchPicker(null);
      return;
    }
    if (!beginGitMutation(conversationId, member)) {
      setBranchChipError(t(
        "工作区里还有别的操作在进行，请稍后再切换工作树",
        "Another operation is running in this workspace; try toggling the worktree later"
      ));
      return;
    }
    setBranchChipError(null);
    try {
      if (enabled) {
        const worktree = await createConversationWorktree(conversationId, member);
        updateActiveConversation((conversation) => ({
          ...conversation,
          worktrees: withConversationWorktree(conversation.worktrees, member, entry.registered, worktree)
        }));
      } else {
        const removed = await releaseConversationWorktree(conversationId, member);
        // Clear the record even when uncommitted work keeps the tree; retained files belong to the user, not this conversation.
        updateActiveConversation((conversation) => ({
          ...conversation,
          worktrees: withConversationWorktree(conversation.worktrees, member, entry.registered, null)
        }));
        if (!removed) {
          setBranchChipError(t(
            "工作树里还有未提交的改动，目录与分支已保留",
            "The worktree still has uncommitted work, so its directory and branch were kept"
          ));
        }
      }
      setBranchPicker(null);
    } catch (reason) {
      setBranchChipError(failureMessage(
        reason,
        enabled
          ? t("无法建立隔离工作树", "Could not create the isolated worktree")
          : t("无法释放隔离工作树", "Could not release the isolated worktree")
      ));
    } finally {
      endGitMutation(conversationId);
      // The host resolves the checkout from the saved record, so the next read has to follow it.
      await flushLatestDocument();
      await refreshGitSnapshot(entry.key, entry.surfaceKey, entry.target);
    }
  }, [
    activeConversationId,
    activeGitMembers,
    activeWorkspaceMember,
    beginGitMutation,
    endGitMutation,
    flushLatestDocument,
    refreshGitSnapshot,
    t,
    updateActiveConversation
  ]);

  /** Evaluate functional global changes against the store's current value to preserve multiple same-tick updates. */
  const handleGlobalSettingsChange = useCallback((change: GlobalSettingsChange) => {
    documentStore.update((current) => current ? applyGlobalSettingsChange(current, change) : current);
  }, [documentStore]);

  /** Saving a preset creates an independent template; neither it nor the source conversation follows later edits. */
  const conversationMoveIsBlocked = useCallback((
    conversationId: string,
    sourceWorkspaceId: string,
    targetWorkspaceId: string
  ): boolean => (
    deletingConversationIdsRef.current.has(conversationId)
    || deletingWorkspaceIdsRef.current.has(sourceWorkspaceId)
    || deletingWorkspaceIdsRef.current.has(targetWorkspaceId)
  ), []);

  const moveActiveConversation = useCallback(async (targetWorkspaceId: string) => {
    const sourceWorkspaceId = activeWorkspaceIdRef.current;
    const movingConversationId = activeConversationIdRef.current;
    const initialDocument = documentStore.current();
    const sourceWorkspace = initialDocument?.workspaces.find((workspace) => (
      workspace.id === sourceWorkspaceId
    ));
    const moving = sourceWorkspace?.conversations.find((conversation) => (
      conversation.id === movingConversationId
    ));
    if (
      !sourceWorkspaceId
      || !movingConversationId
      || !moving
      || !conversationHasNoContexts(moving)
    ) return false;
    const destinationId = targetWorkspaceId;
    if (
      destinationId === sourceWorkspaceId
      || conversationMoveIsBlocked(movingConversationId, sourceWorkspaceId, destinationId)
    ) return false;
    const movingTerminals = Object.values(terminalController.current())
      .filter((session) => session.conversationId === moving.id);
    const terminalResults = await Promise.allSettled(movingTerminals.map((session) => (
      requestTerminalSessionClose(moving.id, session.terminalId)
    )));
    if (
      terminalResults.some((result) => result.status === "rejected")
      || conversationMoveIsBlocked(moving.id, sourceWorkspaceId, destinationId)
      || activeConversationIdRef.current !== moving.id
    ) return false;
    const latest = documentStore.current();
    const source = latest?.workspaces.find((workspace) => workspace.id === sourceWorkspaceId);
    const destination = latest?.workspaces.find((workspace) => workspace.id === destinationId);
    const latestMoving = source?.conversations.find((conversation) => conversation.id === moving.id);
    if (
      !latest
      || !source
      || !destination
      || !latestMoving
      || !conversationHasNoContexts(latestMoving)
      || conversationMoveIsBlocked(moving.id, sourceWorkspaceId, destinationId)
    ) return false;
    const stripped = latest.workspaces.map((workspace) => workspace.id === sourceWorkspaceId
      ? { ...workspace, conversations: detachAbsentParents(workspace.conversations.filter((conversation) => conversation.id !== moving.id)) }
      : workspace);
    const next = {
      ...latest,
      workspaces: stripped.map((workspace) => workspace.id === destinationId
        ? { ...workspace, conversations: detachAbsentParents([latestMoving, ...workspace.conversations]) }
        : workspace)
    };
    documentStore.update(() => next);
    // Reordering the destination workspace also updates the host-side conversation ownership.
    conversationSync.reordered(
      destinationId,
      next.workspaces.find((workspace) => workspace.id === destinationId)
        ?.conversations.map((conversation) => conversation.id) ?? []
    );
    setActiveWorkspaceId(destinationId);
    setEditor(null);
    return true;
  }, [
    conversationMoveIsBlocked,
    conversationSync,
    documentStore,
    requestTerminalSessionClose
  ]);

  const reorderWorkspace = useCallback((workspaceId: string, targetWorkspaceId: string, position: "before" | "after") => {
    if (workspaceId === targetWorkspaceId) return;
    documentStore.update((current) => {
      if (!current) return current;
      const sourceIndex = current.workspaces.findIndex((workspace) => workspace.id === workspaceId);
      const targetIndex = current.workspaces.findIndex((workspace) => workspace.id === targetWorkspaceId);
      if (sourceIndex < 0 || targetIndex < 0) return current;
      const next = [...current.workspaces];
      const [moving] = next.splice(sourceIndex, 1);
      const adjustedTargetIndex = next.findIndex((workspace) => workspace.id === targetWorkspaceId);
      next.splice(adjustedTargetIndex + (position === "after" ? 1 : 0), 0, moving);
      return { ...current, workspaces: next };
    });
  }, []);

  const reorderSidebarConversation = useCallback((
    workspaceId: string,
    conversationId: string,
    targetConversationId: string,
    position: "before" | "after"
  ) => {
    if (modelRunController.current()[conversationId] || conversationId === targetConversationId) return;
    documentStore.update((current) => {
      if (!current) return current;
      const workspace = current.workspaces.find((item) => item.id === workspaceId);
      if (!workspace
        || !workspace.conversations.some((conversation) => conversation.id === conversationId)
        || !workspace.conversations.some((conversation) => conversation.id === targetConversationId)) return current;
      return {
        ...current,
        workspaces: current.workspaces.map((item) => item.id === workspaceId
          ? { ...item, conversations: reorderItems(item.conversations, conversationId, targetConversationId, position, (conversation) => conversation.id) }
          : item)
      };
    });
    const orderedIds = documentStore.current()?.workspaces
      .find((workspace) => workspace.id === workspaceId)?.conversations
      .map((conversation) => conversation.id);
    if (orderedIds) conversationSync.reordered(workspaceId, orderedIds);
  }, [conversationSync, documentStore, modelRunController]);

  const selectConversation = (workspaceId: string, conversationId: string) => {
    if (activeConversationId) {
      const scroller = window.document.querySelector<HTMLElement>('[data-main-context-stream="true"]');
      if (scroller) contextScrollRef.current[activeConversationId] = scroller.scrollTop;
    }
    setActiveWorkspaceId(workspaceId);
    setActiveConversationId(conversationId);
    // The draft, its composer text included, waits where it is: the next new task returns to it.
    setEditor(null);
    window.requestAnimationFrame(() => {
      const scroller = window.document.querySelector<HTMLElement>('[data-main-context-stream="true"]');
      if (scroller) scroller.scrollTop = contextScrollRef.current[conversationId] ?? scroller.scrollHeight;
    });
  };

  // Effects that switch the page (a handoff continuation) call the
  // current closure, which saves the scroll position of the page being left.
  const selectConversationRef = useRef(selectConversation);
  selectConversationRef.current = selectConversation;

  const renameConversation = useCallback((workspaceId: string, conversationId: string, title: string) => {
    updateConversation(workspaceId, conversationId, (conversation) => ({
      ...conversation,
      title,
      // A fork the user names stops following its origin's title.
      ...(conversation.forkOf ? { forkOf: null } : {}),
      updatedAt: new Date().toISOString()
    }));
    // A name the user chose is final: the local helper model must not replace it.
    settleConversationTitle(conversationId).catch((error) => console.error("Could not settle the conversation title", error));
  }, [updateConversation]);

  /**
   * Forks follow their origin's title however it changed — a rename here, the
   * first message's excerpt, the local helper model's title, a language switch.
   * A title is written once per change of target: a commit the host refuses
   * comes back as the old title, and writing the same target again would loop.
   */
  const forkTitleTargetsRef = useRef(new Map<string, string>());
  useEffect(() => {
    if (!document) return;
    for (const update of staleForkTitles(document)) {
      if (forkTitleTargetsRef.current.get(update.conversationId) === update.title) continue;
      forkTitleTargetsRef.current.set(update.conversationId, update.title);
      updateConversation(update.workspaceId, update.conversationId, (conversation) => ({
        ...conversation,
        title: update.title
      }));
    }
  }, [document, updateConversation]);

  /** The top bar's title while it is being renamed in place. */
  const [titleDraft, setTitleDraft] = useState<{ conversationId: string; value: string } | null>(null);
  const [conversationSearchOpen, setConversationSearchOpen] = useState(false);

  /** See `lib/conversationHistory.ts`. Only real conversations are recorded; the draft is not. */
  const [conversationHistory, setConversationHistory] = useState<ConversationHistory>(EMPTY_CONVERSATION_HISTORY);
  useEffect(() => {
    if (!activeWorkspaceId || !activeConversationId || isDraftConversationId(activeConversationId)) return;
    setConversationHistory((current) => visitConversation(current, {
      workspaceId: activeWorkspaceId,
      conversationId: activeConversationId
    }));
  }, [activeWorkspaceId, activeConversationId]);
  /** Where each listed conversation lives now; a history entry may have moved project since. */
  const listedConversationWorkspaces = useMemo(() => {
    const workspaceIds = new Map<string, string>();
    for (const workspace of document?.workspaces ?? []) {
      for (const conversation of visibleConversations(workspace.conversations)) {
        workspaceIds.set(conversation.id, workspace.id);
      }
    }
    return workspaceIds;
  }, [document?.workspaces]);
  const locateConversation = useCallback(
    (conversationId: string) => listedConversationWorkspaces.get(conversationId) ?? null,
    [listedConversationWorkspaces]
  );
  const historyBack = conversationHistoryTarget(conversationHistory, activeConversationId, -1, locateConversation);
  const historyForward = conversationHistoryTarget(conversationHistory, activeConversationId, 1, locateConversation);
  const stepConversationHistory = (target: typeof historyBack) => {
    if (!target) return;
    setConversationHistory((current) => ({ ...current, index: target.index }));
    selectConversation(target.workspaceId, target.conversationId);
  };
  const stepConversationHistoryRef = useRef<(step: -1 | 1) => void>(() => undefined);
  stepConversationHistoryRef.current = (step) => stepConversationHistory(step < 0 ? historyBack : historyForward);
  // A mouse's own back and forward buttons walk the same history.
  useEffect(() => {
    const onMouseUp = (event: MouseEvent) => {
      if (event.button !== 3 && event.button !== 4) return;
      event.preventDefault();
      stepConversationHistoryRef.current(event.button === 3 ? -1 : 1);
    };
    window.addEventListener("mouseup", onMouseUp);
    return () => window.removeEventListener("mouseup", onMouseUp);
  }, []);

  /**
   * Conversations whose run finished while another one was open. Their mark turns blue until
   * they are opened; this is the renderer's own notice, so a restart forgets it.
   */
  const [unseenCompletions, setUnseenCompletions] = useState<ReadonlySet<string>>(() => new Set());
  const runningConversationIdsRef = useRef<ReadonlySet<string>>(new Set());
  useEffect(() => {
    const running = new Set(Object.keys(modelRunSummaries).filter((id) => modelRunSummaries[id]));
    const finished = [...runningConversationIdsRef.current].filter((id) => (
      !running.has(id) && id !== activeConversationIdRef.current
    ));
    runningConversationIdsRef.current = running;
    if (!finished.length) return;
    setUnseenCompletions((current) => new Set([...current, ...finished]));
  }, [modelRunSummaries]);
  useEffect(() => {
    if (!activeConversationId) return;
    setUnseenCompletions((current) => {
      if (!current.has(activeConversationId)) return current;
      const next = new Set(current);
      next.delete(activeConversationId);
      return next;
    });
  }, [activeConversationId]);
  /** Conversations waiting on the user: an approval card, a question card, a fork request. */
  const blockedConversationIds = useMemo(() => {
    const blocked = new Set<string>();
    for (const [conversationId, prompts] of Object.entries(toolPrompts)) {
      if (prompts.length) blocked.add(conversationId);
    }
    for (const request of forkRequests) blocked.add(request.sourceConversationId);
    return blocked;
  }, [forkRequests, toolPrompts]);
  const conversationStatus = useCallback((conversationId: string): ConversationStatus => (
    blockedConversationIds.has(conversationId) ? "blocked"
      : conversationHasLiveActivity(conversationId) ? "running"
        : unseenCompletions.has(conversationId) ? "completed"
          : "idle"
  ), [blockedConversationIds, conversationHasLiveActivity, unseenCompletions]);

  /**
   * Drafts and persisted conversations must resolve settings identically so materialization preserves draft edits.
   * Workspace creation prefers an explicit workspace preset, then remembered settings, then the global default;
   * global new-task creation always uses the global default.
   */
  const resolveNewConversationSettings = useCallback((
    target: Workspace | null,
    source: NewConversationSource
  ): { settings: ConversationSettingsType; presetId: string } | null => {
    // Read from the store because the startup effect opens a draft in the same tick that it loads the document.
    const document = documentStore.current();
    if (!document) return null;
    const knownToolNames = new Set(document.tools.map((tool) => tool.name));
    const blankSettings: ConversationSettingsType = {
      includeAppDataPath: false,
      enabledTools: [],
      hookIds: [],
      skillIds: [],
      mcpIds: [],
      toolDescriptionFileId: null,
      agentDefinitions: [],
      // Presets applied below determine this option for a new conversation.
      allowRolelessSubagents: false,
      webSearch: defaultConversationWebSearchSettings(),
      // Off until a preset says otherwise, for the same reason the memory tiers
      // are: reaching the network is a capability a conversation is given, not
      // one it starts with.
      webSearchEnabled: false,
      reasoningEffort: document.globalSettings.lastReasoningEffort,
      // Security has no global default; presets or workspace snapshots supply it, and this is the safest base value.
      securityLevel: "request_approval",
      // Both tiers start off; the preset applied below is what actually decides
      // them for a fresh conversation.
      globalMemoryEnabled: false,
      projectMemoryEnabled: false,
      // Presets determine skill-on-demand loading; this conservative base preserves skill prose in the system prompt.
      skillToolEnabled: false,
      mcpToolDiscoveryEnabled: false
    };
    const workspacePreset = source === "workspace" && target
      ? conversationPresetById(document.globalSettings, target.defaultConversationPresetId)
      : null;
    const remembered = source === "workspace" && target && !workspacePreset
      ? target.lastConversationSettings
      : null;
    if (workspacePreset) {
      return {
        settings: applyConversationPresetSettings(blankSettings, workspacePreset.settings, knownToolNames),
        presetId: workspacePreset.id
      };
    }
    // A remembered snapshot is the workspace's own unnamed draft: it has no preset identity.
    // Plan mode is not remembered: it belongs to the task it was switched on for.
    if (remembered) {
      return {
        settings: { ...cloneConversationSettings(remembered, knownToolNames), planModeEnabled: false },
        presetId: ""
      };
    }
    const fallbackPreset = defaultConversationPreset(document.globalSettings);
    if (!fallbackPreset) return { settings: blankSettings, presetId: "" };
    return {
      settings: applyConversationPresetSettings(blankSettings, fallbackPreset.settings, knownToolNames),
      presetId: fallbackPreset.id
    };
  }, [documentStore]);

  const createConversation = useCallback((
    workspaceId?: string,
    source: NewConversationSource = "global",
    settingsOverride?: ConversationSettingsType,
    runTargetOverride?: RunTargetType | null,
    parentConversationId: string | null = null,
    // A materialized draft brings the content the user wrote before sending. Seeding it here rather
    // than writing it afterwards lets `conversationSync.created` carry it to the host in one piece.
    initialContexts: ContextItem[] = [],
    /** Preset the overridden settings came from; only a materialized draft supplies it. */
    presetIdOverride = "",
    /** Template whose queue the draft was showing; only a materialized draft supplies it. */
    templateIdOverride = "",
    /** Workspaces the draft was granted; only a materialized draft supplies them. */
    attachedWorkspacesOverride: AttachedWorkspace[] = [],
    /** The id a materialized draft was minted with, which its terminals are already open under. */
    conversationIdOverride?: string,
    /** A timeline fork's name and origin; see `lib/conversationForks.ts`. */
    fork?: { title: string; forkOf: ConversationForkOrigin },
    /**
     * The source's lock, for a branch or fork that carries history over: the
     * cache that history rides on is the source's, and it runs out at the same
     * moment in both (`toolLock.ts`).
     */
    inheritedToolLock?: ConversationToolLock
  ): string | null => {
    // The store, not the rendered snapshot: a workspace registered in this same
    // event — the directory the user just picked — is in the store already and
    // in the snapshot only after the next render, and a conversation created
    // against the stale snapshot would land in whatever workspace came first.
    const document = documentStore.current();
    if (!document) return null;
    const requestedId = workspaceId ?? activeWorkspaceId;
    const target = document.workspaces.find((workspace) => workspace.id === requestedId) ?? document.workspaces[0];
    if (!target) return null;
    if (deletingWorkspaceIdsRef.current.has(target.id)) return null;
    const knownToolNames = new Set(document.tools.map((tool) => tool.name));
    const targetId = target.id;
    const now = new Date().toISOString();
    const conversationId = conversationIdOverride ?? createId("conv");
    let created: Conversation | null = null;
    // Materializing a draft preserves its exact settings; other creation paths resolve them now.
    const resolved = settingsOverride
      ? { settings: cloneConversationSettings(settingsOverride, knownToolNames), presetId: presetIdOverride }
      : resolveNewConversationSettings(target, source);
    if (!resolved) return null;
    const resolvedSettings = inheritedToolLock
      ? { ...resolved.settings, toolLock: inheritedToolLock }
      : resolved.settings;
    documentStore.update((current) => {
      if (!current) return current;
      const conversation: Conversation = {
        id: conversationId,
        title: fork?.title ?? t("新任务", "New task"),
        ...(fork ? { forkOf: fork.forkOf } : {}),
        createdAt: now,
        updatedAt: now,
        settings: resolvedSettings,
        contexts: initialContexts,
        queuedMessages: [],
        branches: [],
        userAbortedTasks: [],
        worktrees: [],
        runTarget: runTargetOverride ?? null,
        attachedWorkspaces: attachedWorkspacesOverride,
        parentConversationId,
        presetId: resolved.presetId,
        // Only a materialized draft carries one: every other creation path has
        // never had a template applied to it.
        templateId: templateIdOverride
      };
      created = conversation;
      return {
        ...current,
        workspaces: current.workspaces.map((workspace) => workspace.id === targetId ? {
          ...workspace,
          // A new task becomes the workspace's remembered settings snapshot. A
          // branch or fork only copies an existing conversation's, which may be
          // older than what the user last chose for a new task.
          ...(parentConversationId ? {} : { lastConversationSettings: resolvedSettings }),
          conversations: [conversation, ...workspace.conversations]
        } : workspace)
      };
    });
    if (created) {
      const orderedIds = documentStore.current()?.workspaces
        .find((workspace) => workspace.id === targetId)?.conversations
        .map((conversation) => conversation.id) ?? [conversationId];
      conversationSync.created(targetId, created, orderedIds);
    }
    setActiveWorkspaceId(targetId);
    setActiveConversationId(conversationId);
    // A brand-new conversation starts on its own timeline; the per-conversation view state means
    // this only has to clear a stale entry left by a conversation id that was reused. A draft's
    // terminal tabs are already filed under the id it materializes as, and are its own.
    dispatchSidePanes({ type: "remove_conversation", conversationId });
    if (!conversationIdOverride) dispatchTerminalTabs({ type: "remove_conversation", conversationId });
    return conversationId;
  }, [activeWorkspaceId, dispatchSidePanes, dispatchTerminalTabs, documentStore, resolveNewConversationSettings, t]);

  /**
   * The dev servers the draft started, in every workspace of its project and on whichever machine
   * each is, which the host files under the id it will be sent as. A host that cannot be asked —
   * or a machine that cannot be reached — answers with none.
   */
  const draftPreviewServers = useCallback(async (
    draft: DraftConversationState
  ): Promise<PreviewServerSnapshot[]> => {
    if (!hasBackendRuntime()) return [];
    const workspace = documentStore.current()?.workspaces.find((candidate) => (
      candidate.id === draft.workspaceId
    ));
    if (!workspace || workspace.kind !== "directory") return [];
    const listed = await listPreviewServers({
      conversationId: draft.materializesAs,
      draftWorkspaceId: workspace.id
    }).catch((): PreviewServerSnapshot[] => []);
    return listed.filter((server) => server.sessionId === draft.materializesAs);
  }, [documentStore]);

  /**
   * Ends everything the draft opened against its project: its shells and dev servers are killed,
   * its preview page closed, and the panes that showed them put away along with the workspace
   * chip's selection and the Git snapshot, which described the old checkout; the new target's poll
   * reads the next one. The draft then gets a fresh id to open its next ones under, so nothing the
   * host still holds for the old project can be handed to the conversation it becomes in the new
   * one.
   *
   * Conversation settings is the one pane that stays: the draft's settings come along.
   */
  const releaseDraftSurfaces = useCallback((): {
    /** The id the draft's next shells and page are opened under. */
    materializesAs: string;
    /** Settles once the host has been asked to end each of them; never rejects. */
    released: Promise<void>;
  } | null => {
    const draft = draftConversationRef.current;
    if (!draft) return null;
    const ownerId = draft.materializesAs;
    const terminalIds = new Set([
      ...terminalTabsFor(terminalTabsStateRef.current, ownerId).tabs.map((tab) => tab.id),
      ...Object.values(terminalController.current())
        .filter((session) => session.conversationId === ownerId)
        .map((session) => session.terminalId)
    ]);
    const closing: Promise<void>[] = [...terminalIds].map((terminalId) => (
      requestTerminalSessionClose(ownerId, terminalId)
    ));
    dispatchTerminalTabs({ type: "remove_conversation", conversationId: ownerId });
    // Reads the project off this draft, before it is aimed anywhere else.
    closing.push(draftPreviewServers(draft).then(async (servers) => {
      await Promise.all(servers.map((server) => stopPreviewServer(server.handle).catch(() => false)));
    }));
    // Reads the roster, so it goes before the roster is cleared below.
    closing.push(requestConversationBrowserClose(DRAFT_CONVERSATION_ID));
    for (const pane of sidePaneLayoutFor(sidePanesStateRef.current, DRAFT_CONVERSATION_ID).panes) {
      if (paneKind(pane) === "settings") continue;
      dispatchSidePanes({ type: "close", conversationId: DRAFT_CONVERSATION_ID, pane });
    }
    for (const sessionId of previewSessionsFor(sidePanesStateRef.current, DRAFT_CONVERSATION_ID)) {
      dispatchSidePanes({ type: "forget_preview", conversationId: DRAFT_CONVERSATION_ID, sessionId });
    }
    const forget = <T,>(current: Record<string, T>) => {
      if (!(DRAFT_CONVERSATION_ID in current)) return current;
      const next = { ...current };
      delete next[DRAFT_CONVERSATION_ID];
      return next;
    };
    setSelectedWorkspaceMembers(forget);
    setReviewPageMembers(forget);
    setReviewPageOrders(forget);
    updateGitSnapshots((current) => {
      const drafts = Object.keys(current).filter((key) => (
        gitSnapshotKeyConversation(key) === DRAFT_CONVERSATION_ID
      ));
      if (drafts.length === 0) return current;
      const next = { ...current };
      for (const key of drafts) delete next[key];
      return next;
    });
    return {
      materializesAs: createId("conv"),
      released: Promise.allSettled(closing).then(() => undefined)
    };
  }, [
    dispatchSidePanes,
    dispatchTerminalTabs,
    draftPreviewServers,
    requestConversationBrowserClose,
    requestTerminalSessionClose,
    terminalController,
    updateGitSnapshots
  ]);

  /**
   * Aims the draft at another project, or at none, ending what it had open against the old one.
   * The result settles once the host has been asked to end each shell and page; it never rejects.
   */
  const retargetDraft = useCallback((workspaceId: string | null): Promise<void> => {
    const draft = draftConversationRef.current;
    if (!draft || draft.workspaceId === workspaceId) return Promise.resolve();
    const release = releaseDraftSurfaces();
    const next = { ...draft, workspaceId, materializesAs: release?.materializesAs ?? draft.materializesAs };
    // Handlers that run before the next render must already see the new project and id.
    draftConversationRef.current = next;
    setDraftConversation(next);
    return release?.released ?? Promise.resolve();
  }, [releaseDraftSurfaces]);

  /**
   * Aims the draft at another project, asking first when that ends shells or dev servers it still
   * has running. The renderer only sees the shells on screen, so the host is asked for the rest,
   * and for the servers. `proceed` runs after the draft has moved, and not at all if the user keeps
   * it where it is.
   */
  const requestDraftRetarget = useCallback(async (
    workspaceId: string | null,
    proceed: () => void = () => undefined
  ) => {
    const draft = draftConversationRef.current;
    if (!draft) return;
    if (draft.workspaceId === workspaceId) {
      proceed();
      return;
    }
    const ownerId = draft.materializesAs;
    const onScreen = Object.values(terminalController.current()).filter((session) => (
      session.conversationId === ownerId && terminalSessionIsLive(session)
    )).length;
    const [shells, servers] = await Promise.all([
      hasBackendRuntime() ? liveTerminalCount(ownerId).catch(() => onScreen) : onScreen,
      draftPreviewServers(draft).then((listed) => listed.length)
    ]);
    // The draft may have moved on, or become a conversation, while the host answered.
    if (draftConversationRef.current?.materializesAs !== ownerId) return;
    if (shells > 0 || servers > 0) {
      setDraftRetargetPrompt({ ownerId, workspaceId, shells, servers, proceed });
      return;
    }
    void retargetDraft(workspaceId);
    proceed();
  }, [draftPreviewServers, retargetDraft, terminalController]);

  /**
   * Open a new task: the one renderer-held draft, aimed at the requested project when there is one.
   * There is a single draft for the whole app rather than one per project, because its project is
   * still the user's to change; it becomes a real conversation, fixed in whichever project it then
   * names, only when something needs the host to know it — see {@link redeemDraft}.
   *
   * An unsent draft outlives a visit to another conversation, so a new task returns to it — text,
   * settings and all — aimed at the project the new task was asked for.
   */
  const openDraftConversation = useCallback((
    workspaceId?: string,
    source: NewConversationSource = "global"
  ) => {
    const current = documentStore.current();
    if (!current) return;
    const available = (id: string | null | undefined) => current.workspaces.find((workspace) => (
      workspace.id === id && !deletingWorkspaceIdsRef.current.has(workspace.id)
    )) ?? null;
    const target = available(workspaceId ?? activeWorkspaceIdRef.current);
    const existing = draftConversationRef.current;
    if (existing) {
      // With no project asked for, the draft keeps its own — unless that one has since gone.
      const aimedAt = target?.id ?? available(existing.workspaceId)?.id ?? null;
      const show = () => {
        setActiveWorkspaceId(aimedAt);
        setActiveConversationId(DRAFT_CONVERSATION_ID);
        setEditor(null);
      };
      if (existing.workspaceId !== null && !available(existing.workspaceId)) {
        // Its project went, and its shells with it; there is nothing left to ask about.
        void retargetDraft(aimedAt);
        show();
        return;
      }
      void requestDraftRetarget(aimedAt, show);
      return;
    }
    // A draft left unsent by an earlier run of the app comes back with the settings it had: it
    // copied its preset once, when it was opened, and has owned them since. Only a task that has
    // never been opened is made from a preset.
    const stored = current.globalSettings.draftConversation;
    const resolved = stored
      ? {
        settings: cloneConversationSettings(stored.settings, new Set(current.tools.map((tool) => tool.name))),
        presetId: stored.presetId
      }
      : resolveNewConversationSettings(target, source);
    if (!resolved) return;
    setDraftConversation({
      workspaceId: target?.id ?? null,
      materializesAs: createId("conv"),
      settings: resolved.settings,
      createdAt: new Date().toISOString(),
      worktreeMembers: [],
      runTarget: null,
      attachedWorkspaces: [],
      contexts: [],
      presetId: resolved.presetId,
      templateId: ""
    });
    setActiveWorkspaceId(target?.id ?? null);
    setActiveConversationId(DRAFT_CONVERSATION_ID);
    setEditor(null);
  }, [
    documentStore,
    requestDraftRetarget,
    resolveNewConversationSettings,
    retargetDraft,
    setActiveConversationId,
    setActiveWorkspaceId
  ]);
  openDraftConversationRef.current = openDraftConversation;

  /**
   * Move everything the renderer keyed by the draft's placeholder id onto the real conversation:
   * pane layout, composer text, staged images, the workspace chip's choice, and the Git snapshot.
   * A worktree is the one exception — creating it changes branch, HEAD, and modifications, so its
   * snapshot must be re-read rather than carried. Its terminals and preview page need nothing:
   * they are already keyed by the id it became.
   */
  const adoptDraftConversationId = useCallback((
    conversationId: string,
    options: { worktreeMembers?: readonly number[] } = {}
  ) => {
    dispatchSidePanes({
      type: "adopt_conversation", conversationId, from: DRAFT_CONVERSATION_ID
    });
    composerController.updateDrafts((current) => {
      const pending = current[DRAFT_CONVERSATION_ID];
      if (pending === undefined) return current;
      const next = { ...current, [conversationId]: pending };
      delete next[DRAFT_CONVERSATION_ID];
      return next;
    });
    composerController.updatePastedTexts((current) => {
      const pending = current[DRAFT_CONVERSATION_ID];
      if (!pending?.length) return current;
      const next = { ...current, [conversationId]: pending };
      delete next[DRAFT_CONVERSATION_ID];
      return next;
    });
    composerController.updateImageDrafts((current) => {
      const pending = current[DRAFT_CONVERSATION_ID];
      if (!pending?.length) return current;
      const next = { ...current, [conversationId]: pending };
      delete next[DRAFT_CONVERSATION_ID];
      return next;
    });
    composerController.updateFileDrafts((current) => {
      const pending = current[DRAFT_CONVERSATION_ID];
      if (!pending?.length) return current;
      const next = { ...current, [conversationId]: pending };
      delete next[DRAFT_CONVERSATION_ID];
      return next;
    });
    composerController.updateAttachmentNotices((current) => {
      const pending = current[DRAFT_CONVERSATION_ID];
      if (!pending?.length) return current;
      const next = { ...current, [conversationId]: pending };
      delete next[DRAFT_CONVERSATION_ID];
      return next;
    });
    composerController.updateElementPicks((current) => {
      const pending = current[DRAFT_CONVERSATION_ID];
      if (!pending?.length) return current;
      const next = { ...current, [conversationId]: pending };
      delete next[DRAFT_CONVERSATION_ID];
      return next;
    });
    // The workspace chip's choice is where the draft's terminals and Git surface were pointed,
    // and the review pane's page and page order are the reader's; all three carry over.
    const adopt = <T,>(current: Record<string, T>) => {
      if (!(DRAFT_CONVERSATION_ID in current)) return current;
      const next = { ...current, [conversationId]: current[DRAFT_CONVERSATION_ID]! };
      delete next[DRAFT_CONVERSATION_ID];
      return next;
    };
    setSelectedWorkspaceMembers(adopt);
    setReviewPageMembers(adopt);
    setReviewPageOrders(adopt);
    updateGitSnapshots((current) => {
      const next = gitSnapshotsAfterDraftRedemption(current, DRAFT_CONVERSATION_ID, conversationId);
      // A workspace the draft asked a worktree of moves to another checkout: the root's snapshot
      // would describe the wrong one until the next poll.
      const stale = (options.worktreeMembers ?? []).map((member) => gitSnapshotKey(conversationId, member));
      if (!stale.some((key) => next[key])) return next;
      const pruned = { ...next };
      for (const key of stale) delete pruned[key];
      return pruned;
    });
  }, [composerController, updateGitSnapshots]);

  /** Turn the draft into a conversation in the project it names, or the temporary one when it names none. */
  const materializeDraft = useCallback((): {
    conversationId: string;
    workspaceId: string;
    worktreeMembers: number[];
  } | null => {
    const draft = draftConversationRef.current;
    if (!draft) return null;
    const workspaceId = draft.workspaceId ?? TEMPORARY_WORKSPACE_ID;
    const created = createConversation(workspaceId, "global", draft.settings, draft.runTarget, null, draft.contexts, draft.presetId, draft.templateId, draft.attachedWorkspaces, draft.materializesAs);
    if (!created) return null;
    // The draft is this conversation now; clear it so it cannot regain the active view. The ref too:
    // a new task opened before the next render must not find it still waiting.
    draftConversationRef.current = null;
    setDraftConversation(null);
    // Its settings live on the conversation now, so there is no draft left to bring back.
    documentStore.update((current) => current?.globalSettings.draftConversation
      ? { ...current, globalSettings: { ...current.globalSettings, draftConversation: null } }
      : current);
    const project = documentStore.current()?.workspaces.find((workspace) => workspace.id === workspaceId);
    // Only a directory project's own workspaces can have worktrees; discard an unrealizable
    // request — a temporary project, a workspace removed since it was ticked — before it can
    // block sending.
    const worktreeMembers = project?.kind === "directory"
      ? draft.worktreeMembers.filter((member) => member >= 1 && member <= registeredProjectWorkspaces(project).length)
      : [];
    adoptDraftConversationId(created, { worktreeMembers });
    return {
      conversationId: created,
      workspaceId,
      worktreeMembers
    };
  }, [adoptDraftConversationId, createConversation, documentStore]);

  /**
   * Make the draft a conversation the host can act on, fixed from then on in the project it names.
   * This happens only when something needs the host to know it — the first send, or a tool the
   * user runs or records on its timeline — so until then the draft's project stays the user's to
   * change. A worktree the draft asked for is created here, before anything can run at the
   * workspace root in its place; failing to create one throws, with the conversation already real.
   *
   * `persist` waits until the host holds the conversation. A send skips it only because the send
   * persists the conversation together with its first message.
   */
  const redeemDraft = useCallback(async (
    { persist }: { persist: boolean }
  ): Promise<{ conversationId: string; workspaceId: string } | null> => {
    const redeemed = materializeDraft();
    if (!redeemed) return null;
    if (redeemed.worktreeMembers.length === 0) {
      if (persist) await flushLatestDocument({ durable: true });
      return redeemed;
    }
    // The host resolves a conversation only from its saved document, worktree requests included.
    await flushLatestDocument({ durable: true });
    const project = documentStore.current()?.workspaces.find((workspace) => workspace.id === redeemed.workspaceId);
    const registered = registeredProjectWorkspaces(project);
    // One after another: two worktrees of one repository must not race for a name.
    for (const member of redeemed.worktreeMembers) {
      const workspace = registered[member - 1];
      if (!workspace) continue;
      const worktree = await createConversationWorktree(redeemed.conversationId, member);
      updateConversation(
        redeemed.workspaceId,
        redeemed.conversationId,
        (conversation) => ({
          ...conversation,
          worktrees: withConversationWorktree(conversation.worktrees, member, workspace, worktree)
        })
      );
    }
    // The host also reads the persisted worktree records for trusted path resolution; without
    // them, the work would run at the workspace roots.
    await flushLatestDocument({ durable: true });
    // Refresh the Git snapshots after moving from the workspace roots to the worktree checkouts.
    for (const member of redeemed.worktreeMembers) {
      void refreshGitSnapshot(
        gitSnapshotKey(redeemed.conversationId, member),
        gitSurfaceKey(redeemed.workspaceId, member),
        gitConversationTarget(redeemed.conversationId, member)
      );
    }
    return redeemed;
  }, [documentStore, flushLatestDocument, materializeDraft, refreshGitSnapshot, updateConversation]);

  /* Conversation templates.
   *
   * The host owns template bodies, so this holds only the list the picker draws
   * and refreshes it after every mutation. Nothing here sends a body: saving
   * names a conversation to capture, and applying names a template to replay. */
  const [conversationTemplates, setConversationTemplates] = useState<ConversationTemplateSummary[]>([]);
  const [templateError, setTemplateError] = useState<string | null>(null);
  /* Kept apart from `templateError` so a failed skill delete is reported on the
     skills page rather than on a page the user is not looking at. */
  const [capabilityError, setCapabilityError] = useState<string | null>(null);
  const [templateSwitchPrompt, setTemplateSwitchPrompt] = useState<{ presetId: string } | null>(null);

  const refreshConversationTemplates = useCallback(async () => {
    try {
      setConversationTemplates(await listConversationTemplates());
    } catch (reason) {
      setTemplateError(failureMessage(reason, t("无法读取对话模板", "Could not read conversation templates")));
    }
  }, [t]);

  useEffect(() => {
    void refreshConversationTemplates();
  }, [refreshConversationTemplates]);

  /**
   * Applies a preset's settings and, when it has one, the message queue it opens
   * with — in that order, onto a conversation the host can already see.
   *
   * Settings alone are a draft's own business, so a preset without a queue
   * leaves a draft a draft. A queue is different: the host attests its copied
   * tool cards against the TARGET conversation id, so a draft is redeemed FIRST,
   * before either write, and is fixed in its project from then on. Once it is
   * real, both writes go through explicit ids rather than through
   * `updateActiveConversation`, whose target was read before the draft became real.
   */
  const applyPresetBody = useCallback(async (preset: ConversationPreset) => {
    setTemplateError(null);
    const latestDocument = documentStore.current();
    const knownToolNames = new Set(latestDocument?.tools.map((tool) => tool.name) ?? []);
    const { provider: lockProvider, model: lockModelProfile } = latestDocument
      ? modelChoiceForConversation(latestDocument)
      : { provider: undefined, model: undefined };
    const lockModel = toolLockModelOf(lockProvider, lockModelProfile);
    /* A model that cannot take a tool mid-conversation keeps the surface its
       last request had, preset or no preset. */
    const withPreset = (conversation: Conversation): Conversation => {
      const applied = applyConversationPresetSettings(conversation.settings, preset.settings, knownToolNames);
      const state = toolLockState(applied, lockModel, Date.now());
      return {
        ...conversation,
        settings: state.hard ? restoreLockedSettings(applied, state) : applied,
        presetId: preset.id
      };
    };
    let workspaceId = activeWorkspaceId;
    let conversationId = activeConversationId;
    if (isDraftConversationId(conversationId)) {
      if (!preset.templateId) {
        updateActiveConversation(withPreset);
        return;
      }
      try {
        const redeemed = await redeemDraft({ persist: true });
        if (!redeemed) return;
        ({ workspaceId, conversationId } = redeemed);
      } catch (reason) {
        setTemplateError(failureMessage(reason, t("无法套用对话模板", "Could not apply the conversation template")));
        return;
      }
    }
    if (!workspaceId || !conversationId) return;
    updateConversation(workspaceId, conversationId, withPreset);
    if (!preset.templateId) return;
    try {
      const contexts = await applyConversationTemplate({
        workspaceId,
        templateId: preset.templateId,
        targetConversationId: conversationId
      });
      updateConversation(workspaceId, conversationId, (conversation) => ({
        ...conversation,
        contexts,
        templateId: preset.templateId
      }));
      // The timeline is a different one now; nothing on record was done to it.
      timelineHistoryRef.current!.forget(conversationId);
    } catch (reason) {
      setTemplateError(failureMessage(reason, t("无法套用对话模板", "Could not apply the conversation template")));
    }
  }, [
    activeConversationId,
    activeWorkspaceId,
    documentStore,
    modelChoiceForConversation,
    redeemDraft,
    t,
    updateActiveConversation,
    updateConversation
  ]);

  /**
   * Whether laying a preset's queue over this timeline has to ask first.
   *
   * Only a timeline that is the user's own work is at risk. An empty one has
   * nothing to lose, and one whose length still matches the applied template's
   * is being swapped for another preset's queue rather than overwritten.
   *
   * The length comparison is deliberately loose: editing a message in place
   * keeps the count, so that case does not prompt. That is the cheap direction
   * to be wrong in — the dialog only asks, and asking about a template's own
   * queue on every switch would train the user to dismiss it unread. Adding or
   * deleting a message, which is what actually makes a timeline the user's own,
   * does change the count and does prompt.
   */
  const templateSwitchNeedsConfirmation = useCallback((): boolean => {
    if (!activeConversation || conversationHasNoContexts(activeConversation)) return false;
    const applied = conversationTemplates.find(
      (template) => template.id === activeConversation.templateId
    );
    return !applied || applied.messageCount !== activeConversation.contexts.length;
  }, [activeConversation, conversationTemplates]);

  /**
   * Applying a preset copies its values and records which preset they came from.
   *
   * A preset that opens with a message queue also replaces the timeline, which
   * is the one destructive half of this — so it is the half that asks, and only
   * when there is work of the user's own to lose. A preset with no queue never
   * touches the timeline and so never asks.
   */
  const applyPresetToActiveConversation = useCallback((presetId: string) => {
    const current = documentStore.current();
    if (!current || !activeConversation) return;
    const preset = conversationPresetById(current.globalSettings, presetId);
    if (!preset) return;
    if (preset.templateId && templateSwitchNeedsConfirmation()) {
      setTemplateSwitchPrompt({ presetId: preset.id });
      return;
    }
    void applyPresetBody(preset);
  }, [
    activeConversation,
    applyPresetBody,
    documentStore,
    templateSwitchNeedsConfirmation
  ]);

  /**
   * Reads a template body for an editor to work on.
   *
   * An id nothing has been written under is an empty body, not a failure: that
   * is every preset and every role before its first save.
   */
  const readTemplateBody = useCallback(async (templateId: string): Promise<ContextItem[]> => {
    if (!templateId) return [];
    return previewConversationTemplate(templateId);
  }, []);

  /**
   * Writes a template body, minting the id when its owner has none yet, and
   * resolves with the id it landed under so the owner can record it.
   *
   * The host normalizes every tool card it takes, so nothing authored in an
   * editor comes back carrying a claim the application did not make. Failures
   * propagate: the editor that asked is the only surface that can say what went
   * wrong about the body the user is looking at.
   */
  const writeTemplateBody = useCallback(async (
    templateId: string,
    contexts: ContextItem[]
  ): Promise<string> => {
    const id = templateId || createId("template");
    await updateConversationTemplate(id, contexts);
    await refreshConversationTemplates();
    return id;
  }, [refreshConversationTemplates]);

  /**
   * Removes a skill, an MCP server or a hook from the place it is configured, from
   * the conversation-settings pane that lists it.
   *
   * All three are files the user owns — a `skills/<dir>` folder, a key in an
   * `mcp.json`, a line in a `hooks.json` — so only the host can remove one, and
   * the renderer names the entry by its catalog id rather than describing it.
   * Afterwards the catalog is rescanned rather than patched: every id is a hash
   * over the entry's position on disk, so a delete moves the neighbours' meaning
   * and only a fresh scan knows what is left.
   */
  const deleteCapabilityResource = useCallback(async (
    kind: "skills" | "mcp" | "hooks",
    resource: ResourceDescriptor
  ) => {
    setCapabilityError(null);
    try {
      if (kind === "hooks") await deleteHook(resource.id);
      else if (kind === "skills") await deleteSkill(resource.id);
      else await deleteMcpServer(resource.id);
      await rescanCapabilities();
    } catch (reason) {
      setCapabilityError(failureMessage(reason, t("无法删除这一项", "Could not delete this entry")));
    }
  }, [rescanCapabilities, t]);

  /** Surfaces a failed rescan on the page that asked for it rather than dropping it. */
  const rescanCapabilitiesFromPane = useCallback(async () => {
    setCapabilityError(null);
    try {
      await rescanCapabilities();
    } catch (reason) {
      setCapabilityError(failureMessage(reason, t("无法扫描配置目录", "Could not scan the configuration directories")));
    }
  }, [rescanCapabilities, t]);

  /**
   * Opens the directory a capability kind is configured in. The host creates it
   * when it does not exist yet, so a first-time user lands somewhere rather than
   * nowhere.
   */
  const revealCapabilityDirectory = useCallback(async (
    kind: "skills" | "mcp" | "hooks",
    workspaceId: string | null
  ) => {
    setCapabilityError(null);
    try {
      await revealCapabilityLocation(kind, workspaceId ?? undefined);
    } catch (reason) {
      setCapabilityError(failureMessage(reason, t("无法打开配置目录", "Could not open the configuration directory")));
    }
  }, [t]);

  /**
   * Dials one discovered MCP server. The host looks the server up in a fresh scan
   * by id, so nothing executable leaves the renderer; the probe id only has to be
   * unique among live probes.
   */
  const probeCapabilityMcpServer = useCallback(
    (resource: ResourceDescriptor) => probeMcpServer(resource.id),
    []
  );

  /** Renames a saved preset. Conversations cite it by id, so the trace follows. */
  const renameConversationPreset = useCallback((presetId: string, name: string) => {
    if (!name.trim() || isBuiltinConversationPreset(presetId)) return;
    handleGlobalSettingsChange((current) => ({
      ...current,
      conversationPresets: current.conversationPresets.map((preset) => (
        preset.id === presetId ? { ...preset, name: name.trim() } : preset
      ))
    }));
  }, [handleGlobalSettingsChange]);

  /* Deleting leaves every citing conversation's `presetId` dangling, which reads
   * as an unnamed draft everywhere it is resolved — the same treatment a deleted
   * template gets. The default, though, is a real setting and cannot dangle, so
   * it moves to whatever preset is left.
   *
   * The body the preset opened with goes too. Nothing else cites it — a template
   * belongs to exactly one owner now — so leaving it would be leaving a row no
   * surface can ever reach again. */
  const deleteConversationPreset = useCallback((presetId: string) => {
    // The host would put it back on save anyway; not asking is the honest answer.
    if (isBuiltinConversationPreset(presetId)) return;
    const doomed = documentStore.current()?.globalSettings.conversationPresets.find(
      (preset) => preset.id === presetId
    );
    handleGlobalSettingsChange((current) => {
      const conversationPresets = current.conversationPresets.filter(
        (preset) => preset.id !== presetId
      );
      return {
        ...current,
        conversationPresets,
        defaultConversationPresetId: current.defaultConversationPresetId === presetId
          ? conversationPresets[0]?.id ?? ""
          : current.defaultConversationPresetId
      };
    });
    // Best effort, and deliberately after the document change: losing the body
    // is recoverable nowhere, but so is keeping it, and the preset is gone from
    // the user's view either way.
    if (doomed?.templateId) {
      void deleteConversationTemplate(doomed.templateId)
        .then(refreshConversationTemplates)
        .catch(() => {});
    }
  }, [documentStore, handleGlobalSettingsChange, refreshConversationTemplates]);

  /** Writes an edited body onto a saved preset. Conversations already stamped
   * from it keep what they were given; a preset never reaches back into one. */
  const saveConversationPreset = useCallback((
    presetId: string,
    settings: ConversationPresetSettings
  ) => {
    if (isBuiltinConversationPreset(presetId)) return;
    handleGlobalSettingsChange((current) => ({
      ...current,
      conversationPresets: current.conversationPresets.map((preset) => (
        preset.id === presetId ? { ...preset, settings } : preset
      ))
    }));
  }, [handleGlobalSettingsChange]);

  /* Saves what the user made of a preset they cannot change — the built-in one —
   * as a new preset of their own, named after it. The copy opens with the
   * template as it was edited in the window, or with the built-in's own when it
   * was not: a copy of a preset is a copy of what it opens with too. The body is
   * written under a fresh id, since the built-in's own cannot be shared — the
   * host rewrites it on every start. */
  const saveConversationPresetCopy = useCallback(async (
    presetId: string,
    settings: ConversationPresetSettings,
    templateBody: ContextItem[] | null
  ) => {
    setTemplateError(null);
    const source = documentStore.current()?.globalSettings.conversationPresets
      .find((preset) => preset.id === presetId);
    let templateId = "";
    try {
      const body = templateBody ?? (source?.templateId ? await previewConversationTemplate(source.templateId) : []);
      if (body.length) {
        const id = createId("template");
        await updateConversationTemplate(id, body);
        templateId = id;
        await refreshConversationTemplates();
      }
    } catch (reason) {
      // The settings are still worth keeping; the copy opens with nothing, and says why.
      setTemplateError(failureMessage(reason, t("无法复制对话模板", "Could not copy the conversation template")));
    }
    handleGlobalSettingsChange((current) => ({
      ...current,
      conversationPresets: [...current.conversationPresets, {
        id: createId("preset"),
        name: t("{name} 副本", "{name} copy", { name: source?.name ?? "" }).trim(),
        description: "",
        templateId,
        settings
      }]
    }));
  }, [documentStore, handleGlobalSettingsChange, refreshConversationTemplates, t]);

  /**
   * Saves the active conversation's settings as a new preset, and with them, when
   * asked, its timeline as the preset's template.
   *
   * The timeline is taken by the host from its own store
   * (`captureConversationTemplate`), so the tool cards in it keep the results
   * this application really produced; the document is flushed first, so the
   * host's copy is the one on screen. A draft has no row there yet, and nothing
   * but prose in it either — a tool card is what makes a draft real — so its
   * timeline is written as a body like any edited template. The template is
   * stored before the preset is: a preset citing a template that failed to land
   * would open with nothing and not say why.
   */
  const createConversationPreset = useCallback(async ({ name, captureTemplate }: {
    name: string;
    captureTemplate: boolean;
  }): Promise<ConversationPreset> => {
    const conversation = activeConversation;
    if (!conversation) throw new Error(t("没有活动对话", "No active conversation"));
    let templateId = "";
    if (captureTemplate && conversation.contexts.length) {
      templateId = createId("template");
      if (isDraftConversationId(conversation.id)) {
        await updateConversationTemplate(templateId, conversation.contexts);
      } else {
        const workspaceId = activeWorkspaceIdRef.current;
        if (!workspaceId) throw new Error(t("没有活动对话", "No active conversation"));
        await flushLatestDocument();
        await captureConversationTemplate({ workspaceId, conversationId: conversation.id, templateId });
      }
      await refreshConversationTemplates();
    }
    const preset: ConversationPreset = {
      id: createId("preset"),
      name,
      description: "",
      templateId,
      settings: captureConversationPresetSettings(conversation.settings)
    };
    handleGlobalSettingsChange((current) => ({
      ...current,
      conversationPresets: [...current.conversationPresets, preset]
    }));
    return preset;
  }, [activeConversation, flushLatestDocument, handleGlobalSettingsChange, refreshConversationTemplates, t]);

  /* Records which template a preset opens with, the moment its body is written.
   * It is deliberately not part of `saveConversationPreset`: the body is already
   * on disk by then, and waiting for the preset dialog's own Save would leave a
   * window in which closing that dialog stranded a body nothing cites. */
  const bindPresetTemplate = useCallback((presetId: string, templateId: string) => {
    if (isBuiltinConversationPreset(presetId)) return;
    handleGlobalSettingsChange((current) => ({
      ...current,
      conversationPresets: current.conversationPresets.map((preset) => (
        preset.id === presetId ? { ...preset, templateId } : preset
      ))
    }));
  }, [handleGlobalSettingsChange]);

  /**
   * Point the draft at another project, or at none. The draft stays a draft — settings, composer
   * text and hand-written content all come along — because nothing the host would have to know
   * about has happened in it yet; {@link redeemDraft} fixes it in a project once something has.
   */
  const setDraftWorkspace = useCallback((workspaceId: string | null) => {
    if (!draftConversationRef.current || !isDraftConversationId(activeConversationIdRef.current)) return;
    if (workspaceId !== null && !documentStore.current()?.workspaces.some(
      (workspace) => workspace.id === workspaceId
    )) return;
    void requestDraftRetarget(workspaceId, () => {
      // Only while the draft is still what is on screen.
      if (isDraftConversationId(activeConversationIdRef.current)) setActiveWorkspaceId(workspaceId);
    });
  }, [documentStore, requestDraftRetarget, setActiveWorkspaceId]);

  /** Set or clear the workspace default preset; it affects workspace-plus creation only, not global new-task creation. */
  const setWorkspaceDefaultPreset = useCallback((workspaceId: string, presetId: string) => {
    const current = documentStore.current();
    const workspace = current?.workspaces.find((item) => item.id === workspaceId);
    if (!current || !workspace) return;
    documentStore.update((latest) => latest ? {
      ...latest,
      workspaces: latest.workspaces.map((item) => item.id === workspaceId
        ? { ...item, defaultConversationPresetId: presetId }
        : item)
    } : latest);
  }, []);

  /** Only real presets: with none defined, the workspace menu has nothing to follow and says so. */
  const workspacePresetOptions = useMemo(
    () => document?.globalSettings.conversationPresets.map(
      (preset) => ({ id: preset.id, name: preset.name })
    ) ?? [],
    [document]
  );

  /**
   * Shortcut definitions and defaults are code constants; documents store user overrides only.
   * Refresh the action table through a ref rather than rebinding the listener every render, and
   * avoid dependency evaluation of handlers declared below this point.
   */
  const shortcutActionsRef = useRef<Partial<Record<ShortcutCommandId, () => void>>>({});
  useEffect(() => {
    const stepConversation = (delta: number) => {
      const conversations = activeWorkspace?.conversations ?? [];
      if (conversations.length < 2 || !activeConversationId) return;
      const index = conversations.findIndex((conversation) => conversation.id === activeConversationId);
      if (index < 0) return;
      // Wrap rather than stop at either end because these commands move to the next conversation.
      const next = conversations[(index + delta + conversations.length) % conversations.length];
      if (next && activeWorkspace) selectConversation(activeWorkspace.id, next.id);
    };
    const adjustZoom = (delta: number) => handleGlobalSettingsChange((current) => ({
      ...current,
      appearance: { ...current.appearance, zoom: clampZoom(current.appearance.zoom + delta) }
    }));
    const lastContextOfKind = (kind: "user" | "assistant") => {
      const contexts = activeConversation?.contexts ?? [];
      for (let index = contexts.length - 1; index >= 0; index -= 1) {
        const context = contexts[index];
        if (context.kind === kind) return context;
      }
      return null;
    };
    shortcutActionsRef.current = {
      "app.settings.open": () => openGlobalSettings("providers"),
      "app.conversation_settings.open": () => {
        setGlobalSettingsView(null);
        openConversationSettings();
      },
      "app.zoom.in": () => adjustZoom(ZOOM_STEP),
      "app.zoom.out": () => adjustZoom(-ZOOM_STEP),
      "app.zoom.reset": () => handleGlobalSettingsChange((current) => ({
        ...current,
        appearance: { ...current.appearance, zoom: 1 }
      })),
      "conversation.create": () => { openDraftConversationRef.current(); },
      "conversation.next": () => stepConversation(1),
      "conversation.previous": () => stepConversation(-1),
      "conversation.stop": () => {
        if (activeConversationId) void stopModelRun(activeConversationId);
      },
      "message.copy_last": () => {
        const last = lastContextOfKind("assistant");
        if (last && "content" in last && last.content) void navigator.clipboard?.writeText(last.content);
      },
      "message.edit_last_user": () => {
        const last = lastContextOfKind("user");
        if (last) handleContextEdit(last);
      },
      /**
       * Toggles this conversation's preview pane: on the page last seen there, or, with no page
       * yet, on the start page of the workspace the composer's chip has selected.
       */
      "panel.browser.toggle": () => {
        if (openPreviewSessionId) {
          closePane(previewPaneId(openPreviewSessionId));
          return;
        }
        showPreviewPanel();
      },
      "panel.close": () => closeLastPane()
    };
  });

  const shortcutBindings = document?.globalSettings.shortcuts;
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      // IME composition keystrokes are not shortcuts.
      if (isImeKeyEvent(event)) return;
      for (const command of SHORTCUT_COMMANDS) {
        const preference = resolveShortcut(shortcutBindings ?? {}, command);
        if (!preference.enabled || !preference.binding.length) continue;
        if (!matchesEvent(preference.binding, event)) continue;
        // In focused inputs, unmodified combinations yield to typing.
        if (shouldSuppressForFocus(preference.binding, event.target)) continue;
        const action = shortcutActionsRef.current[command.id];
        if (!action) continue;
        event.preventDefault();
        action();
        return;
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [shortcutBindings]);

  /**
   * Registers a project — one or more workspaces, the first being its own directory — or reuses
   * the project already registered with exactly those workspaces. Each workspace's identity is
   * its machine and its path together — one machine's `/srv/app` is not another's — and a POSIX
   * path keeps its case, since `normalizedWorkspacePath` folds only Windows spellings.
   */
  const addWorkspace = async (
    name: string,
    workspaces: AttachedWorkspace[],
    assignConversation = assignWorkspaceAfterAdd
  ) => {
    const current = documentStore.current();
    if (!current) return;
    const [first, ...members] = workspaces;
    if (!first) return;
    const path = first.path;
    const machine = first.machine ?? null;
    // A remote directory is POSIX and may begin `~`; only a directory on this machine is held
    // to this machine's idea of an absolute path.
    if (!machine && !isAbsoluteWorkspacePath(path)) return;
    const normalized = normalizedWorkspacePath(path);
    const sameMembers = (workspace: Workspace) => {
      const existingMembers = workspace.additionalWorkspaces ?? [];
      return existingMembers.length === members.length
        && existingMembers.every((entry, index) => (
          sameMachine(entry.machine, members[index].machine)
          && normalizedWorkspacePath(entry.path) === normalizedWorkspacePath(members[index].path)
        ));
    };
    const sourceWorkspaceId = activeWorkspaceIdRef.current;
    const movingConversationId = activeConversationIdRef.current;
    const sourceWorkspace = current.workspaces.find((workspace) => workspace.id === sourceWorkspaceId);
    const movingConversation = sourceWorkspace?.conversations.find((conversation) => (
      conversation.id === movingConversationId
    ));
    const existing = current.workspaces.find((workspace) => (
      workspace.kind === "directory"
      && sameMachine(workspace.machine, machine)
      && normalizedWorkspacePath(workspace.path) === normalized
      && sameMembers(workspace)
    ));
    // Drafts have no stored ownership; assign their workspace id directly when it exists, or after
    // creation. Only the draft on screen: one waiting behind another conversation is not the one
    // the user is choosing a project for.
    const assigningDraft = assignConversation
      && Boolean(draftConversationRef.current)
      && isDraftConversationId(movingConversationId);
    if (assigningDraft && existing) {
      setDraftWorkspace(existing.id);
      setWorkspaceDialogOpen(false);
      setAssignWorkspaceAfterAdd(false);
      return;
    }
    if (
      assignConversation
      && sourceWorkspaceId
      && movingConversationId
      && conversationMoveIsBlocked(
        movingConversationId,
        sourceWorkspaceId,
        existing?.id ?? sourceWorkspaceId
      )
    ) return;
    if (existing && assignConversation) {
      await moveActiveConversation(existing.id);
      setWorkspaceDialogOpen(false);
      setAssignWorkspaceAfterAdd(false);
      return;
    }
    if (existing) return;
    const workspace: Workspace = {
      id: createId("ws"),
      name: name.trim()
        || path.trim().split(/[\\/]/).filter(Boolean).at(-1)
        || t("新项目", "New project"),
      kind: "directory",
      path: path.trim(),
      ...(machine ? { machine } : {}),
      ...(members.length ? {
        additionalWorkspaces: members.map((member) => (
          member.machine ? { machine: member.machine, path: member.path.trim() } : { path: member.path.trim() }
        ))
      } : {}),
      createdAt: new Date().toISOString(),
      // A new workspace has no history, so leave its default preset empty until first creation records a snapshot.
      defaultConversationPresetId: "",
      lastConversationSettings: null,
      conversations: []
    };
    const next = { ...current, workspaces: [...current.workspaces, workspace] };
    documentStore.update(() => next);
    /* Everything below asks the host about the new project by id — the capability scan, and the
     * draft aimed at it the moment it is: its Git status, its dev servers, its file pane — and the
     * host answers only for a project its document holds. Left to the debounced save, the project
     * reaches the host after all of them, each is refused, and Git only asks again once its
     * backoff runs out. A save that fails is the save status's to report; the draft still opens. */
    await awaitWorkspaceAtHost(documentStore, workspace.id).catch(() => undefined);
    /* A new project adds a whole configuration level: its first directory's `.mework`
     * may already hold skills, MCP servers and hooks that nothing has scanned
     * yet. A failure here only leaves the catalog as stale as it already was.
     * A directory on another machine is not scanned: the host reads capability
     * files from its own filesystem only. */
    if (!machine) void rescanCapabilities().catch(() => {});
    if (assigningDraft) {
      setDraftWorkspace(workspace.id);
      setWorkspaceDialogOpen(false);
      setAssignWorkspaceAfterAdd(false);
      return;
    }
    const shouldAssign = Boolean(
      assignConversation
      && sourceWorkspaceId
      && movingConversationId
      && movingConversation
      && conversationHasNoContexts(movingConversation)
    );
    if (shouldAssign) {
      await moveActiveConversation(workspace.id);
    } else {
      // A new workspace has no conversation, so open a draft rather than an empty selection.
      openDraftConversationRef.current(workspace.id);
    }
    setWorkspaceDialogOpen(false);
    setAssignWorkspaceAfterAdd(false);
  };


  /**
   * Changes an existing project's name and the workspaces after its first. The first workspace
   * is the project's identity — the files pane, capability files and the worktree records from
   * before every workspace could have one hang off it — so the dialog never offers to change it
   * and this never does. A further workspace removed here takes its conversations' worktrees of
   * it out of use: they name the workspace they came from, never a position.
   */
  const updateProject = (projectId: string, name: string, workspaces: AttachedWorkspace[]) => {
    documentStore.update((current) => {
      if (!current) return current;
      return {
        ...current,
        workspaces: current.workspaces.map((workspace) => {
          if (workspace.id !== projectId || workspace.kind !== "directory") return workspace;
          const members = workspaces.slice(1).map((member): AttachedWorkspace => (
            member.machine ? { machine: member.machine, path: member.path.trim() } : { path: member.path.trim() }
          ));
          const { additionalWorkspaces: _previous, ...rest } = workspace;
          return {
            ...rest,
            name: name.trim() || workspace.name,
            ...(members.length ? { additionalWorkspaces: members } : {})
          };
        })
      };
    });
    setProjectEditor(null);
  };

  const deleteConversation = async (conversation: Conversation, workspace: Workspace) => {
    if (
      conversationIsBusy(conversation.id)
      || deletingConversationIdsRef.current.has(conversation.id)
    ) return;
    deletingConversationIdsRef.current.add(conversation.id);
    setDeletingConversationIds((current) => new Set(current).add(conversation.id));
    try {
      const terminalSessions = Object.values(terminalController.current())
        .filter((session) => session.conversationId === conversation.id);
      const closeResults = await Promise.allSettled([
        requestConversationBrowserClose(conversation.id),
        ...terminalSessions.map((session) => requestTerminalSessionClose(
          conversation.id,
          session.terminalId
        ))
      ]);
      if (closeResults.some((result) => result.status === "rejected")) return;
      const latest = documentStore.current();
      const latestWorkspace = latest?.workspaces.find((item) => item.id === workspace.id);
      const latestConversation = latestWorkspace?.conversations.find((item) => item.id === conversation.id);
      if (
        !latest
        || !latestWorkspace
        || !latestConversation
        || conversationIsBusy(conversation.id)
      ) return;
      // Children move up to the deleted conversation's parent, mirroring what
      // the host store does on delete; the host never re-sends them, so the
      // renderer's copy has to make the same move itself.
      const remaining = reparentChildren(latestWorkspace.conversations, conversation.id)
        .filter((item) => item.id !== conversation.id);
      const next = {
        ...latest,
        workspaces: latest.workspaces.map((item) => item.id === latestWorkspace.id
          ? { ...item, conversations: remaining }
          : item)
      };
      invalidateComposerImages([conversation.id]);
      composerController.forgetQueuedMessages(
        latestConversation.queuedMessages.map((message) => message.id)
      );
      documentStore.update(() => next);
      conversationSync.deleted(latestWorkspace.id, conversation.id);
      dispatchSidePanes({ type: "remove_conversation", conversationId: conversation.id });
      dispatchTerminalTabs({ type: "remove_conversation", conversationId: conversation.id });
      timelineHistoryRef.current!.forget(conversation.id);
      composerController.updateDrafts((current) => {
        const next = { ...current };
        delete next[conversation.id];
        return next;
      });
      composerController.updatePastedTexts((current) => {
        const next = { ...current };
        delete next[conversation.id];
        return next;
      });
      setModelRunErrors((current) => {
        const next = { ...current };
        delete next[conversation.id];
        return next;
      });
      if (
        activeWorkspaceIdRef.current === latestWorkspace.id
        && activeConversationIdRef.current === conversation.id
      ) {
        // After deleting the active conversation, open a draft in the same workspace rather than an unrelated list neighbor.
        openDraftConversationRef.current(latestWorkspace.id, "workspace");
        setEditor(null);
      }
    } finally {
      deletingConversationIdsRef.current.delete(conversation.id);
      setDeletingConversationIds((current) => {
        if (!current.has(conversation.id)) return current;
        const next = new Set(current);
        next.delete(conversation.id);
        return next;
      });
    }
  };

  const deleteWorkspace = async (workspace: Workspace) => {
    if (isReservedWorkspace(workspace)) return;
    const currentWorkspace = () => (
      documentStore.current()?.workspaces.find((candidate) => candidate.id === workspace.id) ?? null
    );
    const workspaceHasActiveConversation = () => Boolean(currentWorkspace()?.conversations.some((conversation) => (
      Boolean(modelRunController.current()[conversation.id])
      || conversationOperationIsActive(conversation.id)
      || deletingConversationIdsRef.current.has(conversation.id)
    )));
    if (
      !currentWorkspace()
      || deletingWorkspaceIdsRef.current.has(workspace.id)
      || workspaceHasActiveConversation()
    ) return;
    deletingWorkspaceIdsRef.current.add(workspace.id);
    setDeletingWorkspaceIds((current) => new Set(current).add(workspace.id));
    try {
      const workspaceBeforeClose = currentWorkspace();
      if (!workspaceBeforeClose || workspaceHasActiveConversation()) return;
      const conversationIdsBeforeClose = workspaceBeforeClose.conversations.map((
        conversation
      ) => conversation.id);
      const workspaceConversationIds = new Set(conversationIdsBeforeClose);
      const terminalSessions = Object.values(terminalController.current())
        .filter((session) => workspaceConversationIds.has(session.conversationId));
      // The draft aimed here loses its project with everyone else: its shells and page end, and it
      // is aimed at none. A running command of its own would otherwise hold up the deletion.
      const draftReleased = draftConversationRef.current?.workspaceId === workspace.id
        ? retargetDraft(null)
        : Promise.resolve();
      const [browserResults, terminalResults] = await Promise.all([
        Promise.allSettled(conversationIdsBeforeClose.map((conversationId) => (
          requestConversationBrowserClose(conversationId)
        ))),
        Promise.allSettled(terminalSessions.map((session) => (
          requestTerminalSessionClose(session.conversationId, session.terminalId)
        ))),
        draftReleased
      ]);
      if (
        browserResults.some((result) => result.status === "rejected")
        || terminalResults.some((result) => result.status === "rejected")
        || workspaceHasActiveConversation()
      ) return;
      const workspaceAfterClose = currentWorkspace();
      if (
        !workspaceAfterClose
        || workspaceAfterClose.conversations.length !== conversationIdsBeforeClose.length
        || workspaceAfterClose.conversations.some((
          conversation,
          index
        ) => conversation.id !== conversationIdsBeforeClose[index])
      ) return;
      const latest = documentStore.current();
      const removedWorkspace = latest?.workspaces.find((item) => item.id === workspace.id);
      if (!latest || !removedWorkspace || workspaceHasActiveConversation()) return;
      const fallback = latest?.workspaces.find((item) => item.id !== workspace.id) ?? null;
      const removedConversationIds = new Set(
        removedWorkspace.conversations.map((conversation) => conversation.id)
      );
      const next = {
        ...latest,
        workspaces: latest.workspaces.filter((item) => item.id !== workspace.id)
      };
      invalidateComposerImages(removedConversationIds);
      composerController.forgetQueuedMessages(
        removedWorkspace.conversations.flatMap((conversation) => (
          conversation.queuedMessages.map((message) => message.id)
        ))
      );
      documentStore.update(() => next);
      composerController.updateDrafts((current) => Object.fromEntries(
        Object.entries(current).filter(([conversationId]) => !removedConversationIds.has(conversationId))
      ));
      composerController.updatePastedTexts((current) => Object.fromEntries(
        Object.entries(current).filter(([conversationId]) => !removedConversationIds.has(conversationId))
      ));
      setModelRunErrors((current) => Object.fromEntries(
        Object.entries(current).filter(([conversationId]) => !removedConversationIds.has(conversationId))
      ));
      const removedBrowserSession = (sessionId: string) => [...removedConversationIds].some(
        (conversationId) => previewSessionBelongsToConversation(sessionId, conversationId)
      );
      browserController.updateStatuses((current) => Object.fromEntries(
        Object.entries(current).filter(([sessionId]) => !removedBrowserSession(sessionId))
      ));
      const presentedSession = browserController.visibleSession();
      if (presentedSession && removedBrowserSession(presentedSession)) {
        browserController.setVisibleSession(null);
      }
      if (
        activeConversationIdRef.current
        && removedConversationIds.has(activeConversationIdRef.current)
      ) {
        browserController.setRuntimeReady(false);
      }
      for (const conversation of removedWorkspace.conversations) {
        dispatchSidePanes({ type: "remove_conversation", conversationId: conversation.id });
        dispatchTerminalTabs({ type: "remove_conversation", conversationId: conversation.id });
        timelineHistoryRef.current!.forget(conversation.id);
      }
      if (activeWorkspaceIdRef.current === workspace.id) {
        // After deletion, open a draft in the first remaining workspace.
        openDraftConversationRef.current(fallback?.id, "workspace");
        setEditor(null);
      }
    } finally {
      deletingWorkspaceIdsRef.current.delete(workspace.id);
      setDeletingWorkspaceIds((current) => {
        if (!current.has(workspace.id)) return current;
        const next = new Set(current);
        next.delete(workspace.id);
        return next;
      });
    }
  };

  const closeTimelineEditors = () => {
    setEditor(null);
    setQuestionEditor(null);
  };

  const handleContextInsert = (index: number, kind: InsertableContextKind, toolName?: string) => {
    if (!activeConversation || contextMutationIsBlocked(activeConversation.id)) return;
    setQuestionEditor(null);
    setEditor({ mode: "insert", kind, index, toolName });
  };
  const handleContextEdit = (item: ContextItem) => {
    if (
      !activeConversation
      || contextMutationIsBlocked(activeConversation.id)
    ) return;
    // Encrypted reasoning is delete-only. Guard every edit entry point so user text can never be persisted as model reasoning.
    if (item.kind === "reasoning" && isEncryptedReasoning(item)) return;
    setQuestionEditor(null);
    setEditor({ mode: "edit", kind: item.kind, item, index: activeConversation.contexts.findIndex((context) => context.id === item.id) });
  };

  const handleQuestionEdit = (item: ToolContext, answer?: UserContext) => {
    if (!activeConversation || contextMutationIsBlocked(activeConversation.id)) return;
    if (!activeConversation.contexts.some((context) => context.id === item.id)) return;
    if (answer && !activeConversation.contexts.some((context) => context.id === answer.id)) return;
    setEditor(null);
    setQuestionEditor({
      conversationId: activeConversation.id,
      item,
      answer
    });
  };

  /**
   * The key a conversation's timeline history is filed under. A draft is filed
   * under the id it will be sent as, so what was done to it before it was sent
   * is still there to undo after; a new draft has a new id and starts clean, and
   * so does a fork or a hand-over, which are conversations of their own.
   */
  const timelineHistoryKey = (conversationId: string): string => (
    isDraftConversationId(conversationId)
      ? draftConversationRef.current?.materializesAs ?? conversationId
      : conversationId
  );

  const showTimelineNotice = (conversationId: string, message: string, hint?: TimelineNoticeState["hint"]) => {
    const id = createId("notice");
    setTimelineNotice({ id, conversationId, message, hint });
    if (timelineNoticeTimerRef.current !== null) window.clearTimeout(timelineNoticeTimerRef.current);
    timelineNoticeTimerRef.current = window.setTimeout(() => {
      timelineNoticeTimerRef.current = null;
      setTimelineNotice((current) => (current?.id === id ? null : current));
    }, TIMELINE_NOTICE_MS);
  };
  useEffect(() => () => {
    if (timelineNoticeTimerRef.current !== null) window.clearTimeout(timelineNoticeTimerRef.current);
  }, []);

  /**
   * Puts focus back in the main timeline when an edit took the focused element
   * with it — the delete button of the card it deleted, the editor it saved —
   * so the undo keys answer straight away. Focus the user moved anywhere else
   * stays where they put it.
   */
  const keepTimelineFocus = () => window.requestAnimationFrame(() => {
    const focused = window.document.activeElement;
    if (focused && focused !== window.document.body && focused.isConnected) return;
    window.document
      .querySelector<HTMLElement>(".conversation-pane__main .context-scroll")
      ?.focus({ preventScroll: true });
  });

  /** A conversation's timeline as it stands now, read past the render that is on screen. */
  const timelineContextsOf = (workspaceId: string, conversationId: string): ContextItem[] => (
    isDraftConversationId(conversationId)
      ? draftConversationRef.current?.contexts ?? []
      : findConversation(documentStore.current(), workspaceId, conversationId).conversation?.contexts ?? []
  );

  /**
   * Writes a timeline patch onto a conversation, the draft included, and says
   * what came of it: the contexts it left, or why it could not land. A patch
   * lands by id onto whatever the list holds now (`applyTimelinePatch`), so a
   * reply the model added since is kept.
   */
  const writeTimelinePatch = (
    conversationId: string,
    patch: TimelinePatch
  ): { contexts: ContextItem[] } | { refused: "absent" | "branch" } => {
    if (isDraftConversationId(conversationId)) {
      const draft = draftConversationRef.current;
      if (!draft || !isDraftConversationId(activeConversationIdRef.current)) return { refused: "absent" };
      const contexts = applyTimelinePatch(draft.contexts, patch);
      if (!contexts) return { refused: "absent" };
      setDraftConversation((current) => (
        current ? { ...current, contexts: applyTimelinePatch(current.contexts, patch) ?? current.contexts } : current
      ));
      return { contexts };
    }
    const latest = documentStore.current();
    const workspace = latest?.workspaces.find((candidate) => (
      candidate.conversations.some((conversation) => conversation.id === conversationId)
    ));
    const conversation = workspace?.conversations.find((candidate) => candidate.id === conversationId);
    if (!workspace || !conversation) return { refused: "absent" };
    // A message a branch was taken from anchors that branch; taking it out would orphan it.
    if (removedIds(patch).some((id) => isConversationBranchFork(conversation, id))) return { refused: "branch" };
    const contexts = applyTimelinePatch(conversation.contexts, patch);
    if (!contexts) return { refused: "absent" };
    updateConversation(workspace.id, conversationId, (current) => ({
      ...current,
      contexts: applyTimelinePatch(current.contexts, patch) ?? current.contexts,
      updatedAt: new Date().toISOString()
    }));
    return { contexts };
  };

  /**
   * Makes an edit on a timeline and files it in that timeline's history, so the
   * undo keys can take it back. Every edit made on the timeline goes through
   * here: a delete, a placed or rewritten card, a run of a tool by hand.
   */
  const commitTimelineEdit = (
    conversationId: string,
    patch: TimelinePatch,
    label: string,
    notice?: string
  ): boolean => {
    const written = writeTimelinePatch(conversationId, patch);
    if (!("contexts" in written)) return false;
    timelineHistoryRef.current!.record(timelineHistoryKey(conversationId), { patch, label });
    setContextUsage((current) => ({ ...current, [conversationId]: estimateActiveContextUsage(written.contexts) }));
    if (notice) {
      showTimelineNotice(conversationId, notice, {
        keys: timelineHistoryShortcut("undo"),
        action: t("撤回", "to undo")
      });
    }
    keepTimelineFocus();
    return true;
  };

  /** Takes back the last edit on a timeline, or makes the last one taken back again. */
  const stepTimelineHistory = (conversationId: string, action: "undo" | "redo") => {
    const history = timelineHistoryRef.current!;
    const key = timelineHistoryKey(conversationId);
    const entry = action === "undo" ? history.nextUndo(key) : history.nextRedo(key);
    if (!entry) {
      showTimelineNotice(conversationId, action === "undo"
        ? t("没有可撤回的修改", "Nothing to undo")
        : t("没有可重做的修改", "Nothing to redo"));
      return;
    }
    if (contextMutationIsBlocked(conversationId)) {
      showTimelineNotice(conversationId, t(
        "模型回合进行中，暂时不能修改上下文",
        "Context cannot be changed while a model turn is in progress"
      ));
      return;
    }
    const written = writeTimelinePatch(
      conversationId,
      action === "undo" ? invertTimelinePatch(entry.patch) : entry.patch
    );
    if (!("contexts" in written)) {
      if (written.refused === "branch") {
        showTimelineNotice(conversationId, t(
          "有分支从这条消息开始，不能撤回",
          "A branch starts at this message, so this cannot be undone"
        ));
        return;
      }
      // Nothing of it is left to change: whatever it touched is gone, or back already.
      if (action === "undo") history.dropUndo(key);
      else history.dropRedo(key);
      showTimelineNotice(conversationId, t("这次修改已不在时间线上", "That edit is no longer on the timeline"));
      return;
    }
    if (action === "undo") history.undone(key);
    else history.redone(key);
    closeTimelineEditors();
    setContextUsage((current) => ({ ...current, [conversationId]: estimateActiveContextUsage(written.contexts) }));
    showTimelineNotice(
      conversationId,
      action === "undo"
        ? t("已撤回：{label}", "Undone: {label}", { label: entry.label })
        : t("已重做：{label}", "Redone: {label}", { label: entry.label }),
      action === "undo"
        ? { keys: timelineHistoryShortcut("redo"), action: t("重做", "to redo") }
        : { keys: timelineHistoryShortcut("undo"), action: t("撤回", "to undo") }
    );
    keepTimelineFocus();
  };

  const deletedMessagesNotice = (count: number) => (count === 1
    ? t("已删除 1 条消息", "Deleted 1 message")
    : t("已删除 {count} 条消息", "Deleted {count} messages", { count }));

  const deleteQuestionContext = (item: ToolContext, answer?: UserContext) => {
    if (!activeConversation || !activeWorkspaceId || contextMutationIsBlocked(activeConversation.id)) return;
    const deletedItems = answer ? [item, answer] : [item];
    if (deletedItems.some((context) => isConversationBranchFork(activeConversation, context.id))) return;
    const removed = placedContexts(activeConversation.contexts, new Set(deletedItems.map((context) => context.id)));
    if (removed.length !== deletedItems.length) return;
    setQuestionEditor(null);
    commitTimelineEdit(
      activeConversation.id,
      { ...EMPTY_TIMELINE_PATCH, removed },
      answer
        ? t("删除整条提问消息", "Delete the complete question message")
        : t("删除提问消息", "Delete the question message"),
      deletedMessagesNotice(1)
    );
  };

  const deleteContext = (item: ContextItem) => {
    // A draft has no workspace until it is sent, yet its hand-written content is still deletable.
    if (
      !activeConversation
      || (!activeWorkspaceId && !draftActive)
      || contextMutationIsBlocked(activeConversation.id)
    ) return;
    if (isConversationBranchFork(activeConversation, item.id)) return;
    // Confirm before computing deletion results; asking after that would perform the deletion first.
    if (
      appearance.confirmMessageDelete
      && !window.confirm(t("确定删除这条上下文？", "Delete this context item?"))
    ) return;
    const removed = placedContexts(activeConversation.contexts, new Set([item.id]));
    if (!removed.length) return;
    commitTimelineEdit(
      activeConversation.id,
      { ...EMPTY_TIMELINE_PATCH, removed },
      item.kind === "tool" ? t("删除工具调用", "Delete the tool call") : t("删除上下文", "Delete the context"),
      deletedMessagesNotice(1)
    );
  };

  /**
   * Deletes everything a selection box picked out, as one edit with one undo. A
   * message a branch was taken from stays: it anchors that branch, and the
   * single delete refuses it too.
   */
  const deleteTimelineContexts = (ids: string[]) => {
    if (
      !activeConversation
      || (!activeWorkspaceId && !draftActive)
      || contextMutationIsBlocked(activeConversation.id)
    ) return;
    const doomed = new Set(ids.filter((id) => !isConversationBranchFork(activeConversation, id)));
    const removed = placedContexts(activeConversation.contexts, doomed);
    if (!removed.length) return;
    if (
      appearance.confirmMessageDelete
      && !window.confirm(t("确定删除选中的 {count} 条上下文？", "Delete the {count} selected context items?", {
        count: removed.length
      }))
    ) return;
    setEditor((current) => (current?.mode === "edit" && doomed.has(current.item.id) ? null : current));
    setQuestionEditor((current) => (current && doomed.has(current.item.id) ? null : current));
    commitTimelineEdit(
      activeConversation.id,
      { ...EMPTY_TIMELINE_PATCH, removed },
      removed.length === 1
        ? t("删除 1 条消息", "Delete 1 message")
        : t("删除 {count} 条消息", "Delete {count} messages", { count: removed.length }),
      deletedMessagesNotice(removed.length)
    );
  };

  /**
   * Files picked, pasted or dropped into a user message on the timeline — one
   * being edited, or one being written from the context menu.
   *
   * The same gates the composer applies, because this is the same kind of
   * message arriving through a different box: vision, per-image size and
   * pixels, file formats and sizes, and how many one message may carry.
   * Numbering happens here, against the live transcript, so an attached image
   * is citable as `[Image #N]` the moment the card is saved. A model without
   * image input turns pictures away with a reason instead of letting bytes
   * land where it could never read them.
   */
  const timelineImageInput = Boolean(activeModelChoice && supportsVision(activeModelChoice.model));
  const addAttachmentsToTimelineMessage = activeConversation
    ? messageAttachmentAdder(timelineImageInput, () => reserveQueuedMessageIds(
      imageShortIdsInUse(activeConversation.contexts),
      activeConversation.queuedMessages
    ))
    : undefined;

  const saveTextContext = (content: string, images?: ImageAttachment[], files?: FileAttachment[]) => {
    if (!editor || !activeConversation || contextMutationIsBlocked(activeConversation.id)) return;
    const conversationId = activeConversation.id;
    if (editor.mode === "edit") {
      const before = activeConversation.contexts.find((item) => item.id === editor.item.id);
      if (!before || before.kind === "tool") return;
      const after: ContextItem = before.kind === "assistant" || before.kind === "reasoning"
        ? { ...before, content, interrupted: false }
        : before.kind === "user" && images
          ? { ...before, content, images, files: files?.length ? files : undefined }
          : { ...before, content };
      commitTimelineEdit(
        conversationId,
        { ...EMPTY_TIMELINE_PATCH, replaced: [{ before, after }] },
        t("编辑上下文", "Edit the context")
      );
    } else {
      const base = { id: createId("ctx"), createdAt: new Date().toISOString(), content };
      const item: ContextItem = editor.kind === "reasoning"
        // Manually inserted reasoning is explicitly plaintext because the user supplied all of its text.
        ? { ...base, kind: "reasoning", form: "plaintext" }
        : editor.kind === "user"
          // A placed message carries what was pasted into it; every other kind
          // has nowhere to put an image and is never handed one.
          ? { ...base, kind: "user", ...(images?.length ? { images } : {}), ...(files?.length ? { files } : {}) }
          : { ...base, kind: editor.kind as "system" | "assistant" };
      commitTimelineEdit(
        conversationId,
        insertionPatch(activeConversation.contexts, item, editor.index),
        t("插入上下文", "Insert a context")
      );
    }
    setEditor(null);
  };

  /**
   * Writes hand-written arguments and result onto a card, recorded or brand new.
   * Nothing is executed, so the card would not be provable; the host issues its
   * attestation over what was typed and returns the exact bytes to store. A
   * placed card is how a call with side effects gets into the transcript without
   * causing them.
   */
  /**
   * The conversation a tool the user runs or records on the timeline goes into, as the host knows
   * it. A tool card belongs to a workspace, and the host executes and attests one only against a
   * conversation it holds, so a draft materializes here and is fixed in its project from then on.
   *
   * Callers write through the returned ids, never through `updateActiveConversation`: its target
   * was read at render time, before this call made the draft real.
   */
  const conversationForTimelineTool = async (): Promise<{
    workspaceId: string;
    conversationId: string;
  }> => {
    if (activeConversation && !isDraftConversationId(activeConversation.id)) {
      if (!activeWorkspace) throw new Error(t("没有活动对话", "No active conversation"));
      await flushLatestDocument();
      return { workspaceId: activeWorkspace.id, conversationId: activeConversation.id };
    }
    // An upload still addressed to the draft would be dropped once the draft is gone.
    if (composerController.current().imageLoadingIds.has(DRAFT_CONVERSATION_ID)) {
      throw new Error(t(
        "输入框里的图片还在上传，稍后再运行工具",
        "Images in the composer are still uploading; run the tool once they finish"
      ));
    }
    let redeemed: { workspaceId: string; conversationId: string } | null;
    try {
      redeemed = await redeemDraft({ persist: true });
    } catch (reason) {
      throw new Error(t("工具没有运行：{reason}", "The tool did not run: {reason}", {
        reason: failureMessage(reason, t("原因不明", "unknown reason"))
      }));
    }
    if (!redeemed) throw new Error(t("没有活动对话", "No active conversation"));
    return redeemed;
  };

  const saveToolContextEdit = async (input: JsonObject, output: string, images: ImageAttachment[]): Promise<void> => {
    if (!editor || !activeConversation) {
      throw new Error(t("没有活动对话", "No active conversation"));
    }
    if (contextMutationIsBlocked(activeConversation.id)) {
      throw new Error(t(
        "模型回合进行中，无法修改上下文",
        "Context cannot be changed while a model turn is in progress"
      ));
    }
    if (editor.mode === "insert") {
      if (editor.kind !== "tool" || !editor.toolName) {
        throw new Error(t("没有选择工具", "No tool was chosen"));
      }
      const contextId = createId("ctx");
      const toolName = editor.toolName;
      const index = editor.index;
      const { workspaceId, conversationId } = await conversationForTimelineTool();
      const attested = await attestInsertedToolContext({ conversationId, contextId, toolName, input, output });
      const placed: ContextItem = {
        id: contextId,
        kind: "tool",
        toolName,
        input: attested.input,
        result: attested.result,
        attestation: attested.attestation,
        createdAt: new Date().toISOString()
      };
      commitTimelineEdit(
        conversationId,
        insertionPatch(timelineContextsOf(workspaceId, conversationId), placed, index),
        t("插入工具调用", "Insert a tool call")
      );
      setEditor(null);
      return;
    }
    if (editor.item.kind !== "tool") {
      throw new Error(t("没有活动对话", "No active conversation"));
    }
    const contextId = editor.item.id;
    const { workspaceId, conversationId } = await conversationForTimelineTool();
    const attested = await attestEditedToolContext({
      conversationId,
      contextId,
      toolName: editor.item.toolName,
      input,
      output,
      images
    });
    const before = timelineContextsOf(workspaceId, conversationId).find((item) => item.id === contextId);
    if (before?.kind === "tool") {
      commitTimelineEdit(
        conversationId,
        {
          ...EMPTY_TIMELINE_PATCH,
          replaced: [{
            before,
            after: {
              ...before,
              requestedInput: undefined,
              input: attested.input,
              result: attested.result,
              attestation: attested.attestation
            }
          }]
        },
        t("编辑工具调用", "Edit the tool call")
      );
    }
    setEditor(null);
  };

  const saveQuestionContext = async (input: JsonObject, answerContent?: string): Promise<void> => {
    if (!questionEditor || !activeConversation || questionEditor.conversationId !== activeConversation.id) {
      throw new Error(t("没有活动提问", "No active question"));
    }
    if (contextMutationIsBlocked(activeConversation.id)) {
      throw new Error(t(
        "模型回合进行中，无法修改上下文",
        "Context cannot be changed while a model turn is in progress"
      ));
    }
    const originalQuestions = questionsFromInput(questionEditor.item.input);
    const nextQuestions = questionsFromInput(input);
    if (
      !isClaudeQuestionInput(input)
      || nextQuestions.length !== originalQuestions.length
    ) {
      throw new Error(t(
        "提问数量必须保持不变，且每一项都必须符合提问格式",
        "The number of questions must stay unchanged and every item must remain valid"
      ));
    }
    if (questionEditor.answer) {
      const nextAnswers = answersFromFormattedContent(answerContent ?? "", nextQuestions);
      if (!nextAnswers || nextAnswers.length !== nextQuestions.length || nextAnswers.some((answer) => !answer.trim())) {
        throw new Error(t(
          "每一个问题都必须保留非空回答",
          "Every question must keep a non-empty answer"
        ));
      }
    }

    const questionId = questionEditor.item.id;
    const answerId = questionEditor.answer?.id;
    if (
      !activeConversation.contexts.some((context) => context.id === questionId)
      || (answerId && !activeConversation.contexts.some((context) => context.id === answerId))
    ) {
      throw new Error(t("提问消息已不存在", "The question message no longer exists"));
    }
    const replaced = activeConversation.contexts.flatMap((context): TimelinePatch["replaced"] => {
      if (context.id === questionId && context.kind === "tool") {
        return [{ before: context, after: { ...context, requestedInput: undefined, input } }];
      }
      if (answerId && context.id === answerId && context.kind === "user") {
        return [{ before: context, after: { ...context, content: answerContent ?? context.content } }];
      }
      return [];
    });
    commitTimelineEdit(
      activeConversation.id,
      { ...EMPTY_TIMELINE_PATCH, replaced },
      t("编辑提问", "Edit the questions")
    );
    setQuestionEditor(null);
  };

  const saveToolContext = async (toolName: string, input: JsonObject): Promise<ToolResult> => {
    // A draft with no project runs its tool in the temporary workspace it materializes into.
    if (!document || !editor || (!activeWorkspace && !draftActive) || !activeConversation) {
      throw new Error(t("没有活动对话", "No active conversation"));
    }
    if (contextMutationIsBlocked(activeConversation.id)) {
      throw new Error(t(
        "模型回合进行中，无法修改上下文",
        "Context cannot be changed while a model turn is in progress"
      ));
    }
    if (!activeConversationTools.some((tool) => tool.name === toolName) || !activeEnabledTools.includes(toolName)) {
      throw new Error(t(
        "当前工作区模式不可使用工具 {tool}",
        "Tool {tool} is unavailable in the current workspace mode",
        { tool: toolName }
      ));
    }
    if (
      editor.mode === "edit"
      && editor.item.kind === "tool"
      && editor.item.toolName === "ask_user"
    ) {
      if (!isClaudeQuestionInput(input)) {
        throw new Error(t(
          "提问参数不符合 Claude Code AskUserQuestion 格式",
          "The questions do not match the Claude Code AskUserQuestion format"
        ));
      }
      const id = editor.item.id;
      const existingResult = editor.item.result;
      const before = activeConversation.contexts.find((item) => item.id === id);
      if (before?.kind === "tool") {
        commitTimelineEdit(
          activeConversation.id,
          { ...EMPTY_TIMELINE_PATCH, replaced: [{ before, after: { ...before, requestedInput: undefined, input } }] },
          t("编辑提问", "Edit the questions")
        );
      }
      setEditor(null);
      return existingResult;
    }
    const { workspaceId, conversationId } = await conversationForTimelineTool();
    const workspacePath = documentStore.current()?.workspaces
      .find((workspace) => workspace.id === workspaceId)?.path ?? "";
    const request = { conversationId, workspacePath, toolName, input };
    // Rust classifies every operation from the persisted security policy. Safe calls return
    // without a card; calls that need consent come back with a prompt to draw, and answering
    // it is what mints the single-use nonce — from the arguments Rust classified, not from
    // anything the renderer could substitute in between.
    const approval = await requestToolApproval(request);
    const granted = approval?.prompt
      ? await awaitManualToolApproval(conversationId, approval.prompt)
      : approval;
    const execution = await executeTool(request, granted?.nonce);
    const current = timelineContextsOf(workspaceId, conversationId);
    if (editor.mode === "edit") {
      const before = current.find((item) => item.id === editor.item.id);
      if (before?.kind === "tool") {
        commitTimelineEdit(
          conversationId,
          {
            ...EMPTY_TIMELINE_PATCH,
            replaced: [{ before, after: { ...before, requestedInput: undefined, input, result: execution } }]
          },
          t("重新运行工具调用", "Run the tool call again")
        );
      }
    } else {
      const item: ContextItem = {
        id: createId("ctx"),
        kind: "tool",
        toolName,
        input,
        result: execution,
        createdAt: new Date().toISOString()
      };
      commitTimelineEdit(
        conversationId,
        insertionPatch(current, item, editor.index),
        t("运行工具调用", "Run a tool call")
      );
    }
    setEditor(null);
    return execution;
  };

  /** Records the surface and model a run's request goes out with as the
   * conversation's lock (`toolLock.ts`). The composer's own send already folds
   * this into its durable write; this covers the runs that start without a new
   * user message. */
  const lockConversationTools = useCallback(
    (
      workspaceId: string,
      conversationId: string,
      tools: string[],
      request: { providerId: string; modelId: string }
    ) => {
      const latest = documentStore.current();
      const current = findConversation(latest, workspaceId, conversationId).conversation;
      if (!current) return;
      /* The same question the composer asks before its own send: did this run
         put `web_fetch` in front of the model, or only `web_search`? */
      const context = {
        webFetch: latest ? grantsWebFetch(
          current.settings.webSearchEnabled === true,
          current.settings.webSearch,
          latest.globalSettings.webSearch,
          modelChoiceForConversation(latest).provider?.family
        ) : false,
        ...request,
        at: new Date().toISOString()
      };
      updateConversation(workspaceId, conversationId, (conversation) => ({
        ...conversation,
        settings: withRunToolLock(conversation.settings, tools, context)
      }));
    },
    [documentStore, modelChoiceForConversation, updateConversation]
  );
  /* Picking a model again puts back what its last request's lock holds, in
     every conversation that request came from: a change made while another
     model was selected was free then, and is not now. */
  useEffect(() => {
    restoreLocksForModelRef.current = (providerId, modelId) => {
      const latest = documentStore.current();
      const provider = latest?.globalSettings.apiProviders.find((item) => item.id === providerId);
      const model = provider?.models.find((item) => item.id === modelId);
      const lockModel = toolLockModelOf(provider, model);
      if (!latest || !lockModel) return;
      const restore = (settings: Conversation["settings"]) => (
        restoreLockedSettings(settings, toolLockState(settings, lockModel, Date.now()))
      );
      for (const workspace of latest.workspaces) {
        for (const conversation of workspace.conversations) {
          if (restore(conversation.settings) === conversation.settings) continue;
          updateConversation(workspace.id, conversation.id, (current) => {
            const settings = restore(current.settings);
            return settings === current.settings ? current : { ...current, settings };
          });
        }
      }
    };
  }, [documentStore, updateConversation]);
  const refreshConversationToolLock = useCallback(
    (workspaceId: string, conversationId: string) => {
      updateConversation(workspaceId, conversationId, (conversation) => {
        const lock = conversation.settings.toolLock;
        if (!lock?.lastRequest) return conversation;
        return {
          ...conversation,
          settings: {
            ...conversation.settings,
            toolLock: refreshedToolLock(toolLockOf(conversation.settings), new Date().toISOString())
          }
        };
      });
    },
    [updateConversation]
  );

  /** Refresh App facilities after each commit; the send pipeline reads them through `host()` at call time. */  const sendPipelineHostRef = useRef<SendPipelineHost | null>(null);
  useEffect(() => {
    sendPipelineHostRef.current = {
      t,
      openProviderSettings: () => openGlobalSettings("providers"),
      activeWorkspaceId: () => activeWorkspaceIdRef.current,
      activeConversationId: () => activeConversationIdRef.current,
      draftIsOpen: () => draftConversationRef.current !== null,
      activeConversationTools: () => activeConversationTools,
      activeEnabledTools: () => activeEnabledTools,
      lockConversationTools,
      refreshConversationToolLock,
      contextMutationIsBlocked,
      ensureConversationBody: async (conversationId) => {
        await conversationBodies.ensureLoaded(conversationId);
      },
      closeEditorIfActive: (conversationId) => {
        if (activeConversationIdRef.current === conversationId) setEditor(null);
      },
      scrollTimelineToBottom: () => window.requestAnimationFrame(() => window.document
        .querySelector<HTMLElement>('[data-main-context-stream="true"]')
        ?.scrollTo({ top: 1e9, behavior: "smooth" })),
      persistDocumentImmediately,
      flushLatestDocument,
      updateConversation,
      startConversationTurn,
      continueConversationTurn,
      resumeAdoptedConversationTurn,
      splitConversationTurn,
      updateRunningTurnUsage,
      finishConversationTurns,
      persistInterruptedRun,
      failConversationTurn,
      refreshConversation,
      conversationWriteFailure: (conversationId) => conversationSync.lastFailure(conversationId),
      applySettledToolContext,
      clearModelRunError,
      setModelRunError: (conversationId, error) => setModelRunErrors((current) => ({
        ...current,
        [conversationId]: error
      })),
      setContextUsage: (conversationId, usage) => setContextUsage((current) => ({
        ...current,
        [conversationId]: usage
      })),
      openToolPrompt,
      closeToolPrompt,
      // At run end, reconcile host-held pending approvals instead of clearing them; background cards outlive turns. Preserve renderer-manual cards by reference.
      reconcileToolPrompts: (conversationId) => {
        const drop = () => setToolPrompts((current) => {
          if (!current[conversationId]) return current;
          const kept = current[conversationId].filter(
            (prompt) => manualToolPromptsRef.current.has(prompt.promptId)
          );
          const next = { ...current };
          if (kept.length) next[conversationId] = kept;
          else delete next[conversationId];
          return next;
        });
        void listPendingToolPrompts().then((pending) => {
          const alive = new Set(
            pending
              .filter((prompt) => prompt.conversationId === conversationId)
              .map((prompt) => prompt.promptId)
          );
          setToolPrompts((current) => {
            if (!current[conversationId]) return current;
            const kept = current[conversationId].filter(
              (prompt) => alive.has(prompt.promptId)
                || manualToolPromptsRef.current.has(prompt.promptId)
            );
            if (kept.length === current[conversationId].length) return current;
            const next = { ...current };
            if (kept.length) next[conversationId] = kept;
            else delete next[conversationId];
            return next;
          });
        }).catch(drop);
      },
      pendingQuestionFor: (conversationId) => {
        const card = toolPrompts[conversationId]?.find((prompt) => prompt.kind === "question");
        if (!card) return null;
        return {
          promptId: card.promptId,
          response: questionDraftsRef.current.get(card.promptId) ?? { action: "close", answers: [] }
        };
      },
      answerQuestion: answerQuestionPrompt,
      registerPreviewSessionForTool: (conversationId, toolName) => {
        // The tools that only read or kill a dev-server process are excluded — they never
        // create a page for the conversation to own.
        if (isPreviewPageToolName(toolName)) registerPreviewSession(conversationId);
      }
    };
  });
  const [sendPipeline] = useState(() => createSendPipeline(
    { documentStore, composerController, modelRunController },
    () => {
      const pipelineHost = sendPipelineHostRef.current;
      if (!pipelineHost) throw new Error("send pipeline host is not ready before first commit");
      return pipelineHost;
    }
  ));
  const {
    performModelRun,
    sendComposer,
    wakeConversation,
    startForkedConversationRun,
    deleteQueuedMessage,
    steerQueuedMessage,
    addComposerImages,
    addComposerAttachments,
    removeComposerImage,
    removeComposerFile,
    dispatchNextQueuedMessage,
    retryFailedQueuedPromotion
  } = sendPipeline;

  /** Materialize a draft synchronously before `sendComposer`, which requires both active ids in the document. Do not materialize while images are uploading, because send rejection must not leave an empty conversation. */
  const sendActiveComposer = useCallback(async () => {
    if (isDraftConversationId(activeConversationIdRef.current)) {
      if (composerController.current().imageLoadingIds.has(DRAFT_CONVERSATION_ID)) return;
      try {
        // A requested worktree is created before the first message so every tool call uses its
        // isolated checkout; never silently fall back to the workspace root.
        if (!await redeemDraft({ persist: false })) return;
      } catch (reason) {
        // Include send failure explicitly because `failureMessage` otherwise favors the host's Git detail.
        setBranchChipError(t(
          "无法建立隔离工作树，消息还没有发出：{reason}。取消勾选「工作树」可以直接在工作区根上开始。",
          "The isolated worktree could not be created, so nothing was sent: {reason}. Clear the worktree checkbox to start on the workspace root instead.",
          { reason: failureMessage(reason, t("原因不明", "unknown reason")) }
        ));
        return;
      }
    }
    await sendComposer();
  }, [
    composerController,
    redeemDraft,
    sendComposer,
    t
  ]);

  // After loading, adopt host runs that are still live or awaiting settlement. Queue dispatch waits for adoption so both paths cannot race for one conversation. A ref latch allows only one adoption; failed adoption reopens it for retry.
  const [resumableRunsAdopted, setResumableRunsAdopted] = useState(false);
  const resumableRunsAdoptionStartedRef = useRef(false);
  const [adoptionRetrySignal, setAdoptionRetrySignal] = useState(0);
  useEffect(() => {
    if (!document || resumableRunsAdoptionStartedRef.current) return;
    resumableRunsAdoptionStartedRef.current = true;
    void sendPipeline.adoptResumableRuns().then(
      () => setResumableRunsAdopted(true),
      () => {
        window.setTimeout(() => {
          resumableRunsAdoptionStartedRef.current = false;
          setAdoptionRetrySignal((current) => current + 1);
        }, 3000);
      }
    );
  }, [document, sendPipeline, adoptionRetrySignal]);

  // `taskSettled` is an edge signal that reload and bounded host backlogs can lose. After adoption, rescan deliverable idle conversations to restore the level signal.
  useEffect(() => {
    if (!resumableRunsAdopted) return;
    void listWakePendingConversations().then((conversationIds) => {
      if (!conversationIds.length) return;
      for (const conversationId of conversationIds) {
        pendingWakeConversationsRef.current.add(conversationId);
      }
      setWakeSignal((current) => current + 1);
    }).catch(() => {
      // Rescan failure is non-fatal; a later edge notification can restore the signal.
    });
  }, [resumableRunsAdopted]);

  // Rescan unresolved background-task approvals after adoption. They are not replayed with a run and may predate all open streams; prompt-id deduplication makes duplicate push or replay delivery safe.
  useEffect(() => {
    if (!resumableRunsAdopted) return;
    void listPendingToolPrompts().then((prompts) => {
      for (const { conversationId, ...prompt } of prompts) {
        openToolPrompt(conversationId, prompt);
      }
    }).catch(() => {
      // Rescan failure is non-fatal; later push or replay can show the card.
    });
  }, [resumableRunsAdopted, openToolPrompt]);

  // Fork cards belong to no run and outlive every stream; re-list them after adoption so a
  // reload cannot hide a request until its 30-minute expiry. Fork-id deduplication makes a
  // duplicate push delivery safe.
  useEffect(() => {
    if (!resumableRunsAdopted) return;
    void listPendingForkRequests().then((requests) => {
      for (const request of requests) openForkRequest(request);
    }).catch(() => {
      // Rescan failure is non-fatal; a later push can still show the card.
    });
  }, [resumableRunsAdopted, openForkRequest]);

  // Fork decisions are host rows: re-read them whenever a conversation becomes active, so a
  // card answered while another conversation was open still shows up in this one's task bar.
  useEffect(() => {
    if (!activeConversationId || isDraftConversationId(activeConversationId)) return;
    let cancelled = false;
    void listForkDecisions(activeConversationId).then((records) => {
      if (cancelled) return;
      setForkDecisions((current) => ({ ...current, [activeConversationId]: records }));
    }).catch(() => {
      // The rows are advisory; a failed read keeps whatever was already shown.
    });
    return () => { cancelled = true; };
  }, [activeConversationId]);

  // The durable intent is authoritative; a successfully delivered push is not an acknowledgement.
  useEffect(() => {
    if (!resumableRunsAdopted) return;
    let cancelled = false;
    void listPendingForkStarts().then((starts) => {
      if (cancelled) return;
      pendingForkStartsRef.current.push(...starts);
      setForkSignal((current) => current + 1);
    }).catch((error) => {
      if (!cancelled) setForkStartError(failureMessage(error, t("无法读取分叉首轮待启动记录", "Could not load pending fork runs")));
    });
    return () => { cancelled = true; };
  }, [resumableRunsAdopted, forkRetrySignal, t]);

  // Start the first run of every child the host forked. The host wrote the child (history copy
  // plus the prompt as its last user message) and published `forkResolved`; the renderer is the
  // only run starter, so it loads the authoritative body, files it under its workspace — without
  // a create command, the host already owns it — and runs it. Waits for adoption like a wake does.
  useEffect(() => {
    if (!document || !resumableRunsAdopted) return;
    const starts = pendingForkStartsRef.current.splice(0);
    for (const { workspaceId, conversationId } of starts) {
      if (forkStartsAttemptedRef.current.has(conversationId)) continue;
      forkStartsAttemptedRef.current.add(conversationId);
      void loadConversationRemote(conversationId).then(async (authoritative) => {
        if (!authoritative) throw new Error(t("分叉子会话不存在", "The forked conversation is unavailable"));
        // A new child opens the list, where the host filed it; one already listed (a start
        // retried after a restart) keeps its place.
        documentStore.update((current) => current ? {
          ...current,
          workspaces: current.workspaces.map((workspace) => (
            workspace.id === workspaceId
              ? {
                ...workspace,
                conversations: workspace.conversations.some((candidate) => candidate.id === authoritative.id)
                  ? workspace.conversations.map((candidate) => candidate.id === authoritative.id ? authoritative : candidate)
                  : [authoritative, ...workspace.conversations]
              }
              : workspace
          ))
        } : current);
        setContextUsage((currentUsage) => ({
          ...currentUsage,
          [authoritative.id]: estimateActiveContextUsage(authoritative.contexts)
        }));
        const handedOffFrom = handoffJumpsRef.current.get(conversationId);
        if (handedOffFrom !== undefined) {
          handoffJumpsRef.current.delete(conversationId);
          if (activeConversationIdRef.current === handedOffFrom) {
            selectConversationRef.current(workspaceId, conversationId);
          }
        }
        if (!await startForkedConversationRun(workspaceId, conversationId)) {
          throw new Error(authoritative.handoffOf
            ? t("交接会话首轮尚未启动，请检查模型配置后重试", "The handover conversation's run has not started. Check model settings and retry.")
            : t("分叉首轮尚未启动，请检查模型配置后重试", "The fork run has not started. Check model settings and retry."));
        }
      }).catch((error) => {
        setForkStartError(failureMessage(error, t("分叉出的会话未能开始运行", "The forked conversation could not start its run")));
      });
    }
  }, [forkSignal, document, resumableRunsAdopted, documentStore, startForkedConversationRun, estimateActiveContextUsage, t]);

  // Start a no-user-message wake run for active conversations with pending wake state, but only after adoption completes to avoid competing for a conversation.
  useEffect(() => {
    if (!document || !resumableRunsAdopted || !activeConversation) return;
    const conversationId = activeConversation.id;
    if (!pendingWakeConversationsRef.current.has(conversationId)) return;
    // Consume the record before awaiting, not after. `wakeConversation` only
    // resolves once the run it starts has settled, and that run's own state
    // updates reopen this effect — with the record still in place, the rerun
    // that lands after the run is gone starts a second, duplicate wake run.
    // A wake that could not proceed puts the record back without signalling,
    // so the next natural rerun retries it exactly as before.
    pendingWakeConversationsRef.current.delete(conversationId);
    void wakeConversation().then((consumed) => {
      if (!consumed) pendingWakeConversationsRef.current.add(conversationId);
    });
  }, [wakeSignal, document, resumableRunsAdopted, activeConversation, modelRunSummaries, wakeConversation]);

  useEffect(() => {
    if (!document || !resumableRunsAdopted) return;
    for (const workspace of document.workspaces) {
      for (const conversation of workspace.conversations) {
        const queuedHead = conversation.queuedMessages[0];
        if (
          queuedHead
          && !composerController.current().failedQueuedPromotionIds.has(queuedHead.id)
          && !modelRunController.current()[conversation.id]
          && !modelRunController.hasPerformingRun(conversation.id)
          && !modelRunController.hasPreparingRun(conversation.id)
        ) {
          void dispatchNextQueuedMessage(workspace.id, conversation.id);
        }
      }
    }
  }, [dispatchNextQueuedMessage, document, modelRunSummaries, resumableRunsAdopted]);

  /**
   * Branching starts a genuinely independent conversation that owns a full copy
   * of the history preceding the branch point. The branched user message itself
   * is handed to the new composer as a draft, so the branch only begins once
   * the user actually sends it, and the original conversation is never
   * truncated.
   *
   * The host performs the copy (`fork_conversation_contexts`): context ids are
   * unique document-wide and every copied tool result needs an execution
   * receipt re-issued for its new owner. Nothing is shared by id — each branch
   * is its own session with its own persisted data.
   */
  const branchFromUserContext = async (item: ContextItem) => {
    const workspaceId = activeWorkspaceId;
    const conversationId = activeConversationId;
    const latest = documentStore.current();
    if (
      !latest
      || !workspaceId
      || !conversationId
      || item.kind !== "user"
    ) return;
    const located = findConversation(latest, workspaceId, conversationId);
    if (!located.workspace || !located.conversation) return;
    const source = located.conversation;
    const forkIndex = source.contexts.findIndex((context) => context.id === item.id);
    if (forkIndex < 0) return;

    // Create the branch before any await: `createConversation` reads the
    // render-time document, which a suspended continuation would leave stale.
    // The branch nests under its source in the sidebar; it is otherwise an
    // independent conversation, which retries the source's work under the
    // source's settings — a Full access conversation does not branch into Manual.
    const created = createConversation(
      workspaceId,
      "global",
      source.settings,
      undefined,
      conversationId,
      [],
      source.presetId,
      "",
      [],
      undefined,
      undefined,
      // A branch with nothing before it carries no history, and so no cache.
      forkIndex > 0 ? source.settings.toolLock : undefined
    );
    if (!created) return;
    composerController.updateDrafts((current) => ({ ...current, [created]: item.content ?? "" }));
    window.requestAnimationFrame(() => composerTextareaRef.current?.focus());

    // Everything before the branch point carries over. The branched message
    // stays in the composer instead, so the user can edit it before sending.
    if (forkIndex === 0) return;
    const throughContextId = source.contexts[forkIndex - 1].id;
    try {
      // The host copies from its own committed document, so the new
      // conversation and any pending edit must be on disk before it reads.
      await flushLatestDocument();
      const contexts = await forkConversationContexts({
        workspaceId,
        sourceConversationId: conversationId,
        targetConversationId: created,
        throughContextId,
        sourceContexts: source.contexts
      });
      const current = documentStore.current();
      if (!current) return;
      const next: AppDocument = {
        ...current,
        workspaces: current.workspaces.map((workspace) => workspace.id === workspaceId ? {
          ...workspace,
          conversations: workspace.conversations.map((conversation) => (
            conversation.id === created
              ? { ...conversation, contexts, updatedAt: new Date().toISOString() }
              : conversation
          ))
        } : workspace)
      };
      // Receipts issued by the fork are consumed by this save, so it must not
      // wait for the debounce.
      await persistDocumentImmediately(next);
      setContextUsage((currentUsage) => ({
        ...currentUsage,
        [created]: estimateActiveContextUsage(contexts)
      }));
    } catch {
      // The branch exists even if historical contexts could not be copied; users can branch again.
    }
  };

  /**
   * Forks the active conversation at a gap in its timeline: everything above
   * the insertion line is copied into a new conversation, which opens. Unlike a
   * branch from a user message nothing is handed to the composer, and the fork
   * keeps the source's settings — it carries on that work rather than starting
   * a new task. Its lock comes along as it stands, so what is orange there and
   * which models the menu marks as cached run out when the source's do. The
   * copy is the host's, on the same terms as a branch's.
   *
   * The fork is named after its origin (`lib/conversationForks.ts`), and the
   * name is settled at once so the local helper model never retitles it.
   */
  const forkConversationAt = async (index: number) => {
    const workspaceId = activeWorkspaceId;
    const conversationId = activeConversationId;
    const latest = documentStore.current();
    if (!latest || !workspaceId || !conversationId || index <= 0) return;
    const located = findConversation(latest, workspaceId, conversationId);
    if (!located.workspace || !located.conversation) return;
    const source = located.conversation;
    const through = source.contexts[Math.min(index, source.contexts.length) - 1];
    if (!through) return;
    const origin = forkOrigin(latest, source);
    const forkOf = { conversationId: origin.conversationId, number: nextForkNumber(latest, origin.conversationId) };

    // Created before any await, like a branch: `createConversation` reads the
    // render-time document. It nests under the source's root in the sidebar.
    const created = createConversation(
      workspaceId,
      "global",
      source.settings,
      undefined,
      source.parentConversationId ?? source.id,
      [],
      source.presetId,
      "",
      [],
      undefined,
      { title: forkTitle(origin.title, forkOf.number), forkOf },
      source.settings.toolLock
    );
    if (!created) return;
    try {
      // The host copies from its own committed document, so the new
      // conversation and any pending edit must be on disk before it reads.
      await flushLatestDocument();
      settleConversationTitle(created).catch((error) => console.error("Could not settle the fork's title", error));
      const contexts = await forkConversationContexts({
        workspaceId,
        sourceConversationId: conversationId,
        targetConversationId: created,
        throughContextId: through.id,
        sourceContexts: source.contexts
      });
      const current = documentStore.current();
      if (!current) return;
      const next: AppDocument = {
        ...current,
        workspaces: current.workspaces.map((workspace) => workspace.id === workspaceId ? {
          ...workspace,
          conversations: workspace.conversations.map((conversation) => (
            conversation.id === created
              ? { ...conversation, contexts, updatedAt: new Date().toISOString() }
              : conversation
          ))
        } : workspace)
      };
      // Receipts issued by the fork are consumed by this save, so it must not
      // wait for the debounce.
      await persistDocumentImmediately(next);
      setContextUsage((currentUsage) => ({
        ...currentUsage,
        [created]: estimateActiveContextUsage(contexts)
      }));
      window.requestAnimationFrame(() => window.document
        .querySelector<HTMLElement>('[data-main-context-stream="true"]')
        ?.scrollTo({ top: 1e9 }));
    } catch (error) {
      // The fork exists even if its history could not be copied; the user can fork again.
      console.error("Could not copy the forked history", error);
    }
  };


  const selectConversationBranch = async (forkContextId: string, branchId: string) => {
    const workspaceId = activeWorkspaceId;
    const conversationId = activeConversationId;
    const latest = documentStore.current();
    if (
      !latest
      || !workspaceId
      || !conversationId
      || contextMutationIsBlocked(conversationId)
    ) return;
    const located = findConversation(latest, workspaceId, conversationId);
    if (!located.conversation) return;
    const switched = switchConversationBranch(
      located.conversation,
      forkContextId,
      branchId,
      new Date().toISOString()
    );
    if (!switched) return;
    const nextDocument: AppDocument = {
      ...latest,
      workspaces: latest.workspaces.map((workspace) => workspace.id === workspaceId ? {
        ...workspace,
        conversations: workspace.conversations.map((conversation) => (
          conversation.id === conversationId ? switched : conversation
        ))
      } : workspace)
    };

    // The edits on record were made to the branch that is leaving the screen; undoing one
    // would reach into the branch arriving.
    timelineHistoryRef.current!.forget(timelineHistoryKey(conversationId));
    modelRunController.addPreparingRun(conversationId);
    try {
      await persistDocumentImmediately(nextDocument);
      setEditor(null);
      setContextUsage((current) => ({
        ...current,
        [conversationId]: estimateActiveContextUsage(switched.contexts)
      }));
      setModelRunErrors((current) => {
        const next = { ...current };
        delete next[conversationId];
        return next;
      });
      window.requestAnimationFrame(() => window.document
        .querySelector<HTMLElement>('[data-main-context-stream="true"]')
        ?.scrollTo({ top: 1e9 }));
    } catch {
      // The branch changed locally; the store retries persistence and the title bar surfaces failure.
    } finally {
      modelRunController.deletePreparingRun(conversationId);
    }
  };

  const retryActiveModelRun = async () => {
    if (!document || !activeWorkspace || !activeConversation) return;
    const failed = modelRunErrors[activeConversation.id];
    if (!failed || contextMutationIsBlocked(activeConversation.id)) return;
    const retryChoice = modelChoiceForConversation(document);
    const provider = retryChoice.provider;
    const model = retryChoice.model;
    if (
      !provider?.enabled
      || !hasUsableBaseUrl(provider)
      || !model
      || !model.id.trim()
    ) {
      openGlobalSettings("providers");
      return;
    }
    if (!supportsVision(model) && contextsContainProjectedImages(activeConversation.contexts)) return;
    const requestContexts = [...activeConversation.contexts];
    const workspaceId = activeWorkspace.id;
    const conversationId = activeConversation.id;
      modelRunController.addPreparingRun(conversationId);
      try {
        await flushLatestDocument();
      } catch {
        // Do not start a request until API configuration has persisted safely.
        return;
      } finally {
        modelRunController.deletePreparingRun(conversationId);
      }
      await performModelRun(workspaceId, conversationId, {
        ...failed.request,
        provider,
        model,
        reasoningEffort: activeConversation.settings.reasoningEffort,
        conversationId,
        workspacePath: activeWorkspace.path,
        enabledTools: [...activeEnabledTools],
        contexts: requestContexts,
        tools: activeConversationTools
      });
  };

  /** Limit waiting after stop confirmation. Timeout does not mean cancellation failed; keep the run card visible and allow another idempotent stop request. */
  const STOP_SETTLE_TIMEOUT_MS = 15_000;

  const stopModelRun = async (
    conversationId: string,
    onCancellationAcknowledged?: () => Promise<void>
  ): Promise<boolean> => {
    if (modelStoppingIdsRef.current.has(conversationId)) return false;
    const running = modelRunController.current()[conversationId];
    if (!running || modelRunController.runToken(conversationId) !== running.requestId) return false;

    const nextStopping = new Set(modelStoppingIdsRef.current);
    nextStopping.add(conversationId);
    modelStoppingIdsRef.current = nextStopping;
    setModelStoppingIds(nextStopping);
    try {
      // Conversation id is the stable stop key across reloads; previews lack a conversation-level hub and use request id.
      const acknowledged = (await cancelConversationRun(conversationId))
        || (await cancelModelRun(running.requestId));
      if (!acknowledged) {
        throw new Error(t(
          "后端未确认这个运行仍处于活动状态",
          "The backend did not confirm that this run was still active"
        ));
      }
      await onCancellationAcknowledged?.();
      // The cancellation command only publishes the stop flag. Keep the run and
      // its task cards visible until the run promise has joined every scoped
      // worker and the send pipeline has persisted the backend's terminal
      // records — but bounded: an uncooperative teardown must not hold the
      // stop button hostage forever.
      const settled = await Promise.race([
        modelRunController
          .waitForRunExit(conversationId, running.requestId)
          .then(() => true as const),
        new Promise<false>((resolve) => {
          window.setTimeout(() => resolve(false), STOP_SETTLE_TIMEOUT_MS);
        })
      ]);
      if (settled) {
        setModelRunErrors((current) => {
          if (!(conversationId in current)) return current;
          const next = { ...current };
          delete next[conversationId];
          return next;
        });
      }
      return settled;
    } catch {
      return false;
    } finally {
      const remaining = new Set(modelStoppingIdsRef.current);
      remaining.delete(conversationId);
      modelStoppingIdsRef.current = remaining;
      setModelStoppingIds(remaining);
    }
  };

  const stopBrowserAutomation = async (conversationId: string): Promise<boolean> => {
    if (browserAutomationStoppingIdsRef.current.has(conversationId)) return false;
    if (!browserAutomationToolForRun(modelRunController.current()[conversationId])) return false;
    const nextStopping = new Set(browserAutomationStoppingIdsRef.current);
    nextStopping.add(conversationId);
    browserAutomationStoppingIdsRef.current = nextStopping;
    setBrowserAutomationStoppingIds(nextStopping);
    try {
      return await stopModelRun(conversationId, async () => {
        // Browser cleanup must follow the cancellation ACK but precede the
        // settlement wait: the in-flight browser call may itself need this
        // explicit stop in order to unwind.
        if (!hasBackendRuntime()) return;
        try {
          const status = await performBrowserAction(conversationId, "stop");
          browserController.updateStatuses((current) => ({ ...current, [conversationId]: status }));
        } catch {
          // Browser cleanup failed after the preview tool stopped; the next toggle reconciles the surface.
        }
      });
    } catch {
      return false;
    } finally {
      const remaining = new Set(browserAutomationStoppingIdsRef.current);
      remaining.delete(conversationId);
      browserAutomationStoppingIdsRef.current = remaining;
      setBrowserAutomationStoppingIds(remaining);
    }
  };
  /**
   * Stop subagent and workflow tasks through their task-level channel. A task-level stop never
   * escalates to cancelling the whole run: a row the host cannot address just fails to stop, and
   * silently killing the conversation's run instead is the bug that made a task close look like a
   * session interrupt. The host settles the stopped task like any other terminal result — it folds
   * into the timeline, wakes an idle conversation, and its body says the user closed it — so the
   * renderer writes no abort record of its own; the real terminal row is the truth.
   * Preview closure is different: it destroys a live Chromium process and profile. A model-driven
   * preview stops automation instead, which does stop the run, by an older and separate decision.
   * Stopping a dev server takes its page down with it — the page is a view of the process, and one
   * left pointing at a port nothing answers on is the state this row exists to prevent.
   */
  const stopTaskItem = async (item: TaskItem) => {
    const conversationId = item.conversationId;
    const stoppingKey = JSON.stringify([conversationId, item.id]);
    if (!conversationId || stoppingTaskIds.includes(stoppingKey)) return;
    if (item.kind === "browser" && !item.automationTool) {
      setStoppingTaskIds((current) => [...current, stoppingKey]);
      try {
        await requestBrowserSessionClose(item.sessionId);
      } finally {
        setStoppingTaskIds((current) => current.filter((id) => id !== stoppingKey));
      }
      return;
    }
    setStoppingTaskIds((current) => [...current, stoppingKey]);
    try {
      if (item.kind === "preview") {
        await stopPreviewServer(item.server.handle);
        // The host's own change event closes the page too, but not before this row has already
        // gone; closing here is what makes one click read as one action.
        await closePagesServedAt(conversationId, item.server);
      } else if (item.kind === "terminal") {
        await destroyTerminalSession(item.terminal.conversationId, item.terminal.terminalId);
      } else if (item.kind === "shell") {
        await stopShellTask(item.shell.conversationId, item.shell.shellTaskId);
      } else if (item.kind === "browser") {
        // The no-automation path returned above; only stopping the model driving this page remains.
        await stopBrowserAutomation(conversationId);
      } else if (item.kind === "subagent" || item.kind === "workflow") {
        // Resolve subagent and workflow runs by model name, then workflow label. A miss means the
        // host has no live entry for this row — report nothing and leave the run alone.
        const address = item.agent.name || item.agent.label;
        if (address) await stopConversationTask(conversationId, address);
      }
    } finally {
      setStoppingTaskIds((current) => current.filter((id) => id !== stoppingKey));
    }
  };

  if (loadError) {
    return (
      <WindowFrame chrome={windowChrome} bare>
        <ErrorView message={loadError} onReset={async () => { const next = await resetDocument(); documentStore.load(next); setLoadError(null); }} />
      </WindowFrame>
    );
  }
  if (!document) {
    return (
      <WindowFrame chrome={windowChrome} bare>
        <div className="app-loading">
          <MeworkMark className="app-loading__mark" /><p>{t("正在打开 Mework…", "Opening Mework…")}</p>
        </div>
      </WindowFrame>
    );
  }

  /**
   * Where a new terminal can start: the shells a workspace's machine was probed to have, and with
   * more than one workspace, the workspace first and its shells beside it. The top-right terminal
   * button and the terminal pane's `+` both open it.
   */
  const newTerminalMenu: { sections: PopoverMenuSection[]; submenu?: "flyout" } = activeTerminalWorkspaces.length > 1
    ? {
      submenu: "flyout",
      sections: [{
        id: "workspaces",
        label: t("在哪个工作区打开", "Open in which workspace"),
        items: terminalWorkspaceMenuItems({
          workspaces: activeTerminalWorkspaces,
          sshMachines: document.globalSettings.executionEnvironments.sshMachines,
          shellsFor: terminalShellsOn,
          emptyLabel: t("未探测到可用的 shell", "No shells detected"),
          disabled: activeWorkspaceLifecycleOperationRunning,
          onSelect: openTerminalIn
        })
      }]
    }
    : {
      sections: [{
        id: "shells",
        label: activeSelectedWorkspace
          ? t("在 {name} 打开", "Open in {name}", {
            name: workspaceDirectoryLabel(activeSelectedWorkspace.path)
          })
          : undefined,
        items: terminalShellMenuItems(
          activeTerminalShells,
          (shell) => openTerminalIn(activeWorkspaceMember, shell),
          t("未探测到可用的 shell", "No shells detected"),
          activeWorkspaceLifecycleOperationRunning
        )
      }]
    };

  /**
   * Draws one open side pane.
   *
   * Every pane is a `SidePane` pseudo-window, so the title bar, the close button and the focus
   * reporting are written once here rather than in each surface. A pane whose subject has gone —
   * an agent that no longer exists, a shell task the host dropped — renders nothing; the effects
   * above close it, and returning null keeps the frame from drawing an empty window in the gap.
   */
  const renderPane = (pane: SidePaneId): ReactNode => {
    if (!activeConversation) return null;
    const conversation = activeConversation;
    const conversationId = conversation.id;
    const kind = paneKind(pane);
    const target = paneTarget(pane);
    const onFocus = () => dispatchSidePanes({ type: "focus", conversationId, pane });
    const expanded = activeExpandedPane === pane;
    const onToggleExpand = () => dispatchSidePanes({ type: "toggle_expand", conversationId, pane });

    if (kind === "terminal") {
      // Terminals are the host identity's; for the draft that is the id it will materialize as,
      // and the host reads the shell's directory off the project it is aimed at.
      const ownerId = activeHostConversationId ?? conversationId;
      const draftWorkspaceId = draftActive
        ? draftConversation?.workspaceId ?? TEMPORARY_WORKSPACE_ID
        : null;
      const terminals = terminalTabsFor(terminalTabsState, ownerId);
      // A tab is named after the shell it runs, the way the reference shell names its tabs after
      // the program running in them, and numbered by how many of that shell the conversation
      // has opened: each shell counts on its own, and a closed one keeps its number.
      const tabLabel = (tab: TerminalTab) => {
        if (tab.name) return tab.name;
        const shell = tab.launch?.shell;
        return shell
          ? `${terminalShellLabel(shell)} ${tab.number}`
          : t("终端 {n}", "Terminal {n}", { n: tab.number });
      };
      // The read-only page: a command the model ran, named the way its task row is. A command the
      // host has already let go of has nothing left to show, so its page is not drawn; the
      // eviction that dropped the row closes the page too.
      const readOnlyTask = terminals.readOnly === null ? null
        : activeShellTasks.find((task) => task.shellTaskId === terminals.readOnly) ?? null;
      const readOnlyLabel = readOnlyTask ? shellTaskTitle(readOnlyTask) : "";
      const readOnlyDirectory = readOnlyTask ? shellTaskDirectory(readOnlyTask) : null;
      const readOnlyOpen = terminalPaneOpen && terminals.activeId === READ_ONLY_TERMINAL_TAB_ID;
      return (
        <SidePane
          id={pane}
          title={t("终端", "Terminal")}
          onFocus={onFocus}
          expanded={expanded}
          onToggleExpand={onToggleExpand}
          onClose={() => closePane(pane)}
          // The tabs are this pane's whole chrome, so they take the title bar and the pane draws
          // no title of its own — the reference shell puts nothing else above a terminal.
          header={(
            <TerminalTabBar
              tabs={[
                ...(readOnlyTask ? [{
                  id: READ_ONLY_TERMINAL_TAB_ID,
                  label: readOnlyLabel,
                  title: readOnlyTask.command,
                  content: readOnlyDirectory === null ? undefined : (
                    <PathText
                      prefix={`${readOnlyTask.toolName}:`}
                      path={readOnlyDirectory}
                      title={null}
                    />
                  ),
                  readOnly: true
                }] : []),
                ...terminals.tabs.map((tab) => ({ id: tab.id, label: tabLabel(tab) }))
              ]}
              activeId={terminals.activeId}
              panelId={(terminalId) => terminalPanelId(ownerId, terminalId)}
              closingIds={new Set(terminals.tabs
                .filter((tab) => (
                  terminalSessions[terminalSessionKey(ownerId, tab.id)]?.phase === "closing"
                ))
                .map((tab) => tab.id))}
              onSelect={(terminalId) => dispatchTerminalTabs({
                type: "activate", conversationId: ownerId, terminalId
              })}
              onClose={(terminalId) => closeTerminalTab(conversationId, terminalId)}
              onReorder={(terminalIds) => dispatchTerminalTabs({
                type: "reorder",
                conversationId: ownerId,
                terminalIds
              })}
              onRename={(terminalId, name) => {
                const tab = terminals.tabs.find((candidate) => candidate.id === terminalId);
                // Submitting the name the terminal already shows is not a rename: it keeps
                // following the derived one, so it still loses its number when the others go.
                dispatchTerminalTabs({
                  type: "rename",
                  conversationId: ownerId,
                  terminalId,
                  name: tab && name.trim() === tabLabel(tab) ? "" : name
                });
              }}
              add={newTerminalMenu}
            />
          )}
        >
          {readOnlyTask && (
            // Stacked with the shells and kept mounted like them, so coming back to it is a
            // repaint rather than a fresh subscribe and replay.
            <section
              id={terminalPanelId(ownerId, READ_ONLY_TERMINAL_TAB_ID)}
              className={`collapse-region terminal-panel-region${readOnlyOpen ? "" : " collapse-region--closed"}`}
              aria-label={readOnlyLabel}
              aria-hidden={!readOnlyOpen || undefined}
              inert={!readOnlyOpen || undefined}
            >
              <div className="collapse-region__inner terminal-panel-region__inner">
                <ShellTaskPanel
                  task={readOnlyTask}
                  open={readOnlyOpen}
                  stopping={stoppingTaskIds.includes(JSON.stringify([conversationId, readOnlyTask.shellTaskId]))}
                  onStop={() => void stopShellTask(readOnlyTask.conversationId, readOnlyTask.shellTaskId)}
                />
              </div>
            </section>
          )}
          {terminals.tabs.map((tab) => (
            <TerminalPanel
              key={tab.id}
              ref={(handle) => {
                const key = terminalSessionKey(ownerId, tab.id);
                if (handle) terminalPanelHandlesRef.current.set(key, handle);
                else terminalPanelHandlesRef.current.delete(key);
              }}
              conversationId={ownerId}
              terminalId={tab.id}
              label={tabLabel(tab)}
              launch={tab.launch ?? undefined}
              draftWorkspaceId={draftWorkspaceId}
              // A project the draft was just aimed at may still be on its way to the host.
              beforeOpen={draftWorkspaceId
                ? () => awaitWorkspaceAtHost(documentStore, draftWorkspaceId)
                : undefined}
              // Every tab stays mounted; only the selected one is on screen. A hidden tab keeps
              // its shell, its scrollback and its box, so coming back to it is a repaint.
              open={terminalPaneOpen && tab.id === terminals.activeId}
              initialState={terminalSessions[terminalSessionKey(ownerId, tab.id)]}
              inputDisabledReason={terminalInputDisabledReason}
              onCommandStart={() => beginTerminalCommand(ownerId, tab.id)}
              onStateChange={updateTerminalSession}
              onClose={async () => {
                await requestTerminalSessionClose(ownerId, tab.id);
              }}
              onCleanExit={() => closeTerminalTab(conversationId, tab.id)}
            />
          ))}
        </SidePane>
      );
    }

    if (kind === "preview") {
      // A session another conversation minted has no pane here; the registry is per conversation,
      // and this guard covers the window where the layout and the registry disagree.
      if (!target || !currentPreviewSessions.includes(target)) return null;
      const automationHolds = isPrimaryPreviewSession(target, activeHostConversationId ?? conversationId)
        && (Boolean(activePreviewPageTool) || activeBrowserAutomationStopping);
      const pageWorkspace = previewWorkspaceOf(sidePanesState, target);
      const multipleWorkspaces = activeTerminalWorkspaces.length > 1;
      const sshMachines = document.globalSettings.executionEnvironments.sshMachines;
      // A page is named after what it shows — its title, or its host — and a page with nothing
      // loaded after the workspace whose start page it is. The number the composer's chip carries
      // marks which workspace a page belongs to once there is more than one to tell apart.
      const pageTabs = currentPreviewSessions.map((sessionId) => {
        const member = previewWorkspaceOf(sidePanesState, sessionId);
        const workspace = activeTerminalWorkspaces[member - 1] ?? null;
        const status = browserStatuses[sessionId];
        const address = status?.url && status.url !== "about:blank" ? splitBrowserAddress(status.url) : null;
        const directory = workspace ? workspaceDirectoryLabel(workspace.path) : t("预览", "Preview");
        return {
          id: sessionId,
          label: address ? (status?.title?.trim() || address.host) : directory,
          title: address
            ? status?.url
            : workspace
              ? workspaceLocationTitle(workspace.path, workspace.machine, sshMachines)
              : undefined,
          icon: workspace?.machine ? machineIcon(workspace.machine, 11) : undefined,
          badge: multipleWorkspaces ? String(member) : undefined
        };
      });
      const pageMachine = activeTerminalWorkspaces[pageWorkspace - 1]?.machine ?? null;
      return (
        <BrowserPanel
          paneId={pane}
          tabs={(
            <PreviewPageTabs
              tabs={pageTabs}
              activeId={target}
              onSelect={(sessionId) => void openBrowserTab(sessionId)}
              onClose={(sessionId) => void closePreviewPage(sessionId)}
              onReorder={(sessionIds) => dispatchSidePanes({
                type: "reorder_previews",
                conversationId,
                sessionIds
              })}
              add={multipleWorkspaces
                ? previewWorkspaceMenuItems({
                  workspaces: activeTerminalWorkspaces,
                  sshMachines,
                  disabled: activeWorkspaceLifecycleOperationRunning,
                  onSelect: openPreviewPage
                })
                : () => openPreviewPage(1)}
            />
          )}
          onPaneFocus={onFocus}
          onPaneClose={() => closePane(pane)}
          paneExpanded={expanded}
          onPaneToggleExpand={onToggleExpand}
          onAttachImage={(file) => void addComposerImages(conversationId, [file])}
          onElementPicked={(element) => {
            composerController.updateElementPicks((current) => ({
              ...current,
              [conversationId]: [...(current[conversationId] ?? []), element]
            }));
            // The crop rides the ordinary attachment path, so it passes the same vision, size
            // and budget gates as any dragged-in image. It just never shows up as one: the
            // chip is what the user made, so the chip is the only thing the composer draws,
            // and recording the attachment on the pick is what lets the chip take it away.
            if (!element.screenshotBase64) return;
            void addComposerImages(conversationId, [selectedElementImageFile(element)])
              .then((added) => {
                const screenshotImageId = added[0]?.id;
                if (!screenshotImageId) return;
                composerController.updateElementPicks((current) => ({
                  ...current,
                  [conversationId]: (current[conversationId] ?? []).map((pick) => (
                    pick.sequence === element.sequence ? { ...pick, screenshotImageId } : pick
                  ))
                }));
              });
          }}
          onContentBoundsChange={syncBrowserPanelBounds}
          paneTrailing={automationHolds ? (
            <IconButton
              label={activeBrowserAutomationStopping
                ? t("正在停止页面操作", "Stopping page automation")
                : t("停止页面操作", "Stop page automation")}
              disabled={activeBrowserAutomationStopping}
              onClick={() => void stopBrowserAutomation(conversationId)}
            >
              {activeBrowserAutomationStopping
                ? <LoaderCircle size={12} className="spin" />
                : <Square size={9} fill="currentColor" />}
            </IconButton>
          ) : null}
          native={isTauriRuntime()}
          sessionId={target}
          initialStatus={browserStatuses[target] ?? null}
          // Covered by another pane's expand, this pane keeps a full-size box it is no longer
          // allowed to draw in — the tile is only made `visibility:hidden`, so its rectangle still
          // measures. The host parks the page for the same reason, and the panel has to agree, or
          // it goes on reporting the page as covered and swapping in snapshots nobody can see.
          active={activeExpandedPane === null || expanded}
          // The page's own workspace, on whichever machine it is: its start page lists that
          // workspace's servers and runs them there. The file picker only ever reads this computer.
          target={previewTargetForPage(target)}
          fileTarget={activePrimaryGitTarget}
          linkNotice={remoteLinkNotice(pageMachine)}
          onReservedBottomChange={(reservedBottom) => reservePreviewBottom(target, reservedBottom)}
          onOpenPage={() => openBrowserTab(target)}
        />
      );
    }

    if (kind === "review") {
      if (!activeWorkspace || activeReviewPages.length === 0) return null;
      const page = activeReviewPages.find((entry) => entry.member === activeReviewMember)
        ?? activeReviewPages[0]!;
      const sshMachines = document.globalSettings.executionEnvironments.sshMachines;
      // A page is named after what it reviews: the workspace, the branch — the one a worktree
      // was forked from, which is what its changes are read against — and the worktree itself.
      const pageName = (entry: typeof page) => ({
        workspace: workspaceDirectoryLabel(entry.registered.path),
        branch: entry.worktree?.baseBranch
          ?? entry.snapshot.branch
          ?? entry.snapshot.head?.slice(0, 8)
          ?? t("尚无提交", "No commits yet"),
        worktree: entry.worktree ? worktreeName(entry.worktree) : null
      });
      const pageTabs = activeReviewPages.length > 1 ? (
        <PageTabs
          ariaLabel={t("审阅的工作区", "Workspaces under review")}
          moreLabel={t("更多工作区", "More workspaces")}
          maxTabWidth={220}
          tabs={activeReviewPages.map((entry) => {
            const name = pageName(entry);
            const failure = gitSurfaceFailures[entry.key];
            return {
              id: String(entry.member),
              label: [name.workspace, name.branch, name.worktree].filter(Boolean).join(" → "),
              content: <GitReviewPageLabel {...name} />,
              title: [
                workspaceLocationTitle(entry.registered.path, entry.registered.machine, sshMachines),
                entry.worktree?.path,
                failure && t("最近一次读取失败，显示的是上次的状态：{reason}", "The last read failed; this is the state from before: {reason}", {
                  reason: failure
                })
              ].filter(Boolean).join("\n"),
              icon: failure
                ? <CircleAlert size={11} className="git-review__page-stale" />
                : entry.registered.machine ? machineIcon(entry.registered.machine, 11) : undefined
            };
          })}
          activeId={String(page.member)}
          onSelect={(id) => setReviewPageMembers((current) => ({ ...current, [conversationId]: Number(id) }))}
          onReorder={(ids) => setReviewPageOrders((current) => ({
            ...current,
            [conversationId]: ids.map(Number)
          }))}
        />
      ) : undefined;
      // The conversations sharing this page's checkout: every one working in the workspace's own
      // directory, unless this conversation reviews a worktree of its own.
      const pagePeers = activeWorkspacePeers.map((peer) => ({
        id: peer.id,
        worktree: worktreeFor(peer, page.member, page.registered)
      }));
      return (
        <GitReviewPanel
          key={`${page.member}:${gitReviewSnapshotCacheKey(page.snapshot)}`}
          paneId={pane}
          target={page.target}
          snapshot={page.snapshot}
          active
          checkout={{
            worktreeName: page.worktree ? worktreeName(page.worktree) : null,
            baseBranch: page.worktree?.baseBranch ?? null,
            baseOid: page.worktree?.baseOid ?? null
          }}
          pageTabs={pageTabs}
          revealRequest={reviewPaneRequest?.conversationId === conversationId
            && reviewPaneRequest.member === page.member
            ? reviewPaneRequest
            : null}
          onRevealRequestHandled={onReviewPaneRequestHandled}
          mutationDisabledReason={gitMutationDisabledReasonFor(page.member)}
          paneExpanded={expanded}
          onPaneToggleExpand={onToggleExpand}
          onPaneFocus={onFocus}
          onPaneClose={() => closePane(pane)}
          onSnapshotChange={(next) => updateGitSnapshots((current) => (
            gitSnapshotsAfterWorkspaceMutation(
              current,
              // Broadcast Git snapshots only to conversations using the same checkout. Drafts use
              // the workspace root; worktree conversations do not.
              gitSnapshotBroadcastIds(pagePeers, conversationId, Boolean(page.worktree))
                .map((id) => gitSnapshotKey(id, page.member)),
              page.surfaceKey,
              next
            )
          ))}
          onMutationStart={() => beginGitMutation(conversationId, page.member)}
          onMutationEnd={() => endGitMutation(conversationId)}
        />
      );
    }

    if (kind === "files") {
      if (!filesPaneAvailable) return null;
      return (
        <FilesPane
          key={conversationId}
          paneId={pane}
          workspaces={filesPaneWorkspaces}
          sshMachines={document.globalSettings.executionEnvironments.sshMachines}
          hostWindows={hostIsWindows(platform)}
          active
          openRequest={filesPaneRequest?.conversationId === conversationId ? filesPaneRequest : null}
          onOpenRequestHandled={onFilesPaneRequestHandled}
          expanded={expanded}
          onToggleExpand={onToggleExpand}
          onPaneFocus={onFocus}
          onPaneClose={() => closePane(pane)}
        />
      );
    }

    if (kind === "tasks") {
      const tab = activeLayout.tasksTab;
      return (
        <SidePane
          id={pane}
          // The tabs take the title bar; the pane's region goes by the page on show.
          title={tasksPaneTabLabel(tab, t)}
          header={<TasksPaneTabBar activeTab={tab} onSelect={showTasksTab} />}
          onFocus={onFocus}
          expanded={expanded}
          onToggleExpand={onToggleExpand}
          onClose={() => closePane(pane)}
        >
          {tab === "history" ? (
            <HistoryPane
              conversationId={conversationId}
              contexts={conversation.contexts}
              streaming={activeModelRunBusy}
            />
          ) : (
            <TasksPane
              agents={subagents}
              terminals={activeTaskTerminals}
              shellTasks={activeShellTasks}
              previewServers={previewServers}
              browserSessions={activeBrowserSessions}
              browserSessionId={conversationId}
              conversationId={conversationId}
              browserAutomationTool={activePreviewPageTool}
              browserAutomationStopping={activeBrowserAutomationStopping}
              modelRequestId={taskSources.modelRequestId}
              userAbortedTasks={conversation.userAbortedTasks}
              forkDecisions={forkDecisions[conversationId] ?? []}
              inheritedModelId={taskSources.inheritedModelId}
              plan={activePlan}
              planAwaitingApproval={planAwaitingApproval}
              selectedAgentId={selectedSubagentId}
              selectedRowId={selectedTaskRowId}
              stoppingIds={stoppingTaskIds.flatMap((key) => {
                const [stoppingConversationId, itemId] = JSON.parse(key) as [string, string];
                return stoppingConversationId === conversationId ? [itemId] : [];
              })}
              workflowProgress={workflowProgressByRun}
              workflowRunIds={workflowRunIdsByRun}
              onWorkflowStepControl={handleWorkflowStepControl}
              onSelectAgent={(agentId) => openSubagentPanel(agentId)}
              onOpenItem={(item) => void openTaskItemPage(item)}
              onStopItem={(item) => void stopTaskItem(item)}
            />
          )}
        </SidePane>
      );
    }

    if (kind === "history") {
      // One agent's history. It reads the same record from the same store as
      // the tasks pane's history tab and differs only in whose entries it asks for.
      const view = target ? findOpenableSubagentView(subagents, target) : null;
      if (!view) return null;
      return (
        <SidePane
          id={pane}
          title={t("{label} 的历史记录", "{label} · history", { label: view.label })}
          onFocus={onFocus}
          expanded={expanded}
          onToggleExpand={onToggleExpand}
          onClose={() => closePane(pane)}
        >
          <HistoryPane
            conversationId={conversationId}
            contexts={conversation.contexts}
            streaming={activeModelRunBusy}
            /* A spawned agent's entries are keyed by its name; a workflow
               step's by the run-scoped address the host publishes on its shell.
               A view that carries neither — a step shell written before the
               address rode along — asks for nothing rather than falling back to
               the conversation's own entries. */
            owners={view.ledgerOwner ? [view.ledgerOwner] : []}
          />
        </SidePane>
      );
    }

    if (kind === "plan") {
      if (!activePlan) return null;
      const plan = activePlan;
      return (
        <SidePane
          id={pane}
          title={planTitle(plan.markdown) ?? t("实施计划", "Implementation plan")}
          onFocus={onFocus}
          expanded={expanded}
          onToggleExpand={onToggleExpand}
          onClose={() => closePane(pane)}
          trailing={(
            <span className="plan-page__status" data-plan-status={plan.status ?? "draft"}>
              {planStatusLabel(plan, planAwaitingApproval, t)}
            </span>
          )}
        >
          <PlanPane
            plan={plan}
            awaitingApproval={planAwaitingApproval}
            dock={activeToolPrompt && approvalDockOwner === "plan" ? (
              /* The exit card is answered here, beside the plan it is about. */
              <ToolApprovalDock
                pending={activeToolPrompt}
                stack={activeToolPromptStack}
                onDecide={(decision, feedback) => {
                  decideToolPrompt(conversationId, activeToolPrompt.promptId, decision, feedback);
                }}
              />
            ) : undefined}
          />
        </SidePane>
      );
    }

    if (kind === "subagent") {
      const view = selectedSubagentView;
      if (!view) return null;
      // A tab still filed under the call id its agent has since left resolves to the agent's
      // view, and the effects above move it over; until then the two must not draw twice.
      const tabAgents = [...new Map(activeLayout.subagentTabs.flatMap((subagentId) => {
        const agent = findOpenableSubagentView(subagents, subagentId);
        return agent ? [[agent.id, agent] as const] : [];
      })).values()];
      const ledgerPane = subagentHistoryPaneId(view.id);
      return (
        <SidePane
          id={pane}
          title={view.label}
          onFocus={onFocus}
          expanded={expanded}
          onToggleExpand={onToggleExpand}
          onClose={() => closePane(pane)}
          // Agents share this pane as its tabs, so the strip takes the title bar and names the
          // shown one; the pane's region keeps that agent's name.
          header={(
            <SubagentTabBar
              agents={tabAgents}
              activeId={view.id}
              onSelect={openSubagentPanel}
              onClose={(subagentId) => dispatchSidePanes({ type: "close_subagent", conversationId, subagentId })}
              onReorder={(subagentIds) => dispatchSidePanes({
                type: "reorder_subagents",
                conversationId,
                subagentIds
              })}
            />
          )}
          /* What happened in this agent, beside the transcript it settled
             into. The two are not the same account: the transcript is what the
             child kept, the history is every payload it sent and everything
             that came back. */
          trailing={(
            <IconButton
              className="side-pane__ledger"
              label={t("{label} 的历史记录", "{label} · history", { label: view.label })}
              aria-pressed={paneIsOpen(activeLayout, ledgerPane)}
              onMouseDown={(event) => event.preventDefault()}
              onClick={() => togglePane(ledgerPane)}
            >
              <History size={13} aria-hidden="true" />
            </IconButton>
          )}
        >
          <StreamedSubagentPanel
            // Each agent's transcript is its own: switching tabs must not carry one's scroll
            // and open disclosures over to the next.
            key={view.id}
            conversation={conversation}
            modelRunController={modelRunController}
            externalStepBodies={externalStepBodies}
            selectedSubagentId={view.id}
            tools={activeConversationTools}
            chromeless
            pathBaseDir={timelinePathBaseDir}
            onSelectAgent={(agentId) => openSubagentPanel(agentId)}
            onClose={() => closePane(pane)}
            dock={approvalDockOwner === pane ? (
              /* Render approval cards on subagent panes so their source-navigation target remains visible and actionable. */
              <ToolApprovalDock
                pending={activeToolPrompt}
                stack={activeToolPromptStack}
                onDecide={(decision, feedback) => {
                  if (activeToolPrompt) {
                    decideToolPrompt(conversationId, activeToolPrompt.promptId, decision, feedback);
                  }
                }}
              />
            ) : undefined}
          />
        </SidePane>
      );
    }

    if (kind === "settings") {
      return (
        <SidePane
          id={pane}
          title={t("对话设置", "Conversation settings")}
          onFocus={onFocus}
          expanded={expanded}
          onToggleExpand={onToggleExpand}
          onClose={() => closePane(pane)}
        >
          <ConversationSettings
            conversation={conversation}
            globalSettings={document.globalSettings}
            tools={activeConversationTools}
            capabilities={document.capabilities}
            onChange={saveActiveConversationComposition}
            onChangeConversationOnly={updateActiveConversationSettingsOnly}
            onApplyPreset={applyPresetToActiveConversation}
            onRenamePreset={renameConversationPreset}
            onDeletePreset={deleteConversationPreset}
            onSavePreset={saveConversationPreset}
            onSavePresetCopy={saveConversationPresetCopy}
            onBindPresetTemplate={bindPresetTemplate}
            templates={conversationTemplates}
            onReadTemplate={readTemplateBody}
            onWriteTemplate={writeTemplateBody}
            onDeleteCapability={(kind, resource) => void deleteCapabilityResource(kind, resource)}
            workspaceId={activeWorkspace?.kind === "directory" ? activeWorkspace.id : null}
            onRescanCapabilities={rescanCapabilitiesFromPane}
            onRevealCapabilityLocation={(kind, workspaceId) => (
              void revealCapabilityDirectory(kind, workspaceId)
            )}
            onProbeMcpServer={probeCapabilityMcpServer}
            capabilityError={capabilityError}
            presetError={templateError}
            onCreatePreset={createConversationPreset}
          />
        </SidePane>
      );
    }

    return null;
  };

  const activeProjectName = activeWorkspace
    ? isTemporaryWorkspace(activeWorkspace) ? t("临时项目", "Temporary project") : activeWorkspace.name
    : t("未选择项目", "No project selected");
  // A draft has no conversation to rename yet, and a project being deleted takes no edits.
  const titleLocked = draftActive || Boolean(activeWorkspaceId && deletingWorkspaceIds.has(activeWorkspaceId));
  const finishTitleRename = () => {
    if (!titleDraft) return;
    const title = titleDraft.value.trim();
    const conversation = activeConversation?.id === titleDraft.conversationId ? activeConversation : null;
    if (conversation && activeWorkspace && title && title !== conversation.title) {
      renameConversation(activeWorkspace.id, conversation.id, title);
    }
    setTitleDraft(null);
  };

  return (
    <CommonErrorBoundary>
      <WindowLayerProvider>
      <WindowFrame chrome={windowChrome}>
      <div
        className={`app-shell ${sidebarOpen ? "" : "app-shell--sidebar-closed"} ${sidebarResizing || paneResizing ? "app-shell--resizing" : ""}`}
        style={{
          "--sidebar-width": `${sidebarWidth}px`
        } as AppShellStyle}
      >
        <ShellNav
          sidebarOpen={sidebarOpen}
          onToggleSidebar={() => setSidebarOpen((open) => !open)}
          canGoBack={Boolean(historyBack)}
          canGoForward={Boolean(historyForward)}
          onBack={() => stepConversationHistory(historyBack)}
          onForward={() => stepConversationHistory(historyForward)}
          onSearch={() => setConversationSearchOpen(true)}
        />
        <Sidebar
            workspaces={document.workspaces}
            sshMachines={document.globalSettings.executionEnvironments.sshMachines}
            activeWorkspaceId={activeWorkspaceId}
            activeConversationId={activeConversationId}
            onSelectConversation={selectConversation}
            onNewConversation={openDraftConversation}
            onAddWorkspace={() => { setAssignWorkspaceAfterAdd(false); setWorkspaceDialogOpen(true); }}
            onEditProject={(workspaceId) => setProjectEditor(workspaceId)}
            onRenameConversation={renameConversation}
            onDeleteConversation={deleteConversation}
            onDeleteWorkspace={deleteWorkspace}
            isConversationRunning={conversationIsBusy}
            conversationStatus={conversationStatus}
            conversationPresets={workspacePresetOptions}
            onSetWorkspaceDefaultPreset={setWorkspaceDefaultPreset}
            isWorkspaceDeleting={(workspaceId) => deletingWorkspaceIds.has(workspaceId)}
            onOpenSettings={() => openGlobalSettings("providers")}
            open={sidebarOpen}
            width={sidebarWidth}
            onWidthChange={updateSidebarWidth}
            onResizeStateChange={setSidebarResizing}
            onReorderWorkspace={reorderWorkspace}
            onReorderConversation={reorderSidebarConversation}
          />

        {/* Portaled so no ancestor transform can turn the tray's fixed box into a layout box. */}
        {createPortal(
          <ForkRequestTray
            requests={forkRequests}
            onDecide={decideForkRequest}
            onOpenSource={selectConversation}
          />,
          window.document.body
        )}

        <PathChoiceMenu />

        <main className="main-pane">
          {/* This is the window's top row, so its empty stretches move the window. */}
          <header className="topbar" data-tauri-drag-region="deep">
            <div className="topbar__leading">
              {activeConversation ? (
                <div className="conversation-title">
                  <h1 className="conversation-title__heading">
                    {titleDraft?.conversationId === activeConversation.id ? (
                      <span className="conversation-title__field" data-value={titleDraft.value}>
                        <input
                          className="conversation-title__input"
                          aria-label={t("对话标题", "Conversation title")}
                          size={1}
                          autoFocus
                          value={titleDraft.value}
                          onChange={(event) => setTitleDraft((current) => current ? { ...current, value: event.target.value } : current)}
                          onFocus={(event) => event.currentTarget.select()}
                          onBlur={finishTitleRename}
                          onKeyDown={(event) => {
                            if (isImeKeyEvent(event.nativeEvent)) return;
                            if (event.key === "Enter") {
                              event.preventDefault();
                              event.currentTarget.blur();
                            } else if (event.key === "Escape") {
                              event.preventDefault();
                              setTitleDraft(null);
                            }
                          }}
                        />
                      </span>
                    ) : (
                      <button
                        type="button"
                        className="conversation-title__name"
                        disabled={titleLocked}
                        aria-label={titleLocked
                          ? activeConversation.title
                          : t("对话标题：{title}，点击重命名", "Conversation title: {title}. Click to rename", { title: activeConversation.title })}
                        title={titleLocked ? undefined : t("点击重命名", "Click to rename")}
                        onClick={() => setTitleDraft({ conversationId: activeConversation.id, value: activeConversation.title })}
                      >
                        {activeConversation.title}
                      </button>
                    )}
                  </h1>
                  <span className="conversation-title__project">{activeProjectName}</span>
                </div>
              ) : (
                <div className="topbar__empty">
                  <h1 className="topbar__empty-title">Mework</h1>
                  <span className="topbar__empty-tagline">{t("喵。开工。", "Mew. Work.")}</span>
                </div>
              )}
            </div>
            <div className="topbar__actions">
              {saveStatusChip}
              {activeConversation && (
                <PaneToolbar
                  buttons={[
                    {
                      id: "terminal",
                      label: t("终端", "Terminal"),
                      activeLabel: t("终端（命令运行中）", "Terminal (command running)"),
                      icon: <SquareTerminal size={18} aria-hidden="true" />,
                      pressed: terminalPaneOpen,
                      activity: Object.values(terminalSessions).some((session) => (
                        session.conversationId === activeHostConversationId && session.busy
                      )),
                      onToggle: () => togglePane("terminal"),
                      // A new shell, as the terminal pane's `+` offers it; showing or hiding
                      // the terminals already open is the row after them.
                      menu: {
                        label: t("新建终端", "New terminal"),
                        submenu: newTerminalMenu.submenu,
                        sections: [
                          ...newTerminalMenu.sections,
                          {
                            id: "pane",
                            items: [{
                              id: "toggle",
                              label: terminalPaneOpen
                                ? t("收起终端面板", "Hide the terminal pane")
                                : t("显示终端面板", "Show the terminal pane"),
                              icon: <PanelRight size={14} />,
                              onSelect: () => togglePane("terminal")
                            }]
                          }
                        ]
                      }
                    },
                    {
                      id: "review",
                      label: t("审阅", "Review"),
                      activeLabel: t("审阅（有未提交改动）", "Review (uncommitted changes)"),
                      icon: <GitCompareArrows size={18} aria-hidden="true" />,
                      pressed: gitReviewPanelOpen,
                      activity: activeReviewPages.some((entry) => !entry.snapshot.isClean),
                      // Without a repository among the conversation's workspaces there is nothing to review.
                      disabled: activeReviewPages.length === 0,
                      title: activeReviewPages.length === 0
                        ? t("这个任务的工作区都不是 Git 仓库", "None of this task's workspaces is a Git repository")
                        : undefined,
                      onToggle: () => (gitReviewPanelOpen
                        ? closePane("review")
                        : openGitReview())
                    },
                    {
                      id: "preview",
                      label: t("预览", "Preview"),
                      activeLabel: t("预览（模型正在操作页面）", "Preview (the model is driving the page)"),
                      icon: <Globe size={18} aria-hidden="true" />,
                      pressed: browserPanelOpen,
                      activity: Boolean(activePreviewPageTool),
                      onToggle: () => (openPreviewSessionId
                        ? closePane(previewPaneId(openPreviewSessionId))
                        : showPreviewPanel()),
                      // With one workspace the button is the pane's toggle. With more, a page is
                      // opened for a workspace: the menu lists them — each opening that
                      // workspace's start page — and the row after them shows or hides the pane,
                      // on the page last seen or, with none yet, the chip-selected workspace's.
                      menu: activeTerminalWorkspaces.length > 1
                        ? {
                          label: t("打开预览", "Open a preview"),
                          sections: [
                            {
                              id: "workspaces",
                              label: t("在哪个工作区打开", "Open in which workspace"),
                              items: previewWorkspaceMenuItems({
                                workspaces: activeTerminalWorkspaces,
                                sshMachines: document.globalSettings.executionEnvironments.sshMachines,
                                disabled: activeWorkspaceLifecycleOperationRunning,
                                onSelect: openPreviewPage
                              })
                            },
                            {
                              id: "pane",
                              items: [{
                                id: "toggle",
                                label: browserPanelOpen
                                  ? t("收起预览面板", "Hide the preview pane")
                                  : t("打开预览面板", "Open the preview pane"),
                                icon: <PanelRight size={14} />,
                                onSelect: () => (openPreviewSessionId
                                  ? closePane(previewPaneId(openPreviewSessionId))
                                  : showPreviewPanel())
                              }]
                            }
                          ]
                        }
                        : undefined
                    }
                  ]}
                  menuItems={[
                    {
                      id: "files",
                      label: t("文件", "Files"),
                      icon: <Folder size={14} aria-hidden="true" />,
                      checked: paneIsOpen(activeLayout, "files"),
                      disabled: !filesPaneAvailable,
                      onSelect: () => togglePane("files")
                    },
                    // Two rows for the one pane: each is that tab on show.
                    {
                      id: "tasks",
                      label: t("任务", "Tasks"),
                      icon: <ListChecks size={14} aria-hidden="true" />,
                      checked: shownTasksTab(activeLayout) === "tasks",
                      onSelect: () => toggleTasksTab("tasks")
                    },
                    {
                      id: "history",
                      label: t("历史记录", "History"),
                      icon: <History size={14} aria-hidden="true" />,
                      checked: shownTasksTab(activeLayout) === "history",
                      onSelect: () => toggleTasksTab("history")
                    },
                    {
                      id: "settings",
                      label: t("对话设置", "Conversation settings"),
                      icon: <SlidersHorizontal size={14} aria-hidden="true" />,
                      checked: paneIsOpen(activeLayout, "settings"),
                      onSelect: () => togglePane("settings")
                    }
                  ]}
                />
              )}
            </div>
          </header>

          {/* A workspace is optional for entering a draft; sending uses the temporary workspace. Panes that need a real workspace remain individually guarded. */}
          {activeConversation ? (
            <PaneTiles
              onResizeStateChange={setPaneResizing}
              columns={activeLayout.columns}
              sideFlex={activeLayout.sideFlex}
              columnFlex={activeLayout.columnFlex}
              paneFlex={activeLayout.paneFlex}
              expanded={activeExpandedPane}
              onSideFlexChange={(sideFlex) => dispatchSidePanes({
                type: "set_side_flex",
                conversationId: activeConversation.id,
                sideFlex
              })}
              onColumnFlexChange={(columnFlex) => dispatchSidePanes({
                type: "set_column_flex",
                conversationId: activeConversation.id,
                columnFlex
              })}
              onPaneFlexChange={(paneFlex) => dispatchSidePanes({
                type: "set_pane_flex",
                conversationId: activeConversation.id,
                paneFlex
              })}
              panes={[
                ...activeLayout.panes.flatMap((pane) => {
                  const node = renderPane(pane);
                  return node ? [{ id: pane, node }] : [];
                }),
                // The terminal stays mounted while its pane is closed. Unmounting it would run the
                // panel's teardown, which reports the session idle, and a still-running shell would
                // vanish from the task rows and from every terminal-busy guard that reads them.
                ...(!terminalPaneOpen
                  ? [{ id: "terminal" as SidePaneId, node: renderPane("terminal"), hidden: true }]
                  : [])
              ]}
              chat={(
            <div className="conversation-pane">
              <StreamedConversationView
                className="conversation-pane__main"
                timelinePlaceholder={activeBodyUnloaded ? (
                  <div className="conversation-view__body-loading" role="status">
                    {bodyLoadFailures[activeConversation.id] ? (
                      <>
                        <span>
                          {t("对话内容载入失败：{error}", "The conversation could not be loaded: {error}", {
                            error: bodyLoadFailures[activeConversation.id] ?? ""
                          })}
                        </span>
                        <button
                          type="button"
                          className="button button--small"
                          onClick={() => loadActiveBody(activeConversation.id)}
                        >
                          {t("重试", "Retry")}
                        </button>
                      </>
                    ) : (
                      <span>{t("正在载入对话…", "Loading the conversation…")}</span>
                    )}
                  </div>
                ) : undefined}
                conversation={activeConversation}
                conversationTurns={conversationTurns[activeConversation.id]}
                modelRunController={modelRunController}
                onRetryTurnError={() => void retryActiveModelRun()}
                onDismissTurnError={() => clearModelRunError(activeConversation.id)}
                retryableTurnRequestId={
                  modelRunErrors[activeConversation.id]?.retryable
                    ? modelRunErrors[activeConversation.id]?.requestId ?? null
                    : null
                }
                tools={activeTimelineTools}
                enabledTools={activeEnabledTools}
                pathBaseDir={timelinePathBaseDir}
                timelineMutationLocked={activeTimelineMutationBlocked}
                onEdit={handleContextEdit}
                onDelete={deleteContext}
                onEditQuestion={handleQuestionEdit}
                onDeleteQuestion={deleteQuestionContext}
                editor={editor}
                questionEditor={
                  questionEditor && questionEditor.conversationId === activeConversation.id
                    ? questionEditor
                    : null
                }
                onCancelEdit={closeTimelineEditors}
                onSaveText={saveTextContext}
                onAddAttachments={addAttachmentsToTimelineMessage}
                attachmentImageInput={timelineImageInput}
                onSaveTool={saveToolContext}
                onSaveToolEdit={saveToolContextEdit}
                onSaveQuestion={saveQuestionContext}
                onBranchFrom={(item) => void branchFromUserContext(item)}
                branchFromDisabledReason={branchFromDisabledReason}
                onForkAt={draftActive ? undefined : (index) => void forkConversationAt(index)}
                forkDisabledReason={forkDisabledReason}
                branchNavigations={activeBranchNavigations}
                onSelectBranch={(forkContextId, branchId) => void selectConversationBranch(forkContextId, branchId)}
                branchSwitchDisabledReason={branchSwitchDisabledReason}
                onInsert={handleContextInsert}
                onDeleteContexts={deleteTimelineContexts}
                onUndo={() => stepTimelineHistory(activeConversation.id, "undo")}
                onRedo={() => stepTimelineHistory(activeConversation.id, "redo")}
                onOpenSubagent={(subagentId) => openSubagentPanel(subagentId)}
                agents={subagents}
                taskMessages={taskMessages}
                onOpenWorkflowRun={focusWorkflowRunPanel}
                runningTaskCount={runningTaskCount}
                onOpenTasks={openTasksPane}
                composer={(
                  <>
                    <div className="composer-wrap">
                {/* The docks live inside .composer-wrap so the floating context
                    row anchors above them instead of covering them. */}
                <ToolApprovalDock
                  /* Exactly one surface owns the card; the composer takes it only when no open pane claims it. */
                  pending={approvalDockOwner === "composer" ? activeToolPrompt : null}
                  stack={activeToolPromptStack}
                  onDecide={(decision, feedback) => {
                    if (activeToolPrompt) {
                      decideToolPrompt(activeConversation.id, activeToolPrompt.promptId, decision, feedback);
                    }
                  }}
                />
                <QuestionDock
                  prompt={approvalDockOwner === "composer" ? activeToolPrompt : null}
                  stack={activeToolPromptStack}
                  disabled={activeWorkspaceLifecycleOperationRunning}
                  onRespond={(response) => {
                    if (activeToolPrompt) {
                      void answerQuestionPrompt(activeConversation.id, activeToolPrompt.promptId, response);
                    }
                  }}
                  onDraftChange={rememberQuestionDraft}
                />
                <QueuedMessageList
                  messages={visibleQueuedMessages}
                  canSteer={(message) => {
                    const running = modelRunSummaries[activeConversation.id];
                    return Boolean(
                      running
                      && (!message.images?.length || running.supportsVision)
                    );
                  }}
                  steeringIds={steeringMessageIds}
                  failedPromotionIds={failedQueuedPromotionIds}
                  onSteer={(message) => void steerQueuedMessage(activeConversation.id, message)}
                  onRetry={(message) => retryFailedQueuedPromotion(
                    activeWorkspace?.id ?? "",
                    activeConversation.id,
                    message.id
                  )}
                  onDelete={(message) => deleteQueuedMessage(
                    activeWorkspace?.id ?? "",
                    activeConversation.id,
                    message.id
                  )}
                />
                {/* Project, workspace, and branch describe where the conversation runs, so they sit above the composer rather than inside it. */}
                <div className="composer-context">
                    {/* Floats above this row, whatever it holds: it belongs to the timeline
                        over it, and says what the last edit there did. */}
                    {timelineNotice?.conversationId === activeConversation.id && (
                      <div key={timelineNotice.id} className="timeline-notice" role="status">
                        <span>{timelineNotice.message}</span>
                        {timelineNotice.hint && (
                          <>
                            <kbd>{timelineNotice.hint.keys}</kbd>
                            <span>{timelineNotice.hint.action}</span>
                          </>
                        )}
                      </div>
                    )}
                    {/* The project is chosen before the task starts; once the conversation has
                        content it belongs to that project for good, and the chip goes away. */}
                    {!activeConversationStarted && (
                      <ProjectSelector
                        projects={document.workspaces}
                        activeProject={activeWorkspace}
                        sshMachines={document.globalSettings.executionEnvironments.sshMachines}
                        disabled={deletingConversationIds.has(activeConversation.id)}
                        disabledReason={t("对话正在删除", "The conversation is being deleted")}
                        isProjectDeleting={(projectId) => deletingWorkspaceIds.has(projectId)}
                        onSelect={(projectId) => {
                          // Drafts change one field; persisted conversations must relocate.
                          if (draftActive) setDraftWorkspace(projectId);
                          else void moveActiveConversation(projectId);
                        }}
                        onCreateProject={() => {
                          setAssignWorkspaceAfterAdd(true);
                          setWorkspaceDialogOpen(true);
                        }}
                      />
                    )}
                    {/* Shown for a single workspace too: besides picking what Git shows, its
                        menu is where a workspace's variables and its machine's settings open. */}
                    {activeProjectWorkspaces.length > 0 && (
                      <WorkspaceMemberSelector
                        workspaces={activeProjectWorkspaces}
                        selected={activeWorkspaceMember}
                        sshMachines={document.globalSettings.executionEnvironments.sshMachines}
                        showIndex={activeProjectWorkspaces.length + activeConversation.attachedWorkspaces.length > 1}
                        onSelect={(member) => setSelectedWorkspaceMembers((current) => ({
                          ...current,
                          [activeConversation.id]: member
                        }))}
                        onConfigureMachine={(machine) => setMachineSettings({ machine })}
                        onConfigureWorkspace={(member) => {
                          // The registered directory, not the worktree standing in for workspace 1:
                          // its settings belong to the directory the worktree was checked out from.
                          const registered = projectWorkspaces(activeWorkspace)[member - 1];
                          if (!registered) return;
                          setWorkspaceSettings({
                            machine: registered.machine ?? null,
                            path: registered.path,
                            name: workspaceDirectoryLabel(registered.path)
                          });
                        }}
                      />
                    )}
                    {/* Branch and worktree are chosen before the task starts, like the project:
                        once the conversation has content it runs where it began, and the group goes. */}
                    {activeGitSnapshot && !activeConversationStarted && (
                      <div className="composer-chip-group">
                        <PopoverMenu
                          triggerClassName="composer-chip composer-chip--flush"
                          trigger={<>
                            <GitBranch size={13} />
                            <span className="composer-chip__label">
                              {activeBranchLabel ?? t("游离 HEAD", "Detached HEAD")}
                            </span>
                            <ChevronDown size={11} className="composer-chip__caret" />
                          </>}
                          triggerLabel={t("分支：{name}", "Branch: {name}", {
                            name: activeBranchLabel ?? t("游离 HEAD", "Detached HEAD")
                          })}
                          triggerTitle={activeGitTarget
                            ? undefined
                            : t(
                              "分支：{name}（工作区在另一台机器上，只能在那台机器上切换分支）",
                              "Branch: {name} (this workspace is on another machine; switch branches there)",
                              { name: activeBranchLabel ?? t("游离 HEAD", "Detached HEAD") }
                            )}
                          disabled={branchChipDisabled}
                          menuLabel={t("切换分支", "Switch branch")}
                          menuWidth={260}
                          placement="above"
                          searchPlaceholder={t("搜索分支…", "Search branches…")}
                          emptyLabel={branchPicker?.status === "loading"
                            ? t("正在读取分支…", "Reading branches…")
                            : branchPicker?.message ?? t("没有匹配的分支", "No matching branches")}
                          onOpen={loadBranchPicker}
                          sections={[{
                            id: "branches",
                            items: (branchPicker?.conversationId === activeConversation.id
                              ? branchPicker.branches
                              : []).map((branch) => ({
                              id: branch.name,
                              label: branch.name,
                              checked: branch.current,
                              // This menu operates on the workspace root, so require disabling an active worktree rather than silently switching the wrong checkout.
                              disabled: Boolean(activeWorktree),
                              title: activeWorktree
                                ? t(
                                  "本对话正跑在隔离工作树上；先关掉工作树再切换分支",
                                  "This conversation runs in an isolated worktree; turn it off before switching branches"
                                )
                                : undefined,
                              onSelect: () => void checkoutComposerBranch(branch.name)
                            }))
                          }]}
                        />
                        {/* Each workspace of a directory project can run on a worktree of its own, on
                            whichever machine it is; the box is the selected workspace's. */}
                        {activeWorkspace?.kind === "directory" && <>
                        <span className="composer-chip-group__divider" aria-hidden="true" />
                        <label
                          className="composer-worktree"
                          title={draftActive
                            ? t(
                              "在一份独立检出上运行本任务；工作树在你发出第一条消息时建立",
                              "Run this task on its own checkout. The worktree is created when you send the first message."
                            )
                            : t(
                              "在一份独立检出上运行本对话，与项目里别的对话互不干扰",
                              "Run this conversation on its own checkout, isolated from other conversations in this project"
                            )}
                        >
                          <input
                            type="checkbox"
                            checked={activeWorktreeChecked}
                            disabled={branchChipDisabled}
                            onChange={(event) => void toggleConversationWorktree(event.target.checked)}
                          />
                          <span>{t("工作树", "worktree")}</span>
                        </label>
                        </>}
                      </div>
                    )}
                    {/* Attached workspaces sit with the other "where this runs" chips, and the
                        picker button trails them so a new one lands where the button was.
                        Both are frozen while a turn runs: the host snapshots the granted set when it
                        builds the request, so a grant given or taken back mid-run would show here
                        without reaching the calls that turn is still making.

                        The number is shown only when there is more than one workspace, which is
                        exactly when the host states the numbers to the model — a lone chip with a
                        "2" on it would be an address for something the model never sees. */}
                    {activeConversation.attachedWorkspaces.map((workspace, position) => (
                      <span
                        key={`${runEnvKey(workspace.machine)} ${workspace.path}`}
                        className="composer-chip composer-chip--static"
                        title={workspaceChipTitle(
                          workspace,
                          document.globalSettings.executionEnvironments.sshMachines
                        )}
                      >
                        <WorkspaceMachineIcon machine={workspace.machine} />
                        <span className="composer-chip__index" aria-hidden="true">{position + activeProjectWorkspaceCount + 1}</span>
                        <span className="composer-chip__label">{directoryLabel(workspace.path)}</span>
                        <button
                          type="button"
                          className="composer-chip__remove"
                          aria-label={t("{name} 的设置", "Settings for {name}", {
                            name: directoryLabel(workspace.path)
                          })}
                          title={t("{name} 的设置", "Settings for {name}", {
                            name: directoryLabel(workspace.path)
                          })}
                          onClick={() => setWorkspaceSettings({
                            machine: workspace.machine ?? null,
                            path: workspace.path,
                            name: directoryLabel(workspace.path)
                          })}
                        >
                          <Settings size={11} />
                        </button>
                        <button
                          type="button"
                          className="composer-chip__remove"
                          aria-label={t("移除工作区：{path}", "Remove workspace: {path}", { path: workspace.path })}
                          disabled={activeModelRunning}
                          onClick={() => detachWorkspace(workspace)}
                        >
                          <X size={11} />
                        </button>
                      </span>
                    ))}
                    {hasNativeWorkspacePicker() && (
                      <PopoverMenu
                        triggerClassName="composer-chip composer-chip--icon"
                        trigger={<FolderPlus size={13} />}
                        triggerLabel={t("附加工作区", "Attach a workspace")}
                        disabled={activeModelRunning}
                        menuLabel={t("在哪台机器上选目录", "Which machine to pick a directory on")}
                        menuWidth={244}
                        placement="above"
                        emptyLabel={t("没有可选的机器", "No machines to choose from")}
                        onOpen={loadMachineMenu}
                        sections={[{
                          id: "machines",
                          items: [
                            {
                              id: "local",
                              label: t("本机", "This machine"),
                              onSelect: () => void attachLocalWorkspace()
                            },
                            ...(machineMenuDistros ?? []).map((distro) => ({
                              id: `wsl:${distro.name}`,
                              label: distro.name,
                              onSelect: () => setRemoteWorkspacePicker({
                                machine: { kind: "wsl", distro: distro.name },
                                name: distro.name,
                                purpose: "attach"
                              })
                            })),
                            ...document.globalSettings.executionEnvironments.sshMachines.map((machine) => ({
                              id: `ssh:${machine.id}`,
                              label: machine.name,
                              onSelect: () => setRemoteWorkspacePicker({
                                machine: { kind: "ssh", machineId: machine.id },
                                name: machine.name,
                                purpose: "attach"
                              })
                            }))
                          ]
                        }]}
                      />
                    )}
                    {activeGitSnapshot && (
                      <GitStatusCard
                        git={activeGitSnapshot}
                        gitOpen={gitReviewPanelOpen && activeReviewMember === activeWorkspaceMember}
                        onOpenGitReview={() => openGitReview(activeWorkspaceMember)}
                      />
                    )}
                  </div>
                {forkStartError && (
                  <div className="composer-run-error" role="alert">
                    <span>{forkStartError}</span>
                    <button type="button" onClick={() => {
                      setForkStartError(null);
                      forkStartsAttemptedRef.current.clear();
                      setForkRetrySignal((current) => current + 1);
                    }}>{t("重试分叉首轮", "Retry pending fork runs")}</button>
                  </div>
                )}
                {branchChipError && (
                  <p className="composer-context__error" role="alert">
                    <CircleAlert size={13} />
                    <span>{branchChipError}</span>
                  </p>
                )}
                <div
                  ref={composerDrop.ref}
                  className="composer"
                  data-attachment-drop-ready={composerDrop.dragging && !composerDrop.over ? "true" : undefined}
                >
                  {composerIsUnsentTask && <ComposerCat />}
                  <AttachmentDropOverlay state={composerDrop} imageInput={activeComposerImageInput} />
                  {/* A failure that reached a turn is read in the timeline, where it
                      happened. Only failures with no turn to land on — a send refused
                      before the run started — still need the composer to carry them. */}
                  {modelRunErrors[activeConversation.id]
                    && !modelRunErrors[activeConversation.id]?.requestId
                    && !modelRunSummaries[activeConversation.id] && (
                    <div className="composer-run-error" role="alert">
                      <CircleAlert size={15} />
                      <span>
                        <strong>{modelRunErrors[activeConversation.id]?.providerName} · {modelRunErrors[activeConversation.id]?.modelName}</strong>
                        <small>{modelRunErrors[activeConversation.id]?.message}</small>
                      </span>
                      {modelRunErrors[activeConversation.id]?.retryable && (
                        <button type="button" onClick={() => void retryActiveModelRun()}>
                          {t("重试", "Retry")}
                        </button>
                      )}
                      <IconButton
                        label={t("关闭模型错误", "Dismiss model error")}
                        onClick={() => clearModelRunError(activeConversation.id)}
                      ><X size={13} /></IconButton>
                    </div>
                  )}
                  {activeComposerImagesUnsupported && (
                    <div className="composer-run-error" role="alert">
                      <CircleAlert size={15} />
                      <span>
                        <strong>{activeModelChoice?.provider.name} · {activeModelChoice?.model.id}</strong>
                        <small>{t(
                          "这条消息带有图片，但当前模型没有启用图片输入，因此无法发送。请换一个支持视觉的模型，或移除图片。",
                          "This message carries images, but the selected model has no image input enabled, so it cannot be sent. Switch to a vision model, or remove the images."
                        )}</small>
                      </span>
                    </div>
                  )}
                  <SelectedElementChips
                    elements={activeElementPicks}
                    onRemove={(sequence) => {
                      const removed = activeElementPicks.find((pick) => pick.sequence === sequence);
                      composerController.updateElementPicks((current) => ({
                        ...current,
                        [activeConversation.id]: (current[activeConversation.id] ?? [])
                          .filter((pick) => pick.sequence !== sequence)
                      }));
                      // The chip is the only handle the crop has: the strip does not
                      // draw it, so nothing else could take it off this message.
                      if (removed?.screenshotImageId) {
                        removeComposerImage(activeConversation.id, removed.screenshotImageId);
                      }
                    }}
                  />
                  <AttachmentNotice
                    rejected={activeComposerAttachmentNotice}
                    onDismiss={() => composerController.updateAttachmentNotices((current) => {
                      const next = { ...current };
                      delete next[activeConversation.id];
                      return next;
                    })}
                  />
                  <ImageStrip
                    images={activeComposerVisibleImages}
                    files={activeComposerFiles}
                    compact
                    busy={activeComposerImageLoading}
                    className="composer__images"
                    onRemove={(imageId) => removeComposerImage(activeConversation.id, imageId)}
                    onRemoveFile={(fileId) => removeComposerFile(activeConversation.id, fileId)}
                  />
                  <div className="composer__input">
                    <div className="composer__field pasted-text-host">
                      {composerPasteTags.layer}
                      <textarea
                        ref={composerTextareaRef}
                        rows={1}
                        value={activeComposerDraft}
                        placeholder={activeQuestionPending
                          ? t("发送消息会先交回卡片上已填的回答…", "Sending hands back the card's answers first…")
                          : t("向 Agent 发送消息…", "Message the Agent…")}
                        aria-label={t("向 Agent 发送消息", "Message the Agent")}
                        disabled={activeWorkspaceLifecycleOperationRunning}
                        spellCheck={appearance.spellCheck}
                        onPaste={(event) => {
                          const files = Array.from(event.clipboardData.files);
                          const text = event.clipboardData.getData("text/plain");
                          if (files.length) {
                            // A clipboard carrying both keeps its text; the files ride along.
                            if (!text) event.preventDefault();
                            void addComposerAttachments(activeConversation.id, files);
                            return;
                          }
                          // A long paste folds into a tag the send expands again,
                          // so the box stays readable and the model reads it all.
                          composerPasteTags.onPaste(event);
                        }}
                        onChange={(event) => {
                          const draft = event.target.value;
                          const conversationId = activeConversation.id;
                          composerController.updateDrafts((current) => ({ ...current, [conversationId]: draft }));
                        }}
                        onKeyDown={(event) => {
                          // Send and newline shortcuts are configurable but mutually exclusive, so checking send first cannot trigger both.
                          if (isImeKeyEvent(event.nativeEvent)) return;
                          if (matchesEvent(appearance.sendShortcut, event.nativeEvent)) {
                            event.preventDefault();
                            void sendActiveComposer();
                            return;
                          }
                          if (matchesEvent(appearance.newlineShortcut, event.nativeEvent)) {
                            // Let the textarea handle newline insertion and its undo stack; unmatched keys also pass through.
                            return;
                          }
                        }}
                      />
                    </div>
                    <div className="composer__send">
                      {activeModelRunning && activeComposerQueuesMessage && (
                        // Queuing repurposes the primary button, but a running conversation must retain a visible stop control.
                        <button
                          type="button"
                          className="send-button send-button--stop"
                          aria-label={activeModelStopping
                            ? t("正在停止生成", "Stopping generation")
                            : t("停止生成", "Stop generating")}
                          disabled={activeModelStopping}
                          onClick={() => void stopModelRun(activeConversation.id)}
                        >
                          <span className="stop-icon" aria-hidden="true" />
                        </button>
                      )}
                      <button
                        type="button"
                        className={`send-button${(activeModelRunning && !activeComposerQueuesMessage) ? " send-button--stop" : ""}`}
                        aria-label={activeComposerQueuesMessage
                          ? activeQuestionPending
                            ? t("发送", "Send")
                            : t("加入排队消息", "Queue message")
                          : activeModelRunning
                            ? activeModelStopping
                              ? t("正在停止生成", "Stopping generation")
                              : t("停止生成", "Stop generating")
                            : activeWorkspaceDeletionRunning
                              ? t("项目删除中", "Project deletion in progress")
                              : t("发送", "Send")}
                        disabled={activeModelStopping || (activeComposerImageLoading && !activeComposerStopsRun) || (!activeModelRunning && (activeWorkspaceLifecycleOperationRunning || !activeModelChoice || activeComposerImagesUnsupported))}
                        onClick={() => activeComposerQueuesMessage
                          ? void sendActiveComposer()
                          : activeModelRunning
                            ? void stopModelRun(activeConversation.id)
                            : void sendActiveComposer()}
                      >
                        {(activeModelRunning && !activeComposerQueuesMessage)
                          ? <span className="stop-icon" aria-hidden="true" />
                          : <ArrowUp size={17} />}
                      </button>
                    </div>
                  </div>
                </div>
                <div className="composer__footer">
                  <div className="composer__tools">
                    <PopoverMenu
                      triggerClassName="composer-option"
                      trigger={<span className="composer-option__label">{activeSecurityLevelLabel}</span>}
                      triggerLabel={t("安全层级：{name}", "Security level: {name}", {
                        name: activeSecurityLevelLabel
                      })}
                      /* Movable at any time, a streaming turn and a waiting approval
                         card included: the host moves the running turn's level at once. */
                      disabled={activeWorkspaceLifecycleOperationRunning}
                      menuLabel={t("安全层级", "Security level")}
                      menuWidth={256}
                      sections={[{
                        id: "security",
                        items: SECURITY_LEVEL_OPTIONS.map((option) => ({
                          id: option,
                          label: securityLevelLabelFor(option),
                          checked: activeConversation.settings.securityLevel === option,
                          onSelect: () => {
                            // Security options are conversation-only settings, not preset composition; update them directly without detaching a preset.
                            updateActiveConversation((conversation) => ({
                              ...conversation,
                              settings: { ...conversation.settings, securityLevel: option }
                            }));
                          }
                        }))
                      }]}
                    />
                    {/* Plan mode is a switch of its own, not a level: on, the model plans and
                       asks for approval before it changes anything; an approved plan turns
                       it off. Movable at any time, a streaming turn included — the next
                       round boundary tells the model. */}
                    <button
                      type="button"
                      className="composer-option composer-plan-toggle"
                      aria-pressed={Boolean(activeConversation.settings.planModeEnabled)}
                      disabled={activeWorkspaceLifecycleOperationRunning
                        || (!activeConversation.settings.planModeEnabled && activePlanModeLocked)}
                      title={!activeConversation.settings.planModeEnabled && activePlanModeLocked
                        ? t(
                          "当前模型不能在对话中途加入工具，而计划模式要加入 plan 和 exit_plan_mode；换个模型或开新对话",
                          "The current model cannot take tools mid-conversation, and plan mode adds plan and exit_plan_mode; switch models or start a new conversation"
                        )
                        : activeConversation.settings.planModeEnabled
                          ? t("计划模式已开启：模型先写计划、请你批准，批准后自动关闭", "Plan mode is on: the model writes a plan and asks you to approve it; approval turns it off")
                          : t("开启计划模式：模型改动任何东西之前先写计划并请你批准", "Turn on plan mode: the model writes a plan and asks you to approve it before changing anything")}
                      onClick={() => {
                        const enabled = !activeConversation.settings.planModeEnabled;
                        updateActiveConversation((conversation) => ({
                          ...conversation,
                          settings: { ...conversation.settings, planModeEnabled: enabled }
                        }));
                      }}
                    >
                      <span className="composer-option__label">{t("计划", "Plan")}</span>
                      <span className="composer-plan-toggle__dot" aria-hidden="true" />
                    </button>
                    <ComposerAddFiles
                      key={activeConversation.id}
                      disabled={activeWorkspaceLifecycleOperationRunning}
                      imageInput={activeComposerImageInput}
                      onChooseFiles={(files) => void addComposerAttachments(activeConversation.id, files)}
                    />
                  </div>
                  <div className="composer__options">
                    <PopoverMenu
                      rootClassName="composer__model"
                      triggerClassName="composer-option"
                      trigger={<span className="composer-option__label">{activeModelLabel}</span>}
                      triggerLabel={t("模型：{name}", "Model: {name}", { name: activeModelLabel })}
                      triggerTitle={activeModelLabel}
                      disabled={Boolean(modelRunSummaries[activeConversation.id]) || activeWorkspaceLifecycleOperationRunning || !enabledModelChoices.length}
                      menuLabel={t("模型", "Model")}
                      menuWidth={300}
                      align="end"
                      dense
                      searchPlaceholder={t("搜索模型…", "Search models…")}
                      emptyLabel={enabledModelChoices.length
                        ? t("没有匹配的模型", "No matching models")
                        : t("没有已启用的模型", "No enabled models")}
                      sections={enabledModelSections}
                    />
                    <PopoverMenu
                      triggerClassName="composer-option"
                      trigger={<span className="composer-option__label">{activeReasoningEffortLabel}</span>}
                      triggerLabel={t("思考程度：{name}", "Reasoning effort: {name}", {
                        name: activeReasoningEffortLabel
                      })}
                      disabled={Boolean(modelRunSummaries[activeConversation.id]) || activeWorkspaceLifecycleOperationRunning}
                      menuLabel={t("思考程度", "Reasoning effort")}
                      menuWidth={232}
                      align="end"
                      sections={[{
                        id: "effort",
                        items: REASONING_EFFORTS.map((effort) => ({
                          id: effort,
                          label: effort,
                          checked: activeReasoningEffort === effort,
                          onSelect: () => {
                            const changedAt = new Date().toISOString();
                            // Update global lastReasoningEffort with the conversation setting and its workspace snapshot; drafts update only the global side until materialization.
                            if (draftActive) {
                              setDraftConversation((current) => (current
                                ? { ...current, settings: { ...current.settings, reasoningEffort: effort } }
                                : current));
                            }
                            documentStore.update((current) => current ? {
                              ...current,
                              globalSettings: {
                                ...current.globalSettings,
                                lastReasoningEffort: effort
                              },
                              workspaces: current.workspaces.map((workspace) => {
                                if (draftActive || workspace.id !== activeWorkspace?.id) return workspace;
                                let lastConversationSettings = workspace.lastConversationSettings;
                                const conversations = workspace.conversations.map((conversation) => {
                                  if (conversation.id !== activeConversation.id) return conversation;
                                  const settings = { ...conversation.settings, reasoningEffort: effort };
                                  lastConversationSettings = settings;
                                  return { ...conversation, updatedAt: changedAt, settings };
                                });
                                return { ...workspace, conversations, lastConversationSettings };
                              })
                            } : current);
                          }
                        }))
                      }]}
                    />
                    {activeContextUsage && (
                      <ContextUsageMeter
                        contexts={activeConversation.contexts}
                        tokens={activeContextUsage.tokens}
                        estimated={activeContextUsage.estimated}
                        unprojectable={activeContextUsage.unprojectable}
                        contextWindow={activeModelChoice?.model.contextWindow ?? null}
                        counts={{
                          tools: activeConversation.settings.enabledTools.length,
                          mcpServers: activeConversation.settings.mcpIds.length,
                          skills: activeConversation.settings.skillIds.length,
                          agentRoles: activeConversation.settings.agentDefinitions.length
                        }}
                        autoCompact={document.globalSettings.autoCompact}
                        onAutoCompactChange={(autoCompact) => handleGlobalSettingsChange(
                          (current) => ({ ...current, autoCompact })
                        )}
                        autoCompactUnavailable={Boolean(activeModelChoice
                          && !appendsTools(activeModelChoice.provider, activeModelChoice.model))}
                      />
                    )}
                  </div>
                </div>
                    </div>
                  </>
                )}
              />
            </div>
              )}
            />
          ) : null}
        </main>

        {globalSettingsView && (
          <Dialog
            title={t("全局设置", "Global settings")}
            width="1040px"
            sidebar
            bodyClassName="dialog__body--flush"
            onClose={() => setGlobalSettingsView(null)}
          >
            <GlobalSettings
              initialView={globalSettingsView}
              settings={document.globalSettings}
              document={document}
              onChange={handleGlobalSettingsChange}
              onFlush={flushLatestDocument}
              onViewChange={setGlobalSettingsView}
            />
          </Dialog>
        )}
        {/* The preset's and the role's windows, opened from the conversation-settings
            pane but mounted here, where global settings is. */}
        <WindowLayerOutlet />

        {draftRetargetPrompt && (
          <Dialog
            title={t("把新任务换到别的项目？", "Move this new task to another project?")}
            description={[
              draftRetargetPrompt.shells === 0
                ? null
                : draftRetargetPrompt.shells === 1
                  ? t(
                    "它有 1 个终端还在运行 shell。",
                    "One of its terminals still has a shell running."
                  )
                  : t(
                    "它有 {count} 个终端还在运行 shell。",
                    "{count} of its terminals still have a shell running.",
                    { count: draftRetargetPrompt.shells }
                  ),
              draftRetargetPrompt.servers === 0
                ? null
                : draftRetargetPrompt.servers === 1
                  ? t(
                    "它启动的 1 个开发服务器还在运行。",
                    "One dev server it started is still running."
                  )
                  : t(
                    "它启动的 {count} 个开发服务器还在运行。",
                    "{count} dev servers it started are still running.",
                    { count: draftRetargetPrompt.servers }
                  )
            ].filter(Boolean).join(resolvedLanguage === "zh-CN" ? "" : " ")}
            onClose={() => setDraftRetargetPrompt(null)}
            width="420px"
            footer={(
              <>
                <button type="button" className="button" onClick={() => setDraftRetargetPrompt(null)}>
                  {t("取消", "Cancel")}
                </button>
                <button
                  type="button"
                  className="button button--primary"
                  onClick={() => {
                    const { ownerId, workspaceId, proceed } = draftRetargetPrompt;
                    setDraftRetargetPrompt(null);
                    // Only for the draft it asked about: one that has moved or been sent since is
                    // not the one this answer is for.
                    if (draftConversationRef.current?.materializesAs !== ownerId) return;
                    void retargetDraft(workspaceId);
                    proceed();
                  }}
                >{t("结束并换项目", "End them and move")}</button>
              </>
            )}
          >
            <p className="confirm-copy">{t(
              "终端和开发服务器跟着项目走：换项目会结束这些 shell 进程和开发服务器，并关掉它的预览与审阅面板；Git 状态会按新项目重新读取。",
              "Terminals and dev servers belong to the project: moving ends these shell processes and dev servers, and closes the task's preview and review panes. Git status is read again for the new project."
            )}</p>
          </Dialog>
        )}

        {templateSwitchPrompt && (
          <Dialog
            title={t("套用这份预设？", "Apply this preset?")}
            description={t(
              "当前时间线上的消息会被这份预设的开局消息整体替换。",
              "The messages on this timeline will be replaced wholesale by the preset's opening messages."
            )}
            onClose={() => setTemplateSwitchPrompt(null)}
            width="420px"
            footer={(
              <>
                <button type="button" className="button" onClick={() => setTemplateSwitchPrompt(null)}>
                  {t("取消", "Cancel")}
                </button>
                <button
                  type="button"
                  className="button button--primary"
                  onClick={() => {
                    const { presetId } = templateSwitchPrompt;
                    setTemplateSwitchPrompt(null);
                    const preset = documentStore.current()?.globalSettings.conversationPresets
                      .find((item) => item.id === presetId);
                    if (preset) void applyPresetBody(preset);
                  }}
                >{t("替换", "Replace")}</button>
              </>
            )}
          >
            <p className="confirm-copy">{t(
              "这段对话现在的内容不是某份预设的开局消息，替换后不会保留。",
              "This conversation's current content is not a preset's opening queue, and replacing it will not keep it."
            )}</p>
          </Dialog>
        )}

        {workspaceDialogOpen && <ProjectDialog
          mode="create"
          sshMachines={document.globalSettings.executionEnvironments.sshMachines}
          showWsl={hostIsWindows(platform)}
          nativePicker={hasNativeWorkspacePicker()}
          onPickLocalDirectory={pickWorkspaceDirectory}
          onSaveSshMachine={saveSshMachine}
          onDeleteSshMachine={deleteSshMachine}
          machineUsage={(machine) => machineUsage(document.workspaces, machine)}
          machineShells={machineShellsControl!}
          onClose={() => { setWorkspaceDialogOpen(false); setAssignWorkspaceAfterAdd(false); }}
          onSubmit={(name, workspaces) => void addWorkspace(name, workspaces)}
        />}

        {editedProject && <ProjectDialog
          mode="edit"
          initialName={editedProject.name}
          initialWorkspaces={projectWorkspaces(editedProject)}
          sshMachines={document.globalSettings.executionEnvironments.sshMachines}
          showWsl={hostIsWindows(platform)}
          nativePicker={hasNativeWorkspacePicker()}
          onPickLocalDirectory={pickWorkspaceDirectory}
          onSaveSshMachine={saveSshMachine}
          onDeleteSshMachine={deleteSshMachine}
          machineUsage={(machine) => machineUsage(document.workspaces, machine)}
          machineShells={machineShellsControl!}
          onClose={() => setProjectEditor(null)}
          onSubmit={(name, workspaces) => updateProject(editedProject.id, name, workspaces)}
        />}

        {machineSettings && <MachineSettingsDialog
          machine={machineSettings.machine}
          sshMachines={document.globalSettings.executionEnvironments.sshMachines}
          usage={machineUsage(document.workspaces, machineSettings.machine)}
          shells={machineShellsControl!}
          onSaveSshMachine={saveSshMachine}
          onDeleteSshMachine={deleteSshMachine}
          onClose={() => setMachineSettings(null)}
        />}

        {workspaceSettings && <WorkspaceSettingsDialog
          name={workspaceSettings.name}
          machine={workspaceSettings.machine}
          path={workspaceSettings.path}
          sshMachines={document.globalSettings.executionEnvironments.sshMachines}
          vars={document.globalSettings.executionEnvironments.envVars[
            workspaceEnvKey(workspaceSettings.machine, workspaceSettings.path)
          ] ?? {}}
          sandbox={document.globalSettings.executionEnvironments.sandboxes?.[
            workspaceEnvKey(workspaceSettings.machine, workspaceSettings.path)
          ]}
          onChangeVars={(vars) => saveWorkspaceEnvVars(workspaceSettings.machine, workspaceSettings.path, vars)}
          onChangeSandbox={(sandbox) => saveWorkspaceSandbox(workspaceSettings.machine, workspaceSettings.path, sandbox)}
          onClose={() => setWorkspaceSettings(null)}
        />}

        {remoteWorkspacePicker && <RemoteDirectoryPicker
          machine={remoteWorkspacePicker.machine}
          machineName={remoteWorkspacePicker.name}
          onPick={(path) => {
            const { machine } = remoteWorkspacePicker;
            setRemoteWorkspacePicker(null);
            attachWorkspace(machine, path);
          }}
          onClose={() => setRemoteWorkspacePicker(null)}
        />}

        {conversationSearchOpen && <ConversationSearch
          workspaces={document.workspaces}
          activeConversationId={activeConversationId}
          onSelect={selectConversation}
          onClose={() => setConversationSearchOpen(false)}
        />}
      </div>
      </WindowFrame>
      </WindowLayerProvider>
    </CommonErrorBoundary>
  );
}

export default App;
