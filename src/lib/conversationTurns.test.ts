import { afterEach, beforeEach, describe, expect, it } from "vitest";
import type { ContextItem } from "../types";
import {
  CONVERSATION_TURNS_STORAGE_KEY,
  TIMELINE_START_ANCHOR,
  annotateTurnFailure,
  clearTurnFailures,
  dropEmptyConversationTurns,
  dropEmptyTurns,
  findResumableTurn,
  formatTurnDuration,
  loadConversationTurns,
  materializeRunTurnContexts,
  repairTurnAnchor,
  resumeConversationTurn,
  subtractModelUsage,
  sumModelUsage,
  turnLeavesNoRecord
} from "./conversationTurns";
import type { ConversationTurn } from "./conversationTurns";

function turn(overrides: Partial<ConversationTurn>): ConversationTurn {
  return {
    id: "turn-default",
    requestId: "request-current",
    anchorContextId: "user-default",
    modelId: "model-test",
    startedAt: "2026-07-24T00:00:00.000Z",
    status: "running",
    contextIds: [],
    usage: {},
    usageOffset: {},
    usageBaseline: {},
    usageRevisionAtStart: 0,
    segmentCount: 1,
    expanded: true,
    userToggled: false,
    ...overrides
  };
}

describe("conversation turn presentation state", () => {
  beforeEach(() => window.localStorage.removeItem(CONVERSATION_TURNS_STORAGE_KEY));
  afterEach(() => window.localStorage.removeItem(CONVERSATION_TURNS_STORAGE_KEY));

  it.each([
    [-1, "0s"],
    [999, "0s"],
    [1_000, "1s"],
    [59_999, "59s"],
    [60_000, "1m0s"],
    [61_000, "1m1s"],
    [3_600_000, "1h0m0s"],
    [3_661_000, "1h1m1s"],
    [86_400_000, "1d0h0m0s"],
    [90_061_000, "1d1h1m1s"]
  ])("formats %i ms as %s", (durationMs, expected) => {
    expect(formatTurnDuration(durationMs)).toBe(expected);
  });

  it("sums provider usage fields independently and subtracts a clamped baseline", () => {
    const cumulative = sumModelUsage([
      { inputTokens: 10, cachedInputTokens: 4, outputTokens: 2, totalTokens: 12 },
      { inputTokens: 7, cachedInputTokens: 3, outputTokens: 5, totalTokens: 12 },
      {}
    ]);

    expect(cumulative).toEqual({
      inputTokens: 17,
      cachedInputTokens: 7,
      outputTokens: 7,
      totalTokens: 24
    });
    expect(subtractModelUsage(cumulative, {
      inputTokens: 10,
      cachedInputTokens: 8,
      outputTokens: 2,
      totalTokens: 12
    })).toEqual({
      inputTokens: 7,
      cachedInputTokens: 0,
      outputTokens: 5,
      totalTokens: 12
    });
    expect(sumModelUsage([{}, {}])).toEqual({});
    expect(subtractModelUsage({ outputTokens: 3 }, { inputTokens: 9 })).toEqual({
      outputTokens: 3
    });
  });

  it("materializes every live turn from its anchor up to the next anchor", () => {
    const contexts: ContextItem[] = [
      { id: "system-before", kind: "system", content: "system", createdAt: "2026-07-24T00:00:00Z" },
      { id: "user-first", kind: "user", content: "first", createdAt: "2026-07-24T00:00:01Z" },
      { id: "reasoning-first", kind: "reasoning", content: "think first", createdAt: "2026-07-24T00:00:02Z" },
      { id: "assistant-first", kind: "assistant", content: "answer first", createdAt: "2026-07-24T00:00:03Z" },
      { id: "user-second", kind: "user", content: "second", createdAt: "2026-07-24T00:00:04Z" },
      { id: "reasoning-second", kind: "reasoning", content: "think second", createdAt: "2026-07-24T00:00:05Z" },
      { id: "assistant-second", kind: "assistant", content: "answer second", createdAt: "2026-07-24T00:00:06Z" }
    ];
    const historical = turn({
      id: "turn-historical",
      requestId: "request-old",
      anchorContextId: "missing-old-anchor",
      status: "completed",
      contextIds: ["historical-context"],
      expanded: false
    });
    const second = turn({
      id: "turn-second",
      anchorContextId: "user-second",
      startedAt: "2026-07-24T00:00:04.000Z"
    });
    const first = turn({
      id: "turn-first",
      anchorContextId: "user-first",
      startedAt: "2026-07-24T00:00:01.000Z"
    });

    const materialized = materializeRunTurnContexts(
      [historical, second, first],
      contexts,
      "request-current"
    );

    expect(materialized.find((item) => item.id === "turn-first")?.contextIds).toEqual([
      "reasoning-first",
      "assistant-first"
    ]);
    expect(materialized.find((item) => item.id === "turn-second")?.contextIds).toEqual([
      "reasoning-second",
      "assistant-second"
    ]);
    expect(materialized.find((item) => item.id === "turn-historical")?.contextIds).toEqual([
      "historical-context"
    ]);
    expect(contexts.map((context) => context.id)).toEqual([
      "system-before",
      "user-first",
      "reasoning-first",
      "assistant-first",
      "user-second",
      "reasoning-second",
      "assistant-second"
    ]);
  });

  it("recovers a stale persisted running turn as an expanded interruption", () => {
    window.localStorage.setItem(CONVERSATION_TURNS_STORAGE_KEY, JSON.stringify({
      conversation: [{
        id: "turn-stale",
        requestId: "request-stale",
        anchorContextId: "user-stale",
        modelId: "model-stale",
        startedAt: "2026-07-24T00:00:00.000Z",
        status: "running",
        contextIds: ["partial-stale"],
        usage: {
          inputTokens: 25,
          cachedInputTokens: 9,
          outputTokens: 4
        },
        usageBaseline: {},
        usageRevisionAtStart: 2
      }]
    }));

    expect(loadConversationTurns(Date.parse("2026-07-24T00:00:12.500Z"))).toEqual({
      conversation: [{
        id: "turn-stale",
        requestId: "request-stale",
        anchorContextId: "user-stale",
        modelId: "model-stale",
        startedAt: "2026-07-24T00:00:00.000Z",
        endedAt: "2026-07-24T00:00:12.500Z",
        durationMs: 12_500,
        status: "interrupted",
        contextIds: ["partial-stale"],
        usage: {
          inputTokens: 25,
          cachedInputTokens: 9,
          outputTokens: 4
        },
        usageOffset: {},
        usageBaseline: {},
        usageRevisionAtStart: 2,
        segmentCount: 1,
        expanded: true,
        userToggled: false
      }]
    });
  });

  it("preserves a persisted ask_user pause so its answer can resume the same turn", () => {
    window.localStorage.setItem(CONVERSATION_TURNS_STORAGE_KEY, JSON.stringify({
      conversation: [{
        ...turn({
          id: "turn-waiting",
          status: "awaiting_user",
          usage: { inputTokens: 20, cachedInputTokens: 5, outputTokens: 3 }
        })
      }]
    }));

    expect(loadConversationTurns(Date.parse("2026-07-24T00:00:12.500Z")).conversation[0])
      .toMatchObject({
        id: "turn-waiting",
        status: "awaiting_user",
        durationMs: 0,
        usage: { inputTokens: 20, cachedInputTokens: 5, outputTokens: 3 },
        expanded: true
      });
  });

  const failure = {
    message: "Cannot connect to API: bad port",
    providerName: "DeadEndpoint",
    modelName: "dead-model",
    at: "2026-07-24T00:00:12.500Z"
  };

  it("records a failure on the request's last turn, so an ask_user split blames the running segment", () => {
    const turns = [
      turn({ id: "turn-first", requestId: "run-1", status: "awaiting_user", contextIds: ["a"] }),
      turn({ id: "turn-tail", requestId: "run-1", status: "interrupted" }),
      turn({ id: "turn-other", requestId: "run-2", status: "completed", contextIds: ["b"] })
    ];

    const annotated = annotateTurnFailure(turns, "run-1", failure);

    expect(annotated.map((entry) => entry.error)).toEqual([undefined, failure, undefined]);
  });

  it("expands the failed turn unless the user already chose its state", () => {
    const collapsed = turn({ id: "turn-collapsed", expanded: false });
    const pinned = turn({ id: "turn-pinned", expanded: false, userToggled: true });

    expect(annotateTurnFailure([collapsed], "request-current", failure)[0].expanded).toBe(true);
    expect(annotateTurnFailure([pinned], "request-current", failure)[0].expanded).toBe(false);
  });

  it("leaves the turns untouched when the request has none", () => {
    const turns = [turn({ id: "turn-other", requestId: "run-2" })];

    expect(annotateTurnFailure(turns, "run-missing", failure)).toBe(turns);
  });

  it("drops a header-only failed turn on clear but keeps one that produced output", () => {
    const turns = [
      turn({ id: "turn-empty", status: "interrupted", error: failure }),
      turn({ id: "turn-partial", status: "interrupted", contextIds: ["partial"], error: failure }),
      turn({ id: "turn-clean", status: "completed", contextIds: ["reply"] })
    ];

    const cleared = clearTurnFailures(turns);

    // A turn whose only content was the notice would otherwise become exactly
    // the bare "stopped after 12s" header this field exists to remove.
    expect(cleared.map((entry) => entry.id)).toEqual(["turn-partial", "turn-clean"]);
    expect(cleared.every((entry) => entry.error === undefined)).toBe(true);
  });

  it("keeps an empty but still-running turn, which has yet to produce anything", () => {
    const turns = [turn({ id: "turn-live", status: "running", error: failure })];

    expect(clearTurnFailures(turns).map((entry) => entry.id)).toEqual(["turn-live"]);
  });

  it("returns the same reference when nothing failed, so state never churns", () => {
    const turns = [turn({ id: "turn-clean", status: "completed", contextIds: ["reply"] })];

    expect(clearTurnFailures(turns)).toBe(turns);
  });

  it("round-trips a failure through storage and drops one with no message", () => {
    window.localStorage.setItem(CONVERSATION_TURNS_STORAGE_KEY, JSON.stringify({
      conversation: [
        turn({ id: "turn-failed", status: "interrupted", error: failure }),
        turn({ id: "turn-blank", status: "interrupted", error: { ...failure, message: "   " } })
      ]
    }));

    const loaded = loadConversationTurns(Date.parse("2026-07-24T00:00:12.500Z")).conversation;

    // The notice was all either round had. Without a readable message the second
    // one has nothing left to show, so it is not a record worth loading.
    expect(loaded.map((entry) => entry.id)).toEqual(["turn-failed"]);
    expect(loaded[0].error).toEqual(failure);
  });
});

