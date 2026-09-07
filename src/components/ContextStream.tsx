import {
  Bot,
  BrainCircuit,
  Check,
  ChevronLeft,
  ChevronRight,
  CircleAlert,
  Copy,
  FileJson,
  GitBranch,
  MessageSquare,
  Pencil,
  Plus,
  RotateCcw,
  Shield,
  Trash2,
  UserRound,
  Workflow,
  Wrench,
  X
} from "lucide-react";
import { Fragment, memo, useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { ReactNode } from "react";
import { useI18n } from "../i18n";
import type { ContextItem, InsertableContextKind, ToolContext, ToolDescriptor, UserContext } from "../types";
import type { ContextBranchNavigation } from "../lib/conversationBranches";
import { turnAnchorIndex } from "../lib/conversationTurns";
import type { ConversationTurn } from "../lib/conversationTurns";
import type { WorkflowRunView } from "../lib/workflowRuns";
import type { LiveReasoningView } from "../lib/runContexts";
import { EmptyState, IconButton } from "./Common";
import { buildContextRenderNodes, ToolCallGroup } from "./ToolCallGroup";
import type { ContextRenderNode } from "./ToolCallGroup";
import { WorkflowRunCard } from "./WorkflowRunCard";
import { ConversationTurnDisclosure } from "./ConversationTurnDisclosure";
import { ReasoningContent } from "./ReasoningContent";
import { isEncryptedReasoning } from "../lib/modelCapabilities";
import { useAppearance } from "../lib/appearance";
import { MarkdownContent } from "./MarkdownContent";
import { QuestionTimelineCard } from "./QuestionTimelineCard";
import { StreamWaitingIndicator } from "./StreamWaitingIndicator";
import { ImageStrip } from "./ImageStrip";

export interface ContextStreamProps {
  contexts: ContextItem[];
  /** Frontend-only presentation spans; never used to assemble model context. */
  turns?: ConversationTurn[];
  onToggleTurn?: (turnId: string) => void;
  tools: ToolDescriptor[];
  enabledTools: string[];
  /** Distinguishes wholesale timeline switches from appended output. */
  timelineId?: string;
  /** Keeps the main message renderer while removing all timeline mutation affordances. */
  readOnly?: boolean;
  /** Temporarily locks timeline edits while preserving main-surface controls such as Run and branch navigation. */
  timelineMutationLocked?: boolean;
  /** True for the full model round, including the gaps before the first and between provider stream events. */
  streaming?: boolean;
  /**
   * Reasoning the live round is doing right now, narrated beside the cat.
   * Encrypted reasoning with no summary has no card while it streams, so this is
   * the only surface it gets.
   */
  thinking?: LiveReasoningView | null;
  /** Live "request failed, retrying" notice for the streaming round. A
   * transient hint only — never part of the timeline contexts. */
  retryNotice?: { attempt: number; maxAttempts: number; message: string } | null;
  ariaLabel?: string;
  /** The `ask_user` tool context currently awaiting an answer, if any. */
  pendingQuestionId?: string | null;
  /**
   * Content to replace the default empty state. The main conversation uses a usage card; read-only
   * subagent transcripts retain the default because global usage is not meaningful there.
   */
  emptyState?: ReactNode;
  /**
   * Whether an empty timeline is allowed to show `emptyState` at all. The landing
   * card belongs to arriving at an empty conversation; once this visit has had
   * content, emptying it again leaves a bare timeline rather than snapping back.
   */
  emptyStateVisible?: boolean;
  onEdit?: (item: ContextItem) => void;
  onDelete?: (item: ContextItem) => void;
  /** Edits the projected ask_user tool and its paired answer as one message. */
  onEditQuestion?: (item: ToolContext, answer?: UserContext) => void;
  /** Deletes the projected ask_user tool and its paired answer as one message. */
  onDeleteQuestion?: (item: ToolContext, answer?: UserContext) => void;
  /** Opens a new conversation drafted with this existing user message. */
  onBranchFrom?: (item: ContextItem) => void;
  branchFromDisabledReason?: string | null;
  branchNavigations?: Record<string, ContextBranchNavigation>;
  onSelectBranch?: (forkContextId: string, branchId: string) => void;
  branchSwitchDisabledReason?: string | null;
  onInsert?: (index: number, kind: InsertableContextKind) => void;
  /** Opens the read-only child conversation for a subagent tool call. */
  onOpenSubagent?: (subagentId: string) => void;
  /**
   * Workflow runs, keyed by the owning `workflow` tool call id. A call with no
   * entry renders no card: an empty card would claim a zero-step run, which is
   * indistinguishable on screen from a run whose steps were never received.
   */
  workflowRunByCall?: Record<string, WorkflowRunView>;
  /** Brings a run's panel forward in the task container. */
  onOpenWorkflowRun?: (runId: string) => void;
  /**
   * Retries the failed run whose notice a turn is currently showing. Only offered
   * while the renderer still holds the failed request, so a reloaded transcript
   * shows the notice without a dead button.
   */
  onRetryTurnError?: () => void;
  /** Request id the live retry would resend; a turn only offers Retry when it matches. */
  retryableTurnRequestId?: string | null;
  /** Retracts the failure notice without sending anything. */
  onDismissTurnError?: () => void;
  /**
   * Working directory that relative paths in model output resolve against.
   * Absolute paths are clickable without it.
   */
  pathBaseDir?: string | null;
}

/**
 * A run failure, read where it happened. It is a rendered notice rather than a
 * context: it is never persisted into the conversation and never reaches the
 * model, so the timeline explains the gap without inventing history.
 */
function TurnErrorNotice({ error, onRetry, onDismiss }: {
  error: NonNullable<ConversationTurn["error"]>;
  onRetry?: () => void;
  onDismiss?: () => void;
}) {
  const { t } = useI18n();
  const origin = [error.providerName, error.modelName].filter(Boolean).join(" · ");
  return (
    <div className="turn-error" role="alert">
      <CircleAlert size={15} aria-hidden="true" />
      <span>
        <strong>{origin || t("模型请求失败", "The model request failed")}</strong>
        <small>{error.message}</small>
      </span>
      {onRetry && (
        <button type="button" onClick={onRetry}>
          {t("重试", "Retry")}
        </button>
      )}
      {onDismiss && (
        <IconButton label={t("关闭模型错误", "Dismiss model error")} onClick={onDismiss}>
          <X size={13} />
        </IconButton>
      )}
    </div>
  );
}

const contextMeta: Record<InsertableContextKind, {
  label: (t: ReturnType<typeof useI18n>["t"]) => string;
  icon: typeof Shield;
  description: (t: ReturnType<typeof useI18n>["t"]) => string;
}> = {
  system: { label: (t) => t("系统提示词", "System prompt"), icon: Shield, description: (t) => t("为这一位置注入额外系统指令", "Inject an additional system instruction at this point") },
  user: { label: (t) => t("用户输入", "User input"), icon: UserRound, description: (t) => t("插入一条用户消息", "Insert a user message") },
  reasoning: { label: (t) => t("思考字段", "Reasoning field"), icon: BrainCircuit, description: (t) => t("插入可编辑的明文思考", "Insert editable plain-text reasoning") },
  tool: { label: (t) => t("工具调用", "Tool call"), icon: Wrench, description: (t) => t("选择工具，只填写调用参数", "Choose a tool and fill in only its arguments") },
  assistant: { label: (t) => t("模型回复", "Model reply"), icon: Bot, description: (t) => t("插入一条模型回复", "Insert a model reply") }
};

function formatTime(iso: string, locale: string): string {
  return new Intl.DateTimeFormat(locale, { hour: "2-digit", minute: "2-digit" }).format(new Date(iso));
}

function contextVisualLength(item: ContextItem | undefined): number {
  if (!item) return 0;
  if (item.kind === "tool") return item.result.output.length + (item.result.diff?.length ?? 0) + (item.result.images?.length ?? 0) * 200;
  return (item.content?.length ?? 0) + (item.kind === "user" ? (item.images?.length ?? 0) * 200 : 0);
}

/**
 * A reasoning card that exists only because the round announced it: encrypted,
 * still streaming, and with nothing in it. The renderer withholds it rather than
 * drawing an empty disclosure — a subagent transcript is the surface that can
 * still produce one, because its cards carry no resolved `form` and the run
 * projection's own gate never sees them.
 */
function isLiveEncryptedReasoning(item: ContextItem): boolean {
  return item.kind === "reasoning"
    && item.streaming === true
    && (item.content ?? "").length === 0
    && isEncryptedReasoning(item);
}

function ContextActions({
  item,
  onEdit,
  onDelete,
  allowEdit = true,
  disabled = false
}: {
  item: ContextItem;
  onEdit: () => void;
  onDelete: () => void;
  allowEdit?: boolean;
  disabled?: boolean;
}) {
  const { t } = useI18n();
  return (
    <div className="context-actions">
      {allowEdit && (
        <IconButton label={t("编辑上下文", "Edit context")} onClick={onEdit} disabled={disabled}>
          <Pencil size={14} />
        </IconButton>
      )}
      <IconButton label={t("删除上下文", "Delete context")} onClick={onDelete} disabled={disabled}>
        <Trash2 size={14} />
      </IconButton>
    </div>
  );
}

function UserMessageToolbar({
  content,
  navigation,
  editable,
  mutationDisabled,
  branchFromDisabledReason,
  branchDisabledReason,
  onEdit,
  onDelete,
  onBranch,
  onSelectBranch
}: {
  content: string;
  navigation?: ContextBranchNavigation;
  editable: boolean;
  mutationDisabled: boolean;
  branchFromDisabledReason?: string | null;
  branchDisabledReason?: string | null;
  onEdit: () => void;
  onDelete: () => void;
  onBranch?: () => void;
  onSelectBranch?: (branchId: string) => void;
}) {
  const { t } = useI18n();
  const [copyStatus, setCopyStatus] = useState<"idle" | "success" | "error">("idle");
  const copyResetTimer = useRef<number | null>(null);
  const previousId = navigation?.branchIds[navigation.activeIndex - 1];
  const nextId = navigation?.branchIds[navigation.activeIndex + 1];
  const branchDisabled = Boolean(branchDisabledReason);

  useEffect(() => () => {
    if (copyResetTimer.current !== null) window.clearTimeout(copyResetTimer.current);
  }, []);

  const copyMessage = async () => {
    let copied = false;
    try {
      await navigator.clipboard.writeText(content);
      copied = true;
    } catch {
      const textarea = document.createElement("textarea");
      textarea.value = content;
      textarea.setAttribute("readonly", "");
      textarea.style.position = "fixed";
      textarea.style.opacity = "0";
      document.body.appendChild(textarea);
      textarea.select();
      try {
        copied = typeof document.execCommand === "function" && document.execCommand("copy");
      } catch {
        copied = false;
      }
      textarea.remove();
    }

    setCopyStatus(copied ? "success" : "error");
    if (copyResetTimer.current !== null) window.clearTimeout(copyResetTimer.current);
    copyResetTimer.current = window.setTimeout(() => setCopyStatus("idle"), 1600);
  };

  const copyLabel = copyStatus === "success" ? t("已复制", "Copied") : copyStatus === "error" ? t("复制失败", "Copy failed") : t("复制用户消息", "Copy user message");
  return (
    <div className="user-message-toolbar">
      {content.length > 0 && (
        <button type="button" className="user-message-toolbar__action user-message-toolbar__copy" aria-label={copyLabel} aria-live="polite" title={copyLabel} onClick={copyMessage}>
          {copyStatus === "success" ? <Check size={12} /> : <Copy size={12} />}
        </button>
      )}
      {editable && (
        <>
          <button
            type="button"
            className="user-message-toolbar__action user-message-toolbar__edit"
            aria-label={t("编辑上下文", "Edit context")}
            title={t("编辑上下文", "Edit context")}
            disabled={mutationDisabled}
            onClick={onEdit}
          >
            <Pencil size={12} />
          </button>
          <button
            type="button"
            className="user-message-toolbar__action user-message-toolbar__delete"
            aria-label={t("删除上下文", "Delete context")}
            title={t("删除上下文", "Delete context")}
            disabled={mutationDisabled}
            onClick={onDelete}
          >
            <Trash2 size={12} />
          </button>
        </>
      )}
      {onBranch && (
        <button
          type="button"
          className="user-message-toolbar__action user-message-toolbar__branch"
          aria-label={t("从此消息分支", "Branch from this message")}
          title={branchFromDisabledReason ?? t("在新对话中继续这条消息", "Continue this message in a new conversation")}
          disabled={Boolean(branchFromDisabledReason)}
          onClick={onBranch}
        >
          <GitBranch size={12} />
        </button>
      )}
      {navigation && (
        <div className="user-message-toolbar__branches" role="group" aria-label={t("此消息的分支", "Branches from this message")}>
          <IconButton
            label={t("上一个分支", "Previous branch")}
            title={branchDisabledReason ?? t("上一个分支", "Previous branch")}
            disabled={branchDisabled || !previousId}
            onClick={() => previousId && onSelectBranch?.(previousId)}
          >
            <ChevronLeft size={13} />
          </IconButton>
          <span aria-live="polite">{navigation.activeIndex + 1} / {navigation.branchIds.length}</span>
          <IconButton
            label={t("下一个分支", "Next branch")}
            title={branchDisabledReason ?? t("下一个分支", "Next branch")}
            disabled={branchDisabled || !nextId}
            onClick={() => nextId && onSelectBranch?.(nextId)}
          >
            <ChevronRight size={13} />
          </IconButton>
        </div>
      )}
    </div>
  );
}

/**
 * Memoize cards so a stream flush rerenders only cards whose identity changed. Stable callbacks
 * and persisted `item` references preserve that boundary between flushes.
 */
const ContextCard = memo(function ContextCard({
  item,
  index,
  readOnly,
  mutationReadOnly,
  onEditItem,
  onDeleteItem,
  onBranchFromItem,
  branchFromDisabledReason,
  branchNavigation,
  onSelectBranchFor,
  branchSwitchDisabledReason,
  deferOffscreen,
  pathBaseDir,
  onOpenInsertAt
}: {
  item: Exclude<ContextItem, ToolContext>;
  index: number;
  readOnly: boolean;
  mutationReadOnly: boolean;
  onEditItem?: (item: ContextItem) => void;
  onDeleteItem?: (item: ContextItem) => void;
  onBranchFromItem?: (item: ContextItem) => void;
  branchFromDisabledReason?: string | null;
  branchNavigation?: ContextBranchNavigation;
  onSelectBranchFor?: (forkContextId: string, branchId: string) => void;
  branchSwitchDisabledReason?: string | null;
  deferOffscreen: boolean;
  pathBaseDir: string | null;
  onOpenInsertAt: (event: React.MouseEvent | React.KeyboardEvent, index: number) => void;
}) {
  const { resolvedLanguage, t } = useI18n();
  // User messages render as plain text by default so displayed text matches what the user typed.
  // Markdown rendering is an appearance preference.
  const { renderUserMarkdown } = useAppearance();
  const meta = contextMeta[item.kind];
  const hookExecution = item.kind === "system" ? item.hookExecution : undefined;
  const Icon = hookExecution ? Workflow : meta.icon;
  const assistantStreaming = item.kind === "assistant" && item.streaming === true;
  const reasoningStreaming = item.kind === "reasoning" && item.streaming === true;
  const hookStreaming = hookExecution?.status === "running";
  const streaming = assistantStreaming || reasoningStreaming || hookStreaming;
  const hasUserToolbar = item.kind === "user" && !streaming;
  // Encrypted reasoning is deletable but not editable because its body never reached the client.
  // `isEncryptedReasoning` prefers the card's `form`, falling back only to the legacy empty-body heuristic.
  const encryptedReasoning = item.kind === "reasoning" && isEncryptedReasoning(item);
  const showMutationActions = !readOnly && !hookExecution;
  const onEdit = () => onEditItem?.(item);
  const onDelete = () => onDeleteItem?.(item);
  const onBranchFrom = onBranchFromItem && item.kind === "user"
    ? () => onBranchFromItem(item)
    : undefined;
  const onOpenInsert = (event: React.MouseEvent | React.KeyboardEvent, after: boolean) => (
    onOpenInsertAt(event, index + (after ? 1 : 0))
  );
  const hookStatus = hookExecution?.status === "running" ? t("执行中", "Running")
    : hookExecution?.status === "blocked" ? t("已阻止", "Blocked")
      : hookExecution?.status === "failed" ? t("错误（未阻断）", "Error (not blocking)") : t("完成", "Completed");

  return (
    <article
      className={`context-card context-card--${item.kind} ${hookExecution ? `context-card--hook context-card--hook-${hookExecution.status}` : ""} ${streaming && !assistantStreaming ? "context-card--streaming" : ""} ${hasUserToolbar ? "context-card--has-user-toolbar" : ""}`}
      tabIndex={mutationReadOnly ? undefined : 0}
      data-context-id={item.id}
      data-context-index={index}
      aria-busy={streaming || undefined}
      onContextMenu={mutationReadOnly ? undefined : (event) => {
        event.preventDefault();
        const box = event.currentTarget.getBoundingClientRect();
        onOpenInsert(event, event.clientY > box.top + box.height / 2);
      }}
      onKeyDown={mutationReadOnly ? undefined : (event) => {
        if (event.shiftKey && event.key === "F10") {
          event.preventDefault();
          onOpenInsert(event, true);
        }
      }}
    >
      {item.kind !== "user" && item.kind !== "reasoning" && (
        <header className="context-card__header">
          <span className="context-card__kind">
            <Icon size={14} />
            {hookExecution ? `${hookExecution.event} · ${hookStatus}` : meta.label(t)}
          </span>
          {item.kind === "system" && item.localOnly && <span className="context-card__local-only" title={t("只保存在本地时间线，不会发送给模型", "Saved only in the local timeline and not sent to the model")}>{t("仅本地", "Local only")}</span>}
          {hookExecution?.contextInjected && <span className="context-card__hook-injected" title={t("此卡片的内容已作为系统上下文加入后续模型请求", "This card's content has been added as system context for later model requests")}>{t("已加入模型上下文", "Added to model context")}</span>}
          {item.kind !== "assistant" && <span className="context-card__time">{formatTime(item.createdAt, resolvedLanguage)}</span>}
          {showMutationActions && <ContextActions item={item} onEdit={onEdit} onDelete={onDelete} disabled={mutationReadOnly || streaming} />}
        </header>
      )}

      {item.kind === "reasoning" && (
        <ReasoningContent
          content={item.content ?? ""}
          streaming={item.streaming === true}
          startedAt={item.startedAt}
          durationMs={item.durationMs}
          tokens={item.tokens}
          encrypted={encryptedReasoning}
          deferOffscreen={deferOffscreen}
          pathBaseDir={pathBaseDir}
          actions={showMutationActions ? (
            <ContextActions
              item={item}
              onEdit={onEdit}
              onDelete={onDelete}
              // Encrypted cards cannot be edited because fabricated reasoning becomes part of the
              // next round's history.
              allowEdit={!encryptedReasoning}
              disabled={mutationReadOnly || streaming}
            />
          ) : undefined}
        />
      )}

      {item.kind === "user" && <ImageStrip images={item.images} className="context-card__images" />}
      {(item.kind === "system" || item.kind === "assistant" || (item.kind === "user" && Boolean(item.content))) && (
        <div className="context-card__content" aria-live={assistantStreaming || hookStreaming ? "polite" : undefined}>
          {item.kind === "assistant" || (item.kind === "user" && renderUserMarkdown)
            ? (
              <MarkdownContent
                content={item.content}
                deferOffscreen={deferOffscreen}
                streaming={assistantStreaming}
                // Only model output is scanned for paths. User text keeps
                // rendering exactly what was typed, Markdown preference or not.
                linkifyPaths={item.kind === "assistant"}
                pathBaseDir={pathBaseDir}
              />
            )
            : item.content}
        </div>
      )}
      {item.kind === "assistant" && (item.sources?.length ?? 0) > 0 && (
        <nav className="context-card__sources" aria-label={t("引用来源", "Cited sources")}>
          {(item.sources ?? []).map((source) => {
            const label = source.title?.trim() || sourceHostname(source.url) || source.id;
            const href = safeSourceHref(source.url);
            return href ? (
              <a
                key={source.id}
                className="context-card__source"
                href={href}
                title={href}
                target="_blank"
                rel="noreferrer noopener"
              >
                {label}
              </a>
            ) : (
              // List citations without a trusted URL too; omitting them hides part of what the
              // model cited.
              <span key={source.id} className="context-card__source" title={source.id}>
                {label}
              </span>
            );
          })}
        </nav>
      )}
      {hasUserToolbar && (
        <UserMessageToolbar
          content={item.content ?? ""}
          navigation={branchNavigation}
          editable={!readOnly}
          mutationDisabled={mutationReadOnly}
          branchFromDisabledReason={branchFromDisabledReason}
          branchDisabledReason={branchSwitchDisabledReason}
          onEdit={onEdit}
          onDelete={onDelete}
          onBranch={readOnly ? undefined : onBranchFrom}
          onSelectBranch={(branchId) => onSelectBranchFor?.(item.id, branchId)}
        />
      )}
    </article>
  );
});

/** Falls back to the hostname when a citation has no title. */
function sourceHostname(url: string | undefined): string | undefined {
  if (!url) return undefined;
  try {
    return new URL(url).hostname || undefined;
  } catch {
    return undefined;
  }
}

/**
 * Renders only http/https citations as links. Upstream search URLs are untrusted input, so this
 * manually written anchor must reject schemes such as `javascript:`.
 */
function safeSourceHref(url: string | undefined): string | undefined {
  if (!url) return undefined;
  try {
    const parsed = new URL(url);
    return parsed.protocol === "http:" || parsed.protocol === "https:" ? url : undefined;
  } catch {
    return undefined;
  }
}

interface ContextMenuState {
  x: number;
  y: number;
  index: number;
  trigger: HTMLElement | null;
}

function renderNodeKey(node: ContextRenderNode): string {
  return node.kind === "context" ? node.item.id : node.key;
}

function renderNodeContextIds(node: ContextRenderNode): string[] {
  if (node.kind === "context") return [node.item.id];
  if (node.kind === "workflow-card") return [node.entry.item.id];
  if (node.kind === "question") {
    return [node.entry.item.id, ...(node.answer ? [node.answer.item.id] : [])];
  }
  return node.entries.map((entry) => entry.item.id);
}

interface TurnNodeProjection {
  turn: ConversationTurn;
  bodyNodes: ContextRenderNode[];
  terminalNode?: ContextRenderNode;
}

export const ContextStream = memo(function ContextStream({ contexts, turns = [], onToggleTurn, tools, enabledTools, timelineId, readOnly = false, timelineMutationLocked = false, streaming = false, thinking = null, retryNotice = null, ariaLabel, pendingQuestionId, emptyState, emptyStateVisible = true, onEdit, onDelete, onEditQuestion, onDeleteQuestion, onBranchFrom, branchFromDisabledReason, branchNavigations, onSelectBranch, branchSwitchDisabledReason, onInsert, onOpenSubagent, workflowRunByCall, onOpenWorkflowRun, onRetryTurnError, retryableTurnRequestId = null, onDismissTurnError, pathBaseDir = null }: ContextStreamProps) {
  const { t } = useI18n();
  const [menu, setMenu] = useState<ContextMenuState | null>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  const streamRef = useRef<HTMLDivElement>(null);
  const scrollPinnedRef = useRef(true);
  const scrollFrameRef = useRef<number | null>(null);
  const previousTimelineRef = useRef<{
    timelineId: string | undefined;
    length: number;
    lastId: string | null;
    lastVisualLength: number;
  } | null>(null);
  const mutationReadOnly = readOnly || timelineMutationLocked;
  const contextIndexes = useMemo(
    () => new Map(contexts.map((context, index) => [context.id, index])),
    [contexts]
  );
  const turnAnchorIndexes = useMemo(
    () => new Set(turns.flatMap((turn) => {
      const index = contextIndexes.get(turn.anchorContextId);
      return index === undefined ? [] : [index];
    })),
    [contextIndexes, turns]
  );
  const renderNodes = useMemo(
    () => buildContextRenderNodes(contexts, turnAnchorIndexes),
    [contexts, turnAnchorIndexes]
  );
  const turnProjection = useMemo(() => {
    const firstBodyNode = new Map<string, TurnNodeProjection>();
    const bodyNodeKeys = new Set<string>();
    const emptyTurnsAfterNode = new Map<string, ConversationTurn[]>();
    const leadingTurns: ConversationTurn[] = [];
    const visibleRunningTurnIds = new Set<string>();
    const nodesWithIds = renderNodes.map((node) => ({
      node,
      ids: renderNodeContextIds(node),
      key: renderNodeKey(node)
    }));
    const nodeIndexByContextId = new Map<string, number>();
    nodesWithIds.forEach(({ ids }, index) => {
      ids.forEach((id) => nodeIndexByContextId.set(id, index));
    });
    /**
     * Where a turn sits in the stream. Its anchor is the natural answer, but a
     * turn outlives the deletion of its own anchor: fall back to just before
     * the first message it still owns, so a round that lost its user message
     * keeps the header carrying its duration and token counts instead of
     * disappearing and leaving its replies looking like part of the round
     * before it.
     */
    const streamPosition = (turn: ConversationTurn): number | undefined => {
      const anchorIndex = turnAnchorIndex(turn.anchorContextId, contextIndexes);
      if (anchorIndex !== undefined) return anchorIndex;
      const owned = turn.contextIds
        .map((id) => contextIndexes.get(id))
        .filter((index): index is number => index !== undefined);
      return owned.length ? Math.min(...owned) - 1 : undefined;
    };
    /**
     * The drawn node a header with no body of its own sits after. The context at
     * that position may not be drawn at all — an empty protocol assistant, or an
     * `ask_user` answer folded into its question card — so walk back to the
     * nearest one that is, rather than dropping the header entirely.
     */
    const anchorNodeFor = (turn: ConversationTurn) => {
      const position = streamPosition(turn);
      if (position === undefined) return undefined;
      for (let index = position; index >= 0; index -= 1) {
        const nodeIndex = nodeIndexByContextId.get(contexts[index].id);
        if (nodeIndex !== undefined) return nodesWithIds[nodeIndex];
      }
      return undefined;
    };
    const sortedTurns = turns
      .flatMap((turn) => {
        const position = streamPosition(turn);
        return position === undefined ? [] : [{ turn, position }];
      })
      .sort((left, right) => left.position - right.position)
      .map((entry) => entry.turn);

    for (const turn of sortedTurns) {
      const orderedContexts = turn.contextIds
        .flatMap((id) => {
          const index = contextIndexes.get(id);
          return index === undefined ? [] : [{ item: contexts[index], index }];
        })
        .sort((left, right) => left.index - right.index);
      const terminalAssistant = turn.status === "completed"
        ? [...orderedContexts].reverse().find(({ item }) => (
          item.kind === "assistant" && !item.interrupted
        ))?.item
        : undefined;
      const terminalNodeIndex = terminalAssistant
        ? nodeIndexByContextId.get(terminalAssistant.id)
        : undefined;
      const ownedNodeIndexes = [...new Set(orderedContexts.flatMap(({ item }) => {
        const index = nodeIndexByContextId.get(item.id);
        return index === undefined ? [] : [index];
      }))].sort((left, right) => left - right);

      if (ownedNodeIndexes.length) {
        const first = ownedNodeIndexes[0];
        const bodyNodes = ownedNodeIndexes
          .filter((index) => index !== terminalNodeIndex)
          .map((index) => renderNodes[index]);
        const terminalNode = terminalNodeIndex === undefined
          ? undefined
          : renderNodes[terminalNodeIndex];
        firstBodyNode.set(renderNodeKey(renderNodes[first]), { turn, bodyNodes, terminalNode });
        ownedNodeIndexes.forEach((index) => bodyNodeKeys.add(renderNodeKey(renderNodes[index])));
        // Only a turn that reaches the DOM hosts the waiting indicator; counting
        // one that was dropped below would suppress the end-of-stream fallback
        // and leave a live run with no visible sign of activity at all.
        if (turn.status === "running") visibleRunningTurnIds.add(turn.id);
        continue;
      }

      // Nothing of this turn's own is drawn: either every message it claimed has
      // been deleted, or it never produced one. The header is worth keeping only
      // while it still hosts something — the streaming indicator of a live run,
      // or the notice explaining why a run stopped. A stop that lands before the
      // first message leaves neither, and "stopped after 3s" over an empty body
      // is the shape of a bug report, not a record of a round. That round is
      // dropped from storage as it settles, so what reaches here is a round
      // whose own messages the user deleted: it keeps its record, and simply
      // draws nothing until it has something to disclose.
      if (turn.status !== "running" && !turn.error) continue;
      const anchorNode = anchorNodeFor(turn);
      if (anchorNode) {
        const pending = emptyTurnsAfterNode.get(anchorNode.key) ?? [];
        emptyTurnsAfterNode.set(anchorNode.key, [...pending, turn]);
      } else {
        // Nothing is drawn ahead of this turn — it opens the conversation, or
        // the deletion that took its anchor took everything before it. Lead the
        // stream with the header rather than dropping the only surface a live
        // run has before its first message arrives.
        leadingTurns.push(turn);
      }
      if (turn.status === "running") visibleRunningTurnIds.add(turn.id);
    }

    return { firstBodyNode, bodyNodeKeys, emptyTurnsAfterNode, leadingTurns, visibleRunningTurnIds };
  }, [contextIndexes, contexts, renderNodes, turns]);
  const renderedContextIndexes = useMemo(() => renderNodes.flatMap((node) => {
    if (node.kind === "context") return [node.index];
    if (node.kind === "workflow-card") return [node.entry.index];
    if (node.kind === "question") {
      return node.answer ? [node.entry.index, node.answer.index] : [node.entry.index];
    }
    return node.entries.map((entry) => entry.index);
  }).sort((left, right) => left - right), [renderNodes]);
  const displayInsertionIndex = menu === null
    ? null
    : renderedContextIndexes.find((index) => index >= menu.index) ?? contexts.length;

  useEffect(() => {
    if (mutationReadOnly) {
      setMenu(null);
      return;
    }
    if (!menu) return;
    const close = () => setMenu(null);
    window.addEventListener("pointerdown", close);
    window.addEventListener("resize", close);
    window.addEventListener("scroll", close, true);
    menuRef.current?.querySelector<HTMLButtonElement>("button")?.focus();
    return () => {
      window.removeEventListener("pointerdown", close);
      window.removeEventListener("resize", close);
      window.removeEventListener("scroll", close, true);
    };
  }, [menu, mutationReadOnly]);

  useEffect(() => {
    const scroller = streamRef.current;
    const previous = previousTimelineRef.current;
    const last = contexts[contexts.length - 1];
    const next = {
      timelineId,
      length: contexts.length,
      lastId: last?.id ?? null,
      lastVisualLength: contextVisualLength(last)
    };
    previousTimelineRef.current = next;
    if (scrollFrameRef.current !== null) {
      window.cancelAnimationFrame(scrollFrameRef.current);
      scrollFrameRef.current = null;
    }
    const timelineChanged = previous !== null && previous.timelineId !== next.timelineId;
    // The owning conversation surface restores its saved scroll position after
    // a switch. Do not reinterpret the new timeline as appended output.
    if (timelineChanged || !scroller || contexts.length === 0 || menu || !scrollPinnedRef.current) return;

    const initialTimeline = previous === null;
    const appended = previous !== null && next.length > previous.length;
    const growingTail = previous !== null
      && next.length === previous.length
      && next.lastId === previous.lastId
      && next.lastVisualLength > previous.lastVisualLength;
    const streamingUpdate = previous !== null
      && streaming
      && next.length >= previous.length
      && next.lastId === previous.lastId;
    if (!initialTimeline && !appended && !growingTail && !streamingUpdate) return;

    scrollFrameRef.current = window.requestAnimationFrame(() => {
      scrollFrameRef.current = null;
      const current = streamRef.current;
      if (!current || !scrollPinnedRef.current) return;
      // Keep follow-output instant. A smooth scroll can outlive a tail deletion
      // and close a context menu opened immediately after it.
      current.scrollTop = current.scrollHeight;
    });
  }, [contexts, menu, streaming, timelineId]);

  useEffect(() => () => {
    if (scrollFrameRef.current !== null) window.cancelAnimationFrame(scrollFrameRef.current);
  }, []);

  const openMenu = useCallback((event: React.MouseEvent | React.KeyboardEvent, index: number) => {
    if (mutationReadOnly) return;
    const safeIndex = index;
    const mouse = "clientX" in event ? event : null;
    const target = event.currentTarget as HTMLElement;
    const rect = target.getBoundingClientRect();
    const rawX = mouse?.clientX || rect.left + 32;
    const rawY = mouse?.clientY || rect.bottom;
    setMenu({
      x: Math.max(8, Math.min(rawX, window.innerWidth - 290)),
      y: Math.max(8, Math.min(rawY, window.innerHeight - 360)),
      index: safeIndex,
      trigger: target
    });
  }, [mutationReadOnly]);

  const renderTimelineNode = (node: ContextRenderNode): ReactNode => {
    if (node.kind === "tool-group") {
      return (
        <div className="context-slot" key={node.key}>
          <ToolCallGroup
            entries={node.entries}
            tools={tools}
            insertionIndex={displayInsertionIndex}
            readOnly={mutationReadOnly}
            onEdit={onEdit}
            onDelete={onDelete}
            onOpenInsert={openMenu}
            onOpenSubagent={onOpenSubagent}
          />
        </div>
      );
    }

    if (node.kind === "question") {
      if (node.entry.item.id === pendingQuestionId) return null;
      return (
        <div className="context-slot" key={node.key}>
          {!mutationReadOnly && displayInsertionIndex === node.entry.index && <div className="insertion-line"><span><Plus size={12} /></span></div>}
          <QuestionTimelineCard
            item={node.entry.item}
            index={node.entry.index}
            answer={node.answer?.item}
            answerIndex={node.answer?.index}
            readOnly={mutationReadOnly}
            onEdit={onEditQuestion}
            onDelete={onDeleteQuestion}
            onOpenInsert={openMenu}
          />
        </div>
      );
    }

    if (node.kind === "workflow-card") {
      // Do not render an unfinished workflow call without a view: an empty card would falsely
      // represent a zero-step run. Terminal calls fall back to the generic tool card, including
      // live calls completed before a run record exists.
      const view = workflowRunByCall?.[node.entry.item.id];
      if (!view) {
        const item = node.entry.item;
        const settled =
          item.streamStatus === "completed"
          || (!item.streaming && item.streamStatus === undefined);
        if (!settled) return null;
        return (
          <div className="context-slot" key={node.key}>
            <ToolCallGroup
              entries={[node.entry]}
              tools={tools}
              insertionIndex={displayInsertionIndex}
              readOnly={mutationReadOnly}
              onEdit={onEdit}
              onDelete={onDelete}
              onOpenInsert={openMenu}
              onOpenSubagent={onOpenSubagent}
              workflowFallback
            />
          </div>
        );
      }
      return (
        <div
          className="context-slot"
          key={node.key}
          onContextMenu={mutationReadOnly ? undefined : (event) => {
            event.preventDefault();
            // The stream-level fallback recognizes only `.context-card`; stop propagation so the
            // menu targets this workflow entry rather than the end of the timeline.
            event.stopPropagation();
            const box = event.currentTarget.getBoundingClientRect();
            openMenu(event, node.entry.index + (event.clientY > box.top + box.height / 2 ? 1 : 0));
          }}
        >
          {!mutationReadOnly && displayInsertionIndex === node.entry.index && <div className="insertion-line"><span><Plus size={12} /></span></div>}
          {/* Read-only transcripts have no task container, so the workflow card is only a summary. */}
          <WorkflowRunCard view={view} onOpen={readOnly ? undefined : onOpenWorkflowRun} />
        </div>
      );
    }

    // Encrypted reasoning that is still arriving has no body to disclose and no
    // duration to state yet. The stream indicator narrates it beside the cat
    // until the round closes; only then does the card appear, carrying the
    // figures that are its whole content.
    if (isLiveEncryptedReasoning(node.item)) return null;

    return (
      <div className="context-slot" key={node.item.id}>
        {!mutationReadOnly && displayInsertionIndex === node.index && <div className="insertion-line"><span><Plus size={12} /></span></div>}
        <ContextCard
          item={node.item}
          index={node.index}
          readOnly={readOnly}
          mutationReadOnly={mutationReadOnly}
          onEditItem={onEdit}
          onDeleteItem={onDelete}
          onBranchFromItem={onBranchFrom}
          branchFromDisabledReason={branchFromDisabledReason}
          branchNavigation={branchNavigations?.[node.item.id]}
          onSelectBranchFor={onSelectBranch}
          branchSwitchDisabledReason={branchSwitchDisabledReason}
          deferOffscreen={node.index < contexts.length - 4}
          pathBaseDir={pathBaseDir}
          onOpenInsertAt={openMenu}
        />
      </div>
    );
  };

  const renderTurnDisclosure = (projection: TurnNodeProjection): ReactNode => (
    <Fragment key={projection.turn.id}>
      <ConversationTurnDisclosure
        turn={projection.turn}
        onToggle={() => onToggleTurn?.(projection.turn.id)}
      >
        {projection.bodyNodes.map(renderTimelineNode)}
        {streaming && projection.turn.status === "running" && (
          <StreamWaitingIndicator contexts={contexts} tools={tools} thinking={thinking} retryNotice={retryNotice} />
        )}
        {projection.turn.error && (
          <TurnErrorNotice
            error={projection.turn.error}
            onRetry={
              onRetryTurnError && retryableTurnRequestId === projection.turn.requestId
                ? onRetryTurnError
                : undefined
            }
            onDismiss={onDismissTurnError}
          />
        )}
      </ConversationTurnDisclosure>
      {projection.terminalNode ? renderTimelineNode(projection.terminalNode) : null}
    </Fragment>
  );

  return (
    <div
      ref={streamRef}
      className="context-scroll"
      data-main-context-stream={!readOnly || undefined}
      role={ariaLabel ? "region" : undefined}
      aria-label={ariaLabel}
      onScroll={(event) => {
        const scroller = event.currentTarget;
        scrollPinnedRef.current = scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight < 180;
      }}
      onContextMenu={mutationReadOnly ? undefined : (event) => {
        const target = event.target as HTMLElement;
        if (target.closest(".context-card")) return;
        event.preventDefault();
        const cards = Array.from(event.currentTarget.querySelectorAll<HTMLElement>(".context-card"));
        const nearestCard = cards.find((card) => {
          const box = card.getBoundingClientRect();
          return event.clientY < box.top + box.height / 2;
        });
        const nearestIndex = nearestCard ? Number(nearestCard.dataset.contextIndex) : contexts.length;
        openMenu(event, Number.isInteger(nearestIndex) ? nearestIndex : contexts.length);
      }}
    >
      <div className="context-stream">
        {emptyStateVisible && renderNodes.length === 0 && turnProjection.leadingTurns.length === 0 ? (
          emptyState ?? (
            <EmptyState
              icon={<MessageSquare size={22} />}
              title={t("这段对话还没有消息", "This conversation has no messages yet")}
              description={t("会话内容会显示在这里。", "Conversation content will appear here.")}
            />
          )
        ) : (
          <>
            {turnProjection.leadingTurns.map((turn) => renderTurnDisclosure({ turn, bodyNodes: [] }))}
            {renderNodes.map((node) => {
              const key = renderNodeKey(node);
              if (turnProjection.bodyNodeKeys.has(key)) {
                const projection = turnProjection.firstBodyNode.get(key);
                return projection ? renderTurnDisclosure(projection) : null;
              }
              const emptyTurns = turnProjection.emptyTurnsAfterNode.get(key) ?? [];
              return (
                <Fragment key={`timeline:${key}`}>
                  {renderTimelineNode(node)}
                  {emptyTurns.map((turn) => renderTurnDisclosure({ turn, bodyNodes: [] }))}
                </Fragment>
              );
            })}
          </>
        )}
        {streaming && turnProjection.visibleRunningTurnIds.size === 0 && (
          <StreamWaitingIndicator contexts={contexts} tools={tools} thinking={thinking} retryNotice={retryNotice} />
        )}
        {!mutationReadOnly && displayInsertionIndex === contexts.length && renderNodes.length > 0 && <div className="insertion-line"><span><Plus size={12} /></span></div>}
        {!mutationReadOnly && <p className="context-stream__hint">{t("在上下文之间右键，可精确插入新内容", "Right-click between contexts to insert content precisely")}</p>}
      </div>

      {!mutationReadOnly && menu && (
        <div
          ref={menuRef}
          className="context-menu"
          role="menu"
          aria-label={t("添加上下文", "Add context")}
          style={{ left: menu.x, top: menu.y }}
          onPointerDown={(event) => event.stopPropagation()}
          onKeyDown={(event) => {
            if (event.key === "Escape") {
              event.preventDefault();
              const trigger = menu.trigger;
              setMenu(null);
              window.requestAnimationFrame(() => trigger?.focus());
              return;
            }
            if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return;
            event.preventDefault();
            const buttons = Array.from(event.currentTarget.querySelectorAll<HTMLButtonElement>("button:not(:disabled)"));
            const current = buttons.indexOf(document.activeElement as HTMLButtonElement);
            const direction = event.key === "ArrowDown" ? 1 : -1;
            buttons[(current + direction + buttons.length) % buttons.length]?.focus();
          }}
        >
          <div className="context-menu__title">
            <Plus size={13} />
            {t("在此处添加上下文", "Add context here")}
          </div>
          {(Object.keys(contextMeta) as InsertableContextKind[]).map((kind) => {
            const item = contextMeta[kind];
            const Icon = item.icon;
            const disabled = kind === "tool" && enabledTools.length === 0;
            return (
              <button
                type="button"
                role="menuitem"
                key={kind}
                disabled={disabled}
                onClick={() => {
                  onInsert?.(menu.index, kind);
                  setMenu(null);
                }}
              >
                <span className={`context-menu__icon context-menu__icon--${kind}`}>
                  <Icon size={15} />
                </span>
                <span>
                  <strong>{item.label(t)}</strong>
                  <small>{disabled ? t("当前对话没有启用工具", "No tools are enabled in this conversation") : item.description(t)}</small>
                </span>
                {kind === "tool" && <FileJson size={14} className="context-menu__trailing" />}
              </button>
            );
          })}
          <div className="context-menu__footer">
            <RotateCcw size={12} /> {t("新内容会插入到第 {index} 个位置", "New content will be inserted at position {index}", { index: menu.index + 1 })}
          </div>
        </div>
      )}
    </div>
  );
});
