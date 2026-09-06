import type {
  ContextItem,
  ModelUsage,
  PendingToolPrompt,
  SubagentLiveState,
  SubagentRunRecord,
  SubagentRunStatus,
  SubagentUpdate,
  TextContext,
  ToolContext
} from "../types";
import { normalizeModelUsage, sumModelUsage, sumUsageByRound } from "./conversationTurns";

export type SubagentViewStatus = "running" | SubagentRunStatus;

/**
 * One agent as shown in the workspace card, chips and the read-only drawer.
 * `agent_spawn` and every message/follow-up call touching the same agent merge
 * into a single view keyed by the agent name; legacy `subagent` calls keep one
 * view per call.
 */
export interface SubagentView {
  /** Agent name when known, else the (latest) provider call id. */
  id: string;
  /** Addressable name for messaging/task_wait; null on legacy views. */
  name: string | null;
  kind: "general" | "workflowStep";
  /**
   * True when this view is the workflow *run* — the script driver — rather than
   * one of the agents it starts. Both carry `kind: "workflowStep"`, so this is
   * what tells them apart.
   *
   * It is read off the `workflow` call that started the run, never off the
   * children: a run that has not spawned its first step yet, or one the host
   * rejected before it spawned any, is still a run. It has no transcript worth
   * opening either way — its `contexts` are the driver's own synthetic step
   * list — so no surface may present it as an agent.
   */
  workflowRun: boolean;
  label: string;
  task: string;
  status: SubagentViewStatus;
  summary: string;
  contexts: ContextItem[];
  updates: SubagentUpdate[];
  live: SubagentLiveState | null;
  /** Every spawn/send call id that touched this agent, for chip → view routing. */
  callIds: string[];
  /** Owning view id when this agent runs inside another agent's transcript;
   * null for agents the conversation itself started. */
  parentId: string | null;
  /** 0 for a conversation-level agent, +1 per nesting level. */
  depth: number;
  /** Direct child view ids, in the order they appear in the returned list. */
  childIds: string[];
  /**
   * Declared workflow phase of a `workflow_step` run. The host writes it into
   * the synthetic step context; every other run kind leaves it null.
   */
  phase: string | null;
  /** Plan-order index of {@link phase}, so unnamed phases still group in order. */
  phaseIndex: number | null;
  /**
   * Plan slot this step occupies, when the host stated one.
   *
   * Only a step whose body was moved out to the run directory carries it — the
   * coordinate the renderer fetches that body back with. Every other step is
   * identified by its label alone, so this is null far more often than not and
   * a consumer must have an answer for that case rather than assuming order.
   */
  stepIndex: number | null;
  /**
   * The configured role this run was bound to, and the exact model that role
   * runs on. A workflow step names its role through `agentType` — which model
   * answers for it is the user's configuration, never the plan's — and the host
   * persists the resolved binding with the record, so both are already here.
   *
   * Null for a run that named no role and therefore inherited its parent's
   * model: that is an absent binding, not a model this view may guess at.
   *
   * `modelId` is null on its own when the name came from a source that carries
   * no model — an `agent_spawn` whose record has not landed yet states the role
   * it asked for in its own input, and nothing more. The name is still the
   * honest answer to "what is this running as"; the model is simply not known
   * yet.
   */
  role: { name: string; modelId: string | null } | null;
  /**
   * The exact model this run answers on, when the host persisted one.
   *
   * A named agent carries it inside its role binding; a conversation fork
   * carries the exact model it was frozen against, and has no role at all. An
   * ordinary role-less child has neither: it inherits whatever model the
   * conversation is on, which is not a fact this record knows. Null therefore
   * means "inherited", and the surface that knows the conversation fills it in
   * rather than this projection guessing.
   */
  modelId: string | null;
  /**
   * The plan's `meta.name` on a workflow run — what the script calls itself.
   * Null on every other run kind, and on a run whose script had no name.
   */
  scriptName: string | null;
  /**
   * Tokens this agent alone spent, summed over every persisted record that
   * touched it. Empty for a run still streaming — the host bills the parent as
   * it goes and only writes the per-agent figure when it records the turn.
   */
  usage: ModelUsage;
  /**
   * Tool calls in this agent's own transcript, excluding its children's. The
   * task row reports it in the "Tools" column.
   *
   * `null` means unknowable, not zero: an externalized workflow step whose
   * shell predates the host writing the count has no transcript to count and no
   * figure to read, and "0 tools" would be a claim the data does not support.
   */
  toolCount: number | null;
  createdAt: string;
  completedAt: string | null;
}

export interface SubagentViewMessages {
  fallbackLabel: (index: number) => string;
  workflowStepLabel: string;
  missingTask: string;
  updateReturned: string;
  running: string;
  interrupted: string;
  failed: string;
  stopped: string;
  roundLimit: string;
  completed: string;
}

const defaultSubagentViewMessages: SubagentViewMessages = {
  fallbackLabel: (index) => `子代理 ${index}`,
  workflowStepLabel: "工作流步骤",
  missingTask: "未提供任务说明",
  updateReturned: "状态已返回给主智能体",
  running: "正在工作",
  interrupted: "子代理已中断",
  failed: "子代理已失败",
  stopped: "子代理已停止",
  roundLimit: "子代理已达轮次上限",
  completed: "子代理已完成"
};

