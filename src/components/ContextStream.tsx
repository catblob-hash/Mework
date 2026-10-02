import {
  Bot,
  BrainCircuit,
  Check,
  ChevronDown,
  ChevronLeft,
  ChevronRight,
  ChevronUp,
  CircleAlert,
  Copy,
  GitBranch,
  Pencil,
  Shield,
  Trash2,
  UserRound,
  Wrench,
  X
} from "lucide-react";
import { Fragment, memo, useCallback, useEffect, useId, useLayoutEffect, useMemo, useRef, useState } from "react";
import type { ReactNode } from "react";
import { useI18n } from "../i18n";
import type { AssistantContext, ContextItem, FileAttachment, ImageAttachment, InsertableContextKind, JsonObject, SystemContext, ToolContext, ToolDescriptor, UserContext } from "../types";
import type { AddMessageAttachments } from "../lib/fileAttachments";
import type { ContextBranchNavigation } from "../lib/conversationBranches";
import { turnAnchorIndex } from "../lib/conversationTurns";
import type { ConversationTurn } from "../lib/conversationTurns";
import type { WorkflowRunView } from "../lib/workflowRuns";
import type { LiveReasoningView } from "../lib/runContexts";
import { CopyButton, EmptyState, IconButton } from "./Common";
import { MenuFlyout, MenuSurfacesContext, createMenuSurfaces } from "./MenuFlyout";
import { MeworkIcon } from "./MeworkIcon";
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
import { stepFollowGlide } from "../lib/followGlide";
import type { FollowGlide } from "../lib/followGlide";
import { MarkdownContent } from "./MarkdownContent";
import { QuestionTimelineCard } from "./QuestionTimelineCard";
import { TimelineSelectionContext, useTimelineSelected } from "./timelineSelection";
import { timelineHistoryKey, timelineSelectionModifier } from "../lib/timelineHistory";
import { StreamWaitingIndicator } from "./StreamWaitingIndicator";
import { groupLabel, groupMeta } from "./ToolSelectionGroups";
import type { ToolCategory } from "./ToolSelectionGroups";
import { isPreviewToolName } from "../lib/taskTools";
import { ImageStrip } from "./ImageStrip";
import { summarizeSpanChanges, TurnChanges, turnChangeSpans } from "./TurnChanges";
import type { ChangeSpan, TurnChangeSummary } from "./TurnChanges";

// Wide enough to absorb fractional scroll metrics, narrow enough that the user
// has to actually be at the bottom to re-attach to follow-output.
const SCROLL_BOTTOM_EPSILON = 16;

const REDUCED_MOTION_QUERY = "(prefers-reduced-motion: reduce)";

/** Keys that scroll a page up when nothing editable has them. */
const UPWARD_SCROLL_KEYS = new Set(["ArrowUp", "PageUp", "Home"]);

/**
 * Whether scrolling up from `target` — a wheel over it, or a key while it has
 * focus — moves `scroller` itself, rather than a code block or an output pane
 * inside it that still has room to scroll.
 */
function upwardScrollMovesScroller(target: EventTarget | null, scroller: HTMLElement): boolean {
  for (let element = target instanceof Element ? target : null; element && element !== scroller; element = element.parentElement) {
    if (element.scrollHeight <= element.clientHeight || element.scrollTop <= 0) continue;
    const overflow = window.getComputedStyle(element).overflowY;
    if (overflow === "auto" || overflow === "scroll") return false;
  }
  return scroller.scrollTop > 0;
}

function isEditableTarget(target: EventTarget | null): boolean {
  return target instanceof HTMLElement
    && (target.isContentEditable || ["INPUT", "TEXTAREA", "SELECT"].includes(target.tagName));
}

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
  /**
   * Finished stretches of work whose changed files are listed at their end. The
   * finished `turns` when absent; a transcript without turn records — a
   * subagent's — derives its own.
   */
  changeSpans?: ChangeSpan[];
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
  /** Rows the tasks pane lists as running, narrated beside the cat as a link to that pane. */
  runningTaskCount?: number;
  onOpenTasks?: () => void;
  ariaLabel?: string;
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
  /**
   * Deletes every context a selection box picked out, as one edit. The box is
   * drawn by dragging with Ctrl held (⌘ on a Mac), and only where this is given.
   */
  onDeleteContexts?: (ids: string[]) => void;
  /**
   * Reverts the last edit made on this timeline. Ctrl+Z (⌘Z on a Mac) asks for
   * it, and only while focus is somewhere in the timeline: the same keys typed
   * into a field undo the typing instead.
   */
  onUndo?: () => void;
  /** Makes the last reverted edit again: Ctrl+X (⌘X), on the same terms. */
  onRedo?: () => void;
  /** Opens a new conversation holding the contexts above the insertion line at `index`. */
  onForkAt?: (index: number) => void;
  forkDisabledReason?: string | null;
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

