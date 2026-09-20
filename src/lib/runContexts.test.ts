import { describe, expect, it } from "vitest";
import type { Conversation, ContextItem, JsonObject, ModelRunRequest, ToolContext } from "../types";
import { reduceModelStreamEvent, resetRoundForRetry } from "./modelStream";
import type { ModelRunState, StreamingToolState } from "./modelStream";
import {
  applyStreamedRunContexts,
  backfillRequestedToolInput,
  contextsFromInterruptedRun,
  contextsFromModelRun,
  liveReasoningFromModelRun,
  mergeFinalizedInterruptedRunContexts,
  reasoningSegmentContextId,
  roundModelTurnId,
  roundProseContextId,
  runStreamContextPrefixes
} from "./runContexts";

const AT = "2026-08-04T00:00:01.000Z";

function cancelledRun(): ModelRunState {
  return {
    requestId: "run_cancelled",
    providerName: "provider",
    modelName: "model",
    workspaceId: "ws",
    // The projection reads the actual request envelope: a reasoning card's form
    // is `model.reasoningContent` verbatim, so the fixture must carry a concrete
    // one to exercise live-card classification.
    request: {
      contexts: [],
      provider: { family: "openai_responses" },
      model: { capabilities: [], reasoningContent: "encrypted" }
    } as unknown as ModelRunRequest,
    startedAt: "2026-08-14T00:00:00.000Z",
    streamedTextByRound: { 2: "取消前可见的正文" },
    streamedReasoningByRound: {},
    completedReasoningByRound: {},
    reasoningStartedAtByRound: {},
    reasoningDurationByRound: {},
    streamedToolsByRound: {
      1: [{
        id: "ctx-workflow",
        callId: "call-workflow",
        toolName: "workflow",
        input: { plan: { name: "audit" } },
        result: {
          success: true,
          output: "workflow started",
          executedAt: "2026-08-14T00:00:01.000Z",
          durationMs: 1
        },
        streamStatus: "completed",
        live: {
          contexts: [],
          updates: [{ content: "步骤正在执行", createdAt: "2026-08-14T00:00:02.000Z" }],
          status: "running"
        },
        createdAt: "2026-08-14T00:00:01.000Z"
      }]
    },
    streamedHooksByRound: {},
    steeredInputsByRound: {},
    usageByRound: {},
    subagentUsageByCall: {},
    workflowProgressByCall: {},
    workflowRunIdByCall: {},
    usageRevision: 0
  };
}

describe("redacted reasoning item facts", () => {
  it("keeps encrypted and readable items distinct through events and projection", () => {
    let run = cancelledRun();
    run.request.provider.family = "anthropic";
    run = reduceModelStreamEvent(run, { type: "reasoning_start", round: 1, item: 0, form: "encrypted" }, AT).run;
    run = reduceModelStreamEvent(run, { type: "reasoning_start", round: 1, item: 1, form: "plaintext" }, AT).run;
    run = reduceModelStreamEvent(run, { type: "reasoning_delta", round: 1, item: 1, delta: "readable" }, AT).run;
    run = reduceModelStreamEvent(run, { type: "reasoning_done", round: 1, item: 1, durationMs: 1 }, AT).run;
    for (const streaming of [true, false]) {
      const cards = contextsFromModelRun(run, streaming).filter((card) => card.kind === "reasoning");
      expect(cards).toMatchObject([
        { id: reasoningSegmentContextId(run.requestId, 1, 0), content: "", form: "encrypted" },
        { id: reasoningSegmentContextId(run.requestId, 1, 1), content: "readable", form: "plaintext" }
      ]);
    }
    expect(resetRoundForRetry(run, 1).reasoningFormsByRound?.[1]).toBeUndefined();
  });
});