const agentRunToolNames = new Set([
  "subagent",
  "agent_spawn",
  "agent_send",
  "send_message",
  "followup_task",
  // Workflow owns synthetic workflow_step contexts; both carry one nested,
  // read-only child transcript when their run has started.
  "workflow",
  "workflow_step"
]);
/**
 * Run tools whose child is conditional. A workflow call rejected before its
 * child exists has no transcript, so it must not open an empty read-only thread.
 */
const optionalChildRunToolNames = new Set([
  "workflow",
  "workflow_step"
]);

/** Specialized role a run tool implies when no persisted record states one. */
function impliedRunKind(toolName: string): SubagentView["kind"] | null {
  if (toolName === "workflow" || toolName === "workflow_step") return "workflowStep";
  return null;
}

function roleKindLabel(
  kind: SubagentView["kind"],
  messages: SubagentViewMessages
): string {
  return kind === "workflowStep" ? messages.workflowStepLabel : "";
}

/**
 * The role binding a run carries, reduced to what a task surface shows.
 *
 * The persisted record is authoritative and is consulted first: a fork inherits
 * its parent's model instead of naming a role, and the host already refuses to
 * persist both bindings at once, so an `agentDefinition` on the latest record
 * settles the question outright.
 *
 * Without one, the run's own calls are read. This is not a nicety — the record
 * is the *late* source in every direction that matters. It is minted when the
 * call finishes, and a workflow step's is minted only when the whole run
 * settles; a step whose body was externalized never gets one in the document at
 * all, and the drawer has to fetch it over IPC when someone opens that step. So
 * a surface that only read the record showed no role for an agent's entire
 * life, and for an externalized step until it was opened — which is precisely
 * the "you have to click into it before it says anything" complaint.
 *
 * Two keys answer, in this order, and each is gated to the one tool that owns
 * it — a run's calls carry whatever the model wrote, and a key read off the
 * wrong tool would be someone else's field:
 *
 * - `role`/`roleModelId` on a `workflow_step`. Host-written, from the binding
 *   it actually applied, and streamed with the step's identity on its first
 *   frame (`workflow.rs` `step_identity_input`).
 * - `agent_type` on an `agent_spawn`. Model-written, so it is the role the call
 *   *asked* for. The host resolves it and fails the spawn outright when it does
 *   not resolve, so a running child's requested name is its real one; the
 *   record still supersedes it above, which is what covers separator folding.
 *   No model travels with it, and none is invented.
 */
function agentRole(
  record: SubagentRunRecord | undefined,
  items: ToolContext[]
): SubagentView["role"] {
  const binding = record?.agentDefinition;
  if (binding) return { name: binding.name, modelId: binding.modelId };
  for (let index = items.length - 1; index >= 0; index -= 1) {
    const item = items[index];
    if (item.toolName === "workflow_step") {
      const bound = stringInput(item, "role");
      if (bound) return { name: bound, modelId: stringInput(item, "roleModelId") || null };
    }
    if (item.toolName === "agent_spawn") {
      const requested = stringInput(item, "agent_type");
      if (requested) return { name: requested, modelId: null };
    }
  }
  return null;
}

/**
 * The exact model a run answers on, read off whichever binding the host wrote.
 *
 * The two bindings are mutually exclusive by construction — a named agent has
 * `agentDefinition`, a conversation fork has `forkModelBinding` — so this reads
 * as "whichever one is here" rather than as a precedence rule. An ordinary
 * role-less child has neither and inherits the conversation's model, which is
 * absence rather than a model to guess at.
 *
 * The role's own model is the same fact arriving earlier, from the shell the
 * host writes rather than from the record it mints at the end. Falling through
 * to it keeps the model column populated for a step that is still streaming and
 * for one whose body was moved out to the run directory.
 */
function agentModelId(
  record: SubagentRunRecord | undefined,
  role: SubagentView["role"]
): string | null {
  return record?.agentDefinition?.modelId
    ?? record?.forkModelBinding?.modelId
    ?? role?.modelId
    ?? null;
}
const agentMessageToolNames = new Set(["agent_send", "send_message", "followup_task"]);
// Workflow steps are intentionally excluded: synthetic workflow runs have no
// durable public name and cannot be addressed by send_message/followup_task.
const addressableAgentRunToolNames = new Set([
  "agent_spawn",
  "agent_send",
  "send_message",
  "followup_task"
]);

/** Tool calls that carry a child run (chips, live stream, persisted record). */
export function isAgentRunTool(toolName: string): boolean {
  return agentRunToolNames.has(toolName);
}

/**
 * Agent calls whose public name is a durable host-issued identity.
 * Stream-only state must never synthesize a persisted record for these calls:
 * only the backend's signed final record may make them reloadable.
 */
export function isAddressableAgentRunTool(toolName: string): boolean {
  return addressableAgentRunToolNames.has(toolName);
}

/** Stable route used by timeline affordances across streaming and persisted contexts. */
export function agentTimelineRouteId(item: ToolContext): string {
  return item.subagent?.name?.trim()
    || stringInput(item, "target")
    || stringInput(item, "agent")
    || stringInput(item, "name")
    || item.id;
}

