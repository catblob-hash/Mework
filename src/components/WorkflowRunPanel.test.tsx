import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { deriveWorkflowItems } from "../lib/taskContainer";
import { deriveWorkflowProgress } from "../lib/workflowProgress";
import { deriveWorkflowRun } from "../lib/workflowRuns";
import type { WorkflowProgressView } from "../lib/workflowProgress";
import type { SubagentView } from "../lib/subagents";
import { subagentViewFixture, taskMessagesFixture } from "../test/fixtures";
import { WorkflowRunPanel } from "./WorkflowRunPanel";

const NOW = Date.parse("2026-08-01T00:20:12Z");

function runView(steps: Partial<SubagentView>[], progress: WorkflowProgressView | null = null) {
  const agents: SubagentView[] = [
    // Three deliberately distinct identities so assertions catch field swapping
    // or fallback: `name` is the model-supplied workflow name and task address,
    // `scriptName` is the script's `meta.name`, and `label` is only the legacy
    // fallback.
    subagentViewFixture("run", {
      workflowRun: true,
      name: "fetch-models-gate-audit",
      scriptName: "gate-audit-plan",
      label: "legacy-driver-handle",
      task: "Audit every pre-flight gate on the 拉取模型 path",
      status: "running",
      completedAt: null,
      childIds: steps.map((_step, index) => `s${index}`),
      usage: { totalTokens: 87_200 },
      createdAt: "2026-08-01T00:00:00Z"
    }),
    ...steps.map((step, index) => subagentViewFixture(`s${index}`, {
      parentId: "run",
      depth: 1,
      callIds: [`wf-ws${index + 1}`],
      ...step
    }))
  ];
  return deriveWorkflowRun(deriveWorkflowItems(agents, taskMessagesFixture, NOW)[0], progress);
}

const AUDIT_STEPS: Partial<SubagentView>[] = [
  {
    label: "audit:rust-path",
    phase: "Audit",
    phaseIndex: 0,
    role: { name: "auditor", modelId: "sonnet-5" },
    modelId: "sonnet-5",
    usage: { totalTokens: 87_200 },
    createdAt: "2026-08-01T00:00:00Z",
    completedAt: "2026-08-01T00:05:46Z"
  },
  {
    label: "audit:renderer-path",
    phase: "Audit",
    phaseIndex: 0,
    role: { name: "auditor", modelId: "sonnet-5" },
    modelId: "sonnet-5",
    status: "running",
    createdAt: "2026-08-01T00:00:00Z",
    completedAt: null
  }
];

/** The one bar that heads a run. It is not a control, so it is addressed
    structurally rather than by role. */
function runBar(): HTMLElement {
  const bar = document.querySelector(".workflow-panel__bar");
  if (!bar) throw new Error("the run panel drew no header bar");
  return bar as HTMLElement;
}

/** One step's tile. `.workflow-step` is the component's own stable block. */
function stepTile(label: string): HTMLElement {
  const tile = screen.getByText(label).closest(".workflow-step");
  if (!tile) throw new Error(`no step tile for ${label}`);
  return tile as HTMLElement;
}