describe("mergeFinalizedInterruptedRunContexts", () => {
  it("replaces a completed hook before any refresh while retaining its distinct injection", () => {
    const run = cancelledRun();
    run.streamedToolsByRound = {};
    run.streamedTextByRound = {};
    run.streamedHooksByRound = { 3: [{
      executionId: "exec_1", hookId: "hook_1", hookName: "Hook", event: "PreToolUse",
      status: "succeeded", contextInjected: true,
      result: { success: true, output: "streamed display", durationMs: 1, executedAt: AT }, createdAt: AT
    }] };
    const display: ContextItem = { id: "ctx_hook_run_cancelled_exec_1_display_0", kind: "system",
      content: "host terminal display", localOnly: true, createdAt: AT };
    const injection: ContextItem = { id: "ctx_hook_run_cancelled_exec_1_injection_0", kind: "system",
      content: "injected context", localOnly: false, createdAt: AT };
    const merged = mergeFinalizedInterruptedRunContexts(run, [display, injection]);
    expect(merged).toEqual([display, injection]);
    run.streamedHooksByRound[3][0].status = "running";
    expect(mergeFinalizedInterruptedRunContexts(run, [])).toHaveLength(1);
  });

  it("keeps partial text and replaces a live workflow with its trusted terminal record", () => {
    const finalized: ToolContext = {
      id: "ctx-workflow",
      kind: "tool",
      toolName: "workflow",
      round: 1,
      modelTurnId: "model-turn-run_cancelled-1",
      input: { plan: { name: "audit" } },
      result: {
        success: true,
        output: "workflow started",
        executedAt: "2026-08-14T00:00:01.000Z",
        durationMs: 1
      },
      subagent: {
        name: "workflow-a1",
        kind: "workflowStep",
        task: "audit",
        status: "interrupted",
        contexts: [],
        updates: [{ content: "步骤正在执行", createdAt: "2026-08-14T00:00:02.000Z" }]
      },
      attestation: "host-attestation",
      createdAt: "2026-08-14T00:00:01.000Z"
    };

    const merged = mergeFinalizedInterruptedRunContexts(cancelledRun(), [finalized]);
    const workflow = merged.find((context): context is ToolContext => context.id === "ctx-workflow");
    const partial = merged.find((context) => context.kind === "assistant" && context.content);

    expect(merged.filter((context) => context.id === "ctx-workflow")).toHaveLength(1);
    expect(workflow?.subagent?.status).toBe("interrupted");
    expect(workflow?.attestation).toBe("host-attestation");
    expect(workflow?.live).toBeUndefined();
    expect(partial).toMatchObject({ content: "取消前可见的正文", interrupted: true });
  });
});

function proseRun(requestId: string): ModelRunState {
  return {
    ...cancelledRun(),
    requestId,
    streamedToolsByRound: {},
    streamedReasoningByRound: { 1: ["先读代码"] },
    completedReasoningByRound: { 1: true },
    streamedTextByRound: { 1: "读完了" }
  };
}