/**
 * The run status a timeline row must show for an agent call.
 *
 * `result.success` alone cannot say this: a completed spawn call may still own
 * a live background child, and a settled context may carry a terminal
 * lifecycle the child reported before `settleSubagentContexts` stripped its
 * streaming markers.
 */
export function agentTimelineRunStatus(item: ToolContext): SubagentViewStatus {
  if (item.streaming && item.live?.status) {
    if (item.live.status === "running") return "running";
    if (item.live.status === "idle") return "completed";
    // Every other live value is already a persisted-status name. Passing it
    // through keeps the row in agreement with the card and the task row:
    // folding them all into "interrupted" reported a stopped or round-limited
    // run as a failure it never was.
    return item.live.status;
  }
  if (item.streaming && item.streamStatus !== "completed") return "running";
  // spawn/send only acknowledge that the protocol call was accepted. During
  // the streamed handoff there is a brief gap before live/subagent arrives;
  // the background child is still running throughout that gap.
  if (
    item.streaming
    && item.result.success
    && (item.toolName === "agent_spawn"
      || item.toolName === "agent_send"
      || item.toolName === "followup_task")
    && !item.subagent
  ) return "running";
  if (item.subagent) return item.subagent.status;
  // A terminal lifecycle the child reported before `settleSubagentContexts`
  // stripped the streaming markers — the same rule `contextStatus` applies, so
  // the row and the card cannot disagree about a settled workflow step. A
  // settled context still claiming `running` never reported an ending, so it
  // keeps the interruption its result already records.
  if (item.live?.status && item.live.status !== "running") {
    return item.live.status === "idle" ? "completed" : item.live.status;
  }
  return item.result.success ? "completed" : "interrupted";
}

export const subagentAvatarTones = ["amber", "green", "mint", "blue", "violet", "rose"] as const;
export type SubagentAvatarTone = typeof subagentAvatarTones[number];

export function hashSubagentId(value: string): number {
  let hash = 2166136261;
  for (const character of value) {
    hash ^= character.codePointAt(0) ?? 0;
    hash = Math.imul(hash, 16777619);
  }
  return hash >>> 0;
}

export function subagentAvatarTone(id: string): SubagentAvatarTone {
  return subagentAvatarTones[hashSubagentId(id) % subagentAvatarTones.length];
}

function stringInput(item: ToolContext, key: string): string {
  const value = item.input[key];
  return typeof value === "string" ? value.trim() : "";
}

