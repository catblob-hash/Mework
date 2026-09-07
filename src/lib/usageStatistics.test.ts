import { afterEach, describe, expect, it, vi } from "vitest";
import {
  formatCompactCount,
  MAX_SERIES_POINTS,
  niceTicks,
  purrDuration,
  summarizeUsage,
  usageBackfillEventsFromTurns,
  USAGE_BACKFILL_STORAGE_KEY,
  type ActivityBucket,
  type UsageBucket,
  type UsageStatistics
} from "./usageStatistics";
import { CONVERSATION_TURNS_STORAGE_KEY } from "./conversationTurns";
import type { AppDocument } from "../types";

/**
 * Construct buckets at local whole hours. The ledger stores UTC hours, but rendering converts them to local days and hours; local inputs make assertions independent of the machine time zone.
 */
function hourAt(year: number, month: number, day: number, hour = 10): number {
  return new Date(year, month - 1, day, hour, 0, 0, 0).getTime();
}

function activity(hourStartMs: number, patch: Partial<ActivityBucket> = {}): ActivityBucket {
  return { hourStartMs, userMessages: 0, assistantMessages: 0, sessions: 0, ...patch };
}

function usage(hourStartMs: number, patch: Partial<UsageBucket> = {}): UsageBucket {
  return {
    hourStartMs,
    providerId: "p1",
    providerName: "Provider",
    modelId: "model-a",
    origin: "conversation",
    requests: 1,
    inputTokens: 0,
    cachedInputTokens: 0,
    outputTokens: 0,
    totalTokens: 0,
    ...patch
  };
}

function statistics(patch: Partial<UsageStatistics> = {}): UsageStatistics {
  return {
    generatedAtMs: hourAt(2026, 8, 20, 12),
    ledgerStartedAtMs: 0,
    activity: [],
    usage: [],
    ...patch
  };
}

