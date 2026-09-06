import type { ContextItem } from "../types";
import type { SubagentView } from "./subagents";
import type { TaskItem, TaskItemState } from "./taskContainer";
import { taskStateForStatus } from "./taskContainer";
import type { WorkflowProgressView } from "./workflowProgress";

/**
 * The one projection both workflow surfaces draw from: the compact card in the
 * message stream and the detail panel in the task container.
 *
 * # Why two sources are merged
 *
 * Neither source alone can describe a run.
 *
 * The **agent roster** is the only place a step's role, model, token total and
 * wall time exist, and the only source that survives the model run: a settled
 * workflow still has its steps because the host persisted each one's record.
 * What it cannot show is a slot that never got an agent — a step replayed from
 * the resume journal registers nothing (`workflow.rs` skips `pool.register` on
 * a cache hit), and neither does one still waiting on a concurrency slot.
 *
 * The **live progress ledger** has a row for every slot including those, plus
 * the run's narration lines, but it is fed by stream events that die with the
 * model run and it carries no per-step model or usage at all.
 *
 * So: the roster decides the table, the ledger fills in the slots the roster
 * cannot have, and a settled run simply has no ledger to merge.
 *
 * # Matching a step to its ledger row
 *
 * There is no plan index on a step for most of its life. The synthetic call id
 * carries one (`<parent call>-ws<N>`), but what survives into the roster is the
 * *context* id, which the host derives by hashing — the suffix is not
 * recoverable from it. `stepIndex` reaches the renderer only once a step's body
 * has been moved out to the run directory.
 *
 * So the match is the stated `stepIndex` when there is one, and otherwise the
 * label, which both sides render from the same `step_display_label`. Labels are
 * not unique by construction — an unlabelled step falls back to its phase name,
 * so two of them in one phase collide — which is why claiming is greedy: the
 * first unclaimed row with that label wins, and the second step takes the next.
 *
 * A step that matches nothing keeps a null plan index rather than being assigned
 * one by position. Position is the tempting fallback and the wrong one: it would
 * quietly hand a step the slot number of an unrelated row, and that number is
 * what Skip sends to the host.
 */

/** A workflow task item, which is the shape this module projects. */
export type WorkflowTaskItem = Extract<TaskItem, { kind: "workflow" }>;

/**
 * What a step reports to the surfaces. `pending` is a fourth state the task
 * tree has no need for: a slot with no agent has not failed and has not
 * finished, and colouring it as either would misreport the run.
 */
export type WorkflowRunStepState = TaskItemState | "pending";

export interface WorkflowRunStep {
  /** Stable render identity. Never sent anywhere; only React keys on it. */
  key: string;
  /**
   * Plan slot, which is what the host's Skip command addresses.
   *
   * Null when the step could not be matched to a ledger row — a settled run has
   * no ledger at all, and a live one can have more agents than rows for a beat.
   * A null here must disable Skip rather than fall back to a guess: the host
   * would happily skip whichever slot the guess named.
   */
  planIndex: number | null;
  label: string;
  state: WorkflowRunStepState;
  /** The spawned agent, so a row can open its transcript. Null for an empty slot. */
  agentId: string | null;
  /** Configured role name, or null when the step named none. */
  role: string | null;
  /**
   * Exact model the step answers on.
   *
   * Usually the role's, but not only: a step that named no role still runs on
   * the conversation's model, and a role whose binding has not been persisted
   * yet states its model in the shell the host streams. Null means nobody has
   * said, which is why the tile falls back to it rather than leading with it.
   */
  modelId: string | null;
  tokens: number | null;
  elapsedMs: number | null;
  /** Replayed from the resume journal: a real result, but no agent ran for it. */
  cached: boolean;
  /** Waiting on a concurrency slot; nothing has started. */
  blocked: boolean;
  /** Skipped by the user. The ledger records this as an error with a sentinel. */
  skipped: boolean;
}

export interface WorkflowRunPhase {
  /** Render key. Declared index when there is one, so two same-named phases stay apart. */
  key: string;
  /** Declared phase title, or null for the group of steps the plan left unphased. */
  heading: string | null;
  steps: WorkflowRunStep[];
  /** Steps that reached a terminal, successful state — the "1/4" in the header. */
  done: number;
  total: number;
}

export interface WorkflowRunView {
  /** The workflow driver's view id, which addresses the run in the task panel. */
  id: string;
  /** The name the model gave this run, which is also how it is addressed. */
  name: string;
  /**
   * The plan's `meta.name` — what the script calls itself. Null on a run whose
   * script named nothing, and on one whose public input has not arrived yet.
   */
  scriptName: string | null;
  /** The plan's `meta.description`; empty when the run has none to show. */
  description: string;
  state: TaskItemState;
  stepCount: number;
  elapsedMs: number | null;
  tokens: number | null;
  phases: WorkflowRunPhase[];
  /** Plan order, flattened — what the compact card's status strip draws. */
  steps: WorkflowRunStep[];
  /** Narration lines, live only. A settled run has none to show. */
  logs: string[];
  running: boolean;
}

