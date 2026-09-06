//! Responses-compatible endpoint dialect adaptation.
//!
//! Plaintext-reasoning models require response adaptation. The adapter maps
//! DeepSeek `reasoning_text` content events to the summary events modeled by
//! `@ai-sdk/openai`. Requests retain encrypted replay data in every mode.

import { sseDialectFetch } from "./sse.js";

/** Fast marker for SSE lines that may need reasoning-event translation. */
const REASONING_TEXT_MARKER = "reasoning_text";

/**
 * Translate one SSE line from reasoning-text content events to summary events.
 *
 * Exactly three frame shapes are recognized; `event:` lines are never used for
 * dispatch. Synthesized chunks must carry `item_id: string`,
 * `output_index: number|null`, `summary_index: number`, and `delta: string`
 * where applicable — a missing field fails the SDK's Zod validation and
 * terminates the stream. All other frames pass through unchanged so the SDK can
 * handle or ignore them.
 */
export function translateReasoningTextSseLine(line: string): string {
  if (!line.startsWith("data:") || !line.includes(REASONING_TEXT_MARKER)) return line;
  const payload = line.slice("data:".length).trim();
  if (payload.length === 0 || payload === "[DONE]") return line;
  let frame: unknown;
  try {
    frame = JSON.parse(payload);
  } catch {
    return line;
  }
  if (typeof frame !== "object" || frame === null) return line;
  const chunk = frame as {
    type?: unknown;
    item_id?: unknown;
    output_index?: unknown;
    content_index?: unknown;
    delta?: unknown;
    part?: { type?: unknown; text?: unknown };
  };
  if (typeof chunk.type !== "string" || typeof chunk.item_id !== "string") return line;
  const summaryIndex = typeof chunk.content_index === "number" ? chunk.content_index : 0;
  const outputIndex = typeof chunk.output_index === "number" ? chunk.output_index : null;

  if (chunk.type === "response.reasoning_text.delta" && typeof chunk.delta === "string") {
    return `data: ${JSON.stringify({
      type: "response.reasoning_summary_text.delta",
      item_id: chunk.item_id,
      output_index: outputIndex,
      summary_index: summaryIndex,
      delta: chunk.delta,
    })}`;
  }
  const isReasoningPart =
    typeof chunk.part === "object" && chunk.part !== null && chunk.part.type === "reasoning_text";
  if (chunk.type === "response.content_part.added" && isReasoningPart) {
    return `data: ${JSON.stringify({
      type: "response.reasoning_summary_part.added",
      item_id: chunk.item_id,
      output_index: outputIndex,
      summary_index: summaryIndex,
    })}`;
  }
  if (chunk.type === "response.content_part.done" && isReasoningPart) {
    return `data: ${JSON.stringify({
      type: "response.reasoning_summary_part.done",
      item_id: chunk.item_id,
      output_index: outputIndex,
      summary_index: summaryIndex,
    })}`;
  }
  // Unmodeled event types pass through for the SDK to ignore.
  return line;
}

/**
 * Dialect fetch for plaintext-reasoning Responses models.
 *
 * It translates reasoning content events without changing request parameters.
 */
export function plaintextReasoningFetch(
  inner: typeof globalThis.fetch = globalThis.fetch
): typeof globalThis.fetch {
  return sseDialectFetch(() => translateReasoningTextSseLine, undefined, inner);
}
