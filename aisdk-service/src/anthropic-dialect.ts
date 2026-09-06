//! Dialect differences for Anthropic-compatible endpoints.
//!
//! This layer absorbs provider-specific shape differences with a `fetch` wrapper
//! that preserves semantics. Its behaviors are copied from Claude Code, the
//! reference client for the Messages protocol, not derived from documentation.
//!
//! Request side (innermost, `anthropicSelfHealFetch`):
//!
//! - Claude Code's betas: `interleaved-thinking-2025-05-14` for every model that
//!   supports it, so thinking can continue between tool calls.
//! - Claude Code's prompt-cache breakpoints: one on the last system block and one
//!   on the last block of the last message.
//! - Claude Code's 400 self-heal chain: a rejected thinking signature strips every
//!   thinking block and retries, a rejected `thinking.type` swaps
//!   `enabled`/`adaptive`, an unsupported `effort` is dropped, an `input +
//!   max_tokens` overflow lowers `max_tokens`, and a rejected beta or
//!   `cache_control` is removed. Each repair is remembered per endpoint and model
//!   so later requests skip the wasted round trip.
//!
//! Request side (outer, `rewriteAdaptiveThinkingBody`): adaptive thinking is
//! rewritten into an explicit budget for endpoints that are not Anthropic itself.
//!
//! Response side: a non-streamed Message body answering a streaming request is
//! replayed as SSE, and the line rewriter makes the stream as tolerant as Claude
//! Code's assembler — unknown block and delta types are ignored instead of
//! failing the whole turn, missing scalar fields get their empty defaults, and
//! relay-wrapped server-tool error objects are unwrapped.

import { sseDialectFetch, type SseLineRewrite } from "./sse.js";

/**
 * `true` when requests reach Anthropic itself.
 *
 * A missing base URL selects the SDK default, which is official. An unparseable one
 * is treated as official as well, because every accommodation here trades correctness
 * on official endpoints for compatibility on relays and must not be applied blindly.
 */
function isOfficialAnthropicEndpoint(baseURL: string | undefined): boolean {
  if (!baseURL) return true;
  try {
    return new URL(baseURL).hostname === "api.anthropic.com";
  } catch {
    return true;
  }
}

type JsonObject = Record<string, unknown>;

