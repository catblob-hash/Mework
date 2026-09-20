import type { ModelProfile } from "../types";

/**
 * Held back from the window so that, at the moment compaction fires, there is
 * still room to send the conversation plus the summarization prompt. Without a
 * reserve the trigger would arrive exactly when the request it needs to make no
 * longer fits.
 */
export const AUTO_COMPACT_BUFFER_TOKENS = 13_000;

/**
 * A model that never declares `maxOutputTokens` still needs room for its reply,
 * and one that declares a huge ceiling must not hand the whole window to it.
 * Both cases collapse to this cap.
 */
export const OUTPUT_RESERVE_CAP_TOKENS = 20_000;

/**
 * Input tokens at which auto-compaction fires, or `null` when the model cannot
 * support it. A model whose context window is unset has no free space to
 * measure — there is no honest threshold, and guessing one would compact a
 * conversation that had room or miss one that did not.
 */
export function autoCompactThreshold(model: ModelProfile | null | undefined): number | null {
  const window = model?.contextWindow;
  if (!window || window <= 0) return null;
  const outputReserve = Math.min(model?.maxOutputTokens ?? OUTPUT_RESERVE_CAP_TOKENS, OUTPUT_RESERVE_CAP_TOKENS);
  const threshold = window - outputReserve - AUTO_COMPACT_BUFFER_TOKENS;
  // A window small enough that the reserves consume it cannot be compacted into.
  return threshold > 0 ? threshold : null;
}

/** Whether `usedTokens` has consumed the free space the buffer was protecting. */
export function shouldAutoCompact(
  model: ModelProfile | null | undefined,
  usedTokens: number
): boolean {
  const threshold = autoCompactThreshold(model);
  return threshold !== null && usedTokens >= threshold;
}
