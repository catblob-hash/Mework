import { isHostAuthoredUserContext } from "./orchestration";
import type { ContextItem, ModelUsage } from "../types";

export const CONVERSATION_TURNS_STORAGE_KEY = "mework.conversation-turns.v1";

export type ConversationTurnStatus = "running" | "awaiting_user" | "completed" | "interrupted";

/**
 * Why a turn stopped producing. Carried on the turn so the failure is readable
 * where it happened instead of only in a composer banner that a reload erases —
 * a bare "stopped after 12s" header with no body is the shape of a bug report.
 * It is presentation metadata like the rest of the turn: never a model message,
 * never written into Conversation.contexts.
 */
export interface ConversationTurnError {
  message: string;
  providerName: string;
  modelName: string;
  at: string;
}

/**
 * Frontend-only presentation metadata. It is intentionally stored outside
 * Conversation.contexts and never participates in a model request.
 */
export interface ConversationTurn {
  id: string;
  requestId: string;
  anchorContextId: string;
  modelId: string;
  startedAt: string;
  endedAt?: string;
  durationMs?: number;
  status: ConversationTurnStatus;
  contextIds: string[];
  usage: ModelUsage;
  /** Usage completed by earlier backend requests in the same logical UI turn. */
  usageOffset: ModelUsage;
  /** Cumulative backend-run usage when this UI turn began. */
  usageBaseline: ModelUsage;
  usageRevisionAtStart: number;
  /** Backend requests joined by resumable UI-only pauses such as ask_user. */
  segmentCount: number;
  expanded: boolean;
  userToggled: boolean;
  /** Set when the run behind this turn failed; cleared when the next run starts. */
  error?: ConversationTurnError;
}

export type ConversationTurns = Record<string, ConversationTurn[]>;

function finiteTokenCount(value: unknown): number | undefined {
  return typeof value === "number" && Number.isFinite(value) && value >= 0
    ? Math.floor(value)
    : undefined;
}

export function normalizeModelUsage(value: unknown): ModelUsage {
  if (!value || typeof value !== "object") return {};
  const usage = value as Record<string, unknown>;
  return {
    ...(finiteTokenCount(usage.inputTokens) !== undefined
      ? { inputTokens: finiteTokenCount(usage.inputTokens) }
      : {}),
    ...(finiteTokenCount(usage.cachedInputTokens) !== undefined
      ? { cachedInputTokens: finiteTokenCount(usage.cachedInputTokens) }
      : {}),
    ...(finiteTokenCount(usage.outputTokens) !== undefined
      ? { outputTokens: finiteTokenCount(usage.outputTokens) }
      : {}),
    ...(finiteTokenCount(usage.totalTokens) !== undefined
      ? { totalTokens: finiteTokenCount(usage.totalTokens) }
      : {})
  };
}

function normalizeTurnError(value: unknown): ConversationTurnError | undefined {
  if (!value || typeof value !== "object") return undefined;
  const error = value as Partial<ConversationTurnError>;
  // A blank message would render an error card that explains nothing, which is
  // the empty-header failure this field exists to remove. Drop it instead.
  if (typeof error.message !== "string" || !error.message.trim()) return undefined;
  return {
    message: error.message,
    providerName: typeof error.providerName === "string" ? error.providerName : "",
    modelName: typeof error.modelName === "string" ? error.modelName : "",
    at: typeof error.at === "string" ? error.at : ""
  };
}

/** The status as written to disk, before load converts a stale `running` one. */
function storedTurnStatus(value: Partial<ConversationTurn>): ConversationTurnStatus {
  return value.status === "awaiting_user"
    || value.status === "completed"
    || value.status === "interrupted"
    ? value.status
    : "running";
}

