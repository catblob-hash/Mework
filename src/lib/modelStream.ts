/**
 * Pure projection layer for model streams. It folds `ModelStreamEvent` values
 * into `ModelRunState`, and nested subagent/workflow fragments into
 * `SubagentLiveState`.
 *
 * Exports are either pure reducers that return `ModelStreamEffect` instructions,
 * or `createModelStreamCoalescer`, which batches every event at a fixed 10 FPS
 * without changing event order or reducer semantics.
 */
import { createId } from "./id";
import { canonicalJson } from "./contextTokens";
import { isEncryptedReasoning } from "./modelCapabilities";
import { getI18nSnapshot, translate } from "../i18n";
import { sumModelUsage, sumUsageByRound } from "./conversationTurns";
import type {
  ContextItem,
  JsonObject,
  ModelRunRequest,
  ModelStreamEvent,
  ModelUsage,
  SubagentLiveState,
  ToolResult,
  UserContext,
  WorkflowProgressEntry
} from "../types";

export type StreamingToolPhase = "announced" | "ready" | "running" | "completed";
export type StreamingToolState = {
  /** Stable local id; provider callId below exists only while this stream is active. */
  id: string;
  callId: string;
  toolName: string;
  requestedInput?: JsonObject;
  input: JsonObject;
  result: ToolResult;
  streamStatus: StreamingToolPhase;
  live: SubagentLiveState;
  createdAt: string;
};
export type StreamingHookState = {
  executionId: string;
  hookId: string;
  hookName: string;
  event: string;
  statusMessage?: string;
  result: ToolResult;
  status: "running" | "succeeded" | "failed" | "blocked";
  contextInjected: boolean;
  reason?: string;
  createdAt: string;
};
export type ModelRunRetryState = {
  round: number;
  attempt: number;
  maxAttempts: number;
  message: string;
  /** True until the retried attempt streams its first event for the round;
   * that first event discards the failed attempt's partial content. */
  dirty: boolean;
};
export type ModelRunState = {
  requestId: string;
  providerName: string;
  modelName: string;
  workspaceId: string;
  request: ModelRunRequest;
  startedAt: string;
  streamedTextByRound: Record<number, string>;
  /** Reasoning text per round, one slot per wire `item` ordinal. An encrypted-only
   * item opens an empty slot at `reasoning_start` so its row remains visible. */
  streamedReasoningByRound: Record<number, string[]>;
  reasoningFormsByRound?: Record<number, Record<number, "plaintext" | "encrypted">>;
  completedReasoningByRound: Record<number, boolean>;
  activeReasoningItemsByRound?: Record<number, Record<number, boolean>>;
  /** When each round's reasoning opened, per `reasoning_start`. Drives the
   * card's live clock before the provider's own figure lands, and is what
   * makes a summary-less (encrypted-only) reasoning item visible at all. */
  reasoningStartedAtByRound: Record<number, string>;
  /** The sidecar's authoritative reasoning wall clock per round, from
   * `reasoning_done`. Supersedes the live clock the moment it arrives so the
   * number never changes across a reload. */
  reasoningDurationByRound: Record<number, number>;
  streamedToolsByRound: Record<number, StreamingToolState[]>;
  streamedHooksByRound: Record<number, StreamingHookState[]>;
  steeredInputsByRound: Record<number, UserContext[]>;
  /** Provider-reported cumulative snapshots, isolated by backend request round. */
  usageByRound: Record<number, ModelUsage>;
  /** Nested agent usage snapshots, isolated by agent call and child request round. */
  subagentUsageByCall: Record<string, Record<number, ModelUsage>>;
  /** Progress ledger rows per `workflow` call, in arrival order. The card folds
   * them with `deriveWorkflowProgress`; keeping the raw stream here rather than
   * the folded view means a late-arriving entry re-folds cheaply and the
   * projection stays pure. */
  workflowProgressByCall: Record<string, WorkflowProgressEntry[]>;
  /** Live run id per `workflow` call — the address Skip/Retry commands carry. */
  workflowRunIdByCall: Record<string, string>;
  /** Lifecycle events that arrived before their call's announcement, keyed by
   * round then callId. Optional so long-lived fixtures need not spell it out;
   * absent means empty. Replayed and cleared when the announcement lands. */
  pendingToolEventsByRound?: Record<number, Record<string, ToolStreamEvent[]>>;
  usageRevision: number;
  retry?: ModelRunRetryState;
};
export type ModelRuns = Partial<Record<string, ModelRunState>>;

export type ToolStreamEvent = Extract<
  ModelStreamEvent,
  {
    type:
      | "tool_call_announced"
      | "tool_call_arguments_ready"
      | "tool_execution_started"
      | "tool_execution_completed";
  }
>;

export type HookStreamEvent = Extract<
  ModelStreamEvent,
  { type: "hook_execution_started" | "hook_execution_completed" }
>;

/** Discards the failed attempt's streamed content for one round once its
 * retry starts delivering data, so the fresh attempt never appends onto the
 * partial it replaces. Hook records are host-side executions and survive;
 * tools of this round can only have been announced (they execute after the
 * stream completes), so dropping them is always safe. */
export function resetRoundForRetry(run: ModelRunState, round: number): ModelRunState {
  const { [round]: _text, ...streamedTextByRound } = run.streamedTextByRound;
  const { [round]: _reasoning, ...streamedReasoningByRound } = run.streamedReasoningByRound;
  const { [round]: _active, ...activeReasoningItemsByRound } = run.activeReasoningItemsByRound ?? {};
  const { [round]: _forms, ...reasoningFormsByRound } = run.reasoningFormsByRound ?? {};
  const { [round]: _completed, ...completedReasoningByRound } = run.completedReasoningByRound;
  // Retry-specific timing belongs to the retried round; retaining it would
  // start the new attempt's clock at the previous attempt.
  const { [round]: _startedAt, ...reasoningStartedAtByRound } = run.reasoningStartedAtByRound;
  const { [round]: _duration, ...reasoningDurationByRound } = run.reasoningDurationByRound;
  const { [round]: _tools, ...streamedToolsByRound } = run.streamedToolsByRound;
  const { [round]: _usage, ...usageByRound } = run.usageByRound;
  return {
    ...run,
    streamedTextByRound,
    streamedReasoningByRound,
    reasoningFormsByRound,
    completedReasoningByRound,
    activeReasoningItemsByRound,
    reasoningStartedAtByRound,
    reasoningDurationByRound,
    streamedToolsByRound,
    usageByRound
  };
}

export function cumulativeModelRunUsage(run: ModelRunState): ModelUsage {
  return sumModelUsage([
    sumUsageByRound(run.usageByRound),
    ...Object.values(run.subagentUsageByCall).map(sumUsageByRound)
  ]);
}

