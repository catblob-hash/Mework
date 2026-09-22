import {
  ArrowUp,
  ChevronDown,
  CircleAlert,
  Folder,
  FolderPlus,
  Gauge,
  GitBranch,
  GitCompareArrows,
  Globe,
  History,
  LoaderCircle,
  ListChecks,
  Monitor,
  PanelLeft,
  PanelRight,
  RotateCcw,
  ShieldCheck,
  Server,
  SlidersHorizontal,
  Square,
  SquareTerminal,
  Undo2,
  X
} from "lucide-react";
import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";
import { createPortal } from "react-dom";
import type { CSSProperties, ReactNode } from "react";
import { CommonErrorBoundary } from "./components/ErrorBoundary";
import { MeworkIcon } from "./components/MeworkIcon";
import { ComposerAddImages } from "./components/ComposerAddMenu";
import { ComposerCat } from "./components/ComposerCat";
import { PopoverMenu, type PopoverMenuSection } from "./components/PopoverMenu";
import { ContextUsageMeter } from "./components/ContextUsageMeter";
import { ImageStrip } from "./components/ImageStrip";
import { SelectedElementChips } from "./components/SelectedElementChips";
import { imagesWithoutElementCrops, selectedElementImageFile } from "./lib/selectedElement";
import { ConversationSettings } from "./components/ConversationSettings";
import { ConversationTemplateEditor } from "./components/ConversationTemplateEditor";
import { ConversationView } from "./components/ConversationView";
import {
  StreamedConversationView,
  StreamedSubagentPanel,
  subagentViewMessages
} from "./components/StreamedConversationView";
import { QuestionDock } from "./components/QuestionDock";
import { ToolApprovalDock } from "./components/ToolApprovalDock";
import { QueuedMessageList } from "./components/QueuedMessageList";
import { Dialog, EmptyState, IconButton } from "./components/Common";
import { GlobalSettings } from "./components/GlobalSettings";
import {
  clampSidebarWidth,
  Sidebar,
  SIDEBAR_DEFAULT_WIDTH
} from "./components/Sidebar";
import { RemoteDirectoryPicker } from "./components/RemoteDirectoryPicker";
import { ProjectSelector, TerminalShellButton, terminalShellMenuItems, WorkspaceMemberSelector } from "./components/ProjectChips";
import { ProjectDialog } from "./components/ProjectDialog";
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
import type { TaskSources } from "./lib/taskContainer";
import { reorderItems } from "./components/usePointerDrag";
import { createId } from "./lib/id";
import { estimateContextsTokens, liveContextTokens } from "./lib/contextTokens";
import { hasUsableBaseUrl, isEncryptedReasoning, supportsVision } from "./lib/modelCapabilities";
import {
  imageShortIdsInUse,
  nextImageShortId,
  reserveQueuedMessageIds
} from "./lib/imageShortIds";
import { acceptPastedImages } from "./lib/imagePaste";
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
  type SendPipelineHost,
  type ToolExposureMode
} from "./lib/sendPipeline";
import {
  conversationWorkspaces,
  hostIsWindows,
  isReservedWorkspace,
  projectWorkspaces,
  terminalShellLabel,
  terminalShellsFor,
  runEnvKey,
  sameMachine,
  TEMPORARY_WORKSPACE_ID,
  toolsForHost,
  withWorkspaceArgument,
  workspaceDirectoryLabel,
  workspaceLocationTitle
} from "./lib/workspaces";
import {
  settingsAtToolLockFloor,
  toolLockOf,
  withRunToolLock
} from "./lib/toolLock";
import { grantsWebFetch } from "./lib/webSearch";
import type { NewConversationSource, TerminalShell } from "./lib/workspaces";
import {
  draftAsConversation,
  draftSlotOf,
  DRAFT_CONVERSATION_ID,
  isDraftConversationId
} from "./lib/draftConversation";
import type { DraftConversationState } from "./lib/draftConversation";
import { listWslDistros } from "./lib/runtime";
import { applyConversationTemplate, attestEditedToolContext, attestInsertedToolContext, cancelConversationRun, cancelModelRun, defaultConversationWebSearchSettings, deleteConversationTemplate, deleteHook, deleteMcpServer, deleteSkill, executeTool, forkConversationContexts, listConversationTemplates, listForkDecisions, listPendingForkStarts, listPendingForkRequests, listPendingToolPrompts, listWakePendingConversations, loadConversationPlan, loadConversationRemote, loadDocument, prepareImageAttachment, previewConversationTemplate, probeMcpServer, refreshCapabilities, revealCapabilityLocation, updateConversationTemplate, requestToolApproval, resetDocument, resolveForkRequest, resolveToolPrompt, runModel, skipWorkflowStep, steerModelRun, workflowStepRecord } from "./lib/runtime";
import { SECURITY_LEVEL_OPTIONS, securityLevelLabel } from "./lib/securityLevels";
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
import { BrowserPanel } from "./components/BrowserPanel";
import { GitReviewPanel } from "./components/GitReviewPanel";
import { ShellTaskPanel } from "./components/ShellTaskPanel";
import { FilesPane } from "./components/FilesPane";
import type { FilesPaneOpenRequest } from "./components/FilesPane";
import { setPathOpenHandler } from "./lib/pathLinks";
import { workspaceRelativePath } from "./lib/workspaceFiles";
import { HistoryPane } from "./components/HistoryPane";
import { PaneTiles } from "./components/PaneTiles";
import { PaneToolbar } from "./components/PaneToolbar";
import { SidePane } from "./components/SidePane";
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
import { createTerminalController, terminalSessionKey } from "./lib/terminalController";
import type { TerminalSessionState } from "./lib/terminal";
import {
  initialTerminalTabsState,
  terminalDisplayNumber,
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
  type PreviewServerSnapshot
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
  gitSurfaceKey,
  gitSurfaceProjectId,
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
  expandedPane,
  focusedPane,
  isPrimaryPreviewSession,
  loadSidePanesState,
  openPreviewSession,
  paneIsOpen,
  paneKind,
  paneTarget,
  persistSidePanesState,
  previewPaneId,
  previewSessionBelongsToConversation,
  previewSessionsFor,
  shellPaneId,
  sidePaneDomId,
  sidePaneLayoutFor,
  sidePanesReducer,
  subagentHistoryPaneId,
  subagentPaneId,
  type SidePaneId,
  type SidePanesAction
} from "./lib/sidePanes";
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
  sameConversationPresetSettings,
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
import { editableUserAgentDefinition } from "./lib/agentDefinitions";
import type {
  ProviderFamily,
  ApiProvider,
  AppDocument,
  AttachedWorkspace,
  ConversationTemplateSummary,
  ContextItem,
  Conversation,
  ConversationPlan,
  ConversationPreset,
  ConversationPresetSettings,
  ConversationSettings as ConversationSettingsType,
  ConversationToolLock,
  GlobalSettings as GlobalSettingsType,
  ImageAttachment,
  InsertableContextKind,
  JsonObject,
  ModelProfile,
  ModelRunRequest,
  ModelStreamEvent,
  ModelUsage,
  PendingForkRequest,
  ForkDecisionRecord,
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
  Workspace,
  WslDistro
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
  const composerImageDrafts = composerState.imageDrafts;
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
  const [sidebarOpen, setSidebarOpen] = useState(true);
  const [sidebarWidth, setSidebarWidth] = useState(loadSidebarWidth);
  const [sidebarResizing, setSidebarResizing] = useState(false);
  const [paneResizing, setPaneResizing] = useState(false);
  const [viewportWidth, setViewportWidth] = useState(() => window.innerWidth);
  const [saveAsPresetDialog, setSaveAsPresetDialog] = useState<{ name: string; description: string } | null>(null);
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
  /** The project whose edit dialog is open, from the sidebar's project menu. */
  const [projectEditor, setProjectEditor] = useState<string | null>(null);
  const editedProject = projectEditor
    ? document?.workspaces.find((workspace) => (
      workspace.id === projectEditor && workspace.kind === "directory"
    )) ?? null
    : null;
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
  /** One handle per mounted terminal tab; the tab's close control drives the panel through it. */
  const terminalPanelHandlesRef = useRef(new Map<string, TerminalPanelHandle>());
  const sidePanesStateRef = useRef(sidePanesState);
  const terminalTabsStateRef = useRef(terminalTabsState);
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
  const activeLayout = sidePaneLayoutFor(sidePanesState, activeConversationId);
  const currentPreviewSessions = previewSessionsFor(sidePanesState, activeConversationId);
  const activeFocusedPane = focusedPane(activeLayout);
  const activeExpandedPane = expandedPane(activeLayout);
  // Several subagent panes can be stacked; the focused one is the transcript the conversation's
  // own docks and body-loading effects follow.
  const selectedSubagentId = (activeFocusedPane && paneKind(activeFocusedPane) === "subagent"
    ? paneTarget(activeFocusedPane)
    : paneTarget(activeLayout.panes.find((pane) => paneKind(pane) === "subagent") ?? "review")) || null;
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

  /**
   * `surfaceKey` is what the snapshot is stored under: a project id, or a project id with the
   * member number when the snapshot is of another of the project's workspaces.
   */
  const refreshGitSnapshot = useCallback(async (
    conversationId: string,
    surfaceKey: string,
    target: GitTarget
  ): Promise<GitWorkspaceSnapshot | null | undefined> => {
    const projectId = gitSurfaceProjectId(surfaceKey);
    const workspace = documentStore.current()?.workspaces.find((candidate) => candidate.id === projectId);
    if (
      !hasBackendRuntime()
      // Drafts can hold mutation leases despite being absent from workspace conversation lists.
      || gitController.mutationIsActive(conversationId)
      || workspace?.conversations.some((conversation) => (
        gitController.mutationIsActive(conversation.id)
      ))
    ) return undefined;
    return gitController.refresh(conversationId, surfaceKey, target);
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
    for (const sessionId of previewSessionsFor(sidePanesStateRef.current, conversationId)) {
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
    for (const conversationId of Object.keys(sidePanesStateRef.current.previewSessions)) {
      if (!previewSessionBelongsToConversation(sessionId, conversationId)) continue;
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
    // A terminal pane with no tabs has nothing to show: the last tab leaving is what closed it,
    // so asking for the pane again asks for a terminal to put in it.
    if (pane === "terminal") dispatchTerminalTabs({ type: "ensure", conversationId });
    dispatchSidePanes({ type: "open", conversationId, pane });
  }, [dispatchSidePanes, dispatchTerminalTabs]);

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
  const lastPreviewBoundsRef = useRef<
    Record<string, { x: number; y: number; width: number; height: number }>
  >({});
  const previewPageHeight = useCallback((sessionId: string, height: number) => (
    Math.max(1, height - (previewReservedBottomRef.current[sessionId] ?? 0))
  ), []);

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
      lastPreviewBoundsRef.current = {
        ...lastPreviewBoundsRef.current,
        [sessionId]: { x: rect.x, y: rect.y, width: rect.width, height: rect.height }
      };
      await setBrowserPanelBounds(sessionId, {
        x: rect.x,
        y: rect.y,
        width: rect.width,
        height: previewPageHeight(sessionId, rect.height),
        visible: true
      }, epoch).catch(() => undefined);
      return;
    }
  }, [previewPageHeight]);

  /**
   * Presents this conversation's preview page.
   *
   * There is exactly one, and the host keys it by the conversation id, so `sessionId` only ever
   * names the page that already exists; omitting it opens that same page. The tab roster the Agent
   * used to publish is gone with the tools that published it.
   */
  const openBrowserTab = useCallback(async (sessionId?: string) => {
    if (!activeConversationId) return;
    const conversationId = activeConversationId;
    const targetSessionId = sessionId ?? conversationId;
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
        || browserController.visibleSession() !== targetSessionId
        || !browserIntentIsCurrent(targetSessionId, intentEpoch, "open")
      ) {
        return;
      }
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
   * Shows one agent's read-only transcript in its own pane. The conversation stays on screen
   * beside it, so the next selection is one click away.
   */
  const openSubagentPanel = useCallback((subagentId: string) => {
    setEditor(null);
    openPane(subagentPaneId(subagentId));
  }, [openPane]);

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
    const wasLast = terminalTabsFor(terminalTabsStateRef.current, conversationId).tabs.length <= 1;
    const handle = terminalPanelHandlesRef.current.get(
      terminalSessionKey(conversationId, terminalId)
    );
    const ended = handle
      ? handle.close()
      : requestTerminalSessionClose(conversationId, terminalId).catch(() => undefined);
    void Promise.resolve(ended).then(() => {
      dispatchTerminalTabs({ type: "close", conversationId, terminalId });
      if (!wasLast) return;
      dispatchSidePanes({ type: "close", conversationId, pane: "terminal" });
      if (activeConversationIdRef.current === conversationId) {
        composerTextareaRef.current?.focus({ preventScroll: true });
      }
    });
  }, [dispatchSidePanes, dispatchTerminalTabs, requestTerminalSessionClose]);

  /**
   * Opens the pane behind a task row. Subagents and workflows show a transcript instead and never
   * reach here.
   *
   * A terminal row has no pane of its own: the conversation's single PTY lives in the terminal
   * pane, so its row opens that pane.
   */
  const openTaskItemPage = useCallback(async (item: TaskItem) => {
    const conversationId = activeConversationIdRef.current;
    if (!conversationId) return;
    if (item.kind === "terminal") {
      openPane("terminal");
      return;
    }
    if (item.kind === "shell") {
      openPane(shellPaneId(item.shell.shellTaskId));
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
    if (item.kind === "preview") await openBrowserTab(conversationId);
  }, [documentStore, openBrowserTab, openPane, setActiveConversationId, setActiveWorkspaceId]);

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
    // The host moved the conversation into or out of plan mode on its own. It has
    // already committed the level, so this mirrors it into the read model without
    // writing back — persisting here would race the host's own write.
    if (event.type === "conversationSecurityLevelChanged") {
      documentStore.update((current) => current ? {
        ...current,
        workspaces: current.workspaces.map((workspace) => ({
          ...workspace,
          conversations: workspace.conversations.map((conversation) => (
            conversation.id === event.conversationId
            && conversation.settings.securityLevel !== event.securityLevel
              ? {
                ...conversation,
                settings: { ...conversation.settings, securityLevel: event.securityLevel }
              }
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
      const pane = shellPaneId(event.shellTaskId);
      if (paneIsOpen(sidePaneLayoutFor(sidePanesStateRef.current, event.conversationId), pane)) {
        dispatchSidePanes({ type: "close", conversationId: event.conversationId, pane });
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
  }), [documentStore, openToolPrompt, closeToolPrompt, openForkRequest, closeForkRequest, appendForkDecision, dispatchSidePanes]);

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
  /**
   * Whether the composer belongs to a task that has never been sent, which is where the cat
   * loafs. The renderer draft is only one half of that state: picking a workspace — or opening
   * a new task while one is already picked — turns the draft into a real conversation that is
   * still unsent, still withheld from the sidebar, and still the same empty desk to the user.
   * Keying the cat on the draft alone hid it from every install that has a workspace, which is
   * every install after the first pick.
   */
  const composerIsUnsentTask = draftActive || Boolean(
    activeConversation
    && activeWorkspace
    && draftSlotOf(activeWorkspace.conversations)?.id === activeConversation.id
  );
  /** The project's own workspaces as this conversation uses them: its worktree stands in for the first. */
  const activeProjectWorkspaces = useMemo(
    () => projectWorkspaces(activeWorkspace, activeConversation),
    [activeWorkspace, activeConversation]
  );
  /**
   * The project workspace the composer's workspace chip has selected, 1-based. A selection
   * past the end — a workspace removed from the project since — falls back to the first.
   */
  const activeWorkspaceMember = (() => {
    const selected = activeConversation ? selectedWorkspaceMembers[activeConversation.id] ?? 1 : 1;
    return selected >= 1 && selected <= activeProjectWorkspaces.length ? selected : 1;
  })();
  const activeSelectedWorkspace = activeProjectWorkspaces[activeWorkspaceMember - 1] ?? null;
  /**
   * The key the Git snapshot of the selected workspace is stored under: the project id for its
   * first workspace, and the id with the member number for the others, so a snapshot of one
   * directory is never read as another's.
   */
  const activeGitSurfaceKey = activeWorkspace
    ? gitSurfaceKey(activeWorkspace.id, activeWorkspaceMember)
    : undefined;
  const activeGitSnapshotEntry = activeConversation ? gitSnapshots[activeConversation.id] : undefined;
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
   * Whether the active workspace's directory is on another machine. The Git
   * surface, the worktree toggle and the file pane act on a checkout in this
   * filesystem; a remote workspace has none here, and the host refuses to
   * resolve its path locally.
   */
  const activeWorkspaceIsRemote = Boolean(activeWorkspace?.machine);
  /**
   * The conversation's own checkout — the project's first workspace, or its worktree. The file
   * pane, previews and worktree bookkeeping act on this one whatever the chip has selected.
   */
  const activePrimaryGitTarget = useMemo((): GitTarget | null => {
    if (!activeGitConversationId || activeWorkspaceIsRemote) return null;
    if (!draftActive) return gitConversationTarget(activeGitConversationId);
    if (!activeGitWorkspaceId || activeGitWorkspaceKind !== "directory") return null;
    return gitWorkspaceTarget(activeGitWorkspaceId);
  }, [activeGitConversationId, activeGitWorkspaceId, activeGitWorkspaceKind, activeWorkspaceIsRemote, draftActive]);
  /** Whether the selected workspace's directory is on another machine, where the host has no checkout. */
  const activeSelectedWorkspaceIsRemote = Boolean(activeSelectedWorkspace?.machine);
  /**
   * The checkout the Git chip, the status card and the review pane describe: the selected
   * workspace. Another workspace of the project is addressed by its number within the project,
   * never by path, and a remote one has no Git surface here.
   */
  const activeGitTarget = useMemo((): GitTarget | null => {
    if (activeWorkspaceMember === 1) return activePrimaryGitTarget;
    if (!activeGitConversationId || !activeGitWorkspaceId || activeSelectedWorkspaceIsRemote) return null;
    return gitWorkspaceTarget(activeGitWorkspaceId, activeWorkspaceMember);
  }, [
    activeGitConversationId,
    activeGitWorkspaceId,
    activePrimaryGitTarget,
    activeSelectedWorkspaceIsRemote,
    activeWorkspaceMember
  ]);
  /**
   * Whether the conversation has started. The project it belongs to is settled from then on,
   * so the composer stops offering to change it.
   */
  const activeConversationStarted = Boolean(
    !draftActive && activeConversation && activeConversation.contexts.length > 0
  );
  /**
   * How many of the conversation's workspace numbers the project takes. A temporary project
   * still takes one — the host gives it a scratch directory — so attached workspaces always
   * start after it.
   */
  const activeProjectWorkspaceCount = Math.max(1, activeProjectWorkspaces.length);
  /** The shells a terminal in the selected workspace can start, which follow its machine. */
  const activeTerminalShells = useMemo(
    () => terminalShellsFor(activeSelectedWorkspace?.machine ?? activeWorkspace?.machine, platform),
    [activeSelectedWorkspace?.machine, activeWorkspace?.machine, platform]
  );
  /**
   * Opens a terminal in the workspace the composer has selected, in `shell`.
   *
   * The tab every conversation starts with is only a place for the pane to park until a shell is
   * asked for; while it has never started one, the choice lands in it rather than beside it.
   */
  const openTerminalInSelectedWorkspace = useCallback((shell: TerminalShell) => {
    const conversationId = activeConversationIdRef.current;
    if (!conversationId || isDraftConversationId(conversationId)) return;
    const launch = { workspace: activeWorkspaceMember, shell };
    const layout = terminalTabsFor(terminalTabsStateRef.current, conversationId);
    const [only] = layout.tabs;
    const pristine = layout.tabs.length === 1
      && layout.nextOrdinal === only.ordinal + 1
      && only.launch === null
      && !terminalController.current()[terminalSessionKey(conversationId, only.id)];
    if (pristine) {
      dispatchTerminalTabs({ type: "configure", conversationId, terminalId: only.id, launch });
    } else {
      dispatchTerminalTabs({ type: "add", conversationId, launch });
    }
    openPane("terminal");
  }, [activeWorkspaceMember, dispatchTerminalTabs, openPane, terminalController]);
  useEffect(() => {
    const conversationId = activeConversation?.id;
    const workspaceId = activeGitSurfaceKey;
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
  }, [activeConversation?.id, activeGitTarget, activeGitSurfaceKey, refreshGitSnapshot]);
  // A workspace that stopped being a Git repository has no review pane to show. Leaving it up
  // would strand the user on a pane whose panel has nothing to render.
  useEffect(() => {
    if (!activeConversation || activeGitSnapshotState !== null) return;
    if (!paneIsOpen(sidePaneLayoutFor(sidePanesStateRef.current, activeConversation.id), "review")) return;
    dispatchSidePanes({ type: "close", conversationId: activeConversation.id, pane: "review" });
  }, [activeConversation, activeGitSnapshotState, dispatchSidePanes]);
  // The file pane reads this machine's filesystem; a conversation moved to a
  // remote workspace has nothing for it to show.
  useEffect(() => {
    if (!activeConversation || !activeWorkspaceIsRemote) return;
    if (!paneIsOpen(sidePaneLayoutFor(sidePanesStateRef.current, activeConversation.id), "files")) return;
    dispatchSidePanes({ type: "close", conversationId: activeConversation.id, pane: "files" });
  }, [activeConversation, activeWorkspaceIsRemote, dispatchSidePanes]);
  /**
   * Shows the review pane.
   *
   * Deliberately unconditional: the status card's rows all point at the same pane, and returning
   * early when it was already open used to make a second click do nothing at all.
   */
  const openGitReview = useCallback(() => {
    if (!activeConversation || !activeGitSnapshot) return;
    openPane("review");
  }, [activeConversation, activeGitSnapshot, openPane]);
  const activeComposerDraft = activeConversation ? composerDrafts[activeConversation.id] ?? "" : "";
  const activeComposerImages = activeConversation ? composerImageDrafts[activeConversation.id] ?? [] : [];
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
          snapshot: gitSnapshotForWorkspace(gitSnapshots[conversation.id], activeGitSurfaceKey),
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
    };
    const sections: PopoverMenuSection[] = [];
    for (const choice of enabledModelChoices) {
      const item = {
        id: choice.value,
        label: choice.model.id,
        checked: choice.value === activeModelChoice?.value,
        onSelect: () => selectModel(choice.provider.id, choice.model.id)
      };
      const existing = sections.find((section) => section.id === choice.provider.id);
      if (existing) existing.items.push(item);
      else sections.push({ id: choice.provider.id, label: choice.provider.name, items: [item] });
    }
    return sections;
  }, [activeModelChoice, documentStore, enabledModelChoices]);
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
  const securityLevelLabelFor = (level: SecurityLevel): string => securityLevelLabel(level, t);
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
  const activeSecurityLevelLabel = securityLevelLabelFor(
    activeConversation?.settings.securityLevel ?? SECURITY_LEVEL_OPTIONS[0]
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
  const activeBranchLabel = (activeWorkspaceMember === 1 ? activeWorktree?.branch : undefined)
    ?? activeGitSnapshot?.branch
    ?? null;
  /** The directory a path written in this conversation's transcript is written against. */
  const timelinePathBaseDir = activeWorktree?.path
    ?? (activeWorkspaceMember === 1 ? activeGitSnapshot?.worktreeRoot : undefined)
    ?? activeWorkspace?.path
    ?? null;
  /** The checkout the file pane browses, which is what the pane can show a file from. */
  // A remote workspace has no host directory for the file pane or a timeline path click to open.
  const filesPaneRoot = activeWorkspaceIsRemote ? null : activeWorktree?.path ?? activeWorkspace?.path ?? null;
  const [filesPaneRequest, setFilesPaneRequest] = useState<
    (FilesPaneOpenRequest & { conversationId: string }) | null
  >(null);
  const filesPaneRequestNonce = useRef(0);

  /**
   * A file path clicked anywhere in a transcript opens in the file pane.
   *
   * The interceptor that catches the click lives outside React, so this is where
   * the two meet: the handler is what knows which workspace is open and can turn
   * an address written against the conversation's working directory into a path
   * inside the checkout the pane browses. Anything that does not resolve to one —
   * a file elsewhere on the disk, a click while no workspace is open — is left
   * for the file manager by answering false.
   */
  useEffect(() => {
    const conversationId = activeConversation?.id ?? null;
    if (conversationId === null || filesPaneRoot === null) return;
    return setPathOpenHandler(({ path, baseDir, line }) => {
      const relative = workspaceRelativePath(path, baseDir ?? timelinePathBaseDir, filesPaneRoot);
      if (relative === null) return false;
      filesPaneRequestNonce.current += 1;
      setFilesPaneRequest({
        conversationId,
        path: relative,
        line,
        nonce: filesPaneRequestNonce.current
      });
      openPane("files");
      return true;
    });
  }, [activeConversation?.id, filesPaneRoot, openPane, timelinePathBaseDir]);
  const branchChipDisabled = Boolean(
    !activeGitSnapshot
    || activeModelRunning
    || activeWorkspaceLifecycleOperationRunning
    || (activeConversation && gitMutationConversationIds.has(activeConversation.id))
  );
  // Tool-description overrides are assembled by the backend from a trusted snapshot at runtime.
  const activeConversationTools = useMemo(() => {
    if (!document) return [];
    return toolsForHost(document.tools, platform)
      .map((tool) => localizeToolDescriptor(tool, resolvedLanguage));
  }, [document, platform, resolvedLanguage]);
  /**
   * What the timeline's manual tool cards are edited against: the catalog plus
   * the `workspace` argument the host puts on the wire once the conversation
   * has more than one workspace. Only the timeline sees it — the tool picker
   * and the subagent panel name tools, they do not fill in calls. Keyed on the
   * two conversation fields that decide the numbers, not the conversation,
   * so a streaming turn does not hand the timeline a fresh descriptor list on
   * every context row.
   */
  const activeAttachedWorkspaces = activeConversation?.attachedWorkspaces ?? null;
  const activeTimelineTools = useMemo(
    () => withWorkspaceArgument(
      activeConversationTools,
      conversationWorkspaces(
        activeWorkspace,
        activeAttachedWorkspaces
          ? { worktree: activeWorktree, attachedWorkspaces: activeAttachedWorkspaces }
          : null
      ),
      /^win/i.test(platform),
      t("工作区编号", "Workspace number")
    ),
    [activeConversationTools, activeWorkspace, activeAttachedWorkspaces, activeWorktree, platform, t]
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
    openPane("tasks");
    window.requestAnimationFrame(() => {
      window.document
        .querySelector(`[data-workflow-run="${CSS.escape(runId)}"]`)
        ?.scrollIntoView({ block: "nearest" });
    });
  }, [openPane]);
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
  /**
   * Exactly one surface owns the pending card. Several panes can be open at once now, so the
   * dock has to be addressed to a single one or the same prompt would be answerable from every
   * open subagent pane and the composer at the same time.
   */
  const approvalDockOwner: SidePaneId | "composer" = (() => {
    if (!activeToolPrompt) return "composer";
    if (activeToolPrompt.kind === "plan_exit" && planPageOpen) return "plan";
    if (activeToolPrompt.sourceAgent || activeToolPrompt.sourceCallId) {
      const target = approvalPromptSubagentView(subagents, activeToolPrompt);
      const pane = target ? subagentPaneId(target.id) : null;
      if (pane && paneIsOpen(activeLayout, pane)) return pane;
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
  const closePagesServedAt = useCallback(async (conversationId: string, address: string) => {
    const doomed = activeBrowserSessionsRef.current.filter(
      ({ status }) => previewUrlIsServedAt(status.url, address)
    );
    const layout = sidePaneLayoutFor(sidePanesStateRef.current, conversationId);
    for (const { sessionId } of doomed) {
      if (paneIsOpen(layout, previewPaneId(sessionId))) {
        // A refusal is not worth reporting: the page may already be gone, and the pane's own body
        // reads the committed URL either way.
        await navigateBrowser(sessionId, "about:blank").catch(() => undefined);
        continue;
      }
      await closePreviewSession(conversationId, sessionId);
    }
  }, [closePreviewSession]);
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
    target: GitTarget,
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
      !server.sessionId || server.sessionId === conversationId
    ));
    const previous = previewServersRef.current;
    previewServersRef.current = mine;
    // Replace the state only on a real change: this runs on a host event, and a fresh array every
    // time would re-render the whole shell for a list that says the same thing.
    if (
      previous.length !== mine.length
      || mine.some((server, index) => (
        previous[index].serverId !== server.serverId || previous[index].status !== server.status
      ))
    ) {
      setPreviewServers(mine);
    }
    // Only the servers somebody stopped take their page with them. A server that exited on its
    // own is gone from the list too, and its page stays: what it shows then — a connection
    // refused, a stack trace the framework printed on the way down — is the whole diagnosis, and
    // the pane's own card offers the restart.
    const stopped = previous.filter((server) => (
      stoppedIds.includes(server.serverId) && !mine.some((live) => live.serverId === server.serverId)
    ));
    for (const server of stopped) {
      await closePagesServedAtRef.current(conversationId, previewServerAddress(server));
    }
  }, []);
  useEffect(() => {
    const conversationId = activeConversation?.id;
    if (!conversationId || !activePrimaryGitTarget || !hasBackendRuntime()) {
      previewServersRef.current = [];
      setPreviewServers([]);
      return undefined;
    }
    const target = activePrimaryGitTarget;
    // A conversation switch starts from nothing rather than from the previous conversation's list:
    // a server missing from *this* conversation's list was never this conversation's to close.
    previewServersRef.current = [];
    setPreviewServers([]);
    void refreshPreviewServers(conversationId, target, []);
    return onAppPushEvent((event) => {
      if (event.type !== "previewServersChanged") return;
      void refreshPreviewServers(conversationId, target, event.stopped);
    });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [activeConversation?.id, JSON.stringify(activePrimaryGitTarget), refreshPreviewServers]);
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
  const selectedTaskRowId = useMemo(() => {
    if (!activeFocusedPane) return null;
    const kind = paneKind(activeFocusedPane);
    if (kind === "subagent") return paneTarget(activeFocusedPane);
    if (kind === "preview") return activeFocusedPane;
    if (kind === "shell") return paneTarget(activeFocusedPane);
    if (kind === "plan") return "plan";
    // A targeted request ledger is that agent's surface too, so reading what it
    // sent keeps its row marked rather than clearing the mark the transcript set.
    if (kind === "history") return paneTarget(activeFocusedPane);
    return null;
  }, [activeFocusedPane]);
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
  const gitMutationDisabledReason = activeModelRunBusy
    ? t(
      "模型或子代理正在使用工作区，结束后才能执行 Git 写操作",
      "A model or subagent is using the workspace. Wait for it to finish before changing Git state."
    )
      : activeWorkspacePeerOperationRunning
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
  // The mirror of `gitMutationDisabledReason`: a Git write is rewriting the very checkout the
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

  const syncBrowserPanelBounds = useCallback((bounds: { x: number; y: number; width: number; height: number }) => {
    const sessionId = browserController.visibleSession();
    if (!isTauriRuntime() || !browserPanelOpen || previewPaneCovered || !sessionId) return;
    const intent = browserController.currentIntent(sessionId);
    if (intent?.desired !== "open") return;
    lastPreviewBoundsRef.current = {
      ...lastPreviewBoundsRef.current,
      [sessionId]: { x: bounds.x, y: bounds.y, width: bounds.width, height: bounds.height }
    };
    // Spelled out field by field: a measured `DOMRect` keeps its coordinates on the prototype, so
    // spreading one into the wire payload would silently drop every one of them.
    void setBrowserPanelBounds(sessionId, {
      x: bounds.x,
      y: bounds.y,
      width: bounds.width,
      height: previewPageHeight(sessionId, bounds.height),
      visible: true
    }, intent.epoch).then((status) => {
      if (
        !browserIntentIsCurrent(sessionId, intent.epoch, "open")
        || browserController.visibleSession() !== sessionId
      ) return;
      browserController.updateStatuses((current) => ({ ...current, [sessionId]: status }));
    }).catch(() => undefined);
  }, [browserController, browserIntentIsCurrent, browserPanelOpen, previewPageHeight, previewPaneCovered]);

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

  // The main area only shows an agent that still exists. A selection made while
  // the call was streaming follows the agent to its persisted record, whose view
  // id switches from the call id to the stable name id. A selection that lands
  // on a workflow run is dropped outright: a run is a script and has no
  // transcript, so there is nothing for the main area to show. An agent's
  // request ledger is addressed by the same id and follows it the same way.
  useEffect(() => {
    const conversationId = activeConversationId;
    if (!conversationId) return;
    for (const pane of sidePaneLayoutFor(sidePanesStateRef.current, conversationId).panes) {
      const kind = paneKind(pane);
      if (kind !== "subagent" && kind !== "history") continue;
      const subagentId = paneTarget(pane);
      // The bare `history` pane is the conversation's own ledger and belongs to
      // no agent.
      if (!subagentId) continue;
      const paneFor = kind === "subagent" ? subagentPaneId : subagentHistoryPaneId;
      const matched = findOpenableSubagentView(subagents, subagentId);
      if (!matched) {
        dispatchSidePanes({ type: "close", conversationId, pane });
      } else if (matched.id !== subagentId) {
        dispatchSidePanes({ type: "close", conversationId, pane });
        dispatchSidePanes({ type: "open", conversationId, pane: paneFor(matched.id) });
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
      // worktree belongs to a persisted conversation id and the draft carries `worktreeRequested`.
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
    activeGitSurfaceKey,
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

  /**
   * Create worktrees from the current workspace HEAD and persist their record for trusted host path resolution.
   * On disable, release before clearing the conversation pointer because the host uses that record to find the tree.
   * Drafts retain only the request until they materialize before their first send.
   */
  const toggleConversationWorktree = useCallback(async (enabled: boolean) => {
    const conversationId = activeConversationId;
    const workspaceId = activeWorkspaceId;
    const target = activePrimaryGitTarget;
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
    activePrimaryGitTarget,
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
      // A new preset opens with nothing. Its template is written on its own page,
      // which is also what mints the id — capturing the conversation's timeline
      // here would make "save as preset" a second, silent way to store a body.
      templateId: "",
      settings: captureConversationPresetSettings(activeConversation.settings)
    };
    handleGlobalSettingsChange((current) => ({
      ...current,
      conversationPresets: [...current.conversationPresets, preset]
    }));
  }, [activeConversation, document, handleGlobalSettingsChange]);

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
    if (workspacePreset) {
      return {
        settings: applyConversationPresetSettings(blankSettings, workspacePreset.settings, knownToolNames),
        presetId: workspacePreset.id
      };
    }
    // A remembered snapshot is the workspace's own unnamed draft: it has no preset identity.
    if (remembered) {
      return { settings: cloneConversationSettings(remembered, knownToolNames), presetId: "" };
    }
    const fallbackPreset = defaultConversationPreset(
      document.globalSettings,
      document.tools,
      resolvedLanguage,
      platform
    );
    return {
      settings: applyConversationPresetSettings(blankSettings, fallbackPreset.settings, knownToolNames),
      presetId: fallbackPreset.id
    };
  }, [documentStore, platform, resolvedLanguage]);

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
    attachedWorkspacesOverride: AttachedWorkspace[] = []
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
    const conversationId = createId("conv");
    let created: Conversation | null = null;
    // Materializing a draft preserves its exact settings; other creation paths resolve them now.
    const resolved = settingsOverride
      ? { settings: cloneConversationSettings(settingsOverride, knownToolNames), presetId: presetIdOverride }
      : resolveNewConversationSettings(target, source);
    if (!resolved) return null;
    const resolvedSettings = resolved.settings;
    documentStore.update((current) => {
      if (!current) return current;
      const conversation: Conversation = {
        id: conversationId,
        title: t("新任务", "New task"),
        createdAt: now,
        updatedAt: now,
        settings: resolvedSettings,
        contexts: initialContexts,
        queuedMessages: [],
        branches: [],
        userAbortedTasks: [],
        worktree: null,
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
    // A brand-new conversation starts on its own timeline; the per-conversation view state means
    // this only has to clear a stale entry left by a conversation id that was reused.
    dispatchSidePanes({ type: "remove_conversation", conversationId });
    dispatchTerminalTabs({ type: "remove_conversation", conversationId });
    return conversationId;
  }, [activeWorkspaceId, dispatchSidePanes, dispatchTerminalTabs, documentStore, resolveNewConversationSettings, t]);

  const discardDraftConversation = useCallback(() => {
    if (!draftConversationRef.current) return;
    setDraftConversation(null);
    // Panes are keyed by conversation id and the draft's is a constant, so a layout left
    // under it would reopen itself in the next brand-new task.
    dispatchSidePanes({ type: "remove_conversation", conversationId: DRAFT_CONVERSATION_ID });
    composerController.invalidateImages([DRAFT_CONVERSATION_ID]);
    composerController.updateDrafts((current) => {
      if (current[DRAFT_CONVERSATION_ID] === undefined) return current;
      const next = { ...current };
      delete next[DRAFT_CONVERSATION_ID];
      return next;
    });
  }, [composerController]);

  /**
   * Open a new task. A known workspace makes the conversation real at once, because a hand-built
   * tool call needs an id the host recognizes; each workspace returns to its own single slot. With
   * no workspace chosen there is nowhere to persist it yet, so it stays in the renderer until
   * {@link setDraftWorkspace} or the first send picks one.
   */
  const openDraftConversation = useCallback(async (
    workspaceId?: string,
    source: NewConversationSource = "global"
  ) => {
    const current = documentStore.current();
    if (!current) return;
    const requestedId = workspaceId ?? activeWorkspaceIdRef.current;
    const target = current.workspaces.find((workspace) => (
      workspace.id === requestedId && !deletingWorkspaceIdsRef.current.has(workspace.id)
    )) ?? null;
    const resolved = resolveNewConversationSettings(target, source);
    if (!resolved) return;
    // Starting a new task discards any existing unsent draft composer content.
    discardDraftConversation();
    if (target) {
      const slot = draftSlotOf(target.conversations);
      if (slot) {
        setActiveWorkspaceId(target.id);
        setActiveConversationId(slot.id);
      } else {
        createConversation(target.id, source);
      }
      setEditor(null);
      // The host only recognizes a conversation it has saved; a hand-built tool call here must not
      // outrun that write.
      await flushLatestDocument({ durable: true });
      return;
    }
    setDraftConversation({
      workspaceId: null,
      settings: resolved.settings,
      createdAt: new Date().toISOString(),
      worktreeRequested: false,
      runTarget: null,
      attachedWorkspaces: [],
      contexts: [],
      presetId: resolved.presetId,
      templateId: ""
    });
    setActiveWorkspaceId(null);
    setActiveConversationId(DRAFT_CONVERSATION_ID);
    setEditor(null);
  }, [
    createConversation,
    discardDraftConversation,
    documentStore,
    flushLatestDocument,
    resolveNewConversationSettings
  ]);
  openDraftConversationRef.current = openDraftConversation;

  /**
   * Move everything the renderer keyed by the draft's placeholder id onto the real conversation:
   * composer text, staged images, and the Git snapshot. A worktree is the one exception — creating
   * it changes branch, HEAD, and modifications, so its snapshot must be re-read rather than carried.
   */
  const adoptDraftConversationId = useCallback((
    conversationId: string,
    options: { migrateGitSnapshot?: boolean } = {}
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
    composerController.updateImageDrafts((current) => {
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
    if (options.migrateGitSnapshot === false) return;
    updateGitSnapshots((current) => (
      gitSnapshotsAfterDraftRedemption(current, DRAFT_CONVERSATION_ID, conversationId)
    ));
  }, [composerController, updateGitSnapshots]);

  /** Materialize the draft at send time, when no workspace was ever chosen for it. */
  const materializeDraft = useCallback((): {
    conversationId: string;
    workspaceId: string;
    worktreeRequested: boolean;
  } | null => {
    const draft = draftConversationRef.current;
    if (!draft) return null;
    const workspaceId = draft.workspaceId ?? TEMPORARY_WORKSPACE_ID;
    const created = createConversation(workspaceId, "global", draft.settings, draft.runTarget, null, draft.contexts, draft.presetId, draft.templateId, draft.attachedWorkspaces);
    if (!created) return null;
    const worktreeRequested = draft.worktreeRequested
      && documentStore.current()?.workspaces.find(
        (workspace) => workspace.id === workspaceId
      )?.kind === "directory";
    adoptDraftConversationId(created, { migrateGitSnapshot: !worktreeRequested });
    return {
      conversationId: created,
      workspaceId,
      // Only directory workspaces can create worktrees; discard an unrealizable request before it can block sending.
      worktreeRequested
    };
  }, [adoptDraftConversationId, createConversation, documentStore]);

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
   * A draft is materialized and flushed FIRST, before either write. The host
   * attests the copied tool cards against the TARGET conversation id, so the
   * target has to be real; and once it is, both writes go through explicit ids
   * rather than through `updateActiveConversation`, whose draft branch reads a
   * ref that this tick's own materialization has already invalidated.
   */
  const applyPresetBody = useCallback(async (preset: ConversationPreset) => {
    setTemplateError(null);
    let workspaceId = activeWorkspaceId;
    let conversationId = activeConversationId;
    if (isDraftConversationId(conversationId)) {
      const redeemed = materializeDraft();
      if (!redeemed) return;
      workspaceId = redeemed.workspaceId;
      conversationId = redeemed.conversationId;
      await flushLatestDocument({ durable: true });
    }
    if (!workspaceId || !conversationId) return;
    const knownToolNames = new Set(documentStore.current()?.tools.map((tool) => tool.name) ?? []);
    updateConversation(workspaceId, conversationId, (conversation) => ({
      ...conversation,
      settings: applyConversationPresetSettings(
        conversation.settings,
        preset.settings,
        knownToolNames
      ),
      presetId: preset.id
    }));
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
    } catch (reason) {
      setTemplateError(failureMessage(reason, t("无法套用对话模板", "Could not apply the conversation template")));
    }
  }, [
    activeConversationId,
    activeWorkspaceId,
    documentStore,
    flushLatestDocument,
    materializeDraft,
    t,
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
    if (!activeConversation || activeConversation.contexts.length === 0) return false;
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
    const preset = presetId === IMPLICIT_CONVERSATION_PRESET_ID
      ? implicitConversationPreset(current.tools, resolvedLanguage, platform)
      : current.globalSettings.conversationPresets.find((item) => item.id === presetId);
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
    platform,
    resolvedLanguage,
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
    (resource: ResourceDescriptor) => probeMcpServer(resource.id, createId("mcp-probe")),
    []
  );

  /** Renames a saved preset. Conversations cite it by id, so the trace follows. */
  const renameConversationPreset = useCallback((presetId: string, name: string) => {
    if (!name.trim()) return;
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
    handleGlobalSettingsChange((current) => ({
      ...current,
      conversationPresets: current.conversationPresets.map((preset) => (
        preset.id === presetId ? { ...preset, settings } : preset
      ))
    }));
  }, [handleGlobalSettingsChange]);

  /* Records which template a preset opens with, the moment its body is written.
   * It is deliberately not part of `saveConversationPreset`: the body is already
   * on disk by then, and waiting for the preset dialog's own Save would leave a
   * window in which closing that dialog stranded a body nothing cites. */
  const bindPresetTemplate = useCallback((presetId: string, templateId: string) => {
    handleGlobalSettingsChange((current) => ({
      ...current,
      conversationPresets: current.conversationPresets.map((preset) => (
        preset.id === presetId ? { ...preset, templateId } : preset
      ))
    }));
  }, [handleGlobalSettingsChange]);

  /**
   * Choosing a workspace for the renderer-held draft is what makes it real: the host can only
   * accept contexts against a conversation that lives somewhere. Clearing the workspace leaves the
   * draft where it is, since there is nothing to persist it into.
   */
  const setDraftWorkspace = useCallback(async (workspaceId: string | null) => {
    const draft = draftConversationRef.current;
    if (!draft) return;
    if (workspaceId === null) {
      setDraftConversation((current) => (current ? { ...current, workspaceId: null } : current));
      setActiveWorkspaceId(null);
      return;
    }
    const target = documentStore.current()?.workspaces.find(
      (workspace) => workspace.id === workspaceId
    );
    if (!target) return;
    const slot = draftSlotOf(target.conversations);
    // Reuse this workspace's own empty slot rather than stacking a second one beside it, carrying
    // across whatever the user already set up on the draft.
    let conversationId = slot?.id ?? null;
    if (slot) {
      updateConversation(workspaceId, slot.id, (conversation) => ({
        ...conversation,
        settings: draft.settings,
        runTarget: draft.runTarget,
        attachedWorkspaces: draft.attachedWorkspaces.length
          ? draft.attachedWorkspaces
          : conversation.attachedWorkspaces,
        presetId: draft.presetId,
        templateId: draft.templateId,
        contexts: draft.contexts.length ? draft.contexts : conversation.contexts
      }));
    } else {
      conversationId = createConversation(
        workspaceId,
        "global",
        draft.settings,
        draft.runTarget,
        null,
        draft.contexts,
        draft.presetId,
        draft.templateId,
        draft.attachedWorkspaces
      );
    }
    if (!conversationId) return;
    adoptDraftConversationId(conversationId);
    setActiveWorkspaceId(workspaceId);
    setActiveConversationId(conversationId);
    setDraftConversation(null);
    // The host recognizes a conversation only from its own saved document, so anything the user
    // does next — a worktree, a Git write, a hand-built tool call — has to wait for this barrier.
    await flushLatestDocument({ durable: true });
  }, [
    adoptDraftConversationId,
    createConversation,
    documentStore,
    flushLatestDocument,
    setActiveWorkspaceId,
    updateConversation
  ]);

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
       * Toggles this conversation's preview pane.
       *
       * With no tab strip there is no creation entry point here: the shortcut opens the session the
       * model already minted, or mints the primary one when there is none yet.
       */
      "panel.browser.toggle": () => {
        if (openPreviewSessionId) {
          closePane(previewPaneId(openPreviewSessionId));
          return;
        }
        void openBrowserTab(currentPreviewSessions.at(0));
      },
      "panel.close": () => closeLastPane()
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


  /**
   * Changes an existing project's name and the workspaces after its first. The first workspace
   * is the project's identity — its conversations' worktrees, files pane and capability files
   * hang off it — so the dialog never offers to change it and this never does.
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
        dispatchSidePanes({ type: "remove_conversation", conversationId: conversation.id });
        dispatchTerminalTabs({ type: "remove_conversation", conversationId: conversation.id });
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
    const workspaceId = activeWorkspaceId;
    // A draft has no workspace until it is sent, yet its hand-written content is still deletable.
    if (
      !activeConversation
      || (!workspaceId && !draftActive)
      || contextMutationIsBlocked(activeConversation.id)
    ) return;
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
        if (draftActive) {
          // Draft content lives in the renderer, so restore it there — and only while that draft is
          // still open, because switching conversations discards the draft outright.
          if (!isDraftConversationId(activeConversationIdRef.current)) return;
          setDraftConversation((current) => {
            if (!current || current.contexts.some((context) => context.id === item.id)) return current;
            const contexts = [...current.contexts];
            contexts.splice(Math.min(index, contexts.length), 0, item);
            return { ...current, contexts };
          });
          setContextUsage((current) => {
            const next = { ...current };
            delete next[conversationId];
            return next;
          });
          setPendingUndo(null);
          return;
        }
        if (!workspaceId) return;
        if (stateDeletion) {
          const currentConversation = documentStore.current()?.workspaces
            .find((workspace) => workspace.id === workspaceId)?.conversations
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
          updateConversation(workspaceId, conversationId, () => restored);
          setContextUsage((current) => {
            const next = { ...current };
            delete next[conversationId];
            return next;
          });
          setPendingUndo(null);
          return;
        }
        updateConversation(workspaceId, conversationId, (conversation) => {
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

  /**
   * Images pasted into a user message on the timeline — one being edited, or
   * one being written from the context menu.
   *
   * The same gates the composer applies, because this is the same kind of
   * message arriving through a different box: vision, per-image size and
   * pixels, and how many one message may carry. Numbering happens here, against
   * the live transcript, so a pasted image is citable as `[Image #N]` the
   * moment the card is saved. Undefined when the conversation's model has no
   * image input, which is what withholds the paste instead of letting bytes
   * land where the model could never read them.
   */
  const pasteImagesIntoTimelineMessage = activeConversation
    && activeModelChoice
    && supportsVision(activeModelChoice.model)
    ? async (
      files: File[],
      existing: readonly ImageAttachment[]
    ): Promise<ImageAttachment[]> => acceptPastedImages(
      files,
      existing,
      reserveQueuedMessageIds(
        imageShortIdsInUse(activeConversation.contexts),
        activeConversation.queuedMessages
      )
    )
    : undefined;

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
        : editor.kind === "user"
          // A placed message carries what was pasted into it; every other kind
          // has nowhere to put an image and is never handed one.
          ? { ...base, kind: "user", ...(images?.length ? { images } : {}) }
          : { ...base, kind: editor.kind as "system" | "assistant" };
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

  /**
   * Writes hand-written arguments and result onto a card, recorded or brand new.
   * Nothing is executed, so the card would not be provable; the host issues its
   * attestation over what was typed and returns the exact bytes to store. A
   * placed card is how a call with side effects gets into the transcript without
   * causing them.
   */
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
      const conversationId = activeConversation.id;
      const contextId = createId("ctx");
      const toolName = editor.toolName;
      const index = editor.index;
      await flushLatestDocument();
      const attested = await attestInsertedToolContext({ conversationId, contextId, toolName, input, output });
      updateActiveConversation((conversation) => {
        const contexts = [...conversation.contexts];
        contexts.splice(Math.min(index, contexts.length), 0, {
          id: contextId,
          kind: "tool",
          toolName,
          input: attested.input,
          result: attested.result,
          attestation: attested.attestation,
          createdAt: new Date().toISOString()
        });
        return { ...conversation, contexts };
      });
      setContextUsage((current) => {
        const next = { ...current };
        delete next[conversationId];
        return next;
      });
      setEditor(null);
      return;
    }
    if (editor.item.kind !== "tool") {
      throw new Error(t("没有活动对话", "No active conversation"));
    }
    const conversationId = activeConversation.id;
    const contextId = editor.item.id;
    await flushLatestDocument();
    const attested = await attestEditedToolContext({
      conversationId,
      contextId,
      toolName: editor.item.toolName,
      input,
      output,
      images
    });
    updateActiveConversation((conversation) => ({
      ...conversation,
      contexts: conversation.contexts.map((item) => item.id === contextId && item.kind === "tool"
        ? {
          ...item,
          requestedInput: undefined,
          input: attested.input,
          result: attested.result,
          attestation: attested.attestation
        }
        : item)
    }));
    setContextUsage((current) => {
      const next = { ...current };
      delete next[conversationId];
      return next;
    });
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

  /** Pending answer to "this model cannot take the tools you just added". */
  const [toolExposurePrompt, setToolExposurePrompt] = useState<{
    conversationId: string;
    additions: ConversationToolLock;
  } | null>(null);
  /** Re-opens the composer's model menu after a dialog closes onto it. */
  const [modelMenuOpenSignal, setModelMenuOpenSignal] = useState(0);
  /** Merges a run's exposure into its conversation's lock. Tool surface only
   * ever widens, so this is the record the settings panel grays out against.
   * The composer's own send already folds this into its durable write; this
   * covers the runs that start without a new user message. */
  const lockConversationTools = useCallback(
    (workspaceId: string, conversationId: string, tools: string[]) => {
      const latest = documentStore.current();
      const current = findConversation(latest, workspaceId, conversationId).conversation;
      if (!current) return;
      /* The same question the composer asks before its own send: did this run
         put `web_fetch` in front of the model, or only `web_search`? */
      const exposure = {
        webFetch: latest ? grantsWebFetch(
          current.settings.webSearchEnabled === true,
          current.settings.webSearch,
          latest.globalSettings.webSearch,
          modelChoiceForConversation(latest).provider?.family
        ) : false
      };
      // Most runs add nothing, and `updateConversation` allocates a fresh
      // document snapshot even for an updater that changes nothing — which
      // would schedule a save per round for an unchanged lock.
      if (withRunToolLock(current.settings, tools, exposure) === current.settings) return;
      updateConversation(workspaceId, conversationId, (conversation) => ({
        ...conversation,
        settings: withRunToolLock(conversation.settings, tools, exposure)
      }));
    },
    [documentStore, modelChoiceForConversation, updateConversation]
  );
  /** What the exposure prompt is asking about, in the names the user picked them by. */
  const toolExposureAdditionLabels = useMemo(() => {
    if (!toolExposurePrompt) return [];
    const { additions } = toolExposurePrompt;
    const toolLabels = new Map(activeConversationTools.map((tool) => [tool.name, tool.label]));
    const mcpLabels = new Map((document?.capabilities.mcps ?? []).map((mcp) => [mcp.id, mcp.name]));
    return [
      ...additions.tools.map((name) => toolLabels.get(name) ?? name),
      ...additions.mcpIds.map((id) => mcpLabels.get(id) ?? id),
      ...(additions.globalMemory ? [t("全局记忆", "Global memory")] : []),
      ...(additions.projectMemory ? [t("项目记忆", "Project memory")] : []),
      ...(additions.skillTool ? [t("技能按需加载", "Load skills on demand")] : []),
      ...(additions.webSearch ? [t("联网搜索", "Web search")] : []),
      /* The fetch pin is the only record that `web_fetch` itself is new: web
         access is one switch, and a conversation can have been searching for
         rounds before a fetch backend resolved. */
      ...(additions.fetchProvider ? [t("抓取网页", "Web fetch")] : [])
    ];
  }, [activeConversationTools, document, t, toolExposurePrompt]);
  // The question belongs to one conversation's send. Leaving abandons it rather
  // than parking it to reappear on the way back.
  useEffect(() => {
    setToolExposurePrompt(null);
  }, [activeConversationId]);

  /** Refresh App facilities after each commit; the send pipeline reads them through `host()` at call time. */  const sendPipelineHostRef = useRef<SendPipelineHost | null>(null);
  useEffect(() => {
    sendPipelineHostRef.current = {
      t,
      openProviderSettings: () => openGlobalSettings("providers"),
      activeWorkspaceId: () => activeWorkspaceIdRef.current,
      activeConversationId: () => activeConversationIdRef.current,
      activeConversationTools: () => activeConversationTools,
      activeEnabledTools: () => activeEnabledTools,
      lockConversationTools,
      promptToolExposureChange: (conversationId, additions) => setToolExposurePrompt({
        conversationId,
        additions
      }),
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
    removeComposerImage,
    dispatchNextQueuedMessage,
    retryFailedQueuedPromotion
  } = sendPipeline;

  /** Materialize a draft synchronously before `sendComposer`, which requires both active ids in the document. Do not materialize while images are uploading, because send rejection must not leave an empty conversation. */
  const sendActiveComposer = useCallback(async (
    overrideText?: string,
    toolExposure?: ToolExposureMode
  ) => {
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
    await sendComposer(overrideText, toolExposure);
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
        await stopPreviewServer(item.server.serverId);
        // The host's own change event closes the page too, but not before this row has already
        // gone; closing here is what makes one click read as one action.
        await closePagesServedAt(conversationId, item.address);
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

  if (loadError) return <ErrorView message={loadError} onReset={async () => { const next = await resetDocument(); documentStore.load(next); setLoadError(null); }} />;
  if (!document) {
    return (
      <div className="app-loading">
        <MeworkIcon size={38} /><p>{t("正在打开 Mework…", "Opening Mework…")}</p>
      </div>
    );
  }

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
      const terminals = terminalTabsFor(terminalTabsState, conversationId);
      // The reference shell names a lone terminal after the thing itself and numbers them only
      // once there is more than one to tell apart. The number is the tab's place in the strip,
      // so a closed terminal gives its number back to the next one.
      // A tab opened for a particular shell is named after it, the way the reference shell names
      // its tabs after the program running in them.
      const tabLabel = (tab: TerminalTab) => {
        if (tab.name) return tab.name;
        const shell = tab.launch?.shell;
        if (shell) {
          return terminals.tabs.length > 1
            ? `${terminalShellLabel(shell)} ${terminalDisplayNumber(terminals, tab.id)}`
            : terminalShellLabel(shell);
        }
        return terminals.tabs.length > 1
          ? t("终端 {n}", "Terminal {n}", { n: terminalDisplayNumber(terminals, tab.id) })
          : t("终端", "Terminal");
      };
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
              tabs={terminals.tabs.map((tab) => ({ id: tab.id, label: tabLabel(tab) }))}
              activeId={terminals.activeId}
              panelId={(terminalId) => terminalPanelId(conversationId, terminalId)}
              closingIds={new Set(terminals.tabs
                .filter((tab) => (
                  terminalSessions[terminalSessionKey(conversationId, tab.id)]?.phase === "closing"
                ))
                .map((tab) => tab.id))}
              onSelect={(terminalId) => dispatchTerminalTabs({
                type: "activate", conversationId, terminalId
              })}
              onClose={(terminalId) => closeTerminalTab(conversationId, terminalId)}
              onRename={(terminalId, name) => {
                const tab = terminals.tabs.find((candidate) => candidate.id === terminalId);
                // Submitting the name the terminal already shows is not a rename: it keeps
                // following the derived one, so it still loses its number when the others go.
                dispatchTerminalTabs({
                  type: "rename",
                  conversationId,
                  terminalId,
                  name: tab && name.trim() === tabLabel(tab) ? "" : name
                });
              }}
              // Another terminal like the one in front: same workspace, same shell.
              onAdd={() => dispatchTerminalTabs({
                type: "add",
                conversationId,
                launch: terminals.tabs.find((tab) => tab.id === terminals.activeId)?.launch ?? null
              })}
            />
          )}
        >
          {terminals.tabs.map((tab) => (
            <TerminalPanel
              key={tab.id}
              ref={(handle) => {
                const key = terminalSessionKey(conversationId, tab.id);
                if (handle) terminalPanelHandlesRef.current.set(key, handle);
                else terminalPanelHandlesRef.current.delete(key);
              }}
              conversationId={conversationId}
              terminalId={tab.id}
              label={tabLabel(tab)}
              launch={tab.launch ?? undefined}
              // Every tab stays mounted; only the selected one is on screen. A hidden tab keeps
              // its shell, its scrollback and its box, so coming back to it is a repaint.
              open={terminalPaneOpen && tab.id === terminals.activeId}
              initialState={terminalSessions[terminalSessionKey(conversationId, tab.id)]}
              inputDisabledReason={terminalInputDisabledReason}
              onCommandStart={() => beginTerminalCommand(conversationId, tab.id)}
              onStateChange={updateTerminalSession}
              onClose={async () => {
                await requestTerminalSessionClose(conversationId, tab.id);
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
      const automationHolds = isPrimaryPreviewSession(target, conversationId)
        && (Boolean(activePreviewPageTool) || activeBrowserAutomationStopping);
      return (
        <BrowserPanel
          paneId={pane}
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
          // Covered by another pane's expand, this pane keeps a full-size box it is no longer
          // allowed to draw in — the tile is only made `visibility:hidden`, so its rectangle still
          // measures. The host parks the page for the same reason, and the panel has to agree, or
          // it goes on reporting the page as covered and swapping in snapshots nobody can see.
          active={activeExpandedPane === null || expanded}
          target={activePrimaryGitTarget}
          onReservedBottomChange={(reservedBottom) => reservePreviewBottom(target, reservedBottom)}
        />
      );
    }

    if (kind === "review") {
      if (!activeWorkspace || !activeGitSnapshot || !activeGitTarget) return null;
      const workspace = activeWorkspace;
      const snapshot = activeGitSnapshot;
      return (
        <GitReviewPanel
          key={gitReviewSnapshotCacheKey(snapshot)}
          paneId={pane}
          target={activeGitTarget}
          snapshot={snapshot}
          active
          mutationDisabledReason={gitMutationDisabledReason}
          paneExpanded={expanded}
          onPaneToggleExpand={onToggleExpand}
          onPaneFocus={onFocus}
          onPaneClose={() => closePane(pane)}
          onSnapshotChange={(next) => updateGitSnapshots((current) => (
            gitSnapshotsAfterWorkspaceMutation(
              current,
              // Broadcast Git snapshots only to conversations using the same checkout. Drafts use the workspace root; worktree conversations do not.
              // A worktree only ever stands in for the project's first workspace, so every
              // conversation of the project shares the checkout of any other one.
              gitSnapshotBroadcastIds(
                workspace.conversations.map((conversation) => (
                  activeWorkspaceMember === 1 ? conversation : { ...conversation, worktree: null }
                )),
                conversationId,
                activeWorkspaceMember === 1 && Boolean(activeWorktree)
              ),
              activeGitSurfaceKey ?? workspace.id,
              next
            )
          ))}
          onMutationStart={() => beginGitMutation(conversationId)}
          onMutationEnd={() => endGitMutation(conversationId)}
        />
      );
    }

    if (kind === "files") {
      return (
        <FilesPane
          key={conversationId}
          paneId={pane}
          target={activePrimaryGitTarget ?? { kind: "conversation", conversationId }}
          rootLabel={activeWorkspace?.name ?? t("工作区", "Workspace")}
          workspacePath={filesPaneRoot}
          active
          openRequest={filesPaneRequest?.conversationId === conversationId ? filesPaneRequest : null}
          expanded={expanded}
          onToggleExpand={onToggleExpand}
          onPaneFocus={onFocus}
          onPaneClose={() => closePane(pane)}
        />
      );
    }

    if (kind === "tasks") {
      return (
        <SidePane
          id={pane}
          title={t("任务", "Tasks")}
          onFocus={onFocus}
          expanded={expanded}
          onToggleExpand={onToggleExpand}
          onClose={() => closePane(pane)}
        >
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
            status={agentStatus}
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
        </SidePane>
      );
    }

    if (kind === "history") {
      // Targeted means one agent's ledger; bare means the conversation's own.
      // They read the same rows from the same store and differ only in which
      // ledger they ask for, so one pane draws both.
      const view = target ? findOpenableSubagentView(subagents, target) : null;
      if (target && !view) return null;
      return (
        <SidePane
          id={pane}
          title={view
            ? t("{label} 发出的请求", "{label} · outgoing requests", { label: view.label })
            : t("发出的请求", "Outgoing requests")}
          onFocus={onFocus}
          expanded={expanded}
          onToggleExpand={onToggleExpand}
          onClose={() => closePane(pane)}
        >
          <HistoryPane
            conversationId={conversationId}
            contexts={conversation.contexts}
            streaming={activeModelRunBusy}
            /* A spawned agent's ledger is keyed by its name; a workflow step's
               by the run-scoped address the host publishes on its shell. A view
               that carries neither — a step shell written before the address
               rode along — asks for nothing rather than falling back to the
               conversation's own rows. */
            owners={view ? (view.ledgerOwner ? [view.ledgerOwner] : []) : undefined}
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
      const view = target ? findOpenableSubagentView(subagents, target) : null;
      if (!view || !target) return null;
      const ledgerPane = subagentHistoryPaneId(view.id);
      return (
        <SidePane
          id={pane}
          title={view.label}
          onFocus={onFocus}
          expanded={expanded}
          onToggleExpand={onToggleExpand}
          onClose={() => closePane(pane)}
          /* What this agent put on the wire, beside the transcript it settled
             into. The two are not the same account: the transcript is what the
             child kept, the ledger is every payload it sent. */
          trailing={(
            <IconButton
              className="side-pane__ledger"
              label={t("{label} 发出的请求", "{label} · outgoing requests", { label: view.label })}
              aria-pressed={paneIsOpen(activeLayout, ledgerPane)}
              onMouseDown={(event) => event.preventDefault()}
              onClick={() => togglePane(ledgerPane)}
            >
              <History size={13} aria-hidden="true" />
            </IconButton>
          )}
        >
          <StreamedSubagentPanel
            conversation={conversation}
            modelRunController={modelRunController}
            externalStepBodies={externalStepBodies}
            selectedSubagentId={target}
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
            onSaveAsPreset={() => setSaveAsPresetDialog({ name: "", description: "" })}
          />
        </SidePane>
      );
    }

    const shellTask = target ? activeShellTasks.find((task) => task.shellTaskId === target) : null;
    if (!shellTask) return null;
    return (
      <SidePane
        id={pane}
        title={shellTask.toolName}
        onFocus={onFocus}
        expanded={expanded}
        onToggleExpand={onToggleExpand}
        onClose={() => closePane(pane)}
      >
        <ShellTaskPanel
          task={shellTask}
          open
          stopping={stoppingTaskIds.includes(JSON.stringify([conversationId, shellTask.shellTaskId]))}
          onStop={() => void stopShellTask(shellTask.conversationId, shellTask.shellTaskId)}
        />
      </SidePane>
    );
  };

  return (
    <CommonErrorBoundary>
      <div
        className={`app-shell ${sidebarOpen ? "" : "app-shell--sidebar-closed"} ${sidebarResizing || paneResizing ? "app-shell--resizing" : ""}`}
        style={{
          "--sidebar-width": `${sidebarWidth}px`
        } as AppShellStyle}
      >
        <Sidebar
            workspaces={document.workspaces}
            sshMachines={document.globalSettings.executionEnvironments.sshMachines}
            activeWorkspaceId={activeWorkspaceId}
            activeConversationId={activeConversationId}
            onSelectConversation={selectConversation}
            onNewConversation={openDraftConversation}
            onAddWorkspace={() => { setAssignWorkspaceAfterAdd(false); setWorkspaceDialogOpen(true); }}
            onEditProject={(workspaceId) => setProjectEditor(workspaceId)}
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
            onOpenSettings={() => openGlobalSettings("providers")}
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
                      {activeWorkspace?.name ?? t("未选择项目", "No project selected")}
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
                <PaneToolbar
                  buttons={[
                    {
                      id: "terminal",
                      label: t("终端", "Terminal"),
                      activeLabel: t("终端（命令运行中）", "Terminal (command running)"),
                      icon: <SquareTerminal size={18} aria-hidden="true" />,
                      pressed: terminalPaneOpen,
                      activity: Object.values(terminalSessions).some((session) => (
                        session.conversationId === activeConversation.id && session.busy
                      )),
                      // A draft has no host conversation yet, so there is nothing to hang a PTY on.
                      disabled: draftActive,
                      title: draftActive
                        ? t("先发送一条消息再打开", "Send a message first")
                        : undefined,
                      onToggle: () => togglePane("terminal"),
                      // The same choice as the composer's terminal button: a new shell in the
                      // selected workspace. Showing or hiding the terminals already open is the
                      // row after them.
                      menu: {
                        label: t("新建终端", "New terminal"),
                        sections: [
                          {
                            id: "shells",
                            label: activeSelectedWorkspace
                              ? t("在 {name} 打开", "Open in {name}", {
                                name: workspaceDirectoryLabel(activeSelectedWorkspace.path)
                              })
                              : undefined,
                            items: terminalShellMenuItems(
                              activeTerminalShells,
                              openTerminalInSelectedWorkspace,
                              activeWorkspaceLifecycleOperationRunning
                            )
                          },
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
                      activity: Boolean(activeGitSnapshot && activeGitSnapshot.files.length > 0),
                      // Without a snapshot and a target there is no repository to review.
                      disabled: !(activeGitSnapshot && activeGitTarget),
                      title: activeSelectedWorkspaceIsRemote
                        ? t("工作区在另一台机器上，本机的 Git 面板不可用", "This workspace is on another machine; the host's Git pane is unavailable")
                        : !(activeGitSnapshot && activeGitTarget)
                          ? t("当前工作区不是 Git 仓库", "This workspace is not a Git repository")
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
                      disabled: draftActive,
                      title: draftActive
                        ? t("先发送一条消息再打开", "Send a message first")
                        : undefined,
                      onToggle: () => (openPreviewSessionId
                        ? closePane(previewPaneId(openPreviewSessionId))
                        : void openBrowserTab(currentPreviewSessions.at(0)))
                    }
                  ]}
                  menuItems={[
                    {
                      id: "files",
                      label: t("文件", "Files"),
                      icon: <Folder size={14} aria-hidden="true" />,
                      checked: paneIsOpen(activeLayout, "files"),
                      disabled: draftActive || activeWorkspaceIsRemote,
                      title: activeWorkspaceIsRemote
                        ? t("工作区在另一台机器上，本机的文件面板不可用", "This workspace is on another machine; the host's file pane is unavailable")
                        : undefined,
                      onSelect: () => togglePane("files")
                    },
                    {
                      id: "tasks",
                      label: t("任务", "Tasks"),
                      icon: <ListChecks size={14} aria-hidden="true" />,
                      checked: paneIsOpen(activeLayout, "tasks"),
                      disabled: draftActive,
                      onSelect: () => togglePane("tasks")
                    },
                    {
                      id: "history",
                      label: t("发出的请求", "Outgoing requests"),
                      icon: <History size={14} aria-hidden="true" />,
                      checked: paneIsOpen(activeLayout, "history"),
                      disabled: draftActive,
                      onSelect: () => togglePane("history")
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
              sideFlex={activeLayout.sideFlex}
              paneFlex={activeLayout.paneFlex}
              expanded={activeExpandedPane}
              onSideFlexChange={(sideFlex) => dispatchSidePanes({
                type: "set_side_flex",
                conversationId: activeConversation.id,
                sideFlex
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
                ...(!draftActive && !terminalPaneOpen
                  ? [{ id: "terminal" as SidePaneId, node: renderPane("terminal"), hidden: true }]
                  : [])
              ]}
              chat={(
            <div className="conversation-pane">
              <StreamedConversationView
                className="conversation-pane__main"
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
                pendingQuestionId={pendingQuestion?.context.id ?? null}
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
                onPasteImages={pasteImagesIntoTimelineMessage}
                onSaveTool={saveToolContext}
                onSaveToolEdit={saveToolContextEdit}
                onSaveQuestion={saveQuestionContext}
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
                  pending={pendingQuestion}
                  disabled={activeWorkspaceLifecycleOperationRunning}
                  editDisabled={activeTimelineMutationBlocked}
                  onAnswer={answerPendingQuestion}
                  onEditQuestion={(item) => handleQuestionEdit(item)}
                  onDismissQuestion={(item) => deleteQuestionContext(item)}
                />
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
                {/* Project, workspace, and branch describe where the conversation runs, so they sit above the composer rather than inside it. */}
                <div className="composer-context">
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
                    {activeProjectWorkspaces.length > 1 && (
                      <WorkspaceMemberSelector
                        workspaces={activeProjectWorkspaces}
                        selected={activeWorkspaceMember}
                        sshMachines={document.globalSettings.executionEnvironments.sshMachines}
                        onSelect={(member) => setSelectedWorkspaceMembers((current) => ({
                          ...current,
                          [activeConversation.id]: member
                        }))}
                      />
                    )}
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
                        {activeWorkspaceMember === 1 && <>
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
                          aria-label={t("移除工作区：{path}", "Remove workspace: {path}", { path: workspace.path })}
                          disabled={activeModelRunning}
                          onClick={() => detachWorkspace(workspace)}
                        >
                          <X size={11} />
                        </button>
                      </span>
                    ))}
                    {/* Opens a shell in the workspace selected to the left, so it sits with the
                        chips that say where things run. Drafts have no host conversation to hang a
                        PTY on until the first message materializes one. */}
                    <TerminalShellButton
                      workspaceLabel={activeSelectedWorkspace
                        ? workspaceDirectoryLabel(activeSelectedWorkspace.path)
                        : t("工作区", "the workspace")}
                      shells={activeTerminalShells}
                      disabled={draftActive || activeWorkspaceLifecycleOperationRunning}
                      disabledReason={draftActive
                        ? t("先发送一条消息再打开终端", "Send a message before opening a terminal")
                        : undefined}
                      onSelect={(shell) => openTerminalInSelectedWorkspace(shell)}
                    />
                    {hasNativeWorkspacePicker() && (
                      <PopoverMenu
                        triggerClassName="composer-chip composer-chip--icon"
                        trigger={<FolderPlus size={13} />}
                        triggerLabel={t("附加工作区", "Attach a workspace")}
                        disabled={activeModelRunning}
                        menuLabel={t("在哪台机器上选目录", "Which machine to pick a directory on")}
                        menuWidth={244}
                        emptyLabel={t("没有可选的机器", "No machines to choose from")}
                        onOpen={loadMachineMenu}
                        sections={[{
                          id: "machines",
                          items: [
                            {
                              id: "local",
                              label: t("本机", "This machine"),
                              icon: <Monitor size={14} />,
                              onSelect: () => void attachLocalWorkspace()
                            },
                            ...(machineMenuDistros ?? []).map((distro) => ({
                              id: `wsl:${distro.name}`,
                              label: distro.name,
                              icon: <SquareTerminal size={14} />,
                              onSelect: () => setRemoteWorkspacePicker({
                                machine: { kind: "wsl", distro: distro.name },
                                name: distro.name,
                                purpose: "attach"
                              })
                            })),
                            ...document.globalSettings.executionEnvironments.sshMachines.map((machine) => ({
                              id: `ssh:${machine.id}`,
                              label: machine.name,
                              icon: <Server size={14} />,
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
                        gitOpen={gitReviewPanelOpen}
                        onOpenGitReview={openGitReview}
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
                  {composerIsUnsentTask && <ComposerCat />}
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
                  <ImageStrip
                    images={activeComposerVisibleImages}
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
                      disabled={Boolean(modelRunSummaries[activeConversation.id]) || activeWorkspaceLifecycleOperationRunning}
                      menuLabel={t("安全层级", "Security level")}
                      menuWidth={256}
                      sections={[{
                        id: "security",
                        items: SECURITY_LEVEL_OPTIONS.map((option) => ({
                          id: option,
                          label: securityLevelLabelFor(option),
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
                        aria-expanded={terminalPaneOpen}
                        aria-controls={sidePaneDomId("terminal")}
                        disabled={activeWorkspaceLifecycleOperationRunning}
                        onClick={() => togglePane("terminal")}
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
                      dense
                      searchPlaceholder={t("搜索模型…", "Search models…")}
                      emptyLabel={enabledModelChoices.length
                        ? t("没有匹配的模型", "No matching models")
                        : t("没有已启用的模型", "No enabled models")}
                      sections={enabledModelSections}
                      openSignal={modelMenuOpenSignal}
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

        {toolExposurePrompt && activeConversation
          && toolExposurePrompt.conversationId === activeConversation.id && (
          <Dialog
            title={t("这个模型接不住中途新增的工具", "This model will not take tools added mid-conversation")}
            description={t(
              "{name} 用的协议，在一段对话开始之后就不再接受更宽的工具集。",
              "The protocol {name} speaks will not accept a wider tool set once a conversation has started.",
              { name: activeModelLabel }
            )}
            onClose={() => setToolExposurePrompt(null)}
            width="460px"
            footer={(
              <>
                <button
                  type="button"
                  className="button"
                  onClick={() => {
                    setToolExposurePrompt(null);
                    openConversationSettings();
                  }}
                >{t("打开对话设置", "Open conversation settings")}</button>
                <button
                  type="button"
                  className="button"
                  onClick={() => {
                    setToolExposurePrompt(null);
                    setModelMenuOpenSignal((current) => current + 1);
                  }}
                >{t("换个模型", "Switch models")}</button>
                <button
                  type="button"
                  className="button button--primary"
                  onClick={() => {
                    setToolExposurePrompt(null);
                    saveActiveConversationComposition(
                      settingsAtToolLockFloor(activeConversation.settings)
                    );
                    void sendActiveComposer(undefined, "locked");
                  }}
                >{t("关掉再发送", "Turn them off and send")}</button>
              </>
            )}
          >
            <p className="confirm-copy">{t(
              "上一轮之后新开的这些会被关掉，对话回到模型已经拿到的那套工具。消息还没有发出去。",
              "What was opened since the last round will be turned back off, returning this conversation to the tool set the model already has. Nothing has been sent yet."
            )}</p>
            {toolExposureAdditionLabels.length > 0 && (
              <ul className="confirm-list">
                {toolExposureAdditionLabels.map((label) => <li key={label}>{label}</li>)}
              </ul>
            )}
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

        {workspaceDialogOpen && <ProjectDialog
          mode="create"
          sshMachines={document.globalSettings.executionEnvironments.sshMachines}
          envVars={document.globalSettings.executionEnvironments.envVars}
          showWsl={hostIsWindows(platform)}
          nativePicker={hasNativeWorkspacePicker()}
          onPickLocalDirectory={pickWorkspaceDirectory}
          onSaveSshMachine={(machine, vars) => {
            saveSshMachine(machine);
            saveRunEnvironmentVars(`ssh:${machine.id}`, vars);
          }}
          onClose={() => { setWorkspaceDialogOpen(false); setAssignWorkspaceAfterAdd(false); }}
          onSubmit={(name, workspaces) => void addWorkspace(name, workspaces)}
        />}

        {editedProject && <ProjectDialog
          mode="edit"
          initialName={editedProject.name}
          initialWorkspaces={projectWorkspaces(editedProject)}
          sshMachines={document.globalSettings.executionEnvironments.sshMachines}
          envVars={document.globalSettings.executionEnvironments.envVars}
          showWsl={hostIsWindows(platform)}
          nativePicker={hasNativeWorkspacePicker()}
          onPickLocalDirectory={pickWorkspaceDirectory}
          onSaveSshMachine={(machine, vars) => {
            saveSshMachine(machine);
            saveRunEnvironmentVars(`ssh:${machine.id}`, vars);
          }}
          onClose={() => setProjectEditor(null)}
          onSubmit={(name, workspaces) => updateProject(editedProject.id, name, workspaces)}
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
      </div>
    </CommonErrorBoundary>
  );
}

export default App;