describe("回合正文的身份两侧同源派生", () => {
  // These literals are a cross-language contract: the host's
  // `round_prose_context_id` and `round_model_turn_id` in src-tauri/src/api.rs
  // must derive exactly the same strings.
  it("按运行与轮次派生，与宿主的规则逐字一致", () => {
    expect(roundProseContextId("run_1", 1, "assistant")).toBe("ctx_assistant_run_1_1");
    expect(roundProseContextId("run_1", 2, "reasoning")).toBe("ctx_reasoning_run_1_2");
    expect(roundModelTurnId("run_1", 3)).toBe("model-turn-run_1-3");
    expect(runStreamContextPrefixes("run_1")).toEqual([
      "ctx_assistant_run_1_",
      "ctx_reasoning_run_1_",
      "ctx_hook_run_1_"
    ]);
  });

  it("多段思考逐段成行，身份按段号派生", () => {
    // Each item is one card. Round metadata belongs only to the first card, and
    // the live pointer targets only the final growing segment.
    const run: ModelRunState = {
      ...proseRun("run_segments"),
      streamedTextByRound: {},
      streamedReasoningByRound: { 1: ["第一段", "第二段"] },
      completedReasoningByRound: {},
      reasoningStartedAtByRound: { 1: AT },
      reasoningDurationByRound: { 1: 5_000 }
    };
    const reasoning = contextsFromModelRun(run, true)
      .filter((context) => context.kind === "reasoning");

    expect(reasoning).toHaveLength(2);
    expect(reasoning[0]).toMatchObject({
      id: reasoningSegmentContextId(run.requestId, 1, 0),
      content: "第一段",
      startedAt: AT,
      durationMs: 5_000,
      streaming: false
    });
    expect(reasoning[1]).toMatchObject({
      id: reasoningSegmentContextId(run.requestId, 1, 1),
      content: "第二段",
      streaming: true
    });
    if (reasoning[1].kind !== "reasoning") throw new Error("应当是思考上下文");
    expect(reasoning[1].startedAt).toBeUndefined();
    expect(reasoning[1].durationMs).toBeUndefined();

    expect(reasoningSegmentContextId("req", 2, 0)).toBe(roundProseContextId("req", 2, "reasoning"));
    expect(reasoningSegmentContextId("req", 2, 3)).toBe("ctx_reasoning_req_2_3");
  });

  it("流式投影用的就是派生身份，不再自铸", () => {
    const projected = contextsFromModelRun(proseRun("run_a"), true);
    const reasoning = projected.find((context) => context.kind === "reasoning");
    const assistant = projected.find((context) => context.kind === "assistant");

    expect(reasoning?.id).toBe(roundProseContextId("run_a", 1, "reasoning"));
    expect(assistant?.id).toBe(roundProseContextId("run_a", 1, "assistant"));
    expect(reasoning?.modelTurnId).toBe(roundModelTurnId("run_a", 1));
    expect(assistant?.modelTurnId).toBe(roundModelTurnId("run_a", 1));
    expect(projected.some((context) => context.id.startsWith("stream-assistant-"))).toBe(false);
    expect(projected.some((context) => context.id.startsWith("stream-reasoning-"))).toBe(false);
  });

  /** Encrypted reasoning requests (`include: reasoning.encrypted_content`) may
   * provide no summary, leaving `streamedReasoningByRound` empty. The round must
   * still appear in the timeline so its duration and token count remain visible.
   */
  it("投影出没有摘要正文的思考轮，并带上耗时与思考 token", () => {
    const run: ModelRunState = {
      ...cancelledRun(),
      requestId: "run_hidden",
      streamedTextByRound: {},
      streamedToolsByRound: {},
      streamedReasoningByRound: {},
      completedReasoningByRound: { 1: true },
      reasoningStartedAtByRound: { 1: "2026-08-29T00:00:00.000Z" },
      reasoningDurationByRound: { 1: 18_000 },
      usageByRound: { 1: { outputTokens: 2_000, reasoningTokens: 1_240 } }
    };

    const projected = contextsFromModelRun(run, true);
    expect(projected).toHaveLength(1);
    const reasoning = projected[0];
    expect(reasoning.kind).toBe("reasoning");
    if (reasoning.kind !== "reasoning") throw new Error("应当是思考上下文");
    expect(reasoning.id).toBe(roundProseContextId("run_hidden", 1, "reasoning"));
    expect(reasoning.content).toBe("");
    expect(reasoning.durationMs).toBe(18_000);
    expect(reasoning.tokens).toBe(1_240);
    expect(reasoning.startedAt).toBe("2026-08-29T00:00:00.000Z");
    // The fixture's model declares encrypted reasoning, so the live card must
    // use the same form as the host's settled card.
    expect(reasoning.form).toBe("encrypted");
  });

  /** With identical state and empty prose, changing only the model setting must
   * change the live card form. Form derives from model attributes, not inferred
   * from whether prose is empty. */
  it("按模型属性给直播思考卡定形，而不是按正文空不空", () => {
    const base = {
      ...cancelledRun(),
      requestId: "run_plaintext",
      streamedTextByRound: {},
      streamedToolsByRound: {},
      streamedReasoningByRound: {},
      completedReasoningByRound: { 1: true },
      reasoningStartedAtByRound: { 1: "2026-08-29T00:00:00.000Z" },
      reasoningDurationByRound: { 1: 18_000 }
    };
    const run: ModelRunState = {
      ...base,
      request: {
        ...base.request,
        provider: { family: "openai_responses" },
        model: { capabilities: [], reasoningContent: "plaintext" }
      } as unknown as ModelRunRequest
    };

    const reasoning = contextsFromModelRun(run, true)[0];
    if (reasoning.kind !== "reasoning") throw new Error("应当是思考上下文");
    expect(reasoning.content).toBe("");
    expect(reasoning.form).toBe("plaintext");
  });

  /** A zero reasoning-token count means no reasoning. Responses reports `0`
   * rather than omitting the field; storing it would display a meaningless
   * `· 0`. */
  it("不把 provider 报的零思考 token 写进上下文", () => {
    const run: ModelRunState = {
      ...proseRun("run_zero"),
      usageByRound: { 1: { outputTokens: 12, reasoningTokens: 0 } }
    };
    const reasoning = contextsFromModelRun(run, true).find((context) => context.kind === "reasoning");
    expect(reasoning && "tokens" in reasoning ? reasoning.tokens : undefined).toBeUndefined();
  });

  /** Some reasoning models report reasoning tokens only in usage, with no
   * reasoning events. The live projection must create the metadata-only card so
   * it does not appear suddenly before the answer at settlement. */
  it("token-only 的思考轮在流式投影里就有卡，且不标 streaming", () => {
    const run: ModelRunState = {
      ...cancelledRun(),
      requestId: "run_tokens",
      streamedTextByRound: { 1: "答案" },
      streamedToolsByRound: {},
      streamedReasoningByRound: {},
      completedReasoningByRound: {},
      reasoningStartedAtByRound: {},
      reasoningDurationByRound: {},
      usageByRound: { 1: { outputTokens: 40, reasoningTokens: 28 } }
    };

    const projected = contextsFromModelRun(run, true);
    const reasoning = projected.find((context) => context.kind === "reasoning");
    expect(reasoning).toBeDefined();
    if (!reasoning || reasoning.kind !== "reasoning") throw new Error("应当是思考上下文");
    expect(reasoning.id).toBe(roundProseContextId("run_tokens", 1, "reasoning"));
    expect(reasoning.content).toBe("");
    expect(reasoning.tokens).toBe(28);
    // With no start or delta, usage-only accounting is not active reasoning and
    // must not start the timer.
    expect(reasoning.streaming).toBeFalsy();
    // The reasoning card precedes prose from the same round.
    const assistantIndex = projected.findIndex((context) => context.kind === "assistant");
    expect(projected.indexOf(reasoning)).toBeLessThan(assistantIndex);
  });

  // Preserve host-issued IDs verbatim. Rewriting them would desynchronize the
  // renderer read model from the conversation store and cause host prose to win
  // the update concurrency guard.
  it("结算回执的身份原样保留，一个字都不改写", () => {
    const run = proseRun("run_b");
    const settled: ContextItem[] = [
      {
        id: roundProseContextId("run_b", 1, "reasoning"),
        kind: "reasoning",
        content: "先读代码",
        round: 1,
        modelTurnId: roundModelTurnId("run_b", 1),
        createdAt: "2026-08-25T00:00:00.000Z"
      },
      {
        id: "ctx_assistant_deadbeef",
        kind: "assistant",
        content: "读完了",
        round: 1,
        modelTurnId: "ctx_model-turn_deadbeef",
        createdAt: "2026-08-25T00:00:01.000Z"
      }
    ];

    expect(backfillRequestedToolInput(settled, run)).toEqual(settled);
  });

  it("同一 run 的正文行原位替换，不在时间线上留下第二份", () => {
    const conversation = {
      id: "conv",
      contexts: [
        { id: "ctx_user", kind: "user", content: "去读一下", createdAt: "2026-08-25T00:00:00.000Z" }
      ],
      queuedMessages: [],
      branches: []
    } as unknown as Conversation;

    const first = applyStreamedRunContexts(
      conversation,
      contextsFromModelRun(proseRun("run_c"), false),
      "run_c"
    );
    const grown = {
      ...proseRun("run_c"),
      streamedTextByRound: { 1: "读完了，接着写" }
    };
    const second = applyStreamedRunContexts(first, contextsFromModelRun(grown, false), "run_c");

    const assistants = second.contexts.filter((context) => context.kind === "assistant");
    expect(assistants).toHaveLength(1);
    expect(assistants[0]).toMatchObject({
      id: roundProseContextId("run_c", 1, "assistant"),
      content: "读完了，接着写"
    });
  });
});

