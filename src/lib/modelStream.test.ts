import { contextsFromModelRun, liveReasoningFromModelRun } from "./runContexts";
import { describe, expect, it, vi } from "vitest";
import {
  createModelStreamCoalescer,
  cumulativeModelRunUsage,
  MODEL_STREAM_COMMIT_INTERVAL_MS,
  reduceModelStreamEvent,
  resetRoundForRetry,
  settleSubagentContexts,
  SUBAGENT_LIVE_TOTAL_CONTEXT_BUDGET,
  trimLiveContextBudget,
  type ModelRunState,
  type ModelStreamEffect
} from "./modelStream";
import type { ContextItem, ModelRunRequest, ModelStreamEvent, SubagentLiveState } from "../types";

function runFixture(overrides: Partial<ModelRunState> = {}): ModelRunState {
  return {
    requestId: "run_test",
    providerName: "provider",
    modelName: "model",
    workspaceId: "ws",
    request: {
      contexts: [],
      provider: { family: "openai_responses" },
      model: { capabilities: [], reasoningContent: "encrypted" }
    } as unknown as ModelRunRequest,
    startedAt: "2026-08-04T00:00:00.000Z",
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
    usageRevision: 0,
    ...overrides
  };
}

const AT = "2026-08-04T00:00:01.000Z";

function reduceAll(
  run: ModelRunState,
  events: ModelStreamEvent[]
): {
  run: ModelRunState;
  effects: ModelStreamEffect[];
} {
  const effects: ModelStreamEffect[] = [];
  let current = run;
  for (const event of events) {
    const outcome = reduceModelStreamEvent(current, event, AT);
    current = outcome.run;
    effects.push(...outcome.effects);
  }
  return { run: current, effects };
}