function normalizeTurn(value: unknown, now: number): ConversationTurn | null {
  if (!value || typeof value !== "object") return null;
  const turn = value as Partial<ConversationTurn>;
  if (
    typeof turn.id !== "string"
    || typeof turn.requestId !== "string"
    || typeof turn.anchorContextId !== "string"
    || typeof turn.modelId !== "string"
    || typeof turn.startedAt !== "string"
    || Number.isNaN(new Date(turn.startedAt).getTime())
  ) return null;
  const storedStatus = storedTurnStatus(turn);
  const status: ConversationTurnStatus = storedStatus === "running" ? "interrupted" : storedStatus;
  const endedAt = status === "interrupted" && storedStatus === "running"
    ? new Date(now).toISOString()
    : typeof turn.endedAt === "string" ? turn.endedAt : undefined;
  const storedDuration = finiteTokenCount(turn.durationMs);
  const activeElapsed = Math.max(0, now - new Date(turn.startedAt).getTime());
  const error = normalizeTurnError(turn.error);
  const durationMs = storedStatus === "running"
    ? (storedDuration ?? 0) + activeElapsed
    : storedDuration
      ?? (endedAt
        ? Math.max(0, new Date(endedAt).getTime() - new Date(turn.startedAt).getTime())
        : 0);
  return {
    id: turn.id,
    requestId: turn.requestId,
    anchorContextId: turn.anchorContextId,
    modelId: turn.modelId,
    startedAt: turn.startedAt,
    ...(endedAt ? { endedAt } : {}),
    durationMs,
    status,
    contextIds: Array.isArray(turn.contextIds)
      ? turn.contextIds.filter((id): id is string => typeof id === "string")
      : [],
    usage: normalizeModelUsage(turn.usage),
    usageOffset: normalizeModelUsage(turn.usageOffset),
    usageBaseline: normalizeModelUsage(turn.usageBaseline),
    usageRevisionAtStart: finiteTokenCount(turn.usageRevisionAtStart) ?? 0,
    segmentCount: Math.max(1, finiteTokenCount(turn.segmentCount) ?? 1),
    expanded: typeof turn.expanded === "boolean" ? turn.expanded : status !== "completed",
    userToggled: turn.userToggled === true,
    ...(error ? { error } : {})
  };
}

/**
 * A round that produced nothing and explains nothing is not a record.
 *
 * A stop, a dropped connection or a steering message that lands before the
 * first message leaves a turn owning no context at all. Storing it would make
 * the next run inherit a round the user never got: a bare Send continues the
 * newest unfinished round, so the header would come back carrying the dead
 * round's elapsed time and token counters over output it never produced.
 *
 * A live turn is exempt — `running` still has its stream ahead of it, and
 * `awaiting_user` is a question whose answer has to come back to it. So is one
 * carrying a failure notice, which is the one thing an empty round still has
 * to say.
 */
export function turnLeavesNoRecord(turn: ConversationTurn): boolean {
  return turn.status !== "running"
    && turn.status !== "awaiting_user"
    && !turn.contextIds.length
    && !turn.error;
}

/** Reference-preserving so a list with nothing to drop never churns state. */
export function dropEmptyTurns(turns: ConversationTurn[]): ConversationTurn[] {
  const kept = turns.filter((turn) => !turnLeavesNoRecord(turn));
  return kept.length === turns.length ? turns : kept;
}

export function dropEmptyConversationTurns(turns: ConversationTurns): ConversationTurns {
  let changed = false;
  const next = Object.fromEntries(Object.entries(turns).map(([conversationId, entries]) => {
    const kept = dropEmptyTurns(entries);
    if (kept !== entries) changed = true;
    return [conversationId, kept];
  }));
  return changed ? next : turns;
}

export function loadConversationTurns(now = Date.now()): ConversationTurns {
  if (typeof window === "undefined") return {};
  try {
    const raw = window.localStorage.getItem(CONVERSATION_TURNS_STORAGE_KEY);
    if (!raw) return {};
    const parsed = JSON.parse(raw) as unknown;
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return {};
    return Object.fromEntries(Object.entries(parsed as Record<string, unknown>).flatMap(([conversationId, value]) => {
      if (!Array.isArray(value)) return [];
      const turns = value
        .flatMap((entry) => {
          const turn = normalizeTurn(entry, now);
          if (!turn) return [];
          // A turn stored as `running` belongs to a run the host may still be
          // advancing. Load marks it interrupted, but adoption resumes it, so
          // it survives the emptiness rule until that resolves; anything else
          // that reaches disk empty is residue this load is here to sweep.
          const adoptable = storedTurnStatus(entry as Partial<ConversationTurn>) === "running";
          return adoptable || !turnLeavesNoRecord(turn) ? [turn] : [];
        });
      return turns.length ? [[conversationId, turns]] : [];
    }));
  } catch {
    return {};
  }
}

export function saveConversationTurns(turns: ConversationTurns): void {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.setItem(CONVERSATION_TURNS_STORAGE_KEY, JSON.stringify(turns));
  } catch {
    // Presentation metadata must never make the conversation itself unsavable.
  }
}

// `reasoningTokens` participates in field-wise arithmetic but is a subset of `outputTokens`, not an independent billed amount.
// Omitting it would make `modelUsageEqual` miss reasoning-only changes.
const usageFields = ["inputTokens", "cachedInputTokens", "outputTokens", "totalTokens", "reasoningTokens"] as const;

export function sumModelUsage(usages: ModelUsage[]): ModelUsage {
  const result: ModelUsage = {};
  for (const field of usageFields) {
    const values = usages.map((usage) => usage[field]).filter((value): value is number => value !== undefined);
    if (values.length) result[field] = values.reduce((total, value) => total + value, 0);
  }
  return result;
}

