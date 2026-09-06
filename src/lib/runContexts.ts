import { getI18nSnapshot, translate } from "../i18n";
import type { ContextItem, Conversation, ToolContext } from "../types";
import { canonicalJson } from "./contextTokens";
import { isEncryptedReasoning } from "./modelCapabilities";
import type { ModelRunState, StreamingHookState, StreamingToolState } from "./modelStream";

/**
 * Derives the timeline identity for one round's prose (answer or reasoning).
 * The renderer and host use the same requestId-and-round inputs so streaming and
 * persisted contexts refer to the same timeline entry.
 */
export function roundProseContextId(
  requestId: string,
  round: number,
  kind: "assistant" | "reasoning"
): string {
  return `ctx_${kind}_${requestId}_${round}`;
}

/** Shared grouping identity for one round's output. The host derives the same value. */
export function roundModelTurnId(requestId: string, round: number): string {
  return `model-turn-${requestId}-${round}`;
}

/**
 * Derives the identity for a round's `segment`-th reasoning item. It must match
 * the host's `reasoning_segment_identity`: segment zero uses the round base id
 * and later segments append `_<segment>`, allowing live rows to replace settled cards.
 */
export function reasoningSegmentContextId(
  requestId: string,
  round: number,
  segment: number
): string {
  const base = roundProseContextId(requestId, round, "reasoning");
  return segment === 0 ? base : `${base}_${segment}`;
}

export function hookContextId(requestId: string, executionId: string, purpose: "display" | "injection", index = 0): string {
  return `ctx_hook_${requestId}_${executionId}_${purpose}_${index}`;
}

/**
 * Context-id prefixes that may have been produced exclusively by this run.
 * Prose and hook contexts share host-derived ids.
 */
export function runStreamContextPrefixes(requestId: string): string[] {
  return [
    `ctx_assistant_${requestId}_`,
    `ctx_reasoning_${requestId}_`,
    `ctx_hook_${requestId}_`
  ];
}

function hookStreamContext(run: ModelRunState, hook: StreamingHookState, streaming: boolean): ContextItem {
  const headline = hook.status === "running"
    ? hook.statusMessage ?? translate(getI18nSnapshot().resolvedLanguage, "正在执行", "Running")
    : hook.status === "blocked"
      ? translate(getI18nSnapshot().resolvedLanguage, "已阻止当前操作", "Blocked the current operation")
      : hook.status === "failed"
        ? translate(getI18nSnapshot().resolvedLanguage, "执行错误（未阻断）", "Failed without blocking")
        : translate(getI18nSnapshot().resolvedLanguage, "执行完成", "Completed");
  const details = hook.result.output || headline;
  const hookLabel = translate(getI18nSnapshot().resolvedLanguage, "生命周期钩子", "Lifecycle hook");
  return {
    id: hookContextId(run.requestId, hook.executionId, "display"),
    kind: "system",
    content: `[${hookLabel} · ${hook.event}]\n${hook.hookName}\n${headline}${hook.reason ? `\n\n${hook.reason}` : ""}${details === headline ? "" : `\n\n${details}`}`,
    localOnly: true,
    hookExecution: {
      executionId: hook.executionId,
      hookId: hook.hookId,
      hookName: hook.hookName,
      event: hook.event,
      status: hook.status,
      contextInjected: hook.contextInjected
    },
    ...(streaming && hook.status === "running" ? { streaming: true } : {}),
    createdAt: hook.createdAt
  };
}

/**
 * The browser action the model is currently driving, for the task card's
 * "Model is driving: …" line and the stop-automation affordance.
 *
 * The whole browser surface is one `playwright` tool now, so returning the tool
 * name would make every automation read the same. The action is what the reader
 * needs; a call whose arguments have not finished streaming falls back to the
 * tool name rather than reporting nothing.
 */