describe("reduceModelStreamEvent", () => {
  it("keeps interleaved reasoning live until every known item closes", () => {
    let run = reduceAll(runFixture(), [
      { type: "reasoning_start", round: 0 },
      { type: "reasoning_delta", round: 0, delta: "A" },
      { type: "reasoning_delta", round: 0, item: 1, delta: "B" },
      { type: "reasoning_done", round: 0, item: 9 },
      { type: "reasoning_done", round: 0 }
    ]).run;
    expect(run.completedReasoningByRound[0]).toBe(false);
    expect(liveReasoningFromModelRun(run)).not.toBeNull();
    expect(
      contextsFromModelRun(run, true)
        .filter((row) => row.kind === "reasoning")
        .map((row) => row.streaming)
    ).toEqual([false, true]);
    run = reduceAll(run, [{ type: "reasoning_done", round: 0, item: 1, durationMs: 42 }]).run;
    expect(run.completedReasoningByRound[0]).toBe(true);
    expect(liveReasoningFromModelRun(run)).toBeNull();
    expect(run.reasoningDurationByRound[0]).toBe(42);
    run = reduceAll(run, [{ type: "reasoning_start", round: 0, item: 1 }]).run;
    expect(run.completedReasoningByRound[0]).toBe(false);
    expect(resetRoundForRetry(run, 0).activeReasoningItemsByRound?.[0]).toBeUndefined();
  });
  it("appends text deltas per round and marks earlier reasoning complete", () => {
    const { run } = reduceAll(runFixture(), [
      { type: "reasoning_delta", round: 0, delta: "思考" },
      { type: "text_delta", round: 0, delta: "你" },
      { type: "text_delta", round: 0, delta: "好" }
    ]);
    expect(run.streamedTextByRound[0]).toBe("你好");
    expect(run.streamedReasoningByRound[0]).toEqual(["思考"]);
    // The first text delta closes the round's reasoning stream.
    expect(run.completedReasoningByRound[0]).toBe(true);
  });

  it("keeps interleaved reasoning items in their own slots", () => {
    // Interleaved reasoning items must retain separate card slots.
    const { run } = reduceAll(runFixture(), [
      { type: "reasoning_start", round: 1, item: 0 },
      { type: "reasoning_delta", round: 1, item: 0, delta: "甲" },
      { type: "reasoning_start", round: 1, item: 1 },
      { type: "reasoning_delta", round: 1, item: 1, delta: "乙" },
      { type: "reasoning_delta", round: 1, item: 0, delta: "续" }
    ]);

    expect(run.streamedReasoningByRound[1]).toEqual(["甲续", "乙"]);
    expect(run.reasoningStartedAtByRound[1]).toBe(AT);
  });

  it("buffers a completed that arrives before its announcement and applies it on announce", () => {
    // Reconnection can deliver a terminal event before its announcement.
    const result = { success: false, output: "炸了", executedAt: AT, durationMs: 2 };
    const { run, effects } = reduceAll(runFixture(), [
      { type: "tool_execution_completed", round: 1, callId: "c1", result },
      { type: "tool_call_announced", round: 1, callId: "c1", toolName: "bash", contextId: "ctx_tool_1" }
    ]);

    expect(run.streamedToolsByRound[1][0]).toMatchObject({ streamStatus: "completed", result });
    expect(run.pendingToolEventsByRound?.[1]?.c1).toBeUndefined();
    // Emit the completion effect when the buffered announcement supplies the tool name.
    expect(effects.filter((effect) => effect.kind === "tool_completed")).toEqual([
      { kind: "tool_completed", toolName: "bash", success: false, output: "炸了" }
    ]);

    // An unannounced call has no identity to project into the timeline.
    const orphan = reduceAll(runFixture(), [
      { type: "tool_execution_completed", round: 1, callId: "lost", result }
    ]);
    expect(orphan.run.streamedToolsByRound[1]).toBeUndefined();
    expect(orphan.effects.some((effect) => effect.kind === "tool_completed")).toBe(false);
  });

  it("replays buffered arguments and completion in arrival order", () => {
    // Replay every buffered event in arrival order into the same announced card.
    const { run } = reduceAll(runFixture(), [
      { type: "tool_call_arguments_ready", round: 1, callId: "c1", input: { command: "ls" } },
      {
        type: "tool_execution_completed",
        round: 1,
        callId: "c1",
        result: { success: true, output: "ok", executedAt: AT, durationMs: 2 }
      },
      { type: "tool_call_announced", round: 1, callId: "c1", toolName: "bash", contextId: "ctx_tool_1" }
    ]);

    expect(run.streamedToolsByRound[1][0]).toMatchObject({
      input: { command: "ls" },
      streamStatus: "completed"
    });
  });

  it("ignores empty deltas", () => {
    const base = runFixture();
    const { run } = reduceAll(base, [{ type: "text_delta", round: 0, delta: "" }]);
    expect(run).toBe(base);
  });

  it("turns a settled tool card into a replacement effect without touching run state", () => {
    const base = runFixture();
    const settled = {
      id: "ctx_tool_settled",
      kind: "tool" as const,
      toolName: "agent_spawn",
      round: 1,
      input: { prompt: "任务", name: "a1" },
      result: { success: true, output: "已派生", executedAt: AT, durationMs: 3 },
      subagent: {
        kind: "general" as const,
        name: "a1",
        task: "任务",
        status: "completed" as const,
        contexts: [],
        updates: [],
        usage: {}
      },
      attestation: "re-attested",
      createdAt: AT
    };
    const { run, effects } = reduceAll(base, [{ type: "tool_context_settled", round: 2, context: settled }]);
    // The live projection remains unchanged; replacement only affects the document copy.
    expect(run).toBe(base);
    expect(effects).toEqual([{ kind: "tool_context_settled", context: settled }]);
  });

  it("promotes a stranded announced row to completed when its settled card arrives", () => {
    // A settled card is terminal truth and must complete a stranded announced row.
    const { run: withRow } = reduceAll(runFixture(), [
      { type: "tool_call_announced", round: 1, callId: "c1", toolName: "bash", contextId: "ctx_tool_1" }
    ]);
    const settled = {
      id: "ctx_tool_1",
      kind: "tool" as const,
      toolName: "bash",
      round: 1,
      input: { command: "ls" },
      result: { success: false, output: "command not found", executedAt: AT, durationMs: 3 },
      createdAt: AT
    };
    const { run } = reduceAll(withRow, [{ type: "tool_context_settled", round: 1, context: settled }]);
    const row = run.streamedToolsByRound[1][0];
    expect(row.streamStatus).toBe("completed");
    expect(row.result.success).toBe(false);
    expect(row.result.output).toBe("command not found");
    expect(row.input).toEqual({ command: "ls" });
  });

  it("leaves a completed live row alone when its settled card arrives", () => {
    // A settled card must not overwrite a live row that already completed.
    const { run: withRow } = reduceAll(runFixture(), [
      { type: "tool_call_announced", round: 1, callId: "c1", toolName: "bash", contextId: "ctx_tool_1" },
      { type: "tool_call_arguments_ready", round: 1, callId: "c1", input: { command: "ls" } },
      {
        type: "tool_execution_completed",
        round: 1,
        callId: "c1",
        result: { success: true, output: "ok", executedAt: AT, durationMs: 2 }
      }
    ]);
    const settled = {
      id: "ctx_tool_1",
      kind: "tool" as const,
      toolName: "bash",
      round: 1,
      input: { command: "ls" },
      result: { success: true, output: "ok（持久化改写）", executedAt: AT, durationMs: 2 },
      createdAt: AT
    };
    const { run } = reduceAll(withRow, [{ type: "tool_context_settled", round: 1, context: settled }]);
    expect(run).toBe(withRow);
    expect(run.streamedToolsByRound[1][0].result.output).toBe("ok");
  });

  it("drops the failed round's partials when the first post-retry event arrives", () => {
    const base = runFixture({
      streamedTextByRound: { 1: "旧内容" },
      usageByRound: { 1: { inputTokens: 5, outputTokens: 5 } },
      retry: { round: 1, attempt: 2, maxAttempts: 3, message: "retrying", dirty: true }
    });
    const { run, effects } = reduceAll(base, [{ type: "text_delta", round: 1, delta: "新" }]);
    expect(run.streamedTextByRound[1]).toBe("新");
    expect(run.retry).toBeUndefined();
    expect(run.usageByRound[1]).toBeUndefined();
    // Publish the reduced cumulative usage immediately after removing failed-round usage.
    expect(effects).toContainEqual({
      kind: "turn_usage",
      usage: cumulativeModelRunUsage({ ...resetRoundForRetry(base, 1), retry: undefined }),
      revision: base.usageRevision
    });
  });

  it("keeps subagent and hook events out of retry resets", () => {
    const base = runFixture({
      streamedTextByRound: { 1: "部分" },
      retry: { round: 1, attempt: 1, maxAttempts: 3, message: "m", dirty: true }
    });
    const { run } = reduceAll(base, [
      { type: "subagent_delta", round: 1, callId: "call", channel: "text", delta: "x" }
    ]);
    expect(run.streamedTextByRound[1]).toBe("部分");
    expect(run.retry).toBeDefined();
  });

  it("records every terminal lifecycle status a child reports", () => {
    // The status channel's wire vocabulary is `AgentLiveStatus::wire`. Dropping
    // the terminal values would pin the child to its last-known state, so a run
    // that failed or was stopped would keep rendering as if it were still going.
    for (const status of ["failed", "stopped", "roundLimit"] as const) {
      const { run } = reduceAll(runFixture(), [
        {
          type: "tool_call_announced",
          round: 1,
          callId: "call",
          toolName: "agent_spawn",
          contextId: "ctx-call"
        },
        { type: "subagent_delta", round: 1, callId: "call", channel: "status", delta: "running" },
        { type: "subagent_delta", round: 1, callId: "call", channel: "status", delta: status }
      ]);
      expect(run.streamedToolsByRound[1][0].live.status).toBe(status);
    }
  });

  it("treats an empty status delta as a heartbeat and keeps the current status", () => {
    const { run } = reduceAll(runFixture(), [
      {
        type: "tool_call_announced",
        round: 1,
        callId: "call",
        toolName: "agent_spawn",
        contextId: "ctx-call"
      },
      { type: "subagent_delta", round: 1, callId: "call", channel: "status", delta: "running" },
      { type: "subagent_delta", round: 1, callId: "call", channel: "status", delta: "" }
    ]);
    expect(run.streamedToolsByRound[1][0].live.status).toBe("running");
  });

  it("publishes cumulative usage with each usage update", () => {
    const { run, effects } = reduceAll(runFixture(), [
      { type: "usage_updated", round: 0, usage: { inputTokens: 10, outputTokens: 2 } }
    ]);
    expect(run.usageRevision).toBe(1);
    const usageEffects = effects.filter((effect) => effect.kind === "turn_usage");
    expect(usageEffects).toHaveLength(1);
  });

  it("keeps a direct child's usage under its bare call id", () => {
    // The accounting map's existing key shape is load-bearing for everything
    // that already reads it; unwrapping deeper envelopes must not rename it.
    const { run } = reduceAll(runFixture(), [
      {
        type: "tool_call_announced",
        round: 1,
        callId: "child",
        toolName: "agent_spawn",
        contextId: "ctx-child"
      },
      {
        type: "subagent_event",
        round: 1,
        callId: "child",
        event: { type: "usage_updated", round: 0, usage: { inputTokens: 9, outputTokens: 3 } }
      }
    ]);
    expect(run.subagentUsageByCall).toEqual({
      child: { 0: { inputTokens: 9, outputTokens: 3 } }
    });
  });

  /**
   * Child reasoning arrives as nested model-stream events in `subagent_event`, not
   * through the string-only `subagent_delta` channel. It must retain the same
   * duration and token accounting as main-conversation reasoning, even with no summary.
   */
  it("retains nested retry content until replacement events arrive", () => {
    const nested = (event: ModelStreamEvent): ModelStreamEvent => ({
      type: "subagent_event",
      round: 1,
      callId: "child",
      event
    });
    let run = reduceAll(runFixture(), [
      {
        type: "tool_call_announced",
        round: 1,
        callId: "child",
        toolName: "agent_spawn",
        contextId: "ctx-child"
      },
      nested({ type: "text_delta", round: 0, delta: "old" }),
      nested({ type: "reasoning_delta", round: 0, item: 1, delta: "thought" }),
      nested({ type: "usage_updated", round: 0, usage: { outputTokens: 8 } }),
      nested({
        type: "stream_retry_scheduled",
        round: 0,
        attempt: 1,
        maxAttempts: 2,
        delayMs: 100,
        message: "retry"
      })
    ]).run;
    const live = () => run.streamedToolsByRound[1][0].live;
    expect(live().contexts.map((row) => ("content" in row ? row.content : undefined))).toEqual([
      "old",
      "thought"
    ]);
    expect(live().usageByRound?.[0]).toEqual({ outputTokens: 8 });
    run = reduceAll(run, [nested({ type: "text_delta", round: 1, delta: "other" })]).run;
    expect(live().contexts).toHaveLength(3);
    run = reduceAll(run, [
      nested({ type: "text_delta", round: 0, delta: "new" }),
      nested({ type: "text_delta", round: 0, delta: " tail" })
    ]).run;
    expect(live().contexts.map((row) => ("content" in row ? row.content : undefined))).toEqual([
      "other",
      "new tail"
    ]);
    expect(live().usageByRound?.[0]).toBeUndefined();
  });

  it("preserves nested reasoning item identities and counts duration only once", () => {
    const nested = (event: ModelStreamEvent): ModelStreamEvent => ({
      type: "subagent_event",
      round: 1,
      callId: "child",
      event
    });
    const { run } = reduceAll(runFixture(), [
      {
        type: "tool_call_announced",
        round: 1,
        callId: "child",
        toolName: "agent_spawn",
        contextId: "ctx-child"
      },
      nested({ type: "reasoning_delta", round: 0, delta: "甲" }),
      nested({ type: "reasoning_delta", round: 0, item: 1, delta: "乙" }),
      nested({ type: "reasoning_delta", round: 0, delta: "续" }),
      nested({ type: "reasoning_done", round: 0, item: 1, durationMs: 50 }),
      nested({ type: "usage_updated", round: 0, usage: { reasoningTokens: 9 } })
    ]);
    const rows = run.streamedToolsByRound[1][0].live.contexts.filter((row) => row.kind === "reasoning");
    expect(rows.map((row) => row.content)).toEqual(["甲续", "乙"]);
    expect(new Set(rows.map((row) => row.id)).size).toBe(2);
    expect(rows.map((row) => row.streaming)).toEqual([true, false]);
    expect(rows.map((row) => row.durationMs)).toEqual([50, undefined]);
    expect(rows.map((row) => row.tokens)).toEqual([9, undefined]);
  });

  it("gives a child's summary-less reasoning its own card, duration and tokens", () => {
    const nested = (event: ModelStreamEvent): ModelStreamEvent => ({
      type: "subagent_event",
      round: 1,
      callId: "child",
      event
    });
    const { run } = reduceAll(runFixture(), [
      {
        type: "tool_call_announced",
        round: 1,
        callId: "child",
        toolName: "agent_spawn",
        contextId: "ctx-child"
      },
      nested({ type: "reasoning_start", round: 0 }),
      nested({ type: "reasoning_done", round: 0, durationMs: 7_500 }),
      nested({ type: "usage_updated", round: 0, usage: { outputTokens: 900, reasoningTokens: 640 } })
    ]);

    const call = run.streamedToolsByRound[1]?.find((tool) => tool.callId === "child");
    const reasoning = call?.live?.contexts.find((context) => context.kind === "reasoning");
    expect(reasoning).toBeDefined();
    if (reasoning?.kind !== "reasoning") throw new Error("应当是思考上下文");
    expect(reasoning.content).toBe("");
    expect(reasoning.streaming).toBe(false);
    expect(reasoning.durationMs).toBe(7_500);
    expect(reasoning.tokens).toBe(640);
  });

  it("accounts for a workflow step's usage through both of its envelopes", () => {
    // A step's own model events reach the renderer wrapped twice — the run's
    // envelope around the step's. Reading only the immediate payload dropped
    // every workflow's tokens, so the turn's total silently omitted them.
    const step = (callId: string, contextId: string): ModelStreamEvent[] => [
      {
        type: "subagent_event",
        round: 1,
        callId: "wf",
        event: { type: "tool_call_announced", round: 0, callId, toolName: "workflow_step", contextId }
      }
    ];
    const usage = (callId: string, tokens: number): ModelStreamEvent => ({
      type: "subagent_event",
      round: 1,
      callId: "wf",
      event: {
        type: "subagent_event",
        round: 0,
        callId,
        event: { type: "usage_updated", round: 0, usage: { inputTokens: tokens, outputTokens: 1 } }
      }
    });
    const { run, effects } = reduceAll(runFixture(), [
      { type: "tool_call_announced", round: 1, callId: "wf", toolName: "workflow", contextId: "ctx-wf" },
      ...step("ws1", "ctx-ws1"),
      ...step("ws2", "ctx-ws2"),
      usage("ws1", 100),
      usage("ws2", 40)
    ]);
    // Two steps of one run are distinct spenders: keyed by the whole call path,
    // they accumulate instead of overwriting each other under "wf".
    expect(run.subagentUsageByCall).toEqual({
      "wf/ws1": { 0: { inputTokens: 100, outputTokens: 1 } },
      "wf/ws2": { 0: { inputTokens: 40, outputTokens: 1 } }
    });
    expect(cumulativeModelRunUsage(run)).toEqual({ inputTokens: 140, outputTokens: 2 });
    expect(effects.filter((effect) => effect.kind === "turn_usage")).toHaveLength(2);

    // The leaf snapshot also lands on the step's own live state, which is what
    // the task row reads before the run settles and writes a record.
    const workflowLive = run.streamedToolsByRound[1][0].live;
    const stepLive = workflowLive.contexts
      .filter((context): context is Extract<ContextItem, { kind: "tool" }> => context.kind === "tool")
      .map((context) => context.live);
    expect(stepLive.map((live) => live?.usageByRound)).toEqual([
      { 0: { inputTokens: 100, outputTokens: 1 } },
      { 0: { inputTokens: 40, outputTokens: 1 } }
    ]);
  });

  it("replaces a child's per-round snapshot instead of accumulating it", () => {
    // Provider snapshots are already cumulative for their round. Adding them
    // would multiply a child's cost by however many chunks its provider chose
    // to report usage in.
    const snapshot = (outputTokens: number): ModelStreamEvent => ({
      type: "subagent_event",
      round: 1,
      callId: "child",
      event: { type: "usage_updated", round: 0, usage: { inputTokens: 50, outputTokens } }
    });
    const { run } = reduceAll(runFixture(), [
      {
        type: "tool_call_announced",
        round: 1,
        callId: "child",
        toolName: "agent_spawn",
        contextId: "ctx-child"
      },
      snapshot(2),
      snapshot(9)
    ]);
    expect(run.subagentUsageByCall.child).toEqual({ 0: { inputTokens: 50, outputTokens: 9 } });
    expect(run.streamedToolsByRound[1][0].live.usageByRound).toEqual({
      0: { inputTokens: 50, outputTokens: 9 }
    });
  });

  it("splits the turn exactly once for a steered input, deduplicating replays", () => {
    const input = {
      type: "user_input_received" as const,
      round: 0,
      id: "queued_1",
      content: "插话",
      createdAt: AT
    };
    const first = reduceAll(runFixture(), [input]);
    expect(first.effects.filter((effect) => effect.kind === "turn_split")).toHaveLength(1);
    expect(first.effects.filter((effect) => effect.kind === "steering_delivered")).toHaveLength(1);
    expect(first.run.steeredInputsByRound[0]).toHaveLength(1);

    const replay = reduceAll(first.run, [input]);
    expect(replay.effects.filter((effect) => effect.kind === "turn_split")).toHaveLength(0);
    expect(replay.run.steeredInputsByRound[0]).toHaveLength(1);
  });

  it("reports tool completion with the announced tool name", () => {
    const { effects } = reduceAll(runFixture(), [
      {
        type: "tool_call_announced",
        round: 0,
        callId: "call_1",
        toolName: "playwright",
        contextId: "ctx-call_1"
      },
      {
        type: "tool_execution_completed",
        round: 0,
        callId: "call_1",
        result: { success: true, output: "[]", executedAt: AT, durationMs: 1 }
      }
    ]);
    expect(effects).toContainEqual({
      kind: "tool_completed",
      toolName: "playwright",
      success: true,
      output: "[]"
    });
  });

  it("marks reasoning done only when the round actually streamed reasoning", () => {
    const untouched = reduceAll(runFixture(), [{ type: "reasoning_done", round: 0 }]);
    expect(untouched.run.completedReasoningByRound[0]).toBeUndefined();

    const streamed = reduceAll(runFixture({ streamedReasoningByRound: { 0: ["想法"] } }), [
      { type: "reasoning_done", round: 0 }
    ]);
    expect(streamed.run.completedReasoningByRound[0]).toBe(true);
  });

  /**
   * `reasoning_start` is the only signal for encrypted reasoning with no
   * summaries, so whether a round reasoned cannot depend on text content.
   */
  it("opens a reasoning round from reasoning_start alone and closes it with the provider's duration", () => {
    const { run } = reduceAll(runFixture(), [
      { type: "reasoning_start", round: 1 },
      { type: "reasoning_done", round: 1, durationMs: 18_000 }
    ]);
    expect(run.reasoningStartedAtByRound[1]).toBe(AT);
    // Item 0 must retain an empty slot for encrypted-only reasoning.
    expect(run.streamedReasoningByRound[1]).toEqual([""]);
    expect(run.completedReasoningByRound[1]).toBe(true);
    expect(run.reasoningDurationByRound[1]).toBe(18_000);
  });

  /**
   * Upstream sends a start for each summary segment. Preserve the first start
   * time so duration covers the entire reasoning operation.
   */
  it("keeps the first reasoning_start of a round and lets a delta open one on providers that never send it", () => {
    const repeated = reduceAll(runFixture(), [
      { type: "reasoning_start", round: 1 },
      { type: "reasoning_start", round: 1 }
    ]);
    expect(repeated.run.reasoningStartedAtByRound[1]).toBe(AT);

    const deltaOnly = reduceAll(runFixture(), [{ type: "reasoning_delta", round: 1, delta: "想法" }]);
    expect(deltaOnly.run.reasoningStartedAtByRound[1]).toBe(AT);
  });

  /**
   * Retry resets must remove the failed attempt's reasoning timing data.
   */
  it("drops the failed attempt's reasoning clock when the round is reset for a retry", () => {
    const reset = resetRoundForRetry(
      runFixture({
        reasoningStartedAtByRound: { 1: "2026-08-04T00:00:00.000Z", 2: "keep" },
        reasoningDurationByRound: { 1: 9_000, 2: 1 }
      }),
      1
    );
    expect(reset.reasoningStartedAtByRound).toEqual({ 2: "keep" });
    expect(reset.reasoningDurationByRound).toEqual({ 2: 1 });
  });

  /**
   * A settled reasoning card states how long the round thought for, and states it
   * from `durationMs` alone: `startedAt` is renderer-only and never reaches disk,
   * so a card still counting from a start timestamp would report the time since
   * the transcript was reopened. Every round that closes has to leave a figure.
   */
  it("measures the round's own reasoning duration when the provider reports none", () => {
    const opened = reduceModelStreamEvent(
      runFixture(),
      { type: "reasoning_start", round: 1 },
      "2026-08-04T00:00:01.000Z"
    ).run;
    const closed = reduceModelStreamEvent(
      opened,
      { type: "reasoning_done", round: 1 },
      "2026-08-04T00:00:14.000Z"
    ).run;

    expect(closed.reasoningDurationByRound[1]).toBe(13_000);
  });

  it("measures it from the first text delta on providers that send no done frame", () => {
    const opened = reduceModelStreamEvent(
      runFixture(),
      { type: "reasoning_start", round: 1 },
      "2026-08-04T00:00:01.000Z"
    ).run;
    const answered = reduceModelStreamEvent(
      opened,
      { type: "text_delta", round: 1, delta: "答" },
      "2026-08-04T00:00:04.500Z"
    ).run;

    expect(answered.completedReasoningByRound[1]).toBe(true);
    expect(answered.reasoningDurationByRound[1]).toBe(3_500);
  });

  /** The sidecar times the provider rather than the transport, so its figure
   * wins — and reopening the round must not restart the clock behind it. */
  it("keeps the sidecar's duration when reasoning reopens and closes again", () => {
    const opened = reduceModelStreamEvent(
      runFixture(),
      { type: "reasoning_start", round: 1 },
      "2026-08-04T00:00:01.000Z"
    ).run;
    const closed = reduceModelStreamEvent(
      opened,
      { type: "reasoning_done", round: 1, durationMs: 18_000 },
      "2026-08-04T00:00:02.000Z"
    ).run;
    const reopened = reduceModelStreamEvent(
      closed,
      { type: "reasoning_delta", round: 1, delta: "再想想" },
      "2026-08-04T00:00:03.000Z"
    ).run;
    const reclosed = reduceModelStreamEvent(
      reopened,
      { type: "reasoning_done", round: 1 },
      "2026-08-04T00:01:00.000Z"
    ).run;

    expect(reclosed.reasoningDurationByRound[1]).toBe(18_000);
  });
});