/**
 * Unwraps a `subagent_event` envelope chain down to a leaf usage snapshot.
 *
 * A child's events arrive wrapped once; a workflow step's arrive wrapped twice
 * — the run's envelope around the step's envelope — and nothing stops the tree
 * from being deeper still. Reading only the immediate payload therefore dropped
 * every workflow step's tokens on the floor, so the turn's own total silently
 * omitted whatever a workflow spent.
 *
 * The returned `key` is the whole call path, because two steps of one run are
 * distinct spenders that would otherwise overwrite each other under the run's
 * single call id. A depth-one child's key is still its bare call id, so the
 * accounting map's existing entries keep their names.
 */
export function nestedUsageUpdate(
  event: Extract<ModelStreamEvent, { type: "subagent_event" }>
): { key: string; round: number; usage: ModelUsage } | null {
  const path: string[] = [];
  let node: ModelStreamEvent = event;
  while (node.type === "subagent_event") {
    path.push(node.callId);
    node = node.event;
  }
  if (node.type !== "usage_updated") return null;
  return { key: path.join("/"), round: node.round, usage: node.usage };
}

/** Cap per orphaned call: a call that never announces should cost a bounded
 * amount of memory, not a transcript's worth of buffered lifecycle noise. */
const PENDING_TOOL_EVENT_LIMIT = 32;
/** Cap on distinct orphaned calls per round: without it a hostile or broken
 * stream can allocate one 32-event queue per fabricated callId. Beyond the
 * cap, extra orphans get the old behavior — dropped, backstopped by the
 * settled-card promotion. */
const PENDING_TOOL_CALL_LIMIT = 16;

/** Holds a lifecycle event received before its call announcement. Reorderings
 * can occur during adoption replay and coalescer batching; replay it after the
 * announcement so a completed call remains visible live. */
function bufferOrphanToolEvent(run: ModelRunState, event: ToolStreamEvent, round: number): ModelRunState {
  const roundPending = run.pendingToolEventsByRound?.[round] ?? {};
  const queue = roundPending[event.callId] ?? [];
  if (queue.length >= PENDING_TOOL_EVENT_LIMIT) return run;
  if (queue.length === 0 && Object.keys(roundPending).length >= PENDING_TOOL_CALL_LIMIT) {
    return run;
  }
  return {
    ...run,
    pendingToolEventsByRound: {
      ...run.pendingToolEventsByRound,
      [round]: { ...roundPending, [event.callId]: [...queue, event] }
    }
  };
}

function replayPendingToolEvents(
  run: ModelRunState,
  callId: string,
  round: number,
  eventAt: string
): ModelRunState {
  const roundPending = run.pendingToolEventsByRound?.[round];
  const queue = roundPending?.[callId];
  if (!queue || queue.length === 0) return run;
  const { [callId]: _taken, ...rest } = roundPending;
  let next: ModelRunState = {
    ...run,
    pendingToolEventsByRound: { ...run.pendingToolEventsByRound, [round]: rest }
  };
  for (const pending of queue) {
    next = applyToolStreamEvent(next, pending, round, eventAt);
  }
  return next;
}

export function applyToolStreamEvent(
  run: ModelRunState,
  event: ToolStreamEvent,
  round: number,
  eventAt: string
): ModelRunState {
  const current = run.streamedToolsByRound[round] ?? [];
  const index = current.findIndex((tool) => tool.callId === event.callId);
  let next = current;

  switch (event.type) {
    case "tool_call_announced": {
      if (index >= 0) {
        if (current[index].toolName === event.toolName) return run;
        next = current.map((tool, toolIndex) =>
          toolIndex === index ? { ...tool, toolName: event.toolName } : tool
        );
      } else {
        next = [
          ...current,
          {
            // The host derives this id and sends it with the announcement, so
            // the streaming row and the card that eventually persists are the
            // same timeline entry. Older streams that predate the field fall
            // back to a local id; such a row still renders, it simply cannot
            // be reconciled with the host's card by id.
            id: event.contextId || createId("ctx"),
            callId: event.callId,
            toolName: event.toolName,
            input: {},
            result: {
              success: true,
              output: "",
              executedAt: eventAt,
              durationMs: 0
            },
            streamStatus: "announced",
            live: { contexts: [], updates: [] },
            createdAt: eventAt
          }
        ];
      }
      break;
    }
    case "tool_call_arguments_ready": {
      if (index < 0) return bufferOrphanToolEvent(run, event, round);
      next = current.map((tool, toolIndex) => {
        if (toolIndex !== index) return tool;
        const input = event.input;
        return {
          ...tool,
          requestedInput: tool.requestedInput ?? input,
          input,
          streamStatus: tool.streamStatus === "announced" ? "ready" : tool.streamStatus
        };
      });
      break;
    }
    case "tool_execution_started": {
      if (index < 0) return bufferOrphanToolEvent(run, event, round);
      next = current.map((tool, toolIndex) =>
        toolIndex === index && tool.streamStatus !== "completed" ? { ...tool, streamStatus: "running" } : tool
      );
      break;
    }
    case "tool_execution_completed": {
      if (index < 0) return bufferOrphanToolEvent(run, event, round);
      next = current.map((tool, toolIndex) =>
        toolIndex === index
          ? {
              ...tool,
              result: event.result,
              streamStatus: "completed"
            }
          : tool
      );
      break;
    }
  }

  const applied = {
    ...run,
    streamedToolsByRound: {
      ...run.streamedToolsByRound,
      [round]: next
    }
  };
  // A fresh announcement releases whatever this call buffered while orphaned.
  if (event.type === "tool_call_announced" && index < 0) {
    return replayPendingToolEvents(applied, event.callId, round, eventAt);
  }
  return applied;
}

const SUBAGENT_LIVE_CONTEXT_LIMIT = 200;
const SUBAGENT_LIVE_UPDATE_LIMIT = 50;
/**
 * Retained progress entries per workflow call. Above the host's own 1000-row
 * ledger bound because this array holds transitions, not merged rows: a
 * 50-step run emits roughly three entries per step plus narration, and the
 * limit should only ever bite on a runaway narrator.
 */
const WORKFLOW_PROGRESS_ENTRY_LIMIT = 4_000;

function nestedContextId(parentToolId: string, kind: string, nestedId: string | number): string {
  return `subagent-stream-${parentToolId}-${kind}-${nestedId}`;
}

/**
 * A round that ended without finishing its reasoning, on a card that has nothing
 * else in it.
 *
 * Encrypted reasoning is a presentation copy of a trace whose substance is the
 * provider's ciphertext, and a round that never reached its `done` frame
 * produced none: the card has no body, no duration, and nothing a later turn
 * could replay. Plaintext reasoning is its own content, so even a fragment of it
 * is a truthful record and stays.
 */