describe("summarizeUsage", () => {
  it("adds up sessions, messages and tokens inside the selected range only", () => {
    const now = hourAt(2026, 8, 20, 12);
    const summary = summarizeUsage(
      statistics({
        activity: [
          activity(hourAt(2026, 6, 1, 9), { userMessages: 5, assistantMessages: 5, sessions: 2 }),
          activity(hourAt(2026, 8, 19, 9), { userMessages: 3, assistantMessages: 4, sessions: 1 })
        ],
        usage: [
          usage(hourAt(2026, 6, 1, 9), { totalTokens: 1000, inputTokens: 800, outputTokens: 200 }),
          usage(hourAt(2026, 8, 19, 9), { totalTokens: 50, inputTokens: 30, outputTokens: 20 })
        ]
      }),
      "7d",
      now
    );
    expect(summary.sessions).toBe(1);
    expect(summary.messages).toBe(7);
    expect(summary.totalTokens).toBe(50);
    expect(summary.inputTokens).toBe(30);

    const everything = summarizeUsage(
      statistics({
        activity: [
          activity(hourAt(2026, 6, 1, 9), { userMessages: 5, assistantMessages: 5, sessions: 2 }),
          activity(hourAt(2026, 8, 19, 9), { userMessages: 3, assistantMessages: 4, sessions: 1 })
        ],
        usage: [
          usage(hourAt(2026, 6, 1, 9), { totalTokens: 1000 }),
          usage(hourAt(2026, 8, 19, 9), { totalTokens: 50 })
        ]
      }),
      "all",
      now
    );
    expect(everything.sessions).toBe(3);
    expect(everything.messages).toBe(17);
    expect(everything.totalTokens).toBe(1050);
  });

  it("counts today into the current streak and keeps yesterday's streak alive before the first message", () => {
    const now = hourAt(2026, 8, 20, 12);
    const withToday = summarizeUsage(
      statistics({
        activity: [
          activity(hourAt(2026, 8, 18, 9), { userMessages: 1 }),
          activity(hourAt(2026, 8, 19, 9), { userMessages: 1 }),
          activity(hourAt(2026, 8, 20, 9), { userMessages: 1 })
        ]
      }),
      "all",
      now
    );
    expect(withToday.currentStreak).toBe(3);
    const beforeToday = summarizeUsage(
      statistics({
        activity: [
          activity(hourAt(2026, 8, 18, 9), { userMessages: 1 }),
          activity(hourAt(2026, 8, 19, 9), { userMessages: 1 })
        ]
      }),
      "all",
      now
    );
    expect(beforeToday.currentStreak).toBe(2);
  });

  it("reports the longest streak across a gap, not the current one", () => {
    const now = hourAt(2026, 8, 20, 12);
    const summary = summarizeUsage(
      statistics({
        activity: [
          activity(hourAt(2026, 8, 1, 9), { userMessages: 1 }),
          activity(hourAt(2026, 8, 2, 9), { userMessages: 1 }),
          activity(hourAt(2026, 8, 3, 9), { userMessages: 1 }),
          activity(hourAt(2026, 8, 4, 9), { userMessages: 1 }),
          activity(hourAt(2026, 8, 20, 9), { userMessages: 1 })
        ]
      }),
      "all",
      now
    );
    expect(summary.longestStreak).toBe(4);
    expect(summary.currentStreak).toBe(1);
    expect(summary.activeDays).toBe(5);
  });

  it("picks the busiest local hour and the model with the most tokens", () => {
    const now = hourAt(2026, 8, 20, 12);
    const summary = summarizeUsage(
      statistics({
        activity: [
          activity(hourAt(2026, 8, 19, 9), { userMessages: 2 }),
          activity(hourAt(2026, 8, 19, 20), { userMessages: 9 })
        ],
        usage: [
          usage(hourAt(2026, 8, 19, 9), { modelId: "small", totalTokens: 10, requests: 9 }),
          usage(hourAt(2026, 8, 19, 20), { modelId: "big", totalTokens: 900, requests: 1 })
        ]
      }),
      "all",
      now
    );
    expect(summary.peakHour).toBe(20);
    expect(summary.favoriteModel?.modelId).toBe("big");
    expect(summary.models.map((model) => model.modelId)).toEqual(["big", "small"]);
    expect(summary.models[0].share).toBeCloseTo(900 / 910);
  });

  it("leaves the peak hour empty when the range holds no messages", () => {
    const summary = summarizeUsage(statistics(), "all", hourAt(2026, 8, 20, 12));
    expect(summary.peakHour).toBeNull();
    expect(summary.favoriteModel).toBeNull();
    expect(summary.hasActivity).toBe(false);
  });

  it("lays the heatmap out as 26 week columns ending in the current week", () => {
    const now = hourAt(2026, 8, 20, 12);
    const summary = summarizeUsage(
      statistics({ activity: [activity(hourAt(2026, 8, 20, 9), { userMessages: 4 })] }),
      "all",
      now
    );
    expect(summary.heatmapWeeks).toHaveLength(26);
    expect(summary.heatmapWeeks.every((week) => week.length === 7)).toBe(true);
    const today = summary.heatmapWeeks.at(-1)?.find((cell) => cell.messages === 4);
    expect(today).toBeDefined();
    expect(today?.level).toBeGreaterThan(0);
    expect(today?.future).toBe(false);
    expect(summary.heatmapWeeks.at(-1)?.some((cell) => cell.future)).toBe(true);
  });

  it("always paints the busiest day at the darkest level", () => {
    const now = hourAt(2026, 8, 20, 12);
    const summary = summarizeUsage(
      statistics({
        activity: [
          activity(hourAt(2026, 8, 17, 9), { userMessages: 1 }),
          activity(hourAt(2026, 8, 18, 9), { userMessages: 2 }),
          activity(hourAt(2026, 8, 19, 9), { userMessages: 3 }),
          activity(hourAt(2026, 8, 20, 9), { userMessages: 40 })
        ]
      }),
      "all",
      now
    );
    const levels = new Map(
      summary.heatmapWeeks.flat().filter((cell) => cell.messages > 0)
        .map((cell) => [cell.messages, cell.level])
    );
    expect(levels.get(40)).toBe(4);
    expect(levels.get(1)).toBe(1);
    expect(new Set(levels.values()).size).toBeGreaterThan(1);
  });

  it("keeps the heatmap on the full history even when the tiles are scoped to 7 days", () => {
    const now = hourAt(2026, 8, 20, 12);
    const summary = summarizeUsage(
      statistics({ activity: [activity(hourAt(2026, 7, 1, 9), { userMessages: 3 })] }),
      "7d",
      now
    );
    expect(summary.messages).toBe(0);
    expect(summary.heatmapWeeks.flat().some((cell) => cell.messages === 3)).toBe(true);
  });
});

