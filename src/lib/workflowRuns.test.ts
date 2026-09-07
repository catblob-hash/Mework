import { describe, expect, it } from "vitest";
import { deriveWorkflowItems } from "./taskContainer";
import { deriveWorkflowProgress } from "./workflowProgress";
import { deriveWorkflowRun, formatRunElapsed, formatRunTokens, phaseTone, stepTone } from "./workflowRuns";
import { subagentViewFixture, taskMessagesFixture } from "../test/fixtures";
import type { SubagentView } from "./subagents";

const NOW = Date.parse("2026-08-01T00:20:12Z");

function runFrom(agents: SubagentView[]) {
  const items = deriveWorkflowItems(agents, taskMessagesFixture, NOW);
  expect(items).toHaveLength(1);
  return items[0];
}

/** A driver plus its steps, as the roster hands them over. */
function roster(steps: Partial<SubagentView>[]): SubagentView[] {
  const childIds = steps.map((_step, index) => `s${index}`);
  return [
    subagentViewFixture("run", {
      workflowRun: true,
      label: "fetch-models-gate-audit",
      task: "Audit every pre-flight gate on the 拉取模型 path",
      status: "running",
      completedAt: null,
      childIds,
      usage: { totalTokens: 87_200 }
    }),
    ...steps.map((step, index) => subagentViewFixture(`s${index}`, {
      parentId: "run",
      depth: 1,
      callIds: [`wf-ws${index + 1}`],
      ...step
    }))
  ];
}