function abandonedEncryptedReasoning(context: ContextItem): boolean {
  return (
    context.kind === "reasoning" &&
    context.streaming === true &&
    (context.content ?? "").length === 0 &&
    isEncryptedReasoning(context)
  );
}

export function settleSubagentContexts(contexts: ContextItem[]): ContextItem[] {
  return contexts
    .filter((context) => !abandonedEncryptedReasoning(context))
    .map((context) => {
      if (context.kind === "tool" && context.streaming) {
        const { streaming: _streaming, streamStatus: _streamStatus, requestedInput, ...settled } = context;
        const canonicalRequestedInput =
          requestedInput && canonicalJson(requestedInput) !== canonicalJson(context.input)
            ? requestedInput
            : undefined;
        if (context.streamStatus === "completed") {
          return {
            ...settled,
            ...(canonicalRequestedInput ? { requestedInput: canonicalRequestedInput } : {})
          };
        }
        // A call that reported its own terminal lifecycle ended on its own terms,
        // whatever its stream phase says. `workflow_step` is the case that makes
        // this mandatory rather than defensive: `spawn_step` announces the
        // synthetic call and sends its identity, then never emits a
        // `tool_execution_completed` for it — a step's outcome travels as an agent
        // status delta, and its real result is only minted at the end of the run
        // by `synthesize_step_context`. Reading the missing completion as an
        // interruption therefore reported *every* step of *every* workflow as a
        // failure for the rest of the streaming turn, however well it went.
        const ownStatus = context.live?.status;
        if (ownStatus && ownStatus !== "running") {
          return {
            ...settled,
            ...(canonicalRequestedInput ? { requestedInput: canonicalRequestedInput } : {}),
            // `idle` is the clean ending; the placeholder result stands. Anything
            // else did end badly, so the row keeps its failure tone — but the
            // reason belongs to the child's lifecycle, not to a fabricated
            // "interrupted before it completed" that never happened.
            ...(ownStatus === "idle" ? {} : { result: { ...context.result, success: false } })
          };
        }
        return {
          ...settled,
          ...(canonicalRequestedInput ? { requestedInput: canonicalRequestedInput } : {}),
          result: {
            ...context.result,
            success: false,
            output:
              context.result.output ||
              translate(
                getI18nSnapshot().resolvedLanguage,
                "子代理工具调用在完成前被中断。",
                "The subagent tool call was interrupted before it completed."
              )
          }
        };
      }
      if (
        (context.kind === "assistant" || context.kind === "reasoning" || context.kind === "system") &&
        context.streaming
      ) {
        const { streaming: _streaming, ...settled } = context;
        return settled;
      }
      return context;
    });
}

/** Applies one parent-facing lifecycle/progress fragment to a single live
 * child. The same reducer is used at every nesting level so a dispatcher,
 * executor, and page reader retain identical streaming semantics. */
export function applySubagentChannelDelta(
  live: SubagentLiveState,
  parentToolId: string,
  channel: Extract<ModelStreamEvent, { type: "subagent_delta" }>["channel"],
  delta: string,
  eventAt: string
): SubagentLiveState {
  if (channel === "text") {
    return applyNestedSubagentEvent(live, parentToolId, { type: "text_delta", round: 0, delta }, eventAt);
  }
  if (channel === "reasoning") {
    return applyNestedSubagentEvent(
      live,
      parentToolId,
      { type: "reasoning_delta", round: 0, delta },
      eventAt
    );
  }
  if (channel === "update") {
    return {
      ...live,
      updates: [...live.updates, { content: delta, createdAt: eventAt }].slice(-SUBAGENT_LIVE_UPDATE_LIMIT)
    };
  }
  if (channel === "status") {
    // Empty status deltas are task_wait liveness heartbeats. Every other value
    // in `AgentLiveStatus::wire` is a real lifecycle transition: dropping the
    // terminal ones (failed/stopped/roundLimit) would leave the child pinned to
    // its last-known state and hide how the run actually ended.
    const status: SubagentLiveState["status"] =
      delta === "running" ||
      delta === "idle" ||
      delta === "interrupted" ||
      delta === "failed" ||
      delta === "stopped" ||
      delta === "roundLimit"
        ? delta
        : undefined;
    return status
      ? {
          ...live,
          ...(status === "running" ? {} : { contexts: settleSubagentContexts(live.contexts) }),
          status
        }
      : live;
  }
  const activity: ContextItem = {
    id: nestedContextId(parentToolId, "activity", `${eventAt}-${live.contexts.length}`),
    kind: "tool",
    toolName: "subagent_activity",
    input: { message: delta },
    result: {
      success: true,
      output: translate(getI18nSnapshot().resolvedLanguage, "活动已记录", "Activity recorded"),
      executedAt: eventAt,
      durationMs: 0
    },
    createdAt: eventAt
  };
  return {
    ...live,
    contexts: [...live.contexts, activity].slice(-SUBAGENT_LIVE_CONTEXT_LIMIT)
  };
}

function nestedReasoningId(parentToolId: string, round: number, item = 0): string {
  const base = nestedContextId(parentToolId, "reasoning", round);
  return item === 0 ? base : `${base}-item-${item}`;
}

/** Projects a normalized nested model event into the same ContextItem stream
 * consumed by the primary conversation renderer. */