describe("summarizeUsage series", () => {
  it("lays one bar per day across the range and stacks them in model order", () => {
    const now = hourAt(2026, 8, 20, 12);
    const summary = summarizeUsage(
      statistics({
        usage: [
          usage(hourAt(2026, 8, 19, 9), { modelId: "small", totalTokens: 100 }),
          usage(hourAt(2026, 8, 19, 20), { modelId: "big", totalTokens: 700 }),
          usage(hourAt(2026, 8, 20, 9), { modelId: "small", totalTokens: 50 })
        ]
      }),
      "7d",
      now
    );
    expect(summary.series.bucket).toBe("day");
    expect(summary.series.points).toHaveLength(7);
    expect(summary.series.points.at(-1)?.key).toBe("2026-08-20");
    expect(summary.series.points.at(0)?.key).toBe("2026-08-14");
    expect(summary.models.map((row) => row.modelId)).toEqual(["big", "small"]);

    const [nineteenth, twentieth] = summary.series.points.slice(-2);
    // Index 0 is the biggest model, so the bars and the legend share one ramp.
    expect(nineteenth.byModel).toEqual([700, 100]);
    expect(nineteenth.totalTokens).toBe(800);
    expect(twentieth.byModel).toEqual([0, 50]);
    expect(summary.series.points.slice(0, 5).every((point) => point.totalTokens === 0)).toBe(true);
  });

  it("opens the all-time window at the first token, not the first message", () => {
    const now = hourAt(2026, 8, 20, 12);
    const summary = summarizeUsage(
      statistics({
        activity: [activity(hourAt(2026, 3, 1, 9), { userMessages: 40 })],
        usage: [usage(hourAt(2026, 8, 19, 9), { totalTokens: 10 })]
      }),
      "all",
      now
    );
    expect(summary.series.bucket).toBe("day");
    expect(summary.series.points.map((point) => point.key)).toEqual(["2026-08-19", "2026-08-20"]);
  });

  it("widens the bucket once the window outgrows daily bars", () => {
    const now = hourAt(2026, 8, 20, 12);
    const weekly = summarizeUsage(
      statistics({ usage: [usage(hourAt(2026, 5, 1, 9), { totalTokens: 10 })] }),
      "all",
      now
    );
    expect(weekly.series.bucket).toBe("week");
    expect(weekly.series.points.length).toBeLessThan(MAX_SERIES_POINTS);
    // Every bar starts on a Monday and the last one covers today.
    expect(weekly.series.points.every((point) => new Date(point.startMs).getDay() === 1)).toBe(true);
    expect(weekly.series.points.at(-1)?.startMs).toBe(hourAt(2026, 8, 17, 0));
    expect(weekly.series.points.reduce((sum, point) => sum + point.totalTokens, 0)).toBe(10);

    const monthly = summarizeUsage(
      statistics({ usage: [usage(hourAt(2022, 1, 9, 9), { totalTokens: 10 })] }),
      "all",
      now
    );
    expect(monthly.series.bucket).toBe("month");
    expect(monthly.series.points.every((point) => new Date(point.startMs).getDate() === 1)).toBe(true);
    expect(monthly.series.points.at(-1)?.key).toBe("2026-08-01");
  });

  it("caps the bar count from the recent end when a timestamp predates everything", () => {
    const now = hourAt(2026, 8, 20, 12);
    const summary = summarizeUsage(
      statistics({ usage: [usage(0, { totalTokens: 10 })] }),
      "all",
      now
    );
    expect(summary.series.bucket).toBe("month");
    expect(summary.series.points).toHaveLength(MAX_SERIES_POINTS);
    expect(summary.series.points.at(-1)?.key).toBe("2026-08-01");
    // The truncated end is the old one, and the tokens it held fold into the
    // oldest surviving bar rather than vanishing from the chart.
    expect(summary.series.points[0].totalTokens).toBe(10);
  });

  it("never lets the bars add up to less than the legend", () => {
    const now = hourAt(2026, 8, 20, 12);
    const cases: Record<string, UsageBucket[]> = {
      ordinary: [
        usage(hourAt(2026, 8, 19, 9), { modelId: "a", totalTokens: 700 }),
        usage(hourAt(2026, 8, 20, 9), { modelId: "b", totalTokens: 50 })
      ],
      // A clock that moved backwards leaves ledger rows dated after `now`.
      future: [usage(hourAt(2026, 8, 21, 9), { totalTokens: 900 })],
      // Older than any bar the window can hold.
      prehistoric: [usage(0, { totalTokens: 90 })],
      // Long enough to force week buckets, then month buckets.
      weekly: [usage(hourAt(2026, 5, 1, 9), { totalTokens: 40 })],
      monthly: [usage(hourAt(2021, 1, 1, 9), { totalTokens: 40 })]
    };
    for (const [name, buckets] of Object.entries(cases)) {
      const summary = summarizeUsage(statistics({ usage: buckets }), "all", now);
      const charted = summary.series.points.reduce((sum, point) => sum + point.totalTokens, 0);
      expect(`${name}:${charted}`).toBe(`${name}:${summary.totalTokens}`);
      // Per-model stacks must add up to the same figure as the bar they compose.
      for (const point of summary.series.points) {
        expect(point.byModel.reduce((sum, value) => sum + value, 0)).toBe(point.totalTokens);
      }
      expect(summary.series.points.length).toBeLessThanOrEqual(MAX_SERIES_POINTS);
    }
  });

  it("counts real bars, not days, when choosing the bucket width", () => {
    // Sunday through Saturday: 434 inclusive days, but 63 Monday-aligned weeks.
    // Dividing days by seven says "62 weeks" and quietly draws one bar too many.
    const summary = summarizeUsage(
      statistics({ usage: [usage(hourAt(2025, 6, 15, 9), { totalTokens: 10 })] }),
      "all",
      hourAt(2026, 8, 22, 12)
    );
    expect(summary.series.bucket).toBe("month");
    expect(summary.series.points.length).toBeLessThanOrEqual(62);
  });

  it("rounds the axis up to readable steps", () => {
    expect(niceTicks(0)).toEqual({ max: 0, ticks: [] });
    expect(niceTicks(19_400_000)).toEqual({
      max: 20_000_000,
      ticks: [0, 5_000_000, 10_000_000, 15_000_000, 20_000_000]
    });
    expect(niceTicks(1_000_000)).toEqual({
      max: 1_000_000,
      ticks: [0, 250_000, 500_000, 750_000, 1_000_000]
    });
    expect(niceTicks(7)).toEqual({ max: 8, ticks: [0, 2, 4, 6, 8] });
  });

  it("spends the headroom rather than jumping a whole decade", () => {
    // A peak just past a step must not round to the next decade: 20.4M under a
    // fixed four-interval scale lands on 30M and leaves the top third empty.
    expect(niceTicks(20_400_000)).toEqual({
      max: 25_000_000,
      ticks: [0, 5_000_000, 10_000_000, 15_000_000, 20_000_000, 25_000_000]
    });
    // Steps stay integral, so a peak of 7 never produces a 7.5 gridline.
    expect(niceTicks(7).ticks.every((tick) => Number.isInteger(tick))).toBe(true);
    // Too small to carry three intervals — take the coarse axis over none.
    expect(niceTicks(1)).toEqual({ max: 1, ticks: [0, 1] });
    expect(niceTicks(2)).toEqual({ max: 2, ticks: [0, 1, 2] });
    for (const peak of [1, 3, 7, 999, 1234, 20_400_000, 115_400_000, 2_500_000_000]) {
      const { max, ticks } = niceTicks(peak);
      expect(max).toBeGreaterThanOrEqual(peak);
      expect(ticks.at(0)).toBe(0);
      expect(ticks.at(-1)).toBe(max);
      expect(ticks.every((tick) => Number.isInteger(tick))).toBe(true);
      // Duplicated levels would stack labels and grid lines on the same pixel.
      expect(new Set(ticks).size).toBe(ticks.length);
    }
  });

  it("leaves the axis empty when the range recorded requests but no tokens", () => {
    const now = hourAt(2026, 8, 20, 12);
    const summary = summarizeUsage(
      statistics({ usage: [usage(hourAt(2026, 8, 20, 9), { requests: 2 })] }),
      "7d",
      now
    );
    expect(summary.models).toHaveLength(1);
    expect(summary.series.max).toBe(0);
    expect(summary.series.ticks).toEqual([]);
    expect(summary.series.points.every((point) => point.totalTokens === 0)).toBe(true);
  });
});

