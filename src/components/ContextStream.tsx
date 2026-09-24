import {
  Bot,
  BrainCircuit,
  Check,
  ChevronLeft,
  ChevronRight,
  CircleAlert,
  Copy,
  GitBranch,
  MessageSquare,
  Pencil,
  Shield,
  Trash2,
  UserRound,
  Wrench,
  X
} from "lucide-react";
import { Fragment, memo, useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import type { ReactNode } from "react";
import { useI18n } from "../i18n";
import type { AssistantContext, ContextItem, FileAttachment, ImageAttachment, InsertableContextKind, JsonObject, SystemContext, ToolContext, ToolDescriptor, UserContext } from "../types";
import type { AddMessageAttachments } from "../lib/fileAttachments";
import type { ContextBranchNavigation } from "../lib/conversationBranches";
import { turnAnchorIndex } from "../lib/conversationTurns";
import type { ConversationTurn } from "../lib/conversationTurns";
import type { WorkflowRunView } from "../lib/workflowRuns";
import type { LiveReasoningView } from "../lib/runContexts";
import { EmptyState, IconButton } from "./Common";
import { buildContextRenderNodes, TimelineBlock, TimelineRow } from "./TimelineBlock";
import type { ContextRenderNode } from "./TimelineBlock";
import { InlineTextEditor } from "./InlineTextEditor";
import { draftToolContext, InlineToolEditor } from "./InlineToolEditor";
import { getToolPresentation, toolRowName } from "./ToolRenderers";
import { estimateContextTokens, formatCompactTokenCount } from "../lib/contextTokens";
import { writeClipboardText } from "../lib/clipboard";
import { useFloatingSurface } from "../lib/floatingSurfaces";
import { textWithoutAppendedImagePlaceholders } from "../lib/imageShortIds";
import { stripSelectedElementBlocks } from "../lib/selectedElement";
import { useAppearance } from "../lib/appearance";
import { MarkdownContent } from "./MarkdownContent";
import { QuestionTimelineCard } from "./QuestionTimelineCard";
import { StreamWaitingIndicator } from "./StreamWaitingIndicator";
import { groupLabel, groupMeta } from "./ToolSelectionGroups";
import type { ToolCategory } from "./ToolSelectionGroups";
import { isPreviewToolName } from "../lib/taskTools";
import { ImageStrip } from "./ImageStrip";

// Wide enough to absorb fractional scroll metrics, narrow enough that the user
// has to actually be at the bottom to re-attach to follow-output.
const SCROLL_BOTTOM_EPSILON = 16;

/**
 * Everything the stream-level right-click can place an insertion against: the
 * message cards and the work blocks between them, each carrying the raw context
 * index the menu splices at.
 */
const TIMELINE_ANCHOR_SELECTOR = ".context-card, .timeline-block, .question-history";

/**
 * The one editor open on this timeline. Editing happens in the card itself, so
 * the owner keeps the state and the stream decides which card gives up its body
 * for it.
 */
export type TimelineEditorState =
  | { mode: "insert"; kind: InsertableContextKind; index: number; toolName?: string }
  | { mode: "edit"; kind: InsertableContextKind; item: ContextItem; index: number };

export interface TimelineQuestionEditorState {
  item: ToolContext;
  answer?: UserContext;
}

export interface ContextStreamProps {
  contexts: ContextItem[];
  /** Frontend-only presentation spans; never used to assemble model context. */
  turns?: ConversationTurn[];
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
  onEdit?: (item: ContextItem) => void;
  onDelete?: (item: ContextItem) => void;
  /** Edits the projected ask_user tool and its paired answer as one message. */
  onEditQuestion?: (item: ToolContext, answer?: UserContext) => void;
  /** Deletes the projected ask_user tool and its paired answer as one message. */
  onDeleteQuestion?: (item: ToolContext, answer?: UserContext) => void;
  /** The card or insertion point currently showing an editor in place of its body. */
  editor?: TimelineEditorState | null;
  questionEditor?: TimelineQuestionEditorState | null;
  onCancelEdit?: () => void;
  onSaveText?: (content: string, images?: ImageAttachment[], files?: FileAttachment[]) => void;
  /**
   * Attaches files picked, pasted or dropped into a user message being written
   * or edited, and returns what was accepted (images numbered) and what was
   * turned away. Absent where nothing can be attached.
   */
  onAddAttachments?: AddMessageAttachments;
  /** Whether this surface's model can see images, which decides how a drag of pictures reads. */
  attachmentImageInput?: boolean;
  onSaveTool?: (toolName: string, input: JsonObject) => Promise<unknown>;
  onSaveToolEdit?: (input: JsonObject, output: string, images: ImageAttachment[]) => Promise<unknown>;
  onSaveQuestion?: (input: JsonObject, answerContent?: string) => void | Promise<void>;
  /** Opens a new conversation drafted with this existing user message. */
  onBranchFrom?: (item: ContextItem) => void;
  branchFromDisabledReason?: string | null;
  branchNavigations?: Record<string, ContextBranchNavigation>;
  onSelectBranch?: (forkContextId: string, branchId: string) => void;
  branchSwitchDisabledReason?: string | null;
  /** Places a new context, naming the tool when a call is what is being placed. */
  onInsert?: (index: number, kind: InsertableContextKind, toolName?: string) => void;
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
}> = {
  system: { label: (t) => t("系统提示词", "System prompt"), icon: Shield },
  user: { label: (t) => t("用户输入", "User input"), icon: UserRound },
  reasoning: { label: (t) => t("思考字段", "Reasoning field"), icon: BrainCircuit },
  tool: { label: (t) => t("工具调用", "Tool call"), icon: Wrench },
  assistant: { label: (t) => t("模型回复", "Model reply"), icon: Bot }
};

function contextVisualLength(item: ContextItem | undefined): number {
  if (!item) return 0;
  if (item.kind === "tool") return item.result.output.length + (item.result.diff?.length ?? 0) + (item.result.images?.length ?? 0) * 200;
  return (item.content?.length ?? 0)
    + (item.kind === "user" ? ((item.images?.length ?? 0) + (item.files?.length ?? 0)) * 200 : 0);
}

/** The one line a collapsed system-prompt row shows of the prompt it holds. */
function firstProseLine(content: string): string | undefined {
  return content.split("\n").map((line) => line.trim()).find(Boolean);
}

function ContextActions({
  onEdit,
  onDelete,
  disabled = false
}: {
  onEdit: () => void;
  onDelete: () => void;
  disabled?: boolean;
}) {
  const { t } = useI18n();
  return (
    <div className="context-actions">
      <IconButton label={t("编辑上下文", "Edit context")} onClick={onEdit} disabled={disabled}>
        <Pencil size={14} />
      </IconButton>
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
    const copied = await writeClipboardText(content);
    setCopyStatus(copied ? "success" : "error");
    if (copyResetTimer.current !== null) window.clearTimeout(copyResetTimer.current);
    copyResetTimer.current = window.setTimeout(() => setCopyStatus("idle"), 1600);
  };

  const copyLabel = copyStatus === "success" ? t("已复制", "Copied") : copyStatus === "error" ? t("复制失败", "Copy failed") : t("复制用户消息", "Copy user message");
  return (
    <div className="user-message-toolbar">
      {/* Branch position stays visible: which of several branches is on screen
          is state, not an action, and reading it should not require hovering. */}
      <div className="user-message-toolbar__actions">
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
      </div>
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
  editing,
  onCancelEdit,
  onSaveText,
  onAddAttachments,
  attachmentImageInput,
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
  item: SystemContext | UserContext | AssistantContext;
  index: number;
  readOnly: boolean;
  mutationReadOnly: boolean;
  editing: boolean;
  onCancelEdit?: () => void;
  onSaveText?: (content: string, images?: ImageAttachment[], files?: FileAttachment[]) => void;
  onAddAttachments?: AddMessageAttachments;
  attachmentImageInput?: boolean;
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
  const { t } = useI18n();
  // User messages render as plain text by default so displayed text matches what the user typed.
  // Markdown rendering is an appearance preference.
  const { renderUserMarkdown, collapseReasoning } = useAppearance();
  const promptCard = item.kind === "system";
  const [promptExpanded, setPromptExpanded] = useState(!collapseReasoning);
  const assistantStreaming = item.kind === "assistant" && item.streaming === true;
  const hasUserToolbar = item.kind === "user";
  // A picked element expands into a prompt block the user never typed, and an attached image
  // gets the `[Image #N]` the model cites it by. The chip and the thumbnail are what the user
  // made; both expansions belong to the model, so the transcript shows the message without
  // them. A number the user typed into their own prose is theirs, and stays.
  const displayContent = item.kind === "user" && item.content
    ? textWithoutAppendedImagePlaceholders(stripSelectedElementBlocks(item.content), item.images)
    : item.content;
  const editingInPlace = editing && Boolean(onCancelEdit && onSaveText);
  const showMutationActions = !readOnly && !editingInPlace;
  // The system prompt is a timeline row like reasoning is, so it edits the way
  // reasoning does: inside the row, under its own header. Only the kinds that
  // replace the whole card while editing wear the editing ring.
  const promptEditing = promptCard && editingInPlace;
  const onEdit = () => onEditItem?.(item);
  const onDelete = () => onDeleteItem?.(item);
  const onBranchFrom = onBranchFromItem && item.kind === "user"
    ? () => onBranchFromItem(item)
    : undefined;
  const onOpenInsert = (event: React.MouseEvent | React.KeyboardEvent, after: boolean) => (
    onOpenInsertAt(event, index + (after ? 1 : 0))
  );

  // A reply or a message has no card shell to sit in while being written, so
  // both creating one from the context menu and a pencil edit show the bare editor.
  if ((item.kind === "assistant" || item.kind === "user") && editingInPlace) {
    return (
      <InlineTextEditor
        kind={item.kind}
        content={item.content ?? ""}
        images={item.kind === "user" ? item.images : undefined}
        files={item.kind === "user" ? item.files : undefined}
        onCancel={onCancelEdit!}
        onSave={onSaveText!}
        onAddAttachments={item.kind === "user" ? onAddAttachments : undefined}
        imageInput={attachmentImageInput}
      />
    );
  }

  return (
    <article
      className={`context-card context-card--${item.kind} ${hasUserToolbar && !editingInPlace ? "context-card--has-user-toolbar" : ""} ${editingInPlace && !promptCard ? "context-card--editing" : ""}`}
      tabIndex={mutationReadOnly ? undefined : 0}
      data-context-id={item.id}
      data-context-index={index}
      aria-busy={assistantStreaming || undefined}
      onContextMenu={mutationReadOnly || editingInPlace ? undefined : (event) => {
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
      {promptCard && (
        <TimelineRow
          contextId={item.id}
          index={index}
          rowKind="system"
          icon={Shield}
          name="system"
          line={firstProseLine(item.content)}
          stat={t("{tokens} token", "{tokens} tokens", { tokens: formatCompactTokenCount(estimateContextTokens(item)) })}
          accessibleName={t("系统提示词", "System prompt")}
          badge={item.localOnly ? (
            <span
              className="timeline-row__badge"
              title={t("只保存在本地时间线，不会发送给模型", "Saved only in the local timeline and not sent to the model")}
            >
              {t("仅本地", "Local only")}
            </span>
          ) : undefined}
          actions={showMutationActions ? (
            <>
              <IconButton label={t("编辑上下文", "Edit context")} onClick={onEdit} disabled={mutationReadOnly}>
                <Pencil size={13} />
              </IconButton>
              <IconButton label={t("删除上下文", "Delete context")} onClick={onDelete} disabled={mutationReadOnly}>
                <Trash2 size={13} />
              </IconButton>
            </>
          ) : undefined}
          expandable={item.content.length > 0 || promptEditing}
          expanded={(item.content.length > 0 || promptEditing) && (promptExpanded || promptEditing)}
          onToggleExpanded={() => !promptEditing && setPromptExpanded((current) => !current)}
          readOnly={mutationReadOnly}
          onOpenInsert={onOpenInsertAt}
        >
          {promptEditing
            ? (
              <InlineTextEditor
                kind="system"
                content={item.content ?? ""}
                onCancel={onCancelEdit!}
                onSave={onSaveText!}
              />
            )
            : (
              <div className="timeline-row__prose">
                <MarkdownContent
                  content={item.content}
                  deferOffscreen={deferOffscreen}
                  linkifyPaths
                  pathBaseDir={pathBaseDir}
                />
              </div>
            )}
        </TimelineRow>
      )}

      {item.kind === "user" && !editingInPlace && (
        <ImageStrip images={item.images} files={item.files} className="context-card__images" />
      )}
      {!editingInPlace && !promptCard && (item.kind === "assistant" || Boolean(displayContent)) && (
        <div className="context-card__content" aria-live={assistantStreaming ? "polite" : undefined}>
          {item.kind === "assistant" || renderUserMarkdown
            ? (
              <MarkdownContent
                content={displayContent}
                deferOffscreen={deferOffscreen}
                streaming={assistantStreaming}
                // Only model output is scanned for paths and has its HTML
                // rendered. User text keeps rendering exactly what was typed,
                // Markdown preference or not.
                linkifyPaths={item.kind === "assistant"}
                renderHtml={item.kind === "assistant"}
                pathBaseDir={pathBaseDir}
              />
            )
            : displayContent}
        </div>
      )}
      {item.kind === "assistant" && !editingInPlace && (item.sources?.length ?? 0) > 0 && (
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
      {/* A reply names itself by being a reply, so it carries no title. Its
          controls wait under the prose and only surface on hover, where they
          cannot cover a word of what was said. */}
      {item.kind === "assistant" && !editingInPlace && showMutationActions && (
        <div className="context-card__footer-actions">
          <ContextActions
            onEdit={onEdit}
            onDelete={onDelete}
            disabled={mutationReadOnly || assistantStreaming}
          />
        </div>
      )}
      {hasUserToolbar && !editingInPlace && (
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
  /**
   * Which panel the one menu is showing. Naming a tool is two choices deep, so
   * that one opens beside the item rather than replacing what it came from.
   */
  view: { kind: "insert"; tools?: { category: ToolCategory | null } };
}

/**
 * A panel that opens beside the item that owns it.
 *
 * It measures itself once, where it was drawn, and moves only if it would leave
 * the window: to the other side of its item when the right edge is too close,
 * and upward by however much hangs below the bottom. Measuring again after
 * moving would let a flipped panel decide to flip back.
 */
function ContextSubmenu({ label, scrollable = false, children }: {
  label: string;
  /** Only a panel with nothing nested inside it may scroll; see the stylesheet. */
  scrollable?: boolean;
  children: ReactNode;
}) {
  const panelRef = useRef<HTMLDivElement>(null);
  const [placement, setPlacement] = useState<{ flipped: boolean; shift: number }>({ flipped: false, shift: 0 });
  useLayoutEffect(() => {
    const panel = panelRef.current;
    if (!panel) return;
    // A window that reports no size — an unrendered preview — cannot say the
    // panel has left it, and treating 0 as the edge throws it off screen.
    if (!window.innerWidth || !window.innerHeight) return;
    const box = panel.getBoundingClientRect();
    const flipped = box.right > window.innerWidth - 8;
    const shift = Math.min(0, window.innerHeight - 8 - box.bottom);
    if (flipped || shift) setPlacement({ flipped, shift });
  }, []);
  useEffect(() => {
    panelRef.current?.querySelector<HTMLButtonElement>("button")?.focus();
  }, []);
  return (
    <div
      ref={panelRef}
      role="menu"
      aria-label={label}
      className={`context-menu__submenu${scrollable ? " context-menu__submenu--scroll" : ""}${placement.flipped ? " context-menu__submenu--flipped" : ""}`}
      style={placement.shift ? { marginTop: placement.shift } : undefined}
    >
      {children}
    </div>
  );
}

/**
 * A context being placed, drawn in the shell it will have once it exists.
 *
 * Placing a context and correcting one are the same act on the same surface, so
 * they are the same editor inside the same card or row — a separate "add" form
 * would be a second place to learn, and a second place for the two to drift.
 */
function TimelineInsertEditor({ editor, tools, onCancel, onSaveText, onAddAttachments, attachmentImageInput, onSaveTool, onSaveToolEdit }: {
  editor: Extract<TimelineEditorState, { mode: "insert" }>;
  tools: ToolDescriptor[];
  onCancel: () => void;
  onSaveText?: (content: string, images?: ImageAttachment[], files?: FileAttachment[]) => void;
  onAddAttachments?: AddMessageAttachments;
  attachmentImageInput?: boolean;
  onSaveTool?: (toolName: string, input: JsonObject) => Promise<unknown>;
  onSaveToolEdit?: (input: JsonObject, output: string, images: ImageAttachment[]) => Promise<unknown>;
}) {
  const { t } = useI18n();
  const toolName = editor.kind === "tool" ? editor.toolName : undefined;
  const descriptor = tools.find((tool) => tool.name === toolName);
  // Memoized so the editor's argument draft is not reset on every stream flush.
  const draft = useMemo(() => (toolName ? draftToolContext(toolName) : null), [toolName]);

  if (draft && toolName) {
    // A placed call has to be committable somewhere, by either route: running it
    // writes it down, and so does writing it down by hand. A template offers
    // only the second, which is reason enough to draw the editor.
    if (!onSaveTool && !onSaveToolEdit) return null;
    const presentation = getToolPresentation(draft, descriptor, t);
    return (
      <TimelineRow
        contextId={draft.id}
        index={editor.index}
        rowKind="tool"
        icon={presentation.icon}
        name={toolRowName(draft, descriptor)}
        accessibleName={t("添加工具调用 {label}", "Add tool call {label}", { label: descriptor?.label ?? toolName })}
        expandable
        expanded
        onToggleExpanded={() => {}}
        // Not a context of its own yet, so it is not something to right-click an
        // insertion around.
        readOnly
      >
        <InlineToolEditor
          item={draft}
          descriptor={descriptor}
          inserting
          onCancel={onCancel}
          onRun={onSaveTool}
          onSave={onSaveToolEdit}
        />
      </TimelineRow>
    );
  }

  if (editor.kind === "tool" || !onSaveText) return null;
  const textEditor = (
    <InlineTextEditor
      kind={editor.kind}
      showKind={editor.kind === "user"}
      onCancel={onCancel}
      onSave={onSaveText}
      onAddAttachments={editor.kind === "user" ? onAddAttachments : undefined}
      imageInput={attachmentImageInput}
    />
  );
  // Reasoning and the system prompt live inside rows, so they are edited as
  // rows; a reply has no shell of its own even while being written.
  if (editor.kind === "reasoning") {
    return (
      <TimelineRow
        contextId="draft"
        index={editor.index}
        rowKind="reasoning"
        icon={BrainCircuit}
        name="think"
        accessibleName={t("添加思考字段", "Add a reasoning field")}
        expandable
        expanded
        onToggleExpanded={() => {}}
        readOnly
      >
        {textEditor}
      </TimelineRow>
    );
  }
  if (editor.kind === "system") {
    return (
      <TimelineRow
        contextId="draft"
        index={editor.index}
        rowKind="system"
        icon={Shield}
        name="system"
        accessibleName={t("添加系统提示词", "Add a system prompt")}
        expandable
        expanded
        onToggleExpanded={() => {}}
        readOnly
      >
        {textEditor}
      </TimelineRow>
    );
  }
  if (editor.kind === "assistant" || editor.kind === "user") return textEditor;
  return (
    <article className={`context-card context-card--${editor.kind} context-card--editing`}>
      {textEditor}
    </article>
  );
}

function renderNodeKey(node: ContextRenderNode): string {
  return node.kind === "context" ? node.item.id : node.key;
}

function renderNodeContextIds(node: ContextRenderNode): string[] {
  if (node.kind === "context") return [node.item.id];
  if (node.kind === "question") {
    return [node.entry.item.id, ...(node.answer ? [node.answer.item.id] : [])];
  }
  return node.entries.map((entry) => entry.item.id);
}

export const ContextStream = memo(function ContextStream({ contexts, turns = [], tools, enabledTools, timelineId, readOnly = false, timelineMutationLocked = false, streaming = false, thinking = null, retryNotice = null, ariaLabel, pendingQuestionId, onEdit, onDelete, onEditQuestion, onDeleteQuestion, editor = null, questionEditor = null, onCancelEdit, onSaveText, onAddAttachments, attachmentImageInput, onSaveTool, onSaveToolEdit, onSaveQuestion, onBranchFrom, branchFromDisabledReason, branchNavigations, onSelectBranch, branchSwitchDisabledReason, onInsert, onOpenSubagent, workflowRunByCall, onOpenWorkflowRun, onRetryTurnError, retryableTurnRequestId = null, onDismissTurnError, pathBaseDir = null }: ContextStreamProps) {
  const { t } = useI18n();
  const [menu, setMenu] = useState<ContextMenuState | null>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  const streamRef = useRef<HTMLDivElement>(null);
  const scrollPinnedRef = useRef(true);
  const scrollTopRef = useRef(0);
  const scrollTimelineRef = useRef(timelineId);
  const scrollFrameRef = useRef<number | null>(null);
  const previousTimelineRef = useRef<{
    timelineId: string | undefined;
    length: number;
    lastId: string | null;
    lastVisualLength: number;
  } | null>(null);
  const mutationReadOnly = readOnly || timelineMutationLocked;

  // This menu is fixed to the pointer and can reach past the chat column, where the built-in
  // browser's native page paints above every HTML layer. Registering it buys the same hole in
  // that page the shared popovers get.
  useFloatingSurface(menuRef, !mutationReadOnly && menu !== null);
  // Orchestration tools run only inside the model loop, so they are not offered
  // as calls to place by hand. Preview is one merged tool in the settings and has
  // no single call behind that name, so it is not placeable either.
  const insertableToolGroups = useMemo(() => {
    const placeable = tools.filter((tool) => (
      tool.category !== "orchestration"
      && !isPreviewToolName(tool.name)
      && enabledTools.includes(tool.name)
    ));
    return (Object.keys(groupMeta) as ToolCategory[])
      .map((category) => ({ category, tools: placeable.filter((tool) => tool.category === category) }))
      .filter((group) => group.tools.length > 0);
  }, [enabledTools, tools]);
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
  // Only *which* calls have a run decides grouping. A live run's contents change
  // on every tick, and rebuilding every block's entries for a progress bar would
  // rerender the whole timeline several times a second.
  const workflowViewKey = useMemo(
    () => Object.keys(workflowRunByCall ?? {}).sort().join(" "),
    [workflowRunByCall]
  );
  const renderNodes = useMemo(() => {
    const withView = new Set(workflowViewKey ? workflowViewKey.split(" ") : []);
    return buildContextRenderNodes(contexts, turnAnchorIndexes, (callId) => withView.has(callId));
  }, [contexts, turnAnchorIndexes, workflowViewKey]);
  const turnProjection = useMemo(() => {
    const noticesAfterNode = new Map<string, ConversationTurn[]>();
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
     * still shows its notice among its own replies instead of at the top of
     * the timeline.
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
     * The drawn node a turn owning nothing sits after. The context at that
     * position may not be drawn at all — an empty protocol assistant, or an
     * `ask_user` answer folded into its question card — so walk back to the
     * nearest one that is, rather than dropping the notice entirely.
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
    const noticeAfter = (key: string, turn: ConversationTurn) => {
      noticesAfterNode.set(key, [...(noticesAfterNode.get(key) ?? []), turn]);
    };
    const sortedTurns = turns
      .flatMap((turn) => {
        const position = streamPosition(turn);
        return position === undefined ? [] : [{ turn, position }];
      })
      .sort((left, right) => left.position - right.position)
      .map((entry) => entry.turn);

    for (const turn of sortedTurns) {
      // A round draws nothing of its own: its messages are timeline nodes like
      // any other, in their own order. What it still owns is the streaming
      // indicator of a live run and the notice explaining why one stopped, and
      // both belong after the last message the round produced.
      if (turn.status !== "running" && !turn.error) continue;
      const lastOwnedNode = turn.contextIds
        .flatMap((id) => {
          const index = nodeIndexByContextId.get(id);
          return index === undefined ? [] : [index];
        })
        .reduce<number | undefined>(
          (last, index) => (last === undefined || index > last ? index : last),
          undefined
        );
      if (lastOwnedNode !== undefined) {
        noticeAfter(nodesWithIds[lastOwnedNode].key, turn);
      } else {
        const anchorNode = anchorNodeFor(turn);
        // Nothing is drawn ahead of this turn — it opens the conversation, or
        // the deletion that took its anchor took everything before it. Lead the
        // stream with the notice rather than dropping the only surface a live
        // run has before its first message arrives.
        if (anchorNode) noticeAfter(anchorNode.key, turn);
        else leadingTurns.push(turn);
      }
      // Only a turn that reaches the DOM hosts the waiting indicator; counting
      // one that was dropped above would suppress the end-of-stream fallback
      // and leave a live run with no visible sign of activity at all.
      if (turn.status === "running") visibleRunningTurnIds.add(turn.id);
    }

    return { noticesAfterNode, leadingTurns, visibleRunningTurnIds };
  }, [contextIndexes, contexts, renderNodes, turns]);
  const renderedContextIndexes = useMemo(() => renderNodes.flatMap((node) => {
    if (node.kind === "context") return [node.index];
    if (node.kind === "question") {
      return node.answer ? [node.entry.index, node.answer.index] : [node.entry.index];
    }
    return node.entries.map((entry) => entry.index);
  }).sort((left, right) => left - right), [renderNodes]);
  const displayInsertionIndex = menu === null
    ? null
    : renderedContextIndexes.find((index) => index >= menu.index) ?? contexts.length;
  const editingContextId = !mutationReadOnly && editor?.mode === "edit" ? editor.item.id : null;
  const editingQuestionId = !mutationReadOnly ? questionEditor?.item.id ?? null : null;
  // The insert editor snaps to the same drawn row the insertion line does, so an
  // index that addresses a projected or undrawn context still lands somewhere
  // visible. Only the editor moves — `editor.index` still decides where the new
  // context is spliced in.
  const insertEditorIndex = editor?.mode === "insert" && !mutationReadOnly && onCancelEdit
    ? renderedContextIndexes.find((index) => index >= editor.index) ?? contexts.length
    : null;
  const insertEditor = editor?.mode === "insert" && onCancelEdit ? (
    <div className="context-slot context-slot--inserting">
      <TimelineInsertEditor
        // A different tool is a different set of arguments, so the editor starts over.
        key={editor.toolName ?? editor.kind}
        editor={editor}
        tools={tools}
        onCancel={onCancelEdit}
        onSaveText={onSaveText}
        onAddAttachments={onAddAttachments}
        attachmentImageInput={attachmentImageInput}
        onSaveTool={onSaveTool}
        onSaveToolEdit={onSaveToolEdit}
      />
    </div>
  ) : null;

  useEffect(() => {
    if (mutationReadOnly) {
      setMenu(null);
      return;
    }
    if (!menu) return;
    const close = () => setMenu(null);
    // A long tool list scrolls inside the menu; only movement underneath it
    // means the anchor has left.
    const closeOnScroll = (event: Event) => {
      if (event.target instanceof Node && menuRef.current?.contains(event.target)) return;
      setMenu(null);
    };
    window.addEventListener("pointerdown", close);
    window.addEventListener("resize", close);
    window.addEventListener("scroll", closeOnScroll, true);
    return () => {
      window.removeEventListener("pointerdown", close);
      window.removeEventListener("resize", close);
      window.removeEventListener("scroll", closeOnScroll, true);
    };
  }, [menu, mutationReadOnly]);

  // Which panel is on screen, so that opening a submenu does not pull focus back
  // to the row that opened it — the submenu takes focus from there itself.
  const menuPanelKey = menu ? `${menu.index}:${menu.view.kind}` : null;
  useEffect(() => {
    if (menuPanelKey === null) return;
    menuRef.current?.querySelector<HTMLButtonElement>("button")?.focus();
  }, [menuPanelKey]);

  // The panels differ in height — a tool list is much taller than the insert
  // options it replaces — so the anchor is corrected against what was actually
  // drawn rather than against a guess made when the menu opened.
  useLayoutEffect(() => {
    const panel = menuRef.current;
    if (!panel) return;
    const overflow = panel.getBoundingClientRect().bottom - (window.innerHeight - 8);
    if (overflow <= 0) return;
    setMenu((current) => {
      if (!current) return current;
      const y = Math.max(8, current.y - overflow);
      // Identical state ends the pass; a panel taller than the viewport would
      // otherwise keep re-measuring against a clamp it cannot satisfy.
      return y === current.y ? current : { ...current, y };
    });
  }, [menu]);

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
      // The next scroll event compares against where follow-output left the
      // reader, so a jump the browser never reported cannot read as an upward
      // scroll and detach on its own.
      scrollTopRef.current = current.scrollTop;
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
      x: Math.max(8, Math.min(rawX, window.innerWidth - 150)),
      y: Math.max(8, Math.min(rawY, window.innerHeight - 140)),
      index: safeIndex,
      trigger: target,
      view: { kind: "insert" }
    });
  }, [mutationReadOnly]);

  const renderTimelineNode = (node: ContextRenderNode): ReactNode => {
    if (node.kind === "block") {
      // A block draws several contexts as rows, so an insert index anywhere
      // inside it is answered by the one editor drawn ahead of the block.
      const first = Math.min(...node.entries.map((entry) => entry.index));
      const after = Math.max(...node.entries.map((entry) => entry.index)) + 1;
      return (
        <div className="context-slot" key={node.key}>
          {insertEditorIndex !== null && insertEditorIndex >= first && insertEditorIndex < after && insertEditor}
          <TimelineBlock
            entries={node.entries}
            tools={tools}
            insertionIndex={displayInsertionIndex}
            readOnly={mutationReadOnly}
            editingContextId={editingContextId}
            onEdit={onEdit}
            onDelete={onDelete}
            onCancelEdit={onCancelEdit}
            onSaveText={onSaveText}
            onSaveTool={onSaveTool}
            onSaveToolEdit={onSaveToolEdit}
            onOpenInsert={openMenu}
            onOpenSubagent={onOpenSubagent}
            workflowRunByCall={workflowRunByCall}
            // Read-only transcripts have no task container to bring forward.
            onOpenWorkflowRun={readOnly ? undefined : onOpenWorkflowRun}
            deferOffscreen={after < contexts.length - 4}
            pathBaseDir={pathBaseDir}
          />
        </div>
      );
    }

    if (node.kind === "question") {
      if (node.entry.item.id === pendingQuestionId) return null;
      return (
        <div className="context-slot" key={node.key}>
          {insertEditorIndex === node.entry.index && insertEditor}
          {!mutationReadOnly && displayInsertionIndex === node.entry.index && <div className="insertion-line" />}
          <QuestionTimelineCard
            item={node.entry.item}
            index={node.entry.index}
            answer={node.answer?.item}
            answerIndex={node.answer?.index}
            readOnly={mutationReadOnly}
            editing={editingQuestionId === node.entry.item.id}
            onEdit={onEditQuestion}
            onDelete={onDeleteQuestion}
            onCancelEdit={onCancelEdit}
            onSaveQuestion={onSaveQuestion}
            onOpenInsert={openMenu}
          />
        </div>
      );
    }

    return (
      <div className="context-slot" key={node.item.id}>
        {insertEditorIndex === node.index && insertEditor}
        {!mutationReadOnly && displayInsertionIndex === node.index && <div className="insertion-line" />}
        <ContextCard
          item={node.item}
          index={node.index}
          readOnly={readOnly}
          mutationReadOnly={mutationReadOnly}
          editing={editingContextId === node.item.id}
          onCancelEdit={onCancelEdit}
          onSaveText={onSaveText}
          onAddAttachments={onAddAttachments}
          attachmentImageInput={attachmentImageInput}
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

  /**
   * What a round still draws for itself, now that its messages are ordinary
   * timeline nodes: the sign that it is still running, and the notice saying
   * why it stopped.
   */
  const renderTurnNotices = (turn: ConversationTurn): ReactNode => (
    <Fragment key={`turn:${turn.id}`}>
      {streaming && turn.status === "running" && (
        <StreamWaitingIndicator contexts={contexts} tools={tools} thinking={thinking} retryNotice={retryNotice} />
      )}
      {turn.error && (
        <TurnErrorNotice
          error={turn.error}
          onRetry={
            onRetryTurnError && retryableTurnRequestId === turn.requestId
              ? onRetryTurnError
              : undefined
          }
          onDismiss={onDismissTurnError}
        />
      )}
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
        const top = scroller.scrollTop;
        const previousTop = scrollTopRef.current;
        const sameTimeline = scrollTimelineRef.current === timelineId;
        scrollTopRef.current = top;
        scrollTimelineRef.current = timelineId;
        const distance = scroller.scrollHeight - top - scroller.clientHeight;
        // Resting at the bottom re-attaches; any upward move detaches, so one
        // wheel notch escapes follow-output instead of being pulled back.
        if (distance <= SCROLL_BOTTOM_EPSILON) scrollPinnedRef.current = true;
        else if (!sameTimeline) scrollPinnedRef.current = distance < 180;
        else if (top < previousTop) scrollPinnedRef.current = false;
      }}
      onContextMenu={mutationReadOnly ? undefined : (event) => {
        const target = event.target as HTMLElement;
        if (target.closest(TIMELINE_ANCHOR_SELECTOR)) return;
        event.preventDefault();
        const anchors = Array.from(event.currentTarget.querySelectorAll<HTMLElement>(TIMELINE_ANCHOR_SELECTOR));
        const nearest = anchors.find((anchor) => {
          const box = anchor.getBoundingClientRect();
          return event.clientY < box.top + box.height / 2;
        });
        const nearestIndex = nearest ? Number(nearest.dataset.contextIndex) : contexts.length;
        openMenu(event, Number.isInteger(nearestIndex) ? nearestIndex : contexts.length);
      }}
    >
      <div className="context-stream">
        {!insertEditor && renderNodes.length === 0 && turnProjection.leadingTurns.length === 0 ? (
          <EmptyState
            icon={<MessageSquare size={22} />}
            title={t("这段对话还没有消息", "This conversation has no messages yet")}
            description={t("会话内容会显示在这里。", "Conversation content will appear here.")}
          />
        ) : (
          <>
            {turnProjection.leadingTurns.map(renderTurnNotices)}
            {renderNodes.map((node) => {
              const key = renderNodeKey(node);
              const notices = turnProjection.noticesAfterNode.get(key) ?? [];
              return (
                <Fragment key={`timeline:${key}`}>
                  {renderTimelineNode(node)}
                  {notices.map(renderTurnNotices)}
                </Fragment>
              );
            })}
          </>
        )}
        {streaming && turnProjection.visibleRunningTurnIds.size === 0 && (
          <StreamWaitingIndicator contexts={contexts} tools={tools} thinking={thinking} retryNotice={retryNotice} />
        )}
        {insertEditorIndex === contexts.length && insertEditor}
        {!mutationReadOnly && displayInsertionIndex === contexts.length && renderNodes.length > 0 && <div className="insertion-line" />}
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
              // One level at a time: a submenu opened by mistake is closed
              // without losing the menu it was opened from.
              const openTools = menu.view.kind === "insert" && menu.view.tools;
              if (openTools) {
                setMenu((current) => (current && current.view.kind === "insert"
                  ? {
                    ...current,
                    view: openTools.category
                      ? { kind: "insert", tools: { category: null } }
                      : { kind: "insert" }
                  }
                  : current));
                return;
              }
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
          {(Object.keys(contextMeta) as InsertableContextKind[]).map((kind) => {
            const item = contextMeta[kind];
            // A timeline with no way to commit a placed card does not offer
            // to place one. Being unable to *execute* it is not that: a
            // template has nothing to execute against and still holds calls.
            if (kind === "tool" && !onSaveTool && !onSaveToolEdit) return null;
            // A call is placed by naming the tool, which is one choice more
            // than the other kinds need, so this row opens a list beside
            // itself instead of acting.
            if (kind !== "tool") {
              return (
                <button
                  type="button"
                  role="menuitem"
                  key={kind}
                  onClick={() => {
                    onInsert?.(menu.index, kind);
                    setMenu(null);
                  }}
                >
                  {item.label(t)}
                </button>
              );
            }
            const openTools = menu.view.kind === "insert" ? menu.view.tools : undefined;
            const disabled = insertableToolGroups.length === 0;
            return (
              <div className="context-menu__branch" key={kind}>
                <button
                  type="button"
                  role="menuitem"
                  disabled={disabled}
                  aria-haspopup="menu"
                  aria-expanded={Boolean(openTools)}
                  title={disabled ? t("当前对话没有启用工具", "No tools are enabled in this conversation") : undefined}
                  onClick={() => setMenu((current) => (current && current.view.kind === "insert"
                    ? {
                      ...current,
                      view: current.view.tools ? { kind: "insert" } : { kind: "insert", tools: { category: null } }
                    }
                    : current))}
                >
                  {item.label(t)}
                  <ChevronRight className="context-menu__more" size={12} aria-hidden="true" />
                </button>
                {openTools && (
                  <ContextSubmenu label={t("工具调用", "Tool call")}>
                    {insertableToolGroups.map((group) => {
                      const label = groupLabel(group.category, t);
                      const open = openTools.category === group.category;
                      return (
                        <div className="context-menu__branch" key={group.category}>
                          <button
                            type="button"
                            role="menuitem"
                            aria-haspopup="menu"
                            aria-expanded={open}
                            onClick={() => setMenu((current) => (current && current.view.kind === "insert"
                              ? {
                                ...current,
                                view: { kind: "insert", tools: { category: open ? null : group.category } }
                              }
                              : current))}
                          >
                            {label}
                            <ChevronRight className="context-menu__more" size={12} aria-hidden="true" />
                          </button>
                          {open && (
                            <ContextSubmenu label={label} scrollable>
                              {group.tools.map((tool) => (
                                <button
                                  type="button"
                                  role="menuitem"
                                  key={tool.name}
                                  title={tool.name}
                                  onClick={() => {
                                    onInsert?.(menu.index, "tool", tool.name);
                                    setMenu(null);
                                  }}
                                >
                                  {tool.label}
                                </button>
                              ))}
                            </ContextSubmenu>
                          )}
                        </div>
                      );
                    })}
                  </ContextSubmenu>
                )}
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
});