describe("a round that produced nothing", () => {
  beforeEach(() => window.localStorage.removeItem(CONVERSATION_TURNS_STORAGE_KEY));
  afterEach(() => window.localStorage.removeItem(CONVERSATION_TURNS_STORAGE_KEY));

  const notice = {
    message: "上游直接拒绝",
    providerName: "DeadEndpoint",
    modelName: "dead-model",
    at: "2026-07-24T00:00:12.500Z"
  };

  it.each([
    ["a stop before the first message", turn({ status: "interrupted" }), true],
    ["a run that finished empty", turn({ status: "completed" }), true],
    ["one still streaming", turn({ status: "running" }), false],
    ["one paused on a question", turn({ status: "awaiting_user" }), false],
    ["one carrying a failure notice", turn({ status: "interrupted", error: notice }), false],
    ["one that owns a message", turn({ status: "interrupted", contextIds: ["partial"] }), false]
  ])("%s", (_name, entry, discarded) => {
    expect(turnLeavesNoRecord(entry)).toBe(discarded);
  });

  it("returns the same reference when every round left a record", () => {
    const turns = [turn({ status: "running" }), turn({ status: "completed", contextIds: ["reply"] })];

    expect(dropEmptyTurns(turns)).toBe(turns);
  });

  it("drops only the rounds with nothing to show, per conversation", () => {
    const kept = turn({ id: "turn-kept", status: "completed", contextIds: ["reply"] });
    const dropped = turn({ id: "turn-dropped", status: "interrupted" });

    expect(dropEmptyConversationTurns({
      "conversation-a": [kept, dropped],
      "conversation-b": [dropped]
    })).toEqual({
      "conversation-a": [kept],
      "conversation-b": []
    });
  });

  it("sweeps a stored empty round but keeps one a host run may still be advancing", () => {
    window.localStorage.setItem(CONVERSATION_TURNS_STORAGE_KEY, JSON.stringify({
      conversation: [
        turn({ id: "turn-stopped", status: "interrupted" }),
        // Written while the run was live: the host may still be advancing it, so
        // adoption has to find it even before it owns anything.
        turn({ id: "turn-live", status: "running" })
      ]
    }));

    const loaded = loadConversationTurns(Date.parse("2026-07-24T00:00:12.500Z")).conversation;

    expect(loaded.map((entry) => entry.id)).toEqual(["turn-live"]);
    expect(loaded[0].status).toBe("interrupted");
  });

  it("is not a round to continue, so a bare Send opens one of its own", () => {
    const contexts: ContextItem[] = [
      { id: "user-round", kind: "user", content: "本轮", createdAt: "2026-07-24T00:01:00Z" }
    ];
    const empty = turn({ id: "turn-empty", anchorContextId: "user-round", status: "interrupted" });
    const produced = turn({ id: "turn-partial", anchorContextId: "user-round", status: "interrupted", contextIds: ["partial"] });

    expect(findResumableTurn([empty], contexts, "continue")).toBeUndefined();
    expect(findResumableTurn([produced], contexts, "continue")).toBe(produced);
  });
});

