//! Dialect adaptation for Chat Completions-compatible endpoints.
//!
//! Four distinct adaptations, each with its own precondition:
//! 1. legacy `function_call` frames are converted to `tool_calls` shape;
//! 2. a missing tool-call ID is synthesized only on the name-opening frame;
//! 3. the DeepSeek `reasoning_content` sentinel is backfilled only after
//!    same-request dialect evidence — unconditional insertion would corrupt
//!    conforming streams;
//! 4. ID-to-index accounting is stream-local so interleaved calls resolve to
//!    their own indexes — "most recently opened" indexing is incorrect.

import { sseDialectFetch } from "./sse.js";

/** Fast markers for response lines that may need normalization. */
const FUNCTION_CALL_MARKER = '"function_call"';
const TOOL_CALLS_MARKER = '"tool_calls"';
const USAGE_MARKER = '"usage"';
const DONE_SENTINEL = "[DONE]";

interface ChatDelta {
  function_call?: { name?: unknown; arguments?: unknown };
  tool_calls?: Array<{
    index?: unknown;
    id?: unknown;
    type?: unknown;
    function?: { name?: unknown; arguments?: unknown };
  }>;
}

interface ChatChunk {
  id?: unknown;
  choices?: Array<{ delta?: ChatDelta; finish_reason?: unknown }>;
}

/**
 * Per-stream state for synthesized tool-call indexes.
 *
 * Fragments with an ID reuse its assigned index. Anonymous fragments require
 * exactly one attributable call for the entire response.
 */
interface ToolCallIndexState {
  byId: Map<string, number>;
  byIndex: Map<number, { id: string | null; upstreamId: string | null; name: string | null; syntheticId: boolean; syntheticIndex: boolean }>;
  next: number;
  /** Index assigned to the most recently attributable call. */
  last: number | null;
}

/** Normalize a parsed chunk and return whether it changed. */
function normalizeChatChunk(chunk: ChatChunk, indexes: ToolCallIndexState): boolean {
  if (!Array.isArray(chunk.choices)) return false;
  const streamId = typeof chunk.id === "string" && chunk.id.length > 0 ? chunk.id : "call";
  let changed = false;
  for (const choice of chunk.choices) {
    if (typeof choice !== "object" || choice === null) continue;
    const delta = choice.delta;
    if (typeof delta === "object" && delta !== null) {
      const legacy = delta.function_call;
      if (typeof legacy === "object" && legacy !== null) {
        // Legacy responses carry one call per message. Derive its ID from the
        // stream ID so it is deterministic and cannot collide with `call_*`.
        const hasName = typeof legacy.name === "string" && legacy.name.length > 0;
        delta.tool_calls = [
          {
            index: 0,
            ...(hasName ? { id: `legacy_${streamId}`, type: "function" } : {}),
            function: {
              ...(hasName ? { name: legacy.name } : {}),
              ...(typeof legacy.arguments === "string" ? { arguments: legacy.arguments } : {}),
            },
          },
        ];
        delete delta.function_call;
        changed = true;
      }
      if (Array.isArray(delta.tool_calls)) {
        for (const entry of delta.tool_calls) {
          if (typeof entry !== "object" || entry === null) continue;
          const startsCall =
            typeof entry.function === "object"
            && entry.function !== null
            && typeof entry.function.name === "string"
            && entry.function.name.length > 0;
          const entryId = typeof entry.id === "string" && entry.id.length > 0 ? entry.id : null;
          const explicitIndex = typeof entry.index === "number";
          if (explicitIndex) {
            const slot = entry.index as number;
            const owner = indexes.byIndex.get(slot);
            const knownSlot = entryId ? indexes.byId.get(entryId) : undefined;
            if ((knownSlot !== undefined && knownSlot !== slot)
              || (entryId && owner?.id && owner.id !== entryId
                && !(owner.syntheticId && (!owner.upstreamId || owner.upstreamId === entryId)
                  && (!startsCall || owner.name === entry.function?.name)
                  && (knownSlot === undefined || knownSlot === slot)))) {
              throw new Error(`Chat tool-call index conflict at index ${slot}`);
            }
            // Preserve upstream indexes and advance the synthetic slot counter.
            indexes.next = Math.max(indexes.next, slot + 1);
            if (entryId) indexes.byId.set(entryId, slot);
            // `last` tracks the most recently attributable call, including
            // explicit continuation fragments.
            indexes.last = slot;
          } else {
            let slot: number;
            if (entryId && indexes.byId.has(entryId)) {
              slot = indexes.byId.get(entryId) ?? 0;
            } else if (startsCall) {
              slot = indexes.next;
              indexes.next += 1;
            } else {
              if (entryId || indexes.byIndex.size !== 1) {
                throw new Error("Chat tool-call identity ambiguity: continuation has no unique owner");
              }
              slot = indexes.byIndex.keys().next().value as number;
            }
            entry.index = slot;
            if (entryId) indexes.byId.set(entryId, slot);
            indexes.last = slot;
            changed = true;
          }
          const slot = entry.index as number;
          let owner = indexes.byIndex.get(slot);
          if (!owner) {
            owner = { id: null, upstreamId: null, name: null, syntheticId: false, syntheticIndex: !explicitIndex };
            indexes.byIndex.set(slot, owner);
          }
          if (entryId) owner.upstreamId ??= entryId;
          if (startsCall) owner.name ??= entry.function?.name as string;
          // Publish one stable identity, even when the upstream supplies its ID
          // late, repeats the name, or changes the enclosing chunk ID.
          if (!owner.id && (startsCall || entryId)) {
            owner.id = entryId ?? `synth_${streamId}_${slot}`;
            owner.syntheticId = !entryId;
            indexes.byId.set(owner.id, slot);
          }
          if (owner.id && entry.id !== owner.id) {
            entry.id = owner.id;
            changed = true;
          }
        }
      }
    }
    if (choice.finish_reason === "function_call") {
      choice.finish_reason = "tool_calls";
      changed = true;
    }
  }
  return changed;
}

