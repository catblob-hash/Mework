import { describe, expect, it } from "vitest";
import { cumulativeModelRunUsage, reduceModelStreamEvent, type ModelRunState } from "./modelStream";
import { streamingContextsFromModelRun } from "./runContexts";
import { deriveSubagentViews } from "./subagents";
import { deriveTaskItems, taskStateForStatus } from "./taskContainer";
import { deriveWorkflowRun, type WorkflowTaskItem } from "./workflowRuns";
import { deriveWorkflowProgress } from "./workflowProgress";
import type { ContextItem, JsonObject, ModelRunRequest, ModelStreamEvent } from "../types";

const messages = {
  fallbackLabel: (index: number) => `子代理 ${index}`,
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

const taskMessages = {
  workflowLabel: "工作流",
  runningStepCount: (running: number, total: number) => `${running}/${total} 个步骤进行中`,
  stepCount: (total: number) => `${total} 个步骤`,
  terminalIdle: "空闲",
  terminalBusy: "正在执行命令",
  shellRunning: "正在运行",
  shellStopping: "正在中止",
  shellExited: (code: number) => `已失败（退出码 ${code}）`,
  shellFinished: "已完成",
  shellFailed: "已失败",
  shellStopped: "已中止",
  browserLabel: "浏览器页面",
  browserLoading: "正在加载",
  browserSuspended: "已挂起",
  browserIdle: "已就绪",
  browserAutomation: (tool: string) => `模型正在操作：${tool}`,
  userAborted: "用户中止操作",
  planLabel: "实施计划",
  planDrafting: "撰写中",
  planAwaitingApproval: "待批准",
  planApproved: "已批准",
  planRejected: "已退回",
  planUpdatedAgo: (minutes: number) => (minutes === 0 ? "刚刚更新" : `${minutes} 分钟前更新`),
  forkApproved: "已创建子对话 · 点击打开",
  forkDeclined: "用户拒绝了分叉"
};

/**
 * The renderer's view of a workflow while its SSE stream is still open.
 *
 * The backend announces each step and then immediately reports its identity
 * (`workflow.rs` spawn_step), so mid-stream every step already carries the
 * label, phase and prompt it will keep once the run settles. These fixtures are
 * what `App.tsx` holds at that moment: a streaming `workflow` context whose
 * live transcript owns one streaming `workflow_step` context per step.
 */
describe("streaming workflow projection", () => {
  /** One step as it exists between `tool_call_arguments_ready` and completion. */
  function streamingStep(callId: string, input: JsonObject): ContextItem {
    return {
      id: callId,
      kind: "tool",
      toolName: "workflow_step",
      input,
      result: { success: true, output: "", executedAt: "2026-08-01T00:00:00Z", durationMs: 0 },
      streaming: true,
      streamStatus: "ready",
      // The worker's own `running` status delta, wrapped twice by the driver.
      live: { contexts: [], updates: [], status: "running" },
      createdAt: "2026-08-01T00:00:00Z"
    };
  }

  function streamingRun(steps: ContextItem[]): ContextItem {
    return {
      id: "call-workflow",
      kind: "tool",
      toolName: "workflow",
      input: { name: "review-run", scriptName: "review-proxy", scriptBytes: 100, scriptSha256: "ab" },
      result: { success: true, output: "", executedAt: "2026-08-01T00:00:00Z", durationMs: 0 },
      streaming: true,
      streamStatus: "running",
      live: { contexts: steps, updates: [], status: "running" },
      createdAt: "2026-08-01T00:00:00Z"
    };
  }

  const twoPhaseRun = () => streamingRun([
    streamingStep("call-workflow-ws1", {
      label: "审查一", phase: "Review", phaseIndex: 0, task: "审查代理内核"
    }),
    streamingStep("call-workflow-ws2", {
      label: "审查二", phase: "Review", phaseIndex: 0, task: "审查渲染层"
    }),
    streamingStep("call-workflow-ws3", {
      label: "验证一", phase: "Verify", phaseIndex: 1, task: "验证第一条结论"
    })
  ]);

  it("names every step in flight instead of repeating one generic label", () => {
    const views = deriveSubagentViews([twoPhaseRun()], messages);
    const steps = views.filter((view) => view.depth === 1);

    expect(steps.map((step) => step.label).sort()).toEqual(["审查一", "审查二", "验证一"]);
    // The whole point: no two rows read the same.
    expect(new Set(steps.map((step) => step.label)).size).toBe(steps.length);
    expect(steps.every((step) => step.label !== messages.workflowStepLabel)).toBe(true);
  });

  it("carries each step's received prompt into its own transcript", () => {
    const views = deriveSubagentViews([twoPhaseRun()], messages);
    const step = views.find((view) => view.label === "审查一");

    expect(step?.task).toBe("审查代理内核");
    // Opening the step shows that prompt as the user message that started it.
    expect(step?.contexts[0]).toMatchObject({ kind: "user", content: "审查代理内核" });
  });

  it("keeps declared phases apart while the run is still streaming", () => {
    const views = deriveSubagentViews([twoPhaseRun()], messages);
    const [item] = deriveTaskItems({ agents: views, terminals: [] }, taskMessages);

    expect(item).toBeDefined();
    if (!item) return;
    expect(item.kind).toBe("workflow");
    if (item.kind !== "workflow") return;
    // The run `name` is its task address; `meta.name` is the script subtitle.
    // A step count is only a fallback when `scriptName` is absent.
    expect(item.label).toBe("review-run");
    expect(item.detail).toBe("review-proxy");
    // Two declared phases stay two groups rather than collapsing into one.
    expect(item.phases.map((phase) => phase.phase)).toEqual(["Review", "Verify"]);
    expect(item.phases.map((phase) => phase.steps.length)).toEqual([2, 1]);
  });

  it("still distinguishes steps a plan left unlabelled", () => {
    // No label and no phase: the host falls back to the step's ordinal, which
    // must remain unique per step.
    const run = streamingRun([
      streamingStep("call-workflow-ws1", { label: "ws1", task: "第一步" }),
      streamingStep("call-workflow-ws2", { label: "ws2", task: "第二步" })
    ]);
    const steps = deriveSubagentViews([run], messages).filter((view) => view.depth === 1);

    expect(steps.map((step) => step.label).sort()).toEqual(["ws1", "ws2"]);
    expect(steps.map((step) => step.task).sort()).toEqual(["第一步", "第二步"]);
  });

  /**
   * Each step's role is available from the first streamed frame.
   *
   * The host streams its resolved binding with step identity because a persisted
   * record is unavailable until the workflow completes, and externalized steps
   * are available only through on-demand drawer IPC.
   */
  it("names each step's bound role from its first frame", () => {
    const run = streamingRun([
      streamingStep("call-workflow-ws1", {
        label: "审查一",
        phase: "Review",
        phaseIndex: 0,
        task: "审查代理内核",
        role: "reviewer",
        roleModelId: "sonnet-5"
      }),
      streamingStep("call-workflow-ws2", {
        label: "无角色", phase: "Review", phaseIndex: 0, task: "沿用对话的模型"
      })
    ]);
    const views = deriveSubagentViews([run], messages);
    const byLabel = new Map(views.filter((view) => view.depth === 1)
      .map((view) => [view.label, view]));

    expect(byLabel.get("审查一")?.role).toEqual({ name: "reviewer", modelId: "sonnet-5" });
    // A role-less step has no binding; do not fabricate one from the conversation model.
    expect(byLabel.get("无角色")?.role).toBeNull();
    expect(byLabel.get("无角色")?.modelId).toBeNull();

    // The task surface draws the row without opening any step.
    const [item] = deriveTaskItems(
      { agents: views, terminals: [], inheritedModelId: "会话模型" },
      taskMessages
    );
    expect(Object.fromEntries((item?.children ?? []).map((child) => [child.label, child.detail])))
      .toEqual({ 审查一: "reviewer", 无角色: "会话模型" });
  });
});

const AT = "2026-08-25T00:00:00.000Z";
const WF = "wf-1";
const STEP = ["wf-1-ws0", "wf-1-ws1"];
const LABEL = ["alpha", "beta"];
const STEP_USAGE = [
  { inputTokens: 1_200, cachedInputTokens: 400, outputTokens: 90 },
  { inputTokens: 800, outputTokens: 30 }
];

function runFixture(): ModelRunState {
  return {
    requestId: "run_test",
    providerName: "provider",
    modelName: "model",
    workspaceId: "ws",
    request: {
      contexts: [],
      provider: { family: "openai_responses" },
      model: { capabilities: [] }
    } as unknown as ModelRunRequest,
    startedAt: AT,
    streamedTextByRound: {},
    streamedReasoningByRound: {},
    completedReasoningByRound: {},
    reasoningStartedAtByRound: {},
    reasoningDurationByRound: {},
    streamedToolsByRound: {},
    streamedHooksByRound: {},
    steeredInputsByRound: {},
    usageByRound: {},
    subagentUsageByCall: {},
    workflowProgressByCall: {},
    workflowRunIdByCall: {},
    usageRevision: 0
  };
}

function nested(event: ModelStreamEvent): ModelStreamEvent {
  return { type: "subagent_event", round: 0, callId: WF, event };
}

function progress(
  index: number,
  state: "start" | "progress" | "done" | "error",
  blocked = false
): ModelStreamEvent {
  return {
    type: "workflow_progress",
    round: 0,
    callId: WF,
    runId: "run-1",
    entry: {
      kind: "agent",
      index,
      state,
      label: LABEL[index],
      phase: "扇出",
      phaseIndex: 0,
      blocked
    }
  };
}

/**
 * The wire, verbatim: what a two-step workflow whose steps both succeed puts on
 * the stream, in host order.
 *
 * The shape that matters is what is *absent*. `spawn_step`
 * (`src-tauri/src/workflow.rs`) announces the synthetic step call and sends its
 * identity, and then nothing else addresses that call id as a tool — a step
 * reports its outcome as an agent status delta, and its real result is only
 * minted when the whole run returns. The driver's own terminal status delta on
 * the *workflow's* call id is the last event before the tool result.
 */
function successfulTwoStepRun(): ModelStreamEvent[] {
  return [
    { type: "tool_call_announced", round: 0, callId: WF, toolName: "workflow", contextId: "ctx-wf" },
    {
      type: "tool_call_arguments_ready",
      round: 0,
      callId: WF,
      input: { scriptName: "progress", scriptBytes: 100, scriptSha256: "ab" }
    },
    { type: "tool_execution_started", round: 0, callId: WF },
    progress(0, "start", true),
    progress(1, "start", true),
    ...STEP.flatMap((callId, index) => [
      nested({
        type: "tool_call_announced",
        round: 0,
        callId,
        toolName: "workflow_step",
        contextId: `ctx-ws${index}`
      }),
      nested({
        type: "tool_call_arguments_ready",
        round: 0,
        callId,
        input: {
          label: LABEL[index],
          phase: "扇出",
          phaseIndex: 0,
          task: `step-${LABEL[index]}`,
          // The host writes the binding it actually applied onto the step's
          // shell, so the role is on the wire before the step has run a token.
          // Only the first step names one, which is what keeps the role-less
          // fallback in this fixture rather than in a second one.
          ...(index === 0 ? { role: "auditor", roleModelId: "sonnet-5" } : {})
        }
      }),
      nested({ type: "subagent_delta", round: 0, callId, channel: "status", delta: "running" }),
      progress(index, "progress")
    ]),
    ...STEP.flatMap((callId, index) => [
      nested({
        type: "subagent_event",
        round: 0,
        callId,
        event: { type: "text_delta", round: 0, delta: `${LABEL[index]} 完成了` }
      }),
      // The step's provider snapshot, wrapped twice like everything else it
      // emits: the run's envelope around the step's own.
      nested({
        type: "subagent_event",
        round: 0,
        callId,
        event: { type: "usage_updated", round: 0, usage: STEP_USAGE[index] }
      }),
      nested({ type: "subagent_delta", round: 0, callId, channel: "status", delta: "idle" }),
      progress(index, "done")
    ]),
    { type: "subagent_delta", round: 0, callId: WF, channel: "status", delta: "idle" },
    {
      type: "tool_execution_completed",
      round: 0,
      callId: WF,
      result: {
        success: true,
        output: JSON.stringify({ results: ["alpha 完成了", "beta 完成了"] }),
        executedAt: AT,
        durationMs: 12
      }
    }
  ] satisfies ModelStreamEvent[];
}

function reduceAll(events: ModelStreamEvent[]): ModelRunState {
  let run = runFixture();
  for (const event of events) run = reduceModelStreamEvent(run, event, AT).run;
  return run;
}

function stepViews(events: ModelStreamEvent[]) {
  return deriveSubagentViews(
    streamingContextsFromModelRun(reduceAll(events)),
    messages
  ).filter((view) => view.depth === 1);
}

/**
 * The same run driven through the actual reducer rather than a hand-built
 * fixture, because the defect this pins lived in a transition no fixture had:
 * the driver's terminal status delta on the workflow's own call id used to run
 * `settleSubagentContexts` over the live step rows, and that function read a
 * missing `tool_execution_completed` as an interruption. Steps never get one,
 * so every step of every workflow — successful or not — turned into a failed
 * task row for the rest of the streaming turn.
 */
describe("a workflow that finishes while its turn is still streaming", () => {
  it("draws the progress card from the host's own transitions", () => {
    const view = deriveWorkflowProgress(reduceAll(successfulTwoStepRun()).workflowProgressByCall[WF] ?? []);

    expect(view.phases.flatMap((phase) => phase.steps).map((step) => step.state)).toEqual([
      "done",
      "done"
    ]);
    expect(view.failed).toBe(0);
  });

  it("does not report a finished step as a failure", () => {
    const steps = stepViews(successfulTwoStepRun());

    expect(steps.map((step) => step.label).sort()).toEqual(["alpha", "beta"]);
    expect(steps.map((step) => step.status)).toEqual(["completed", "completed"]);
    expect(steps.map((step) => taskStateForStatus(step.status))).toEqual([
      "finished",
      "finished"
    ]);
  });

  it("bills each step's tokens to that step and to the turn", () => {
    // A step's usage arrives inside two envelopes, and accounting that read
    // only the outer one dropped it entirely: the step row said "—" until
    // someone opened it, and the turn's own total silently omitted whatever
    // the whole workflow had spent.
    const byLabel = Object.fromEntries(
      stepViews(successfulTwoStepRun()).map((step) => [step.label, step.usage])
    );
    expect(byLabel).toEqual({ alpha: STEP_USAGE[0], beta: STEP_USAGE[1] });

    const run = reduceAll(successfulTwoStepRun());
    expect(run.subagentUsageByCall).toEqual({
      [`${WF}/${STEP[0]}`]: { 0: STEP_USAGE[0] },
      [`${WF}/${STEP[1]}`]: { 0: STEP_USAGE[1] }
    });
    expect(cumulativeModelRunUsage(run)).toEqual({
      inputTokens: 2_000,
      cachedInputTokens: 400,
      outputTokens: 120
    });

    // The task tree shows the same figures without anyone opening a step.
    const [item] = deriveTaskItems(
      { agents: deriveSubagentViews(streamingContextsFromModelRun(run), messages), terminals: [] },
      taskMessages
    );
    expect(item?.children.map((child) => child.metrics.tokens).sort((a, b) => (a ?? 0) - (b ?? 0)))
      .toEqual([830, 1_290]);
  });

  /**
   * The same claim as the tokens column above, for the one field it did not
   * cover: the run's own step rows say which role each step is bound to, from
   * the wire, with nothing opened.
   *
   * This is driven through the reducer rather than a hand-built context tree
   * because that is where the defect lived — the projection read the role off
   * the nested record and nothing else, and a streaming step has no record.
   */
  it("names each step's role in both surfaces without anyone opening a step", () => {
    const views = deriveSubagentViews(
      streamingContextsFromModelRun(reduceAll(successfulTwoStepRun())),
      messages
    );
    const [item] = deriveTaskItems(
      { agents: views, terminals: [], inheritedModelId: "会话模型" },
      taskMessages
    );

    expect(item?.kind).toBe("workflow");
    expect(Object.fromEntries((item?.children ?? []).map((child) => [child.label, child.detail])))
      .toEqual({ alpha: "auditor", beta: "会话模型" });

    // The run panel draws the same steps from the same projection, so the two
    // surfaces cannot disagree about which role is answering. Keyed by label
    // rather than by position: with no progress ledger to state plan order,
    // slots fall back to spawn order, which is not what this pins.
    const run = deriveWorkflowRun(item as WorkflowTaskItem);
    expect(Object.fromEntries(run.steps.map((step) => [step.label, [step.role, step.modelId]])))
      .toEqual({ alpha: ["auditor", "sonnet-5"], beta: [null, null] });
  });

  it("keeps the run's own row out of the red once its steps are done", () => {
    const views = deriveSubagentViews(
      streamingContextsFromModelRun(reduceAll(successfulTwoStepRun())),
      messages
    );
    const [item] = deriveTaskItems({ agents: views, terminals: [] }, taskMessages);

    expect(item?.kind).toBe("workflow");
    expect(item?.state).toBe("finished");
    expect(item?.error).toBeNull();
  });

  it("reads the same before and after the driver's terminal status delta", () => {
    const events = successfulTwoStepRun();
    const cut = events.findIndex(
      (event) => event.type === "subagent_delta" && event.callId === WF
    );

    expect(stepViews(events.slice(0, cut)).map((step) => step.status)).toEqual([
      "completed",
      "completed"
    ]);
    expect(stepViews(events.slice(0, cut + 1)).map((step) => step.status)).toEqual([
      "completed",
      "completed"
    ]);
  });

  it("still reports a step that really failed", () => {
    const steps = stepViews(successfulTwoStepRun().map((event) => (
      event.type === "subagent_event"
      && event.event.type === "subagent_delta"
      && event.event.callId === STEP[1]
      && event.event.delta === "idle"
        ? { ...event, event: { ...event.event, delta: "failed" } } as ModelStreamEvent
        : event
    )));
    const byLabel = new Map(steps.map((step) => [step.label, step.status]));

    expect(byLabel.get("alpha")).toBe("completed");
    expect(byLabel.get("beta")).toBe("failed");
    expect(taskStateForStatus(byLabel.get("beta")!)).toBe("failed");
  });

  it("still reports a step the parent turn cut short", () => {
    // No terminal lifecycle of its own: the step was still running when the
    // workflow ended, which is the case the interrupted-result rule exists for.
    const events = successfulTwoStepRun().filter((event) => !(
      event.type === "subagent_event"
      && event.event.type === "subagent_delta"
      && event.event.callId === STEP[1]
      && event.event.delta === "idle"
    ));
    const byLabel = new Map(stepViews(events).map((step) => [step.label, step.status]));

    expect(byLabel.get("alpha")).toBe("completed");
    expect(byLabel.get("beta")).toBe("interrupted");
  });
});