export function browserAutomationToolForRun(run: ModelRunState | undefined): string | null {
  if (!run) return null;
  let latest: string | null = null;
  const rounds = Object.keys(run.streamedToolsByRound)
    .map(Number)
    .filter(Number.isFinite)
    .sort((left, right) => left - right);
  for (const round of rounds) {
    for (const tool of run.streamedToolsByRound[round] ?? []) {
      if (
        tool.toolName === "playwright"
        && (tool.streamStatus === "running" || tool.streamStatus === "completed")
      ) {
        latest = playwrightAutomationLabel(tool.input);
      }
      for (const context of tool.live.contexts) {
        if (
          context.kind === "tool"
          && context.toolName === "playwright"
          && (context.streamStatus === "running" || context.streamStatus === "completed")
        ) {
          latest = playwrightAutomationLabel(context.input);
        }
      }
    }
  }
  return latest;
}

function playwrightAutomationLabel(input: unknown): string {
  const action = (input as Record<string, unknown> | undefined)?.action;
  return typeof action === "string" && action.trim() ? action.trim() : "playwright";
}

/**
 * The reasoning a run is doing right now, for the stream indicator beside the
 * cat. Derived from run state rather than from the projected contexts because
 * encrypted reasoning with no summary has no context at all until it finishes —
 * this line is the only surface it ever gets.
 */
export interface LiveReasoningView {
  /** When this round's reasoning opened. */
  startedAt: string;
  /** Reasoning tokens the provider has reported for the round so far. Absent until it does. */
  tokens?: number;
}

export function liveReasoningFromModelRun(run: ModelRunState | undefined): LiveReasoningView | null {
  if (!run) return null;
  // The newest open round wins: an earlier round's reasoning closed when its
  // text or its tool calls began, whether or not a `reasoning_done` said so.
  let live: { round: number; startedAt: string } | null = null;
  for (const [key, startedAt] of Object.entries(run.reasoningStartedAtByRound)) {
    const round = Number(key);
    if (run.completedReasoningByRound[round]) continue;
    if (!live || round > live.round) live = { round, startedAt };
  }
  if (!live) return null;
  const tokens = run.usageByRound[live.round]?.reasoningTokens;
  return { startedAt: live.startedAt, ...(tokens ? { tokens } : {}) };
}