function isObject(value: unknown): value is JsonObject {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

// ------------------------------------------------------------ Replayed reasoning

/** Provider-options key under which the host tags a replayed reasoning part with
 * the model that produced it. Mirrors `wire_history::REPLAY_TAG_KEY`. */
export const REPLAY_TAG_KEY = "mework";

/** Placeholder Claude Code inserts when a repair empties an assistant message. */
const NO_CONTENT_PLACEHOLDER = "(no content)";

function replayModelOf(part: JsonObject): string | undefined {
  const options = part.providerOptions;
  if (!isObject(options)) return undefined;
  const tag = options[REPLAY_TAG_KEY];
  if (!isObject(tag)) return undefined;
  return typeof tag.model === "string" ? tag.model : undefined;
}

/**
 * Drop replayed reasoning parts that another model signed.
 *
 * Claude Code removes signed and redacted thinking from history whenever the
 * conversation's model changed: Anthropic binds a signature to the model that
 * produced it and rejects it anywhere else. An assistant message emptied by the
 * removal keeps Claude Code's `(no content)` placeholder so the turn structure
 * survives. Mutates `messages` in place because it is request-private decoded JSON.
 */
export function dropForeignSignedReasoning(messages: unknown[], modelId: string): void {
  for (const message of messages) {
    if (!isObject(message) || message.role !== "assistant" || !Array.isArray(message.content)) continue;
    const kept = message.content.filter((part) => {
      if (!isObject(part) || part.type !== "reasoning") return true;
      const model = replayModelOf(part);
      return model === undefined || model === modelId;
    });
    if (kept.length === message.content.length) continue;
    if (kept.length === 0) kept.push({ type: "text", text: NO_CONTENT_PLACEHOLDER });
    message.content = kept;
  }
}

/** Remove the host's replay tag from every part; no provider may see a key it does not own. */
export function stripReplayTags(messages: unknown[]): void {
  for (const message of messages) {
    if (!isObject(message) || !Array.isArray(message.content)) continue;
    for (const part of message.content) {
      if (!isObject(part)) continue;
      const options = part.providerOptions;
      if (!isObject(options) || !(REPLAY_TAG_KEY in options)) continue;
      delete options[REPLAY_TAG_KEY];
      if (Object.keys(options).length === 0) delete part.providerOptions;
    }
  }
}

// ------------------------------------------------------------ Response tolerance

/** Only lines containing this substring need parsing; most delta lines skip it. */
const ERROR_MARKER = "_tool_result_error";

function isToolResultErrorObject(value: unknown): boolean {
  return (
    typeof value === "object"
    && value !== null
    && typeof (value as { type?: unknown }).type === "string"
    && (value as { type: string }).type.endsWith("_tool_result_error")
  );
}

/** Restores `content: [error]` to `content: error`.
 *
 * `true` means the frame changed, so callers know to serialize it again. Unchanged
 * lines retain their original bytes to avoid needless `JSON.stringify` changes such
 * as key order or escaping.
 */
function unwrapToolResultError(frame: unknown): boolean {
  if (typeof frame !== "object" || frame === null) return false;
  const block = (frame as { content_block?: unknown }).content_block;
  if (typeof block !== "object" || block === null) return false;
  const content = (block as { content?: unknown }).content;
  if (!Array.isArray(content) || content.length !== 1) return false;
  if (!isToolResultErrorObject(content[0])) return false;
  (block as { content: unknown }).content = content[0];
  return true;
}

/** Content block types the shipped `@ai-sdk/anthropic` stream schema models. */
const MODELED_BLOCK_TYPES = new Set([
  "text",
  "thinking",
  "redacted_thinking",
  "tool_use",
  "server_tool_use",
  "mcp_tool_use",
  "mcp_tool_result",
  "compaction",
  "web_search_tool_result",
  "web_fetch_tool_result",
  "code_execution_tool_result",
  "bash_code_execution_tool_result",
  "text_editor_code_execution_tool_result",
  "tool_search_tool_result",
  "advisor_tool_result",
  "fallback",
]);

/** Top-level stream event types the schema models; anything else is ignored. */
const MODELED_EVENT_TYPES = new Set([
  "message_start",
  "content_block_start",
  "content_block_delta",
  "content_block_stop",
  "message_delta",
  "message_stop",
  "ping",
  "error",
]);

function ensureNumber(target: JsonObject, key: string): boolean {
  if (typeof target[key] === "number") return false;
  target[key] = 0;
  return true;
}

function ensureString(target: JsonObject, key: string): boolean {
  if (typeof target[key] === "string") return false;
  target[key] = "";
  return true;
}

/**
 * Make one stream frame acceptable to the SDK schema, the way Claude Code's
 * assembler accepts it. Returns `false` to drop the frame, `true` when it was
 * changed, and `null` when the original bytes may pass through.
 */
function tolerateFrame(frame: JsonObject): boolean | null {
  const type = frame.type;
  if (typeof type !== "string" || !MODELED_EVENT_TYPES.has(type)) return false;
  let changed = false;
  switch (type) {
    case "message_start": {
      if (!isObject(frame.message)) return false;
      const message = frame.message;
      if (!isObject(message.usage)) {
        message.usage = { input_tokens: 0 };
        changed = true;
      } else if (ensureNumber(message.usage, "input_tokens")) {
        changed = true;
      }
      // Only deferred `tool_use` blocks are modeled here; anything else a relay
      // pre-populates is announced again by its own `content_block_start`.
      if (Array.isArray(message.content)
        && message.content.some((block) => !isObject(block) || block.type !== "tool_use")) {
        message.content = [];
        changed = true;
      }
      return changed ? true : null;
    }
    case "content_block_start": {
      if (typeof frame.index !== "number" || !isObject(frame.content_block)) return false;
      const block = frame.content_block;
      if (typeof block.type !== "string" || !MODELED_BLOCK_TYPES.has(block.type)) {
        // Claude Code keeps unknown blocks and never reads them; an empty text
        // block keeps the index alive without producing output.
        frame.content_block = { type: "text", text: "" };
        return true;
      }
      switch (block.type) {
        case "text":
          return ensureString(block, "text") ? true : null;
        case "thinking":
          return ensureString(block, "thinking") ? true : null;
        case "redacted_thinking":
          if (typeof block.data === "string") return null;
          frame.content_block = { type: "text", text: "" };
          return true;
        default:
          return null;
      }
    }
    case "content_block_delta": {
      if (typeof frame.index !== "number" || !isObject(frame.delta)) return false;
      const delta = frame.delta;
      switch (delta.type) {
        case "text_delta":
          return typeof delta.text === "string" ? null : false;
        case "thinking_delta":
          return typeof delta.thinking === "string" ? null : false;
        case "signature_delta":
          return typeof delta.signature === "string" ? null : false;
        case "input_json_delta":
          return typeof delta.partial_json === "string" ? null : false;
        case "compaction_delta":
          return null;
        default:
          // Claude Code ignores `citations_delta` and every unknown delta type.
          return false;
      }
    }
    case "content_block_stop":
      return typeof frame.index === "number" ? null : false;
    case "message_delta": {
      if (!isObject(frame.delta)) {
        frame.delta = {};
        changed = true;
      }
      if (!isObject(frame.usage)) {
        frame.usage = { output_tokens: 0 };
        changed = true;
      } else if (ensureNumber(frame.usage, "output_tokens")) {
        changed = true;
      }
      return changed ? true : null;
    }
    case "error": {
      if (!isObject(frame.error)) return false;
      if (ensureString(frame.error, "type")) changed = true;
      if (ensureString(frame.error, "message")) changed = true;
      return changed ? true : null;
    }
    default:
      return null;
  }
}

/** Normalizes one SSE line, returning the same string when no change is needed. */
export function normalizeAnthropicSseLine(line: string): string | null {
  if (!line.startsWith("data:")) return line;
  const payload = line.slice("data:".length).trim();
  if (payload.length === 0 || payload === "[DONE]") return line;
  let frame: unknown;
  try {
    frame = JSON.parse(payload);
  } catch {
    // Non-JSON is outside this wrapper's scope; the AI SDK validates protocol errors.
    return line;
  }
  if (!isObject(frame)) return line;
  const tolerated = tolerateFrame(frame);
  if (tolerated === false) return null;
  let changed = tolerated === true;
  if (line.includes(ERROR_MARKER) && unwrapToolResultError(frame)) changed = true;
  if (!changed) return line;
  return `data: ${JSON.stringify(frame)}`;
}

function makeAnthropicLineRewriter(): SseLineRewrite {
  const thinking = new Map<number, { signature: string; hasSignatureDelta: boolean; text: string; hasThinkingDelta: boolean }>();
  return (line) => {
    const normalized = normalizeAnthropicSseLine(line);
    if (!normalized?.startsWith("data:")) return normalized;
    let frame: unknown;
    try {
      frame = JSON.parse(normalized.slice(5).trim());
    } catch {
      return normalized;
    }
    if (!isObject(frame)) return normalized;
    if (frame.type === "message_stop" || frame.type === "message_start") thinking.clear();
    if (typeof frame.index !== "number") return normalized;
    const index = frame.index;
    if (frame.type === "content_block_start") {
      thinking.delete(index);
      const block = frame.content_block;
      if (isObject(block) && block.type === "thinking") {
        thinking.set(index, {
          signature: typeof block.signature === "string" ? block.signature : "",
          hasSignatureDelta: false,
          text: typeof block.thinking === "string" ? block.thinking : "",
          hasThinkingDelta: false,
        });
      }
    }
    const state = thinking.get(index);
    if (!state) return normalized;
    if (frame.type === "content_block_delta" && isObject(frame.delta)
      && frame.delta.type === "signature_delta" && typeof frame.delta.signature === "string"
      && frame.delta.signature.length > 0) state.hasSignatureDelta = true;
    if (frame.type === "content_block_delta" && isObject(frame.delta)
      && frame.delta.type === "thinking_delta" && typeof frame.delta.thinking === "string") state.hasThinkingDelta = true;
    if (frame.type === "content_block_stop") {
      thinking.delete(index);
      const deltas: JsonObject[] = [];
      if (state.text && !state.hasThinkingDelta) {
        deltas.push({ type: "thinking_delta", thinking: state.text });
      }
      if (state.signature && !state.hasSignatureDelta) {
        deltas.push({ type: "signature_delta", signature: state.signature });
      }
      if (deltas.length > 0) {
        // Close any upstream event-name line before emitting complete synthetic events.
        return `\n${deltas.map((delta) => `event: content_block_delta\ndata: ${JSON.stringify({ type: "content_block_delta", index, delta })}\n\n`).join("")}event: content_block_stop\n${normalized}`;
      }
    }
    return normalized;
  };
}

// ------------------------------------------------------------ Non-streamed Message

/** Events a streaming endpoint would have sent for one complete Message body. */
function messageAsSseFrames(message: JsonObject): string[] | null {
  const content = message.content;
  if (!Array.isArray(content)) return null;
  // A body with no terminal reason is truncated or malformed; replaying it as a
  // clean stream would dress that up as success.
  if (typeof message.stop_reason !== "string") return null;
  const usage = isObject(message.usage) ? message.usage : {};
  const events: JsonObject[] = [
    {
      type: "message_start",
      message: {
        id: message.id,
        type: "message",
        role: "assistant",
        model: message.model,
        content: [],
        stop_reason: null,
        stop_sequence: null,
        usage: { ...usage, input_tokens: typeof usage.input_tokens === "number" ? usage.input_tokens : 0 },
      },
    },
  ];
  content.forEach((block, index) => {
    if (!isObject(block)) return;
    switch (block.type) {
      case "text":
        events.push({ type: "content_block_start", index, content_block: { type: "text", text: "" } });
        if (typeof block.text === "string" && block.text.length > 0) {
          events.push({ type: "content_block_delta", index, delta: { type: "text_delta", text: block.text } });
        }
        break;
      case "thinking":
        events.push({ type: "content_block_start", index, content_block: { type: "thinking", thinking: "" } });
        if (typeof block.thinking === "string" && block.thinking.length > 0) {
          events.push({ type: "content_block_delta", index, delta: { type: "thinking_delta", thinking: block.thinking } });
        }
        if (typeof block.signature === "string" && block.signature.length > 0) {
          events.push({ type: "content_block_delta", index, delta: { type: "signature_delta", signature: block.signature } });
        }
        break;
      case "tool_use":
      case "server_tool_use": {
        const { input, ...start } = block;
        events.push({ type: "content_block_start", index, content_block: start });
        events.push({
          type: "content_block_delta",
          index,
          delta: { type: "input_json_delta", partial_json: JSON.stringify(input ?? {}) },
        });
        break;
      }
      default:
        events.push({ type: "content_block_start", index, content_block: block });
    }
    events.push({ type: "content_block_stop", index });
  });
  events.push({
    type: "message_delta",
    delta: { stop_reason: message.stop_reason, stop_sequence: message.stop_sequence ?? null },
    usage: { ...usage, output_tokens: typeof usage.output_tokens === "number" ? usage.output_tokens : 0 },
  });
  events.push({ type: "message_stop" });
  return events.map((event) => `data: ${JSON.stringify(event)}\n\n`);
}

/**
 * Replay a non-streamed Message body as an SSE stream.
 *
 * Execution is stream-only, so a relay that answers `stream: true` with one JSON
 * Message failed the turn. Only responses to a request that asked to stream are
 * converted; a genuinely non-streaming round trip is untouched.
 */
function nonStreamingMessageAsSseFetch(inner: typeof globalThis.fetch): typeof globalThis.fetch {
  return async (input, init) => {
    const body = init && typeof init.body === "string" ? init.body : null;
    const response = await inner(input, init);
    if (!body || !response.ok || !response.body) return response;
    const contentType = response.headers.get("content-type") ?? "";
    if (contentType.includes("text/event-stream") || !contentType.includes("json")) return response;
    let request: unknown;
    try {
      request = JSON.parse(body);
    } catch {
      return response;
    }
    if (!isObject(request) || request.stream !== true) return response;

    const text = await response.text();
    const replay = () =>
      new Response(text, { status: response.status, statusText: response.statusText, headers: response.headers });
    let payload: unknown;
    try {
      payload = JSON.parse(text);
    } catch {
      return replay();
    }
    if (!isObject(payload) || payload.type !== "message") return replay();
    const frames = messageAsSseFrames(payload);
    if (frames === null) return replay();
    const headers = new Headers(response.headers);
    headers.set("content-type", "text/event-stream");
    headers.delete("content-length");
    return new Response(new TextEncoder().encode(frames.join("")), {
      status: response.status,
      statusText: response.statusText,
      headers,
    });
  };
}

// ------------------------------------------------------------ Adaptive thinking

/**
 * Reasoning-budget share of `max_tokens` per upstream effort level.
 *
 * These mirror `DEFAULT_REASONING_BUDGET_PERCENTAGES` in `@ai-sdk/provider-utils`,
 * which the SDK itself applies to every model it believes cannot think adaptively.
 * `max` is the effort name the SDK substitutes for `xhigh` on models lacking that
 * level. `minimal` never appears here: the SDK maps it onto `low` before the request.
 */
const EFFORT_BUDGET_SHARE: Record<string, number> = {
  low: 0.1,
  medium: 0.3,
  high: 0.6,
  xhigh: 0.9,
  max: 0.9,
};

/** Anthropic's smallest accepted `budget_tokens`. */
const MIN_THINKING_BUDGET = 1024;

/**
 * The explicit-budget thinking form for `body`.
 *
 * With an effort the budget is that effort's share of `max_tokens`, taken out of
 * the existing ceiling rather than added on top: Anthropic counts thinking inside
 * `max_tokens`, and a relay's per-model ceiling is unknown here. Without an
 * effort it is Claude Code's fixed form, `max_tokens - 1`. Either way the budget
 * stays inside `[1024, max_tokens)`, growing `max_tokens` only when it cannot
 * contain the minimum.
 */
function enabledThinkingFor(body: JsonObject, requireEffort: boolean): JsonObject | null {
  const maxTokens = body.max_tokens;
  if (typeof maxTokens !== "number" || !Number.isInteger(maxTokens) || maxTokens < 1) return null;
  const config = body.output_config;
  const effort = isObject(config) ? config.effort : undefined;
  const share = typeof effort === "string" ? EFFORT_BUDGET_SHARE[effort] : undefined;
  if (share === undefined && requireEffort) return null;
  const budget = share === undefined
    ? Math.max(MIN_THINKING_BUDGET, maxTokens - 1)
    : Math.max(MIN_THINKING_BUDGET, Math.round(maxTokens * share));
  // `budget_tokens` must stay below `max_tokens`, which only binds when the caller's
  // output budget is itself near the minimum.
  if (budget >= maxTokens) body.max_tokens = budget + 1;
  return { type: "enabled", budget_tokens: budget };
}

/**
 * Rewrite adaptive thinking into an explicit budget.
 *
 * `@ai-sdk/anthropic` selects `thinking: {type:"adaptive"}` from a model-name table,
 * so every recent Claude name gets it. Relay endpoints answer `200` and drop the
 * field, and the turn comes back with no thinking block and zero thinking tokens —
 * indistinguishable from a model that chose not to think. `enabled` with an explicit
 * budget is the form every Anthropic-compatible endpoint implements.
 *
 * `effort` is read from the body rather than from the step request: the SDK only emits
 * adaptive thinking together with `output_config.effort`, so a body without one is
 * left alone.
 *
 * Mutates `body` in place; `true` means the caller must serialize it again.
 */
function rewriteAdaptiveThinking(body: unknown): boolean {
  if (!isObject(body)) return false;
  const thinking = body.thinking;
  if (!isObject(thinking) || thinking.type !== "adaptive") return false;
  const enabled = enabledThinkingFor(body, true);
  if (enabled === null) return false;
  body.thinking = enabled;
  return true;
}

/** Return the original text unless its thinking form needs adaptation. */
export function rewriteAdaptiveThinkingBody(text: string): string {
  // Avoid parsing bodies that cannot contain an adaptive thinking request.
  if (!text.includes("adaptive")) return text;
  let body: unknown;
  try {
    body = JSON.parse(text);
  } catch {
    // Non-JSON request bodies belong to the upstream client.
    return text;
  }
  if (!rewriteAdaptiveThinking(body)) return text;
  return JSON.stringify(body);
}

// --------------------------------------------------------- Unsigned reasoning replay

/**
 * Only original Anthropic replay payloads can authorize thinking replay. Neither a
 * relay URL nor a matching model tag proves that unsigned text is replayable.
 * Also discard the synthetic sentinel persisted by older sidecars.
 * Mutates request-private messages and preserves empty assistant turn structure.
 */
export function dropUnsignedReasoning(messages: unknown[]): void {
  for (const message of messages) {
    if (!isObject(message) || message.role !== "assistant" || !Array.isArray(message.content)) continue;
    const kept = message.content.filter((part) => {
      if (!isObject(part) || part.type !== "reasoning") return true;
      const options = part.providerOptions;
      const anthropic = isObject(options) ? options.anthropic : undefined;
      if (!isObject(anthropic)) return false;
      return (typeof anthropic.signature === "string" && anthropic.signature.length > 0
        && anthropic.signature !== "mework-unsigned")
        || (typeof anthropic.redactedData === "string" && anthropic.redactedData.length > 0);
    });
    if (kept.length === message.content.length) continue;
    if (kept.length === 0) kept.push({ type: "text", text: NO_CONTENT_PLACEHOLDER });
    message.content = kept;
  }
}

// ------------------------------------------------------------ Betas and cache breakpoints

/** Claude Code's interleaved-thinking beta; sent for every model that supports it. */
export const INTERLEAVED_THINKING_BETA = "interleaved-thinking-2025-05-14";

/** Claude Code's `avt`: every model except Claude 3 and Haiku 4.5 supports interleaved thinking. */
export function supportsInterleavedThinking(modelId: string): boolean {
  const id = modelId.toLowerCase();
  return !id.includes("claude-3") && !id.includes("haiku-4-5") && !id.includes("haiku-4.5");
}

const BETA_HEADER = "anthropic-beta";

function betasOf(headers: Headers): string[] {
  return (headers.get(BETA_HEADER) ?? "")
    .split(",")
    .map((beta) => beta.trim())
    .filter((beta) => beta.length > 0);
}

function setBetas(headers: Headers, betas: string[]): void {
  if (betas.length === 0) headers.delete(BETA_HEADER);
  else headers.set(BETA_HEADER, betas.join(","));
}

/** Block types Claude Code never marks as a cache breakpoint. */
function isStampable(block: unknown): boolean {
  if (!isObject(block)) return false;
  if (block.type === "thinking" || block.type === "redacted_thinking") return false;
  if (block.type === "text") return typeof block.text === "string" && block.text.trim().length > 0;
  return true;
}

/**
 * Claude Code's prompt-cache breakpoints: the last system block and the last
 * stampable block of the last message. Blocks that already carry a marker are
 * left alone, so caller-supplied markers stay authoritative.
 */
function addCacheBreakpoints(body: JsonObject): boolean {
  let changed = false;
  const marker = { type: "ephemeral" };
  if (typeof body.system === "string" && body.system.length > 0) {
    body.system = [{ type: "text", text: body.system, cache_control: marker }];
    changed = true;
  } else if (Array.isArray(body.system) && body.system.length > 0) {
    const last = body.system[body.system.length - 1];
    if (isObject(last) && last.cache_control === undefined && isStampable(last)) {
      last.cache_control = marker;
      changed = true;
    }
  }
  const messages = body.messages;
  if (!Array.isArray(messages) || messages.length === 0) return changed;
  const last = messages[messages.length - 1];
  if (!isObject(last)) return changed;
  if (typeof last.content === "string") {
    if (last.content.trim().length > 0) {
      last.content = [{ type: "text", text: last.content, cache_control: marker }];
      changed = true;
    }
  } else if (Array.isArray(last.content)) {
    for (let index = last.content.length - 1; index >= 0; index -= 1) {
      const block = last.content[index];
      if (!isStampable(block)) continue;
      if ((block as JsonObject).cache_control === undefined) {
        (block as JsonObject).cache_control = marker;
        changed = true;
      }
      break;
    }
  }
  return changed;
}

function stripCacheControl(value: unknown): boolean {
  let changed = false;
  if (Array.isArray(value)) {
    for (const entry of value) if (stripCacheControl(entry)) changed = true;
    return changed;
  }
  if (!isObject(value)) return false;
  if ("cache_control" in value) {
    delete value.cache_control;
    changed = true;
  }
  for (const entry of Object.values(value)) if (stripCacheControl(entry)) changed = true;
  return changed;
}

// ------------------------------------------------------------ 400 self-heal

/**
 * Repairs learned from an endpoint's 400 responses, keyed by request URL and
 * model. Claude Code keeps the same latches for the life of the process so a
 * rejected capability costs one round trip, not one per request.
 */
interface HealLatch {
  thinkingType?: "adaptive" | "enabled";
  stripThinking?: boolean;
  dropEffort?: boolean;
  dropBetas?: Set<string>;
  noCacheControl?: boolean;
  /** The model's output ceiling as the server stated it in a rejection. */
  maxTokensCap?: number;
}

const healLatches = new Map<string, HealLatch>();

/** Forget every learned repair; tests use it between discriminators. */
export function resetAnthropicHealLatches(): void {
  healLatches.clear();
}

function requestUrlOf(input: Parameters<typeof globalThis.fetch>[0]): string {
  try {
    const url = typeof input === "object" && "url" in input ? input.url : String(input);
    const parsed = new URL(url);
    parsed.hash = "";
    return parsed.toString();
  } catch {
    return "";
  }
}

function latchFor(url: string, model: unknown): HealLatch {
  const key = `${url}|${typeof model === "string" ? model : ""}`;
  let latch = healLatches.get(key);
  if (!latch) {
    latch = {};
    healLatches.set(key, latch);
  }
  return latch;
}

type HealClass =
  | "thinking-signature"
  | "thinking-type"
  | "effort"
  | "max-tokens-overflow"
  | "max-tokens-limit"
  | "beta"
  | "cache-control";

/** The lower-cased, backtick-free message of a 400 body, or the raw text. */
function errorMessageOf(text: string): string {
  let message = text;
  try {
    const parsed = JSON.parse(text);
    if (isObject(parsed)) {
      const error = parsed.error;
      if (isObject(error) && typeof error.message === "string") message = error.message;
      else if (typeof parsed.message === "string") message = parsed.message;
    }
  } catch {
    // Not JSON; classify the raw text.
  }
  return message.toLowerCase().replace(/`/g, "");
}

/** Claude Code's `EZe`: the server rejected a thinking block's signature. */
function blamesThinkingSignature(message: string): boolean {
  if (message.includes("signature in thinking block")) return true;
  if (message.includes("thinking.signature") && message.includes("field required")) return true;
  return (message.includes("thinking block") || message.includes("redacted_thinking"))
    && (message.includes("cannot be modified") || message.includes("invalid signature"));
}

/** Claude Code's `vZe`: which `thinking.type` the server rejected, if any. */
function rejectedThinkingType(message: string): "adaptive" | "enabled" | null {
  if (message.includes("adaptive thinking is not supported")) return "adaptive";
  const match = /thinking\.type[^.]*?(enabled|adaptive)[^.]*?not supported/.exec(message);
  if (match) return match[1] as "adaptive" | "enabled";
  return null;
}

/** Claude Code's `effort_unsupported` class. */
function blamesEffort(message: string): boolean {
  if (message.includes("does not support the effort parameter")) return true;
  if (message.includes("effort") && message.includes("not supported")) return true;
  return message.includes("output_config")
    && (message.includes("extra inputs are not permitted") || message.includes("requires a model that supports"));
}

/** Claude Code's `Fan`: the exact input + `max_tokens` overflow shape. */
function contextOverflow(message: string): { input: number; limit: number } | null {
  const match = /input length and max_tokens exceed context limit: (\d+) \+ (\d+) > (\d+)/.exec(message);
  if (!match) return null;
  return { input: Number(match[1]), limit: Number(match[3]) };
}

/** `max_tokens: N > M, which is the maximum allowed …` for the model. */
function maxTokensLimit(message: string): number | null {
  const match = /max_tokens: (\d+) > (\d+)/.exec(message);
  return match ? Number(match[2]) : null;
}

function rejectedBeta(message: string, betas: string[]): string | null {
  for (const beta of betas) {
    if (!message.includes(beta.toLowerCase())) continue;
    if (/not supported|unsupported|unknown|invalid|unrecognized/.test(message)) return beta;
  }
  return null;
}

function blamesCacheControl(message: string): boolean {
  return message.includes("cache_control")
    && /not supported|unsupported|unknown|invalid|extra inputs|unexpected/.test(message);
}

/** Strip every thinking block; Claude Code's `xVn`, placeholder included. */
function stripThinkingBlocks(body: JsonObject): boolean {
  const messages = body.messages;
  if (!Array.isArray(messages)) return false;
  let changed = false;
  for (const message of messages) {
    if (!isObject(message) || message.role !== "assistant" || !Array.isArray(message.content)) continue;
    const kept = message.content.filter(
      (block) => !isObject(block) || (block.type !== "thinking" && block.type !== "redacted_thinking"),
    );
    if (kept.length === message.content.length) continue;
    const substantive = kept.filter(
      (block) => !isObject(block) || block.type !== "text" || (typeof block.text === "string" && block.text.trim().length > 0),
    );
    if (substantive.length === 0) substantive.push({ type: "text", text: "[Thinking removed]" });
    message.content = substantive;
    changed = true;
  }
  return changed;
}

/** Apply one thinking form to the body; `null` when the body cannot express it. */
function applyThinkingType(body: JsonObject, type: "adaptive" | "enabled"): boolean {
  const thinking = body.thinking;
  if (!isObject(thinking) || thinking.type === type) return false;
  if (thinking.type !== "adaptive" && thinking.type !== "enabled") return false;
  if (type === "adaptive") {
    body.thinking = { type: "adaptive", display: "summarized" };
    return true;
  }
  const enabled = enabledThinkingFor(body, false);
  if (enabled === null) return false;
  body.thinking = enabled;
  return true;
}

function dropEffort(body: JsonObject): boolean {
  const config = body.output_config;
  if (!isObject(config) || !("effort" in config)) return false;
  delete config.effort;
  if (Object.keys(config).length === 0) delete body.output_config;
  return true;
}

/** Keep `budget_tokens` below a lowered `max_tokens`, as Claude Code's clamp does. */
function clampThinkingBudget(body: JsonObject): void {
  const thinking = body.thinking;
  const maxTokens = body.max_tokens;
  if (!isObject(thinking) || typeof thinking.budget_tokens !== "number" || typeof maxTokens !== "number") return;
  if (thinking.budget_tokens < maxTokens) return;
  thinking.budget_tokens = Math.max(MIN_THINKING_BUDGET, maxTokens - 1);
}

/** Apply the endpoint's learned repairs and Claude Code's request-time additions. */
function prepareRequest(body: JsonObject, headers: Headers, latch: HealLatch): { body: boolean; headers: boolean } {
  let bodyChanged = false;
  let headersChanged = false;
  if (latch.stripThinking && stripThinkingBlocks(body)) bodyChanged = true;
  if (latch.thinkingType && applyThinkingType(body, latch.thinkingType)) bodyChanged = true;
  if (latch.dropEffort && dropEffort(body)) bodyChanged = true;
  if (latch.maxTokensCap !== undefined && typeof body.max_tokens === "number" && body.max_tokens > latch.maxTokensCap) {
    body.max_tokens = latch.maxTokensCap;
    clampThinkingBudget(body);
    bodyChanged = true;
  }
  if (!latch.noCacheControl) {
    if (addCacheBreakpoints(body)) bodyChanged = true;
  } else if (stripCacheControl(body)) {
    bodyChanged = true;
  }
  const model = typeof body.model === "string" ? body.model : "";
  const betas = betasOf(headers);
  const wanted = betas.slice();
  if (supportsInterleavedThinking(model) && !wanted.includes(INTERLEAVED_THINKING_BETA)) {
    wanted.push(INTERLEAVED_THINKING_BETA);
  }
  const allowed = wanted.filter((beta) => !latch.dropBetas?.has(beta));
  if (allowed.join(",") !== betas.join(",")) {
    setBetas(headers, allowed);
    headersChanged = true;
  }
  return { body: bodyChanged, headers: headersChanged };
}

/**
 * Decide the one repair a 400 calls for, apply it, and say which class it was.
 * Returns `null` when the message matches no class or the body cannot change.
 */
function heal(body: JsonObject, headers: Headers, latch: HealLatch, message: string): HealClass | null {
  if (blamesThinkingSignature(message)) {
    if (!stripThinkingBlocks(body)) return null;
    latch.stripThinking = true;
    return "thinking-signature";
  }
  const rejectedType = rejectedThinkingType(message);
  if (rejectedType !== null) {
    const swapped = rejectedType === "enabled" ? "adaptive" : "enabled";
    if (!applyThinkingType(body, swapped)) return null;
    latch.thinkingType = swapped;
    return "thinking-type";
  }
  if (blamesEffort(message)) {
    if (!dropEffort(body)) return null;
    latch.dropEffort = true;
    return "effort";
  }
  const overflow = contextOverflow(message);
  if (overflow !== null) {
    const lowered = Math.max(3000, overflow.limit - overflow.input - 1000);
    if (typeof body.max_tokens !== "number" || lowered >= body.max_tokens) return null;
    body.max_tokens = lowered;
    clampThinkingBudget(body);
    return "max-tokens-overflow";
  }
  const limit = maxTokensLimit(message);
  if (limit !== null) {
    if (typeof body.max_tokens !== "number" || limit >= body.max_tokens || limit < 1) return null;
    body.max_tokens = limit;
    clampThinkingBudget(body);
    latch.maxTokensCap = limit;
    return "max-tokens-limit";
  }
  const beta = rejectedBeta(message, betasOf(headers));
  if (beta !== null) {
    (latch.dropBetas ??= new Set()).add(beta);
    setBetas(headers, betasOf(headers).filter((entry) => entry !== beta));
    return "beta";
  }
  if (blamesCacheControl(message)) {
    if (!stripCacheControl(body)) return null;
    latch.noCacheControl = true;
    return "cache-control";
  }
  return null;
}

/**
 * Claude Code's request-time additions and its 400 self-heal chain, at the
 * `fetch` layer so the SDK never sees the rejected attempt.
 *
 * Each repair class runs at most once per request, mirroring Claude Code's
 * de-duplicated recovery tokens; a 400 that matches no class is returned as-is.
 * Bodies are re-read for classification, so the un-healed response is rebuilt
 * from the captured text.
 */
export function anthropicSelfHealFetch(inner: typeof globalThis.fetch = globalThis.fetch): typeof globalThis.fetch {
  return async (input, init) => {
    const text = init && typeof init.body === "string" ? init.body : null;
    let body: unknown;
    try {
      body = text === null ? null : JSON.parse(text);
    } catch {
      body = null;
    }
    if (!isObject(body)) return inner(input, init);
    const url = requestUrlOf(input);
    const latch = latchFor(url, body.model);
    const headers = new Headers(init?.headers);
    prepareRequest(body, headers, latch);
    const applied = new Set<HealClass>();
    for (;;) {
      const response = await inner(input, { ...init, body: JSON.stringify(body), headers });
      if (response.status !== 400) return response;
      const errorText = await response.text();
      const replay = () =>
        new Response(errorText, { status: response.status, statusText: response.statusText, headers: response.headers });
      const message = errorMessageOf(errorText);
      const healed = heal(body, headers, latch, message);
      if (healed === null || applied.has(healed)) return replay();
      applied.add(healed);
    }
  };
}

/** Wraps `fetch` to normalize Anthropic request and response dialect differences.
 *
 * Non-streaming responses and responses without bodies pass through unchanged.
 * Line buffering and CRLF handling live solely in `sse.ts`.
 *
 * `baseURL` decides whether the adaptive rewrite applies; official Anthropic
 * implements adaptive thinking and must receive the SDK's thinking form unchanged.
 */
export function anthropicDialectFetch(
  baseURL: string | undefined,
  inner: typeof globalThis.fetch = globalThis.fetch,
): typeof globalThis.fetch {
  const patchRequest = isOfficialAnthropicEndpoint(baseURL) ? undefined : rewriteAdaptiveThinkingBody;
  // Innermost first: heal and decorate the request, reshape a non-streamed body
  // into a stream, and only then let the line rewriter see ordinary SSE.
  return sseDialectFetch(
    makeAnthropicLineRewriter,
    patchRequest,
    nonStreamingMessageAsSseFetch(anthropicSelfHealFetch(inner)),
  );
}