describe("usageBackfillEventsFromTurns", () => {
  const document = {
    globalSettings: {
      apiProviders: [
        { id: "prov", name: "Prov", models: [{ id: "model-a" }] }
      ]
    },
    workspaces: [
      { id: "ws", conversations: [{ id: "conv" }] }
    ]
  } as unknown as AppDocument;

  it("derives one idempotent event per turn and resolves the provider from the model id", () => {
    const endedAt = new Date(hourAt(2026, 8, 19, 9)).toISOString();
    window.localStorage.setItem(CONVERSATION_TURNS_STORAGE_KEY, JSON.stringify({
      conv: [{
        id: "turn-1",
        requestId: "req-1",
        anchorContextId: "ctx-1",
        modelId: "model-a",
        startedAt: endedAt,
        endedAt,
        status: "completed",
        // A round that owns no message at all is swept from storage before it
        // ever reaches the backfill; a real completed round owns its reply.
        contextIds: ["ctx-2"],
        usage: { inputTokens: 10, cachedInputTokens: 3, outputTokens: 4, totalTokens: 14 },
        usageOffset: {},
        usageBaseline: {},
        usageRevisionAtStart: 0,
        segmentCount: 1,
        expanded: false,
        userToggled: false
      }]
    }));
    const events = usageBackfillEventsFromTurns(document);
    expect(events).toHaveLength(1);
    expect(events[0].id).toBe("turn:conv:turn-1");
    expect(events[0].providerName).toBe("Prov");
    expect(events[0].workspaceId).toBe("ws");
    expect(events[0].totalTokens).toBe(14);
    expect(events[0].occurredAtMs).toBe(hourAt(2026, 8, 19, 9));
    window.localStorage.removeItem(CONVERSATION_TURNS_STORAGE_KEY);
  });

  it("drops turns that never reported any usage", () => {
    window.localStorage.setItem(CONVERSATION_TURNS_STORAGE_KEY, JSON.stringify({
      conv: [{
        id: "turn-empty",
        requestId: "req",
        anchorContextId: "ctx",
        modelId: "model-a",
        startedAt: new Date(hourAt(2026, 8, 19, 9)).toISOString(),
        status: "interrupted",
        contextIds: [],
        usage: {},
        usageOffset: {},
        usageBaseline: {},
        usageRevisionAtStart: 0,
        segmentCount: 1,
        expanded: false,
        userToggled: false
      }]
    }));
    expect(usageBackfillEventsFromTurns(document)).toEqual([]);
    window.localStorage.removeItem(CONVERSATION_TURNS_STORAGE_KEY);
  });
});