/**
 * Give a usage-only terminating chunk the `choices` array the SDK requires.
 *
 * `chunkBaseSchema` declares `choices` as a plain required array, so a relay
 * that closes a successful stream with `{usage:{…}}` and no `choices` turns a
 * finished answer into a hard validation error. An empty array carries no
 * content, so the usage still lands and nothing else changes.
 */
function fillMissingChoices(frame: Record<string, unknown>): boolean {
  if (Array.isArray(frame.choices)) return false;
  if (frame.usage == null) return false;
  frame.choices = [];
  return true;
}

/**
 * Create a line rewriter for one response stream; index state is stream-local.
 */
export function makeChatLineRewriter(): (line: string) => string {
  const indexes: ToolCallIndexState = { byId: new Map(), byIndex: new Map(), next: 0, last: null };
  return (line: string): string => {
    if (!line.startsWith("data:")) return line;
    const payload = line.slice("data:".length).trim();
    if (payload.length === 0) return line;
    // Relays vary the spacing around the terminator. The SDK's stream parser
    // matches one canonical form and hands anything else to `JSON.parse`, which
    // fails the whole turn after the answer already arrived.
    if (payload === DONE_SENTINEL) return line === `data: ${DONE_SENTINEL}` ? line : `data: ${DONE_SENTINEL}`;
    if (
      !line.includes(FUNCTION_CALL_MARKER)
      && !line.includes(TOOL_CALLS_MARKER)
      && !line.includes(USAGE_MARKER)
    ) {
      return line;
    }
    let frame: unknown;
    try {
      frame = JSON.parse(payload);
    } catch {
      return line;
    }
    if (typeof frame !== "object" || frame === null) return line;
    let changed = fillMissingChoices(frame as Record<string, unknown>);
    changed = normalizeChatChunk(frame as ChatChunk, indexes) || changed;
    if (!changed) return line;
    return `data: ${JSON.stringify(frame)}`;
  };
}

