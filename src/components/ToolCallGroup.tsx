import {
  Bot,
  ChevronRight,
  CircleAlert,
  CircleCheck,
  CircleDot,
  CircleSlash,
  CircleX,
  LoaderCircle,
  Pencil,
  Plus,
  Trash2,
  Wrench
} from "lucide-react";
import { memo, useEffect, useId, useState } from "react";
import { useI18n } from "../i18n";
import type { ContextItem, ToolContext, ToolDescriptor, UserContext } from "../types";
import { isHostAuthoredUserContext } from "../lib/orchestration";
import { agentTimelineRouteId, agentTimelineRunStatus } from "../lib/subagents";
import { IconButton } from "./Common";
import {
  getToolPresentation,
  isHoistedToolCall,
  isOrdinaryToolCall,
  summarizeToolGroup,
  ToolDetailRenderer,
  toolSurfaceForName
} from "./ToolRenderers";
import type { ToolViewFamily } from "./ToolRenderers";

const COLLAPSE_RELEASE_DELAY_MS = 280;

export interface IndexedToolContext {
  item: ToolContext;
  index: number;
}

export interface IndexedQuestionAnswer {
  item: UserContext;
  index: number;
}

export type ContextRenderNode =
  | { kind: "context"; item: Exclude<ContextItem, ToolContext>; index: number }
  | { kind: "tool-group"; key: string; entries: IndexedToolContext[] }
  | { kind: "workflow-card"; key: string; entry: IndexedToolContext }
  | { kind: "question"; key: string; entry: IndexedToolContext; answer?: IndexedQuestionAnswer };

function stableToolId(item: ToolContext): string {
  return item.id;
}

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
 * Projects one uninterrupted tool block into its UI surfaces.
 *
 * Everything the block contains belongs to the one two-level disclosure —
 * ordinary calls, agent-protocol calls and task writes alike. Only two
 * things stay outside it: the `ask_user` history card, because it carries the
 * user's own answer, and a *running* `workflow`, whose progress ledger has no
 * row representation.
 *
 * Calls still in flight are not here at all: `isHoistedToolCall` sends them to
 * the end-of-stream waiting indicator, which narrates them as one line each.
 * A block therefore only ever draws work that has already settled.
 */
function projectToolBlock(
  entries: IndexedToolContext[],
  answersByQuestionId: Map<string, IndexedQuestionAnswer>
): ContextRenderNode[] {
  const visibleEntries = entries.filter(({ item }) => !isHoistedToolCall(item));
  if (!visibleEntries.length) return [];

  const ordinary = visibleEntries.filter((entry) => isOrdinaryToolCall(entry.item));
  let groupEmitted = false;
  const blockId = stableToolId(visibleEntries[0].item);
  const nodes: ContextRenderNode[] = [];

  for (const entry of visibleEntries) {
    if (isOrdinaryToolCall(entry.item)) {
      if (groupEmitted) continue;
      groupEmitted = true;
      nodes.push({
        kind: "tool-group",
        key: `tool-group:${blockId}:${stableToolId(ordinary[0].item)}`,
        entries: ordinary
      });
      continue;
    }
    if (toolSurfaceForName(entry.item.toolName) === "workflow") {
      // One node per call, never merged. Two workflow runs in one tool block are
      // two independent runs with their own steps, and a merged node would have
      // to pick one call id to key the card on — which silently drops the other
      // run's card.
      //
      // Settled runs reach here too: the card reads the agent roster, which
      // outlives the model run, so a finished plan keeps its card rather than
      // collapsing into a disclosure row.
      nodes.push({
        kind: "workflow-card",
        key: `workflow-card:${stableToolId(entry.item)}`,
        entry
      });
      continue;
    }
    nodes.push({
      kind: "question",
      key: `question:${stableToolId(entry.item)}`,
      entry,
      answer: answersByQuestionId.get(entry.item.id)
    });
  }
  return nodes;
}

/**
 * Tool calls share a first-level group for as long as no non-tool message
 * interrupts them. Provider round metadata is execution bookkeeping, not a
 * visual grouping boundary; manual and legacy calls follow the same rule.
 */