describe("settleSubagentContexts", () => {
  /**
   * A child round that ended without finishing its reasoning leaves an encrypted
   * card with no body, no duration, and no ciphertext behind it — nothing to
   * show and nothing a later turn could replay. Plaintext reasoning is its own
   * content, so even a fragment of it is a truthful record.
   */
  it("drops a child's unfinished encrypted reasoning and keeps plaintext fragments", () => {
    const settled = settleSubagentContexts([
      { id: "reasoning-empty", kind: "reasoning", content: "", streaming: true, createdAt: AT },
      { id: "reasoning-text", kind: "reasoning", content: "想到一半", streaming: true, createdAt: AT },
      { id: "reasoning-done", kind: "reasoning", content: "", durationMs: 9_000, createdAt: AT }
    ]);

    expect(settled.map((context) => context.id)).toEqual(["reasoning-text", "reasoning-done"]);
    expect(settled[0]).not.toHaveProperty("streaming");
  });
});

describe("trimLiveContextBudget", () => {
  const activity = (id: string): ContextItem => ({
    id,
    kind: "tool",
    toolName: "subagent_activity",
    input: {},
    result: { success: true, output: "", executedAt: AT, durationMs: 0 },
    createdAt: AT
  });

  /** A live child holding `count` rows, optionally with one nested child. */
  const child = (prefix: string, count: number, nested?: SubagentLiveState): SubagentLiveState => ({
    contexts: [
      ...Array.from({ length: count }, (_, index) => activity(`${prefix}-${index}`)),
      ...(nested
        ? [
            {
              id: `${prefix}-nested-call`,
              kind: "tool" as const,
              toolName: "agent_spawn",
              input: {},
              result: { success: true, output: "", executedAt: AT, durationMs: 0 },
              live: nested,
              createdAt: AT
            }
          ]
        : [])
    ],
    updates: []
  });

  const runWith = (children: SubagentLiveState[]): ModelRunState =>
    runFixture({
      streamedToolsByRound: {
        1: children.map((live, index) => ({
          id: `tool-${index}`,
          callId: `call-${index}`,
          toolName: "agent_spawn",
          input: {},
          result: { success: true, output: "", executedAt: AT, durationMs: 0 },
          streamStatus: "running" as const,
          live,
          createdAt: AT
        }))
      }
    });

  const rowCount = (live: SubagentLiveState): number =>
    live.contexts.reduce(
      (total, context) =>
        total + 1 + (context.kind === "tool" && context.live ? rowCount(context.live) - 1 : 0),
      0
    );

  it("leaves a run inside the budget untouched", () => {
    const run = runWith([child("a", 10), child("b", 10)]);
    expect(trimLiveContextBudget(run)).toBe(run);
  });

  it("evicts the deepest child first, then the oldest at that depth", () => {
    // Three depth-0 children plus one nested child. Over budget, the nested
    // (deepest) transcript must give up its rows before any depth-0 sibling,
    // and among the depth-0 siblings the earliest must go before the latest.
    const deep = child("deep", 600);
    const run = runWith([child("first", 600, deep), child("second", 600)]);
    const trimmed = trimLiveContextBudget(run);

    const [firstTool, secondTool] = trimmed.streamedToolsByRound[1];
    const deepAfter = firstTool.live.contexts.find((context) => context.kind === "tool" && context.live);
    const deepRows = deepAfter?.kind === "tool" ? (deepAfter.live?.contexts.length ?? 0) : 0;

    // The deepest transcript is stripped to its single surviving row.
    expect(deepRows).toBe(1);
    // The oldest depth-0 child gave up rows; the newest kept all of its own.
    expect(firstTool.live.contexts.length).toBeLessThan(601);
    expect(secondTool.live.contexts).toHaveLength(600);
  });

  it("never drops a child entirely, so an evicted agent still shows its last row", () => {
    const run = runWith(Array.from({ length: 40 }, (_, index) => child(`c${index}`, 100)));
    const trimmed = trimLiveContextBudget(run);
    trimmed.streamedToolsByRound[1].forEach((tool) => {
      expect(tool.live.contexts.length).toBeGreaterThanOrEqual(1);
    });
  });

  it("brings the whole tree back under the global budget", () => {
    const run = runWith([child("a", 500, child("a-deep", 500)), child("b", 500), child("c", 500)]);
    const trimmed = trimLiveContextBudget(run);
    const total = trimmed.streamedToolsByRound[1].reduce((sum, tool) => sum + rowCount(tool.live), 0);
    expect(total).toBeLessThanOrEqual(SUBAGENT_LIVE_TOTAL_CONTEXT_BUDGET);
  });
});