export function subtractModelUsage(total: ModelUsage, baseline: ModelUsage): ModelUsage {
  const result: ModelUsage = {};
  for (const field of usageFields) {
    const value = total[field];
    if (value !== undefined) result[field] = Math.max(0, value - (baseline[field] ?? 0));
  }
  return result;
}

/** Field-by-field equality, so an absent counter never reads as a changed one. */
export function modelUsageEqual(left: ModelUsage, right: ModelUsage): boolean {
  return usageFields.every((field) => left[field] === right[field]);
}

export function sumUsageByRound(usageByRound: Record<number, ModelUsage>): ModelUsage {
  return sumModelUsage(Object.values(usageByRound));
}

/**
 * Anchor value for a turn whose output begins at the very start of the
 * timeline.
 *
 * A turn is anchored *after* a context, so once every context that preceded it
 * has been deleted there is no id left to name. Dropping the turn instead would
 * erase a round the user really ran, along with the duration and token counts
 * that are the only record of what it cost.
 */
export const TIMELINE_START_ANCHOR = "";

/** Index of the context a turn begins after, or `undefined` once its anchor was deleted. */
export function turnAnchorIndex(
  anchorContextId: string,
  indexes: ReadonlyMap<string, number>
): number | undefined {
  return anchorContextId === TIMELINE_START_ANCHOR ? -1 : indexes.get(anchorContextId);
}

/**
 * Recomputes only the live request's presentation spans. Historical records
 * retain explicit ids so later manual timeline edits cannot expand a turn.
 */
export function materializeRunTurnContexts(
  turns: ConversationTurn[],
  contexts: ContextItem[],
  requestId: string
): ConversationTurn[] {
  const requestTurns = turns
    .filter((turn) => turn.requestId === requestId)
    .sort((left, right) => new Date(left.startedAt).getTime() - new Date(right.startedAt).getTime());
  if (!requestTurns.length) return turns;
  const indexes = new Map(contexts.map((context, index) => [context.id, index]));
  const replacements = new Map<string, ConversationTurn>();
  requestTurns.forEach((turn, index) => {
    const anchorIndex = turnAnchorIndex(turn.anchorContextId, indexes);
    if (anchorIndex === undefined) return;
    const nextAnchorIndex = requestTurns
      .slice(index + 1)
      .map((candidate) => turnAnchorIndex(candidate.anchorContextId, indexes))
      .find((candidate): candidate is number => candidate !== undefined && candidate > anchorIndex);
    const end = nextAnchorIndex ?? contexts.length;
    replacements.set(turn.id, {
      ...turn,
      contextIds: contexts.slice(anchorIndex + 1, end).map((context) => context.id)
    });
  });
  return turns.map((turn) => replacements.get(turn.id) ?? turn);
}

/**
 * Re-place an anchor whose context the user deleted.
 *
 * The anchor only records where a turn's output begins, so the honest
 * replacement is the context immediately before the first message the turn
 * still owns — or the timeline tail when the deletion took all of them. Without
 * this a turn can never claim another context after losing its anchor, so the
 * run that continues it streams into a headerless timeline.
 */
export function repairTurnAnchor(turn: ConversationTurn, contexts: ContextItem[]): string {
  const indexes = new Map(contexts.map((context, index) => [context.id, index]));
  if (turnAnchorIndex(turn.anchorContextId, indexes) !== undefined) return turn.anchorContextId;
  const owned = turn.contextIds
    .map((id) => indexes.get(id))
    .filter((index): index is number => index !== undefined);
  const boundary = owned.length ? Math.min(...owned) : contexts.length;
  return boundary > 0 ? contexts[boundary - 1].id : TIMELINE_START_ANCHOR;
}

/**
 * A round ends on a new user message or on a normal final reply. Anything else
 * — a stop, a dropped connection, a pause for `ask_user` — leaves it unfinished,
 * and a later run that carries no user message continues it rather than opening
 * a round of its own.
 */
export function isUnfinishedTurn(turn: ConversationTurn): boolean {
  return turn.status === "interrupted" || turn.status === "awaiting_user";
}

/**
 * Index of the last timeline position this turn accounts for: the newest
 * message it still owns, or its anchor when the deletion took all of them.
 */
function turnBoundaryIndex(
  turn: ConversationTurn,
  contexts: ContextItem[]
): number | undefined {
  const indexes = new Map(contexts.map((context, index) => [context.id, index]));
  const owned = turn.contextIds
    .map((id) => indexes.get(id))
    .filter((index): index is number => index !== undefined);
  return owned.length ? Math.max(...owned) : turnAnchorIndex(turn.anchorContextId, indexes);
}