export function applyNestedSubagentEvent(
  live: SubagentLiveState,
  parentToolId: string,
  event: ModelStreamEvent,
  eventAt: string
): SubagentLiveState {
  let contexts = live.contexts;
  let toolContextIds = live.toolContextIds ?? {};
  let usageByRound = live.usageByRound;
  const replace = (id: string, update: (context: ContextItem) => ContextItem) => {
    contexts = contexts.map((context) => (context.id === id ? update(context) : context));
  };

  if (event.type === "stream_retry_scheduled") {
    return { ...live, retryRound: event.round };
  }
  if (RETRY_RESET_EVENT_TYPES.has(event.type) && "round" in event && live.retryRound === event.round) {
    // Match the top-level boundary: backoff itself does not erase visible output.
    const assistantId = nestedContextId(parentToolId, "assistant", event.round);
    const reasoningId = nestedContextId(parentToolId, "reasoning", event.round);
    // The failed attempt's usage snapshot goes with them: the replacement
    // attempt reports its own cumulative figure for this round, and keeping
    // both would bill the child twice for one round it only ran once.
    const { [event.round]: _retriedUsage, ...keptUsage } = usageByRound ?? {};
    const cleared = {
      ...live,
      retryRound: undefined,
      contexts: contexts.filter((context) => {
        if (
          context.id === assistantId ||
          context.id === reasoningId ||
          context.id.startsWith(`${reasoningId}-item-`)
        )
          return false;
        return !(
          context.kind === "tool" &&
          context.round === event.round &&
          context.streaming &&
          (context.streamStatus === "announced" || context.streamStatus === "ready")
        );
      }),
      ...(usageByRound ? { usageByRound: keptUsage } : {})
    };
    const keptIds = new Set(cleared.contexts.map((context) => context.id));
    cleared.toolContextIds = Object.fromEntries(
      Object.entries(toolContextIds).filter(([, id]) => keptIds.has(id))
    );
    return applyNestedSubagentEvent(cleared, parentToolId, event, eventAt);
  }

  const modelTurnId = "round" in event ? `model-turn-${parentToolId}-${event.round}` : undefined;
  if (event.type === "text_delta") {
    const id = nestedContextId(parentToolId, "assistant", event.round);
    const existing = contexts.find((context) => context.id === id);
    if (existing?.kind === "assistant") {
      replace(id, (context) =>
        context.kind === "assistant"
          ? { ...context, content: `${context.content}${event.delta}`, streaming: true }
          : context
      );
    } else {
      contexts = [
        ...contexts,
        {
          id,
          kind: "assistant",
          content: event.delta,
          modelTurnId,
          round: event.round,
          streaming: true,
          createdAt: eventAt
        }
      ];
    }
  } else if (event.type === "reasoning_start") {
    // Create the reasoning card at `reasoning_start`: encrypted-only reasoning
    // never emits a delta, and its form must not be inferred from the parent run.
    const id = nestedReasoningId(parentToolId, event.round, event.item);
    if (!contexts.some((context) => context.id === id)) {
      contexts = [
        ...contexts,
        {
          id,
          kind: "reasoning",
          content: "",
          ...(event.form ? { form: event.form } : {}),
          modelTurnId,
          round: event.round,
          streaming: true,
          startedAt: eventAt,
          createdAt: eventAt
        }
      ];
    }
  } else if (event.type === "reasoning_delta") {
    const id = nestedReasoningId(parentToolId, event.round, event.item);
    const existing = contexts.find((context) => context.id === id);
    if (existing?.kind === "reasoning") {
      replace(id, (context) =>
        context.kind === "reasoning"
          ? { ...context, content: `${context.content ?? ""}${event.delta}`, streaming: true }
          : context
      );
    } else {
      contexts = [
        ...contexts,
        {
          id,
          kind: "reasoning",
          content: event.delta,
          modelTurnId,
          round: event.round,
          streaming: true,
          // Providers without `reasoning_start` use the first delta as the start.
          startedAt: eventAt,
          createdAt: eventAt
        }
      ];
    }
  } else if (event.type === "reasoning_done") {
    const id = nestedReasoningId(parentToolId, event.round, event.item);
    replace(id, (context) => (context.kind === "reasoning" ? { ...context, streaming: false } : context));
    if (event.durationMs != null) {
      replace(nestedReasoningId(parentToolId, event.round), (context) =>
        context.kind === "reasoning" ? { ...context, durationMs: event.durationMs } : context
      );
    }
  } else if (event.type === "tool_call_announced") {
    const assistantId = nestedContextId(parentToolId, "assistant", event.round);
    if (contexts.some((context) => context.id === assistantId)) {
      replace(assistantId, (context) =>
        context.kind === "assistant" ? { ...context, streaming: false } : context
      );
    } else {
      contexts = [
        ...contexts,
        {
          id: assistantId,
          kind: "assistant",
          content: "",
          modelTurnId,
          round: event.round,
          streaming: true,
          createdAt: eventAt
        }
      ];
    }
    // Same rule as the top-level rows: the host announces the id the child's
    // card will carry once it persists, so the live row adopts it instead of
    // minting a rival. A nested card lives inside the parent's fingerprinted
    // subagent record, so a mismatch here does not just duplicate a row — it
    // changes the parent's fingerprint and makes the parent card unsaveable.
    const id = toolContextIds[event.callId] ?? (event.contextId || createId("ctx"));
    toolContextIds = { ...toolContextIds, [event.callId]: id };
    if (!contexts.some((context) => context.id === id)) {
      contexts = [
        ...contexts,
        {
          id,
          kind: "tool",
          toolName: event.toolName,
          modelTurnId,
          round: event.round,
          input: {},
          result: { success: true, output: "", executedAt: eventAt, durationMs: 0 },
          streaming: true,
          streamStatus: "announced",
          createdAt: eventAt
        }
      ];
    }
  } else if (event.type === "tool_call_arguments_ready") {
    const id = toolContextIds[event.callId];
    if (!id) return live;
    replace(id, (context) => {
      if (context.kind !== "tool") return context;
      const input = event.input;
      return {
        ...context,
        requestedInput: context.requestedInput ?? input,
        input,
        streamStatus: context.streamStatus === "announced" ? "ready" : context.streamStatus
      };
    });
  } else if (event.type === "tool_execution_started") {
    const id = toolContextIds[event.callId];
    if (!id) return live;
    replace(id, (context) => (context.kind === "tool" ? { ...context, streamStatus: "running" } : context));
  } else if (event.type === "tool_execution_completed") {
    const id = toolContextIds[event.callId];
    if (!id) return live;
    replace(id, (context) =>
      context.kind === "tool"
        ? {
            ...context,
            result: event.result,
            streamStatus: "completed"
          }
        : context
    );
  } else if (event.type === "hook_execution_started") {
    const id = nestedContextId(parentToolId, "hook", event.executionId);
    if (!contexts.some((context) => context.id === id)) {
      const hookLabel = translate(getI18nSnapshot().resolvedLanguage, "生命周期钩子", "Lifecycle hook");
      const runningLabel = translate(getI18nSnapshot().resolvedLanguage, "执行中", "Running");
      contexts = [
        ...contexts,
        {
          id,
          kind: "system",
          content: `[${hookLabel} · ${event.event}]\n${event.hookName}\n${event.statusMessage ?? runningLabel}`,
          localOnly: true,
          hookExecution: {
            executionId: event.executionId,
            hookId: event.hookId,
            hookName: event.hookName,
            event: event.event,
            status: "running",
            contextInjected: false
          },
          streaming: true,
          createdAt: eventAt
        }
      ];
    }
  } else if (event.type === "hook_execution_completed") {
    const id = nestedContextId(parentToolId, "hook", event.executionId);
    const status = event.blocked ? "blocked" : event.result.success ? "succeeded" : "failed";
    const hookLabel = translate(getI18nSnapshot().resolvedLanguage, "生命周期钩子", "Lifecycle hook");
    const statusLabel = event.blocked
      ? translate(getI18nSnapshot().resolvedLanguage, "已阻止当前操作", "Blocked the current operation")
      : event.result.success
        ? translate(getI18nSnapshot().resolvedLanguage, "执行完成", "Completed")
        : translate(getI18nSnapshot().resolvedLanguage, "执行错误（未阻断）", "Failed without blocking");
    const content = `[${hookLabel} · ${event.event}]\n${event.hookName}\n${event.result.output || statusLabel}${event.reason ? `\n\n${event.reason}` : ""}`;
    const next = (context: ContextItem): ContextItem =>
      context.kind === "system"
        ? {
            ...context,
            content,
            streaming: false,
            hookExecution: {
              executionId: event.executionId,
              hookId: event.hookId,
              hookName: event.hookName,
              event: event.event,
              status,
              contextInjected: event.contextInjected
            }
          }
        : context;
    if (contexts.some((context) => context.id === id)) replace(id, next);
    else {
      contexts = [
        ...contexts,
        next({
          id,
          kind: "system",
          content,
          localOnly: true,
          createdAt: eventAt
        })
      ];
    }
  } else if (event.type === "usage_updated") {
    // The child's own provider snapshot. Keyed by round and replaced rather
    // than accumulated: each snapshot is already cumulative for its round, so
    // adding them would multiply the child's cost by the number of chunks its
    // provider happened to report usage in.
    usageByRound = { ...usageByRound, [event.round]: event.usage };
    // Reasoning tokens belong to this round. Child contexts have no other
    // round-to-usage mapping, and zero is omitted.
    const reasoningTokens = event.usage.reasoningTokens;
    if (reasoningTokens) {
      replace(nestedContextId(parentToolId, "reasoning", event.round), (context) =>
        context.kind === "reasoning" ? { ...context, tokens: reasoningTokens } : context
      );
    }
  } else if (event.type === "subagent_event" || event.type === "subagent_delta") {
    // Nested agent events are correlated to the run-tool context announced in
    // this child's own stream. Preserve a live state on that context and feed
    // the payload through this same reducer, recursively. This is what makes a
    // running search group visible as dispatcher → executor → page reader
    // before the backend settles and persists the final transcripts.
    //
    // Usage rides the same recursion: a workflow step's snapshot arrives
    // wrapped twice (run envelope, then step envelope), so it lands on the
    // step's own live state exactly like its text does.
    const id = toolContextIds[event.callId];
    if (!id) return live;
    replace(id, (context) => {
      if (context.kind !== "tool") return context;
      const childLive = context.live ?? { contexts: [], updates: [] };
      return {
        ...context,
        live:
          event.type === "subagent_event"
            ? applyNestedSubagentEvent(childLive, context.id, event.event, eventAt)
            : applySubagentChannelDelta(childLive, context.id, event.channel, event.delta, eventAt)
      };
    });
  }

  return {
    ...live,
    contexts: contexts.slice(-SUBAGENT_LIVE_CONTEXT_LIMIT),
    toolContextIds,
    ...(usageByRound ? { usageByRound } : {})
  };
}