describe("createModelStreamCoalescer", () => {
  function manualScheduler() {
    let scheduled: (() => void) | null = null;
    const cancel = vi.fn(() => {
      scheduled = null;
    });
    return {
      schedule: (flush: () => void) => {
        scheduled = flush;
        return cancel;
      },
      fire: () => {
        scheduled?.();
        scheduled = null;
      },
      cancel,
      get pending() {
        return scheduled !== null;
      }
    };
  }

  const delta = (text: string): ModelStreamEvent => ({ type: "text_delta", round: 0, delta: text });

  it("coalesces deltas into one commit per scheduled flush", () => {
    const commits: ModelStreamEvent[][] = [];
    const scheduler = manualScheduler();
    const coalescer = createModelStreamCoalescer(
      (batch) => commits.push(batch.map((entry) => entry.event)),
      scheduler.schedule
    );
    coalescer.push(delta("a"));
    coalescer.push(delta("b"));
    coalescer.push(delta("c"));
    expect(commits).toHaveLength(0);
    scheduler.fire();
    expect(commits).toEqual([[delta("a"), delta("b"), delta("c")]]);
  });

  it("keeps lifecycle events in arrival order inside the scheduled batch", () => {
    // Every event shares one buffer. A lifecycle event no longer forces its own
    // immediate commit — that made the commit rate track arrivals, which is the
    // thing the fixed cadence exists to stop — but it must still land in order,
    // because the reducer's tool and retry semantics depend on that order.
    const commits: ModelStreamEvent[][] = [];
    const scheduler = manualScheduler();
    const coalescer = createModelStreamCoalescer(
      (batch) => commits.push(batch.map((entry) => entry.event)),
      scheduler.schedule
    );
    const done: ModelStreamEvent = { type: "reasoning_done", round: 0 };
    coalescer.push(delta("a"));
    coalescer.push(done);
    coalescer.push(delta("b"));
    expect(commits).toHaveLength(0);
    scheduler.fire();
    expect(commits).toEqual([[delta("a"), done, delta("b")]]);
  });

  it("commits once per scheduled tick no matter how many events arrive", () => {
    const commits: ModelStreamEvent[][] = [];
    const scheduler = manualScheduler();
    const coalescer = createModelStreamCoalescer(
      (batch) => commits.push(batch.map((entry) => entry.event)),
      scheduler.schedule
    );
    // Two children streaming at once: interleaved deltas plus their lifecycle
    // traffic still buy exactly one commit per tick.
    for (let index = 0; index < 20; index += 1) {
      coalescer.push({ type: "subagent_delta", round: 0, callId: "a", channel: "text", delta: "x" });
      coalescer.push({ type: "subagent_delta", round: 0, callId: "b", channel: "text", delta: "y" });
      coalescer.push({ type: "reasoning_done", round: 0 });
    }
    expect(commits).toHaveLength(0);
    scheduler.fire();
    expect(commits).toHaveLength(1);
    expect(commits[0]).toHaveLength(60);
  });

  it("flush commits pending deltas immediately; dispose drops them", () => {
    const commits: ModelStreamEvent[][] = [];
    const scheduler = manualScheduler();
    const coalescer = createModelStreamCoalescer(
      (batch) => commits.push(batch.map((entry) => entry.event)),
      scheduler.schedule
    );
    coalescer.push(delta("a"));
    coalescer.flush();
    expect(commits).toEqual([[delta("a")]]);

    coalescer.push(delta("b"));
    coalescer.dispose();
    scheduler.fire();
    expect(commits).toHaveLength(1);
  });

  it("stamps each event with its arrival time, not the flush time", () => {
    const scheduler = manualScheduler();
    let batchAt: string[] = [];
    const coalescer = createModelStreamCoalescer((batch) => {
      batchAt = batch.map((entry) => entry.at);
    }, scheduler.schedule);
    coalescer.push(delta("a"));
    scheduler.fire();
    expect(batchAt).toHaveLength(1);
    expect(Number.isNaN(Date.parse(batchAt[0]))).toBe(false);
  });

  it("publishes at 10 FPS on its real default scheduler", () => {
    // The default scheduler is what production uses; a manual scheduler proves
    // batching but not the cadence the user asked for.
    vi.useFakeTimers();
    try {
      const commits: ModelStreamEvent[][] = [];
      const coalescer = createModelStreamCoalescer((batch) =>
        commits.push(batch.map((entry) => entry.event))
      );
      coalescer.push(delta("a"));
      vi.advanceTimersByTime(MODEL_STREAM_COMMIT_INTERVAL_MS - 1);
      expect(commits).toHaveLength(0);
      vi.advanceTimersByTime(1);
      expect(commits).toEqual([[delta("a")]]);

      // One second of continuous streaming buys ten commits, not sixty.
      for (let tick = 0; tick < 10; tick += 1) {
        coalescer.push(delta("x"));
        vi.advanceTimersByTime(MODEL_STREAM_COMMIT_INTERVAL_MS);
      }
      expect(commits).toHaveLength(11);
      coalescer.dispose();
    } finally {
      vi.useRealTimers();
    }
  });
});
