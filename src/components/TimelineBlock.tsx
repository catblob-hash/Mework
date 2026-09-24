import {
  BrainCircuit,
  ChevronRight,
  LoaderCircle,
  Pencil,
  Trash2,
  Webhook
} from "lucide-react";
import { Fragment, memo, useEffect, useId, useMemo, useState } from "react";
import type { ComponentType, ReactNode } from "react";
import { useI18n } from "../i18n";
import type {
  ContextItem,
  ImageAttachment,
  JsonObject,
  ReasoningContext,
  SystemContext,
  ToolContext,
  ToolDescriptor,
  UserContext
} from "../types";
import { useAppearance } from "../lib/appearance";
import { estimateContextTokens, formatCompactTokenCount } from "../lib/contextTokens";
import { isEncryptedReasoning } from "../lib/modelCapabilities";
import { isHostAuthoredUserContext } from "../lib/orchestration";
import { agentTimelineRouteId, agentTimelineRunStatus } from "../lib/subagents";
import { isPreviewToolName } from "../lib/taskTools";
import type { WorkflowRunView } from "../lib/workflowRuns";
import { IconButton } from "./Common";
import { InlineTextEditor } from "./InlineTextEditor";
import { InlineToolEditor } from "./InlineToolEditor";
import { MarkdownContent } from "./MarkdownContent";
import {
  getToolPresentation,
  isHoistedToolCall,
  summarizeBlockKinds,
  ToolDetailRenderer,
  toolRowName,
  toolSummaryKind,
  toolSurfaceForName
} from "./ToolRenderers";
import type { SummaryKind, ToolViewFamily } from "./ToolRenderers";
import { WorkflowRunDetail } from "./WorkflowRunDetail";

const COLLAPSE_RELEASE_DELAY_MS = 280;

/** How much of a reasoning line reaches the DOM; CSS ellipsizes whatever still overflows. */
const REASONING_LINE_LIMIT = 220;

export interface IndexedToolContext {
  item: ToolContext;
  index: number;
}

export interface IndexedQuestionAnswer {
  item: UserContext;
  index: number;
}

/**
 * One row of a block.
 *
 * Tool calls, reasoning and hook records are different kinds of record with the
 * same shape on screen — a marker, a name, and one line of what it did — so
 * they share a row rather than three card layouts that happen to look alike.
 * `workflow` is a tool call whose body is a live run rather than a result.
 */
export type BlockEntry =
  | { kind: "tool"; item: ToolContext; index: number }
  | { kind: "workflow"; item: ToolContext; index: number }
  | { kind: "reasoning"; item: ReasoningContext; index: number }
  | { kind: "hook"; item: SystemContext; index: number };

export type ContextRenderNode =
  | { kind: "context"; item: Exclude<ContextItem, ToolContext | ReasoningContext>; index: number }
  | { kind: "block"; key: string; entries: BlockEntry[] }
  | { kind: "question"; key: string; entry: IndexedToolContext; answer?: IndexedQuestionAnswer };

/**
 * Empty model output is a wire-protocol anchor, not a visible timeline
 * boundary. Context editing can legitimately remove or invalidate its
 * original modelTurnId, so visibility must be derived from the current item
 * itself rather than from provenance metadata.
 *
 * Reasoning is the exception that proves the rule: a Responses round that only
 * returns `encrypted_content` has no summary text at all, yet it really did
 * think and really was billed. Its duration and token count are the entire
 * visible trace of it, so an empty reasoning context carrying either one is a
 * real timeline row — not an anchor.
 */
function isProtocolOnlyContext(item: ContextItem): boolean {
  return (
    item.kind === "assistant"
    && item.content.length === 0
  ) || (
    item.kind === "reasoning"
    && !item.streaming
    && (item.content ?? "").length === 0
    && item.durationMs == null
    && item.tokens == null
  );
}

/**
 * A reasoning record that exists only because the round announced it:
 * encrypted, still streaming, and with nothing in it. The stream indicator
 * narrates it beside the cat until the round closes; a row now would be a name
 * with nothing after it.
 */
function isLiveEncryptedReasoning(item: ReasoningContext): boolean {
  return item.streaming === true
    && (item.content ?? "").length === 0
    && isEncryptedReasoning(item);
}

/**
 * A `workflow` call has settled when the stream says so, or when it never
 * streamed at all — a reloaded transcript.
 */
function isSettledWorkflowCall(item: ToolContext): boolean {
  return item.streamStatus === "completed"
    || (!item.streaming && item.streamStatus === undefined);
}