/**
 * Append one workflow progress row to its call's stream.
 *
 * Deliberately append-only: the merge lives in `deriveWorkflowProgress`, which
 * is pure and therefore testable without a React tree. Folding here instead
 * would put the card's whole merge rule inside a reducer that only a rendered
 * component can exercise.
 *
 * The stream is bounded, because one entry per step transition over a long run
 * accumulates for the whole turn. Only LOG entries are evicted — never agent
 * ones, mirroring the host ledger's rule. Dropping the oldest entries wholesale
 * would be wrong here in a way it is not on the host: the host stores one
 * merged row per step, while this array stores every transition, so evicting
 * the front of it can delete every trace of a step that finished early. That
 * step would vanish from the card mid-run, and the totals with it.
 */
export function applyWorkflowProgress(
  run: ModelRunState,
  event: Extract<ModelStreamEvent, { type: "workflow_progress" }>
): ModelRunState {
  const current = run.workflowProgressByCall[event.callId] ?? [];
  let appended = [...current, event.entry];
  if (appended.length > WORKFLOW_PROGRESS_ENTRY_LIMIT) {
    let excess = appended.length - WORKFLOW_PROGRESS_ENTRY_LIMIT;
    appended = appended.filter((entry) => {
      if (excess > 0 && entry.kind === "log") {
        excess -= 1;
        return false;
      }
      return true;
    });
  }
  return {
    ...run,
    workflowProgressByCall: {
      ...run.workflowProgressByCall,
      [event.callId]: appended
    },
    // Last writer wins, and there is only ever one: the host mints a run id per
    // workflow call and reuses it for every transition of that run, including
    // across a resume.
    workflowRunIdByCall: {
      ...run.workflowRunIdByCall,
      [event.callId]: event.runId
    }
  };
}

/**
 * Total live transcript rows the renderer keeps across every background child
 * of one run, on top of each child's own {@link SUBAGENT_LIVE_CONTEXT_LIMIT}.
 *
 * The per-child cap alone bounds nothing in aggregate: ten children hold ten
 * times as much, and a workflow fan-out holds as much as it likes. This is the
 * ceiling that actually holds.
 */
export const SUBAGENT_LIVE_TOTAL_CONTEXT_BUDGET = 1_200;

interface LiveTrimTarget {
  /** Nesting level; a deeper child is dropped before a shallower one. */
  depth: number;
  /** Arrival order among equal depths; the earliest is dropped first. */
  order: number;
  live: SubagentLiveState;
}

/** Walks one live subtree, appending every node in arrival order. */
function collectLiveTargets(live: SubagentLiveState, depth: number, targets: LiveTrimTarget[]): void {
  targets.push({ depth, order: targets.length, live });
  live.contexts.forEach((context) => {
    if (context.kind === "tool" && context.live) {
      collectLiveTargets(context.live, depth + 1, targets);
    }
  });
}

/** Rebuilds one live subtree, applying whatever drop count it was assigned. */
function applyLiveDrops(live: SubagentLiveState, drops: Map<SubagentLiveState, number>): SubagentLiveState {
  const drop = drops.get(live) ?? 0;
  const kept = drop > 0 ? live.contexts.slice(drop) : live.contexts;
  let changed = drop > 0;
  const contexts = kept.map((context) => {
    if (context.kind !== "tool" || !context.live) return context;
    const nested = applyLiveDrops(context.live, drops);
    if (nested === context.live) return context;
    changed = true;
    return { ...context, live: nested };
  });
  return changed ? { ...live, contexts } : live;
}

/**
 * Enforces {@link SUBAGENT_LIVE_TOTAL_CONTEXT_BUDGET} over one run's whole
 * child tree.
 *
 * Eviction order is deepest-first, then oldest-first: a deep child's transcript
 * is the least likely thing the user is reading (it sits several disclosures
 * down), and within one depth the earliest rows are the ones already scrolled
 * past. Rows are dropped from the front of a child's own transcript rather than
 * dropping the child, so every agent keeps its identity, its status and its
 * most recent output no matter how far over budget the run goes.
 *
 * Returns the same object when nothing had to be dropped, so the common case
 * costs one traversal and no allocation.
 */
