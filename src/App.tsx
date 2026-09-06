import {
  ArrowUp,
  ChevronDown,
  CircleAlert,
  FolderOpen,
  FolderPlus,
  Gauge,
  GitBranch,
  LoaderCircle,
  ListChecks,
  Monitor,
  PanelLeft,
  RotateCcw,
  Settings,
  ShieldCheck,
  SlidersHorizontal,
  Sparkles,
  Square,
  SquareTerminal,
  Undo2,
  X
} from "lucide-react";
import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";
import { createPortal } from "react-dom";
import type { CSSProperties } from "react";
import { CommonErrorBoundary } from "./components/ErrorBoundary";
import { MeworkIcon } from "./components/MeworkIcon";
import { ComposerAddImages } from "./components/ComposerAddMenu";
import { ComposerCat } from "./components/ComposerCat";
import { PopoverMenu } from "./components/PopoverMenu";
import { ContextUsageMeter } from "./components/ContextUsageMeter";
import { ImageStrip } from "./components/ImageStrip";
import { ConversationSettings } from "./components/ConversationSettings";
import { ContextEditor } from "./components/ContextEditor";
import {
  StreamedConversationView,
  StreamedSubagentPanel,
  subagentViewMessages
} from "./components/StreamedConversationView";
import { QuestionDock } from "./components/QuestionDock";
import { ToolApprovalDock } from "./components/ToolApprovalDock";
import { QueuedMessageList } from "./components/QueuedMessageList";
import { QuestionEditorDialog } from "./components/QuestionEditorDialog";
import { WorkflowHistory } from "./components/WorkflowHistory";
import { Dialog, EmptyState, IconButton } from "./components/Common";
import { GlobalSettings } from "./components/GlobalSettings";
import {
  clampSidebarWidth,
  Sidebar,
  SIDEBAR_DEFAULT_WIDTH
} from "./components/Sidebar";
import { WorkspaceSelector } from "./components/WorkspaceSelector";
import { ForkRequestTray } from "./components/ForkRequestTray";
import { detachAbsentParents, reparentChildren } from "./lib/conversationTree";
import { RunLocationPicker } from "./components/RunLocationPicker";
import { GitStatusCard } from "./components/GitStatusCard";
import {
  clampTaskContainerWidth,
  TaskContainer,
  taskContainerId,
  taskContainerMessages,
  TASK_CONTAINER_DEFAULT_WIDTH,
  type TaskItem
} from "./components/TaskContainer";
import { deriveWorkflowProgress } from "./lib/workflowProgress";
import type { WorkflowProgressView } from "./lib/workflowProgress";
import {
  hasAnyTask,
  nextTaskUnreadState,
  taskActivitySignature,
  taskUnreadVisible,
  type TaskSources,
  type TaskUnreadState
} from "./lib/taskContainer";
import { reorderItems } from "./components/usePointerDrag";
import { createId } from "./lib/id";
import { estimateContextsTokens, liveContextTokens } from "./lib/contextTokens";
import { errorMessage } from "./lib/errors";
import { hasUsableBaseUrl, isEncryptedReasoning, supportsVision } from "./lib/modelCapabilities";
import {
  contextsContainProjectedImages,
  MAX_COMPOSER_IMAGE_BYTES,
  MAX_COMPOSER_IMAGE_PIXELS,
  MAX_COMPOSER_IMAGES,
  MAX_IMAGE_ATTACHMENT_BYTES,
  MAX_IMAGE_ATTACHMENT_PIXELS,
  projectedImageBudget
} from "./lib/imageBudget";
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
import { isReservedWorkspace, TEMPORARY_WORKSPACE_ID } from "./lib/workspaces";
import type { NewConversationSource } from "./lib/workspaces";
import {
  draftAsConversation,
  DRAFT_CONVERSATION_ID,
  isDraftConversationId
} from "./lib/draftConversation";
import type { DraftConversationState } from "./lib/draftConversation";
import { UsageStatsCard } from "./components/UsageStatsCard";
import {
  EMPTY_USAGE_STATISTICS,
  ensureUsageBackfill,
  fetchUsageStatistics
} from "./lib/usageStatistics";
import type { UsageStatistics } from "./lib/usageStatistics";
import { cancelConversationRun, cancelModelRun, defaultConversationWebSearchSettings, executeTool, forkConversationContexts, listPendingForkStarts, listPendingForkRequests, listPendingToolPrompts, listWakePendingConversations, loadConversationRemote, loadDocument, prepareImageAttachment, refreshCapabilities, requestToolApproval, resetDocument, resolveForkRequest, resolveToolPrompt, runModel, skipWorkflowStep, steerModelRun, workflowStepRecord } from "./lib/runtime";
import {
  answersFromFormattedContent,
  deriveAgentStatus,
  findPendingQuestion,
  isClaudeQuestionInput,
  questionsFromInput
} from "./lib/orchestration";
import type { AgentStatus, PendingQuestion, TodoItemView } from "./lib/orchestration";
import {
  deleteStateToolContext,
  restoreStateToolContexts
} from "./lib/stateToolDeletion";
import {
  approvalPromptSubagentView,
  deriveSubagentViews,
  findExternalStepBodyRef,
  findOpenableSubagentView,
  graftExternalStepBodies,
  isAddressableAgentRunTool,
  isAgentRunTool
} from "./lib/subagents";
import type { SubagentView } from "./lib/subagents";
import { BROWSER_PANEL_CHROME_HEIGHT, BrowserPanel } from "./components/BrowserPanel";
import { GitReviewPanel } from "./components/GitReviewPanel";
import { ShellTaskPanel } from "./components/ShellTaskPanel";
import { TaskPage } from "./components/TaskPage";
import { TerminalPanel, terminalPanelId } from "./components/TerminalPanel";
import {
  hasBackendRuntime,
  isBrowserDevRuntime,
  isTauriRuntime,
  onBrowserDevReconnected
} from "./lib/backend";
import { onAppPushEvent } from "./lib/appEvents";
import { hasNativeWorkspacePicker, pickWorkspaceDirectory } from "./lib/workspacePicker";
import { createTerminalController, terminalSessionKey } from "./lib/terminalController";
import type { TerminalSessionState } from "./lib/terminal";
import { listShellTasks, stopConversationTask, stopShellTask } from "./lib/shellTasks";
import type { ShellTaskSnapshot } from "./lib/shellTasks";
import {
  closeBrowserSession,
  getBrowserStatus,
  openBrowser,
  performBrowserAction,
  setBrowserPanelBounds
} from "./lib/browser";
import {
  startBrowserRendererMountHeartbeat,
  stopBrowserRendererMountHeartbeat
} from "./lib/browserRendererMount";
import type { BrowserCloseDisposition, BrowserStatus } from "./lib/browser";
import type {
  GitBranch as GitBranchInfo,
  GitCheckoutRef,
  GitReviewView,
  GitTarget,
  GitWorkspaceSnapshot
} from "./lib/git";
import {
  createConversationWorktree,
  executeGitAction,
  getGitBranches,
  gitConversationTarget,
  gitPeerBlocksMutation,
  gitWorkspaceTarget,
  releaseConversationWorktree
} from "./lib/git";
import {
  createGitController,
  gitReviewSnapshotCacheKey,
  gitSnapshotBroadcastIds,
  gitSnapshotForWorkspace,
  gitSnapshotsAfterDraftRedemption,
  gitSnapshotsAfterWorkspaceMutation
} from "./lib/gitController";
import {
  CONVERSATION_VIEW,
  initialMainPaneState,
  isPrimaryPreviewSession,
  mainPanePageDomId,
  mainPaneReducer,
  mainPaneShowsPage,
  mainPaneViewFor,
  mainPaneViewKey,
  previewSessionBelongsToConversation,
  previewSessionsFor,
  previewTabSessionId,
  type MainPaneAction,
  type MainPaneView
} from "./lib/mainPanePages";
import { getI18nSnapshot, resolveApplicationLanguage, translate, useI18n } from "./i18n";
import { configureApplicationAppearance } from "./theme";
import { ZOOM_STEP, clampZoom, defaultAppearancePreferences } from "./lib/appearance";
import {
  SHORTCUT_COMMANDS,
  matchesEvent,
  resolveShortcut,
  shouldSuppressForFocus
} from "./lib/shortcuts";
import { localizeToolDescriptor } from "./lib/toolDefaults";
import {
  applyGlobalSettingsChange,
  applyQuarantinedContextReplacements,
  conversationMatchesPersistenceGeneration,
  findConversation,
  replaceConversationFromAuthority,
  type GlobalSettingsChange
} from "./lib/documentUpdates";
import { createDocumentStore } from "./lib/documentStore";
import { createConversationSync } from "./lib/conversationSync";
import { createComposerController } from "./lib/composerController";
import { createBrowserController } from "./lib/browserController";
import {
  createModelStreamCoalescer,
  cumulativeModelRunUsage,
  reduceModelStreamEvent,
  type ModelRuns,
  type ModelRunState,
  type ModelStreamEffect,
  type StreamingHookState,
  type StreamingToolPhase,
  type StreamingToolState
} from "./lib/modelStream";
import {
  applyConversationPresetSettings,
  captureConversationPresetSettings,
  cloneConversationSettings,
  conversationPresetById,
  defaultConversationPreset,
  implicitConversationPreset,
  IMPLICIT_CONVERSATION_PRESET_ID
} from "./lib/conversationPresets";
import {
  contextBranchNavigations,
  isConversationBranchFork,
  switchConversationBranch
} from "./lib/conversationBranches";
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
  sumModelUsage,
  sumUsageByRound
} from "./lib/conversationTurns";
import type { ConversationTurn, ConversationTurnError, ConversationTurns } from "./lib/conversationTurns";
import type {
  ProviderFamily,
  ApiProvider,
  AppDocument,
  AppSurface,
  ContextItem,
  Conversation,
  ConversationSettings as ConversationSettingsType,
  GlobalSettings as GlobalSettingsType,
  ImageAttachment,
  InsertableContextKind,
  JsonObject,
  ModelProfile,
  ModelRunRequest,
  ModelStreamEvent,
  ModelUsage,
  PendingForkRequest,
  PendingToolPrompt,
  QueuedMessage,
  ReasoningEffort,
  ResourceDescriptor,
  RunTarget as RunTargetType,
  SecurityLevel,
  SshMachineConfig as SshMachineConfigType,
  SettingsView,
  ShortcutCommandId,
  SubagentLiveState,
  SubagentRunRecord,
  ToolApprovalGrant,
  ToolResult,
  ToolContext,
  ToolPromptDecision,
  UserContext,
  WorkflowProgressEntry,
  Workspace
} from "./types";

/** Stable identity for the empty projection so selectors return the same array without an active conversation. */
const NO_PROJECTED_CONTEXTS: ContextItem[] = [];

/** Stable identity for empty workflow entries when no run is active. */
const EMPTY_WORKFLOW_ENTRIES: Record<string, WorkflowProgressEntry[]> = {};
const EMPTY_WORKFLOW_RUN_IDS: Record<string, string> = {};

function todoItemsEqual(previous: TodoItemView, next: TodoItemView): boolean {
  return previous.id === next.id
    && previous.content === next.content
    && previous.status === next.status
    && previous.description === next.description
    && previous.activeForm === next.activeForm
    && (previous.blocks?.length ?? 0) === (next.blocks?.length ?? 0)
    && (previous.blocks ?? []).every((id, index) => next.blocks?.[index] === id)
    && (previous.blockedBy?.length ?? 0) === (next.blockedBy?.length ?? 0)
    && (previous.blockedBy ?? []).every((id, index) => next.blockedBy?.[index] === id);
}

function agentStatusEqual(previous: AgentStatus, next: AgentStatus): boolean {
  if (previous === next) return true;
  if (previous.todo === next.todo) return true;
  if (!previous.todo || !next.todo || previous.todo.length !== next.todo.length) return false;
  return previous.todo.every((item, index) => (
    next.todo?.[index] !== undefined && todoItemsEqual(item, next.todo[index])
  ));
}

function pendingQuestionsEqual(
  previous: PendingQuestion | null,
  next: PendingQuestion | null
): boolean {
  if (previous === next) return true;
  if (!previous || !next) return false;
  return previous.context.id === next.context.id
    && previous.context.input === next.context.input
    && previous.context.result.success === next.context.result.success
    && previous.context.result.output === next.context.result.output;
}

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
const TASK_CONTAINER_WIDTH_STORAGE_KEY = "mework.task-container-width";
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

function loadTaskContainerWidth(): number {
  try {
    const storedWidth = Number(window.localStorage.getItem(TASK_CONTAINER_WIDTH_STORAGE_KEY));
    return Number.isFinite(storedWidth) && storedWidth > 0
      ? clampTaskContainerWidth(storedWidth)
      : TASK_CONTAINER_DEFAULT_WIDTH;
  } catch {
    return TASK_CONTAINER_DEFAULT_WIDTH;
  }
}

type AppShellStyle = CSSProperties & {
  "--sidebar-width": string;
  "--task-container-width": string;
};

type EditorState =
  | { mode: "insert"; kind: InsertableContextKind; index: number }
  | { mode: "edit"; kind: InsertableContextKind; item: ContextItem; index: number };

type QuestionEditorState = {
  conversationId: string;
  item: ToolContext;
  answer?: UserContext;
};

/** Undo control for timeline deletion, scoped to the current conversation. */
type PendingUndoState = {
  id: string;
  conversationId: string;
  label: string;
  run: () => void;
};

export {
  gitReviewSnapshotCacheKey,
  gitSnapshotBroadcastIds,
  gitSnapshotForWorkspace,
  gitSnapshotRefreshResultFromSummary,
  gitSnapshotsAfterDraftRedemption,
  gitSnapshotsAfterRefresh,
  gitSnapshotsAfterWorkspaceMutation
} from "./lib/gitController";
export type {
  GitSnapshotEntry,
  GitSnapshotRefreshResult,
  GitSnapshots
} from "./lib/gitController";

type ResourceKind = "hooks" | "skills" | "mcp";

export function mergeResourceOrder(previous: ResourceDescriptor[], discovered: ResourceDescriptor[]): ResourceDescriptor[] {
  const discoveredById = new Map(discovered.map((resource) => [resource.id, resource]));
  const previousIds = new Set(previous.map((resource) => resource.id));
  return [
    ...previous.flatMap((resource) => {
      const next = discoveredById.get(resource.id);
      return next ? [next] : [];
    }),
    ...discovered.filter((resource) => !previousIds.has(resource.id))
  ];
}

export function reorderDocumentResources(
  current: AppDocument,
  kind: ResourceKind,
  sourceId: string,
  targetId: string,
  position: "before" | "after"
): AppDocument {
  const currentResources = kind === "hooks"
    ? current.capabilities.hooks
    : kind === "skills" ? current.capabilities.skills : current.capabilities.mcps;
  const resources = reorderItems(currentResources, sourceId, targetId, position, (resource) => resource.id);
  if (resources === currentResources) return current;
  const capabilities = kind === "hooks"
    ? { ...current.capabilities, hooks: resources }
    : kind === "skills"
      ? { ...current.capabilities, skills: resources }
      : { ...current.capabilities, mcps: resources };
  return { ...current, capabilities };
}

const reasoningEffortOptions: ReasoningEffort[] = ["disabled", "low", "medium", "high", "xhigh"];
const securityLevelOptions: SecurityLevel[] = ["request_approval", "allow_edits", "full_access"];

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

/** Tab id the native side gives a conversation's own page. Extra tabs use their session suffix. */
const AGENT_PRIMARY_BROWSER_TAB = "main";

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
 * Terminal id of the drawer under the composer. A conversation has exactly one, and the id is
 * stable so a collapsed drawer reattaches to the same PTY instead of spawning a second one.
 */