/**
 * Groups the timeline into blocks, questions and plain message cards.
 *
 * A block runs for as long as nothing the user wrote or the model said
 * interrupts it: tool calls, reasoning and hook records all belong to the same
 * uninterrupted stretch of work, and drawing them as one collapsible list is
 * what keeps a thirty-call round from burying the prose around it. Provider
 * round metadata is execution bookkeeping, not a visual boundary.
 *
 * `ask_user` is the one tool that leaves the block: its card carries the user's
 * own answer, which is a message rather than a record of work.
 */
export function buildContextRenderNodes(
  contexts: ContextItem[],
  /** Raw user-context indexes that must remain UI boundaries even when an
   * ask_user answer is projected into its paired question card. */
  breakAfterIndexes: ReadonlySet<number> = new Set(),
  /** Whether a `workflow` call has a run to show. */
  hasWorkflowView: (callId: string) => boolean = () => false
): ContextRenderNode[] {
  const answersByQuestionId = new Map<string, IndexedQuestionAnswer>();
  const projectedAnswerIds = new Set<string>();
  contexts.forEach((item, index) => {
    if (
      item.kind !== "tool"
      || item.toolName !== "ask_user"
      || !item.result.success
    ) return;

    for (let answerIndex = index + 1; answerIndex < contexts.length; answerIndex += 1) {
      const candidate = contexts[answerIndex];
      // Assistant contexts and host-authored user contexts after a pending question are not answers; continue searching for a real user answer.
      if (
        candidate.kind === "system"
        || candidate.kind === "reasoning"
        || candidate.kind === "assistant"
        || isHostAuthoredUserContext(candidate)
      ) continue;
      if (candidate.kind === "tool") {
        if (candidate.toolName === "ask_user") break;
        continue;
      }
      if (
        candidate.kind === "user"
        && !breakAfterIndexes.has(answerIndex)
        && !projectedAnswerIds.has(candidate.id)
      ) {
        const answer = { item: candidate, index: answerIndex };
        answersByQuestionId.set(item.id, answer);
        projectedAnswerIds.add(candidate.id);
      }
      break;
    }
  });

  const nodes: ContextRenderNode[] = [];
  let block: BlockEntry[] = [];
  const flush = () => {
    if (!block.length) return;
    nodes.push({ kind: "block", key: `block:${block[0].item.id}`, entries: block });
    block = [];
  };

  contexts.forEach((item, index) => {
    if (breakAfterIndexes.has(index)) flush();
    if (isProtocolOnlyContext(item) || projectedAnswerIds.has(item.id)) return;

    if (item.kind === "tool") {
      // A call still in flight is narrated by the end-of-stream indicator as one
      // line; a row for it would be a name and a spinner, replaced the moment
      // the receipt lands.
      if (isHoistedToolCall(item)) return;
      const surface = toolSurfaceForName(item.toolName);
      if (surface === "question") {
        flush();
        nodes.push({
          kind: "question",
          key: `question:${item.id}`,
          entry: { item, index },
          answer: answersByQuestionId.get(item.id)
        });
        return;
      }
      if (surface === "workflow") {
        // An unfinished run with no view would claim a zero-step workflow, which
        // is indistinguishable on screen from one whose steps never arrived.
        if (!hasWorkflowView(item.id) && !isSettledWorkflowCall(item)) return;
        block.push({ kind: "workflow", item, index });
        return;
      }
      block.push({ kind: "tool", item, index });
      return;
    }

    if (item.kind === "reasoning") {
      if (isLiveEncryptedReasoning(item)) return;
      block.push({ kind: "reasoning", item, index });
      return;
    }

    if (item.kind === "system" && item.hookExecution) {
      block.push({ kind: "hook", item, index });
      return;
    }

    flush();
    nodes.push({ kind: "context", item, index });
  });
  flush();
  return nodes;
}

type RowTone = "success" | "error" | "running" | "ready" | "announced" | "halted" | "neutral";

interface RowStatus {
  tone: RowTone;
  label: string;
  /**
   * Drawn at the end of the row when present. Only a row whose state the reader
   * has to act on while it is still changing asks for one: a settled call is
   * overwhelmingly a successful one, and a tick beside every line says nothing
   * the absence of a colour was not already saying.
   */
  icon?: ComponentType<{ size?: number; className?: string }>;
}

/**
 * What a row reports about itself on the right.
 *
 * Its own share of the figure the block heading totals, so the two are one
 * measure at two scales rather than two accounts of the same rows. A record
 * that projects to nothing — a host-local system note — reports nothing rather
 * than a zero.
 */
function rowTokenStat(item: ContextItem, t: ReturnType<typeof useI18n>["t"]): string | undefined {
  const tokens = estimateContextTokens(item);
  if (tokens <= 0) return undefined;
  return t("{tokens} token", "{tokens} tokens", { tokens: formatCompactTokenCount(tokens) });
}

