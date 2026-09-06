import { describe, expect, it } from "vitest";
import { estimateTokens, liveContextTokens } from "./contextTokens";
import type { ModelRunState } from "./modelStream";
import type { ModelRunRequest } from "../types";

function runFixture(overrides: Partial<ModelRunState> = {}): ModelRunState {
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
    startedAt: "2026-08-26T00:00:00.000Z",
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

/** 40 ASCII characters, which the shared estimator prices at exactly 10. */
const FORTY = "a".repeat(40);

describe("liveContextTokens", () => {
  it("leans on the caller's estimate alone until the first provider snapshot", () => {
    // The host writes streamed prose back as throttled `streaming` rows, so the
    // caller's estimate of the conversation already covers what has streamed and
    // already climbs. Adding the run's own buffers on top would bill the same
    // tokens twice.
    const usage = liveContextTokens(
      runFixture({ streamedTextByRound: { 0: FORTY } }),
      1_000
    );
    expect(usage).toEqual({ tokens: 1_000, estimated: true });
  });

  it("anchors on the round's reported input and adds what has streamed since", () => {
    // Anthropic reports input at `message_start`, before a single output token
    // exists. That is the exact context the provider read, so it is the anchor;
    // the round's own output is the part that is still growing.
    const usage = liveContextTokens(
      runFixture({
        usageByRound: { 2: { inputTokens: 5_000, cachedInputTokens: 4_000 } },
        streamedTextByRound: { 2: FORTY }
      }),
      1
    );
    expect(usage).toEqual({ tokens: 5_010, estimated: true });
  });

  it("stops estimating a round the provider has finished reporting", () => {
    // Once output tokens land, the round is accounted for in full and nothing
    // is left to estimate — so the number is authoritative, not `~`.
    const usage = liveContextTokens(
      runFixture({
        usageByRound: { 2: { inputTokens: 5_000, outputTokens: 300 } },
        streamedTextByRound: { 2: FORTY }
      }),
      1
    );
    expect(usage).toEqual({ tokens: 5_300, estimated: false });
  });

  it("counts a later round's stream on top of the last closed round", () => {
    const usage = liveContextTokens(
      runFixture({
        usageByRound: { 2: { inputTokens: 5_000, outputTokens: 300 } },
        streamedTextByRound: { 2: "已计入的旧内容", 3: FORTY }
      }),
      1
    );
    expect(usage).toEqual({ tokens: 5_310, estimated: true });
  });

  it("prices a round's tool calls the same way the settled context will", () => {
    const tool = {
      id: "t1",
      callId: "call-1",
      toolName: "read",
      input: { path: "a.ts" },
      result: { success: true, output: FORTY, images: [], executedAt: "", durationMs: 0 },
      streamStatus: "completed" as const,
      live: { contexts: [], updates: [] },
      createdAt: ""
    };
    const usage = liveContextTokens(
      runFixture({
        usageByRound: { 0: { inputTokens: 4_000 } },
        streamedToolsByRound: { 0: [tool] }
      }),
      0
    );
    expect(usage).toEqual({
      tokens: 4_000 + estimateTokens(`read\n{"path":"a.ts"}\n${FORTY}`),
      estimated: true
    });
  });

  it("declines to anchor on a snapshot with no input count", () => {
    // A snapshot that reports only output says nothing about how large the
    // context is; anchoring on it would claim the conversation had shrunk to
    // the size of one response.
    const usage = liveContextTokens(
      runFixture({
        usageByRound: { 0: { outputTokens: 300 } },
        streamedTextByRound: { 0: FORTY }
      }),
      2_000
    );
    expect(usage).toEqual({ tokens: 2_000, estimated: true });
  });
});
