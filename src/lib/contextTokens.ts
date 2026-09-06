import type { ContextItem, JsonObject, JsonValue } from "../types";
import type { ModelRunState } from "./modelStream";

/** Stable structural JSON form used for open-set supersession comparisons. */
export function canonicalJson(value: JsonValue | JsonObject): string {
  const canonicalize = (item: JsonValue): JsonValue => {
    if (Array.isArray(item)) return item.map(canonicalize);
    if (item !== null && typeof item === "object") {
      return Object.fromEntries(
        Object.keys(item).sort().map((key) => [key, canonicalize(item[key])])
      ) as JsonValue;
    }
    return item;
  };
  return JSON.stringify(canonicalize(value as JsonValue));
}

/** Approximation shared with Rust: ASCII / 4 plus non-ASCII / 1.6, rounded up. */
export function estimateTokens(text: string): number {
  let ascii = 0;
  let nonAscii = 0;
  for (const character of text) {
    if (character.codePointAt(0)! <= 0x7f) ascii += 1;
    else nonAscii += 1;
  }
  return Math.ceil(ascii / 4 + nonAscii / 1.6);
}

function projectedContextText(item: ContextItem): string {
  if (item.kind === "tool") {
    return `${item.toolName}\n${canonicalJson(item.input)}\n${item.result.output}`;
  }
  if (item.kind === "reasoning") {
    return `[Reasoning]\n${item.content ?? ""}`;
  }
  return item.kind === "system" && item.localOnly ? "" : item.content;
}

function estimateContextImageTokens(item: ContextItem): number {
  const images = item.kind === "user"
    ? item.images
    : item.kind === "tool"
      ? item.result.images
      : undefined;
  return images?.reduce((total, image) => {
    if (!image.width || !image.height) return total + 1024;
    const tiled = 85 + Math.ceil(image.width / 512) * Math.ceil(image.height / 512) * 170;
    return total + Math.max(1024, tiled);
  }, 0) ?? 0;
}

export function estimateContextTokens(item: ContextItem): number {
  return estimateTokens(projectedContextText(item)) + estimateContextImageTokens(item);
}

export function estimateContextsTokens(contexts: ContextItem[]): number {
  return contexts.reduce((total, item) => total + estimateContextTokens(item), 0);
}

/** Rounds a run has streamed anything for, whether or not usage arrived. */
function streamedRounds(run: ModelRunState): number[] {
  const rounds = new Set<number>();
  [run.streamedTextByRound, run.streamedReasoningByRound, run.streamedToolsByRound].forEach(
    (byRound) => {
      Object.keys(byRound).forEach((round) => rounds.add(Number(round)));
    }
  );
  return [...rounds];
}

/** What one round has put on screen since the last provider snapshot. */
function streamedRoundTokens(run: ModelRunState, round: number): number {
  const text = run.streamedTextByRound[round] ?? "";
  // Include the `[Reasoning]` prefix for each segment to match settled cards;
  // combining segments once would make the estimate jump at settlement.
  const reasoning = (run.streamedReasoningByRound[round] ?? [])
    .filter((item) => item.length > 0)
    .map((item) => `[Reasoning]\n${item}`)
    .join("\n");
  const tools = run.streamedToolsByRound[round] ?? [];
  return (
    estimateTokens(text)
    + estimateTokens(reasoning)
    // Same projection the settled tool context uses, so a round's estimate does
    // not change shape the moment it stops being live.
    + tools.reduce(
      (total, tool) => total + estimateTokens(
        `${tool.toolName}\n${canonicalJson(tool.input)}\n${tool.result.output}`
      ),
      0
    )
  );
}

/**
 * How large the conversation's context is *right now*, mid-turn.
 *
 * Providers only report usage at round boundaries — Anthropic at
 * `message_start` and `message_delta`, Chat Completions in its final chunk,
 * Responses at `response.completed` — so a gauge that waits for the turn's
 * final response sits frozen for the entire turn and then jumps. This anchors
 * on the newest snapshot the provider has actually given and adds an estimate
 * of everything streamed since, which is the only part that can move between
 * snapshots.
 *
 * `fallbackTokens` is the caller's estimate of the conversation as persisted,
 * used before this run's first snapshot arrives. Nothing is added on top of it:
 * the host writes streamed prose back as throttled `streaming` rows, so that
 * estimate already covers what has streamed and already climbs on its own —
 * adding the run's buffers as well would bill the same tokens twice.
 *
 * `estimated` is true whenever any part of the answer is estimated rather than
 * provider-reported, and drives the `~` the gauge prints.
 */
export function liveContextTokens(
  run: ModelRunState,
  fallbackTokens: number
): { tokens: number; estimated: boolean } {
  const usageRounds = Object.keys(run.usageByRound).map(Number);
  const anchorRound = usageRounds.length ? Math.max(...usageRounds) : null;
  const anchorUsage = anchorRound === null ? undefined : run.usageByRound[anchorRound];
  const anchorInput = anchorUsage?.inputTokens;
  // `inputTokens` is the context the provider actually read, so it is the only
  // figure worth anchoring on. Cached input is deliberately not added: it is
  // already inside `inputTokens`. A snapshot reporting only output says nothing
  // about how large the context is, and anchoring on it would claim the
  // conversation had shrunk to the size of one response.
  if (anchorRound === null || anchorInput === undefined) {
    return { tokens: Math.max(0, fallbackTokens), estimated: true };
  }
  // A round that has reported its output tokens is accounted for in full; one
  // that has only reported its input is still producing, so its own stream is
  // the growth.
  const settled = anchorUsage?.outputTokens;
  const growthFrom = settled === undefined ? anchorRound : anchorRound + 1;
  const growth = streamedRounds(run)
    .filter((round) => round >= growthFrom)
    .reduce((total, round) => total + streamedRoundTokens(run, round), 0);
  return {
    tokens: Math.max(0, anchorInput + (settled ?? 0) + growth),
    estimated: growth > 0
  };
}

export function formatCompactTokenCount(tokens: number): string {
  if (tokens >= 1_000_000) {
    return `${(tokens / 1_000_000).toFixed(tokens >= 10_000_000 ? 0 : 1).replace(/\.0$/, "")}m`;
  }
  if (tokens >= 1_000) {
    return `${(tokens / 1_000).toFixed(tokens >= 100_000 ? 0 : 1).replace(/\.0$/, "")}k`;
  }
  return String(Math.max(0, Math.round(tokens)));
}