export function buildContextRenderNodes(
  contexts: ContextItem[],
  /** Raw user-context indexes that must remain UI boundaries even when an
   * ask_user answer is projected into its paired question card. */
  breakAfterIndexes: ReadonlySet<number> = new Set()
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

  const visibleContexts = contexts.flatMap((item, index) => (
    isProtocolOnlyContext(item) || projectedAnswerIds.has(item.id) ? [] : [{ item, index }]
  ));
  const nodes: ContextRenderNode[] = [];
  let cursor = 0;
  while (cursor < visibleContexts.length) {
    const { item, index } = visibleContexts[cursor];
    if (item.kind !== "tool") {
      nodes.push({ kind: "context", item, index });
      cursor += 1;
      continue;
    }

    const entries: IndexedToolContext[] = [{ item, index }];
    let nextCursor = cursor + 1;
    while (nextCursor < visibleContexts.length) {
      const next = visibleContexts[nextCursor];
      if (next.item.kind !== "tool") break;
      let crossesBoundary = false;
      for (let rawIndex = index + 1; rawIndex < next.index; rawIndex += 1) {
        if (breakAfterIndexes.has(rawIndex)) {
          crossesBoundary = true;
          break;
        }
      }
      if (crossesBoundary) break;
      entries.push({ item: next.item, index: next.index });
      nextCursor += 1;
    }
    nodes.push(...projectToolBlock(entries, answersByQuestionId));
    cursor = nextCursor;
  }
  return nodes;
}

type ExecutionTone = "success" | "error" | "running" | "ready" | "announced" | "halted";

/**
 * The status glyph of one row.
 *
 * An `agent-run` row cannot read its state off `result.success`: the call only
 * acknowledges that the protocol accepted it, so a completed spawn may still
 * own a live background child, and a child that was stopped or hit its round
 * limit ended without failing. `agentTimelineRunStatus` is the single place
 * that judgement lives, shared with the task rail and the read-only drawer.
 */
function executionState(item: ToolContext, family: ToolViewFamily, t: ReturnType<typeof useI18n>["t"]): {
  tone: ExecutionTone;
  label: string;
  icon: typeof CircleCheck;
} {
  if (item.streaming && item.streamStatus !== "completed" && item.streamStatus !== "running") {
    if (item.streamStatus === "ready") return { tone: "ready", label: t("等待执行", "Waiting to run"), icon: CircleDot };
    return { tone: "announced", label: t("准备参数", "Preparing arguments"), icon: CircleDot };
  }
  if (family === "agent-run") {
    switch (agentTimelineRunStatus(item)) {
      case "running": return { tone: "running", label: t("运行中", "Running"), icon: LoaderCircle };
      case "completed": return { tone: "success", label: t("完成", "Completed"), icon: CircleCheck };
      case "stopped": return { tone: "halted", label: t("已停止", "Stopped"), icon: CircleSlash };
      case "roundLimit": return { tone: "halted", label: t("已达轮次上限", "Round limit reached"), icon: CircleAlert };
      case "failed": return { tone: "error", label: t("已失败", "Failed"), icon: CircleX };
      default: return { tone: "error", label: t("已中断", "Interrupted"), icon: CircleX };
    }
  }
  if (item.streaming && item.streamStatus !== "completed") {
    return { tone: "running", label: t("执行中", "Running"), icon: LoaderCircle };
  }
  return item.result.success
    ? { tone: "success", label: t("完成", "Completed"), icon: CircleCheck }
    : { tone: "error", label: t("失败", "Failed"), icon: CircleX };
}