describe("presentation helpers", () => {
  it("compacts counts the way the tiles read them", () => {
    expect(formatCompactCount(0)).toBe("0");
    expect(formatCompactCount(999)).toBe("999");
    expect(formatCompactCount(1234)).toBe("1.2K");
    expect(formatCompactCount(115_400_000)).toBe("115.4M");
    expect(formatCompactCount(2_500_000_000)).toBe("2.5B");
  });

  it("carries the purr duration up to the largest unit that still reads as a number", () => {
    expect(purrDuration(40_000)).toEqual({ value: 2.4, unit: "minute" });
    expect(purrDuration(1_000_000)).toEqual({ value: 1, unit: "hour" });
    expect(purrDuration(115_400_000)?.unit).toBe("day");
    expect(purrDuration(115_400_000)?.value).toBeCloseTo(4.81, 2);
    expect(purrDuration(720_000_000)).toEqual({ value: 1, unit: "month" });

    expect(purrDuration(0)).toBeNull();
  });
});

describe("statistics fetch", () => {
  it("returns the empty payload when there is no backend", async () => {
    vi.resetModules();
    vi.doMock("./backend", () => ({
      hasBackendRuntime: () => false,
      invoke: vi.fn(() => {
        throw new Error("must not reach the backend");
      })
    }));
    const module = await import("./usageStatistics");
    await expect(module.fetchUsageStatistics()).resolves.toEqual(module.EMPTY_USAGE_STATISTICS);
    await expect(module.pushUsageBackfill([])).resolves.toBe(0);
    vi.doUnmock("./backend");
    vi.resetModules();
  });
});