export interface WorkflowHistoryStep {
  index: number;
  label: string;
  state: "completed" | "failed" | "cached" | "unrecorded";
  bodyAvailable: boolean;
  error: string | null;
}

export interface WorkflowHistoryRun {
  runId: string;
  scriptName: string;
  status: string;
  startedAt: string | null;
  steps: WorkflowHistoryStep[];
}

/** Only exact run addresses can associate disk history with an existing rendered workflow. */
export function unrepresentedWorkflowHistory(
  runs: WorkflowHistoryRun[],
  contexts: ContextItem[],
  representedCallIds: string[],
  liveCallIds: string[] = [],
  liveRunIds: string[] = []
): WorkflowHistoryRun[] {
  const represented = new Set(representedCallIds);
  const live = new Set(liveCallIds);
  const existing = new Map<string, Set<number> | null>();
  for (const context of contexts) {
    if (context.kind !== "tool" || context.toolName !== "workflow" || !represented.has(context.id)) continue;
    const runId = typeof context.input.runId === "string" ? context.input.runId
      : context.result?.output.match(/\bworkflow:([A-Za-z0-9_-]+)/)?.[1];
    if (!runId) continue;
    if (live.has(context.id)) { existing.set(runId, null); continue; }
    const indices = new Set<number>();
    for (const step of context.subagent?.contexts ?? []) {
      if (step.kind === "tool" && step.toolName === "workflow_step" && step.input.runId === runId
        && typeof step.input.stepIndex === "number") indices.add(step.input.stepIndex);
    }
    existing.set(runId, indices);
  }
  const seen = new Set<string>();
  return runs.flatMap((run) => {
    if (seen.has(run.runId) || liveRunIds.includes(run.runId)) return [];
    seen.add(run.runId);
    if (!existing.has(run.runId)) return [run];
    const indices = existing.get(run.runId);
    if (!indices) return [];
    const missing = run.steps.filter((step) => !indices.has(step.index));
    return missing.length ? [{ ...run, steps: missing }] : [];
  });
}

/** Steps with no declared phase sort after every declared one. */
const UNPHASED_ORDER = Number.MAX_SAFE_INTEGER;

/**
 * 1_240 → "1.2k", 12_400 → "12k". Below 1000 the exact count fits, so it stays
 * exact; the tenth is dropped past 10k, where it is noise.
 *
 * Deliberately the same rounding the task rows use. The two surfaces sit a few
 * hundred pixels apart and report the same run; a figure that disagreed between
 * them would read as two different numbers rather than one rounded twice.
 */
export function formatRunTokens(tokens: number): string {
  if (tokens < 1000) return String(tokens);
  const thousands = tokens / 1000;
  return `${thousands < 10 ? thousands.toFixed(1) : Math.round(thousands)}k`;
}

/**
 * Wall time as the workflow surfaces spell it: "9s", "1m 24s", "19m 47s",
 * "1h 04m". The space is the difference from the metric columns' compact form,
 * which has four figures to fit into a sidebar row and no room for it.
 */
export function formatRunElapsed(ms: number): string {
  const totalSeconds = Math.floor(ms / 1000);
  if (totalSeconds < 60) return `${totalSeconds}s`;
  const minutes = Math.floor(totalSeconds / 60);
  const seconds = totalSeconds % 60;
  if (minutes < 60) return `${minutes}m ${String(seconds).padStart(2, "0")}s`;
  return `${Math.floor(minutes / 60)}h ${String(minutes % 60).padStart(2, "0")}m`;
}

function ledgerState(state: string, blocked: boolean): WorkflowRunStepState {
  if (state === "done") return "finished";
  if (state === "error") return "failed";
  // A blocked row has been planned but not started: the scheduler is holding it
  // behind the concurrency cap, which is not the same as work in flight.
  if (blocked) return "pending";
  return "running";
}

/**
 * How a step's square reads. The two flags outrank the state because they say
 * something the state cannot: a replayed step is "finished" without having run,
 * and a skipped one is an error row carrying a sentinel rather than a failure.
 */
export type WorkflowStepTone = WorkflowRunStepState | "skipped" | "cached";

export function stepTone(step: WorkflowRunStep): WorkflowStepTone {
  if (step.skipped) return "skipped";
  if (step.cached) return "cached";
  return step.state;
}

/** The tone a phase header takes: whatever its most notable step is doing. */
export function phaseTone(phase: WorkflowRunPhase): WorkflowStepTone {
  const tones = phase.steps.map(stepTone);
  if (tones.includes("failed")) return "failed";
  if (tones.includes("running")) return "running";
  if (tones.includes("pending")) return "pending";
  return phase.done === phase.total && phase.total > 0 ? "finished" : "pending";
}

/**
 * Fold a workflow task item, and the live ledger when there is one, into the
 * view both surfaces render.
 *
 * Pure and total: the same inputs always yield the same view, and a missing
 * ledger degrades to the roster rather than to an empty run.
 */