describe("deriveWorkflowRun", () => {
  it("reads the plan's name and description off the run itself", () => {
    const view = deriveWorkflowRun(runFrom(roster([{ label: "a", phase: "Audit", phaseIndex: 0 }])));
    expect(view.name).toBe("fetch-models-gate-audit");
    expect(view.description).toBe("Audit every pre-flight gate on the 拉取模型 path");
    expect(view.tokens).toBe(87_200);
    expect(view.running).toBe(true);
  });

  it("carries each step's role, its model, its tokens and its wall time", () => {
    const view = deriveWorkflowRun(runFrom(roster([{
      label: "audit:rust-path",
      phase: "Audit",
      phaseIndex: 0,
      role: { name: "auditor", modelId: "sonnet-5" },
      usage: { totalTokens: 87_200 },
      createdAt: "2026-08-01T00:00:00Z",
      completedAt: "2026-08-01T00:05:46Z"
    }])));

    expect(view.steps[0]).toMatchObject({
      label: "audit:rust-path",
      role: "auditor",
      modelId: "sonnet-5",
      tokens: 87_200,
      state: "finished"
    });
    expect(formatRunElapsed(view.steps[0].elapsedMs as number)).toBe("5m 46s");
  });

  it("leaves the model absent when a step named no role", () => {
    // Which model answers for a role is the user's configuration. A step with
    // no role inherited its parent's model, and inventing one here would be a
    // claim the record does not make.
    const view = deriveWorkflowRun(runFrom(roster([{ label: "a", phase: "P", phaseIndex: 0 }])));
    expect(view.steps[0].role).toBeNull();
    expect(view.steps[0].modelId).toBeNull();
  });

  it("groups by declared phase index and counts each phase's finished steps", () => {
    const view = deriveWorkflowRun(runFrom(roster([
      { label: "a", phase: "Audit", phaseIndex: 0, status: "completed" },
      { label: "b", phase: "Audit", phaseIndex: 0, status: "running", completedAt: null },
      { label: "c", phase: "Verify", phaseIndex: 1, status: "running", completedAt: null }
    ])));

    expect(view.phases.map((phase) => [phase.heading, phase.done, phase.total])).toEqual([
      ["Audit", 1, 2],
      ["Verify", 0, 1]
    ]);
    expect(phaseTone(view.phases[0])).toBe("running");
  });

  it("adds the plan slots the roster cannot have, matched to steps by label", () => {
    // A replayed step registers no agent at all, and a blocked one has not
    // started, so both exist only in the ledger. The label is what lines a real
    // step up with its row: both sides render it from `step_display_label`.
    const view = deriveWorkflowRun(
      runFrom(roster([
        { label: "b", phase: "Audit", phaseIndex: 0, status: "running", completedAt: null }
      ])),
      deriveWorkflowProgress([
        { kind: "agent", index: 0, state: "done", label: "a", phase: "Audit", phaseIndex: 0, cached: true },
        { kind: "agent", index: 1, state: "progress", label: "b", phase: "Audit", phaseIndex: 0 },
        { kind: "agent", index: 2, state: "start", label: "c", phase: "Audit", phaseIndex: 0, blocked: true },
        { kind: "log", index: 0, state: "progress", message: "2/3 done" }
      ])
    );

    expect(view.steps.map((step) => [step.label, stepTone(step), step.agentId, step.planIndex])).toEqual([
      ["a", "cached", null, 0],
      ["b", "running", "s0", 1],
      ["c", "pending", null, 2]
    ]);
    expect(view.stepCount).toBe(3);
    expect(view.logs).toEqual(["2/3 done"]);
  });

  it("gives two same-named steps one row each rather than collapsing them", () => {
    // An unlabelled step falls back to its phase name, so a label is not unique
    // by construction. Claiming greedily keeps the count honest.
    const view = deriveWorkflowRun(
      runFrom(roster([
        { label: "P", phase: "P", phaseIndex: 0, status: "completed" },
        { label: "P", phase: "P", phaseIndex: 0, status: "running", completedAt: null }
      ])),
      deriveWorkflowProgress([
        { kind: "agent", index: 0, state: "done", label: "P", phase: "P", phaseIndex: 0 },
        { kind: "agent", index: 1, state: "progress", label: "P", phase: "P", phaseIndex: 0 }
      ])
    );

    expect(view.steps.map((step) => [step.agentId, step.planIndex])).toEqual([["s0", 0], ["s1", 1]]);
  });

  it("leaves a step it cannot match without a plan index rather than guessing one", () => {
    // Position is the tempting fallback and the wrong one: that number is what
    // Skip sends to the host, so a guess asks it to skip an unrelated slot.
    const view = deriveWorkflowRun(
      runFrom(roster([{ label: "renamed", phase: "P", phaseIndex: 0, status: "running", completedAt: null }])),
      deriveWorkflowProgress([
        { kind: "agent", index: 0, state: "start", label: "queued", phase: "P", phaseIndex: 0, blocked: true }
      ])
    );

    expect(view.steps.map((step) => [step.label, step.planIndex])).toEqual([
      ["queued", 0],
      ["renamed", null]
    ]);
  });

  it("keeps the roster's answer for a slot both sources describe", () => {
    // The ledger has no model and no usage, so a merge that let it win would
    // blank out the two columns it cannot fill.
    const view = deriveWorkflowRun(
      runFrom(roster([{
        label: "a",
        phase: "Audit",
        phaseIndex: 0,
        role: { name: "auditor", modelId: "sonnet-5" },
        usage: { totalTokens: 1_500 }
      }])),
      deriveWorkflowProgress([
        { kind: "agent", index: 0, state: "progress", label: "a", phase: "Audit", phaseIndex: 0 }
      ])
    );

    expect(view.steps).toHaveLength(1);
    expect(view.steps[0]).toMatchObject({ modelId: "sonnet-5", tokens: 1_500, state: "finished" });
  });

  it("keeps a skipped step marked as skipped once its agent reports back", () => {
    const view = deriveWorkflowRun(
      runFrom(roster([{ label: "a", phase: "P", phaseIndex: 0, status: "stopped" }])),
      deriveWorkflowProgress([
        { kind: "agent", index: 0, state: "error", label: "a", phase: "P", phaseIndex: 0, skipped: true }
      ])
    );
    expect(stepTone(view.steps[0])).toBe("skipped");
  });

  it("sorts unphased steps after every declared phase", () => {
    const view = deriveWorkflowRun(runFrom(roster([
      { label: "loose" },
      { label: "declared", phase: "Audit", phaseIndex: 0 }
    ])));
    expect(view.phases.map((phase) => phase.heading)).toEqual(["Audit", null]);
  });
});

describe("workflow surface formatters", () => {
  it("rounds tokens the way the task rows do", () => {
    expect(formatRunTokens(999)).toBe("999");
    expect(formatRunTokens(1_240)).toBe("1.2k");
    expect(formatRunTokens(12_400)).toBe("12k");
    expect(formatRunTokens(87_200)).toBe("87k");
  });

  it("spells wall time with a space, unlike the compact metric column", () => {
    expect(formatRunElapsed(9_000)).toBe("9s");
    expect(formatRunElapsed(84_000)).toBe("1m 24s");
    expect(formatRunElapsed(1_212_000)).toBe("20m 12s");
    expect(formatRunElapsed(3_840_000)).toBe("1h 04m");
  });
});