function ToolCallRow({
  entry,
  descriptor,
  insertionIndex,
  readOnly,
  groupExpanded,
  onEdit,
  onDelete,
  onOpenInsert,
  onOpenSubagent
}: {
  entry: IndexedToolContext;
  descriptor?: ToolDescriptor;
  insertionIndex: number | null;
  readOnly: boolean;
  groupExpanded: boolean;
  onEdit?: (item: ToolContext) => void;
  onDelete?: (item: ToolContext) => void;
  onOpenInsert?: (event: React.MouseEvent | React.KeyboardEvent, index: number) => void;
  onOpenSubagent?: (subagentId: string) => void;
}) {
  const { t } = useI18n();
  const { item, index } = entry;
  const presentation = getToolPresentation(item, descriptor, t);
  const state = executionState(item, presentation.family, t);
  const transient = item.streaming === true;
  const canExpand = !transient || item.streamStatus !== "announced";
  const failed = state.tone === "error";
  const [expanded, setExpanded] = useState(failed);
  const [detailsMounted, setDetailsMounted] = useState(failed);
  const summaryId = useId();
  const detailsId = useId();
  const PresentationIcon = presentation.icon;
  const StatusIcon = state.icon;
  // The read-only child transcript is reachable from the row that started it.
  // Absent handler means an ambient surface that owns no drawer — the read-only
  // subagent terminal renders the same rows and must not offer to open itself.
  const subagentRoute = onOpenSubagent && presentation.family === "agent-run"
    ? agentTimelineRouteId(item)
    : null;

  useEffect(() => {
    if (failed) {
      setDetailsMounted(true);
      setExpanded(true);
    }
  }, [failed]);

  useEffect(() => {
    if (expanded && groupExpanded) {
      setDetailsMounted(true);
      return;
    }
    if (!detailsMounted) return;
    const timer = window.setTimeout(() => setDetailsMounted(false), COLLAPSE_RELEASE_DELAY_MS);
    return () => window.clearTimeout(timer);
  }, [detailsMounted, expanded, groupExpanded]);

  return (
    <>
      {!readOnly && insertionIndex === index && (
        <div className="insertion-line tool-call-group__insertion"><span><Plus size={12} /></span></div>
      )}
      <article
        className={`tool-call-group__item tool-call-group__item--${state.tone}`}
        role="listitem"
        tabIndex={readOnly ? undefined : 0}
        data-context-id={item.id}
        data-context-index={index}
        data-tool-name={item.toolName}
        aria-busy={state.tone === "running" || state.tone === "ready" || state.tone === "announced" || undefined}
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
        <div className="tool-call-group__item-heading">
          <button
            id={summaryId}
            className="tool-context__summary"
            type="button"
            aria-controls={canExpand ? detailsId : undefined}
            aria-expanded={canExpand ? expanded : false}
            aria-disabled={!canExpand || undefined}
            onClick={() => {
              if (!canExpand) return;
              setExpanded((current) => {
                if (!current) setDetailsMounted(true);
                return !current;
              });
            }}
          >
            <ChevronRight
              className={`disclosure-chevron${canExpand && expanded ? " disclosure-chevron--open" : ""}`}
              size={14}
              aria-hidden="true"
            />
            <span className="tool-context__icon" aria-hidden="true"><PresentationIcon size={14} /></span>
            <span className="tool-context__copy">
              <span className="tool-context__name">{presentation.title}</span>
              {presentation.target && <code title={presentation.target}>{presentation.target}</code>}
            </span>
            {presentation.stat && <span className="tool-context__stat">{presentation.stat}</span>}
            <span className={`execution-status execution-status--${state.tone}`} role="status" aria-label={state.label} title={state.label}>
              {state.tone === "announced" || state.tone === "ready"
                ? <span className="stream-waiting__square stream-waiting__square--tool" aria-hidden="true" />
                : <StatusIcon className={state.tone === "running" ? "spin" : undefined} size={12} aria-hidden="true" />}
              <span className="sr-only">{state.label}</span>
            </span>
          </button>
          {subagentRoute && (
            <button
              type="button"
              className="tool-call-group__open-agent"
              aria-label={t("打开子代理 {label}", "Open subagent {label}", { label: presentation.target || presentation.title })}
              title={t("打开子代理 {label}", "Open subagent {label}", { label: presentation.target || presentation.title })}
              onClick={() => onOpenSubagent?.(subagentRoute)}
            >
              <Bot size={12} aria-hidden="true" />
              <span>{t("打开", "Open")}</span>
            </button>
          )}
          {!readOnly && !transient && (
            <div className="context-actions tool-call-group__actions">
              <IconButton label={t("编辑工具调用 {title}", "Edit tool call {title}", { title: presentation.title })} onClick={() => onEdit?.(item)}>
                <Pencil size={14} />
              </IconButton>
              <IconButton label={t("删除工具调用 {title}", "Delete tool call {title}", { title: presentation.title })} onClick={() => onDelete?.(item)}>
                <Trash2 size={14} />
              </IconButton>
            </div>
          )}
        </div>
        {canExpand && (
          <div
            className={`collapse-region tool-context__details-region${expanded ? "" : " collapse-region--closed"}`}
            aria-hidden={!expanded || undefined}
            inert={!expanded || undefined}
          >
            <div className="collapse-region__inner">
              <div id={detailsId} className="tool-context__details" role="region" aria-labelledby={summaryId}>
                {detailsMounted && <ToolDetailRenderer item={item} descriptor={descriptor} />}
              </div>
            </div>
          </div>
        )}
      </article>
    </>
  );
}