export function deriveWorkflowRun(
  item: WorkflowTaskItem,
  progress: WorkflowProgressView | null = null
): WorkflowRunView {
  type Slot = WorkflowRunStep & { phase: string | null; phaseIndex: number; order: number };

  // The ledger goes in first: it is the only source that knows the plan's shape
  // including the slots no agent ever occupied. The roster then claims the rows
  // it can and overwrites everything the ledger cannot report.
  const rows = (progress?.phases ?? []).flatMap((phase) => phase.steps.map((row) => ({
    row,
    phase: phase.phaseIndex === null ? null : phase.heading,
    phaseIndex: phase.phaseIndex ?? UNPHASED_ORDER
  })));
  const claimed = new Set<number>();

  /** The ledger row this step occupies, by stated index or by label. */
  function claim(step: SubagentView): (typeof rows)[number] | null {
    const free = rows.filter((entry) => !claimed.has(entry.row.index));
    const chosen = (step.stepIndex === null
      ? undefined
      : free.find((entry) => entry.row.index === step.stepIndex))
      ?? free.find((entry) => entry.row.label === step.label);
    if (!chosen) return null;
    claimed.add(chosen.row.index);
    return chosen;
  }

  const slots: Slot[] = [];
  // `children` is built by mapping over `steps`, so the two arrays are index-
  // aligned and the child carries the metrics already computed against `now`.
  item.steps.forEach((step, position) => {
    const metrics = item.children[position]?.metrics ?? null;
    const matched = claim(step);
    slots.push({
      // Plan order when the ledger could supply it; otherwise spawn order, which
      // is the only ordering the roster alone can honestly claim.
      key: `agent:${step.id}`,
      planIndex: matched?.row.index ?? null,
      order: matched?.row.index ?? rows.length + position,
      label: step.label,
      state: taskStateForStatus(step.status),
      agentId: step.id,
      role: step.role?.name ?? null,
      modelId: step.modelId ?? step.role?.modelId ?? null,
      tokens: metrics?.tokens ?? null,
      elapsedMs: metrics?.elapsedMs ?? null,
      // An agent exists for this slot, so it was neither replayed nor blocked.
      // Only a skip can still be true: the user skips a step that is running.
      cached: false,
      blocked: false,
      skipped: matched?.row.skipped ?? false,
      phase: step.phase ?? matched?.phase ?? null,
      phaseIndex: step.phaseIndex ?? matched?.phaseIndex ?? UNPHASED_ORDER
    });
  });

  // Whatever the roster could not claim is a slot with no agent: replayed from
  // the resume journal, still queued behind the concurrency cap, or not started.
  for (const entry of rows) {
    if (claimed.has(entry.row.index)) continue;
    slots.push({
      key: `slot:${entry.row.index}`,
      planIndex: entry.row.index,
      order: entry.row.index,
      label: entry.row.label,
      state: entry.row.skipped ? "failed" : ledgerState(entry.row.state, entry.row.blocked),
      agentId: null,
      role: null,
      modelId: null,
      tokens: null,
      elapsedMs: null,
      cached: entry.row.cached,
      blocked: entry.row.blocked,
      skipped: entry.row.skipped,
      phase: entry.phase,
      phaseIndex: entry.phaseIndex
    });
  }

  const ordered = slots.sort((left, right) => left.order - right.order);
  // Phase membership was only ever a grouping key. Dropping it here means both
  // the flat strip and the tables share one object per step, so a consumer
  // cannot accidentally read a field the view does not promise.
  const steps: WorkflowRunStep[] = ordered.map((slot) => ({
    key: slot.key,
    planIndex: slot.planIndex,
    label: slot.label,
    state: slot.state,
    agentId: slot.agentId,
    role: slot.role,
    modelId: slot.modelId,
    tokens: slot.tokens,
    elapsedMs: slot.elapsedMs,
    cached: slot.cached,
    blocked: slot.blocked,
    skipped: slot.skipped
  }));

  const groups = new Map<string, WorkflowRunPhase & { phaseIndex: number }>();
  ordered.forEach((slot, position) => {
    // Keyed by declared index, not by title: a plan may legitimately reuse a
    // phase name, and merging those two groups would reorder the plan on screen.
    const key = slot.phaseIndex === UNPHASED_ORDER ? "unphased" : String(slot.phaseIndex);
    let group = groups.get(key);
    if (!group) {
      group = { key, heading: slot.phase, steps: [], done: 0, total: 0, phaseIndex: slot.phaseIndex };
      groups.set(key, group);
    }
    group.steps.push(steps[position]);
    group.total += 1;
    if (slot.state === "finished") group.done += 1;
  });

  const phases = [...groups.values()]
    .sort((left, right) => left.phaseIndex - right.phaseIndex)
    .map((phase) => ({
      key: phase.key,
      heading: phase.heading,
      steps: phase.steps,
      done: phase.done,
      total: phase.total
    }));

  return {
    id: item.id,
    name: item.label,
    scriptName: item.agent.scriptName,
    description: item.agent.task.trim(),
    state: item.state,
    stepCount: ordered.length,
    elapsedMs: item.metrics.elapsedMs,
    tokens: item.metrics.tokens,
    phases,
    steps,
    logs: progress?.logs.map((log) => log.message) ?? [],
    running: item.state === "running"
  };
}