export function contextsFromModelRun(
  run: ModelRunState,
  streaming: boolean
): ContextItem[] {
  // Use the request's actual model so live and settled reasoning cards use the
  // same form.
  const reasoningForm = run.request.model.reasoningContent;
  const rounds = [...new Set([
    ...Object.keys(run.streamedReasoningByRound),
    // Encrypted-only reasoning has no streamed text, so its start timestamp
    // must contribute a timeline round.
    ...Object.keys(run.reasoningStartedAtByRound),
    // Some providers report only reasoning-token usage; include those rounds so
    // their settled metadata card does not appear only after the run ends.
    ...Object.entries(run.usageByRound)
      .filter(([, usage]) => (usage.reasoningTokens ?? 0) > 0)
      .map(([round]) => round),
    ...Object.keys(run.streamedTextByRound),
    ...Object.keys(run.streamedToolsByRound),
    ...Object.keys(run.streamedHooksByRound),
    ...Object.keys(run.steeredInputsByRound)
  ].map(Number))].sort((left, right) => left - right);

  return rounds.flatMap<ContextItem>((round) => {
    const contexts: ContextItem[] = [];
    const modelTurnId = roundModelTurnId(run.requestId, round);
    const reasoningItems = run.streamedReasoningByRound[round] ?? [];
    const reasoningStartedAt = run.reasoningStartedAtByRound[round];
    const text = run.streamedTextByRound[round];
    const tools = run.streamedToolsByRound[round] ?? [];
    const hooks = run.streamedHooksByRound[round] ?? [];
    const steeredInputs = run.steeredInputsByRound[round] ?? [];
    contexts.push(...steeredInputs);
    const preflightHooks = hooks.filter((hook) => ["SessionStart", "UserPromptSubmit"].includes(hook.event));
    const preToolHooks = hooks.filter((hook) => ["PreToolUse", "PermissionRequest"].includes(hook.event));
    const postToolHooks = hooks.filter((hook) => hook.event === "PostToolUse");
    const stopHooks = hooks.filter((hook) => hook.event === "Stop");
    contexts.push(...preflightHooks.map((hook) => hookStreamContext(run, hook, streaming)));
    // Reasoning text is not the only evidence that a reasoning card exists:
    // encrypted-only reasoning has a start timestamp, and token-only reasoning
    // has usage metadata.
    const reasoningTokens = run.usageByRound[round]?.reasoningTokens;
    // A token-only card has neither a start nor a delta, so it is not live.
    const reasoningOpened = reasoningItems.length > 0 || reasoningStartedAt != null;
    const reasoningCompleted = run.completedReasoningByRound[round] === true;
    const reasoningBody = reasoningItems.some((text) => text.length > 0);
    /**
     * Encrypted reasoning this round has not finished and has no summary yet.
     *
     * While it streams there is nothing a card could show — the stream
     * indicator narrates it beside the cat, and the card arrives once the
     * round's own duration does. If the round *ends* this way, there is still
     * nothing to show: no summary, and no ciphertext either, since that only
     * arrives with the `done` frame. A summary that did stream stays in either
     * case, live or interrupted, just like plaintext reasoning does: it is
     * readable text the user already saw. What an interrupted card never
     * carries is a replayable trace, and the host's wire projection drops every
     * interrupted reasoning card when the next turn is built.
     */
    const withholdReasoning = reasoningForm === "encrypted"
      && reasoningOpened
      && !reasoningCompleted
      && !reasoningBody;
    if (!withholdReasoning && (reasoningOpened || reasoningTokens)) {
      // Derive one identity per reasoning item. Duration and token counts belong
      // only to the first row, and only the last row remains streaming.
      const rowCount = Math.max(reasoningItems.length, 1);
      for (let item = 0; item < rowCount; item += 1) {
        const lastRow = item === rowCount - 1;
        contexts.push({
          id: reasoningSegmentContextId(run.requestId, round, item),
          kind: "reasoning",
          content: reasoningItems[item] ?? "",
          form: run.reasoningFormsByRound?.[round]?.[item] ?? reasoningForm,
          round,
          modelTurnId,
          ...(streaming
            ? { streaming: reasoningOpened && !reasoningCompleted
              && (run.activeReasoningItemsByRound?.[round]?.[item] ?? lastRow) }
            : {}),
          ...(item === 0 && reasoningStartedAt ? { startedAt: reasoningStartedAt } : {}),
          ...(item === 0 && run.reasoningDurationByRound[round] != null
            ? { durationMs: run.reasoningDurationByRound[round] }
            : {}),
          // Responses reports zero rather than omitting tokens; omit zero to
          // avoid displaying a meaningless `· 0`.
          ...(item === 0 && reasoningTokens ? { tokens: reasoningTokens } : {}),
          createdAt: run.startedAt
        });
      }
    }
    if (text || tools.length > 0) {
      contexts.push({
        id: roundProseContextId(run.requestId, round, "assistant"),
        kind: "assistant",
        content: text ?? "",
        round,
        modelTurnId,
        ...(streaming ? { streaming: true } : {}),
        createdAt: run.startedAt
      });
    }
    contexts.push(...preToolHooks.map((hook) => hookStreamContext(run, hook, streaming)));
    contexts.push(...tools
      // Only the backend mints the receipt that authorizes a tool result into
      // the document, and it mints it when the call actually finishes. A call
      // the stream left unfinished has no attested result, so persisting a
      // synthesized one gets the whole document rejected — and because every
      // later save carries that same context forward, the conversation stops
      // persisting entirely. Settling an interrupted run therefore keeps the
      // calls that finished and drops the rest.
      //
      // The child record is dropped for the same reason: it is renderer-built,
      // and only a backend-minted subagent receipt authorizes one. The host
      // mints those from the finished child transcript, which an interrupted
      // run never delivers.
      .filter((tool) => streaming || tool.streamStatus === "completed")
      .map((tool) => ({
        id: tool.id,
        kind: "tool" as const,
        toolName: tool.toolName,
        round,
        modelTurnId,
        ...(tool.requestedInput && canonicalJson(tool.requestedInput) !== canonicalJson(tool.input)
          ? { requestedInput: tool.requestedInput }
          : {}),
        input: tool.input,
        result: tool.result,
        ...(streaming
          ? {
              streaming: true as const,
              streamStatus: tool.streamStatus,
              live: tool.live
            }
          : {}),
        createdAt: tool.createdAt
      })));
    contexts.push(...postToolHooks.map((hook) => hookStreamContext(run, hook, streaming)));
    contexts.push(...stopHooks.map((hook) => hookStreamContext(run, hook, streaming)));
    return contexts;
  });
}