describe("backfillRequestedToolInput", () => {
  function streamedTool(
    id: string,
    toolName: string,
    input: JsonObject,
    requestedInput?: JsonObject
  ): StreamingToolState {
    return {
      id,
      callId: `call-${id}`,
      toolName,
      ...(requestedInput ? { requestedInput } : {}),
      input,
      result: {
        success: true,
        output: "ok",
        executedAt: "2026-09-01T00:00:01.000Z",
        durationMs: 1
      },
      streamStatus: "completed",
      live: { contexts: [], updates: [] },
      createdAt: "2026-09-01T00:00:01.000Z"
    } as StreamingToolState;
  }

  function runWithTools(tools: StreamingToolState[]): ModelRunState {
    return {
      ...cancelledRun(),
      requestId: "run_backfill",
      streamedTextByRound: {},
      streamedToolsByRound: { 1: tools }
    };
  }

  function settledTool(
    id: string,
    toolName: string,
    input: JsonObject,
    requestedInput?: JsonObject
  ): ToolContext {
    return {
      id,
      kind: "tool",
      toolName,
      round: 1,
      modelTurnId: "model-turn-run_backfill-1",
      ...(requestedInput ? { requestedInput } : {}),
      input,
      result: {
        success: true,
        output: "ok",
        executedAt: "2026-09-01T00:00:01.000Z",
        durationMs: 1
      },
      attestation: `attestation-${id}`,
      createdAt: "2026-09-01T00:00:01.000Z"
    };
  }

  const requested = (context: ContextItem | undefined) =>
    context && context.kind === "tool" ? context.requestedInput : undefined;

  /** Host-synthesized `task_wait` cards were never announced and have no streamed
   * counterpart. Matching by tool name would assign them the model call's
   * original input, invalidating their `requested_input: None` receipt signature
   * and causing the card to be rejected as forged. */
  it("不给宿主合成的同名卡安上模型调用的原始参数", () => {
    const run = runWithTools([
      streamedTool("ctx_tool_model_wait", "task_wait", { label: "改写后" }, { label: "模型原始" })
    ]);
    const settled: ContextItem[] = [
      settledTool("ctx_agent-result_deadbeef", "task_wait", { label: "宿主代收" }),
      settledTool("ctx_tool_model_wait", "task_wait", { label: "改写后" })
    ];

    const [fold, modelCall] = backfillRequestedToolInput(settled, run);

    expect(requested(fold)).toBeUndefined();
    expect(requested(modelCall)).toEqual({ label: "模型原始" });
  });

  /** Concurrent asynchronous tools settle together at round end, so authoritative
   * order differs from announcement order. Matching cursors must not be shared
   * across tool names within a round. */
  it("权威顺序与宣告顺序不一致时仍配到本人", () => {
    const run = runWithTools([
      streamedTool("ctx_tool_search_a", "web_search", { query: "改写 A" }, { query: "原始 A" }),
      streamedTool("ctx_tool_read", "read", { path: "a.md" }),
      streamedTool("ctx_tool_search_c", "web_search", { query: "改写 C" }, { query: "原始 C" })
    ]);
    // Synchronous tools settle first; asynchronous tools settle after the
    // `while` loop, yielding the authoritative order read, A, C.
    const settled: ContextItem[] = [
      settledTool("ctx_tool_read", "read", { path: "a.md" }),
      settledTool("ctx_tool_search_a", "web_search", { query: "改写 A" }),
      settledTool("ctx_tool_search_c", "web_search", { query: "改写 C" })
    ];

    const [, searchA, searchC] = backfillRequestedToolInput(settled, run);

    expect(requested(searchA)).toEqual({ query: "原始 A" });
    expect(requested(searchC)).toEqual({ query: "原始 C" });
  });

  /** A card that already carries requested input must not shift matching of
   * later same-named cards. */
  it("已带参数的卡不挪动后面同名卡的配对", () => {
    const run = runWithTools([
      streamedTool("ctx_tool_write_1", "write", { path: "改写 1" }, { path: "原始 1" }),
      streamedTool("ctx_tool_write_2", "write", { path: "改写 2" }, { path: "原始 2" })
    ]);
    const settled: ContextItem[] = [
      settledTool("ctx_tool_write_1", "write", { path: "改写 1" }, { path: "原始 1" }),
      settledTool("ctx_tool_write_2", "write", { path: "改写 2" })
    ];

    const [first, second] = backfillRequestedToolInput(settled, run);

    expect(requested(first)).toEqual({ path: "原始 1" });
    expect(requested(second)).toEqual({ path: "原始 2" });
  });

  /** Requested input identical to the card's `input` was not hook-rewritten and
   * must not be stored. */
  it("参数没被改写过就不写 requestedInput", () => {
    const run = runWithTools([
      streamedTool("ctx_tool_read", "read", { path: "a.md" }, { path: "a.md" })
    ]);
    const settled = [settledTool("ctx_tool_read", "read", { path: "a.md" })];

    expect(backfillRequestedToolInput(settled, run)).toEqual(settled);
  });
});