export interface ToolCallGroupProps {
  entries: IndexedToolContext[];
  tools: ToolDescriptor[];
  insertionIndex: number | null;
  readOnly?: boolean;
  onEdit?: (item: ToolContext) => void;
  onDelete?: (item: ToolContext) => void;
  onOpenInsert?: (event: React.MouseEvent | React.KeyboardEvent, index: number) => void;
  /** Opens the read-only child transcript of an agent-run row. Absent on
   * surfaces that own no drawer, which is what hides the row's Open button. */
  onOpenSubagent?: (subagentId: string) => void;
  /** Degraded surface: also render `workflow` calls as ordinary rows. Only the
   * ContextStream fallback sets this — after a reload/restart no
   * WorkflowRunView may be derivable, and without a row the receipt
   * (workflow:<runId>, the resume handle) would silently vanish from the
   * timeline. Never set on the ordinary contiguous block: the workflow card
   * owns those entries. */
  workflowFallback?: boolean;
}

/** Persisted item references stay stable between stream flushes, so only active groups rerender. */
function toolCallGroupPropsEqual(previous: ToolCallGroupProps, next: ToolCallGroupProps): boolean {
  return previous.tools === next.tools
    && previous.insertionIndex === next.insertionIndex
    && previous.readOnly === next.readOnly
    && previous.onEdit === next.onEdit
    && previous.onDelete === next.onDelete
    && previous.onOpenInsert === next.onOpenInsert
    && previous.onOpenSubagent === next.onOpenSubagent
    && previous.workflowFallback === next.workflowFallback
    && previous.entries.length === next.entries.length
    && previous.entries.every((entry, index) => (
      entry.item === next.entries[index].item && entry.index === next.entries[index].index
    ));
}

/** Two-level ordinary tool surface: contiguous-block summary -> calls -> tailored detail. */
export const ToolCallGroup = memo(function ToolCallGroup({
  entries,
  tools,
  insertionIndex,
  readOnly = false,
  onEdit,
  onDelete,
  onOpenInsert,
  onOpenSubagent,
  workflowFallback = false
}: ToolCallGroupProps) {
  const { t } = useI18n();
  const headingId = useId();
  const listId = useId();
  const [expanded, setExpanded] = useState(true);
  const ordinaryEntries = entries.filter((entry) => (
    isOrdinaryToolCall(entry.item)
    || (workflowFallback && entry.item.toolName === "workflow")
  ));
  if (!ordinaryEntries.length) return null;

  const firstIndex = Math.min(...ordinaryEntries.map((entry) => entry.index));
  const afterIndex = Math.max(...ordinaryEntries.map((entry) => entry.index)) + 1;
  const summary = summarizeToolGroup(ordinaryEntries.map((entry) => entry.item), t);

  return (
    <section
      className="context-card context-card--tool-group tool-call-group"
      aria-labelledby={headingId}
      tabIndex={readOnly ? undefined : 0}
      data-context-index={firstIndex}
      data-context-end-index={afterIndex}
      onContextMenu={readOnly ? undefined : (event) => {
        if ((event.target as HTMLElement).closest(".tool-call-group__item")) return;
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
      <h3 id={headingId} className="tool-call-group__heading">
        <button
          className="tool-call-group__toggle"
          type="button"
          aria-controls={listId}
          aria-expanded={expanded}
          onClick={() => setExpanded((current) => !current)}
        >
          <span className="tool-call-group__lead-icon" aria-hidden="true"><Wrench size={14} /></span>
          <span>{summary}</span>
          <ChevronRight className={`disclosure-chevron${expanded ? " disclosure-chevron--open" : ""}`} size={15} aria-hidden="true" />
        </button>
      </h3>
      <div
        className={`collapse-region tool-call-group__list-region${expanded ? "" : " collapse-region--closed"}`}
        aria-hidden={!expanded || undefined}
        inert={!expanded || undefined}
      >
        <div id={listId} className="collapse-region__inner tool-call-group__list" role="list">
          {ordinaryEntries.map((entry) => (
            <ToolCallRow
              key={stableToolId(entry.item)}
              entry={entry}
              descriptor={tools.find((tool) => tool.name === entry.item.toolName)}
              insertionIndex={insertionIndex}
              readOnly={readOnly}
              groupExpanded={expanded}
              onEdit={onEdit}
              onDelete={onDelete}
              onOpenInsert={onOpenInsert}
              onOpenSubagent={onOpenSubagent}
            />
          ))}
        </div>
      </div>
    </section>
  );
}, toolCallGroupPropsEqual);