const COMPOSER_TERMINAL_ID = "composer";

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
  /** Replace local conversation read models with authoritative host-written bodies by id. */
  const applyAuthoritativeConversation = useCallback(
    (workspaceId: string, conversation: Conversation) => {
      documentStore.update((current) => current ? {
        ...current,
        workspaces: current.workspaces.map((workspace) => (
          workspace.id === workspaceId
            ? {
              ...workspace,
              conversations: workspace.conversations.map((candidate) => (
                candidate.id === conversation.id ? conversation : candidate
              ))
            }
            : workspace
        ))
      } : current);
    },
    [documentStore]
  );
  /** The host exclusively writes conversation bodies; the renderer sends intents through this command channel. */
  const [conversationSync] = useState(() => createConversationSync(applyAuthoritativeConversation));
  const document = useSyncExternalStore(documentStore.subscribe, documentStore.getSnapshot);
  /** Use factory preferences before the document loads so consumers always receive valid settings. */
  const appearance = document?.globalSettings.appearance ?? defaultAppearancePreferences();
  // StrictMode unmounts the shared store once during development; resume must pair with
  // dispose so that cleanup cannot permanently disable subsequent edits and saves.
  useEffect(() => {
    documentStore.resume();
    return () => documentStore.dispose();
  }, [documentStore]);
  /** Authoritative terminal sessions; App orchestrates guards, pages, and prompts only. */
  const [terminalController] = useState(createTerminalController);
  const terminalSessions = useSyncExternalStore(terminalController.subscribe, terminalController.current);
  /** Authoritative Git snapshots and mutation leases, including last-token-wins polling. */
  const [gitController] = useState(createGitController);
  const gitState = useSyncExternalStore(gitController.subscribe, gitController.current);
  const gitSnapshots = gitState.snapshots;
  const gitMutationConversationIds = gitState.mutationConversationIds;
  /** Authoritative composer drafts, image queue, queued-message save fence, and guides. */
  const [composerController] = useState(createComposerController);
  const composerState = useSyncExternalStore(composerController.subscribe, composerController.current);
  const composerDrafts = composerState.drafts;
  const composerImageDrafts = composerState.imageDrafts;
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
  /** Data for the empty-message usage card; read failures retain the previous value. */
  const [usageStatistics, setUsageStatistics] = useState<UsageStatistics>(EMPTY_USAGE_STATISTICS);
  const [usageStatisticsLoading, setUsageStatisticsLoading] = useState(true);
  const [sidebarOpen, setSidebarOpen] = useState(true);
  const [sidebarWidth, setSidebarWidth] = useState(loadSidebarWidth);
  const [sidebarResizing, setSidebarResizing] = useState(false);
  const [taskContainerResizing, setTaskContainerResizing] = useState(false);
  const [viewportWidth, setViewportWidth] = useState(() => window.innerWidth);
  const [conversationSettingsOpen, setConversationSettingsOpen] = useState(false);
  const [saveAsPresetDialog, setSaveAsPresetDialog] = useState<{ name: string; description: string } | null>(null);
  /**
   * What the message area is showing, per conversation, plus the live preview sessions each one
   * owns. One state, not two: this replaces both the right sidebar's tab container and the
   * separate "selected subagent" flag, so "what is on screen" has a single answer instead of two
   * that had to be kept from contradicting each other.
   */
  const [mainPaneState, setMainPaneState] = useState(initialMainPaneState);
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
  /** Whether the task container is expanded, per conversation-independent preference. */
  const [taskContainerOpen, setTaskContainerOpen] = useState(false);
  const [taskContainerWidth, setTaskContainerWidth] = useState(loadTaskContainerWidth);
  /**
   * Whether the terminal drawer under the composer is expanded, per conversation. Renderer-only:
   * collapsing keeps the PTY alive, so this says nothing about the session — it only decides which
   * conversation shows its drawer, which is why one task's open terminal does not follow the user
   * to the next.
   */
  const [terminalDrawerOpen, setTerminalDrawerOpen] = useState<Record<string, boolean>>({});
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
  const [systemPromptDialogOpen, setSystemPromptDialogOpen] = useState(false);
  const [appSurface, setAppSurface] = useState<AppSurface>({ kind: "workspace" });
  const globalSettingsView = appSurface.kind === "settings" ? appSurface.view : null;
  const [editor, setEditor] = useState<EditorState | null>(null);
  const [questionEditor, setQuestionEditor] = useState<QuestionEditorState | null>(null);
  const [workspaceDialogOpen, setWorkspaceDialogOpen] = useState(false);
  const [assignWorkspaceAfterAdd, setAssignWorkspaceAfterAdd] = useState(false);
  const [startingTerminals, setStartingTerminals] = useState<Set<string>>(() => new Set());
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
  /** The model's fork requests awaiting the user, oldest first; drawn by the top-right tray. */
  const [forkRequests, setForkRequests] = useState<PendingForkRequest[]>([]);
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
  const [pendingUndo, setPendingUndo] = useState<PendingUndoState | null>(null);
  const contextScrollRef = useRef<Record<string, number>>({});
  const conversationTurnsRef = useRef<ConversationTurns>(conversationTurns);
  const queuedQuestionAnswersRef = useRef(new Map<string, string>());
  const deletingWorkspaceIdsRef = useRef(new Set<string>());
  const deletingConversationIdsRef = useRef(new Set<string>());
  const activeWorkspaceIdRef = useRef<string | null>(activeWorkspaceId);
  const activeConversationIdRef = useRef<string | null>(activeConversationId);
  /** Event handlers read the draft ref because they cannot wait for the next render. */
  const draftConversationRef = useRef<DraftConversationState | null>(draftConversation);
  draftConversationRef.current = draftConversation;
  /** Keep this changing callback in a ref so startup effects do not rerun when the document changes. */
  const openDraftConversationRef = useRef<(
    workspaceId?: string,
    source?: NewConversationSource
  ) => void>(() => {});
  const composerTextareaRef = useRef<HTMLTextAreaElement>(null);
  const mainPaneStateRef = useRef(mainPaneState);
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
  const dispatchMainPane = useCallback((action: MainPaneAction) => {
    const next = mainPaneReducer(mainPaneStateRef.current, action);
    mainPaneStateRef.current = next;
    setMainPaneState(next);
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
  const clearPendingUndo = useCallback((conversationId: string) => {
    setPendingUndo((current) => current?.conversationId === conversationId ? null : current);
  }, []);
  const conversationModelRunIsActive = useCallback((conversationId: string) => (
    Boolean(modelRunController.current()[conversationId])
    || modelRunController.hasRunToken(conversationId)
    || modelRunController.hasPerformingRun(conversationId)
    || modelRunController.hasPreparingRun(conversationId)
  ), []);
  const beginGitMutation = useCallback((conversationId: string): boolean => {
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
    const snapshots = gitController.current().snapshots;
    // A peer running a model in its own checkout is not writing here, which is
    // also how the host decides. The draft has no worktree of its own.
    const acting: GitCheckoutRef = {
      snapshot: gitSnapshotForWorkspace(snapshots[conversationId], workspace.id),
      isolated: Boolean(workspace.conversations.find((conversation) => (
        conversation.id === conversationId
      ))?.worktree)
    };
    if (
      workspace.conversations.some((conversation) => (
        conversation.id !== conversationId
        && gitPeerBlocksMutation({
          acting,
          peer: {
            snapshot: gitSnapshotForWorkspace(snapshots[conversation.id], workspace.id),
            isolated: Boolean(conversation.worktree)
          },
          peerModelRunActive: conversationModelRunIsActive(conversation.id),
          peerGitMutationActive: gitController.mutationIsActive(conversation.id)
        })
      ))
      || Object.values(terminalController.current()).some((session) => (
        session.busy
        && workspace.conversations.some((conversation) => (
          conversation.id === session.conversationId
        ))
      ))
    ) return false;
    gitController.acquireMutationLease(
      conversationId,
      // Include the draft because mutations must invalidate every in-flight poll for its workspace.
      [...workspace.conversations.map((conversation) => conversation.id), conversationId]
    );
    clearPendingUndo(conversationId);
    return true;
  }, [clearPendingUndo, contextMutationIsBlocked, conversationModelRunIsActive, gitController, terminalController]);
  const endGitMutation = useCallback((conversationId: string) => {
    gitController.releaseMutationLease(conversationId);
  }, [gitController]);
  const mainPaneView = mainPaneViewFor(mainPaneState, activeConversationId);
  const mainPanePageOpen = mainPaneShowsPage(mainPaneView);
  const currentPreviewSessions = previewSessionsFor(mainPaneState, activeConversationId);
  const selectedSubagentId = mainPaneView.kind === "subagent" ? mainPaneView.subagentId : null;
  // The native page is only presented while its own page is the one on screen. Any other view —
  // the conversation, another preview, the review page — means the surface has to be released.
  const browserPanelOpen = mainPaneView.kind === "preview";
  // Only a conversation that actually owns a preview needs its native status polled. Without this
  // the poll runs forever for every task that never opened one, and each tick is a native IPC
  // round trip.
  const openBrowserSessionKey = currentPreviewSessions.join("\n");
  // Joined first so the polling effect only restarts when the set of sessions really changes.
  const openBrowserSessionIds = useMemo(
    () => (openBrowserSessionKey ? openBrowserSessionKey.split("\n") : []),
    [openBrowserSessionKey]
  );
  const gitReviewPanelOpen = mainPaneView.kind === "review";
  const activeGitReviewView = mainPaneView.kind === "review" ? mainPaneView.view : null;
  const taskContainerVisible = taskContainerOpen
    && appSurface.kind === "workspace"
    && Boolean(activeConversationId);

  const updateTaskContainerWidth = useCallback((width: number) => {
    const nextWidth = clampTaskContainerWidth(width);
    setTaskContainerWidth(nextWidth);
    try {
      window.localStorage.setItem(TASK_CONTAINER_WIDTH_STORAGE_KEY, String(nextWidth));
    } catch {
      // Resizing should continue to work when persistent browser storage is unavailable.
    }
  }, []);

  /**
   * Collapsing the container also returns the message area to the conversation: the container is
   * how one reaches another task, so keeping a page up without it would strand the user
   * somewhere with no way onward.
   */
  const closeTaskContainer = useCallback(() => {
    setTaskContainerOpen(false);
    const conversationId = activeConversationIdRef.current;
    if (conversationId) dispatchMainPane({ type: "back", conversationId });
  }, [dispatchMainPane]);

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
    const updateViewportWidth = () => setViewportWidth(window.innerWidth);
    window.addEventListener("resize", updateViewportWidth);
    return () => window.removeEventListener("resize", updateViewportWidth);
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

  const updateModelRuns = modelRunController.update;

  const updateConversationTurns = useCallback((updater: (current: ConversationTurns) => ConversationTurns) => {
    // Every writer funnels through here, so this is where a round that produced
    // nothing stops being a record. Sweeping it centrally is what keeps the next
    // run from continuing a round the user never saw and inheriting its elapsed
    // time and token counters.
    const next = dropEmptyConversationTurns(updater(conversationTurnsRef.current));
    conversationTurnsRef.current = next;
    setConversationTurns(next);
  }, []);

  const updateSteeringMessageIds = composerController.updateSteeringMessageIds;
  const updateFailedQueuedPromotionIds = composerController.updateFailedQueuedPromotionIds;
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
    // Default titles have no provenance link, so an untouched conversation is
    // identified by an empty system prompt: the user never opened the prompt
    // box. Only those get their default title localized; an edited prompt means
    // the conversation is the user's and its title is left alone.
    documentStore.update((current) => {
      if (!current) return current;
      let changed = false;
      const workspaces = current.workspaces.map((workspace) => ({
        ...workspace,
        conversations: workspace.conversations.map((conversation) => {
          if (conversation.settings.systemPrompt !== "") return conversation;
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
    decision: ToolPromptDecision
  ) => {
    const waiter = manualToolPromptsRef.current.get(promptId);
    manualToolPromptsRef.current.delete(promptId);
    const answered = resolveToolPrompt(promptId, decision);
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

  const requestFitsImageBudget = useCallback((contexts: ContextItem[]): boolean => {
    const budget = projectedImageBudget(contexts);
    return budget.count <= MAX_COMPOSER_IMAGES
      && budget.bytes <= MAX_COMPOSER_IMAGE_BYTES
      && budget.pixels <= MAX_COMPOSER_IMAGE_PIXELS;
  }, []);

  /** Flush the authoritative snapshot through the store's serial queue; durable writes also await conversation commands. */
  const flushLatestDocument = useCallback(
    async (options: { durable?: boolean } = {}) => {
      await documentStore.flush(options);
      await conversationSync.flush();
    },
    [conversationSync, documentStore]
  );

  const refreshGitSnapshot = useCallback(async (
    conversationId: string,
    workspaceId: string,
    target: GitTarget
  ): Promise<GitWorkspaceSnapshot | null | undefined> => {
    const workspace = documentStore.current()?.workspaces.find((candidate) => candidate.id === workspaceId);
    if (
      !hasBackendRuntime()
      // Drafts can hold mutation leases despite being absent from workspace conversation lists.
      || gitController.mutationIsActive(conversationId)
      || workspace?.conversations.some((conversation) => (
        gitController.mutationIsActive(conversation.id)
      ))
    ) return undefined;
    return gitController.refresh(conversationId, workspaceId, target);
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
    const sessionIds = new Set<string>([conversationId]);
    for (const sessionId of previewSessionsFor(mainPaneStateRef.current, conversationId)) {
      sessionIds.add(sessionId);
    }
    return [...sessionIds];
  }, []);

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
    // somewhere else still retires from the right roster.
    for (const conversationId of Object.keys(mainPaneStateRef.current.previewSessions)) {
      if (!previewSessionBelongsToConversation(sessionId, conversationId)) continue;
      dispatchMainPane({ type: "forget_preview", conversationId, sessionId });
    }
  }, [browserController, dispatchMainPane]);

  const openBuiltInBrowser = useCallback(async (
    conversationId: string,
    sessionId: string,
    intentEpoch: number
  ): Promise<boolean> => {
    if (
      browserSessionDeletionIsActive(conversationId)
      || browserController.closeInFlight(sessionId)
      || !browserIntentIsCurrent(sessionId, intentEpoch, "open")
      || !documentStore.current()?.workspaces.some((workspace) => (
        workspace.conversations.some((conversation) => conversation.id === conversationId)
      ))
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

  const hideBuiltInBrowser = useCallback(async (animate = true) => {
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
          animate,
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
            animate,
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

  const closeBuiltInBrowser = useCallback(async () => {
    const conversationId = activeConversationIdRef.current;
    if (browserPanelOpen && conversationId) dispatchMainPane({ type: "back", conversationId });
    await hideBuiltInBrowser(true);
  }, [browserPanelOpen, dispatchMainPane, hideBuiltInBrowser]);

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
    const domId = mainPanePageDomId(mainPaneViewKey({ kind: "preview", sessionId }));
    for (let attempt = 0; attempt < 4; attempt += 1) {
      await new Promise<void>((resolve) => window.requestAnimationFrame(() => resolve()));
      const rect = window.document
        .getElementById(domId)
        ?.querySelector<HTMLElement>(".task-page__content")
        ?.getBoundingClientRect();
      // A page that has not been laid out yet measures zero, which the host would reject.
      if (!rect || rect.width < 1 || rect.height < 1) continue;
      await setBrowserPanelBounds(sessionId, {
        x: rect.x,
        y: rect.y,
        width: rect.width,
        height: rect.height,
        visible: true,
        occludedTop: BROWSER_PANEL_CHROME_HEIGHT
      }, epoch).catch(() => undefined);
      return;
    }
  }, []);

  /**
   * Presents one preview page. `sessionId` selects an existing session; leaving it out mints the
   * next one for this conversation — its first reuses the conversation id so Agent browser tools
   * keep driving that same native session.
   */
  const openBrowserTab = useCallback(async (sessionId?: string) => {
    setConversationSettingsOpen(false);
    if (!activeConversationId) return;
    const conversationId = activeConversationId;
    const openSessionIds = new Set(previewSessionsFor(mainPaneStateRef.current, conversationId));
    const targetSessionId = sessionId
      ?? (openSessionIds.has(conversationId)
        ? previewTabSessionId(conversationId, createId("tab").replace(/[^0-9A-Za-z_-]/g, ""))
        : conversationId);
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
      await hideBuiltInBrowser(false);
      if (activeConversationIdRef.current !== conversationId) return;
    }
    const intentEpoch = issueBrowserIntent(targetSessionId, "open");
    browserController.setVisibleSession(targetSessionId);
    dispatchMainPane({
      type: "show",
      conversationId,
      view: { kind: "preview", sessionId: targetSessionId }
    });
    setTaskContainerOpen(true);
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
        || browserController.visibleSession() !== targetSessionId
        || !browserIntentIsCurrent(targetSessionId, intentEpoch, "open")
      ) {
        return;
      }
      if (!opened) {
        dispatchMainPane({ type: "forget_preview", conversationId, sessionId: targetSessionId });
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
    dispatchMainPane,
    hideBuiltInBrowser,
    issueBrowserIntent,
    openBuiltInBrowser,
    publishPreviewBounds,
    t
  ]);

  /**
   * Mirrors the Agent's browser sessions into the task bar after a `playwright` call.
   *
   * The primary session is registered unconditionally, because the host mints it on *any*
   * playwright action while only the four tab actions return a roster — and with no manual entry
   * point left, a `navigate` that produced no row would leave a live Chromium page the user could
   * neither see nor close. A session the host does not actually have is self-correcting: status
   * polling reports `hasPage: false` and the row never renders.
   *
   * The Agent still never gets to steal the surface: sessions are registered without becoming the
   * shown page, and only the tab the Agent explicitly closed is removed. Reconciling from the
   * tool's own roster keeps this free of polling and correct after a renderer reload, because the
   * next tab tool republishes the whole list.
   */
  const reconcileAgentBrowserTabs = useCallback((conversationId: string, output: string) => {
    dispatchMainPane({ type: "register_preview", conversationId, sessionId: conversationId });
    let roster: { tabs?: unknown; closed?: unknown };
    try {
      roster = JSON.parse(output) as { tabs?: unknown; closed?: unknown };
    } catch {
      return;
    }
    if (!Array.isArray(roster.tabs)) return;
    for (const entry of roster.tabs) {
      const tab = (entry as { tab?: unknown } | null)?.tab;
      if (typeof tab !== "string" || !tab) continue;
      const sessionId = tab === AGENT_PRIMARY_BROWSER_TAB
        ? conversationId
        : previewTabSessionId(conversationId, tab);
      dispatchMainPane({ type: "register_preview", conversationId, sessionId });
    }
    if (typeof roster.closed === "string" && roster.closed !== AGENT_PRIMARY_BROWSER_TAB) {
      commitBrowserSessionClosed(previewTabSessionId(conversationId, roster.closed));
    }
  }, [commitBrowserSessionClosed, dispatchMainPane]);

  /**
   * Shows one agent's read-only transcript in the main area. The task container
   * stays open behind it, so the next selection is one click away.
   */
  const openSubagentPanel = useCallback((subagentId: string) => {
    if (!activeConversationId) return;
    setConversationSettingsOpen(false);
    setEditor(null);
    setSystemPromptDialogOpen(false);
    if (browserPanelOpen) void hideBuiltInBrowser(false);
    dispatchMainPane({
      type: "show",
      conversationId: activeConversationId,
      view: { kind: "subagent", subagentId }
    });
    setTaskContainerOpen(true);
  }, [activeConversationId, browserPanelOpen, dispatchMainPane, hideBuiltInBrowser]);

  /** Leaves whatever page is up and returns the message area to the conversation. */
  const backToConversation = useCallback(() => {
    const conversationId = activeConversationIdRef.current;
    if (conversationId) dispatchMainPane({ type: "back", conversationId });
    if (browserPanelOpen) void hideBuiltInBrowser(true);
  }, [browserPanelOpen, dispatchMainPane, hideBuiltInBrowser]);

  useEffect(() => {
    if (!browserPanelOpen) return;
    const presented = browserController.visibleSession();
    if (
      activeConversationId
      && presented
      && previewSessionBelongsToConversation(presented, activeConversationId)
    ) return;
    void closeBuiltInBrowser();
  }, [activeConversationId, browserController, browserPanelOpen, closeBuiltInBrowser]);

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
        && browserController.visibleSession() === sessionId
        && !status.open
      ) {
        const conversationId = activeConversationIdRef.current;
        if (conversationId) dispatchMainPane({ type: "back", conversationId });
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
    dispatchMainPane,
    openBrowserSessionIds
  ]);

  const updateTerminalSession = useCallback((next: TerminalSessionState) => {
    if (browserSessionDeletionIsActive(next.conversationId)) return;
    terminalController.update(next);
  }, [browserSessionDeletionIsActive, terminalController]);

  const beginTerminalCommand = useCallback((conversationId: string, terminalId: string): boolean => {
    const session = terminalController.current()[terminalSessionKey(conversationId, terminalId)];
    if (!session) return false;
    const workspace = documentStore.current()?.workspaces.find((candidate) => (
      candidate.conversations.some((conversation) => conversation.id === session.conversationId)
    ));
    if (
      !workspace
      || browserSessionDeletionIsActive(session.conversationId)
      || workspace.conversations.some((conversation) => (
        gitController.mutationIsActive(conversation.id)
      ))
    ) return false;
    return terminalController.markCommandStarted(conversationId, terminalId);
  }, [browserSessionDeletionIsActive, gitController, terminalController]);

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
   * Opens the page behind a task row. Subagents and workflows show a transcript instead and never
   * reach here.
   *
   * A terminal row has no page of its own: the PTY lives in the drawer under the composer, so its
   * row expands that drawer and returns the message area to the conversation it sits below.
   */
  const openTaskItemPage = useCallback(async (item: TaskItem) => {
    const conversationId = activeConversationIdRef.current;
    if (!conversationId) return;
    if (item.kind === "terminal") {
      setConversationSettingsOpen(false);
      backToConversation();
      setTerminalDrawerOpen((current) => ({ ...current, [item.terminal.conversationId]: true }));
      return;
    }
    if (item.kind === "shell") {
      setConversationSettingsOpen(false);
      if (browserPanelOpen) void hideBuiltInBrowser(false);
      dispatchMainPane({
        type: "show",
        conversationId,
        view: { kind: "shell", shellTaskId: item.shell.shellTaskId }
      });
      return;
    }
    if (item.kind === "browser") await openBrowserTab(item.sessionId);
  }, [backToConversation, browserPanelOpen, dispatchMainPane, hideBuiltInBrowser, openBrowserTab]);

  const openSecondarySurface = useCallback((surface: Exclude<AppSurface, { kind: "workspace" }>) => {
    setAppSurface(surface);
    setSidebarOpen(true);
    setConversationSettingsOpen(false);
    setSystemPromptDialogOpen(false);
    backToConversation();
  }, [backToConversation]);

  const openGlobalSettings = useCallback((view: SettingsView) => {
    openSecondarySurface({ kind: "settings", view });
  }, [openSecondarySurface]);

  /**
   * Closes one preview session and its native page.
   *
   * With no tab strip there is no × on a page any more, so this is reached from the preview row's
   * stop control in the task bar. The Playwright guard survives the move: only the primary
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
    dispatchMainPane({ type: "forget_preview", conversationId, sessionId });
  }, [browserController, dispatchMainPane, modelRunController, requestBrowserSessionClose]);


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
          const discovered = await refreshCapabilities();
          if (cancelled) return;
          // Replace capabilities only with a complete catalog; missing sections would destabilize the tree.
          if (!discovered?.skills || !discovered.mcps || !discovered.hooks) return;
          documentStore.update((current) => current
            ? { ...current, capabilities: discovered }
            : current);
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
    // Fork cards are global: the tool call that raised one has already returned, so the
    // card belongs to no run and the tray draws it whichever conversation is open.
    if (event.type === "forkRequested") {
      const { type: _type, ...request } = event;
      openForkRequest(request);
      return;
    }
    if (event.type === "forkResolved") {
      closeForkRequest(event.forkId);
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
      // would still open, onto a page that can no longer read anything, so it
      // goes — and a page already showing it goes with it. The id is remembered
      // so a task list requested before the eviction cannot bring the row back.
      evictedShellTaskIdsRef.current.add(event.shellTaskId);
      setShellTasks((current) => current.filter(
        (task) => task.shellTaskId !== event.shellTaskId
      ));
      const view = mainPaneStateRef.current.viewByConversation[event.conversationId];
      if (view?.kind === "shell" && view.shellTaskId === event.shellTaskId) {
        dispatchMainPane({ type: "back", conversationId: event.conversationId });
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
    if (event.type === "conversationSaveRejected") {
      // Revert only the rejected conversation from host authority. Do not overwrite newer local edits when generations differ.
      const language = getI18nSnapshot().resolvedLanguage;
      console.error(
        translate(
          language,
          `对话 ${event.conversationId} 的保存被宿主拒绝并回退到上一份快照：${event.error}`,
          `The host rejected this save of conversation ${event.conversationId} and reverted it to the last committed snapshot: ${event.error}`
        )
      );
      // Do not let a stale rejected generation overwrite newer local content.
      if (!conversationMatchesPersistenceGeneration(
        documentStore.current(),
        event.conversationId,
        event.rejectedUpdatedAt
      )) return;
      void loadDocument()
        .then((authority) => {
          documentStore.update((current) => {
            if (!conversationMatchesPersistenceGeneration(
              current,
              event.conversationId,
              event.rejectedUpdatedAt
            )) return current;
            return replaceConversationFromAuthority(current!, authority, event.conversationId);
          });
        })
        .catch((error) => {
          console.error(
            translate(
              language,
              "拉取宿主权威快照失败，被拒对话仍留在本地内存中",
              "Failed to fetch the host's authoritative snapshot; the rejected conversation is still in local memory"
            ),
            error
          );
        });
      return;
    }
    if (event.type === "documentWriteRecovered") {
      documentStore.reportBackendSaveResult("success");
    }
  }), [documentStore, openToolPrompt, closeToolPrompt, openForkRequest, closeForkRequest, dispatchMainPane]);

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
  const activeWorkspace = draftActive
    ? document?.workspaces.find((workspace) => workspace.id === draftConversation?.workspaceId) ?? null
    : persisted.workspace;
  /** Read statistics only when the empty timeline card appears, including when a fresh draft is opened. */
  const usageCardVisible = Boolean(activeConversation && activeConversation.contexts.length === 0);
  useEffect(() => {
    if (!usageCardVisible) return;
    let cancelled = false;
    setUsageStatisticsLoading(true);
    void (async () => {
      try {
        const statistics = await fetchUsageStatistics();
        if (!cancelled) setUsageStatistics(statistics);
        // Pre-gateway usage survives only in the renderer's local turn table; backfill based on the ledger's accounting start rather than a resettable flag.
        if (await ensureUsageBackfill(documentStore.current(), statistics)) {
          const refreshed = await fetchUsageStatistics();
          if (!cancelled) setUsageStatistics(refreshed);
        }
      } catch (error) {
        console.error(t("读取使用统计失败", "Failed to read usage statistics"), error);
      } finally {
        if (!cancelled) setUsageStatisticsLoading(false);
      }
    })();
    return () => { cancelled = true; };
  }, [documentStore, draftConversation?.createdAt, t, usageCardVisible]);
  /** Memoize the card because changing this prop identity defeats ContextStream memoization and rerenders the timeline during streams. */
  const usageStatsCard = useMemo(
    () => <UsageStatsCard statistics={usageStatistics} loading={usageStatisticsLoading} />,
    [usageStatistics, usageStatisticsLoading]
  );
  const activeGitSnapshotEntry = activeConversation ? gitSnapshots[activeConversation.id] : undefined;
  const activeGitSnapshotState = gitSnapshotForWorkspace(
    activeGitSnapshotEntry,
    activeWorkspace?.id
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
  const activeGitTarget = useMemo((): GitTarget | null => {
    if (!activeGitConversationId) return null;
    if (!draftActive) return gitConversationTarget(activeGitConversationId);
    if (!activeGitWorkspaceId || activeGitWorkspaceKind !== "directory") return null;
    return gitWorkspaceTarget(activeGitWorkspaceId);
  }, [activeGitConversationId, activeGitWorkspaceId, activeGitWorkspaceKind, draftActive]);
  useEffect(() => {
    const conversationId = activeConversation?.id;
    const workspaceId = activeWorkspace?.id;
    if (!conversationId || !workspaceId || !activeGitTarget || !hasBackendRuntime()) return;
    let cancelled = false;
    let inFlight = false;
    let refreshQueued = false;
    let timer: number | null = null;
    const refresh = async () => {
      if (cancelled || window.document.visibilityState === "hidden") return;
      if (inFlight) {
        refreshQueued = true;
        return;
      }
      inFlight = true;
      try {
        await refreshGitSnapshot(conversationId, workspaceId, activeGitTarget);
      } finally {
        inFlight = false;
        if (refreshQueued && !cancelled) {
          refreshQueued = false;
          void refresh();
        }
      }
    };
    const poll = async () => {
      await refresh();
      if (!cancelled) timer = window.setTimeout(() => void poll(), 4000);
    };
    const refreshWhenVisible = () => {
      if (window.document.visibilityState !== "hidden") void refresh();
    };
    void poll();
    window.addEventListener("focus", refreshWhenVisible);
    window.document.addEventListener("visibilitychange", refreshWhenVisible);
    return () => {
      cancelled = true;
      if (timer !== null) window.clearTimeout(timer);
      window.removeEventListener("focus", refreshWhenVisible);
      window.document.removeEventListener("visibilitychange", refreshWhenVisible);
    };
  }, [activeConversation?.id, activeGitTarget, activeWorkspace?.id, refreshGitSnapshot]);
  // A workspace that stopped being a Git repository has no review page to show. Leaving it up
  // would strand the user on a page whose panel has nothing to render.
  useEffect(() => {
    if (!activeConversation || activeGitSnapshotState !== null) return;
    if (mainPaneViewFor(mainPaneStateRef.current, activeConversation.id).kind !== "review") return;
    dispatchMainPane({ type: "back", conversationId: activeConversation.id });
  }, [activeConversation, activeGitSnapshotState, dispatchMainPane]);
  /**
   * Shows the review page on the requested view.
   *
   * Deliberately unconditional: the status card's rows each ask for a different view, and the
   * sidebar's selector used to return early when the requested page was already the active one —
   * so with review open, clicking another row did nothing at all.
   */
  const openGitReview = useCallback((view: GitReviewView) => {
    if (!activeConversation || !activeGitSnapshot) return;
    setConversationSettingsOpen(false);
    if (browserPanelOpen) void hideBuiltInBrowser(false);
    dispatchMainPane({
      type: "show",
      conversationId: activeConversation.id,
      view: { kind: "review", view }
    });
  }, [activeConversation, activeGitSnapshot, browserPanelOpen, dispatchMainPane, hideBuiltInBrowser]);
  const activeComposerDraft = activeConversation ? composerDrafts[activeConversation.id] ?? "" : "";
  const activeComposerImages = activeConversation ? composerImageDrafts[activeConversation.id] ?? [] : [];
  const activeModelRunning = Boolean(activeConversation && modelRunSummaries[activeConversation.id]);
  const activeComposerHasText = Boolean(activeComposerDraft.trim());
  const activeComposerHasPayload = activeComposerHasText || activeComposerImages.length > 0;
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
  const activeWorkspaceGitMutationRunning = Boolean(
    activeWorkspace && activeWorkspace.conversations.some((conversation) => (
      gitMutationConversationIds.has(conversation.id)
    ))
  );
  // Mirrors `beginGitMutation` through the same predicate, so a button never offers a write the
  // synchronous gate then refuses.
  const activeWorkspacePeerOperationRunning = Boolean(
    activeWorkspace && activeConversation && activeWorkspace.conversations.some((conversation) => (
      conversation.id !== activeConversation.id
      && gitPeerBlocksMutation({
        acting: {
          snapshot: activeGitSnapshotState,
          isolated: Boolean(activeConversation.worktree)
        },
        peer: {
          snapshot: gitSnapshotForWorkspace(gitSnapshots[conversation.id], activeWorkspace.id),
          isolated: Boolean(conversation.worktree)
        },
        peerModelRunActive: Boolean(modelRunSummaries[conversation.id]),
        peerGitMutationActive: gitMutationConversationIds.has(conversation.id)
      })
    ))
  );
  const activeWorkspaceTerminalBusy = Boolean(
    activeWorkspace && Object.values(terminalSessions).some((session) => (
      session.busy
      && activeWorkspace.conversations.some((conversation) => (
        conversation.id === session.conversationId
      ))
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
  const activeComposerImageUnavailableReason = activeComposerImageLoading
    ? t("正在处理图片…", "Preparing images…")
    : !activeModelChoice
      ? t("请先选择一个已启用的模型", "Select an enabled model first")
      : !supportsVision(activeModelChoice.model)
        ? t(
          "当前模型不支持图片输入",
          "The current model does not support image input"
        )
        : undefined;
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
  const securityLevelLabel = (level: SecurityLevel): string => (
    level === "request_approval"
      ? t("请求批准", "Ask for approval")
      : level === "allow_edits"
        ? t("允许编辑", "Allow edits")
        : t("完全访问", "Full access")
  );
  const reasoningEffortLabel = (effort: ReasoningEffort): string => (
    effort === "disabled" ? t("关闭思考", "Disabled")
      : effort === "low" ? t("低", "Low")
        : effort === "medium" ? t("中", "Medium")
          : effort === "high" ? t("高", "High")
            : t("极高", "Extra high")
  );
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
  const activeSecurityLevelLabel = securityLevelLabel(
    activeConversation?.settings.securityLevel ?? securityLevelOptions[0]
  );
  const activeReasoningEffort = activeConversation?.settings.reasoningEffort ?? reasoningEffortOptions[0];
  const activeReasoningEffortLabel = reasoningEffortLabel(activeReasoningEffort);
  /** Isolated worktree for this conversation; null runs at the workspace root. */
  const activeWorktree = activeConversation?.worktree ?? null;
  /** A draft checkbox records an intent; its worktree cannot exist until the draft materializes. */
  const activeWorktreeChecked = draftActive
    ? Boolean(draftConversation?.worktreeRequested)
    : Boolean(activeWorktree);
  /** A worktree displays its own branch because that is where the Agent writes, not the workspace-root HEAD. */
  const activeBranchLabel = activeWorktree?.branch ?? activeGitSnapshot?.branch ?? null;
  const branchChipDisabled = Boolean(
    !activeGitSnapshot
    || activeModelRunning
    || activeWorkspaceLifecycleOperationRunning
    || (activeConversation && gitMutationConversationIds.has(activeConversation.id))
  );
  // Tool-description overrides are assembled by the backend from a trusted snapshot at runtime.
  const activeConversationTools = useMemo(() => {
    if (!document) return [];
    return document.tools.map((tool) => localizeToolDescriptor(tool, resolvedLanguage));
  }, [document, resolvedLanguage]);
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
  const agentStatus = useStoreSelector(
    modelRunController.subscribe,
    modelRunController.current,
    (runs) => deriveAgentStatus(projectRenderedContexts(runs)),
    agentStatusEqual
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
  }, [activeConversation?.id, openSubagentPanel, subagents, toolPrompts]);
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
   * The panel may not be mounted yet when the container was closed, so the
   * scroll waits for the frame that mounts it.
   */
  const focusWorkflowRunPanel = useCallback((runId: string) => {
    setTaskContainerOpen(true);
    window.requestAnimationFrame(() => {
      window.document
        .querySelector(`[data-workflow-run="${CSS.escape(runId)}"]`)
        ?.scrollIntoView({ block: "nearest" });
    });
  }, []);
  const pendingQuestion = useStoreSelector(
    modelRunController.subscribe,
    modelRunController.current,
    (runs) => findPendingQuestion(projectRenderedContexts(runs)),
    pendingQuestionsEqual
  );
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
  const activeModelRunBusy = Boolean(activeConversation && (
    modelRunSummaries[activeConversation.id]
    || modelRunController.hasRunToken(activeConversation.id)
    || modelRunController.hasPreparingRun(activeConversation.id)
  ));
  const activePlaywrightTool = activeConversation
    ? modelRunSummaries[activeConversation.id]?.browserAutomationTool ?? null
    : null;
  const activeBrowserAutomationStopping = Boolean(
    activeConversation && browserAutomationStoppingIds.has(activeConversation.id)
  );
  /**
   * Banner every page carries while the conversation is waiting on the user.
   *
   * The approval and question docks live inside the conversation pane, which is hidden behind a
   * page. The right sidebar never had this problem — it sat beside the conversation — so without
   * this a model blocked on approval would simply look stuck from a preview.
   */
  const pageAttention = useMemo(() => {
    if (activeToolPrompt) {
      return {
        label: t("模型在等待工具批准", "The model is waiting for tool approval"),
        onGoBack: backToConversation
      };
    }
    if (pendingQuestion) {
      return {
        label: t("模型在等待你的回答", "The model is waiting for your answer"),
        onGoBack: backToConversation
      };
    }
    return null;
  }, [activeToolPrompt, backToConversation, pendingQuestion, t]);
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
  const activeModelStopping = Boolean(
    activeConversation && modelStoppingIds.has(activeConversation.id)
  );
  const activeTaskTerminals = useMemo(() => {
    if (!activeConversation) return [];
    return Object.values(terminalSessions).filter((session) => (
      session.conversationId === activeConversation.id
      && session.phase === "running"
      && (session.busy || session.hasHistory)
    ));
  }, [activeConversation, terminalSessions]);
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

  const activeShellTasks = useMemo(() => {
    if (!activeConversation) return [];
    return shellTasks.filter((task) => task.conversationId === activeConversation.id);
  }, [activeConversation, shellTasks]);
  /** The command whose read-only output page is showing, if that is what the message area holds. */
  const activeShellPage = useMemo(() => {
    if (mainPaneView.kind !== "shell") return null;
    return activeShellTasks.find((task) => task.shellTaskId === mainPaneView.shellTaskId) ?? null;
  }, [activeShellTasks, mainPaneView]);
  /**
   * Task row the message area is currently showing, so the task bar marks it as current.
   *
   * An agent row's id is its agent id, which is why one field covers transcripts and pages alike.
   * The review page has no task row — it is reached from the Git status card.
   */
  const selectedTaskRowId = useMemo(() => {
    if (mainPaneView.kind === "subagent") return mainPaneView.subagentId;
    if (mainPaneView.kind === "preview") return `preview:${mainPaneView.sessionId}`;
    if (mainPaneView.kind === "shell") return mainPaneView.shellTaskId;
    return null;
  }, [mainPaneView]);
  /** Everything the conversation currently has running, as one set of inputs. */
  const taskSources = useMemo<TaskSources>(() => ({
    conversationId: activeConversation?.id,
    agents: subagents,
    terminals: activeTaskTerminals,
    shellTasks: activeShellTasks,
    browserSessions: activeBrowserSessions,
    browserSessionId: activeConversation?.id ?? null,
    browserAutomationTool: activePlaywrightTool,
    browserAutomationStopping: activeBrowserAutomationStopping,
    modelRequestId: activeConversation ? modelRunController.current()[activeConversation.id]?.requestId ?? null : null,
    userAbortedTasks: activeConversation?.userAbortedTasks ?? [],
    inheritedModelId: activeModelChoice?.model.id ?? null
  }), [
    activeBrowserAutomationStopping,
    activeBrowserSessions,
    activeConversation,
    activeModelChoice,
    activePlaywrightTool,
    activeShellTasks,
    activeTaskTerminals,
    subagents
  ]);
  const taskUnreadInput = useMemo(() => ({
    conversationId: activeConversationId,
    signature: taskActivitySignature(taskSources),
    hasTasks: hasAnyTask(taskSources),
    panelOpen: taskContainerVisible
  }), [activeConversationId, taskContainerVisible, taskSources]);
  const [taskUnread, setTaskUnread] = useState<TaskUnreadState>({
    seen: "",
    conversationId: null
  });
  // The latch only ever moves forward, and returns the same object when nothing
  // moved, so re-running this on every task update settles immediately.
  useEffect(() => {
    setTaskUnread((previous) => nextTaskUnreadState(previous, taskUnreadInput));
  }, [taskUnreadInput]);
  const taskUnreadDot = taskUnreadVisible(taskUnread, taskUnreadInput);

  /** The agent whose read-only transcript replaces the conversation, if any. */
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
  const gitMutationDisabledReason = activeModelRunBusy
    ? t(
      "模型或子代理正在使用工作区，结束后才能执行 Git 写操作",
      "A model or subagent is using the workspace. Wait for it to finish before changing Git state."
    )
      : activeWorkspacePeerOperationRunning
          ? t(
            "同一工作区中的另一项任务正在运行，暂不能执行 Git 写操作",
            "Another task in this workspace is running, so Git changes are temporarily unavailable."
          )
          : activeWorkspaceTerminalBusy
            ? t(
              "内置终端正在执行命令，结束后才能执行 Git 写操作",
              "The built-in terminal is running a command. Wait before changing Git state."
            )
        : activeWorkspaceDeletionRunning
          ? t(
            "工作区正在删除，无法执行 Git 写操作",
            "Git changes are unavailable while the workspace is being deleted."
          )
          : null;
  // The mirror of `gitMutationDisabledReason`: a Git write is rewriting the very checkout the
  // shell is sitting in, so the terminal stops taking input until it lands. The terminal→Git
  // direction is the branch above; both have to hold or the two can still interleave.
  const terminalInputDisabledReason = activeWorkspaceGitMutationRunning
    ? t(
      "Git 写操作进行中，完成后才能在终端输入",
      "A Git write is running. Wait for it to finish before typing in the terminal."
    )
    : null;
  const activeTerminalDrawerOpen = Boolean(
    activeConversation && terminalDrawerOpen[activeConversation.id]
  );
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
        ? t("工作区正在删除，无法切换分支", "Cannot switch branches while the workspace is being deleted")
        : null;
  // Branching only opens a new conversation with this message as a draft, so
  // it needs neither a configured model nor an idle turn — only a workspace
  // that is not going away underneath it.
  const branchFromDisabledReason = activeWorkspaceDeletionRunning
    ? t("工作区正在删除，无法创建分支", "Cannot create a branch while the workspace is being deleted")
    : null;
  /** Title of one preview page, for its task row and its page header. */
  const previewLabel = useCallback((sessionId: string) => {
    const browserStatus = browserStatuses[sessionId];
    if (browserStatus?.title?.trim()) return browserStatus.title.trim();
    if (!browserStatus?.url || browserStatus.url === "about:blank") return t("新页面", "New page");
    try {
      return new URL(browserStatus.url).hostname || t("预览", "Preview");
    } catch {
      return t("预览", "Preview");
    }
  }, [browserStatuses, t]);

  const syncBrowserPanelBounds = useCallback((bounds: { x: number; y: number; width: number; height: number }) => {
    const sessionId = browserController.visibleSession();
    if (!isTauriRuntime() || !browserPanelOpen || !sessionId) return;
    const intent = browserController.currentIntent(sessionId);
    if (intent?.desired !== "open") return;
    void setBrowserPanelBounds(sessionId, {
      ...bounds,
      visible: true,
      occludedTop: BROWSER_PANEL_CHROME_HEIGHT
    }, intent.epoch).then((status) => {
      if (
        !browserIntentIsCurrent(sessionId, intent.epoch, "open")
        || browserController.visibleSession() !== sessionId
      ) return;
      browserController.updateStatuses((current) => ({ ...current, [sessionId]: status }));
    }).catch(() => undefined);
  }, [browserController, browserIntentIsCurrent, browserPanelOpen]);

  // The main area only shows an agent that still exists. A selection made while
  // the call was streaming follows the agent to its persisted record, whose view
  // id switches from the call id to the stable name id. A selection that lands
  // on a workflow run is dropped outright: a run is a script and has no
  // transcript, so there is nothing for the main area to show.
  useEffect(() => {
    const conversationId = activeConversationId;
    if (!conversationId || !selectedSubagentId) return;
    const matched = findOpenableSubagentView(subagents, selectedSubagentId);
    if (!matched) {
      dispatchMainPane({ type: "back", conversationId });
    } else if (matched.id !== selectedSubagentId) {
      dispatchMainPane({
        type: "show",
        conversationId,
        view: { kind: "subagent", subagentId: matched.id }
      });
    }
  }, [activeConversationId, dispatchMainPane, selectedSubagentId, subagents]);

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
            const updated = updater(conversation);
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
        .filter((turn) => turn.status === "running" || turn.status === "awaiting_user")
        .map((turn) => turn.requestId));
      let reconciled = existing;
      openRequestIds.forEach((openRequestId) => {
        reconciled = materializeRunTurnContexts(reconciled, previousContexts, openRequestId);
      });
      reconciled = reconciled.map((turn) => (
        turn.status === "running" || turn.status === "awaiting_user"
          ? {
            ...turn,
            status: "interrupted" as const,
            endedAt: startedAt,
            durationMs: (turn.durationMs ?? 0) + (
              turn.status === "running" ? activeTurnSegmentDuration(turn, startedAt) : 0
            )
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
        segmentCount: 1,
        expanded: true,
        userToggled: false
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
          startedAt: resumedAt,
          expanded: turn.userToggled ? turn.expanded : true
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
    contexts: ContextItem[],
    mode: "continue" | "awaiting"
  ) => {
    const resumedAt = new Date().toISOString();
    let resumed = false;
    updateConversationTurns((current) => {
      const existing = current[conversationId] ?? [];
      const target = findResumableTurn(existing, contexts, mode);
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

  const pauseConversationTurnForUser = useCallback((
    conversationId: string,
    requestId: string,
    contexts: ContextItem[],
    options: { modelId?: string; usage?: ModelUsage; durationMs?: number } = {}
  ) => {
    const pausedAt = new Date().toISOString();
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
      const segmentDuration = options.durationMs !== undefined && requestTurns.length === 1
        ? options.durationMs
        : activeTurnSegmentDuration(runningTurn, pausedAt);
      const next = materialized.map((turn) => turn.id === runningTurn.id ? {
        ...turn,
        ...(options.modelId ? { modelId: options.modelId } : {}),
        status: "awaiting_user" as const,
        durationMs: (turn.durationMs ?? 0) + segmentDuration,
        usage: sumModelUsage([turn.usageOffset, currentSegmentUsage])
      } : turn);
      return { ...current, [conversationId]: next };
    });
  }, [updateConversationTurns]);

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
        segmentCount: 1,
        expanded: true,
        userToggled: false
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
          usage: terminalUsage,
          expanded: status === "completed" && !turn.userToggled ? false : turn.expanded
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

  const toggleConversationTurn = useCallback((conversationId: string, turnId: string) => {
    updateConversationTurns((current) => {
      const existing = current[conversationId] ?? [];
      let changed = false;
      const next = existing.map((turn) => {
        if (turn.id !== turnId) return turn;
        changed = true;
        return { ...turn, expanded: !turn.expanded, userToggled: true };
      });
      return changed ? { ...current, [conversationId]: next } : current;
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
      // Apply the same updater to draft projections so settings surfaces need not special-case drafts.
      const draft = draftConversationRef.current;
      if (draft && isDraftConversationId(activeConversationIdRef.current)) {
        setDraftConversation((current) => (current
          ? { ...current, settings: updater(draftAsConversation(current, "")).settings }
          : current));
        return;
      }
      if (!activeWorkspaceId || !activeConversationId) return;
      updateConversation(activeWorkspaceId, activeConversationId, (conversation) => ({
        ...updater(conversation),
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
    const workspaceId = activeWorkspaceId;
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
  }, [activeConversationId, activeGitTarget, activeWorkspaceId, t]);

  /** Switch branches through the shared GitAction channel and workspace mutation lease; preserve Git's own checkout errors rather than discarding changes. */
  const checkoutComposerBranch = useCallback(async (branch: string) => {
    const conversationId = activeConversationId;
    const workspaceId = activeWorkspaceId;
    const target = activeGitTarget;
    if (!conversationId || !workspaceId || !target) return;
    if (!beginGitMutation(conversationId)) {
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
      await refreshGitSnapshot(conversationId, workspaceId, target);
    }
  }, [
    activeConversationId,
    activeGitTarget,
    activeWorkspaceId,
    beginGitMutation,
    endGitMutation,
    refreshGitSnapshot,
    t
  ]);


  /** Persist real conversation targets directly; drafts retain the intent until materialization because `updateActiveConversation` preserves settings only. */
  const setConversationRunTarget = useCallback((target: RunTargetType | null) => {
    if (isDraftConversationId(activeConversationIdRef.current)) {
      setDraftConversation((current) => (current ? { ...current, runTarget: target } : current));
      return;
    }
    if (!activeWorkspaceId || !activeConversationId) return;
    updateConversation(activeWorkspaceId, activeConversationId, (conversation) => ({
      ...conversation,
      runTarget: target,
      updatedAt: new Date().toISOString()
    }));
  }, [activeWorkspaceId, activeConversationId, updateConversation]);

  const saveRunEnvironmentVars = useCallback((envKey: string, vars: Record<string, string>) => {
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

  const saveSshMachine = useCallback((machine: SshMachineConfigType) => {
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
  }, [documentStore]);

  /** Remove the machine and its variables; orphaned conversation bindings fail at dispatch until the user selects local execution. */
  const deleteSshMachine = useCallback((machineId: string) => {
    documentStore.update((current) => {
      if (!current) return current;
      const envVars = { ...current.globalSettings.executionEnvironments.envVars };
      delete envVars[`ssh:${machineId}`];
      return {
        ...current,
        globalSettings: {
          ...current.globalSettings,
          executionEnvironments: {
            sshMachines: current.globalSettings.executionEnvironments.sshMachines
              .filter((item) => item.id !== machineId),
            envVars
          }
        }
      };
    });
  }, [documentStore]);

  /**
   * Create worktrees from the current workspace HEAD and persist their record for trusted host path resolution.
   * On disable, release before clearing the conversation pointer because the host uses that record to find the tree.
   * Drafts retain only the request until they materialize before their first send.
   */
  const toggleConversationWorktree = useCallback(async (enabled: boolean) => {
    const conversationId = activeConversationId;
    const workspaceId = activeWorkspaceId;
    const target = activeGitTarget;
    if (!conversationId || !workspaceId || !target) return;
    if (draftConversationRef.current && isDraftConversationId(conversationId)) {
      setBranchChipError(null);
      setDraftConversation((current) => (
        current ? { ...current, worktreeRequested: enabled } : current
      ));
      setBranchPicker(null);
      return;
    }
    if (!beginGitMutation(conversationId)) {
      setBranchChipError(t(
        "工作区里还有别的操作在进行，请稍后再切换工作树",
        "Another operation is running in this workspace; try toggling the worktree later"
      ));
      return;
    }
    setBranchChipError(null);
    try {
      if (enabled) {
        const worktree = await createConversationWorktree(conversationId);
        updateActiveConversation((conversation) => ({ ...conversation, worktree }));
      } else {
        const removed = await releaseConversationWorktree(conversationId);
        // Clear the pointer even when uncommitted work keeps the tree; retained files belong to the user, not this conversation.
        updateActiveConversation((conversation) => ({ ...conversation, worktree: null }));
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
      await refreshGitSnapshot(conversationId, workspaceId, target);
    }
  }, [
    activeConversationId,
    activeGitTarget,
    activeWorkspaceId,
    beginGitMutation,
    endGitMutation,
    refreshGitSnapshot,
    t,
    updateActiveConversation
  ]);

  /** Evaluate functional global changes against the store's current value to preserve multiple same-tick updates. */
  const handleGlobalSettingsChange = useCallback((change: GlobalSettingsChange) => {
    documentStore.update((current) => current ? applyGlobalSettingsChange(current, change) : current);
  }, [documentStore]);

  /** Saving a preset creates an independent template; neither it nor the source conversation follows later edits. */
  const saveActiveConversationAsPreset = useCallback((name: string, description: string) => {
    if (!document || !activeConversation) return;
    const preset = {
      id: createId("preset"),
      name,
      description,
      settings: captureConversationPresetSettings(activeConversation.settings)
    };
    handleGlobalSettingsChange((current) => ({
      ...current,
      conversationPresets: [...current.conversationPresets, preset]
    }));
  }, [activeConversation, document, handleGlobalSettingsChange]);

  /** Applying a preset is a one-time copy and creates no follow relationship. */
  const applyPresetToActiveConversation = useCallback((presetId: string) => {
    if (!document || !activeConversation) return;
    const preset = presetId === IMPLICIT_CONVERSATION_PRESET_ID
      ? implicitConversationPreset(document.tools, resolvedLanguage, platform)
      : document.globalSettings.conversationPresets.find((item) => item.id === presetId);
    if (!preset) return;
    const settings = applyConversationPresetSettings(
      activeConversation.settings,
      preset.settings,
      new Set(document.tools.map((tool) => tool.name))
    );
    updateActiveConversation((conversation) => ({ ...conversation, settings }));
  }, [
    activeConversation,
    document,
    platform,
    resolvedLanguage,
    updateActiveConversation
  ]);

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
      || moving.contexts.length
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
      || latestMoving.contexts.length
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
    setSystemPromptDialogOpen(false);
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
    // Selecting a persisted conversation discards the unpersisted draft and its composer content.
    discardDraftConversation();
    setEditor(null);
    setSystemPromptDialogOpen(false);
    backToConversation();
    window.requestAnimationFrame(() => {
      const scroller = window.document.querySelector<HTMLElement>('[data-main-context-stream="true"]');
      if (scroller) scroller.scrollTop = contextScrollRef.current[conversationId] ?? scroller.scrollHeight;
    });
  };

  /**
   * Drafts and persisted conversations must resolve settings identically so materialization preserves draft edits.
   * Workspace creation prefers an explicit workspace preset, then remembered settings, then the global default;
   * global new-task creation always uses the global default.
   */
  const resolveNewConversationSettings = useCallback((
    target: Workspace | null,
    source: NewConversationSource
  ): ConversationSettingsType | null => {
    // Read from the store because the startup effect opens a draft in the same tick that it loads the document.
    const document = documentStore.current();
    if (!document) return null;
    const knownToolNames = new Set(document.tools.map((tool) => tool.name));
    const blankSettings: ConversationSettingsType = {
      systemPrompt: "",
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
      reasoningEffort: document.globalSettings.lastReasoningEffort,
      // Security has no global default; presets or workspace snapshots supply it, and this is the safest base value.
      securityLevel: "request_approval",
      // Both tiers start off; the preset applied below is what actually decides
      // them for a fresh conversation.
      globalMemoryEnabled: false,
      projectMemoryEnabled: false,
      // Presets determine skill-on-demand loading; this conservative base preserves skill prose in the system prompt.
      skillToolEnabled: false
    };
    const workspacePreset = source === "workspace" && target
      ? conversationPresetById(
        document.globalSettings,
        target.defaultConversationPresetId,
        document.tools,
        resolvedLanguage,
        platform
      )
      : null;
    const remembered = source === "workspace" && target && !workspacePreset
      ? target.lastConversationSettings
      : null;
    return workspacePreset
      ? applyConversationPresetSettings(blankSettings, workspacePreset.settings, knownToolNames)
      : remembered
        ? cloneConversationSettings(remembered, knownToolNames)
        : applyConversationPresetSettings(
          blankSettings,
          defaultConversationPreset(
            document.globalSettings,
            document.tools,
            resolvedLanguage,
            platform
          ).settings,
          knownToolNames
        );
  }, [documentStore, platform, resolvedLanguage]);

  const createConversation = useCallback((
    workspaceId?: string,
    source: NewConversationSource = "global",
    settingsOverride?: ConversationSettingsType,
    runTargetOverride?: RunTargetType | null,
    parentConversationId: string | null = null
  ): string | null => {
    if (!document) return null;
    const requestedId = workspaceId ?? activeWorkspaceId;
    const target = document.workspaces.find((workspace) => workspace.id === requestedId) ?? document.workspaces[0];
    if (!target) return null;
    if (deletingWorkspaceIdsRef.current.has(target.id)) return null;
    const knownToolNames = new Set(document.tools.map((tool) => tool.name));
    const targetId = target.id;
    const now = new Date().toISOString();
    const conversationId = createId("conv");
    let created: Conversation | null = null;
    // Materializing a draft preserves its exact settings; other creation paths resolve them now.
    const resolvedSettings = settingsOverride
      ? cloneConversationSettings(settingsOverride, knownToolNames)
      : resolveNewConversationSettings(target, source);
    if (!resolvedSettings) return null;
    documentStore.update((current) => {
      if (!current) return current;
      const conversation: Conversation = {
        id: conversationId,
        title: t("新任务", "New task"),
        createdAt: now,
        updatedAt: now,
        settings: resolvedSettings,
        contexts: [],
        queuedMessages: [],
        branches: [],
        userAbortedTasks: [],
        worktree: null,
        runTarget: runTargetOverride ?? null,
        parentConversationId
      };
      created = conversation;
      return {
        ...current,
        workspaces: current.workspaces.map((workspace) => workspace.id === targetId ? {
          ...workspace,
          // The newly created conversation becomes the workspace's remembered settings snapshot.
          lastConversationSettings: resolvedSettings,
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
    // Clear a materialized draft so no stale draft can later regain the active view.
    setDraftConversation(null);
    setConversationSettingsOpen(false);
    // A brand-new conversation starts on its own timeline; the per-conversation view state means
    // this only has to clear a stale entry left by a conversation id that was reused.
    dispatchMainPane({ type: "back", conversationId });
    setSystemPromptDialogOpen(false);
    return conversationId;
  }, [activeWorkspaceId, dispatchMainPane, document, resolveNewConversationSettings, t]);

  const discardDraftConversation = useCallback(() => {
    if (!draftConversationRef.current) return;
    setDraftConversation(null);
    composerController.invalidateImages([DRAFT_CONVERSATION_ID]);
    composerController.updateDrafts((current) => {
      if (current[DRAFT_CONVERSATION_ID] === undefined) return current;
      const next = { ...current };
      delete next[DRAFT_CONVERSATION_ID];
      return next;
    });
  }, [composerController]);

  /** Open an unpersisted empty draft, inheriting the selected workspace when available; sending materializes it in the temporary workspace otherwise. */
  const openDraftConversation = useCallback((
    workspaceId?: string,
    source: NewConversationSource = "global"
  ) => {
    const current = documentStore.current();
    if (!current) return;
    const requestedId = workspaceId ?? activeWorkspaceIdRef.current;
    const target = current.workspaces.find((workspace) => (
      workspace.id === requestedId && !deletingWorkspaceIdsRef.current.has(workspace.id)
    )) ?? null;
    const settings = resolveNewConversationSettings(target, source);
    if (!settings) return;
    // Starting a new task discards any existing unsent draft composer content.
    discardDraftConversation();
    // Preserve a previously selected workspace, including a temporary workspace; otherwise defer selection until send.
    const draftWorkspaceId = target?.id ?? null;
    setDraftConversation({
      workspaceId: draftWorkspaceId,
      settings,
      createdAt: new Date().toISOString(),
      worktreeRequested: false,
      runTarget: null
    });
    setActiveWorkspaceId(draftWorkspaceId);
    setActiveConversationId(DRAFT_CONVERSATION_ID);
    setEditor(null);
    setConversationSettingsOpen(false);
    setSystemPromptDialogOpen(false);
    backToConversation();
    // Close the container entirely because a draft has neither tasks nor pages; hiding contents alone leaves an empty-width shell.
    closeTaskContainer();
  }, [
    backToConversation,
    closeTaskContainer,
    discardDraftConversation,
    documentStore,
    resolveNewConversationSettings
  ]);
  openDraftConversationRef.current = openDraftConversation;

  /** Materialize the draft at send time and migrate composer drafts and Git snapshots by conversation id. If its checkout changes to a worktree, do not migrate the workspace-root snapshot. */
  const materializeDraft = useCallback((): {
    conversationId: string;
    workspaceId: string;
    worktreeRequested: boolean;
  } | null => {
    const draft = draftConversationRef.current;
    if (!draft) return null;
    const workspaceId = draft.workspaceId ?? TEMPORARY_WORKSPACE_ID;
    const created = createConversation(workspaceId, "global", draft.settings, draft.runTarget);
    if (!created) return null;
    composerController.updateDrafts((current) => {
      const pending = current[DRAFT_CONVERSATION_ID];
      if (pending === undefined) return current;
      const next = { ...current, [created]: pending };
      delete next[DRAFT_CONVERSATION_ID];
      return next;
    });
    composerController.updateImageDrafts((current) => {
      const pending = current[DRAFT_CONVERSATION_ID];
      if (!pending?.length) return current;
      const next = { ...current, [created]: pending };
      delete next[DRAFT_CONVERSATION_ID];
      return next;
    });
    const worktreeRequested = draft.worktreeRequested
      && documentStore.current()?.workspaces.find(
        (workspace) => workspace.id === workspaceId
      )?.kind === "directory";
    // Migrate snapshots only when the checkout is unchanged; worktree creation changes branch, HEAD, and modifications.
    if (!worktreeRequested) {
      updateGitSnapshots((current) => (
        gitSnapshotsAfterDraftRedemption(current, DRAFT_CONVERSATION_ID, created)
      ));
    }
    return {
      conversationId: created,
      workspaceId,
      // Only directory workspaces can create worktrees; discard an unrealizable request before it can block sending.
      worktreeRequested
    };
  }, [composerController, createConversation, updateGitSnapshots]);

  /** Moving a draft changes only its workspace id; persisted conversations must relocate stored ownership, terminals, and ordering. */
  const setDraftWorkspace = useCallback((workspaceId: string | null) => {
    if (!draftConversationRef.current) return;
    setDraftConversation((current) => (current ? { ...current, workspaceId } : current));
    setActiveWorkspaceId(workspaceId);
  }, [setActiveWorkspaceId]);

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

  /** Workspace-menu presets use the implicit project default when no custom preset exists, matching the conversation settings menu. */
  const workspacePresetOptions = useMemo(() => {
    if (!document) return [];
    const presets = document.globalSettings.conversationPresets.length
      ? document.globalSettings.conversationPresets
      : [implicitConversationPreset(document.tools, resolvedLanguage, platform)];
    return presets.map((preset) => ({ id: preset.id, name: preset.name }));
  }, [document, platform, resolvedLanguage]);

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
      "app.settings.open": () => openGlobalSettings("conversation_presets"),
      "app.conversation_settings.open": () => {
        backToConversation();
        if (browserPanelOpen) void closeBuiltInBrowser();
        setAppSurface({ kind: "workspace" });
        setConversationSettingsOpen(true);
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
       * Toggles between the conversation and its preview page.
       *
       * With no tab strip there is no creation entry point here: the model opens the preview, and
       * the shortcut only switches between an existing preview and the conversation.
       */
      "panel.browser.toggle": () => {
        if (browserPanelOpen) {
          backToConversation();
          return;
        }
        const sessionId = currentPreviewSessions.at(0);
        if (sessionId) void openBrowserTab(sessionId);
      },
      "panel.close": () => backToConversation()
    };
  });

  const shortcutBindings = document?.globalSettings.shortcuts;
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      // IME composition keystrokes are not shortcuts.
      if (event.isComposing || event.key === "Process") return;
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

  const addWorkspace = async (
    name: string,
    path: string,
    assignConversation = assignWorkspaceAfterAdd
  ) => {
    const current = documentStore.current();
    if (!current) return;
    if (!isAbsoluteWorkspacePath(path)) return;
    const normalized = normalizedWorkspacePath(path);
    const sourceWorkspaceId = activeWorkspaceIdRef.current;
    const movingConversationId = activeConversationIdRef.current;
    const sourceWorkspace = current.workspaces.find((workspace) => workspace.id === sourceWorkspaceId);
    const movingConversation = sourceWorkspace?.conversations.find((conversation) => (
      conversation.id === movingConversationId
    ));
    const existing = current.workspaces.find((workspace) => (
      workspace.kind === "directory"
      && normalizedWorkspacePath(workspace.path) === normalized
    ));
    // Drafts have no stored ownership; assign their workspace id directly when it exists, or after creation.
    const assigningDraft = assignConversation && Boolean(draftConversationRef.current);
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
        || t("新工作区", "New workspace"),
      kind: "directory",
      path: path.trim(),
      createdAt: new Date().toISOString(),
      // A new workspace has no history, so leave its default preset empty until first creation records a snapshot.
      defaultConversationPresetId: "",
      lastConversationSettings: null,
      conversations: []
    };
    const next = { ...current, workspaces: [...current.workspaces, workspace] };
    documentStore.update(() => next);
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
      && movingConversation.contexts.length === 0
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


  const chooseWorkspaceDirectory = async () => {
    if (!hasNativeWorkspacePicker()) {
      setAssignWorkspaceAfterAdd(true);
      setWorkspaceDialogOpen(true);
      return;
    }
    try {
      const path = await pickWorkspaceDirectory();
      if (path) await addWorkspace("", path, true);
    } catch {
      // Cancellation or failure of directory selection is non-fatal and can be retried.
    }
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
      dispatchMainPane({ type: "remove_conversation", conversationId: conversation.id });
      composerController.updateDrafts((current) => {
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
        openDraftConversationRef.current(latestWorkspace.id);
        setEditor(null);
        setConversationSettingsOpen(false);
        setSystemPromptDialogOpen(false);
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
      const [browserResults, terminalResults] = await Promise.all([
        Promise.allSettled(conversationIdsBeforeClose.map((conversationId) => (
          requestConversationBrowserClose(conversationId)
        ))),
        Promise.allSettled(terminalSessions.map((session) => (
          requestTerminalSessionClose(session.conversationId, session.terminalId)
        )))
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
      setStartingTerminals((current) => new Set(
        [...current].filter((conversationId) => !removedConversationIds.has(conversationId))
      ));
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
        dispatchMainPane({ type: "remove_conversation", conversationId: conversation.id });
      }
      if (activeWorkspaceIdRef.current === workspace.id) {
        // After deletion, open a draft in the first remaining workspace.
        openDraftConversationRef.current(fallback?.id);
        setEditor(null);
        setConversationSettingsOpen(false);
        setSystemPromptDialogOpen(false);
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

  const handleContextInsert = (index: number, kind: InsertableContextKind) => {
    if (!activeConversation || contextMutationIsBlocked(activeConversation.id)) return;
    setQuestionEditor(null);
    setEditor({ mode: "insert", kind, index });
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

  const deleteQuestionContext = (item: ToolContext, answer?: UserContext) => {
    if (!activeConversation || !activeWorkspaceId || contextMutationIsBlocked(activeConversation.id)) return;
    const deletedItems = answer ? [item, answer] : [item];
    if (deletedItems.some((context) => isConversationBranchFork(activeConversation, context.id))) return;

    const conversationId = activeConversation.id;
    const deletedIds = new Set(deletedItems.map((context) => context.id));
    const indexedItems = activeConversation.contexts.flatMap((context, index) => (
      deletedIds.has(context.id) ? [{ context, index }] : []
    ));
    if (indexedItems.length !== deletedItems.length) return;
    const nextContexts = activeConversation.contexts.filter((context) => !deletedIds.has(context.id));
    queuedQuestionAnswersRef.current.delete(conversationId);
    setQuestionEditor(null);
    updateActiveConversation((conversation) => ({ ...conversation, contexts: nextContexts }));
    setContextUsage((current) => ({ ...current, [conversationId]: estimateActiveContextUsage(nextContexts) }));
    setPendingUndo({
      id: createId("undo"),
      conversationId,
      label: answer
        ? t("撤销删除整条提问消息", "Undo deleting the complete question message")
        : t("撤销删除提问消息", "Undo deleting the question message"),
      run: () => {
        if (contextMutationIsBlocked(conversationId)) return;
        updateConversation(activeWorkspaceId, conversationId, (conversation) => {
          const contexts = [...conversation.contexts];
          indexedItems
            .slice()
            .sort((left, right) => left.index - right.index)
            .forEach(({ context, index }) => {
              if (contexts.some((candidate) => candidate.id === context.id)) return;
              contexts.splice(Math.min(index, contexts.length), 0, context);
            });
          return { ...conversation, contexts, updatedAt: new Date().toISOString() };
        });
        setContextUsage((current) => {
          const next = { ...current };
          delete next[conversationId];
          return next;
        });
        setPendingUndo(null);
      }
    });
  };

  const deleteContext = (item: ContextItem) => {
    if (!activeConversation || !activeWorkspaceId || contextMutationIsBlocked(activeConversation.id)) return;
    if (isConversationBranchFork(activeConversation, item.id)) return;
    // Confirm before computing deletion results; asking after that would perform the deletion first.
    if (
      appearance.confirmMessageDelete
      && !window.confirm(t("确定删除这条上下文？", "Delete this context item?"))
    ) return;
    const deletedAt = new Date().toISOString();
    const stateDeletion = deleteStateToolContext(activeConversation, item, deletedAt);
    const index = activeConversation.contexts.findIndex((context) => context.id === item.id);
    if (!stateDeletion && index < 0) return;
    const conversationId = activeConversation.id;
    const nextConversation = stateDeletion?.conversation ?? {
      ...activeConversation,
      contexts: activeConversation.contexts.filter((context) => context.id !== item.id)
    };
    const nextContexts = nextConversation.contexts;
    updateActiveConversation(() => nextConversation);
    setContextUsage((current) => ({ ...current, [conversationId]: estimateActiveContextUsage(nextContexts) }));
    setPendingUndo({
      id: createId("undo"),
      conversationId,
      label: stateDeletion
        ? t("撤销删除任务状态消息", "Undo deleting the task-state messages")
        : item.kind === "tool"
          ? t("撤销删除工具调用", "Undo deleting the tool call")
          : t("撤销删除上下文", "Undo deleting the context"),
      run: () => {
        if (contextMutationIsBlocked(conversationId)) return;
        if (stateDeletion) {
          const currentConversation = documentStore.current()?.workspaces
            .find((workspace) => workspace.id === activeWorkspaceId)?.conversations
            .find((conversation) => conversation.id === conversationId);
          const restored = currentConversation
            ? restoreStateToolContexts(
              currentConversation,
              stateDeletion.removed,
              new Date().toISOString()
            )
            : null;
          // Do not restore the group if task status changed; retain the undo entry instead.
          if (!restored) return;
          updateConversation(activeWorkspaceId, conversationId, () => restored);
          setContextUsage((current) => {
            const next = { ...current };
            delete next[conversationId];
            return next;
          });
          setPendingUndo(null);
          return;
        }
        updateConversation(activeWorkspaceId, conversationId, (conversation) => {
          if (conversation.contexts.some((context) => context.id === item.id)) return conversation;
          const contexts = [...conversation.contexts];
          contexts.splice(Math.min(index, contexts.length), 0, item);
          return { ...conversation, contexts, updatedAt: new Date().toISOString() };
        });
        setContextUsage((current) => ({
          ...current,
          [conversationId]: estimateActiveContextUsage(activeConversation.contexts)
        }));
        setPendingUndo(null);
      }
    });
  };

  const saveTextContext = (content: string, images?: ImageAttachment[]) => {
    if (!editor || !activeConversation || contextMutationIsBlocked(activeConversation.id)) return;
    const conversationId = activeConversation?.id;
    if (editor.mode === "edit") {
      const id = editor.item.id;
      updateActiveConversation((conversation) => {
        const contexts = conversation.contexts.map((item) => {
          if (item.id !== id || item.kind === "tool") return item;
          if (item.kind === "assistant" || item.kind === "reasoning") {
            return { ...item, content, interrupted: false };
          }
          if (item.kind === "user" && images) {
            return { ...item, content, images };
          }
          return { ...item, content };
        });
        return { ...conversation, contexts };
      });
    } else {
      const base = { id: createId("ctx"), createdAt: new Date().toISOString(), content };
      const item: ContextItem = editor.kind === "reasoning"
        // Manually inserted reasoning is explicitly plaintext because the user supplied all of its text.
        ? { ...base, kind: "reasoning", form: "plaintext" }
        : { ...base, kind: editor.kind as "system" | "user" | "assistant" };
      updateActiveConversation((conversation) => {
        const contexts = [...conversation.contexts];
        contexts.splice(editor.index, 0, item);
        return { ...conversation, contexts };
      });
    }
    if (conversationId) {
      setContextUsage((current) => {
        const next = { ...current };
        delete next[conversationId];
        return next;
      });
    }
    setEditor(null);
  };

  const saveToolContextImages = (images: ImageAttachment[]) => {
    if (
      !editor
      || editor.mode !== "edit"
      || editor.item.kind !== "tool"
      || !activeConversation
      || contextMutationIsBlocked(activeConversation.id)
    ) {
      return;
    }
    const conversationId = activeConversation.id;
    const contextId = editor.item.id;
    updateActiveConversation((conversation) => ({
      ...conversation,
      contexts: conversation.contexts.map((item) => item.id === contextId && item.kind === "tool"
        ? { ...item, result: { ...item.result, images } }
        : item)
    }));
    setContextUsage((current) => {
      const next = { ...current };
      delete next[conversationId];
      return next;
    });
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
    updateActiveConversation((conversation) => ({
      ...conversation,
      contexts: conversation.contexts.map((context) => {
        if (context.id === questionId && context.kind === "tool") {
          return { ...context, requestedInput: undefined, input };
        }
        if (answerId && context.id === answerId && context.kind === "user") {
          return { ...context, content: answerContent ?? context.content };
        }
        return context;
      })
    }));
    setContextUsage((current) => {
      const next = { ...current };
      delete next[activeConversation.id];
      return next;
    });
    setQuestionEditor(null);
  };

  const saveToolContext = async (toolName: string, input: JsonObject): Promise<ToolResult> => {
    if (!document || !editor || !activeWorkspace || !activeConversation) {
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
      updateActiveConversation((conversation) => ({
        ...conversation,
        contexts: conversation.contexts.map((item) => (
          item.id === id && item.kind === "tool"
            ? { ...item, requestedInput: undefined, input }
            : item
        ))
      }));
      setContextUsage((current) => {
        const next = { ...current };
        delete next[activeConversation.id];
        return next;
      });
      setEditor(null);
      return existingResult;
    }
    await flushLatestDocument();
    const request = { conversationId: activeConversation.id, workspacePath: activeWorkspace.path, toolName, input };
    // Rust classifies every operation from the persisted security policy. Safe calls return
    // without a card; calls that need consent come back with a prompt to draw, and answering
    // it is what mints the single-use nonce — from the arguments Rust classified, not from
    // anything the renderer could substitute in between.
    const approval = await requestToolApproval(request);
    const granted = approval?.prompt
      ? await awaitManualToolApproval(activeConversation.id, approval.prompt)
      : approval;
    const execution = await executeTool(request, granted?.nonce);
    if (editor.mode === "edit") {
      const id = editor.item.id;
      updateActiveConversation((conversation) => {
        const contexts = conversation.contexts
          .map((item) => item.id === id && item.kind === "tool"
            ? { ...item, requestedInput: undefined, input, result: execution }
            : item);
        return { ...conversation, contexts };
      });
    } else {
      const item: ContextItem = {
        id: createId("ctx"),
        kind: "tool",
        toolName,
        input,
        result: execution,
        createdAt: new Date().toISOString()
      };
      updateActiveConversation((conversation) => {
        const contexts = [...conversation.contexts];
        contexts.splice(editor.index, 0, item);
        return { ...conversation, contexts };
      });
    }
    setContextUsage((current) => {
      const next = { ...current };
      delete next[activeConversation.id];
      return next;
    });
    setEditor(null);
    return execution;
  };

  /** Refresh App facilities after each commit; the send pipeline reads them through `host()` at call time. */
  const sendPipelineHostRef = useRef<SendPipelineHost | null>(null);
  useEffect(() => {
    sendPipelineHostRef.current = {
      t,
      openProviderSettings: () => openGlobalSettings("providers"),
      activeWorkspaceId: () => activeWorkspaceIdRef.current,
      activeConversationId: () => activeConversationIdRef.current,
      activeConversationTools: () => activeConversationTools,
      activeEnabledTools: () => activeEnabledTools,
      requestFitsImageBudget,
      contextMutationIsBlocked,
      clearPendingUndo,
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
      pauseConversationTurnForUser,
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
      reconcileAgentBrowserTabs: (conversationId, toolName, output) => {
        // Only the four tab actions return a roster, and `reconcileAgentBrowserTabs`
        // already ignores any output without a `tabs` array — so the tool name is the
        // whole gate. The completion effect carries no arguments, so the action itself
        // is not visible here.
        if (toolName === "playwright") reconcileAgentBrowserTabs(conversationId, output);
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
    removeComposerImage,
    dispatchNextQueuedMessage,
    retryFailedQueuedPromotion
  } = sendPipeline;

  /** Materialize a draft synchronously before `sendComposer`, which requires both active ids in the document. Do not materialize while images are uploading, because send rejection must not leave an empty conversation. */
  const sendActiveComposer = useCallback(async (overrideText?: string) => {
    if (isDraftConversationId(activeConversationIdRef.current)) {
      if (composerController.current().imageLoadingIds.has(DRAFT_CONVERSATION_ID)) return;
      const redeemed = materializeDraft();
      if (!redeemed) return;
      if (redeemed.worktreeRequested) {
        // Create the requested worktree before the first message so every tool call uses its isolated checkout; never silently fall back to the workspace root.
        try {
          // Persist the materialized conversation before requesting a worktree because the host resolves conversations from its saved document.
          await flushLatestDocument({ durable: true });
          const worktree = await createConversationWorktree(redeemed.conversationId);
          updateConversation(
            redeemed.workspaceId,
            redeemed.conversationId,
            (conversation) => ({ ...conversation, worktree })
          );
          // The host also reads the persisted worktree record for trusted path resolution; without it, this turn would run at the workspace root.
          await flushLatestDocument({ durable: true });
          // Refresh the Git snapshot after moving from the workspace root to a different worktree checkout.
          void refreshGitSnapshot(
            redeemed.conversationId,
            redeemed.workspaceId,
            gitConversationTarget(redeemed.conversationId)
          );
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
    }
    await sendComposer(overrideText);
  }, [
    composerController,
    flushLatestDocument,
    materializeDraft,
    refreshGitSnapshot,
    sendComposer,
    t,
    updateConversation
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
        documentStore.update((current) => current ? {
          ...current,
          workspaces: current.workspaces.map((workspace) => (
            workspace.id === workspaceId
              ? {
                ...workspace,
                conversations: [
                  ...workspace.conversations.filter((candidate) => candidate.id !== authoritative.id),
                  authoritative
                ]
              }
              : workspace
          ))
        } : current);
        setContextUsage((currentUsage) => ({
          ...currentUsage,
          [authoritative.id]: estimateActiveContextUsage(authoritative.contexts)
        }));
        if (!await startForkedConversationRun(workspaceId, conversationId)) {
          throw new Error(t("分叉首轮尚未启动，请检查模型配置后重试", "The fork run has not started. Check model settings and retry."));
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
          // A pending question needs a human answer. Do not dispatch queued messages because an unrelated user context would close the question and discard its actual answer.
          && !findPendingQuestion(conversation.contexts)
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
    // independent conversation.
    const created = createConversation(workspaceId, "global", undefined, undefined, conversationId);
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

    setPendingUndo(null);
    modelRunController.addPreparingRun(conversationId);
    try {
      await persistDocumentImmediately(nextDocument);
      queuedQuestionAnswersRef.current.delete(conversationId);
      setEditor(null);
      backToConversation();
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

  useEffect(() => {
    if (!activeConversation) return;
    const conversationId = activeConversation.id;
    const queuedAnswer = queuedQuestionAnswersRef.current.get(conversationId);
    if (queuedAnswer === undefined) return;
    if (modelRunController.hasRunToken(conversationId) || modelRunController.hasPreparingRun(conversationId)) return;
    if (!findPendingQuestion(activeConversation.contexts)) {
      queuedQuestionAnswersRef.current.delete(conversationId);
      return;
    }
    queuedQuestionAnswersRef.current.delete(conversationId);
    // sendComposer is stable; it reads current host facilities from host() at invocation.
    void sendComposer(queuedAnswer);
  }, [activeConversation, modelRunSummaries]);

  const answerPendingQuestion = async (answer: string): Promise<boolean> => {
    if (!activeConversation) return false;
    const conversationId = activeConversation.id;
    const workspaceId = activeWorkspaceId;
    if (modelRunController.hasRunToken(conversationId) || modelRunController.hasPreparingRun(conversationId)) {
      queuedQuestionAnswersRef.current.set(conversationId, answer);
      return true;
    }
    await sendComposer(answer);
    // Unlock the question only after the answer reaches the conversation store; host rejection leaves it pending.
    if (!workspaceId) return false;
    const located = findConversation(documentStore.current(), workspaceId, conversationId);
    return Boolean(located.conversation) && !findPendingQuestion(located.conversation!.contexts);
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
    if (!requestFitsImageBudget(requestContexts)) return;
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
        systemPrompt: activeConversation.settings.systemPrompt,
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
      queuedQuestionAnswersRef.current.delete(conversationId);
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
          // Browser cleanup failed after Playwright stopped; the next toggle reconciles the surface.
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
      if (item.kind === "terminal") {
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

  if (loadError) return <ErrorView message={loadError} onReset={async () => { const next = await resetDocument(); documentStore.load(next); setLoadError(null); }} />;
  if (!document) {
    return (
      <div className="app-loading">
        <MeworkIcon size={38} /><p>{t("正在打开 Mework…", "Opening Mework…")}</p>
      </div>
    );
  }
  return (
    <CommonErrorBoundary>
      <div
        className={`app-shell ${sidebarOpen ? "" : "app-shell--sidebar-closed"} ${sidebarResizing || taskContainerResizing ? "app-shell--resizing" : ""} ${appSurface.kind === "workspace" && conversationSettingsOpen && activeConversation ? "app-shell--settings-open" : ""} ${taskContainerVisible ? "app-shell--tasks-open" : ""}`}
        style={{
          "--sidebar-width": `${sidebarWidth}px`,
          "--task-container-width": `${taskContainerWidth}px`
        } as AppShellStyle}
      >
        <Sidebar
            workspaces={document.workspaces}
            activeWorkspaceId={activeWorkspaceId}
            activeConversationId={activeConversationId}
            onSelectConversation={selectConversation}
            onNewConversation={openDraftConversation}
            onAddWorkspace={() => { setAssignWorkspaceAfterAdd(false); setWorkspaceDialogOpen(true); }}
            onRenameConversation={(workspaceId, conversationId, title) => {
              updateConversation(workspaceId, conversationId, (conversation) => ({
                ...conversation,
                title,
                updatedAt: new Date().toISOString()
              }));
            }}
            onDeleteConversation={deleteConversation}
            onDeleteWorkspace={deleteWorkspace}
            isConversationRunning={conversationIsBusy}
            hasLiveActivity={conversationHasLiveActivity}
            conversationPresets={workspacePresetOptions}
            onSetWorkspaceDefaultPreset={setWorkspaceDefaultPreset}
            isWorkspaceDeleting={(workspaceId) => deletingWorkspaceIds.has(workspaceId)}
            onOpenSettings={() => openGlobalSettings("conversation_presets")}
            surface={appSurface}
            onSelectSettingsView={openGlobalSettings}
            onReturn={() => setAppSurface({ kind: "workspace" })}
            onClose={() => setSidebarOpen(false)}
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

        <main className="main-pane">
          {globalSettingsView ? (
            <>
              <header className="topbar global-settings-topbar">
                <div className="topbar__leading">
                  {!sidebarOpen && (
                    <IconButton label={t("打开侧栏", "Open sidebar")} onClick={() => setSidebarOpen(true)}>
                      <PanelLeft size={18} />
                    </IconButton>
                  )}
                  <div className="conversation-title">
                    <div><MeworkIcon size={11} /><span>Mework</span></div>
                    <h1>{t("全局设置", "Global settings")}</h1>
                  </div>
                </div>
                <div className="topbar__actions">
                  {saveStatusChip}
                </div>
              </header>
              <GlobalSettings
                initialView={globalSettingsView}
                settings={document.globalSettings}
                tools={document.tools}
                capabilities={document.capabilities}
                workspaces={document.workspaces}
                onChange={handleGlobalSettingsChange}
                onFlush={flushLatestDocument}
                navigationPlacement="external"
                onViewChange={openGlobalSettings}
              />
            </>
          ) : (
            <>
          <header className="topbar">
            <div className="topbar__leading">
              {!sidebarOpen && (
                <IconButton label={t("打开侧栏", "Open sidebar")} onClick={() => setSidebarOpen(true)}>
                  <PanelLeft size={18} />
                </IconButton>
              )}
              {activeConversation ? (
                <div className="conversation-title">
                  <div>
                    <span>
                      {activeWorkspace?.name ?? t("未选择工作区", "No workspace selected")}
                    </span>
                    <ChevronDown size={12} />
                  </div>
                  <h1>{activeConversation.title}</h1>
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
                <button type="button" className={`topbar-button ${conversationSettingsOpen ? "topbar-button--active" : ""}`} onClick={() => {
                   const opening = !conversationSettingsOpen;
                   if (opening) backToConversation();
                   setConversationSettingsOpen(opening);
                }}>
                  <SlidersHorizontal size={16} /> {t("本对话设置", "Conversation settings")}
                </button>
              )}
              {/* Drafts have no tasks, and the task container requires a persisted conversation. */}
              {activeConversation && !draftActive && (
                <IconButton
                  label={taskContainerVisible
                    ? t("收起任务容器", "Collapse tasks")
                    : t("打开任务容器", "Open tasks")}
                  className={`topbar-sidebar-toggle${taskContainerVisible ? " topbar-sidebar-toggle--active" : ""}`}
                  aria-expanded={taskContainerVisible}
                  aria-controls={taskContainerId}
                  onClick={() => (taskContainerVisible ? closeTaskContainer() : setTaskContainerOpen(true))}
                >
                  <ListChecks size={18} />
                  {/* Unread marker: the tasks moved since the panel was last on
                      screen. Purely decorative — the button's own label already
                      says what it does. */}
                  {taskUnreadDot && <span className="topbar-sidebar-toggle__dot" aria-hidden="true" />}
                </IconButton>
              )}
            </div>
          </header>

          {/* A workspace is optional for entering a draft; sending uses the temporary workspace. Pages that need a real workspace remain individually guarded. */}
          {activeConversation ? (
            <>
            {selectedSubagentView && (
              <StreamedSubagentPanel
                conversation={activeConversation}
                modelRunController={modelRunController}
                externalStepBodies={externalStepBodies}
                selectedSubagentId={selectedSubagentId}
                tools={activeConversationTools}
                onSelectAgent={(agentId) => openSubagentPanel(agentId)}
                onClose={backToConversation}
                dock={(
                  /* Render approval cards on subagent pages so their source-navigation target remains visible and actionable. */
                  <ToolApprovalDock
                    pending={activeToolPrompt}
                    stack={activeToolPromptStack}
                    onDecide={(decision) => {
                      if (activeToolPrompt) {
                        decideToolPrompt(activeConversation.id, activeToolPrompt.promptId, decision);
                      }
                    }}
                  />
                )}
              />
            )}
            <div className="conversation-pane" hidden={mainPanePageOpen}>
              <StreamedConversationView
                className="conversation-pane__main"
                conversation={activeConversation}
                conversationTurns={conversationTurns[activeConversation.id]}
                modelRunController={modelRunController}
                onToggleTurn={(turnId) => toggleConversationTurn(activeConversation.id, turnId)}
                onRetryTurnError={() => void retryActiveModelRun()}
                onDismissTurnError={() => clearModelRunError(activeConversation.id)}
                retryableTurnRequestId={
                  modelRunErrors[activeConversation.id]?.retryable
                    ? modelRunErrors[activeConversation.id]?.requestId ?? null
                    : null
                }
                tools={activeConversationTools}
                enabledTools={activeEnabledTools}
                pendingQuestionId={pendingQuestion?.context.id ?? null}
                emptyState={usageStatsCard}
                timelineMutationLocked={activeTimelineMutationBlocked}
                onEdit={handleContextEdit}
                onDelete={deleteContext}
                onEditQuestion={handleQuestionEdit}
                onDeleteQuestion={deleteQuestionContext}
                onBranchFrom={(item) => void branchFromUserContext(item)}
                branchFromDisabledReason={branchFromDisabledReason}
                branchNavigations={activeBranchNavigations}
                onSelectBranch={(forkContextId, branchId) => void selectConversationBranch(forkContextId, branchId)}
                branchSwitchDisabledReason={branchSwitchDisabledReason}
                onInsert={handleContextInsert}
                onOpenSubagent={(subagentId) => openSubagentPanel(subagentId)}
                agents={subagents}
                taskMessages={taskMessages}
                onOpenWorkflowRun={focusWorkflowRunPanel}
                beforeTimeline={(
                  <>
                  <div className="conversation-overview">
                    <div className="conversation-overview__prompt-slot">
                      {/* Show the system prompt directly at remaining width; an empty
                          one says so, because empty now means no base prompt at all. */}
                      <button
                        type="button"
                        className="primary-system-prompt"
                        aria-label={t("主系统提示词", "Primary system prompt")}
                        title={activeConversation.settings.systemPrompt.trim()
                          ? activeConversation.settings.systemPrompt
                          : t("未设置基础系统提示词；点击填写", "No base system prompt is set; click to write one")}
                        onClick={() => {
                          backToConversation();
                          setConversationSettingsOpen(true);
                        }}
                      >
                        <span>{activeConversation.settings.systemPrompt.trim()
                          ? activeConversation.settings.systemPrompt
                          : t("未设置系统提示词", "No system prompt")}</span>
                      </button>
                      {activeGitSnapshot && (
                        <GitStatusCard
                          git={activeGitSnapshot}
                          gitOpen={gitReviewPanelOpen}
                          gitView={activeGitReviewView}
                          onOpenGitReview={openGitReview}
                        />
                      )}
                    </div>
                  </div>
                  <WorkflowHistory
                    key={activeConversation.id}
                    conversationId={activeConversation.id}
                    contexts={activeConversation.contexts}
                    representedCallIds={subagents.filter((agent) => agent.workflowRun).flatMap((agent) => agent.callIds)}
                    liveCallIds={subagents.filter((agent) => agent.workflowRun && agent.status === "running").flatMap((agent) => agent.callIds)}
                    liveRunIds={subagents.filter((agent) => agent.workflowRun && agent.status === "running").flatMap((agent) => workflowRunIdsByRun[agent.id] ? [workflowRunIdsByRun[agent.id]] : [])}
                    tools={activeConversationTools}
                  />
                  </>
                )}
                composer={(
                  <>
                    <ToolApprovalDock
                      /* The subagent page owns this card while open to prevent duplicate dialogs with the same id from competing for focus. */
                      pending={selectedSubagentView ? null : activeToolPrompt}
                      stack={activeToolPromptStack}
                      onDecide={(decision) => {
                        if (activeToolPrompt) {
                          decideToolPrompt(activeConversation.id, activeToolPrompt.promptId, decision);
                        }
                      }}
                    />
                    <QuestionDock
                      pending={pendingQuestion}
                      disabled={activeWorkspaceLifecycleOperationRunning}
                      editDisabled={activeTimelineMutationBlocked}
                      onAnswer={answerPendingQuestion}
                      onEditQuestion={(item) => handleQuestionEdit(item)}
                      onDismissQuestion={(item) => deleteQuestionContext(item)}
                    />
                    <div className="composer-wrap">
                {pendingUndo?.conversationId === activeConversation.id && (
                  <div className="composer-undo">
                    <button
                      type="button"
                      className="composer-undo__action"
                      disabled={contextMutationIsBlocked(pendingUndo.conversationId)}
                      onClick={pendingUndo.run}
                    >
                      <Undo2 size={13} />{pendingUndo.label}
                    </button>
                    <IconButton
                      label={t("放弃撤销", "Discard the undo")}
                      onClick={() => setPendingUndo(null)}
                    >
                      <X size={13} />
                    </IconButton>
                  </div>
                )}
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
                {/* Run location, workspace, and branch describe where the conversation runs, so they sit above the composer rather than inside it. */}
                <div className="composer-context">
                    <RunLocationPicker
                      runTarget={draftActive
                        ? draftConversation?.runTarget ?? null
                        : activeConversation.runTarget}
                      sshMachines={document.globalSettings.executionEnvironments.sshMachines}
                      envVars={document.globalSettings.executionEnvironments.envVars}
                      selectionDisabled={Boolean(modelRunSummaries[activeConversation.id])
                        || activeWorkspaceLifecycleOperationRunning}
                      onSelect={setConversationRunTarget}
                      onSaveEnvVars={saveRunEnvironmentVars}
                      onSaveMachine={saveSshMachine}
                      onDeleteMachine={(machineId) => {
                        // Clear the active SSH binding before deleting its machine to avoid an orphaned run target.
                        const active = draftActive
                          ? draftConversation?.runTarget
                          : activeConversation.runTarget;
                        if (active?.kind === "ssh" && active.machineId === machineId) {
                          setConversationRunTarget(null);
                        }
                        deleteSshMachine(machineId);
                      }}
                    />
                    <WorkspaceSelector
                      workspaces={document.workspaces}
                      activeWorkspace={activeWorkspace}
                      onSelect={(workspaceId) => {
                        // Drafts change one field; persisted conversations must relocate.
                        if (draftActive) setDraftWorkspace(workspaceId);
                        else void moveActiveConversation(workspaceId);
                      }}
                      onChooseDirectory={() => void chooseWorkspaceDirectory()}
                      movementDisabled={deletingConversationIds.has(activeConversation.id)
                        || activeConversation.contexts.length > 0}
                      movementDisabledReason={t(
                        "对话已经有内容，不能再更换工作区",
                        "This conversation already has content, so it can no longer change workspace"
                      )}
                      isWorkspaceDeleting={(workspaceId) => deletingWorkspaceIds.has(workspaceId)}
                    />
                    {activeGitSnapshot && (
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
                          disabled={branchChipDisabled}
                          menuLabel={t("切换分支", "Switch branch")}
                          menuWidth={260}
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
                              icon: <GitBranch size={14} />,
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
                        <span className="composer-chip-group__divider" aria-hidden="true" />
                        <label
                          className="composer-worktree"
                          title={draftActive
                            ? t(
                              "在一份独立检出上运行本任务；工作树在你发出第一条消息时建立",
                              "Run this task on its own checkout. The worktree is created when you send the first message."
                            )
                            : t(
                              "在一份独立检出上运行本对话，与工作区里别的对话互不干扰",
                              "Run this conversation on its own checkout, isolated from other conversations in this workspace"
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
                      </div>
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
                  className="composer"
                  onDragOver={(event) => {
                    if (event.dataTransfer.types.includes("Files")) event.preventDefault();
                  }}
                  onDrop={(event) => {
                    const files = Array.from(event.dataTransfer.files);
                    if (!files.length) return;
                    event.preventDefault();
                    void addComposerImages(activeConversation.id, files);
                  }}
                >
                  {draftActive && <ComposerCat />}
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
                  <ImageStrip
                    images={activeComposerImages}
                    compact
                    className="composer__images"
                    onRemove={(imageId) => removeComposerImage(activeConversation.id, imageId)}
                  />
                  <div className="composer__input">
                    <textarea
                      ref={composerTextareaRef}
                      rows={1}
                      value={activeComposerDraft}
                      placeholder={pendingQuestion
                        ? t("回答 Agent 的提问…", "Answer the Agent's question…")
                        : t("向 Agent 发送消息…", "Message the Agent…")}
                      aria-label={pendingQuestion
                        ? t("回答 Agent 的提问", "Answer the Agent's question")
                        : t("向 Agent 发送消息", "Message the Agent")}
                      disabled={activeWorkspaceLifecycleOperationRunning}
                      spellCheck={appearance.spellCheck}
                      onPaste={(event) => {
                        const files = Array.from(event.clipboardData.files);
                        if (!files.length) return;
                        if (!event.clipboardData.getData("text/plain")) event.preventDefault();
                        void addComposerImages(activeConversation.id, files);
                      }}
                      onChange={(event) => {
                        const draft = event.target.value;
                        const conversationId = activeConversation.id;
                        composerController.updateDrafts((current) => ({ ...current, [conversationId]: draft }));
                      }}
                      onKeyDown={(event) => {
                        // Send and newline shortcuts are configurable but mutually exclusive, so checking send first cannot trigger both.
                        if (event.nativeEvent.isComposing) return;
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
                          ? t("加入排队消息", "Queue message")
                          : activeModelRunning
                            ? activeModelStopping
                              ? t("正在停止生成", "Stopping generation")
                              : t("停止生成", "Stop generating")
                            : activeWorkspaceDeletionRunning
                              ? t("工作区删除中", "Workspace deletion in progress")
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
                      disabled={Boolean(modelRunSummaries[activeConversation.id]) || activeWorkspaceLifecycleOperationRunning}
                      menuLabel={t("安全层级", "Security level")}
                      menuWidth={256}
                      sections={[{
                        id: "security",
                        items: securityLevelOptions.map((option) => ({
                          id: option,
                          label: securityLevelLabel(option),
                          icon: <ShieldCheck size={14} />,
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
                    <ComposerAddImages
                      key={activeConversation.id}
                      disabled={activeWorkspaceLifecycleOperationRunning}
                      imageUnavailableReason={activeComposerImageUnavailableReason}
                      onChooseImages={(files) => void addComposerImages(activeConversation.id, files)}
                    />
                    {/* Drafts exist only in the renderer, so there is no host conversation to
                        hang a PTY on until the first message materializes one. */}
                    {!draftActive && (
                      <button
                        type="button"
                        className="composer-option"
                        aria-expanded={activeTerminalDrawerOpen}
                        aria-controls={terminalPanelId(activeConversation.id, COMPOSER_TERMINAL_ID)}
                        disabled={activeWorkspaceLifecycleOperationRunning}
                        onClick={() => {
                          const conversationId = activeConversation.id;
                          setTerminalDrawerOpen((current) => ({
                            ...current,
                            [conversationId]: !current[conversationId]
                          }));
                        }}
                      >
                        <SquareTerminal size={14} />
                        <span className="composer-option__label">{t("终端", "Terminal")}</span>
                      </button>
                    )}
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
                      searchPlaceholder={t("搜索模型…", "Search models…")}
                      emptyLabel={enabledModelChoices.length
                        ? t("没有匹配的模型", "No matching models")
                        : t("没有已启用的模型", "No enabled models")}
                      sections={[{
                        id: "models",
                        items: enabledModelChoices.map((choice) => ({
                          id: choice.value,
                          label: choice.model.id,
                          description: choice.provider.name,
                          icon: <Sparkles size={14} />,
                          checked: choice.value === activeModelChoice?.value,
                          onSelect: () => {
                            documentStore.update((current) => {
                              if (!current) return current;
                              const provider = current.globalSettings.apiProviders.find((item) => (
                                item.id === choice.provider.id && item.enabled
                              ));
                              const model = provider?.models.find((item) => (
                                item.id === choice.model.id
                              ));
                              if (!provider || !model) return current;
                              return {
                                ...current,
                                globalSettings: {
                                  ...current.globalSettings,
                                  activeProviderId: provider.id,
                                  apiProviders: current.globalSettings.apiProviders.map((item) => item.id === provider.id
                                    ? { ...item, activeModelId: model.id }
                                    : item)
                                }
                              };
                            });
                          }
                        }))
                      }]}
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
                        items: reasoningEffortOptions.map((effort) => ({
                          id: effort,
                          label: reasoningEffortLabel(effort),
                          icon: <Gauge size={14} />,
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
                        systemPrompt={activeConversation.settings.systemPrompt}
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
                        contextSettingsDisabled={Boolean(modelRunSummaries[activeConversation.id])
                          || activeWorkspaceLifecycleOperationRunning}
                        onOpenContextSettings={() => {
                          backToConversation();
                          setConversationSettingsOpen(true);
                        }}
                      />
                    )}
                  </div>
                </div>
                    </div>
                    {/* The interactive PTY docks below the composer: `.terminal-panel-region` is
                        `flex: 0 0 auto` in a column, so expanding it pushes the input up instead
                        of covering the timeline. Collapsing keeps the session alive — the drawer
                        only hides it. */}
                    {!draftActive && (
                      <TerminalPanel
                        conversationId={activeConversation.id}
                        terminalId={COMPOSER_TERMINAL_ID}
                        label={t("终端", "Terminal")}
                        open={activeTerminalDrawerOpen}
                        initialState={terminalSessions[terminalSessionKey(activeConversation.id, COMPOSER_TERMINAL_ID)]}
                        inputDisabledReason={terminalInputDisabledReason}
                        onCommandStart={() => beginTerminalCommand(activeConversation.id, COMPOSER_TERMINAL_ID)}
                        onStateChange={updateTerminalSession}
                        onClose={async () => {
                          // The header's close is "get rid of this terminal": kill the shell,
                          // then fold the drawer away and hand focus back to the composer. A
                          // refused close rejects, and the panel shows the reason in place.
                          const conversationId = activeConversation.id;
                          await requestTerminalSessionClose(conversationId, COMPOSER_TERMINAL_ID);
                          setTerminalDrawerOpen((current) => ({ ...current, [conversationId]: false }));
                          if (activeConversationIdRef.current === conversationId) {
                            composerTextareaRef.current?.focus({ preventScroll: true });
                          }
                        }}
                      />
                    )}
                  </>
                )}
              />
            </div>
            {/* The pages that replaced the right sidebar. They sit beside the conversation pane
                and take the whole message area; the task bar is what navigates between them.

                Preview pages stay mounted per session so a page keeps its address bar and its
                native surface across a trip back to the conversation, and the review panel stays
                mounted so a loaded diff survives the same trip — it idles completely while
                `active` is false. The shell page is the exception: its transcript lives in the
                host's buffer and is replayed on every subscribe, so there is nothing to preserve
                by keeping a hidden one around. */}
            {currentPreviewSessions.map((sessionId) => {
              const active = mainPaneView.kind === "preview" && mainPaneView.sessionId === sessionId;
              const automationHolds = isPrimaryPreviewSession(sessionId, activeConversation.id)
                && (Boolean(activePlaywrightTool) || activeBrowserAutomationStopping);
              return (
                <div className="main-pane__page" key={`preview:${sessionId}`} hidden={!active}>
                  <TaskPage
                    active={active}
                    domId={mainPanePageDomId(mainPaneViewKey({ kind: "preview", sessionId }))}
                    eyebrow={t("预览", "Preview")}
                    title={previewLabel(sessionId)}
                    attention={pageAttention}
                    onBack={backToConversation}
                    onContentBoundsChange={syncBrowserPanelBounds}
                    actions={automationHolds ? (
                      <IconButton
                        label={activeBrowserAutomationStopping
                          ? t("正在停止 Playwright", "Stopping Playwright")
                          : t("停止 Playwright", "Stop Playwright")}
                        disabled={activeBrowserAutomationStopping}
                        onClick={() => void stopBrowserAutomation(activeConversation.id)}
                      >
                        {activeBrowserAutomationStopping
                          ? <LoaderCircle size={12} className="spin" />
                          : <Square size={9} fill="currentColor" />}
                      </IconButton>
                    ) : null}
                  >
                    <BrowserPanel
                      native={isTauriRuntime()}
                      sessionId={sessionId}
                      active={active}
                    />
                  </TaskPage>
                </div>
              );
            })}
            {mainPaneView.kind === "shell" && activeShellPage && (
              <div className="main-pane__page">
                <TaskPage
                  active
                  domId={mainPanePageDomId(mainPaneViewKey(mainPaneView))}
                  eyebrow={t("命令输出（只读）", "Command output (read-only)")}
                  title={activeShellPage.toolName}
                  attention={pageAttention}
                  onBack={backToConversation}
                >
                  <ShellTaskPanel
                    task={activeShellPage}
                    open
                    stopping={stoppingTaskIds.includes(JSON.stringify([activeConversation?.id, activeShellPage.shellTaskId]))}
                    onStop={() => void stopShellTask(
                      activeShellPage.conversationId,
                      activeShellPage.shellTaskId
                    )}
                  />
                </TaskPage>
              </div>
            )}
            {activeWorkspace && activeGitSnapshot && activeGitTarget && (
              <div className="main-pane__page" hidden={!gitReviewPanelOpen}>
                <TaskPage
                  active={gitReviewPanelOpen}
                  domId={mainPanePageDomId(mainPaneViewKey({ kind: "review", view: "changes" }))}
                  eyebrow={t("审阅", "Review")}
                  title={activeWorkspace.name}
                  attention={pageAttention}
                  onBack={backToConversation}
                >
                  <GitReviewPanel
                    key={gitReviewSnapshotCacheKey(activeGitSnapshot)}
                    target={activeGitTarget}
                    snapshot={activeGitSnapshot}
                    initialView={activeGitReviewView ?? "changes"}
                    active={gitReviewPanelOpen}
                    mutationDisabledReason={gitMutationDisabledReason}
                    onViewChange={(view) => dispatchMainPane({
                      type: "show",
                      conversationId: activeConversation.id,
                      view: { kind: "review", view }
                    })}
                    onSnapshotChange={(snapshot) => updateGitSnapshots((current) => (
                      gitSnapshotsAfterWorkspaceMutation(
                        current,
                        // Broadcast Git snapshots only to conversations using the same checkout. Drafts use the workspace root; worktree conversations do not.
                        gitSnapshotBroadcastIds(
                          activeWorkspace.conversations,
                          activeConversation.id,
                          Boolean(activeWorktree)
                        ),
                        activeWorkspace.id,
                        snapshot
                      )
                    ))}
                    onMutationStart={() => beginGitMutation(activeConversation.id)}
                    onMutationEnd={() => endGitMutation(activeConversation.id)}
                  />
                </TaskPage>
              </div>
            )}
            </>
          ) : null}
            </>
          )}
        </main>


        {appSurface.kind === "workspace" && activeConversation && !draftActive && (
          <TaskContainer
            open={taskContainerVisible}
            agents={subagents}
            terminals={activeTaskTerminals}
            shellTasks={activeShellTasks}
            browserSessions={activeBrowserSessions}
            browserSessionId={activeConversation.id}
            conversationId={activeConversation.id}
            browserAutomationTool={activePlaywrightTool}
            browserAutomationStopping={activeBrowserAutomationStopping}
            modelRequestId={taskSources.modelRequestId}
            userAbortedTasks={activeConversation.userAbortedTasks}
            inheritedModelId={taskSources.inheritedModelId}
            status={agentStatus}
            selectedAgentId={selectedSubagentId}
            selectedRowId={selectedTaskRowId}
            stoppingIds={stoppingTaskIds.flatMap((key) => {
              const [conversationId, itemId] = JSON.parse(key) as [string, string];
              return conversationId === activeConversation?.id ? [itemId] : [];
            })}
            workflowProgress={workflowProgressByRun}
            workflowRunIds={workflowRunIdsByRun}
            onWorkflowStepControl={handleWorkflowStepControl}
            width={taskContainerWidth}
            onWidthChange={updateTaskContainerWidth}
            onResizeStateChange={setTaskContainerResizing}
            onSelectAgent={(agentId) => openSubagentPanel(agentId)}
            onOpenItem={(item) => void openTaskItemPage(item)}
            onStopItem={(item) => void stopTaskItem(item)}
            onClose={closeTaskContainer}
          />
        )}

        {appSurface.kind === "workspace" && conversationSettingsOpen && activeConversation && (
          <ConversationSettings
            conversation={activeConversation}
            globalSettings={document.globalSettings}
            tools={activeConversationTools}
            capabilities={document.capabilities}
            onChange={saveActiveConversationComposition}
            onChangeConversationOnly={updateActiveConversationSettingsOnly}
            onApplyPreset={applyPresetToActiveConversation}
            onSaveAsPreset={() => setSaveAsPresetDialog({ name: "", description: "" })}
            onOpenGlobalSettings={openGlobalSettings}
            onClose={() => setConversationSettingsOpen(false)}
          />
        )}

        {saveAsPresetDialog && activeConversation && (
          <Dialog
            title={t("另存为预设", "Save as preset")}
            description={t(
              "把当前对话的系统提示词、启用工具、工具描述与技能/MCP/钩子选择保存为可复用模板。之后修改任一方不会影响另一方。",
              "Save this conversation's system prompt, enabled tools, tool instructions, and skill/MCP/hook selections as a reusable template. Changes to either one won't affect the other."
            )}
            onClose={() => setSaveAsPresetDialog(null)}
            footer={(
              <>
                <button type="button" className="button button--ghost" onClick={() => setSaveAsPresetDialog(null)}>
                  {t("取消", "Cancel")}
                </button>
                <button
                  type="button"
                  className="button button--primary"
                  disabled={!saveAsPresetDialog.name.trim()}
                  onClick={() => {
                    saveActiveConversationAsPreset(
                      saveAsPresetDialog.name.trim(),
                      saveAsPresetDialog.description.trim()
                    );
                    setSaveAsPresetDialog(null);
                  }}
                >
                  {t("保存为预设", "Save as preset")}
                </button>
              </>
            )}
          >
            <label className="field">
              <span className="field__label">{t("预设名称", "Preset name")}</span>
              <input
                className="input"
                autoFocus
                value={saveAsPresetDialog.name}
                onChange={(event) => setSaveAsPresetDialog((current) => current && ({ ...current, name: event.target.value }))}
                placeholder={t("例如：代码评审", "e.g. Code review")}
              />
            </label>
            <label className="field">
              <span className="field__label">{t("说明（可选）", "Description (optional)")}</span>
              <input
                className="input"
                value={saveAsPresetDialog.description}
                onChange={(event) => setSaveAsPresetDialog((current) => current && ({ ...current, description: event.target.value }))}
              />
            </label>
          </Dialog>
        )}

        {editor && activeConversation && (
          <ContextEditor
            mode={editor.mode}
            kind={editor.kind}
            item={editor.mode === "edit" ? editor.item : undefined}
            tools={activeConversationTools}
            enabledTools={activeEnabledTools}
            onClose={() => setEditor(null)}
            onSaveText={saveTextContext}
            onSaveTool={saveToolContext}
            onSaveToolImages={saveToolContextImages}
          />
        )}

        {questionEditor && activeConversation?.id === questionEditor.conversationId && (
          <QuestionEditorDialog
            item={questionEditor.item}
            answer={questionEditor.answer}
            onClose={() => setQuestionEditor(null)}
            onSave={saveQuestionContext}
          />
        )}

        {workspaceDialogOpen && <WorkspaceDialog
          onClose={() => { setWorkspaceDialogOpen(false); setAssignWorkspaceAfterAdd(false); }}
          onSubmit={(name, path) => void addWorkspace(name, path)}
        />}
      </div>
    </CommonErrorBoundary>
  );
}

function WorkspaceDialog({ onClose, onSubmit }: { onClose: () => void; onSubmit: (name: string, path: string) => void }) {
  const { t } = useI18n();
  const [name, setName] = useState("");
  const [path, setPath] = useState("");
  const [picking, setPicking] = useState(false);
  const [pickerError, setPickerError] = useState<string | null>(null);
  const nativePicker = hasNativeWorkspacePicker();
  const chooseDirectory = async () => {
    setPicking(true);
    setPickerError(null);
    try {
      const selected = await pickWorkspaceDirectory();
      if (selected) setPath(selected);
    } catch (error) {
      setPickerError(errorMessage(error, t("未知错误", "Unknown error")));
    } finally {
      setPicking(false);
    }
  };
  return (
    <Dialog
      title={t("添加工作区", "Add workspace")}
      description={t(
        "文件、搜索和命令工具会以这个目录作为安全边界。",
        "File, search, and command tools use this directory as their security boundary."
      )}
      onClose={onClose}
      footer={(
        <>
          <button type="button" className="button button--ghost" onClick={onClose}>
            {t("取消", "Cancel")}
          </button>
          <button
            type="button"
            className="button button--primary"
            disabled={!path.trim()}
            onClick={() => onSubmit(name, path)}
          >
            <FolderPlus size={15} />{t("添加工作区", "Add workspace")}
          </button>
        </>
      )}
    >
      <label className="field">
        <span className="field__label">{t("工作区路径 *", "Workspace path *")}</span>
        <span className="workspace-path-picker">
          <input
            className="input"
            autoFocus
            value={path}
            readOnly={nativePicker}
            onChange={(event) => !nativePicker && setPath(event.target.value)}
            placeholder={nativePicker
              ? t("点击浏览选择文件夹", "Click Browse to choose a folder")
              : "C:\\Projects\\my-app"}
          />
          {nativePicker && (
            <button type="button" className="button button--secondary" disabled={picking} onClick={() => void chooseDirectory()}>
              {picking ? <LoaderCircle size={15} className="spin" /> : <FolderOpen size={15} />}
              {picking ? t("选择中", "Selecting") : t("浏览", "Browse")}
            </button>
          )}
        </span>
        <span className={`field__hint ${pickerError ? "field__hint--error" : ""}`}>
          {pickerError
            ? t(
              "无法打开目录选择器：{error}",
              "Unable to open the directory picker: {error}",
              { error: pickerError }
            )
            : nativePicker
              ? t(
                "桌面版只接受系统目录选择器授权的文件夹。",
                "The desktop app only accepts folders authorized through the system directory picker."
              )
              : t(
                "浏览器预览无法读取系统目录，请粘贴绝对路径。",
                "The browser preview cannot read system directories. Paste an absolute path."
              )}
        </span>
      </label>
      <label className="field">
        <span className="field__label">{t("显示名称", "Display name")}</span>
        <input
          className="input"
          value={name}
          onChange={(event) => setName(event.target.value)}
          placeholder={t("留空时使用文件夹名称", "Leave blank to use the folder name")}
        />
      </label>
      <div className="safe-boundary-note">
        <Settings size={15} />
        <span>
          {t(
            "移除工作区只会从 Mework 取消注册，不会删除任何本地文件。",
            "Removing a workspace only unregisters it from Mework; no local files are deleted."
          )}
        </span>
      </div>
    </Dialog>
  );
}

export default App;