/**
 * Caches the pure streaming projection by run snapshot identity so all
 * subscribers receive the same array for one flush.
 */
const streamingProjectionCache = new WeakMap<ModelRunState, ContextItem[]>();

export function streamingContextsFromModelRun(run: ModelRunState): ContextItem[] {
  const cached = streamingProjectionCache.get(run);
  if (cached) return cached;
  const projected = contextsFromModelRun(run, true);
  streamingProjectionCache.set(run, projected);
  return projected;
}

/**
 * Merges a conversation with its active streaming projection. Streaming contexts
 * win for duplicate ids because they contain the latest delta and streaming state.
 */
export function mergeContextsWithStreamingRun(
  contexts: ContextItem[],
  run: ModelRunState
): ContextItem[] {
  const streaming = streamingContextsFromModelRun(run);
  if (!streaming.length) return contexts;
  const streamingIds = new Set(streaming.map((context) => context.id));
  const persisted = contexts.filter((context) => !streamingIds.has(context.id));
  return [...persisted, ...streaming];
}

/**
 * Applies a run's settled projection to a conversation. Replaceable ids are
 * limited to this run's prefixes, preserving contexts from other runs and user edits.
 * Returns the original conversation when nothing materially changed.
 */
export function applyStreamedRunContexts(
  conversation: Conversation,
  streamed: ContextItem[],
  requestId: string
): Conversation {
  const replaceablePrefixes = runStreamContextPrefixes(requestId);
  const isReplaceable = (id: string) => replaceablePrefixes.some((prefix) => id.startsWith(prefix));
  const textOf = (context: ContextItem) => (context.kind === "tool" ? undefined : context.content);
  const interruptedOf = (context: ContextItem) => (
    context.kind === "assistant" || context.kind === "reasoning" ? context.interrupted : undefined
  );
  const streamedById = new Map(streamed.map((context) => [context.id, context] as const));
  const knownIds = new Set(conversation.contexts.map((context) => context.id));
  const deliveredQueueIds = new Set(streamed.flatMap((context) => (
    context.kind === "user" ? [context.id] : []
  )));
  let replaced = false;
  const contexts = conversation.contexts.map((context) => {
    if (!isReplaceable(context.id)) return context;
    const fresh = streamedById.get(context.id);
    if (!fresh) return context;
    if (textOf(fresh) === textOf(context) && interruptedOf(fresh) === interruptedOf(context)) {
      return context;
    }
    replaced = true;
    return fresh;
  });
  const generated = streamed.filter((context) => {
    if (knownIds.has(context.id)) return false;
    knownIds.add(context.id);
    return true;
  });
  const queuedMessages = conversation.queuedMessages.filter(
    (message) => !deliveredQueueIds.has(message.id)
  );
  if (!replaced && !generated.length && queuedMessages.length === conversation.queuedMessages.length) {
    return conversation;
  }
  return {
    ...conversation,
    contexts: [...contexts, ...generated],
    queuedMessages,
    updatedAt: new Date().toISOString()
  };
}