/**
 * The turn a run carrying no new user message continues.
 *
 * `"awaiting"` is the `ask_user` answer path: it resumes the paused turn
 * wherever it sits, because the answer belongs to the question that paused it
 * and not to whatever ran afterwards.
 *
 * `"continue"` is the bare Send and the task wake. It only ever continues the
 * newest round, and only while that round is genuinely unfinished — a user
 * message sitting after everything the turn owns already closed it, so the run
 * belongs to the round that message started instead. A round that produced
 * nothing is nothing to continue: it leaves no record (`turnLeavesNoRecord`),
 * and one that outlived a reload as an unadopted host run must not merge
 * either, or the new round would open carrying its clock.
 */
export function findResumableTurn(
  turns: ConversationTurn[],
  contexts: ContextItem[],
  mode: "continue" | "awaiting"
): ConversationTurn | undefined {
  if (mode === "awaiting") {
    return [...turns].reverse().find((turn) => turn.status === "awaiting_user");
  }
  const last = turns[turns.length - 1];
  if (!last || !isUnfinishedTurn(last) || !last.contextIds.length) return undefined;
  const boundary = turnBoundaryIndex(last, contexts);
  const closed = contexts.some((context, index) => (
    index > (boundary ?? contexts.length - 1)
    && context.kind === "user"
    && !isHostAuthoredUserContext(context)
  ));
  return closed ? undefined : last;
}

/**
 * Hand an unfinished turn to a new backend request.
 *
 * Its span against the old request is frozen first, then it carries its own
 * totals forward: elapsed time keeps accumulating from `durationMs`, and
 * `usageOffset` makes the new request's input, cached-input and output counts
 * add to what the round already spent instead of replacing it.
 */
export function resumeConversationTurn(
  turns: ConversationTurn[],
  contexts: ContextItem[],
  target: ConversationTurn,
  next: { requestId: string; modelId: string; startedAt: string }
): ConversationTurn[] {
  const materialized = materializeRunTurnContexts(turns, contexts, target.requestId);
  return materialized.map((turn) => {
    if (turn.id !== target.id) return turn;
    // The round is live again, so its terminal stamp and the notice explaining
    // why it stopped are both stale; settlement records new ones.
    const { endedAt: _endedAt, error: _error, ...rest } = turn;
    return {
      ...rest,
      requestId: next.requestId,
      modelId: next.modelId,
      startedAt: next.startedAt,
      anchorContextId: repairTurnAnchor(turn, contexts),
      status: "running" as const,
      usageOffset: turn.usage,
      usageBaseline: {},
      usageRevisionAtStart: 0,
      segmentCount: turn.segmentCount + 1,
      expanded: turn.userToggled ? turn.expanded : true
    };
  });
}

/**
 * Record why a run failed on the turn it failed in. The last turn of the
 * request owns it: an `ask_user` pause splits one request across several turns,
 * and only the segment that was still running when the run broke should carry
 * the notice.
 */
export function annotateTurnFailure(
  turns: ConversationTurn[],
  requestId: string,
  error: ConversationTurnError
): ConversationTurn[] {
  const target = [...turns].reverse().find((turn) => turn.requestId === requestId);
  if (!target) return turns;
  return turns.map((turn) => (
    turn.id === target.id
      ? { ...turn, error, expanded: turn.userToggled ? turn.expanded : true }
      : turn
  ));
}

/**
 * Drop failure notices and the header-only turns that existed solely to carry
 * them. Sending again is the user retracting the last failure, so a run that
 * produced nothing at all leaves no trace; a run that produced output keeps its
 * output and loses only the notice.
 */
export function clearTurnFailures(turns: ConversationTurn[]): ConversationTurn[] {
  let changed = false;
  const cleared = turns.map((turn) => {
    if (!turn.error) return turn;
    changed = true;
    const { error: _error, ...rest } = turn;
    return rest;
  });
  // Stripping the notice can be what empties a turn out, and an emptied round
  // is not a record — the same rule that sweeps a stop with nothing to show.
  return changed ? dropEmptyTurns(cleared) : turns;
}

export function formatTurnDuration(durationMs: number): string {
  const totalSeconds = Math.max(0, Math.floor(durationMs / 1000));
  const days = Math.floor(totalSeconds / 86_400);
  const hours = Math.floor((totalSeconds % 86_400) / 3_600);
  const minutes = Math.floor((totalSeconds % 3_600) / 60);
  const seconds = totalSeconds % 60;
  if (days) return `${days}d${hours}h${minutes}m${seconds}s`;
  if (hours) return `${hours}h${minutes}m${seconds}s`;
  if (minutes) return `${minutes}m${seconds}s`;
  return `${seconds}s`;
}
