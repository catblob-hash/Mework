import type { AutoCompactSettings } from "../types";

/**
 * The composer's auto-compact setting. The host owns the behaviour
 * (`src-tauri/src/handoff.rs`): past the threshold the conversation is armed to
 * hand off. These mirror its range and its rounding so the menu can say exactly
 * when that happens.
 */
export const AUTO_COMPACT_MIN_PERCENT = 20;
export const AUTO_COMPACT_MAX_PERCENT = 97;
const AUTO_COMPACT_DEFAULT_PERCENT = 80;

export function defaultAutoCompactSettings(): AutoCompactSettings {
  return { enabled: true, thresholdPercent: AUTO_COMPACT_DEFAULT_PERCENT };
}

/** A whole percent inside the range the setting offers. */
export function clampAutoCompactPercent(value: number): number {
  if (!Number.isFinite(value)) return AUTO_COMPACT_DEFAULT_PERCENT;
  return Math.min(AUTO_COMPACT_MAX_PERCENT, Math.max(AUTO_COMPACT_MIN_PERCENT, Math.round(value)));
}

export function normalizeAutoCompactSettings(value: unknown, fallback: AutoCompactSettings): AutoCompactSettings {
  const input = value && typeof value === "object" && !Array.isArray(value)
    ? value as Record<string, unknown>
    : {};
  return {
    enabled: typeof input.enabled === "boolean" ? input.enabled : fallback.enabled,
    thresholdPercent: typeof input.thresholdPercent === "number"
      ? clampAutoCompactPercent(input.thresholdPercent)
      : fallback.thresholdPercent
  };
}

/** Tokens at which a conversation is armed to hand off: the percent of the window, rounded down. */
export function autoCompactThresholdTokens(contextWindow: number, percent: number): number {
  return Math.floor((contextWindow * clampAutoCompactPercent(percent)) / 100);
}