export function contextsFromInterruptedRun(run: ModelRunState): ContextItem[] {
  // Rounds whose every call carried a real result through to the end. Those
  // replay as an ordinary model turn. A round that lost a call to the
  // interrupt keeps the calls that finished — they executed and are attested —
  // but its visible text is only a fragment of the turn the model intended, so
  // the text is flagged and replay rebuilds the turn from the surviving calls.
  const settledRounds = new Set(
    Object.entries(run.streamedToolsByRound)
      .filter(([, tools]) => tools.length > 0 && tools.every((tool) => tool.streamStatus === "completed"))
      .map(([round]) => Number(round))
  );

  return contextsFromModelRun(run, false).map((context) => {
    if (
      (context.kind === "assistant" || context.kind === "reasoning")
      && (context.round === undefined || !settledRounds.has(context.round))
    ) {
      return { ...context, interrupted: true };
    }
    return context;
  });
}

/**
 * Keeps local partial text from a cancelled stream while replacing any matching
 * live tool card with the backend's receipt-bearing terminal context. Contexts
 * that only the backend learned during teardown are appended once.
 */
export function mergeFinalizedInterruptedRunContexts(
  run: ModelRunState,
  finalizedContexts: ContextItem[]
): ContextItem[] {
  // An encrypted reasoning card the host recovered from a round that never
  // finished, with no summary to show: no text, and no ciphertext either, since
  // that only arrives with the `done` frame. The local projection withholds
  // the same round for the same reason; the host never mints such a card, but
  // these contexts bypass the projection's gate, so the rule is applied here
  // too. A fragment that does carry a summary stays, as it does locally.
  const abandonedEncrypted = new Set(finalizedContexts.flatMap((context) => (
    context.kind === "reasoning"
      && context.interrupted
      && isEncryptedReasoning(context)
      && !context.content
      ? [context.id]
      : []
  )));
  const finalized = finalizedContexts.filter((context) => !abandonedEncrypted.has(context.id));
  const finalizedById = new Map(finalized.map((context) => [context.id, context]));
  const seen = new Set<string>();
  const merged = contextsFromInterruptedRun(run).flatMap((context) => {
    if (abandonedEncrypted.has(context.id)) return [];
    seen.add(context.id);
    const terminal = finalizedById.get(context.id);
    if (!terminal) return [context];
    if (context.kind === "assistant" && context.interrupted && terminal.kind === "assistant") {
      return [{ ...terminal, interrupted: true }];
    }
    if (context.kind === "reasoning" && context.interrupted && terminal.kind === "reasoning") {
      return [{ ...terminal, interrupted: true }];
    }
    return [terminal];
  });
  for (const context of finalized) {
    if (seen.has(context.id)) continue;
    seen.add(context.id);
    merged.push(context);
  }
  return merged;
}

export function mergeUniqueContexts(...groups: ContextItem[][]): ContextItem[] {
  const ids = new Set<string>();
  return groups.flatMap((group) => group.flatMap((context) => {
    if (ids.has(context.id)) return [];
    ids.add(context.id);
    return [context];
  }));
}

/**
 * Backfills model-request inputs available only in the stream into authoritative
 * tool cards. Match by `context.id` and attach `requestedInput` only when it
 * differs from the card's executed input; receipt-bound fields must not be
 * inferred for an unrelated card.
 */
export function backfillRequestedToolInput(contexts: ContextItem[], run: ModelRunState): ContextItem[] {
  const streamedById = new Map<string, StreamingToolState>();
  for (const tools of Object.values(run.streamedToolsByRound)) {
    for (const tool of tools) streamedById.set(tool.id, tool);
  }
  if (!streamedById.size) return contexts;

  return contexts.map((context) => {
    if (context.kind !== "tool") return context;
    if (context.requestedInput) return context;
    const candidate = streamedById.get(context.id);
    if (!candidate?.requestedInput) return context;
    // Compare the authoritative card's executed input. The requested input only
    // belongs on that card when it differs from its final arguments.
    if (canonicalJson(candidate.requestedInput) === canonicalJson(context.input)) return context;
    return { ...context, requestedInput: candidate.requestedInput };
  });
}