/**
 * Encrypted reasoning has two properties plaintext reasoning does not: while it
 * runs there is nothing to read, and if the round ends before it finishes there
 * is no ciphertext behind it that a later turn could replay. The projection is
 * where both consequences are decided.
 */
describe("加密思考的直播与结算", () => {
  function encryptedRun(overrides: Partial<ModelRunState>): ModelRunState {
    return {
      ...cancelledRun(),
      requestId: "run_thinking",
      streamedTextByRound: {},
      streamedToolsByRound: {},
      streamedReasoningByRound: {},
      completedReasoningByRound: {},
      reasoningStartedAtByRound: {},
      reasoningDurationByRound: {},
      usageByRound: {},
      ...overrides
    };
  }

  /** The stream indicator narrates this round beside the cat; a card would be an
   * empty disclosure with a clock and nothing else in it. */
  it("思考进行中且没有摘要时，直播投影不出卡", () => {
    const run = encryptedRun({
      streamedReasoningByRound: { 1: [""] },
      completedReasoningByRound: { 1: false },
      reasoningStartedAtByRound: { 1: AT }
    });

    expect(contextsFromModelRun(run, true)).toEqual([]);
  });

  /** A Responses summary is readable text as it arrives, so it streams into its
   * own card exactly like plaintext reasoning does. */
  it("加密思考带摘要正文时，直播投影照常按增量出卡", () => {
    const run = encryptedRun({
      streamedReasoningByRound: { 1: ["先读文件"] },
      completedReasoningByRound: { 1: false },
      reasoningStartedAtByRound: { 1: AT }
    });

    const [reasoning, ...rest] = contextsFromModelRun(run, true);
    expect(rest).toEqual([]);
    if (reasoning.kind !== "reasoning") throw new Error("应当是思考上下文");
    expect(reasoning.form).toBe("encrypted");
    expect(reasoning.content).toBe("先读文件");
    expect(reasoning.streaming).toBe(true);
  });

  it("思考结束后，卡片带着耗时与思考 token 出现", () => {
    const run = encryptedRun({
      streamedReasoningByRound: { 1: [""] },
      completedReasoningByRound: { 1: true },
      reasoningStartedAtByRound: { 1: AT },
      reasoningDurationByRound: { 1: 83_000 },
      usageByRound: { 1: { outputTokens: 900, reasoningTokens: 1_240 } }
    });

    const [reasoning] = contextsFromModelRun(run, true);
    if (reasoning.kind !== "reasoning") throw new Error("应当是思考上下文");
    expect(reasoning.durationMs).toBe(83_000);
    expect(reasoning.tokens).toBe(1_240);
    expect(reasoning.streaming).toBe(false);
  });

  /** No summary and no `done` frame means nothing to show: neither text nor
   * ciphertext ever arrived. A summary that did stream is readable text the
   * user already saw, so it stays as an interrupted card, exactly like an
   * unfinished plaintext fragment. */
  it("中断时只丢没有摘要的加密思考，有摘要的按中断卡保留", () => {
    const bare = encryptedRun({
      streamedReasoningByRound: { 1: [""] },
      completedReasoningByRound: { 1: false },
      reasoningStartedAtByRound: { 1: AT }
    });
    const withSummary = encryptedRun({
      streamedReasoningByRound: { 1: ["先读文件"] },
      completedReasoningByRound: { 1: false },
      reasoningStartedAtByRound: { 1: AT }
    });

    expect(contextsFromInterruptedRun(bare)).toEqual([]);
    const [reasoning, ...rest] = contextsFromInterruptedRun(withSummary);
    expect(rest).toEqual([]);
    if (reasoning.kind !== "reasoning") throw new Error("应当是思考上下文");
    expect(reasoning.form).toBe("encrypted");
    expect(reasoning.content).toBe("先读文件");
    expect(reasoning.interrupted).toBe(true);
    expect(reasoning.durationMs).toBeUndefined();
  });

  it("中断时保留已经完成的加密思考", () => {
    const run = encryptedRun({
      streamedReasoningByRound: { 1: [""] },
      completedReasoningByRound: { 1: true },
      reasoningStartedAtByRound: { 1: AT },
      reasoningDurationByRound: { 1: 9_000 }
    });

    const [reasoning] = contextsFromInterruptedRun(run);
    if (reasoning.kind !== "reasoning") throw new Error("应当是思考上下文");
    expect(reasoning.durationMs).toBe(9_000);
  });

  /** Plaintext reasoning is its own content, so an unfinished fragment of it is
   * still a truthful record and stays. */
  it("明文思考没完成也照样保留", () => {
    const run: ModelRunState = {
      ...encryptedRun({
        streamedReasoningByRound: { 1: ["想到一半"] },
        completedReasoningByRound: { 1: false },
        reasoningStartedAtByRound: { 1: AT }
      }),
      request: {
        contexts: [],
        provider: { family: "openai_responses" },
        model: { capabilities: [], reasoningContent: "plaintext" }
      } as unknown as ModelRunRequest
    };

    const [reasoning] = contextsFromInterruptedRun(run);
    if (reasoning.kind !== "reasoning") throw new Error("应当是思考上下文");
    expect(reasoning.form).toBe("plaintext");
    expect(reasoning.content).toBe("想到一半");
    expect(reasoning.interrupted).toBe(true);
  });

  /** The backend mints its own interrupted reasoning during teardown, and those
   * contexts reach the merge without passing the projection's gate: the same
   * rule applies — a summary stays, a body-less encrypted fragment does not. */
  it("宿主回传的未完成加密思考：有摘要的保留，没摘要的丢弃", () => {
    const run = encryptedRun({
      streamedReasoningByRound: { 1: ["先读文件"] },
      completedReasoningByRound: { 1: false },
      reasoningStartedAtByRound: { 1: AT }
    });
    const replaced: ContextItem = {
      id: reasoningSegmentContextId("run_thinking", 1, 0),
      kind: "reasoning",
      content: "先读文件，再改",
      form: "encrypted",
      interrupted: true,
      createdAt: AT
    };
    const hostOnly: ContextItem = {
      id: "ctx_reasoning_host_only",
      kind: "reasoning",
      content: "宿主自己捡回来的片段",
      form: "encrypted",
      interrupted: true,
      createdAt: AT
    };
    const bodiless: ContextItem = {
      id: "ctx_reasoning_host_bodiless",
      kind: "reasoning",
      content: "",
      form: "encrypted",
      interrupted: true,
      createdAt: AT
    };

    expect(mergeFinalizedInterruptedRunContexts(run, [replaced, hostOnly, bodiless])).toEqual([
      { ...replaced, interrupted: true },
      hostOnly
    ]);
  });
});