describe("ensureUsageBackfill", () => {
  const document = {
    globalSettings: { apiProviders: [] },
    workspaces: [{ id: "ws", conversations: [{ id: "conv" }] }]
  } as unknown as AppDocument;

  function storeOneTurn(occurredAtMs: number) {
    const iso = new Date(occurredAtMs).toISOString();
    window.localStorage.setItem(CONVERSATION_TURNS_STORAGE_KEY, JSON.stringify({
      conv: [{
        id: "turn-1",
        requestId: "r",
        anchorContextId: "c",
        modelId: "m",
        startedAt: iso,
        endedAt: iso,
        status: "completed",
        contextIds: ["ctx-reply"],
        usage: { inputTokens: 5, outputTokens: 5, totalTokens: 10 },
        usageOffset: {},
        usageBaseline: {},
        usageRevisionAtStart: 0,
        segmentCount: 1,
        expanded: false,
        userToggled: false
      }]
    }));
  }

  afterEach(() => {
    window.localStorage.removeItem(CONVERSATION_TURNS_STORAGE_KEY);
    window.localStorage.removeItem(USAGE_BACKFILL_STORAGE_KEY);
    vi.doUnmock("./backend");
    vi.resetModules();
  });

  async function withBackend() {
    vi.resetModules();
    const invoke = vi.fn(async (_command: string, _args?: unknown) => 1);
    vi.doMock("./backend", () => ({ hasBackendRuntime: () => true, invoke }));
    return { invoke, module: await import("./usageStatistics") };
  }

  it("pushes only what predates the ledger's start, once per ledger", async () => {
    const started = hourAt(2026, 8, 20, 0);
    storeOneTurn(started - 60_000);
    const { invoke, module } = await withBackend();
    const stats = { ...statistics(), ledgerStartedAtMs: started };

    expect(await module.ensureUsageBackfill(document, stats)).toBe(true);
    expect(invoke).toHaveBeenCalledTimes(1);
    expect(invoke).toHaveBeenCalledWith("backfill_token_usage", expect.anything());
    expect(await module.ensureUsageBackfill(document, stats)).toBe(false);
    expect(invoke).toHaveBeenCalledTimes(1);
    expect(await module.ensureUsageBackfill(
      document,
      { ...stats, ledgerStartedAtMs: started + 1 }
    )).toBe(true);
    expect(invoke).toHaveBeenCalledTimes(2);
  });

  it("never pushes a turn the gateway already recorded itself", async () => {
    const started = hourAt(2026, 8, 20, 0);
    storeOneTurn(started + 60_000);
    const { invoke, module } = await withBackend();

    expect(await module.ensureUsageBackfill(
      document,
      { ...statistics(), ledgerStartedAtMs: started }
    )).toBe(false);
    expect(invoke).not.toHaveBeenCalled();
  });
});