describe("WorkflowRunPanel", () => {
  it("states the plan, its wall time and its token total", () => {
    render(<WorkflowRunPanel view={runView(AUDIT_STEPS)} />);
    const panel = screen.getByRole("region", { name: "工作流 fetch-models-gate-audit" });

    expect(panel.querySelector(".workflow-panel__meta")).toHaveTextContent("20m 12s");
    expect(within(panel).getByText("87k token")).toBeInTheDocument();
    expect(within(panel).getByText("Audit every pre-flight gate on the 拉取模型 path")).toBeInTheDocument();
    expect(within(panel).getByText("Audit")).toBeInTheDocument();
    expect(within(panel).queryByRole("columnheader")).not.toBeInTheDocument();
  });

  it("titles the bar with the name the model gave this run", () => {
    render(<WorkflowRunPanel view={runView(AUDIT_STEPS)} />);
    const bar = runBar();

    // The run name is its address; the bar is not a control.
    expect(bar).not.toHaveAttribute("role", "button");
    expect(within(bar).getByText("fetch-models-gate-audit")).toBeInTheDocument();
    // The subtitle is the script name, not a fallback step count.
    expect(within(bar).getByText("gate-audit-plan")).toBeInTheDocument();
    expect(within(bar).queryByText("2 个步骤")).not.toBeInTheDocument();
    expect(within(bar).getByText("20m 12s")).toBeInTheDocument();
    expect(within(bar).getByText("87k token")).toBeInTheDocument();
    // `label` is only a legacy fallback and must not appear when a name exists.
    expect(screen.queryByText("legacy-driver-handle")).not.toBeInTheDocument();
  });

  it("lists each agent by the role it was bound to, with its tokens and its time", () => {
    render(<WorkflowRunPanel view={runView(AUDIT_STEPS)} />);

    const done = stepTile("audit:rust-path");
    // The role, not the model: which model answers for a role is the user's
    // configuration, and two roles on one model are still two different jobs.
    expect(within(done).getByText("auditor")).toBeInTheDocument();
    expect(within(done).queryByText("sonnet-5")).not.toBeInTheDocument();
    // The model stays reachable — it is the tile's tooltip.
    expect(done.querySelector(".workflow-step__main"))
      .toHaveAttribute("title", "模型：sonnet-5");
    expect(within(done).getByText("87k")).toBeInTheDocument();
    expect(within(done).getByText("5m 46s")).toBeInTheDocument();

    // Usage is only written when the host records the agent's turn, so a step
    // still in flight reports a dash rather than a zero it has not earned.
    const running = stepTile("audit:renderer-path");
    expect(within(running).getAllByText("—")).toHaveLength(1);
    expect(within(running).getByText("20m 12s")).toBeInTheDocument();
  });

  /** A plan may leave a step role-less, and then the model it inherits is the
      only thing the tile has left to report. */
  it("falls back to the model on a step that named no role", () => {
    const view = runView([
      { ...AUDIT_STEPS[0], label: "audit:no-role", role: null, modelId: "opus-5" }
    ]);
    render(<WorkflowRunPanel view={view} />);

    expect(within(stepTile("audit:no-role")).getByText("opus-5")).toBeInTheDocument();
  });

  it("counts a phase's finished steps in its header", () => {
    render(<WorkflowRunPanel view={runView(AUDIT_STEPS)} />);
    const head = document.querySelector(".workflow-phase__head") as HTMLElement;

    expect(head).toHaveTextContent("Audit");
    expect(head).toHaveTextContent("1/2");
    expect(head.closest("button")).toBeNull();
    expect(document.querySelectorAll(".workflow-phase__pip")).toHaveLength(0);
  });

  it("opens a step's transcript and never the run's own", async () => {
    const onOpenAgent = vi.fn();
    render(<WorkflowRunPanel view={runView(AUDIT_STEPS)} onOpenAgent={onOpenAgent} />);

    await userEvent.click(screen.getByRole("button", { name: "打开步骤 audit:rust-path" }));
    expect(onOpenAgent).toHaveBeenCalledWith("s0");
    // The run is a script. There is no control anywhere in the panel that
    // claims otherwise.
    expect(onOpenAgent).not.toHaveBeenCalledWith("run");
  });

  it("offers a slot with no agent no way to open one", () => {
    // A blocked slot exists in the ledger and nowhere else: no agent was ever
    // registered for it, so there is no transcript to route to.
    const view = runView(AUDIT_STEPS, deriveWorkflowProgress([
      { kind: "agent", index: 2, state: "start", label: "queued", phase: "Audit", phaseIndex: 0, blocked: true }
    ]));
    render(<WorkflowRunPanel view={view} onOpenAgent={vi.fn()} />);

    expect(screen.getByText("queued")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "queued" })).not.toBeInTheDocument();
  });

  it("addresses the run and the step index when Skip is clicked", async () => {
    const onStepControl = vi.fn();
    // A live run always has a ledger, and the ledger is where the plan slot
    // number comes from: the roster carries no index of its own.
    const view = runView(AUDIT_STEPS, deriveWorkflowProgress([
      { kind: "agent", index: 0, state: "done", label: "audit:rust-path", phase: "Audit", phaseIndex: 0 },
      { kind: "agent", index: 1, state: "progress", label: "audit:renderer-path", phase: "Audit", phaseIndex: 0 }
    ]));
    render(
      <WorkflowRunPanel view={view} runId="run-abc" onStepControl={onStepControl} />
    );

    // Only a step still moving can be skipped: the scheduler never re-opens a
    // slot it has settled, so a Skip on a finished one would do nothing.
    expect(screen.queryByRole("button", { name: "跳过 audit:rust-path" })).not.toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "跳过 audit:renderer-path" }));
    expect(onStepControl).toHaveBeenCalledWith("run-abc", 1);
  });

  it("offers no Skip on a step it could not match to a plan slot", () => {
    // Without a ledger there is no slot number to send. Guessing one would ask
    // the host to skip whichever step that guess happened to name.
    render(
      <WorkflowRunPanel view={runView(AUDIT_STEPS)} runId="run-abc" onStepControl={vi.fn()} />
    );
    expect(screen.queryByRole("button", { name: /跳过/ })).not.toBeInTheDocument();
  });

  it("exposes no Skip on a run it cannot address", () => {
    render(<WorkflowRunPanel view={runView(AUDIT_STEPS)} onStepControl={vi.fn()} />);
    expect(screen.queryByRole("button", { name: /跳过/ })).not.toBeInTheDocument();
  });

  it("keeps the body and every step on screen: the bar is not a disclosure", () => {
    render(<WorkflowRunPanel view={runView(AUDIT_STEPS)} />);
    const bar = runBar();

    expect(bar).not.toHaveAttribute("aria-expanded");
    expect(document.querySelector(".workflow-panel__body")).toBeInTheDocument();
    expect(screen.getByText("audit:rust-path")).toBeInTheDocument();
    expect(within(bar).getByText("fetch-models-gate-audit")).toBeInTheDocument();
    expect(document.querySelector(".workflow-panel__meta")).toHaveTextContent("20m 12s");
  });

  it("stops the run without hiding the evidence", async () => {
    const onStop = vi.fn();
    render(<WorkflowRunPanel view={runView(AUDIT_STEPS)} onStop={onStop} />);

    await userEvent.click(screen.getByRole("button", { name: "中止工作流" }));
    expect(onStop).toHaveBeenCalledTimes(1);
    // Stopping must not hide the active body or steps.
    expect(document.querySelector(".workflow-panel__body")).toBeInTheDocument();
    expect(screen.getByText("audit:rust-path")).toBeInTheDocument();
  });

  it("shows the run log only when the live ledger carried one", async () => {
    const view = runView(AUDIT_STEPS, deriveWorkflowProgress([
      { kind: "log", index: 0, state: "progress", message: "2/5 found" }
    ]));
    render(<WorkflowRunPanel view={view} />);

    await userEvent.click(screen.getByRole("button", { name: "运行日志（1 条）" }));
    expect(screen.getByText("2/5 found")).toBeInTheDocument();
  });

  it("says nothing about a log a settled run no longer has", () => {
    render(<WorkflowRunPanel view={runView(AUDIT_STEPS)} />);
    expect(screen.queryByRole("button", { name: /运行日志/ })).not.toBeInTheDocument();
  });
});