export function trimLiveContextBudget(run: ModelRunState): ModelRunState {
  const targets: LiveTrimTarget[] = [];
  Object.values(run.streamedToolsByRound).forEach((tools) => {
    tools.forEach((tool) => {
      collectLiveTargets(tool.live, 0, targets);
    });
  });

  let total = targets.reduce((sum, target) => sum + target.live.contexts.length, 0);
  if (total <= SUBAGENT_LIVE_TOTAL_CONTEXT_BUDGET) return run;

  const drops = new Map<SubagentLiveState, number>();
  // Deepest first, then oldest first.
  const ordered = [...targets].sort((left, right) => right.depth - left.depth || left.order - right.order);
  for (const target of ordered) {
    if (total <= SUBAGENT_LIVE_TOTAL_CONTEXT_BUDGET) break;
    // Always leave one row so an evicted child still shows what it last did.
    const drop = Math.min(
      total - SUBAGENT_LIVE_TOTAL_CONTEXT_BUDGET,
      Math.max(0, target.live.contexts.length - 1)
    );
    if (drop <= 0) continue;
    total -= drop;
    drops.set(target.live, drop);
  }
  if (!drops.size) return run;

  const streamedToolsByRound: ModelRunState["streamedToolsByRound"] = {};
  Object.entries(run.streamedToolsByRound).forEach(([round, tools]) => {
    streamedToolsByRound[Number(round)] = tools.map((tool) => {
      const live = applyLiveDrops(tool.live, drops);
      return live === tool.live ? tool : { ...tool, live };
    });
  });
  return { ...run, streamedToolsByRound };
}

export function applySubagentDelta(
  run: ModelRunState,
  event: Extract<ModelStreamEvent, { type: "subagent_delta" }>,
  round: number,
  eventAt: string
): ModelRunState {
  const current = run.streamedToolsByRound[round] ?? [];
  const index = current.findIndex((tool) => tool.callId === event.callId);
  if (index < 0) return run;
  const next = current.map((tool, toolIndex) => {
    if (toolIndex !== index) return tool;
    return {
      ...tool,
      live: applySubagentChannelDelta(tool.live, tool.id, event.channel, event.delta, eventAt)
    };
  });
  return {
    ...run,
    streamedToolsByRound: { ...run.streamedToolsByRound, [round]: next }
  };
}

export function applySubagentEvent(
  run: ModelRunState,
  event: Extract<ModelStreamEvent, { type: "subagent_event" }>,
  round: number,
  eventAt: string
): ModelRunState {
  const current = run.streamedToolsByRound[round] ?? [];
  const index = current.findIndex((tool) => tool.callId === event.callId);
  if (index < 0) return run;
  const next = current.map((tool, toolIndex) =>
    toolIndex === index
      ? {
          ...tool,
          live: applyNestedSubagentEvent(tool.live, tool.id, event.event, eventAt)
        }
      : tool
  );
  return {
    ...run,
    streamedToolsByRound: { ...run.streamedToolsByRound, [round]: next }
  };
}

export function applyHookStreamEvent(
  run: ModelRunState,
  event: HookStreamEvent,
  round: number,
  eventAt: string
): ModelRunState {
  const current = run.streamedHooksByRound[round] ?? [];
  const index = current.findIndex((hook) => hook.executionId === event.executionId);
  let next = current;
  if (event.type === "hook_execution_started") {
    if (index >= 0) return run;
    next = [
      ...current,
      {
        executionId: event.executionId,
        hookId: event.hookId,
        hookName: event.hookName,
        event: event.event,
        statusMessage: event.statusMessage,
        result: { success: true, output: "", executedAt: eventAt, durationMs: 0 },
        status: "running",
        contextInjected: false,
        createdAt: eventAt
      }
    ];
  } else if (index >= 0) {
    next = current.map((hook, hookIndex) =>
      hookIndex === index
        ? {
            ...hook,
            result: event.result,
            status: event.blocked ? "blocked" : event.result.success ? "succeeded" : "failed",
            contextInjected: event.contextInjected,
            reason: event.reason
          }
        : hook
    );
  } else {
    next = [
      ...current,
      {
        executionId: event.executionId,
        hookId: event.hookId,
        hookName: event.hookName,
        event: event.event,
        result: event.result,
        status: event.blocked ? "blocked" : event.result.success ? "succeeded" : "failed",
        contextInjected: event.contextInjected,
        reason: event.reason,
        createdAt: eventAt
      }
    ];
  }
  return {
    ...run,
    streamedHooksByRound: { ...run.streamedHooksByRound, [round]: next }
  };
}

/** Effects emitted after a stream event is folded. Reducers never perform them;
 * the caller applies them to external systems in emission order. */
export type ModelStreamEffect =
  /** A queued message was delivered during the stream; split the completed turn
   * before the input using `runBeforeInput`. */
  | { kind: "turn_split"; item: UserContext; runBeforeInput: ModelRunState }
  /** Clear the delivered message's steering marker. */
  | { kind: "steering_delivered"; contextId: string }
  /** Publish cumulative usage for the active turn. */
  | { kind: "turn_usage"; usage: ModelUsage; revision: number }
  /** A tool execution completed; the interpreter may trigger related work. */
  | { kind: "tool_completed"; toolName: string | undefined; success: boolean; output: string }
  /** A host-settled tool card replaces its persisted counterpart by id. */
  | { kind: "tool_context_settled"; context: Extract<ContextItem, { kind: "tool" }> };

export interface ModelStreamReduction {
  run: ModelRunState;
  effects: ModelStreamEffect[];
}

/** Event types whose first post-retry arrival discards the failed attempt's
 * partial round. Subagent and hook events are excluded: their `round` refers
 * to the spawning round, not the request being retried. */
const RETRY_RESET_EVENT_TYPES = new Set<ModelStreamEvent["type"]>([
  "text_delta",
  "reasoning_start",
  "reasoning_delta",
  "reasoning_done",
  "usage_updated",
  "tool_call_announced",
  "tool_call_arguments_ready",
  "tool_execution_started",
  "tool_execution_completed"
]);

/**
 * The duration to record for a round whose reasoning just closed without the
 * sidecar reporting one.
 *
 * A settled reasoning card states how long the round thought for, and it states
 * it from `durationMs` alone — `startedAt` is renderer-only and never reaches
 * disk, so a card that kept counting from a start timestamp would read the
 * elapsed time since the conversation was loaded once it came back. Measuring
 * the close here gives every completed round a figure of its own; a sidecar
 * figure still wins, because it times the provider rather than the transport.
 *
 * Returns nothing when there is no start to measure from, or when a figure is
 * already recorded — reopening reasoning must not restart the clock.
 */