function numberInput(item: ToolContext, key: string): number | null {
  const value = item.input[key];
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

/**
 * Retrieval coordinates of a `workflow_step` context whose full transcript
 * lives in the run directory (`steps/<index>.json`) instead of a nested record.
 * The host writes them into the tool input only when the disk copy is
 * confirmed, so their presence is the marker that a body exists and can be
 * loaded on demand — and that the step must stay visible in the drawer even
 * though it carries no nested record.
 */
export interface ExternalStepBodyRef {
  runId: string;
  stepIndex: number;
}

export function externalStepBodyRef(item: ToolContext): ExternalStepBodyRef | null {
  if (item.toolName !== "workflow_step" || item.subagent) return null;
  const runId = stringInput(item, "runId");
  const stepIndex = numberInput(item, "stepIndex");
  if (!runId || stepIndex === null) return null;
  return { runId, stepIndex };
}

/**
 * The token usage the host left on an externalized step's shell.
 *
 * A step whose transcript moved to the run directory carries no nested record,
 * and the record is where every other agent's usage comes from. Usage is a
 * handful of integers, so the host keeps it in the shell alongside the terminal
 * status and the body fingerprint: without it the only way to learn a finished
 * step's cost was to open the step and let the drawer fetch its body.
 */
export function externalStepUsage(item: ToolContext): ModelUsage | null {
  if (item.toolName !== "workflow_step" || item.subagent) return null;
  const usage = item.input.usage;
  if (!usage || typeof usage !== "object" || Array.isArray(usage)) return null;
  const normalized = normalizeModelUsage(usage);
  return Object.keys(normalized).length ? normalized : null;
}

/**
 * The tool count for an agent whose transcript is not in the document.
 *
 * `undefined` means "not that kind of agent — count the transcript". A number
 * is what the host left on the shell. `null` is an externalized step whose
 * shell carries no count: the transcript is absent, so counting it would report
 * a confident zero for a step that may well have called a dozen tools. Once the
 * drawer grafts the body the shell stops advertising coordinates and the
 * ordinary transcript count takes over again.
 */
function externalStepToolCount(items: ToolContext[]): number | null | undefined {
  const shell = [...items].reverse().find((item) => externalStepBodyRef(item) !== null);
  return shell ? numberInput(shell, "toolUseCount") : undefined;
}

/** Terminal statuses the host writes into an externalized step's input. */
const externalStepStatuses = new Set<SubagentRunStatus>([
  "completed",
  "interrupted",
  "failed",
  "stopped",
  "roundLimit"
]);

/**
 * Grafts loaded step bodies back onto their externalized `workflow_step`
 * contexts, recursing through nested records — steps live inside their run's
 * record, not at the conversation's top level. `lookup` answers with the
 * loaded record for a coordinate, or `null`/`undefined` when nothing usable
 * was loaded (the context keeps its thin preview shell either way). Untouched
 * branches come back by reference, so memoized consumers only see new objects
 * where a body actually landed.
 */
export function graftExternalStepBodies(
  contexts: ContextItem[],
  lookup: (ref: ExternalStepBodyRef) => SubagentRunRecord | null | undefined
): ContextItem[] {
  let changed = false;
  const grafted = contexts.map((context) => {
    if (context.kind !== "tool") return context;
    const ref = externalStepBodyRef(context);
    if (ref) {
      const record = lookup(ref);
      if (!record) return context;
      changed = true;
      return { ...context, subagent: record };
    }
    if (!context.subagent) return context;
    const nested = graftExternalStepBodies(context.subagent.contexts, lookup);
    if (nested === context.subagent.contexts) return context;
    changed = true;
    return { ...context, subagent: { ...context.subagent, contexts: nested } };
  });
  return changed ? grafted : contexts;
}

/**
 * Finds the retrieval coordinates behind one drawer view's spawn calls. Feed
 * it the same (grafted) tree the views were derived from: a step whose body
 * was already grafted carries a nested record again, yields no coordinates
 * here, and therefore never triggers a second load.
 */
export function findExternalStepBodyRef(
  contexts: ContextItem[],
  callIds: readonly string[]
): ExternalStepBodyRef | null {
  for (const context of contexts) {
    if (context.kind !== "tool") continue;
    if (callIds.includes(context.id)) {
      const ref = externalStepBodyRef(context);
      if (ref) return ref;
    }
    if (context.subagent) {
      const nested = findExternalStepBodyRef(context.subagent.contexts, callIds);
      if (nested) return nested;
    }
  }
  return null;
}

function compactText(value: string, limit = 180): string {
  const compact = value.replace(/\s+/g, " ").trim();
  return compact.length > limit ? `${compact.slice(0, limit - 1).trimEnd()}…` : compact;
}

function validUpdates(updates: SubagentUpdate[] | undefined): SubagentUpdate[] {
  return (updates ?? []).flatMap((update) => {
    const content = update.content.trim();
    return content ? [{ ...update, content }] : [];
  });
}

function mergedUpdates(items: ToolContext[]): SubagentUpdate[] {
  const seen = new Set<string>();
  return items
    .flatMap((item) => [
      ...(item.subagent?.updates ?? []),
      ...(item.live?.updates ?? [])
    ])
    .map((update, order) => ({ update, order }))
    .flatMap(({ update, order }) => validUpdates([update]).map((valid) => ({ update: valid, order })))
    .filter(({ update }) => {
      const key = `${update.createdAt}\u0000${update.content}`;
      if (seen.has(key)) return false;
      seen.add(key);
      return true;
    })
    .sort((left, right) => timestamp(left.update.createdAt) - timestamp(right.update.createdAt)
      || left.order - right.order)
    .map(({ update }) => update);
}

function isUpdateTool(item: ToolContext): boolean {
  return item.toolName === "subagent_update" || item.toolName === "update";
}

function syntheticUpdate(
  id: string,
  update: SubagentUpdate,
  index: number,
  messages: SubagentViewMessages
): ToolContext {
  return {
    id: `${id}:update:${index}`,
    kind: "tool",
    toolName: "subagent_update",
    input: { message: update.content },
    result: {
      success: true,
      output: messages.updateReturned,
      executedAt: update.createdAt,
      durationMs: 0
    },
    createdAt: update.createdAt
  };
}

/** Adds live-only progress envelopes when the nested standard stream did not
 * already produce a structured update tool context. */
function withSupplementalContexts(
  id: string,
  contexts: ContextItem[],
  updates: SubagentUpdate[],
  messages: SubagentViewMessages
): ContextItem[] {
  const structuredCounts = new Map<string, number>();
  contexts.forEach((context) => {
    if (context.kind !== "tool" || !isUpdateTool(context)) return;
    const content = stringInput(context, "message");
    if (content) structuredCounts.set(content, (structuredCounts.get(content) ?? 0) + 1);
  });
  const supplemental = updates.flatMap<ContextItem>((update, index) => {
    const count = structuredCounts.get(update.content) ?? 0;
    if (count > 0) {
      structuredCounts.set(update.content, count - 1);
      return [];
    }
    return [syntheticUpdate(id, update, index, messages)];
  });
  if (!supplemental.length) return contexts;

  const task = contexts[0];
  const body = task ? contexts.slice(1) : contexts;
  const merged = chronologicalContexts([...body, ...supplemental]);
  return task ? [task, ...merged] : merged;
}

function lastAssistantText(contexts: ContextItem[]): string {
  for (let index = contexts.length - 1; index >= 0; index -= 1) {
    const context = contexts[index];
    if (context.kind === "assistant" && context.content.trim()) return context.content.trim();
  }
  return "";
}

/**
 * Input keys that carry a run-starting call's instruction, in priority order.
 *
 * Each run tool names this field for the model, and the names disagree:
 * `agent_spawn` declares `prompt` and the synthetic `workflow_step` context the
 * host mints carries `task`. The timeline stores whatever key the model actually
 * sent, so reading a single key leaves every other tool with an empty task and
 * the "no task description" fallback.
 */
const taskInputKeys = ["task", "prompt"] as const;

/** The task/message a run-starting call put in front of the child. */
export function initialMessage(item: ToolContext): string {
  if (agentMessageToolNames.has(item.toolName)) return stringInput(item, "message");
  for (const key of taskInputKeys) {
    const value = stringInput(item, key);
    if (value) return value;
  }
  return "";
}

function chronologicalContexts(contexts: ContextItem[]): ContextItem[] {
  return contexts
    .map((context, order) => ({ context, order }))
    .sort((left, right) => timestamp(left.context.createdAt) - timestamp(right.context.createdAt)
      || left.order - right.order)
    .map(({ context }) => context);
}

/**
 * Projects every parent tool call that touched one child into a single child
 * conversation. Persisted records are cumulative snapshots, while live events
 * can remain attached to an older spawn/message call when a message is queued
 * during an active child turn. Unioning all of them avoids treating a newer,
 * often empty message-call live state as a replacement transcript.
 */
function conversationTranscript(
  items: ToolContext[],
  id: string,
  task: string,
  updates: SubagentUpdate[],
  messages: SubagentViewMessages
): ContextItem[] {
  const contexts: ContextItem[] = [];
  const seenIds = new Set<string>();
  const persistedUsers: TextContext[] = [];
  const push = (context: ContextItem, persisted = false) => {
    if (seenIds.has(context.id)) return;
    seenIds.add(context.id);
    contexts.push(context);
    if (persisted && context.kind === "user") persistedUsers.push(context);
  };

  // Union all cumulative snapshots in parent-timeline order. Stable context
  // ids make later snapshots add only their newly completed turns.
  items.forEach((item) => item.subagent?.contexts.forEach((context) => push(context, true)));

  // How far into the record the calls have been matched. The child received its
  // turns in the order the record lists them and the calls were made in the
  // order the timeline lists them, so the pairing has to preserve that order —
  // a later call may not claim a turn an earlier one already passed over.
  let matchedThrough = 0;

  items.forEach((item) => {
    const message = initialMessage(item);
    if (message) {
      // A finalized cumulative snapshot already contains the spawn task and
      // each drained message. Consume the matching persisted user turn instead
      // of synthesizing it a second time.
      //
      // Content and position are all the two sides share: the record's context
      // ids are host-minted and the call carries none. An earlier version
      // matched on content and required the persisted turn to be no older than
      // the call — which reads as obviously true and is false. The host stamps
      // an agent card when the *call* finishes (`tool_context_for_turn`) and a
      // `workflow_step` card when the whole *run* settles
      // (`synthesize_step_context`), both strictly after the child's own first
      // user turn. So the match was rejected on every settled agent, and the
      // transcript grew a second copy of its task carrying the card's late
      // clock — which sorted it to the very end, after the child's final answer.
      //
      // Position replaces the clock rather than nothing: a `context:
      // "conversation"` fork copies the parent's own user turns into the head of
      // the child's record, and matching on content alone let an undrained
      // follow-up claim one of those and vanish from the transcript.
      const persistedIndex = persistedUsers.findIndex((candidate, index) => (
        index >= matchedThrough && candidate.content.trim() === message
      ));
      if (persistedIndex >= 0) {
        matchedThrough = persistedIndex + 1;
      } else if (item.toolName !== "send_message") {
        // send_message is queue-only. Until the child actually drains it into
        // a persisted user context it must not look like a completed child
        // turn in the transcript.
        push({
          id: `${id}:message:${item.id}`,
          kind: "user",
          content: message,
          createdAt: item.createdAt
        });
      }
    }

    item.live?.contexts.forEach((context) => push(context));

    // Compatibility for old blocking `subagent` records that only stored the
    // tool output and no structured child transcript.
    if (
      item.toolName === "subagent"
      && !item.subagent?.contexts.length
      && !item.live?.contexts.length
      && item.result.output.trim()
    ) {
      push({
        id: `${id}:answer:${item.id}`,
        kind: "assistant",
        content: item.result.output.trim(),
        createdAt: item.result.executedAt || item.createdAt
      });
    }
  });

  const ordered = chronologicalContexts(contexts);
  const taskIndex = task
    ? ordered.findIndex((context) => context.kind === "user" && context.content.trim() === task)
    : -1;
  const taskContext: ContextItem = taskIndex >= 0
    ? ordered[taskIndex]
    : {
        id: `${id}:task`,
        kind: "user",
        content: task || messages.missingTask,
        createdAt: items[0].createdAt
      };
  const taskFirst = [taskContext, ...ordered.filter((_, index) => index !== taskIndex)];
  return withSupplementalContexts(id, taskFirst, updates, messages);
}

/** Status contributed by one tool context of the agent. */
function contextStatus(item: ToolContext): SubagentViewStatus {
  // A streamed lifecycle transition wins while the parent turn is live: the
  // spawn/follow-up call itself completes instantly, its child keeps running.
  // Legacy `subagent` calls never stream one, so their own state decides.
  if (item.streaming && item.live?.status) {
    if (item.live.status === "running") return "running";
    if (item.live.status === "idle") return "completed";
    // Every other live value is already a persisted-status name; passing it
    // through keeps the live chip and the reloaded card in agreement.
    return item.live.status;
  }
  if (item.streaming && item.streamStatus !== "completed") return "running";
  // agent_spawn/followup_task complete when the host has accepted the protocol
  // call, not when the background child has finished.  There is a short (and
  // observable) hand-off window before the first status delta arrives; keep
  // the child running through it instead of flashing a false completed state.
  if (
    item.streaming
    && item.result.success
    && (item.toolName === "agent_spawn"
      || item.toolName === "agent_send"
      || item.toolName === "followup_task")
    && !item.subagent
  ) return "running";
  if (item.subagent) return item.subagent.status;
  // An externalized workflow step carries no nested record — its terminal
  // status travels in the tool input alongside the retrieval coordinates.
  // Consult it before the success-based fallback so an interrupted or stopped
  // step keeps its real state even though the body lives on disk.
  if (externalStepBodyRef(item)) {
    const status = stringInput(item, "status") as SubagentRunStatus;
    if (externalStepStatuses.has(status)) return status;
  }
  // A terminal lifecycle the child reported before `settleSubagentContexts`
  // stripped the streaming markers. `live` only ever reaches a non-streaming
  // context that way, and a terminal one is the last thing the host actually
  // said about this run — which beats a placeholder tool result the host never
  // wrote. A `workflow_step` reaches the fallback below with nothing else to go
  // on, because its record is only synthesized when the whole run returns.
  //
  // `running` is deliberately not consulted: a settled context that still
  // claims to be running is a child that never reported an ending, which is
  // exactly the interruption the result below already records.
  const settledLive = item.live?.status;
  if (settledLive && settledLive !== "running") {
    return settledLive === "idle" ? "completed" : settledLive;
  }
  // A persisted context without a record: the record lives on a later context
  // of the same agent, or the call itself failed.
  return item.result.success ? "completed" : "interrupted";
}

function completedAt(
  items: ToolContext[],
  status: SubagentViewStatus,
  contexts: ContextItem[],
  updates: SubagentUpdate[]
): string | null {
  if (status === "running") return null;
  return [
    ...contexts.map((context) => context.createdAt),
    ...updates.map((update) => update.createdAt),
    ...items.flatMap((item) => [item.result.executedAt, item.createdAt])
  ]
    .filter(Boolean)
    .sort((left, right) => timestamp(right) - timestamp(left))[0] ?? items[0].createdAt;
}

function summaryFor(
  status: SubagentViewStatus,
  task: string,
  contexts: ContextItem[],
  updates: SubagentUpdate[],
  messages: SubagentViewMessages
): string {
  const latestUpdate = updates.at(-1)?.content ?? "";
  const assistant = lastAssistantText(contexts);
  const terminalFallback = {
    interrupted: messages.interrupted,
    failed: messages.failed,
    stopped: messages.stopped,
    roundLimit: messages.roundLimit
  }[status as "interrupted" | "failed" | "stopped" | "roundLimit"];
  const candidate = status === "running"
    ? latestUpdate || assistant || task || messages.running
    : terminalFallback !== undefined
      // A run that did not finish cleanly leads with what it managed to report;
      // falling through to messages.completed would claim success.
      ? latestUpdate || assistant || terminalFallback
      : assistant || latestUpdate || task || messages.completed;
  return compactText(candidate);
}

function timestamp(value: string | null): number {
  if (!value) return 0;
  const parsed = Date.parse(value);
  return Number.isFinite(parsed) ? parsed : 0;
}

function combinedLiveState(items: ToolContext[]): SubagentLiveState | null {
  const liveItems = items.filter((item) => item.streaming && item.live).map((item) => item.live!);
  if (!liveItems.length) return null;
  const seenIds = new Set<string>();
  const contexts = chronologicalContexts(liveItems
    .flatMap((live) => live.contexts)
    .filter((context) => {
      if (seenIds.has(context.id)) return false;
      seenIds.add(context.id);
      return true;
    }));
  const updates = mergedUpdates(items.filter((item) => item.streaming && item.live));
  const status = [...liveItems].reverse().find((live) => live.status)?.status;
  return { contexts, updates, ...(status ? { status } : {}) };
}

/**
 * What the provider reported for an agent that has no record yet.
 *
 * Each context is one turn of this agent, and within a turn the map holds one
 * cumulative snapshot per round — so rounds sum inside a turn and turns sum
 * across the run, and neither addition double-counts a snapshot that was merely
 * revised. An agent with nothing streamed returns `{}`, which is how the tokens
 * column knows to keep saying "—" rather than "0".
 *
 * Deliberately not gated on `streaming`: `settleSubagentContexts` strips that
 * flag from a workflow step the moment its run reports a terminal status, while
 * the step's own record is only minted at the very end of the run. Requiring it
 * therefore blanked the tokens column at exactly the moment the step finished —
 * the moment its cost is most worth reading.
 */
function liveUsage(items: ToolContext[]): ModelUsage {
  return sumModelUsage(items.flatMap((item) => (
    item.live?.usageByRound ? [sumUsageByRound(item.live.usageByRound)] : []
  )));
}

/** The stable identity one tool context contributes: agent name or call id. */
export function agentKeyForContext(item: ToolContext): { key: string; name: string | null } {
  const recordName = item.subagent?.name?.trim() || "";
  if (recordName) return { key: `name:${recordName}`, name: recordName };
  if (agentMessageToolNames.has(item.toolName)) {
    const target = stringInput(item, "target") || stringInput(item, "agent");
    if (target) return { key: `name:${target}`, name: target };
  }
  if (item.toolName === "agent_spawn") {
    const requested = stringInput(item, "name");
    if (requested) return { key: `name:${requested}`, name: requested };
  }
  // A workflow run is named by the model too, and the host projects that name
  // into the public input. Reading it here is what gives a run its identity
  // from its first streamed frame instead of only once the driver's record
  // lands: without it the live view is keyed by a context id and the settled
  // one by the pool name, and the two are different rows.
  if (item.toolName === "workflow") {
    const requested = stringInput(item, "name");
    if (requested) return { key: `name:${requested}`, name: requested };
  }
  return { key: `context:${item.id}`, name: null };
}

interface AgentAccumulator {
  name: string | null;
  order: number;
  contexts: ToolContext[];
}

/**
 * Builds the card/drawer projection from parent timeline tool contexts.
 * Every spawn/message/follow-up call touching the same agent name merges into
 * one conversation. Cumulative records, continuation messages and every live
 * stream are unioned; the view id prefers the agent name so it stays stable
 * across continuations, with call ids kept for selection matching during
 * streaming hand-offs.
 *
 * The result is a pre-order flattening of the whole agent tree: every view is
 * immediately followed by its descendants, and `parentId`/`depth`/`childIds`
 * carry the relation. A search group therefore reads as its isolated executor
 * while every existing consumer keeps its flat array.
 */
export function deriveSubagentViews(
  contexts: ContextItem[],
  messages: SubagentViewMessages = defaultSubagentViewMessages
): SubagentView[] {
  return nestedSubagentViews(contexts, messages, null, 0, { value: 0 });
}

/**
 * One nesting level. Recursion is deliberately uncapped: a settled transcript
 * is a finite tree, so the only thing a depth limit could do is hide a real
 * child from the audit trail.
 */
function nestedSubagentViews(
  contexts: ContextItem[],
  messages: SubagentViewMessages,
  parentId: string | null,
  depth: number,
  fallback: { value: number }
): SubagentView[] {
  const byKey = new Map<string, AgentAccumulator>();
  contexts.forEach((context, order) => {
    if (context.kind !== "tool" || !isAgentRunTool(context.toolName)) return;
    if (
      optionalChildRunToolNames.has(context.toolName)
      && !context.subagent
      && !context.live?.status
      && !context.live?.contexts.length
      && !context.live?.updates.length
      && !externalStepBodyRef(context)
      // A host-published role-bearing step shell survives a later registration
      // failure even though no worker transcript was ever created.
      && !(context.toolName === "workflow_step" && stringInput(context, "role"))
    ) return;
    const { key, name } = agentKeyForContext(context);
    const existing = byKey.get(key);
    if (existing) {
      existing.contexts.push(context);
      existing.name = existing.name ?? name;
    } else {
      byKey.set(key, { name, order, contexts: [context] });
    }
  });

  const views = [...byKey.values()].map((accumulator) => {
    fallback.value += 1;
    const fallbackNumber = fallback.value;
    const items = accumulator.contexts;
    const first = items[0];
    const last = items[items.length - 1];
    const latestRecordContext = [...items].reverse().find((item) => Boolean(item.subagent)) ?? null;
    const explicitLiveStatusContext = [...items].reverse().find(
      (item) => item.streaming && item.live?.status
    ) ?? null;
    const live = combinedLiveState(items);
    const name = accumulator.name;
    const id = name ?? last.id;
    const kind = [...items].reverse()
      .map((item) => item.subagent?.kind)
      .find((value) => value)
      ?? items.map((item) => impliedRunKind(item.toolName)).find((value) => value)
      ?? "general";
    // The `workflow` call is the run; `workflow_step` calls are its agents.
    const workflowRun = items.some((item) => item.toolName === "workflow");
    const label = [...items].reverse()
      .map((item) => stringInput(item, "label"))
      .find((value) => value)
      || name
      // A workflow run has no `label`: its script body is stripped from the
      // public input down to a name and a fingerprint, so the script's meta
      // name is the only thing left that says what the run is.
      || [...items].reverse().map((item) => stringInput(item, "scriptName")).find((value) => value)
      || roleKindLabel(kind, messages)
      || messages.fallbackLabel(fallbackNumber);
    const task = items
      .map((item) => item.subagent?.task.trim() ?? "")
      .find((value) => value)
      || items.map((item) => (agentMessageToolNames.has(item.toolName) ? "" : initialMessage(item))).find((value) => value)
      || initialMessage(first);
    const hasStreamingContext = items.some((item) => item.streaming);
    // A run tool's own result reports whether that *call* was accepted, not how
    // the child run ended: a spawn whose child later hits a tool error still
    // returns success, and a call the host rejected never had a child at all.
    // The child's own streamed lifecycle status is therefore authoritative
    // whenever it exists — letting a failed result outrank it reported the
    // whole subagent as failed the moment any one of its tool calls failed.
    const status = hasStreamingContext
      ? contextStatus(explicitLiveStatusContext ?? last)
      : latestRecordContext?.subagent?.status ?? contextStatus(last);
    const updates = mergedUpdates(items);
    const transcript = conversationTranscript(items, id, task, updates, messages);
    const shellToolCount = externalStepToolCount(items);
    // Read off the same record the usage figure comes from: the binding is
    // rewritten whole on every persisted turn, so the latest record states the
    // role this run is bound to *now* rather than the one it started on. The
    // calls are the fallback for every moment before that record exists — which
    // is a run's whole life, and an externalized step's whole life in the
    // document.
    const role = agentRole(latestRecordContext?.subagent, items);
    // The settled transcript already unions the persisted record with every
    // live stream, so one recursive pass over it finds this agent's children.
    const descendants = nestedSubagentViews(transcript, messages, id, depth + 1, fallback);
    return {
      view: {
        id,
        name,
        kind,
        workflowRun,
        label,
        task,
        status,
        summary: summaryFor(status, task, transcript, updates, messages),
        contexts: transcript,
        updates,
        live,
        callIds: items.map((item) => item.id),
        parentId,
        depth,
        childIds: descendants
          .filter((descendant) => descendant.parentId === id)
          .map((descendant) => descendant.id),
        phase: items.map((item) => stringInput(item, "phase")).find((value) => value) || null,
        phaseIndex: items.map((item) => numberInput(item, "phaseIndex")).find((value) => value !== null) ?? null,
        stepIndex: items.map((item) => numberInput(item, "stepIndex")).find((value) => value !== null) ?? null,
        role,
        modelId: agentModelId(latestRecordContext?.subagent, role),
        scriptName: [...items].reverse()
          .map((item) => stringInput(item, "scriptName"))
          .find((value) => value) || null,
        // The latest record already covers every turn this agent ran: the host
        // restores the running total onto a rehydrated agent before it
        // continues, so each successive record supersedes the one before it
        // rather than adding to it. Summing them would double-count a
        // followup_task.
        //
        // Failing that, two weaker sources fill the gap the record leaves. An
        // externalized workflow step has no record at all, only the shell the
        // host left behind; and an agent that is still running has no record
        // yet, only the snapshots its provider has streamed so far. Without
        // these the tokens column read "—" for a running agent's whole life,
        // and for a finished workflow step until someone opened it.
        usage: latestRecordContext?.subagent?.usage
          ?? [...items].reverse().map(externalStepUsage).find((value) => value !== null)
          ?? liveUsage(items),
        // Only this agent's own calls. A child's calls live inside the nested
        // record hanging off one of these tool contexts, not beside them.
        //
        // An externalized step is the one case the transcript cannot answer:
        // its body is on disk, so counting the shell's contexts reports zero
        // for a step that may have called a dozen tools. The host leaves the
        // real count on the shell for exactly that reason.
        toolCount: shellToolCount !== undefined
          ? shellToolCount
          : transcript.filter((context) => context.kind === "tool").length,
        createdAt: first.createdAt,
        completedAt: completedAt(items, status, transcript, updates)
      } satisfies SubagentView,
      descendants,
      order: accumulator.order
    };
  });

  return views
    .sort((left, right) => {
      const leftRunning = left.view.status === "running";
      const rightRunning = right.view.status === "running";
      if (leftRunning !== rightRunning) return leftRunning ? -1 : 1;
      const dateDifference = timestamp(right.view.completedAt ?? right.view.createdAt)
        - timestamp(left.view.completedAt ?? left.view.createdAt);
      return dateDifference || right.order - left.order;
    })
    .flatMap(({ view, descendants }) => [view, ...descendants]);
}

/** Finds the view a chip or stale selection refers to, by id or any call id. */
export function findSubagentView(views: SubagentView[], id: string | null): SubagentView | null {
  if (!id) return null;
  return views.find((view) => view.id === id)
    ?? views.find((view) => view.callIds.includes(id))
    ?? null;
}

/**
 * The agent a surface may open a read-only transcript for.
 *
 * A workflow run resolves to nothing. It is a script: the transcript behind it
 * is the driver's own synthetic step list, which is not a conversation anybody
 * held, and the run already has a surface that says what it actually is — the
 * panel with its plan, phases and per-agent metrics. Every route into the
 * read-only panel goes through here, so a stale selection cannot bring that
 * page back.
 */
export function findOpenableSubagentView(
  views: SubagentView[],
  id: string | null
): SubagentView | null {
  const view = findSubagentView(views, id);
  return view && !view.workflowRun ? view : null;
}

/**
 * The subagent page a pending approval card should open, or null when the card
 * belongs to the main session (no source) or its requester has no openable
 * page (yet).
 *
 * Two keys, tried in order, because the requester's identity is spelled
 * differently across a child's life. `sourceAgent` is the addressable name —
 * a general subagent's view id, and a settled workflow step's record name. A
 * *streaming* workflow step has no record and its view is keyed by the
 * parent-timeline context id, which is exactly what `sourceCallId` carries
 * (both worker loops mint it from `shared.latest_call_id()`), and
 * `findSubagentView` matches it through `callIds`.
 */
export function approvalPromptSubagentView(
  views: SubagentView[],
  prompt: Pick<PendingToolPrompt, "sourceAgent" | "sourceCallId">
): SubagentView | null {
  return findOpenableSubagentView(views, prompt.sourceAgent ?? null)
    ?? findOpenableSubagentView(views, prompt.sourceCallId ?? null);
}

