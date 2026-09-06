import { describe, expect, it } from "vitest";
import type { ContextItem } from "../types";
import { computeContextBreakdown } from "./contextBreakdown";
import { estimateContextsTokens, estimateTokens } from "./contextTokens";

function text(kind: "system" | "user" | "assistant", content: string): ContextItem {
  return { id: `${kind}-${content.length}`, kind, content, createdAt: "2026-08-27T00:00:00Z" };
}

function reasoning(content: string): ContextItem {
  return { id: `reasoning-${content.length}`, kind: "reasoning", content, createdAt: "2026-08-27T00:00:00Z" };
}

function tool(output: string): ContextItem {
  return {
    id: `tool-${output.length}`,
    kind: "tool",
    toolName: "read_file",
    input: {},
    result: { success: true, output, executedAt: "2026-08-27T00:00:00Z", durationMs: 1 },
    createdAt: "2026-08-27T00:00:00Z"
  };
}

const contexts = [
  text("user", "u".repeat(400)),
  text("assistant", "a".repeat(800)),
  reasoning("r".repeat(200)),
  tool("t".repeat(1200))
];

describe("computeContextBreakdown", () => {
  it("charges the gap between the provider total and the local estimate to 其他", () => {
    const accounted = estimateContextsTokens(contexts) + estimateTokens("prompt");
    const breakdown = computeContextBreakdown({
      contexts,
      systemPrompt: "prompt",
      used: accounted + 5000,
      window: 100_000
    });

    const other = breakdown.segments.find((segment) => segment.id === "other");
    expect(other?.tokens).toBe(5000);
    // Preserve visible rows unchanged when the authoritative total exceeds the estimate.
    const visible = breakdown.segments
      .filter((segment) => segment.id !== "other")
      .reduce((sum, segment) => sum + segment.tokens, 0);
    expect(visible).toBe(accounted);
    expect(breakdown.free).toBe(100_000 - (accounted + 5000));
  });

  it("scales the estimate back down when it overshoots the authoritative total", () => {
    const accounted = estimateContextsTokens(contexts) + estimateTokens("prompt");
    const used = Math.floor(accounted / 2);
    const breakdown = computeContextBreakdown({
      contexts,
      systemPrompt: "prompt",
      used,
      window: 100_000
    });

    expect(breakdown.segments.some((segment) => segment.id === "other")).toBe(false);
    const total = breakdown.segments.reduce((sum, segment) => sum + segment.tokens, 0);
    // Per-row rounding can introduce a few tokens of error, but rows must never exceed the header total.
    expect(total).toBeLessThanOrEqual(used);
    expect(total).toBeGreaterThan(used - breakdown.segments.length);
  });

  it("folds timeline system contexts into the system prompt row", () => {
    const notice = text("system", "n".repeat(120));
    const withNotice = computeContextBreakdown({
      contexts: [notice],
      systemPrompt: "prompt",
      used: 10_000,
      window: 100_000
    });
    const withoutNotice = computeContextBreakdown({
      contexts: [],
      systemPrompt: "prompt",
      used: 10_000,
      window: 100_000
    });

    const row = (breakdown: typeof withNotice) => (
      breakdown.segments.find((segment) => segment.id === "systemPrompt")?.tokens ?? 0
    );
    expect(row(withNotice)).toBe(row(withoutNotice) + estimateTokens("n".repeat(120)));
  });

  it("drops empty rows and shares the window as the denominator", () => {
    const breakdown = computeContextBreakdown({
      contexts: [text("user", "u".repeat(400))],
      systemPrompt: "",
      used: 1000,
      window: 10_000
    });

    expect(breakdown.segments.map((segment) => segment.id)).toEqual(["user", "other"]);
    expect(breakdown.ratio).toBeCloseTo(0.1, 6);
    expect(breakdown.freeShare).toBeCloseTo(0.9, 6);
    const user = breakdown.segments.find((segment) => segment.id === "user");
    expect(user?.share).toBeCloseTo((user?.tokens ?? 0) / 10_000, 6);
  });

  it("reports no window when the model does not declare one", () => {
    const breakdown = computeContextBreakdown({
      contexts,
      systemPrompt: "",
      used: 5000,
      window: null
    });

    expect(breakdown.window).toBeNull();
    expect(breakdown.free).toBeNull();
    expect(breakdown.freeShare).toBeNull();
    expect(breakdown.ratio).toBeNull();
    // Without a window, shares use used tokens, so component rows still sum to one.
    const shares = breakdown.segments.reduce((sum, segment) => sum + segment.share, 0);
    expect(shares).toBeCloseTo(1, 2);
  });

  it("never reports a negative free space when the context is over budget", () => {
    const breakdown = computeContextBreakdown({
      contexts,
      systemPrompt: "",
      used: 12_000,
      window: 8000
    });

    expect(breakdown.free).toBe(0);
    expect(breakdown.freeShare).toBe(0);
    expect(breakdown.ratio).toBe(1);
  });

  it("survives an empty conversation", () => {
    const breakdown = computeContextBreakdown({
      contexts: [],
      systemPrompt: "",
      used: 0,
      window: 200_000
    });

    expect(breakdown.segments).toEqual([]);
    expect(breakdown.used).toBe(0);
    expect(breakdown.free).toBe(200_000);
    expect(breakdown.ratio).toBe(0);
  });
});