/**
 * A reply's controls. Copying changes nothing, so it stays on a timeline that is
 * read-only or busy running; editing and deleting come only where they can land.
 */
function ContextActions({
  copyText,
  copyLabel,
  copyDisabled = false,
  onEdit,
  onDelete,
  mutationDisabled = false
}: {
  copyText?: string;
  copyLabel: string;
  copyDisabled?: boolean;
  onEdit?: () => void;
  onDelete?: () => void;
  mutationDisabled?: boolean;
}) {
  const { t } = useI18n();
  return (
    <div className="context-actions">
      {copyText ? <CopyButton text={copyText} label={copyLabel} size={14} disabled={copyDisabled} /> : null}
      {onEdit && (
        <IconButton label={t("编辑上下文", "Edit context")} onClick={onEdit} disabled={mutationDisabled}>
          <Pencil size={14} />
        </IconButton>
      )}
      {onDelete && (
        <IconButton label={t("删除上下文", "Delete context")} onClick={onDelete} disabled={mutationDisabled}>
          <Trash2 size={14} />
        </IconButton>
      )}
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

/** How many lines a long message shows while folded (`.context-card__content--folded` agrees). */
const FOLDED_USER_MESSAGE_LINES = 10;
/** A message only this much longer than its folded height shows whole: folding would hide next to nothing. */
const USER_MESSAGE_FOLD_SLACK_LINES = 3;

function lineHeightOf(element: HTMLElement): number {
  const style = window.getComputedStyle(element);
  const lineHeight = Number.parseFloat(style.lineHeight);
  if (style.lineHeight.endsWith("px") && lineHeight > 0) return lineHeight;
  const fontSize = Number.parseFloat(style.fontSize);
  return (style.fontSize.endsWith("px") && fontSize > 0 ? fontSize : 13)
    * (lineHeight > 0 && lineHeight < 4 ? lineHeight : 1.58);
}

/**
 * A user message's text. One long enough to push the conversation out of view
 * waits folded to its first lines under "Show more", and "Show less" folds it
 * again. Wrapping decides what is long, so it is measured, not counted.
 */
function FoldableUserText({ measureKey, children }: { measureKey: string; children: ReactNode }) {
  const { t } = useI18n();
  const contentRef = useRef<HTMLDivElement>(null);
  const toggleRef = useRef<HTMLButtonElement>(null);
  const contentId = useId();
  const [foldable, setFoldable] = useState(false);
  const [expanded, setExpanded] = useState(false);

  // biome-ignore lint/correctness/useExhaustiveDependencies: `measureKey` is what changes the height `measure` reads.
  useLayoutEffect(() => {
    const content = contentRef.current;
    if (!content) return;
    // `scrollHeight` is the whole text's height even while folded, so folding
    // never changes the answer and the observer cannot feed back on itself.
    const measure = () => setFoldable(
      content.scrollHeight > lineHeightOf(content) * (FOLDED_USER_MESSAGE_LINES + USER_MESSAGE_FOLD_SLACK_LINES)
    );
    measure();
    if (typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(measure);
    observer.observe(content);
    return () => observer.disconnect();
  }, [measureKey]);

  const folded = foldable && !expanded;
  return (
    <>
      <div
        ref={contentRef}
        id={contentId}
        className={`context-card__content${folded ? " context-card__content--folded" : ""}`}
      >
        {children}
      </div>
      {foldable && (
        <button
          ref={toggleRef}
          type="button"
          className="context-card__fold"
          aria-expanded={expanded}
          aria-controls={contentId}
          onClick={() => {
            setExpanded(!expanded);
            // Folding a message read to its end leaves the reader below it;
            // bring the toggle, and the message's end, back into view.
            if (expanded) window.requestAnimationFrame(() => toggleRef.current?.scrollIntoView({ block: "nearest" }));
          }}
        >
          {expanded ? <ChevronUp size={13} /> : <ChevronDown size={13} />}
          <span>{expanded ? t("收起", "Show less") : t("展开", "Show more")}</span>
        </button>
      )}
    </>
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
  // The system prompt's own row lights up for it, so the card around that row does not.
  const selected = useTimelineSelected(item.id) && !promptCard;
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
      data-timeline-selected={selected || undefined}
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
          actions={promptEditing || (!showMutationActions && item.content.length === 0) ? undefined : (
            <>
              {item.content.length > 0 && <CopyButton text={item.content} label={t("复制系统提示词", "Copy system prompt")} />}
              {showMutationActions && (
                <>
                  <IconButton label={t("编辑上下文", "Edit context")} onClick={onEdit} disabled={mutationReadOnly}>
                    <Pencil size={13} />
                  </IconButton>
                  <IconButton label={t("删除上下文", "Delete context")} onClick={onDelete} disabled={mutationReadOnly}>
                    <Trash2 size={13} />
                  </IconButton>
                </>
              )}
            </>
          )}
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
      {!editingInPlace && item.kind === "assistant" && (
        <div className="context-card__content" aria-live={assistantStreaming ? "polite" : undefined}>
          <MarkdownContent
            content={displayContent}
            deferOffscreen={deferOffscreen}
            streaming={assistantStreaming}
            linkifyPaths
            renderHtml
            pathBaseDir={pathBaseDir}
          />
        </div>
      )}
      {!editingInPlace && item.kind === "user" && Boolean(displayContent) && (
        <FoldableUserText measureKey={`${renderUserMarkdown ? "md" : "text"}:${displayContent}`}>
          {renderUserMarkdown
            ? (
              // Only model output is scanned for paths and has its HTML
              // rendered. User text keeps rendering exactly what was typed,
              // Markdown preference or not.
              <MarkdownContent
                content={displayContent}
                deferOffscreen={deferOffscreen}
                linkifyPaths={false}
                renderHtml={false}
                pathBaseDir={pathBaseDir}
              />
            )
            : displayContent}
        </FoldableUserText>
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
      {item.kind === "assistant" && !editingInPlace && (showMutationActions || Boolean(item.content)) && (
        <div className="context-card__footer-actions">
          <ContextActions
            copyText={item.content}
            copyLabel={t("复制模型回复", "Copy model reply")}
            copyDisabled={assistantStreaming}
            onEdit={showMutationActions ? onEdit : undefined}
            onDelete={showMutationActions ? onDelete : undefined}
            mutationDisabled={mutationReadOnly || assistantStreaming}
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
   * that one opens beside the item rather than replacing what it came from. A
   * right-click while a selection box has picked contexts out offers only what
   * can be done to all of them at once.
   */
  view: { kind: "insert"; tools?: { category: ToolCategory | null } } | { kind: "selection" };
}

const NO_SELECTION: ReadonlySet<string> = new Set();

/** How close to the top or bottom edge a selection drag scrolls the timeline. */
const MARQUEE_SCROLL_EDGE = 32;
/** The most a selection drag scrolls the timeline per frame. */
const MARQUEE_SCROLL_STEP = 18;

/** A selection drag in progress: where it began, in the timeline's own coordinates, and where the pointer is. */
interface MarqueeDrag {
  pointerId: number;
  startX: number;
  /** Measured from the top of the scrolled content, so scrolling carries the box's far edge, not its start. */
  startContentY: number;
  x: number;
  y: number;
  frame: number | null;
  /** Ends the drag and takes its listeners down. */
  release?: () => void;
}

function sameIds(left: ReadonlySet<string>, right: ReadonlySet<string>): boolean {
  if (left.size !== right.size) return false;
  for (const id of left) if (!right.has(id)) return false;
  return true;
}

/**
 * A panel that opens beside the item that owns it: a panel of its own beside the menu
 * (`MenuFlyout`), which moves to the other side of the menu, or up, rather than leave the
 * window. The arrows move through it; Escape goes back a level through the menu's handler.
 */
function ContextSubmenu({ label, scrollable = false, children }: {
  label: string;
  /** A long list scrolls; see the stylesheet. */
  scrollable?: boolean;
  children: ReactNode;
}) {
  return (
    <MenuFlyout
      role="menu"
      aria-label={label}
      className={`context-menu__submenu${scrollable ? " context-menu__submenu--scroll" : ""}`}
      autoFocus
      onKeyDown={(event) => {
        if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return;
        event.preventDefault();
        event.stopPropagation();
        // Its own rows, not a nested submenu's, which sits beside it and handles its own.
        const buttons = Array.from(event.currentTarget.querySelectorAll<HTMLButtonElement>(
          ":scope > button:not(:disabled), :scope > .context-menu__branch > button:not(:disabled)"
        ));
        const current = buttons.indexOf(document.activeElement as HTMLButtonElement);
        const direction = event.key === "ArrowDown" ? 1 : -1;
        buttons[(current + direction + buttons.length) % buttons.length]?.focus();
      }}
    >
      {children}
    </MenuFlyout>
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

export const ContextStream = memo(function ContextStream({ contexts, turns = [], changeSpans: changeSpansProp, tools, enabledTools, timelineId, readOnly = false, timelineMutationLocked = false, streaming = false, thinking = null, retryNotice = null, runningTaskCount = 0, onOpenTasks, ariaLabel, onEdit, onDelete, onEditQuestion, onDeleteQuestion, editor = null, questionEditor = null, onCancelEdit, onSaveText, onAddAttachments, attachmentImageInput, onSaveTool, onSaveToolEdit, onSaveQuestion, onBranchFrom, branchFromDisabledReason, branchNavigations, onSelectBranch, branchSwitchDisabledReason, onInsert, onDeleteContexts, onUndo, onRedo, onForkAt, forkDisabledReason = null, onOpenSubagent, workflowRunByCall, onOpenWorkflowRun, onRetryTurnError, retryableTurnRequestId = null, onDismissTurnError, pathBaseDir = null }: ContextStreamProps) {
  const { t } = useI18n();
  const [menu, setMenu] = useState<ContextMenuState | null>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  const [menuSurfaces] = useState(createMenuSurfaces);
  const streamRef = useRef<HTMLDivElement>(null);
  const scrollPinnedRef = useRef(true);
  const scrollTopRef = useRef(0);
  const scrollTimelineRef = useRef(timelineId);
  const scrollFrameRef = useRef<number | null>(null);
  const glideRef = useRef<FollowGlide | null>(null);
  const streamContentRef = useRef<HTMLDivElement>(null);
  const previousTimelineRef = useRef<{
    timelineId: string | undefined;
    length: number;
    lastId: string | null;
    lastVisualLength: number;
  } | null>(null);
  const mutationReadOnly = readOnly || timelineMutationLocked;
  const [selection, setSelection] = useState<ReadonlySet<string>>(NO_SELECTION);
  /** The selection box as drawn: the drag's rectangle, clipped to the visible timeline. */
  const [marquee, setMarquee] = useState<{ left: number; top: number; width: number; height: number } | null>(null);
  const marqueeRef = useRef<MarqueeDrag | null>(null);
  const selecting = Boolean(onDeleteContexts) && !mutationReadOnly;

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
  const nodeLayout = useMemo(() => {
    const nodesWithIds = renderNodes.map((node) => ({
      node,
      ids: renderNodeContextIds(node),
      key: renderNodeKey(node)
    }));
    const nodeIndexByContextId = new Map<string, number>();
    nodesWithIds.forEach(({ ids }, index) => {
      ids.forEach((id) => nodeIndexByContextId.set(id, index));
    });
    return { nodesWithIds, nodeIndexByContextId };
  }, [renderNodes]);
  /** An answered question is one card for two contexts, so picking the card picks both. */
  const questionAnswerIds = useMemo(() => new Map(renderNodes.flatMap((node) => (
    node.kind === "question" && node.answer ? [[node.entry.item.id, node.answer.item.id] as const] : []
  ))), [renderNodes]);
  /** The selection against the list as it is now: a context deleted from under it is no longer picked. */
  const selectedIds = useMemo(() => {
    if (!selection.size) return NO_SELECTION;
    const live = new Set([...selection].filter((id) => contextIndexes.has(id)));
    return live.size === selection.size ? selection : live;
  }, [contextIndexes, selection]);
  /** The drawn node holding the newest of `ids`, which is where a span ends on screen. */
  const lastDrawnNodeOf = useCallback((ids: readonly string[]) => ids
    .flatMap((id) => {
      const index = nodeLayout.nodeIndexByContextId.get(id);
      return index === undefined ? [] : [index];
    })
    .reduce<number | undefined>(
      (last, index) => (last === undefined || index > last ? index : last),
      undefined
    ), [nodeLayout]);
  const turnProjection = useMemo(() => {
    const noticesAfterNode = new Map<string, ConversationTurn[]>();
    const leadingTurns: ConversationTurn[] = [];
    const visibleRunningTurnIds = new Set<string>();
    const { nodesWithIds, nodeIndexByContextId } = nodeLayout;
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
      const lastOwnedNode = lastDrawnNodeOf(turn.contextIds);
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
  }, [contextIndexes, contexts, lastDrawnNodeOf, nodeLayout, turns]);
  const changeSpans = useMemo(
    () => changeSpansProp ?? turnChangeSpans(turns),
    [changeSpansProp, turns]
  );
  /**
   * Each finished span's changed files, after the last node it drew. A span whose
   * work is no longer on screen — its calls deleted — has nothing to list.
   */
  const changesAfterNode = useMemo(() => {
    const byNode = new Map<string, TurnChangeSummary[]>();
    const contextFor = (id: string) => {
      const index = contextIndexes.get(id);
      return index === undefined ? undefined : contexts[index];
    };
    for (const span of changeSpans) {
      const summary = summarizeSpanChanges(span, contextFor);
      if (!summary) continue;
      const lastNode = lastDrawnNodeOf(span.contextIds);
      if (lastNode === undefined) continue;
      const key = nodeLayout.nodesWithIds[lastNode].key;
      byNode.set(key, [...(byNode.get(key) ?? []), summary]);
    }
    return byNode;
  }, [changeSpans, contextIndexes, contexts, lastDrawnNodeOf, nodeLayout]);
  const renderedContextIndexes = useMemo(() => renderNodes.flatMap((node) => {
    if (node.kind === "context") return [node.index];
    if (node.kind === "question") {
      return node.answer ? [node.entry.index, node.answer.index] : [node.entry.index];
    }
    return node.entries.map((entry) => entry.index);
  }).sort((left, right) => left - right), [renderNodes]);
  const displayInsertionIndex = menu === null || menu.view.kind !== "insert"
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
      // A submenu is a panel of its own beside the menu, not inside it.
      if (event.target instanceof Node && (menuRef.current?.contains(event.target) || menuSurfaces.contains(event.target))) return;
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
  }, [menu, menuSurfaces, mutationReadOnly]);

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

  /** Stops any follow-output frame, jump or glide, before it runs. */
  const stopFollowing = useCallback(() => {
    if (scrollFrameRef.current !== null) window.cancelAnimationFrame(scrollFrameRef.current);
    scrollFrameRef.current = null;
    glideRef.current = null;
  }, []);

  /** Takes the page to the bottom in one step on the next frame. */
  const jumpToBottom = useCallback(() => {
    stopFollowing();
    scrollFrameRef.current = window.requestAnimationFrame(() => {
      scrollFrameRef.current = null;
      const current = streamRef.current;
      if (!current || !scrollPinnedRef.current) return;
      current.scrollTop = current.scrollHeight;
      // The next scroll event compares against where follow-output left the
      // reader, so a jump the browser never reported cannot read as an upward
      // scroll and detach on its own.
      scrollTopRef.current = current.scrollTop;
    });
  }, [stopFollowing]);

  /**
   * Glides the page down to the bottom (`followGlide.ts`). A glide already
   * under way just carries on: it reads the bottom afresh every frame, so a
   * commit landing mid-glide re-aims it rather than restarting it.
   */
  const glideToBottom = useCallback(() => {
    const scroller = streamRef.current;
    if (glideRef.current || !scroller) return;
    if (window.matchMedia?.(REDUCED_MOTION_QUERY).matches) {
      jumpToBottom();
      return;
    }
    stopFollowing();
    glideRef.current = { position: scroller.scrollTop, velocity: 0, time: null };
    const frame = (now: number) => {
      scrollFrameRef.current = null;
      const current = streamRef.current;
      const glide = glideRef.current;
      if (!current || !glide || !scrollPinnedRef.current) {
        glideRef.current = null;
        return;
      }
      // Something else moved the page — the browser clamping a page that got
      // shorter, or the reader scrolling further down — so go on from there.
      const position = Math.abs(current.scrollTop - glide.position) > 1 ? current.scrollTop : glide.position;
      const next = stepFollowGlide({ ...glide, position }, Math.max(0, current.scrollHeight - current.clientHeight), now);
      current.scrollTop = next.position;
      scrollTopRef.current = current.scrollTop;
      if (next.done) {
        glideRef.current = null;
        return;
      }
      glideRef.current = next;
      scrollFrameRef.current = window.requestAnimationFrame(frame);
    };
    scrollFrameRef.current = window.requestAnimationFrame(frame);
  }, [jumpToBottom, stopFollowing]);

  /** The reader is heading up: let go of the bottom before the next frame pulls them back. */
  const releaseBottom = useCallback(() => {
    scrollPinnedRef.current = false;
    stopFollowing();
  }, [stopFollowing]);

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
    const timelineChanged = previous !== null && previous.timelineId !== next.timelineId;
    // The owning conversation surface restores its saved scroll position after
    // a switch. Do not reinterpret the new timeline as appended output.
    if (timelineChanged || !scroller || contexts.length === 0 || menu || !scrollPinnedRef.current) {
      stopFollowing();
      return;
    }

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
    if (!initialTimeline && !appended && !growingTail && !streamingUpdate) {
      // A glide may finish the distance it was already covering, but never
      // past a tail that went away: it could outlive the deletion and close a
      // context menu opened right after it.
      if (!glideRef.current || next.length < previous.length) stopFollowing();
      return;
    }
    // What a stream writes glides, and so does whatever lands while a glide is
    // still moving, the stream's own settling included. Anything else — a
    // conversation opening, a card placed by hand — jumps.
    if (!initialTimeline && (streaming || glideRef.current)) glideToBottom();
    else jumpToBottom();
  }, [contexts, glideToBottom, jumpToBottom, menu, stopFollowing, streaming, timelineId]);

  // Commits are not the only thing that grows a live page: a body opening
  // under its row, a formula typesetting, the waiting line changing shape.
  // While the stream runs, whatever grows the page is followed the same way.
  useEffect(() => {
    const content = streamContentRef.current;
    if (!streaming || menu || !content || typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(() => {
      // Until the page reports a scroll on this timeline, its position is the
      // one the owning surface is restoring, not one to follow from.
      if (scrollPinnedRef.current && scrollTimelineRef.current === timelineId) glideToBottom();
    });
    observer.observe(content);
    return () => observer.disconnect();
  }, [glideToBottom, menu, streaming, timelineId]);

  useEffect(() => stopFollowing, [stopFollowing]);

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

  /** The contexts whose cards or rows a rectangle in client coordinates touches. */
  const idsInBox = (box: { left: number; top: number; right: number; bottom: number }): ReadonlySet<string> => {
    const content = streamContentRef.current;
    const ids = new Set<string>();
    if (!content) return ids;
    for (const element of content.querySelectorAll<HTMLElement>("[data-context-id]")) {
      const id = element.dataset.contextId;
      // A card still being placed has an id no context has yet.
      if (!id || !contextIndexes.has(id)) continue;
      const rect = element.getBoundingClientRect();
      if (!rect.width && !rect.height) continue;
      if (rect.right < box.left || rect.left > box.right || rect.bottom < box.top || rect.top > box.bottom) continue;
      ids.add(id);
      const answer = questionAnswerIds.get(id);
      if (answer) ids.add(answer);
    }
    return ids;
  };

  /**
   * Redraws the selection box from where the drag began to where the pointer is,
   * and picks out what it touches. Held against the top or bottom edge, the drag
   * scrolls the timeline on and keeps stepping each frame, so a box can reach
   * past what was on screen when it began.
   */
  const stepMarquee = (final = false) => {
    const drag = marqueeRef.current;
    const scroller = streamRef.current;
    if (!drag || !scroller) return;
    drag.frame = null;
    const bounds = scroller.getBoundingClientRect();
    let scrolled = false;
    if (!final) {
      const step = drag.y < bounds.top + MARQUEE_SCROLL_EDGE
        ? -Math.min(MARQUEE_SCROLL_STEP, bounds.top + MARQUEE_SCROLL_EDGE - drag.y)
        : drag.y > bounds.bottom - MARQUEE_SCROLL_EDGE
          ? Math.min(MARQUEE_SCROLL_STEP, drag.y - (bounds.bottom - MARQUEE_SCROLL_EDGE))
          : 0;
      if (step) {
        const before = scroller.scrollTop;
        scroller.scrollTop = before + step;
        scrolled = scroller.scrollTop !== before;
      }
    }
    const startY = drag.startContentY - scroller.scrollTop + bounds.top;
    const box = {
      left: Math.min(drag.startX, drag.x),
      right: Math.max(drag.startX, drag.x),
      top: Math.min(startY, drag.y),
      bottom: Math.max(startY, drag.y)
    };
    const left = Math.max(box.left, bounds.left);
    const top = Math.max(box.top, bounds.top);
    setMarquee({
      left,
      top,
      width: Math.max(0, Math.min(box.right, bounds.right) - left),
      height: Math.max(0, Math.min(box.bottom, bounds.bottom) - top)
    });
    const picked = idsInBox(box);
    setSelection((current) => (sameIds(current, picked) ? current : picked));
    if (scrolled) drag.frame = window.requestAnimationFrame(() => stepMarqueeRef.current());
  };
  const stepMarqueeRef = useRef(stepMarquee);
  stepMarqueeRef.current = stepMarquee;

  /**
   * Starts a selection box at a press with the selection modifier held. The
   * press is the box's and nothing else's: it starts no text selection, no
   * native drag, and the click it ends in reaches nothing underneath.
   */
  const beginMarquee = (event: React.PointerEvent<HTMLDivElement>) => {
    const scroller = event.currentTarget;
    const bounds = scroller.getBoundingClientRect();
    // A press on the scrollbar is scrolling.
    if (event.clientX - bounds.left - scroller.clientLeft >= scroller.clientWidth) return;
    event.preventDefault();
    marqueeRef.current?.release?.();
    window.getSelection()?.removeAllRanges();
    setMenu(null);
    const drag: MarqueeDrag = {
      pointerId: event.pointerId,
      startX: event.clientX,
      startContentY: event.clientY - bounds.top + scroller.scrollTop,
      x: event.clientX,
      y: event.clientY,
      frame: null
    };
    const schedule = () => {
      if (drag.frame === null) drag.frame = window.requestAnimationFrame(() => stepMarqueeRef.current());
    };
    const move = (moved: PointerEvent) => {
      if (moved.pointerId !== drag.pointerId) return;
      drag.x = moved.clientX;
      drag.y = moved.clientY;
      schedule();
    };
    const finish = (ended: PointerEvent) => {
      if (ended.pointerId !== drag.pointerId) return;
      drag.x = ended.clientX;
      drag.y = ended.clientY;
      stepMarqueeRef.current(true);
      drag.release?.();
    };
    const swallow = (swallowed: Event) => {
      swallowed.preventDefault();
      swallowed.stopPropagation();
    };
    const prevent = (prevented: Event) => prevented.preventDefault();
    window.addEventListener("pointermove", move, true);
    window.addEventListener("pointerup", finish, true);
    window.addEventListener("pointercancel", finish, true);
    window.addEventListener("click", swallow, true);
    window.addEventListener("selectstart", prevent, true);
    window.addEventListener("dragstart", prevent, true);
    drag.release = () => {
      if (drag.frame !== null) window.cancelAnimationFrame(drag.frame);
      drag.frame = null;
      window.removeEventListener("pointermove", move, true);
      window.removeEventListener("pointerup", finish, true);
      window.removeEventListener("pointercancel", finish, true);
      window.removeEventListener("selectstart", prevent, true);
      window.removeEventListener("dragstart", prevent, true);
      // The click comes right after the release, in the same turn; nothing later is its.
      window.setTimeout(() => window.removeEventListener("click", swallow, true), 0);
      if (marqueeRef.current === drag) marqueeRef.current = null;
      setMarquee(null);
    };
    marqueeRef.current = drag;
    stepMarqueeRef.current(true);
  };

  useEffect(() => () => marqueeRef.current?.release?.(), []);

  // A selection belongs to the timeline it was drawn on, and only lasts while it
  // can still be acted on.
  // biome-ignore lint/correctness/useExhaustiveDependencies: `timelineId` is the switch that ends the selection, though nothing here reads it.
  useEffect(() => {
    marqueeRef.current?.release?.();
    setSelection(NO_SELECTION);
  }, [timelineId]);
  useEffect(() => {
    if (selecting) return;
    marqueeRef.current?.release?.();
    setSelection(NO_SELECTION);
  }, [selecting]);
  useEffect(() => {
    if (!selectedIds.size) setMenu((current) => (current?.view.kind === "selection" ? null : current));
  }, [selectedIds]);

  const openSelectionMenu = (event: React.MouseEvent) => {
    setMenu({
      x: Math.max(8, Math.min(event.clientX, window.innerWidth - 150)),
      y: Math.max(8, Math.min(event.clientY, window.innerHeight - 60)),
      index: 0,
      trigger: event.target instanceof HTMLElement ? event.target : null,
      view: { kind: "selection" }
    });
  };

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
        <StreamWaitingIndicator
          contexts={contexts}
          tools={tools}
          thinking={thinking}
          retryNotice={retryNotice}
          runningTaskCount={runningTaskCount}
          onOpenTasks={onOpenTasks}
        />
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
    <TimelineSelectionContext.Provider value={selectedIds}>
    <div
      ref={streamRef}
      className="context-scroll"
      data-main-context-stream={!readOnly || undefined}
      data-selecting={marquee ? "true" : undefined}
      role={ariaLabel ? "region" : undefined}
      aria-label={ariaLabel}
      // Focus that lands on the timeline's own background — a click between
      // cards — still has to be in the timeline for its undo keys to answer.
      tabIndex={!readOnly && (onUndo || onRedo) ? -1 : undefined}
      onPointerDownCapture={(event) => {
        if (event.button !== 0) return;
        if (selecting && timelineSelectionModifier(event) && !isEditableTarget(event.target)) {
          beginMarquee(event);
          return;
        }
        // A plain press anywhere but on the selection's own menu lets the selection go.
        if (selection.size && !(event.target instanceof Node && menuRef.current?.contains(event.target))) {
          setSelection(NO_SELECTION);
        }
      }}
      onContextMenuCapture={selecting && selectedIds.size ? (event) => {
        // With contexts picked out, a right-click anywhere is about them, not
        // about inserting beside whatever is under the pointer.
        event.preventDefault();
        event.stopPropagation();
        openSelectionMenu(event);
      } : undefined}
      onWheel={(event) => {
        if (scrollPinnedRef.current && event.deltaY < 0 && upwardScrollMovesScroller(event.target, event.currentTarget)) {
          releaseBottom();
        }
      }}
      onKeyDown={(event) => {
        if (event.defaultPrevented || isEditableTarget(event.target)) return;
        const historyAction = readOnly ? null : timelineHistoryKey(event);
        if (historyAction) {
          const run = historyAction === "undo" ? onUndo : onRedo;
          if (!run) return;
          event.preventDefault();
          run();
          return;
        }
        if (event.key === "Escape" && selection.size && !menu) {
          event.preventDefault();
          setSelection(NO_SELECTION);
          return;
        }
        if (
          scrollPinnedRef.current
          && UPWARD_SCROLL_KEYS.has(event.key)
          && upwardScrollMovesScroller(event.target, event.currentTarget)
        ) releaseBottom();
      }}
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
      <div ref={streamContentRef} className="context-stream">
        {!insertEditor && renderNodes.length === 0 && turnProjection.leadingTurns.length === 0 ? (
          <EmptyState
            icon={<MeworkIcon plate={false} size={44} />}
            title={t("这段对话还没有消息", "This conversation has no messages yet")}
            description={mutationReadOnly ? undefined : t("在上下文之间右键，可精确插入新内容", "Right-click between contexts to insert content precisely")}
          />
        ) : (
          <>
            {turnProjection.leadingTurns.map(renderTurnNotices)}
            {renderNodes.map((node) => {
              const key = renderNodeKey(node);
              const notices = turnProjection.noticesAfterNode.get(key) ?? [];
              const changes = changesAfterNode.get(key) ?? [];
              return (
                <Fragment key={`timeline:${key}`}>
                  {renderTimelineNode(node)}
                  {changes.map((summary) => (
                    <TurnChanges key={summary.key} summary={summary} pathBaseDir={pathBaseDir} />
                  ))}
                  {notices.map(renderTurnNotices)}
                </Fragment>
              );
            })}
          </>
        )}
        {streaming && turnProjection.visibleRunningTurnIds.size === 0 && (
          <StreamWaitingIndicator
            contexts={contexts}
            tools={tools}
            thinking={thinking}
            retryNotice={retryNotice}
            runningTaskCount={runningTaskCount}
            onOpenTasks={onOpenTasks}
          />
        )}
        {insertEditorIndex === contexts.length && insertEditor}
        {!mutationReadOnly && displayInsertionIndex === contexts.length && renderNodes.length > 0 && <div className="insertion-line" />}
      </div>

      {marquee && (
        <div
          className="timeline-marquee"
          aria-hidden="true"
          style={{ left: marquee.left, top: marquee.top, width: marquee.width, height: marquee.height }}
        />
      )}

      {!mutationReadOnly && menu?.view.kind === "selection" && (
        <div
          ref={menuRef}
          className="context-menu"
          role="menu"
          aria-label={t("所选消息", "Selected messages")}
          style={{ left: menu.x, top: menu.y }}
          onPointerDown={(event) => event.stopPropagation()}
          onKeyDown={(event) => {
            if (event.key !== "Escape") return;
            event.preventDefault();
            const trigger = menu.trigger;
            setMenu(null);
            window.requestAnimationFrame(() => trigger?.focus());
          }}
        >
          <button
            type="button"
            role="menuitem"
            onClick={() => {
              const ids = [...selectedIds].sort((left, right) => (
                (contextIndexes.get(left) ?? 0) - (contextIndexes.get(right) ?? 0)
              ));
              setMenu(null);
              setSelection(NO_SELECTION);
              onDeleteContexts?.(ids);
            }}
          >
            {t("删除", "Delete")}
          </button>
        </div>
      )}

      {!mutationReadOnly && menu?.view.kind === "insert" && (
        <MenuSurfacesContext.Provider value={menuSurfaces}>
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
            {onForkAt && (
              <>
                <div className="context-menu__divider" role="separator" />
                <button
                  type="button"
                  role="menuitem"
                  disabled={menu.index === 0 || Boolean(forkDisabledReason)}
                  title={forkDisabledReason
                    ?? (menu.index === 0 ? t("分割线上方没有可分叉的上下文", "There is nothing above the line to fork") : undefined)}
                  onClick={() => {
                    onForkAt(menu.index);
                    setMenu(null);
                  }}
                >
                  {t("分叉会话", "Fork conversation")}
                </button>
              </>
            )}
          </div>
        </MenuSurfacesContext.Provider>
      )}
    </div>
    </TimelineSelectionContext.Provider>
  );
});