function closedReasoningDuration(
  run: ModelRunState,
  round: number,
  eventAt: string
): Pick<ModelRunState, "reasoningDurationByRound"> | Record<string, never> {
  if (run.reasoningDurationByRound[round] != null) return {};
  const startedAt = run.reasoningStartedAtByRound[round];
  if (startedAt === undefined) return {};
  const started = Date.parse(startedAt);
  const closed = Date.parse(eventAt);
  if (!Number.isFinite(started) || !Number.isFinite(closed)) return {};
  return {
    reasoningDurationByRound: {
      ...run.reasoningDurationByRound,
      [round]: Math.max(0, closed - started)
    }
  };
}

/** Folds one model-stream event into run state. This is pure; callers verify the
 * request id and handle approval and ping events before invoking it. */
export function reduceModelStreamEvent(
  run: ModelRunState,
  event: ModelStreamEvent,
  eventAt: string
): ModelStreamReduction {
  const effects: ModelStreamEffect[] = [];
  const round = "round" in event && Number.isInteger(event.round) && event.round >= 0 ? event.round : 0;

  if (event.type === "user_input_received") {
    const item: UserContext = {
      id: event.id,
      kind: "user",
      content: event.content,
      images: event.images,
      createdAt: event.createdAt
    };
    const duplicateAnywhere = Object.values(run.steeredInputsByRound).some((inputs) =>
      inputs.some((input) => input.id === item.id)
    );
    if (!duplicateAnywhere) {
      effects.push({ kind: "turn_split", item, runBeforeInput: run });
    }
    effects.push({ kind: "steering_delivered", contextId: item.id });
    const currentInputs = run.steeredInputsByRound[round] ?? [];
    const next = currentInputs.some((input) => input.id === item.id)
      ? run
      : {
          ...run,
          steeredInputsByRound: {
            ...run.steeredInputsByRound,
            [round]: [...currentInputs, item]
          }
        };
    return { run: next, effects };
  }

  if (event.type === "stream_retry_scheduled") {
    return {
      run: {
        ...run,
        retry: {
          round,
          attempt: event.attempt,
          maxAttempts: event.maxAttempts,
          message: event.message,
          dirty: true
        }
      },
      effects
    };
  }

  let current = run;
  if (
    RETRY_RESET_EVENT_TYPES.has(event.type) &&
    current.retry &&
    current.retry.dirty &&
    current.retry.round === round
  ) {
    current = { ...resetRoundForRetry(current, round), retry: undefined };
    effects.push({
      kind: "turn_usage",
      usage: cumulativeModelRunUsage(current),
      revision: current.usageRevision
    });
  }

  if (event.type === "usage_updated") {
    const next = {
      ...current,
      usageByRound: { ...current.usageByRound, [round]: event.usage },
      usageRevision: current.usageRevision + 1
    };
    effects.push({
      kind: "turn_usage",
      usage: cumulativeModelRunUsage(next),
      revision: next.usageRevision
    });
    return { run: next, effects };
  }

  if (
    event.type === "tool_call_announced" ||
    event.type === "tool_call_arguments_ready" ||
    event.type === "tool_execution_started" ||
    event.type === "tool_execution_completed"
  ) {
    const knownRow = (current.streamedToolsByRound[round] ?? []).find((tool) => tool.callId === event.callId);
    if (event.type === "tool_execution_completed" && knownRow) {
      effects.push({
        kind: "tool_completed",
        toolName: knownRow.toolName,
        success: event.result.success,
        output: event.result.output
      });
    }
    // Do not emit completion effects for orphaned events. Replay after the
    // announcement, when the tool name is available.
    if (event.type === "tool_call_announced" && !knownRow) {
      const buffered = current.pendingToolEventsByRound?.[round]?.[event.callId] ?? [];
      const completed = [...buffered]
        .reverse()
        .find(
          (pending): pending is Extract<ToolStreamEvent, { type: "tool_execution_completed" }> =>
            pending.type === "tool_execution_completed"
        );
      if (completed) {
        effects.push({
          kind: "tool_completed",
          toolName: event.toolName,
          success: completed.result.success,
          output: completed.result.output
        });
      }
    }
    return { run: applyToolStreamEvent(current, event, round, eventAt), effects };
  }

  if (event.type === "subagent_event" || event.type === "subagent_delta") {
    const withEvent =
      event.type === "subagent_event"
        ? applySubagentEvent(current, event, round, eventAt)
        : applySubagentDelta(current, event, round, eventAt);
    const nested = event.type === "subagent_event" ? nestedUsageUpdate(event) : null;
    if (!nested) return { run: withEvent, effects };
    const next = {
      ...withEvent,
      subagentUsageByCall: {
        ...withEvent.subagentUsageByCall,
        [nested.key]: {
          ...withEvent.subagentUsageByCall[nested.key],
          [nested.round]: nested.usage
        }
      },
      usageRevision: withEvent.usageRevision + 1
    };
    effects.push({
      kind: "turn_usage",
      usage: cumulativeModelRunUsage(next),
      revision: next.usageRevision
    });
    return { run: next, effects };
  }

  if (event.type === "workflow_progress") {
    return { run: applyWorkflowProgress(current, event), effects };
  }

  if (event.type === "tool_context_settled") {
    // Usually only persistence changes. If the completion event was absent,
    // advance a lingering live row to the settled terminal state so it cannot
    // hide the persisted card.
    effects.push({ kind: "tool_context_settled", context: event.context });
    const settled = event.context;
    if (settled.kind === "tool") {
      for (const [roundKey, tools] of Object.entries(current.streamedToolsByRound)) {
        const index = tools.findIndex(
          (tool) =>
            tool.id === settled.id &&
            (tool.streamStatus === "announced" ||
              tool.streamStatus === "ready" ||
              tool.streamStatus === "running")
        );
        if (index < 0) continue;
        return {
          run: {
            ...current,
            streamedToolsByRound: {
              ...current.streamedToolsByRound,
              [roundKey]: tools.map((tool, toolIndex) =>
                toolIndex === index
                  ? {
                      ...tool,
                      input: settled.input,
                      result: settled.result,
                      streamStatus: "completed" as const
                    }
                  : tool
              )
            }
          },
          effects
        };
      }
    }
    return { run: current, effects };
  }

  if (event.type === "hook_execution_started" || event.type === "hook_execution_completed") {
    return { run: applyHookStreamEvent(current, event, round, eventAt), effects };
  }

  if (event.type === "reasoning_start" || (event.type === "reasoning_delta" && event.delta)) {
    current = {
      ...current,
      activeReasoningItemsByRound: {
        ...current.activeReasoningItemsByRound,
        [round]: { ...current.activeReasoningItemsByRound?.[round], [event.item ?? 0]: true }
      },
      completedReasoningByRound: { ...current.completedReasoningByRound, [round]: false }
    };
  }

  if (event.type === "reasoning_start") {
    const item = event.item ?? 0;
    if (event.form) {
      current = {
        ...current,
        reasoningFormsByRound: {
          ...current.reasoningFormsByRound,
          [round]: { ...current.reasoningFormsByRound?.[round], [item]: event.form }
        }
      };
    }
    // Open an empty slot for this item. Encrypted-only reasoning emits no text,
    // so the slot is its only live-row evidence.
    const items = current.streamedReasoningByRound[round] ?? [];
    const withSlot =
      items.length > item
        ? current.streamedReasoningByRound
        : {
            ...current.streamedReasoningByRound,
            [round]: [...items, ...Array.from({ length: item + 1 - items.length }, () => "")]
          };
    // Preserve the first start time. Providers can emit one start per summary
    // segment, but the live clock covers the entire reasoning step.
    if (current.reasoningStartedAtByRound[round]) {
      if (withSlot === current.streamedReasoningByRound) return { run: current, effects };
      return {
        run: {
          ...current,
          streamedReasoningByRound: withSlot,
          completedReasoningByRound: {
            ...current.completedReasoningByRound,
            [round]: false
          }
        },
        effects
      };
    }
    return {
      run: {
        ...current,
        streamedReasoningByRound: withSlot,
        reasoningStartedAtByRound: {
          ...current.reasoningStartedAtByRound,
          [round]: eventAt
        },
        completedReasoningByRound: {
          ...current.completedReasoningByRound,
          [round]: false
        }
      },
      effects
    };
  }

  if (event.type === "reasoning_done") {
    // Item completion must not close another interleaved item's live surface.
    const activeItems = { ...current.activeReasoningItemsByRound?.[round], [event.item ?? 0]: false };
    const completed = !Object.values(activeItems).some(Boolean);
    const opened = current.reasoningStartedAtByRound[round] != null;
    if (!opened && (current.streamedReasoningByRound[round] ?? []).length === 0) {
      return { run: current, effects };
    }
    return {
      run: {
        ...current,
        activeReasoningItemsByRound: { ...current.activeReasoningItemsByRound, [round]: activeItems },
        completedReasoningByRound: {
          ...current.completedReasoningByRound,
          [round]: completed
        },
        // The sidecar reports cumulative duration for this step, so replacing
        // the value is idempotent.
        ...(event.durationMs == null
          ? closedReasoningDuration(current, round, eventAt)
          : {
              reasoningDurationByRound: {
                ...current.reasoningDurationByRound,
                [round]: event.durationMs
              }
            })
      },
      effects
    };
  }

  if (event.type === "reasoning_delta") {
    if (!event.delta) return { run: current, effects };
    const item = event.item ?? 0;
    const items = [...(current.streamedReasoningByRound[round] ?? [])];
    while (items.length <= item) items.push("");
    items[item] = `${items[item]}${event.delta}`;
    return {
      run: {
        ...current,
        streamedReasoningByRound: {
          ...current.streamedReasoningByRound,
          [round]: items
        },
        // Providers without `reasoning_start` use the first delta as the start.
        reasoningStartedAtByRound: current.reasoningStartedAtByRound[round]
          ? current.reasoningStartedAtByRound
          : { ...current.reasoningStartedAtByRound, [round]: eventAt },
        completedReasoningByRound: {
          ...current.completedReasoningByRound,
          [round]: false
        }
      },
      effects
    };
  }

  if (event.type === "text_delta") {
    if (!event.delta) return { run: current, effects };
    // Text begins after reasoning. A reasoning start timestamp also covers
    // encrypted-only reasoning with no visible text.
    const closesReasoning =
      (current.streamedReasoningByRound[round] ?? []).length > 0 ||
      current.reasoningStartedAtByRound[round] != null;
    return {
      run: {
        ...current,
        streamedTextByRound: {
          ...current.streamedTextByRound,
          [round]: `${current.streamedTextByRound[round] ?? ""}${event.delta}`
        },
        completedReasoningByRound: {
          ...current.completedReasoningByRound,
          ...(closesReasoning ? { [round]: true } : {})
        },
        ...(closesReasoning
          ? {
              activeReasoningItemsByRound: { ...current.activeReasoningItemsByRound, [round]: {} },
              ...closedReasoningDuration(current, round, eventAt)
            }
          : {})
      },
      effects
    };
  }

  return { run: current, effects };
}