/**
 * The state of one tool row, which reaches the reader as the row's colour and
 * its accessible name rather than as a marker of its own.
 *
 * An `agent-run` row cannot read its state off `result.success`: the call only
 * acknowledges that the protocol accepted it, so a completed spawn may still
 * own a live background child, and a child that was stopped or hit its round
 * limit ended without failing. `agentTimelineRunStatus` is the single place
 * that judgement lives, shared with the task rail and the read-only drawer.
 */
function toolRowStatus(item: ToolContext, family: ToolViewFamily, t: ReturnType<typeof useI18n>["t"]): RowStatus {
  if (item.streaming && item.streamStatus !== "completed" && item.streamStatus !== "running") {
    if (item.streamStatus === "ready") return { tone: "ready", label: t("等待执行", "Waiting to run") };
    return { tone: "announced", label: t("准备参数", "Preparing arguments") };
  }
  if (family === "agent-run") {
    switch (agentTimelineRunStatus(item)) {
      case "running": return { tone: "running", label: t("运行中", "Running") };
      case "completed": return { tone: "success", label: t("完成", "Completed") };
      case "stopped": return { tone: "halted", label: t("已停止", "Stopped") };
      case "roundLimit": return { tone: "halted", label: t("已达轮次上限", "Round limit reached") };
      case "failed": return { tone: "error", label: t("已失败", "Failed") };
      default: return { tone: "error", label: t("已中断", "Interrupted") };
    }
  }
  if (item.streaming && item.streamStatus !== "completed") {
    return { tone: "running", label: t("执行中", "Running") };
  }
  return item.result.success
    ? { tone: "success", label: t("完成", "Completed") }
    : { tone: "error", label: t("失败", "Failed") };
}

function hookRowStatus(item: SystemContext, t: ReturnType<typeof useI18n>["t"]): RowStatus {
  switch (item.hookExecution?.status) {
    case "running": return { tone: "running", label: t("执行中", "Running") };
    case "blocked": return { tone: "error", label: t("已阻止", "Blocked") };
    case "failed": return { tone: "halted", label: t("错误（未阻断）", "Error (not blocking)") };
    default: return { tone: "success", label: t("完成", "Completed") };
  }
}

/**
 * The one line of reasoning a collapsed row shows.
 *
 * While the round is still thinking this is the newest line, so the row reads
 * as the thought moving forward; once it settles the first line is what names
 * what the round set out to do. Markdown markers are stripped because a row is
 * plain text — a lone `##` would be all the reader saw of that line.
 */