describe("liveReasoningFromModelRun", () => {
  it("报出仍在思考的那一轮，并带上已知的思考 token", () => {
    const run: ModelRunState = {
      ...cancelledRun(),
      streamedTextByRound: {},
      streamedToolsByRound: {},
      reasoningStartedAtByRound: { 1: AT, 2: "2026-08-04T00:00:09.000Z" },
      completedReasoningByRound: { 1: true },
      usageByRound: { 2: { outputTokens: 10, reasoningTokens: 640 } }
    };

    // The newest open round wins: round 1 closed when its tools began.
    expect(liveReasoningFromModelRun(run)).toEqual({
      startedAt: "2026-08-04T00:00:09.000Z",
      tokens: 640
    });
  });

  it("思考结束或没有运行时不报", () => {
    const done: ModelRunState = {
      ...cancelledRun(),
      reasoningStartedAtByRound: { 1: AT },
      completedReasoningByRound: { 1: true }
    };

    expect(liveReasoningFromModelRun(done)).toBeNull();
    expect(liveReasoningFromModelRun(undefined)).toBeNull();
  });

  /** Providers report zero rather than omitting the field before the round ends;
   * a `0` beside the cat would read as a measurement rather than as silence. */
  it("零思考 token 不出现在指示器上", () => {
    const run: ModelRunState = {
      ...cancelledRun(),
      reasoningStartedAtByRound: { 1: AT },
      completedReasoningByRound: {},
      usageByRound: { 1: { outputTokens: 4, reasoningTokens: 0 } }
    };

    expect(liveReasoningFromModelRun(run)).toEqual({ startedAt: AT });
  });
});