/** Stateless test entry point for normalizing one SSE line. */
export function normalizeChatSseLine(line: string): string {
  return makeChatLineRewriter()(line);
}

/**
 * Add missing `reasoning_content` sentinels to compatible request bodies.
 *
 * A request is adapted only when another message already supplies the field,
 * preserving byte-for-byte behavior for other endpoint dialects.
 */
export function backfillReasoningContentFromBody(text: string): string {
  if (!text.includes('"reasoning_content"') || !text.includes(TOOL_CALLS_MARKER)) return text;
  let body: unknown;
  try {
    body = JSON.parse(text);
  } catch {
    return text;
  }
  if (typeof body !== "object" || body === null) return text;
  const messages = (body as { messages?: unknown }).messages;
  if (!Array.isArray(messages)) return text;
  let changed = false;
  for (const message of messages) {
    if (typeof message !== "object" || message === null) continue;
    const entry = message as Record<string, unknown>;
    if (entry.role !== "assistant") continue;
    if (!Array.isArray(entry.tool_calls)) continue;
    if ("reasoning_content" in entry) continue;
    entry.reasoning_content = "";
    changed = true;
  }
  if (!changed) return text;
  return JSON.stringify(body);
}

/**
 * Request URLs already observed to reject `stream_options`. Remembering them
 * keeps the cost of the accommodation at one wasted round trip per endpoint per
 * process rather than one per turn.
 *
 * Keyed by the full URL rather than the origin: a gateway routes several
 * upstreams under one host, and one strict route must not silently cost every
 * sibling route its usage block.
 */
const streamOptionsRejectors = new Set<string>();

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

function withoutStreamOptions(body: string): string | null {
  if (!body.includes('"stream_options"')) return null;
  let parsed: unknown;
  try {
    parsed = JSON.parse(body);
  } catch {
    return null;
  }
  if (typeof parsed !== "object" || parsed === null) return null;
  if (!("stream_options" in parsed)) return null;
  delete (parsed as Record<string, unknown>).stream_options;
  return JSON.stringify(parsed);
}

/** Whether a rejection names the field, rather than being an unrelated 4xx. */
function blamesStreamOptions(status: number, body: string): boolean {
  if (status !== 400 && status !== 422) return false;
  return body.includes("stream_options");
}

/**
 * Retry once without `stream_options` when the upstream rejects that field.
 *
 * `includeUsage` asks for the final usage block that strict Chat Completions
 * endpoints require, but several OpenAI-shaped upstreams reject unknown request
 * fields outright — Databricks AI Gateway answers `400 json: unknown field
 * "stream_options"`, and Mistral and Azure AI Foundry serverless answer `422`.
 * Without this the whole turn fails permanently, so the accommodation is worth
 * one extra round trip; losing the usage block only degrades token accounting.
 *
 * The retry fires only when the upstream named the field, so an ordinary `400`
 * is still surfaced as itself.
 */
function retryWithoutStreamOptionsFetch(
  inner: typeof globalThis.fetch,
): typeof globalThis.fetch {
  return async (input, init) => {
    const requestUrl = requestUrlOf(input);
    const body = init && typeof init.body === "string" ? init.body : null;
    if (body && requestUrl && streamOptionsRejectors.has(requestUrl)) {
      const stripped = withoutStreamOptions(body);
      if (stripped !== null) return inner(input, { ...init, body: stripped });
    }
    const response = await inner(input, init);
    if (!body || response.ok || response.status >= 500) return response;

    // Reading the body consumes it, so rebuild an equivalent response for the
    // path where no retry happens.
    const text = await response.text();
    const replay = () =>
      new Response(text, {
        status: response.status,
        statusText: response.statusText,
        headers: response.headers,
      });
    if (!blamesStreamOptions(response.status, text)) return replay();
    const stripped = withoutStreamOptions(body);
    if (stripped === null) return replay();
    if (requestUrl) streamOptionsRejectors.add(requestUrl);
    return inner(input, { ...init, body: stripped });
  };
}

