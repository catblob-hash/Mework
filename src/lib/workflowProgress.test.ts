import { describe, expect, it } from "vitest";
import { deriveWorkflowProgress } from "./workflowProgress";
import type { WorkflowProgressEntry } from "../types";

function step(
  index: number,
  overrides: Partial<WorkflowProgressEntry> = {}
): WorkflowProgressEntry {
  return { kind: "agent", index, state: "start", ...overrides };
}

describe("workflow progress projection", () => {
  /**
   * The card's core promise: a step keeps ONE row for its whole lifetime.
   *
   * The host emits a transition per state change, so a naive append would draw
   * the same step three times — and worse, the finished copy would appear last,
   * so rows would visibly reorder as steps completed. Upserting by index is
   * what keeps a user's eye on the row it started on.
   */
  it("merges a step's whole lifetime into one row that never moves", () => {
    const view = deriveWorkflowProgress([
      step(0, { state: "start", label: "读取" }),
      step(1, { state: "start", label: "改写" }),
      step(0, { state: "progress", label: "读取" }),
      // Step 1 finishes FIRST. If completion appended rather than merged, it
      // would jump ahead of step 0 on screen.
      step(1, { state: "done", label: "改写" }),
      step(0, { state: "done", label: "读取" })
    ]);

    const rows = view.phases.flatMap((phase) => phase.steps);
    expect(rows).toHaveLength(2);
    expect(rows.map((row) => row.index)).toEqual([0, 1]);
    expect(rows.map((row) => row.state)).toEqual(["done", "done"]);
    expect(view.done).toBe(2);
    expect(view.running).toBe(false);
  });

  /**
   * Replaying the same stream twice must not double anything. A reconnect
   * re-delivers entries the renderer already folded in, and a card that counted
   * "4 of 2 steps" would be worse than one that lost the update.
   */
  it("is idempotent under a replayed stream", () => {
    const entries = [step(0, { state: "done" }), step(1, { state: "progress" })];
    const once = deriveWorkflowProgress(entries);
    const twice = deriveWorkflowProgress([...entries, ...entries]);
    expect(twice).toEqual(once);
  });

  /**
   * Grouping is by declared phase INDEX, not by heading text. Two phases may
   * legitimately carry the same name; merging them would silently reorder the
   * plan on screen and hide that the model declared two.
   */
  it("keeps same-named phases apart and sorts unphased steps last", () => {
    const view = deriveWorkflowProgress([
      step(0, { phase: "检查", phaseIndex: 0 }),
      step(1, { phase: "检查", phaseIndex: 1 }),
      step(2)
    ]);

    expect(view.phases.map((phase) => phase.phaseIndex)).toEqual([0, 1, null]);
    expect(view.phases.map((phase) => phase.heading)).toEqual(["检查", "检查", "未分组"]);
    expect(view.phases[2].steps.map((row) => row.index)).toEqual([2]);
  });

  /** Steps arriving out of order still render in plan order. */
  it("sorts steps by index regardless of arrival order", () => {
    const view = deriveWorkflowProgress([step(2), step(0), step(1)]);
    expect(view.phases[0].steps.map((row) => row.index)).toEqual([0, 1, 2]);
  });

  /**
   * A skip is an `error` row plus the sentinel, matching the host ledger. The
   * badge has to survive, because "skipped" and "failed" mean very different things
   * to someone deciding whether to resume.
   */
  it("carries the cached, blocked and skipped badges through", () => {
    const view = deriveWorkflowProgress([
      step(0, { state: "done", cached: true }),
      step(1, { state: "start", blocked: true }),
      step(2, { state: "error", skipped: true })
    ]);

    const rows = view.phases[0].steps;
    expect(rows[0].cached).toBe(true);
    expect(rows[1].blocked).toBe(true);
    expect(rows[2].skipped).toBe(true);
    expect(rows[2].state).toBe("error");
    expect(view.failed).toBe(1);
    expect(view.done).toBe(1);
  });

  /** The bar's accessible name has to carry the numbers, not just a percentage:
   * a screen reader user gets "3 of 5", not an unlabelled progressbar. */
  it("interpolates the live counts into the progress label", () => {
    const view = deriveWorkflowProgress([
      step(0, { state: "done" }),
      step(1, { state: "done" }),
      step(2, { state: "progress" })
    ]);
    expect(view.progressLabel).toBe("工作流进度：3 步中已完成 2 步");
    expect(view.running).toBe(true);
  });

  it("takes localized labels from the options object", () => {
    const view = deriveWorkflowProgress([step(0), step(1, { phaseIndex: 0, phase: "A" })], {
      fallbackStepLabel: (index) => `step ${index}`,
      unphasedHeading: "ungrouped",
      cachedBadge: "cached",
      blockedBadge: "blocked",
      skippedBadge: "skipped",
      progressLabel: (done, total) => `${done}/${total}`
    });

    expect(view.progressLabel).toBe("0/2");
    expect(view.phases.map((phase) => phase.heading)).toEqual(["A", "ungrouped"]);
    expect(view.phases[1].steps[0].label).toBe("step 0");
  });

  /**
   * Logs accumulate; they have no merge identity. They are also bounded here
   * rather than trusted to arrive bounded — a reconnect can deliver a backlog
   * the host already trimmed against its own window.
   */
  it("accumulates logs and trims the oldest past the window", () => {
    const logs: WorkflowProgressEntry[] = Array.from({ length: 1_200 }, (_, index) => ({
      kind: "log",
      index,
      state: "progress",
      message: `line ${index}`
    }));
    const view = deriveWorkflowProgress(logs);

    expect(view.logs).toHaveLength(500);
    expect(view.logs[0].message).toBe("line 700");
    expect(view.logs.at(-1)?.message).toBe("line 1199");
  });

  /**
   * A host newer than the renderer must degrade to a partial card, not a blank
   * one. An unknown row kind is dropped; everything around it still draws.
   */
  it("drops unknown row kinds instead of throwing", () => {
    const view = deriveWorkflowProgress([
      step(0, { state: "done" }),
      { kind: "future" as WorkflowProgressEntry["kind"], index: 9, state: "start" }
    ]);
    expect(view.total).toBe(1);
    expect(view.done).toBe(1);
  });

  it("projects an empty stream to an empty card rather than failing", () => {
    const view = deriveWorkflowProgress([]);
    expect(view.phases).toEqual([]);
    expect(view.total).toBe(0);
    expect(view.running).toBe(false);
  });
});
