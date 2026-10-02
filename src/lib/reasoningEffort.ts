import type { ReasoningEffort } from "../types";

/**
 * The five reasoning levels, lowest first. Mirrors Rust `model.rs::ReasoningEffort`.
 * There is no "off"; what each level becomes on the wire is the sidecar's
 * per-family mapping (`aisdk-service/src/reasoning.ts`).
 */
export const REASONING_EFFORTS: readonly ReasoningEffort[] = ["low", "medium", "high", "extra", "max"];

/** What a level falls back to when a document carries none. Mirrors the Rust `#[default]`. */
export const DEFAULT_REASONING_EFFORT: ReasoningEffort = "medium";

/**
 * Spellings older documents carry for levels this no longer has, mirroring the
 * Rust serde aliases: thinking-off and `minimal` read as the lowest level,
 * `xhigh` is `extra` under its former name.
 */
const RETIRED_SPELLINGS: Readonly<Record<string, ReasoningEffort>> = {
  disabled: "low",
  minimal: "low",
  xhigh: "extra"
};

/** The level `value` names, retired spellings included, or `null` when it names none. */
export function parseReasoningEffort(value: unknown): ReasoningEffort | null {
  if (typeof value !== "string") return null;
  if ((REASONING_EFFORTS as readonly string[]).includes(value)) return value as ReasoningEffort;
  return Object.hasOwn(RETIRED_SPELLINGS, value) ? RETIRED_SPELLINGS[value] : null;
}

export function normalizeReasoningEffort(
  value: unknown,
  fallback: ReasoningEffort = DEFAULT_REASONING_EFFORT
): ReasoningEffort {
  return parseReasoningEffort(value) ?? fallback;
}