/**
 * SSE frame carrying one Chat Completions choice, derived from a whole message.
 *
 * `finish_reason` is copied, never invented: synthesizing `stop` for a body that
 * omits it would report a truncated or malformed response as a clean completion.
 * The caller refuses to convert such a body at all.
 */
function chunkFromMessage(source: Record<string, unknown>, choice: Record<string, unknown>): string {
  const message = (choice.message ?? {}) as Record<string, unknown>;
  const delta: Record<string, unknown> = { role: "assistant" };
  if (typeof message.content === "string") delta.content = message.content;
  if (typeof message.reasoning_content === "string") delta.reasoning_content = message.reasoning_content;
  if (typeof message.reasoning === "string") delta.reasoning = message.reasoning;
  if (Array.isArray(message.tool_calls)) delta.tool_calls = message.tool_calls;
  return JSON.stringify({
    id: source.id,
    object: "chat.completion.chunk",
    created: source.created,
    model: source.model,
    choices: [{ index: 0, delta, finish_reason: choice.finish_reason }],
    ...(source.usage != null ? { usage: source.usage } : {}),
  });
}

/**
 * Replay a non-streamed Chat Completions body as an SSE stream.
 *
 * Execution is stream-only, so an upstream that answers `stream: true` with one
 * ordinary JSON object fails the turn with "Response stream ended without a
 * finish reason". Relays fronting a batch backend do exactly this.
 *
 * Only responses to a request that actually asked to stream are converted, so a
 * genuinely non-streaming round trip is untouched.
 */
function nonStreamingBodyAsSseFetch(inner: typeof globalThis.fetch): typeof globalThis.fetch {
  return async (input, init) => {
    const body = init && typeof init.body === "string" ? init.body : null;
    const response = await inner(input, init);
    if (!body || !response.ok || !response.body) return response;
    const contentType = response.headers.get("content-type") ?? "";
    if (contentType.includes("text/event-stream")) return response;
    if (!contentType.includes("json")) return response;
    let request: unknown;
    try {
      request = JSON.parse(body);
    } catch {
      return response;
    }
    if (typeof request !== "object" || request === null) return response;
    if ((request as { stream?: unknown }).stream !== true) return response;

    const text = await response.text();
    const replay = () =>
      new Response(text, {
        status: response.status,
        statusText: response.statusText,
        headers: response.headers,
      });
    let payload: unknown;
    try {
      payload = JSON.parse(text);
    } catch {
      return replay();
    }
    if (typeof payload !== "object" || payload === null) return replay();
    const source = payload as Record<string, unknown>;
    const choices = source.choices;
    if (!Array.isArray(choices) || choices.length === 0) return replay();
    // Only the first choice, matching what the SDK reads from a real stream.
    // Requests never ask for `n > 1`, and emitting sibling choices as extra
    // frames would interleave two answers into one.
    const choice = choices[0];
    if (typeof choice !== "object" || choice === null) return replay();
    // A body with no terminal reason is malformed. Replaying it unchanged lets
    // the SDK report that, instead of dressing a truncated answer up as `stop`.
    if (typeof (choice as Record<string, unknown>).finish_reason !== "string") return replay();

    const frames = [
      `data: ${chunkFromMessage(source, choice as Record<string, unknown>)}\n\n`,
      `data: ${DONE_SENTINEL}\n\n`,
    ];

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

/** Shared dialect fetch for openai-chat and openai-compatible endpoints. */
export function chatDialectFetch(inner: typeof globalThis.fetch = globalThis.fetch): typeof globalThis.fetch {
  // Innermost first: retry the request, then reshape a non-streamed body into a
  // stream, and only then let the line rewriter see ordinary SSE.
  return sseDialectFetch(
    makeChatLineRewriter,
    backfillReasoningContentFromBody,
    nonStreamingBodyAsSseFetch(retryWithoutStreamOptionsFetch(inner)),
  );
}