describe("continuing an unfinished round", () => {
  const previousRound: ContextItem[] = [
    { id: "user-previous", kind: "user", content: "上一轮", createdAt: "2026-07-24T00:00:00Z" },
    { id: "assistant-previous", kind: "assistant", content: "上一轮回答", createdAt: "2026-07-24T00:00:01Z" }
  ];
  const round: ContextItem[] = [
    { id: "user-round", kind: "user", content: "本轮", createdAt: "2026-07-24T00:01:00Z" },
    { id: "assistant-round", kind: "assistant", content: "中断前生成的一半", createdAt: "2026-07-24T00:01:01Z" }
  ];
  const contexts = [...previousRound, ...round];
  const stopped = turn({
    id: "turn-stopped",
    requestId: "run-stopped",
    anchorContextId: "user-round",
    status: "interrupted",
    contextIds: ["assistant-round"],
    startedAt: "2026-07-24T00:01:00.000Z",
    endedAt: "2026-07-24T00:01:30.000Z",
    durationMs: 30_000,
    usage: { inputTokens: 100, cachedInputTokens: 20, outputTokens: 50 }
  });

  it("keeps an anchor that is still in the timeline", () => {
    expect(repairTurnAnchor(stopped, contexts)).toBe("user-round");
  });

  it("re-anchors to the context before the first message the turn still owns", () => {
    const withoutAnchor = contexts.filter((context) => context.id !== "user-round");

    expect(repairTurnAnchor(stopped, withoutAnchor)).toBe("assistant-previous");
  });

  it("re-anchors to the timeline tail once the whole round is deleted", () => {
    expect(repairTurnAnchor(stopped, previousRound)).toBe("assistant-previous");
  });

  it("anchors at the start of the timeline when nothing precedes what it owns", () => {
    expect(repairTurnAnchor(stopped, round.slice(1))).toBe(TIMELINE_START_ANCHOR);
    expect(repairTurnAnchor(stopped, [])).toBe(TIMELINE_START_ANCHOR);
  });

  it("claims the whole timeline for a turn anchored at its start", () => {
    const fromStart = turn({
      id: "turn-from-start",
      requestId: "run-live",
      anchorContextId: TIMELINE_START_ANCHOR
    });

    expect(materializeRunTurnContexts([fromStart], round, "run-live")[0].contextIds)
      .toEqual(["user-round", "assistant-round"]);
  });

  it("continues the newest round while a stop left it unfinished", () => {
    expect(findResumableTurn([stopped], contexts, "continue")).toBe(stopped);
    expect(findResumableTurn(
      [turn({ id: "turn-done", status: "completed", contextIds: ["assistant-round"] })],
      contexts,
      "continue"
    )).toBeUndefined();
    expect(findResumableTurn([], contexts, "continue")).toBeUndefined();
  });

  it("continues a round whose messages were all deleted, which is the only trace left of it", () => {
    expect(findResumableTurn([stopped], previousRound, "continue")).toBe(stopped);
  });

  it("refuses to continue a round a later user message already closed", () => {
    const withNextMessage: ContextItem[] = [
      ...contexts,
      { id: "user-next", kind: "user", content: "换个话题", createdAt: "2026-07-24T00:02:00Z" }
    ];
    // Host-authored user contexts are the host speaking, not the user, so they
    // never end a round.
    const withHostNotice: ContextItem[] = [
      ...contexts,
      {
        id: "ctx_agent-message_notice",
        kind: "user",
        content: "后台任务已完成",
        createdAt: "2026-07-24T00:02:00Z"
      }
    ];

    expect(findResumableTurn([stopped], withNextMessage, "continue")).toBeUndefined();
    expect(findResumableTurn([stopped], withHostNotice, "continue")).toBe(stopped);
  });

  it("answers the question that paused, even when a newer round sits behind it", () => {
    const paused = turn({ id: "turn-paused", status: "awaiting_user" });
    const newer = turn({ id: "turn-newer", status: "completed", contextIds: ["assistant-round"] });

    expect(findResumableTurn([paused, newer], contexts, "awaiting")).toBe(paused);
    expect(findResumableTurn([newer], contexts, "awaiting")).toBeUndefined();
  });

  it("carries elapsed time and every token counter into the continuing request", () => {
    const [resumed] = resumeConversationTurn([stopped], previousRound, stopped, {
      requestId: "run-continued",
      modelId: "model-next",
      startedAt: "2026-07-24T00:05:00.000Z"
    });

    expect(resumed).toMatchObject({
      id: "turn-stopped",
      requestId: "run-continued",
      modelId: "model-next",
      startedAt: "2026-07-24T00:05:00.000Z",
      // Re-anchored at the tail: the round it belonged to was deleted outright.
      anchorContextId: "assistant-previous",
      status: "running",
      durationMs: 30_000,
      usageOffset: { inputTokens: 100, cachedInputTokens: 20, outputTokens: 50 },
      usageBaseline: {},
      usageRevisionAtStart: 0,
      segmentCount: 2,
      expanded: true
    });
    // The stop stamp and the notice explaining it are stale the moment the round
    // is live again.
    expect(resumed.endedAt).toBeUndefined();
    expect(resumed.error).toBeUndefined();
  });

  it("leaves a collapsed round collapsed when the user chose that state", () => {
    const collapsed = { ...stopped, expanded: false, userToggled: true };
    const reopened = { ...stopped, expanded: false, userToggled: false };
    const next = { requestId: "run-continued", modelId: "model-next", startedAt: "2026-07-24T00:05:00.000Z" };

    expect(resumeConversationTurn([collapsed], contexts, collapsed, next)[0].expanded).toBe(false);
    expect(resumeConversationTurn([reopened], contexts, reopened, next)[0].expanded).toBe(true);
  });
});
