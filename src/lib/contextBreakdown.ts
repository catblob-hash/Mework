import type { ContextItem } from "../types";
import { estimateContextTokens, estimateTokens } from "./contextTokens";

/** A row in the breakdown. `id` is stable; the renderer supplies labels and colors. */
export type ContextSegmentId =
  | "systemPrompt"
  | "user"
  | "assistant"
  | "reasoning"
  | "tool"
  | "other";

export interface ContextSegment {
  id: ContextSegmentId;
  tokens: number;
  /** Fraction of the context window (0..1), or of used tokens when the window is unknown. */
  share: number;
}

export interface ContextBreakdown {
  /** Used tokens, preferring the provider's authoritative snapshot over local estimation. */
  used: number;
  /** Current model context window, or null when unknown. */
  window: number | null;
  free: number | null;
  freeShare: number | null;
  /** `used / window`, capped at 1, or null when the window is unknown. */
  ratio: number | null;
  /** Nonzero component rows in a fixed order. */
  segments: ContextSegment[];
}

const SEGMENT_ORDER: ContextSegmentId[] = [
  "systemPrompt",
  "user",
  "assistant",
  "reasoning",
  "tool",
  "other"
];

export interface ContextBreakdownInput {
  contexts: ContextItem[];
  /** System prompt from conversation settings. It is not a timeline item and needs separate estimation. */
  systemPrompt: string;
  used: number;
  window: number | null;
}

/**
 * Splits used context into visible components.
 *
 * `used` is usually the provider's authoritative input-token count, while individual components
 * can only be estimated with `estimateContextTokens`. Assign any positive difference to `other`,
 * which represents invisible host prompt content, tool schemas, memory, and skills. When estimates
 * exceed `used`, scale visible components down proportionally so they sum to the authoritative
 * total. Timeline `system` items join the system-prompt component; `localOnly` items estimate to
 * zero because they project to empty content.
 */
export function computeContextBreakdown({
  contexts,
  systemPrompt,
  used,
  window
}: ContextBreakdownInput): ContextBreakdown {
  const total = Math.max(0, Math.round(used));
  const limit = window !== null && window > 0 ? window : null;
  const raw: Record<ContextSegmentId, number> = {
    systemPrompt: estimateTokens(systemPrompt),
    user: 0,
    assistant: 0,
    reasoning: 0,
    tool: 0,
    other: 0
  };
  for (const item of contexts) {
    const tokens = estimateContextTokens(item);
    if (item.kind === "system") raw.systemPrompt += tokens;
    else raw[item.kind] += tokens;
  }
  const accounted = SEGMENT_ORDER.reduce((sum, id) => sum + raw[id], 0);
  raw.other = Math.max(0, total - accounted);
  const scale = accounted > total && accounted > 0 ? total / accounted : 1;

  const denominator = limit ?? total;
  const segments = SEGMENT_ORDER
    .map((id) => {
      const tokens = id === "other" ? raw.other : Math.round(raw[id] * scale);
      return { id, tokens, share: denominator > 0 ? tokens / denominator : 0 };
    })
    .filter((segment) => segment.tokens > 0);

  const free = limit === null ? null : Math.max(0, limit - total);
  return {
    used: total,
    window: limit,
    free,
    freeShare: limit !== null && free !== null ? free / limit : null,
    ratio: limit === null ? null : Math.min(1, total / limit),
    segments
  };
}