function reasoningLine(content: string, streaming: boolean): string | undefined {
  const lines = content.split("\n").map((line) => line.trim()).filter(Boolean);
  const line = streaming ? lines[lines.length - 1] : lines[0];
  if (!line) return undefined;
  const stripped = line.replace(/^(#{1,6}|[-*+]|>|\d+\.)\s+/, "").replace(/[*_`]/g, "").trim();
  if (!stripped) return undefined;
  return stripped.length > REASONING_LINE_LIMIT
    ? `${stripped.slice(0, REASONING_LINE_LIMIT - 1).trimEnd()}…`
    : stripped;
}

/**
 * Keeps a row's body in the DOM for the length of the closing animation, so a
 * collapse does not cut its own transition short.
 */
function useDetailsMounting(open: boolean): boolean {
  const [mounted, setMounted] = useState(open);
  // Opening mounts the body in the same render pass: a body that arrives an
  // effect-cycle later changes the region's target height mid-transition and
  // splits the expand animation in two.
  if (open && !mounted) setMounted(true);
  useEffect(() => {
    if (open || !mounted) return;
    const timer = window.setTimeout(() => setMounted(false), COLLAPSE_RELEASE_DELAY_MS);
    return () => window.clearTimeout(timer);
  }, [mounted, open]);
  return mounted;
}

/**
 * The shared row: a marker, a name, one line of the record itself, and a body
 * that opens under it.
 *
 * The edit and delete controls are laid over the right end of the header
 * instead of taking a column of their own — a row is one line, and reserving
 * space for controls nobody is reaching for would shorten every line on screen
 * to make room for the rare hover. The line fades out beneath them so the
 * overlap reads as intended rather than as a collision.
 */
export function TimelineRow({
  contextId,
  index,
  rowKind,
  icon: Icon,
  name,
  line,
  stat,
  status,
  accessibleName,
  badge,
  actions,
  expandable,
  expanded,
  onToggleExpanded,
  onOpenPanel,
  readOnly,
  onOpenInsert,
  children
}: {
  contextId: string;
  index: number;
  /** Names the row's kind in markup, for styling and for tests to address. */
  rowKind: string;
  icon: ComponentType<{ size?: number; className?: string }>;
  name: string;
  line?: string;
  stat?: string;
  status?: RowStatus;
  /** What the disclosure button is called; the row shows an identifier, not a sentence. */
  accessibleName: string;
  badge?: ReactNode;
  actions?: ReactNode;
  expandable: boolean;
  expanded: boolean;
  onToggleExpanded: () => void;
  /**
   * Where a row that has no body of its own sends the click instead. A run that
   * owns a panel is read there rather than in a drawer under its line, so the
   * row carries no disclosure at all.
   */
  onOpenPanel?: () => void;
  readOnly: boolean;
  onOpenInsert?: (event: React.MouseEvent | React.KeyboardEvent, index: number) => void;
  children: ReactNode;
}) {
  const summaryId = useId();
  const detailsId = useId();
  const StatusIcon = status?.icon;
  const tone = status?.tone ?? "neutral";
  // A closed row keeps nothing of its body in the DOM. Markdown, diffs and
  // terminal output are the expensive part of a long transcript, and a block of
  // thirty rows would otherwise render all thirty bodies to show none of them.
  const detailsMounted = useDetailsMounting(expandable && expanded);

  return (
    <article
      className={`timeline-row timeline-row--${rowKind} timeline-row--${tone}`}
      role="listitem"
      tabIndex={readOnly ? undefined : 0}
      data-context-id={contextId}
      data-context-index={index}
      data-row-kind={rowKind}
      aria-busy={tone === "running" || tone === "ready" || tone === "announced" || undefined}
      onContextMenu={readOnly ? undefined : (event) => {
        event.preventDefault();
        const box = event.currentTarget.getBoundingClientRect();
        onOpenInsert?.(event, index + (event.clientY > box.top + box.height / 2 ? 1 : 0));
      }}
      onKeyDown={readOnly ? undefined : (event) => {
        if (event.shiftKey && event.key === "F10") {
          event.preventDefault();
          onOpenInsert?.(event, index + 1);
        }
      }}
    >
      <div className="timeline-row__header">
        <div className="timeline-row__main">
          <button
            id={summaryId}
            className="timeline-row__summary"
            type="button"
            title={accessibleName}
            // The row says what happened, in words, and the identifier it says
            // it about is only its tooltip — so the accessible name has to carry
            // both: the phrase so speech input can address the row by the words
            // on screen, and the identifier and state for anyone who cannot see
            // it, now that no marker states them. The volatile figures stay out
            // — a token count that re-announces itself every flush is worse than
            // not hearing it at all.
            aria-label={`${name} · ${accessibleName}`}
            aria-controls={expandable ? detailsId : undefined}
            aria-expanded={expandable ? expanded : false}
            aria-haspopup={!expandable && onOpenPanel ? "dialog" : undefined}
            aria-disabled={(!expandable && !onOpenPanel) || undefined}
            onClick={() => (expandable ? onToggleExpanded() : onOpenPanel?.())}
          >
            <span className="timeline-row__icon" aria-hidden="true"><Icon size={14} /></span>
            <span className="timeline-row__name">{name}</span>
            {line && <span className="timeline-row__line">{line}</span>}
            {/* Against the end of the row's own words, not at the far edge of
                the line: the arrow belongs to what it opens. */}
            {expandable && (
              <ChevronRight
                className={`timeline-row__chevron disclosure-chevron${expanded ? " disclosure-chevron--open" : ""}`}
                size={14}
                aria-hidden="true"
              />
            )}
            {stat && <span className="timeline-row__stat">{stat}</span>}
          </button>
          {/* Laid over the end of the line rather than given a column of its
              own: reserving space for controls nobody is reaching for would
              shorten every line on screen to make room for the rare hover. The
              figures at that end step aside while they are showing, so nothing
              is read through a button. */}
          {actions && <div className="timeline-row__actions">{actions}</div>}
        </div>
        {badge}
        {StatusIcon && (
          <span className={`execution-status execution-status--${tone}`} role="status" aria-label={status!.label} title={status!.label}>
            <StatusIcon className={tone === "running" ? "spin" : undefined} size={12} aria-hidden="true" />
            <span className="sr-only">{status!.label}</span>
          </span>
        )}
      </div>
      {expandable && (
        <div
          className={`collapse-region timeline-row__details-region${expanded ? "" : " collapse-region--closed"}`}
          aria-hidden={!expanded || undefined}
          inert={!expanded || undefined}
        >
          <div className="collapse-region__inner">
            <div id={detailsId} className="timeline-row__details" role="region" aria-labelledby={summaryId}>
              {detailsMounted && children}
            </div>
          </div>
        </div>
      )}
    </article>
  );
}

interface RowCallbacks {
  readOnly: boolean;
  editingContextId: string | null;
  onEdit?: (item: ContextItem) => void;
  onDelete?: (item: ContextItem) => void;
  onCancelEdit?: () => void;
  onSaveText?: (content: string, images?: ImageAttachment[]) => void;
  onSaveTool?: (toolName: string, input: JsonObject) => Promise<unknown>;
  onSaveToolEdit?: (input: JsonObject, output: string, images: ImageAttachment[]) => Promise<unknown>;
  onOpenInsert?: (event: React.MouseEvent | React.KeyboardEvent, index: number) => void;
  onOpenSubagent?: (subagentId: string) => void;
  onOpenWorkflowRun?: (runId: string) => void;
  pathBaseDir: string | null;
  deferOffscreen: boolean;
}

function RowActions({ item, onEdit, onDelete, allowEdit, editLabel, deleteLabel }: {
  item: ContextItem;
  onEdit?: (item: ContextItem) => void;
  onDelete?: (item: ContextItem) => void;
  allowEdit: boolean;
  editLabel: string;
  deleteLabel: string;
}) {
  return (
    <>
      {allowEdit && (
        <IconButton label={editLabel} onClick={() => onEdit?.(item)}>
          <Pencil size={13} />
        </IconButton>
      )}
      <IconButton label={deleteLabel} onClick={() => onDelete?.(item)}>
        <Trash2 size={13} />
      </IconButton>
    </>
  );
}

function ToolRow({
  entry,
  descriptor,
  workflowView,
  groupExpanded,
  callbacks
}: {
  entry: Extract<BlockEntry, { kind: "tool" | "workflow" }>;
  descriptor?: ToolDescriptor;
  workflowView?: WorkflowRunView;
  groupExpanded: boolean;
  callbacks: RowCallbacks;
}) {
  const { t } = useI18n();
  const { item, index } = entry;
  const presentation = getToolPresentation(item, descriptor, t);
  const status = toolRowStatus(item, presentation.family, t);
  const transient = item.streaming === true;
  const editing = item.id === callbacks.editingContextId;
  const failed = status.tone === "error";
  const [expanded, setExpanded] = useState(failed);
  const agentRoute = presentation.family === "agent-run" ? agentTimelineRouteId(item) : null;
  const subagentRoute = callbacks.onOpenSubagent ? agentRoute : null;
  // A run of its own is read in its own panel, not in a drawer under the line:
  // a workflow's plan and a child's transcript are both longer than the
  // transcript they were spawned from. Those rows carry no disclosure at all —
  // the whole line is the way in. Where no panel exists (a read-only
  // transcript, or a row being edited) the body stands in as before.
  const openPanel = editing
    ? undefined
    : workflowView && callbacks.onOpenWorkflowRun
      ? () => callbacks.onOpenWorkflowRun?.(workflowView.id)
      : subagentRoute
        ? () => callbacks.onOpenSubagent?.(subagentRoute)
        : undefined;
  const expandable = !openPanel && (!transient || item.streamStatus !== "announced");
  const open = (expanded || editing) && groupExpanded;

  // A failure is the one result nobody opened the row to find, so it opens
  // itself. An open editor owns the body: collapsing would unmount it and
  // silently discard whatever had been typed.
  useEffect(() => {
    if (failed) setExpanded(true);
  }, [failed]);
  useEffect(() => {
    if (editing) setExpanded(true);
  }, [editing]);

  // Most rows say what happened, in words: a collapsed row is the whole of what
  // the call shows, and the wire name is already its tooltip. The exceptions are
  // the rows the phrase cannot tell apart. A block can hold several runs that
  // would all read "运行了子代理", so those take the name that addresses the run;
  // a note carries no subject beyond what it says, so it takes its own text.
  // `agentTimelineRouteId` falls back to the context id when the call named no
  // child, and a bare uuid names nothing a reader could act on, so the phrase
  // stands in for it.
  const name = entry.kind === "workflow" && workflowView
    ? workflowView.name
    : agentRoute && agentRoute !== item.id
      ? agentRoute
      : (presentation.family === "agent-note" && presentation.target) || presentation.title;
  const tokenStat = rowTokenStat(item, t);
  const stat = workflowView
    ? [t("{count} 个代理", "{count} agents", { count: workflowView.stepCount }), tokenStat]
      .filter(Boolean).join(" · ")
    : tokenStat;

  return (
    <TimelineRow
      contextId={item.id}
      index={index}
      rowKind={entry.kind}
      icon={presentation.icon}
      name={name}
      stat={stat}
      status={status}
      accessibleName={[toolRowName(item, descriptor), presentation.target, status.label]
        .filter(Boolean).join(" · ")}
      actions={!callbacks.readOnly && !transient && !editing ? (
        <RowActions
          item={item}
          onEdit={callbacks.onEdit}
          onDelete={callbacks.onDelete}
          // Preview is one merged tool in the settings and no single call by
          // that name, so its cards are read and deleted but never rewritten.
          // A card carrying a child transcript is likewise read-only: the host
          // refuses to re-attest one, and a pencil that always fails is worse
          // than no pencil. A timeline with no way to commit a rewritten card
          // at all withholds it for the same reason. Being unable to *execute*
          // the call is not one of those reasons — a template can hold a
          // rewritten card without having anything to run it against.
          allowEdit={!isPreviewToolName(item.toolName)
            && !item.subagent
            && Boolean(callbacks.onSaveToolEdit || callbacks.onSaveTool)}
          editLabel={t("编辑工具调用 {title}", "Edit tool call {title}", { title: presentation.title })}
          deleteLabel={t("删除工具调用 {title}", "Delete tool call {title}", { title: presentation.title })}
        />
      ) : undefined}
      expandable={expandable}
      expanded={open}
      onToggleExpanded={() => !editing && setExpanded((current) => !current)}
      onOpenPanel={openPanel}
      readOnly={callbacks.readOnly}
      onOpenInsert={callbacks.onOpenInsert}
    >
      {editing && callbacks.onCancelEdit && (callbacks.onSaveToolEdit || callbacks.onSaveTool)
        ? (
          <InlineToolEditor
            item={item}
            descriptor={descriptor}
            onCancel={callbacks.onCancelEdit}
            onRun={callbacks.onSaveTool}
            onSave={callbacks.onSaveToolEdit}
          />
        )
        : workflowView
          // Reached only where the row owns no panel to send the click to — a
          // read-only transcript — so the body states the run instead.
          ? <WorkflowRunDetail view={workflowView} />
          : <ToolDetailRenderer item={item} descriptor={descriptor} />}
    </TimelineRow>
  );
}

function ReasoningRow({
  entry,
  groupExpanded,
  callbacks
}: {
  entry: Extract<BlockEntry, { kind: "reasoning" }>;
  groupExpanded: boolean;
  callbacks: RowCallbacks;
}) {
  const { t } = useI18n();
  const { collapseReasoning } = useAppearance();
  const { item, index } = entry;
  const content = item.content ?? "";
  const streaming = item.streaming === true;
  // `isEncryptedReasoning` prefers the card's `form`, falling back only to the
  // legacy empty-body heuristic.
  const encrypted = isEncryptedReasoning(item);
  const editing = item.id === callbacks.editingContextId;
  const editable = !callbacks.readOnly && !streaming;
  // Reasoning opens itself while it is arriving so the round narrates in place;
  // afterwards the appearance preference decides, until the reader says
  // otherwise.
  const [expanded, setExpanded] = useState(streaming || !collapseReasoning);
  const expandable = content.length > 0 || editing;
  const open = expandable && (expanded || editing) && groupExpanded;
  const tokenText = item.tokens ? `${formatCompactTokenCount(item.tokens)} tokens` : undefined;
  // A provider that reports no reasoning-token usage leaves the row without a
  // figure of its own, so the shared content estimate stands in — the same
  // source every other row's right edge reads from.
  const stat = tokenText ?? rowTokenStat(item, t);
  const line = reasoningLine(content, streaming);

  useEffect(() => {
    if (streaming) setExpanded(true);
  }, [streaming]);
  useEffect(() => {
    if (editing) setExpanded(true);
  }, [editing]);

  return (
    <TimelineRow
      contextId={item.id}
      index={index}
      rowKind="reasoning"
      icon={BrainCircuit}
      name="think"
      // A record with no body to quote — encrypted, or a round that was billed
      // for thinking it never wrote down — has its token count as the whole of
      // what it can say for itself, so that takes the line instead.
      line={line ?? tokenText}
      stat={line ? stat : undefined}
      status={streaming ? { tone: "running", label: t("正在思考", "Thinking"), icon: LoaderCircle } : undefined}
      accessibleName={streaming
        ? t("正在思考", "Thinking")
        : encrypted
          ? t("加密思考", "Encrypted reasoning")
          : t("思考过程", "Reasoning")}
      actions={callbacks.readOnly || editing ? undefined : (
        <RowActions
          item={item}
          onEdit={callbacks.onEdit}
          onDelete={callbacks.onDelete}
          // Encrypted reasoning cannot be edited: its body never reached the
          // client, so anything typed here would be fabricated history the next
          // round is asked to believe.
          allowEdit={editable && !encrypted}
          editLabel={t("编辑上下文", "Edit context")}
          deleteLabel={t("删除上下文", "Delete context")}
        />
      )}
      expandable={expandable}
      expanded={open}
      onToggleExpanded={() => !editing && setExpanded((current) => !current)}
      readOnly={callbacks.readOnly}
      onOpenInsert={callbacks.onOpenInsert}
    >
      {editing && callbacks.onCancelEdit && callbacks.onSaveText
          ? (
            <InlineTextEditor
              kind="reasoning"
              content={content}
              onCancel={callbacks.onCancelEdit}
              onSave={callbacks.onSaveText}
            />
          )
          : (
            <div className="timeline-row__prose" aria-live={streaming ? "polite" : undefined}>
              <MarkdownContent
                content={content}
                deferOffscreen={callbacks.deferOffscreen}
                streaming={streaming}
                linkifyPaths
                renderHtml
                pathBaseDir={callbacks.pathBaseDir}
              />
            </div>
          )}
    </TimelineRow>
  );
}

function HookRow({
  entry,
  groupExpanded,
  callbacks
}: {
  entry: Extract<BlockEntry, { kind: "hook" }>;
  groupExpanded: boolean;
  callbacks: RowCallbacks;
}) {
  const { t } = useI18n();
  const { item, index } = entry;
  const execution = item.hookExecution!;
  const status = hookRowStatus(item, t);
  const content = item.content ?? "";
  const [expanded, setExpanded] = useState(false);
  const expandable = content.length > 0;
  const open = expandable && expanded && groupExpanded;

  return (
    <TimelineRow
      contextId={item.id}
      index={index}
      rowKind="hook"
      icon={Webhook}
      name={execution.hookName || execution.event}
      stat={rowTokenStat(item, t)}
      status={status}
      accessibleName={t("{event} 钩子 · {status}", "{event} hook · {status}", { event: execution.event, status: status.label })}
      badge={execution.contextInjected ? (
        <span
          className="timeline-row__badge"
          title={t("此记录的内容已作为系统上下文加入后续模型请求", "This record's content has been added as system context for later model requests")}
        >
          {t("已加入模型上下文", "Added to model context")}
        </span>
      ) : undefined}
      expandable={expandable}
      expanded={open}
      onToggleExpanded={() => setExpanded((current) => !current)}
      readOnly={callbacks.readOnly}
      onOpenInsert={callbacks.onOpenInsert}
    >
      <pre className="timeline-row__output" aria-live={execution.status === "running" ? "polite" : undefined}>
        {content}
      </pre>
    </TimelineRow>
  );
}

export interface TimelineBlockProps {
  entries: BlockEntry[];
  tools: ToolDescriptor[];
  insertionIndex: number | null;
  readOnly?: boolean;
  /** Context id of the row whose body is being edited in place. */
  editingContextId?: string | null;
  onEdit?: (item: ContextItem) => void;
  onDelete?: (item: ContextItem) => void;
  onCancelEdit?: () => void;
  onSaveText?: (content: string, images?: ImageAttachment[]) => void;
  onSaveTool?: (toolName: string, input: JsonObject) => Promise<unknown>;
  onSaveToolEdit?: (input: JsonObject, output: string, images: ImageAttachment[]) => Promise<unknown>;
  onOpenInsert?: (event: React.MouseEvent | React.KeyboardEvent, index: number) => void;
  /** Opens the read-only child transcript of an agent-run row. Absent on
   * surfaces that own no drawer, which is what hides the row's Open button. */
  onOpenSubagent?: (subagentId: string) => void;
  /** Runs keyed by the owning `workflow` call id. */
  workflowRunByCall?: Record<string, WorkflowRunView>;
  /** Brings a run's panel forward in the task container. */
  onOpenWorkflowRun?: (runId: string) => void;
  deferOffscreen?: boolean;
  pathBaseDir?: string | null;
}

/** Persisted item references stay stable between stream flushes, so only active blocks rerender. */
function timelineBlockPropsEqual(previous: TimelineBlockProps, next: TimelineBlockProps): boolean {
  return previous.tools === next.tools
    && previous.insertionIndex === next.insertionIndex
    && previous.readOnly === next.readOnly
    && previous.editingContextId === next.editingContextId
    && previous.onEdit === next.onEdit
    && previous.onDelete === next.onDelete
    && previous.onCancelEdit === next.onCancelEdit
    && previous.onSaveText === next.onSaveText
    && previous.onSaveTool === next.onSaveTool
    && previous.onSaveToolEdit === next.onSaveToolEdit
    && previous.onOpenInsert === next.onOpenInsert
    && previous.onOpenSubagent === next.onOpenSubagent
    && previous.onOpenWorkflowRun === next.onOpenWorkflowRun
    && previous.workflowRunByCall === next.workflowRunByCall
    && previous.deferOffscreen === next.deferOffscreen
    && previous.pathBaseDir === next.pathBaseDir
    && previous.entries.length === next.entries.length
    && previous.entries.every((entry, index) => (
      entry.item === next.entries[index].item && entry.index === next.entries[index].index
    ));
}

/**
 * One uninterrupted stretch of work, as a heading over a list of rows.
 *
 * The heading counts what the block contains rather than naming the first
 * thing in it, because a block is usually several kinds of work at once. Its
 * token figure is deliberately not part of that sentence: it is a number the
 * reader wants occasionally and never wants to read past, so it waits at the
 * right of the heading until the pointer is on the block.
 */
export const TimelineBlock = memo(function TimelineBlock({
  entries,
  tools,
  insertionIndex,
  readOnly = false,
  editingContextId = null,
  onEdit,
  onDelete,
  onCancelEdit,
  onSaveText,
  onSaveTool,
  onSaveToolEdit,
  onOpenInsert,
  onOpenSubagent,
  workflowRunByCall,
  onOpenWorkflowRun,
  deferOffscreen = false,
  pathBaseDir = null
}: TimelineBlockProps) {
  const { t } = useI18n();
  const headingId = useId();
  const listId = useId();
  const [expanded, setExpanded] = useState(true);

  const firstIndex = Math.min(...entries.map((entry) => entry.index));
  const afterIndex = Math.max(...entries.map((entry) => entry.index)) + 1;
  const summary = useMemo(() => summarizeBlockKinds(
    entries.map((entry): SummaryKind => (
      entry.kind === "reasoning" ? "reasoning"
        : entry.kind === "hook" ? "hook"
          : toolSummaryKind(entry.item)
    )),
    t
  ), [entries, t]);
  const tokens = useMemo(
    () => entries.reduce((total, entry) => total + estimateContextTokens(entry.item), 0),
    [entries]
  );
  // A collapsed list is `inert`, which would make an open editor unreachable.
  const listExpanded = expanded || entries.some((entry) => entry.item.id === editingContextId);
  const callbacks: RowCallbacks = {
    readOnly,
    editingContextId,
    onEdit,
    onDelete,
    onCancelEdit,
    onSaveText,
    onSaveTool,
    onSaveToolEdit,
    onOpenInsert,
    onOpenSubagent,
    onOpenWorkflowRun,
    pathBaseDir,
    deferOffscreen
  };

  return (
    <section
      className="timeline-block"
      aria-labelledby={headingId}
      tabIndex={readOnly ? undefined : 0}
      data-context-index={firstIndex}
      data-context-end-index={afterIndex}
      onContextMenu={readOnly ? undefined : (event) => {
        if ((event.target as HTMLElement).closest(".timeline-row")) return;
        event.preventDefault();
        const box = event.currentTarget.getBoundingClientRect();
        onOpenInsert?.(event, event.clientY > box.top + box.height / 2 ? afterIndex : firstIndex);
      }}
      onKeyDown={readOnly ? undefined : (event) => {
        if (event.target !== event.currentTarget || !event.shiftKey || event.key !== "F10") return;
        event.preventDefault();
        onOpenInsert?.(event, afterIndex);
      }}
    >
      <h3 id={headingId} className="timeline-block__heading">
        <button
          className="timeline-block__toggle"
          type="button"
          // The figure beside the sentence is not part of what the heading
          // says: a token count that re-announces itself every flush is worse
          // than not hearing it at all.
          aria-label={summary}
          aria-controls={listId}
          aria-expanded={listExpanded}
          onClick={() => setExpanded((current) => !current)}
        >
          <span className="timeline-block__summary">{summary}</span>
          <ChevronRight
            className={`timeline-block__chevron disclosure-chevron${listExpanded ? " disclosure-chevron--open" : ""}`}
            size={14}
            aria-hidden="true"
          />
          <span className="timeline-block__tokens">
            {t("{tokens} token", "{tokens} tokens", { tokens: formatCompactTokenCount(tokens) })}
          </span>
        </button>
      </h3>
      <div
        className={`collapse-region timeline-block__list-region${listExpanded ? "" : " collapse-region--closed"}`}
        aria-hidden={!listExpanded || undefined}
        inert={!listExpanded || undefined}
      >
        <div id={listId} className="collapse-region__inner timeline-block__list" role="list">
          {entries.map((entry) => (
            <Fragment key={entry.item.id}>
              {!readOnly && insertionIndex === entry.index && (
                <div className="insertion-line timeline-block__insertion" />
              )}
              {entry.kind === "reasoning"
                ? <ReasoningRow entry={entry} groupExpanded={listExpanded} callbacks={callbacks} />
                : entry.kind === "hook"
                  ? <HookRow entry={entry} groupExpanded={listExpanded} callbacks={callbacks} />
                  : (
                    <ToolRow
                      entry={entry}
                      descriptor={tools.find((tool) => tool.name === entry.item.toolName)}
                      workflowView={entry.kind === "workflow" ? workflowRunByCall?.[entry.item.id] : undefined}
                      groupExpanded={listExpanded}
                      callbacks={callbacks}
                    />
                  )}
            </Fragment>
          ))}
        </div>
      </div>
    </section>
  );
}, timelineBlockPropsEqual);