export interface QueuedModelStreamEvent {
  event: ModelStreamEvent;
  /** Arrival timestamp. Batching preserves it instead of using flush time. */
  at: string;
}

export interface ModelStreamCoalescer {
  push(event: ModelStreamEvent): void;
  /** Immediately commits the buffered events as one batch, if any. */
  flush(): void;
  /** Discards uncommitted events and cancels the timer after the run ends. */
  dispose(): void;
}

type CancelScheduled = () => void;
type Scheduler = (flush: () => void) => CancelScheduled;

/** Fixed commit cadence: 10 FPS. */
export const MODEL_STREAM_COMMIT_INTERVAL_MS = 100;

/**
 * Schedules commits at a fixed 10 FPS. Decoupling rendering from event arrival
 * bounds projection and Markdown work while preserving every event in each batch.
 */
const intervalScheduler: Scheduler = (flush) => {
  const timeoutId = setTimeout(flush, MODEL_STREAM_COMMIT_INTERVAL_MS);
  return () => clearTimeout(timeoutId);
};

/**
 * Batches model-stream events at a fixed 10 FPS. `commit` receives events in
 * arrival order, then applies their effects in that order.
 * Every event enters one buffer, so batching changes commit frequency only.
 */
export function createModelStreamCoalescer(
  commit: (batch: QueuedModelStreamEvent[]) => void,
  schedule: Scheduler = intervalScheduler
): ModelStreamCoalescer {
  let pending: QueuedModelStreamEvent[] = [];
  let cancelScheduled: CancelScheduled | null = null;

  const flushPending = () => {
    cancelScheduled = null;
    if (!pending.length) return;
    const batch = pending;
    pending = [];
    commit(batch);
  };

  return {
    push(event) {
      pending.push({ event, at: new Date().toISOString() });
      cancelScheduled ??= schedule(flushPending);
    },
    flush() {
      cancelScheduled?.();
      flushPending();
    },
    dispose() {
      cancelScheduled?.();
      cancelScheduled = null;
      pending = [];
    }
  };
}
