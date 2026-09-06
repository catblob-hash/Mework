import type { WorkflowProgressEntry, WorkflowStepState } from "../types";

/**
 * The workflow progress card's projection: a flat row stream folded into a
 * phase → step tree.
 *
 * # Why the frontend re-does the host's merge
 *
 * The host keeps its own ledger (`workflow-core/src/progress.rs`) and emits one
 * entry per transition. The renderer cannot simply replay the host's final
 * ledger because it never receives one — it receives the transitions, possibly
 * missing a prefix (a page reload mid-run) and possibly out of order across a
 * reconnect. So the merge rule has to be idempotent and order-insensitive on
 * this side too, which is what upserting by identity buys.
 *
 * # Row identity
 *
 * `${kind}:${index}` — the same key the host merges on. A step upserts in place
 * for its whole lifetime, so the row a user is watching never jumps when it
 * finishes. Logs carry a host-assigned monotonic index and therefore never
 * collide; they accumulate rather than merge.
 */

export interface WorkflowProgressMessages {
  /** Step label when the plan declared none. */
  fallbackStepLabel: (index: number) => string;
  /** Phase heading for steps that declared no phase. */
  unphasedHeading: string;
  cachedBadge: string;
  blockedBadge: string;
  skippedBadge: string;
  /** Accessible name of the determinate bar; receives the live counts. */
  progressLabel: (done: number, total: number) => string;
}

export const defaultWorkflowProgressMessages: WorkflowProgressMessages = {
  fallbackStepLabel: (index) => `步骤 ${index + 1}`,
  unphasedHeading: "未分组",
  cachedBadge: "已缓存",
  blockedBadge: "等待中",
  skippedBadge: "已跳过",
  progressLabel: (done, total) => `工作流进度：${total} 步中已完成 ${done} 步`
};

/** One step as the card draws it. */
export interface WorkflowStepRow {
  index: number;
  label: string;
  state: WorkflowStepState;
  preview: string | null;
  message: string | null;
  cached: boolean;
  blocked: boolean;
  skipped: boolean;
}

/** One phase group, in plan order. */
export interface WorkflowPhaseGroup {
  /** Declared phase index; null groups every unphased step together. */
  phaseIndex: number | null;
  heading: string;
  steps: WorkflowStepRow[];
}

/** One narration line. Logs are ordered but not addressable. */
export interface WorkflowLogRow {
  index: number;
  message: string;
}

export interface WorkflowProgressView {
  phases: WorkflowPhaseGroup[];
  logs: WorkflowLogRow[];
  total: number;
  done: number;
  failed: number;
  /** Accessible name for the determinate bar, numbers already interpolated. */
  progressLabel: string;
  /** True while any step is still start/progress — drives `aria-busy`. */
  running: boolean;
}

/**
 * Bounds, mirroring `MAX_PROGRESS_ROWS` / `PROGRESS_TRIM_TARGET` in
 * workflow-core.
 *
 * The renderer trims independently rather than trusting the host to have
 * trimmed: on a reconnect it can receive a backlog the host already considers
 * bounded, and an unbounded row array here is a hang in a component that
 * re-renders on every event.
 */
const MAX_LOG_ROWS = 1_000;
const LOG_TRIM_TARGET = 500;

/**
 * Fold entries into a view.
 *
 * Pure and total: the same entry applied twice yields the same view, which is
 * what makes a replayed or duplicated stream safe. Unknown row kinds are
 * dropped rather than thrown on — a host newer than the renderer must degrade
 * to a partial card, not a blank screen.
 */
export function deriveWorkflowProgress(
  entries: WorkflowProgressEntry[],
  messages: WorkflowProgressMessages = defaultWorkflowProgressMessages
): WorkflowProgressView {
  const steps = new Map<string, WorkflowStepRow & { phaseIndex: number | null; phase: string | null }>();
  const logs: WorkflowLogRow[] = [];

  for (const entry of entries) {
    if (entry.kind === "log") {
      logs.push({ index: entry.index, message: entry.message ?? "" });
      continue;
    }
    if (entry.kind !== "agent") continue;
    steps.set(`${entry.kind}:${entry.index}`, {
      index: entry.index,
      label: entry.label ?? messages.fallbackStepLabel(entry.index),
      state: entry.state,
      preview: entry.preview ?? null,
      message: entry.message ?? null,
      cached: entry.cached === true,
      blocked: entry.blocked === true,
      skipped: entry.skipped === true,
      phase: entry.phase ?? null,
      phaseIndex: entry.phaseIndex ?? null
    });
  }

  if (logs.length > MAX_LOG_ROWS) {
    logs.splice(0, logs.length - LOG_TRIM_TARGET);
  }

  const grouped = new Map<string, WorkflowPhaseGroup>();
  for (const step of [...steps.values()].sort((left, right) => left.index - right.index)) {
    // Group by declared index, not by heading text: two phases may legitimately
    // share a name, and merging them would reorder the plan on screen.
    const key = step.phaseIndex === null ? "unphased" : String(step.phaseIndex);
    let group = grouped.get(key);
    if (!group) {
      group = {
        phaseIndex: step.phaseIndex,
        heading: step.phase ?? messages.unphasedHeading,
        steps: []
      };
      grouped.set(key, group);
    }
    group.steps.push({
      index: step.index,
      label: step.label,
      state: step.state,
      preview: step.preview,
      message: step.message,
      cached: step.cached,
      blocked: step.blocked,
      skipped: step.skipped
    });
  }

  // Unphased steps sort last. They are the ones the plan never placed, so
  // hoisting them above declared phase 0 would misrepresent plan order.
  const phases = [...grouped.values()].sort((left, right) => {
    if (left.phaseIndex === null) return right.phaseIndex === null ? 0 : 1;
    if (right.phaseIndex === null) return -1;
    return left.phaseIndex - right.phaseIndex;
  });

  const all = phases.flatMap((phase) => phase.steps);
  const done = all.filter((step) => step.state === "done").length;
  // Skipped counts as failed for the totals, following the host ledger, where a
  // skip is an Error row carrying a sentinel rather than a fifth state.
  const failed = all.filter((step) => step.state === "error").length;
  const running = all.some((step) => step.state === "start" || step.state === "progress");

  return {
    phases,
    logs,
    total: all.length,
    done,
    failed,
    progressLabel: messages.progressLabel(done, all.length),
    running
  };
}
