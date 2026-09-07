//! AI SDK sidecar main loop.
//!
//! The host sends one `step`; the sidecar runs one `streamText`, translates its
//! `fullStream` to host events, and sends `done`. Tools have declarations but no
//! `execute`, so the SDK stops at a tool call and Rust retains execution, approval,
//! hooks, and next-round decisions.
//!
//! The sidecar does not retry, read environment variables, access the filesystem, or
//! drive multiple steps unless the host explicitly sets `maxSteps > 1`. The one
//! exception is the `claude-agent` family (`claude-agent.ts`), which spawns the
//! locally installed Claude Code executable and therefore checks that it exists and
//! composes that process's environment.

import { APICallError } from "@ai-sdk/provider";
import { jsonSchema, stepCountIs, streamText, tool, type ModelMessage, type ToolSet } from "ai";
import { Agent, setGlobalDispatcher } from "undici";

import {
  HEARTBEAT_INTERVAL_MS,
  MAX_STREAM_TEXT,
  MAX_TOOL_ARGUMENTS,
  PROTOCOL_VERSION,
  ProtocolError,
  createLineSplitter,
  decodeHostFrame,
  fullSystemPrompt,
  type HostFrame,
  type OutboundFrame,
  type StepEvent,
  type StepRequest,
  type StepResult,
  type Usage,
} from "./protocol.js";
import { redactError, redactSecrets, secretsOf } from "./error-redaction.js";
import { resolveModel } from "./providers.js";
import {
  dropForeignSignedReasoning,
  dropUnsignedReasoning,
  stripReplayTags,
} from "./anthropic-dialect.js";
import { nativeSearchTool } from "./search.js";
import { createClaudeAgentRuntime } from "./claude-agent.js";

// ---------------------------------------------------------------- stdout integrity
//
// Dependency `console.log` output would corrupt the protocol stream. Redirect every
// console method to stderr before doing anything else.

const log = (...args: unknown[]) => {
  process.stderr.write(`${args.map((a) => (typeof a === "string" ? a : JSON.stringify(a))).join(" ")}\n`);
};
console.log = log;
console.info = log;
console.warn = log;
console.debug = log;
console.error = log;

// ---------------------------------------------------------------- HTTP idle policy
//
// Do not treat silence as failure. Encrypted reasoning can emit no frames during a
// healthy long thought, indistinguishable from a stalled upstream on an idle timer.
// Disable undici `headersTimeout` and `bodyTimeout`; retain its connection timeout,
// which reports an explicit failure when a connection cannot be established.
//
// A global dispatcher covers AI SDK requests, native search, and credential-token
// requests because npm and Node undici share `undici.globalDispatcher.1`.
setGlobalDispatcher(
  new Agent({
    headersTimeout: 0,
    bodyTimeout: 0,
  }),
);

// ---------------------------------------------------------------- outbound frames with backpressure

let seq = 0;
let writeChain: Promise<void> = Promise.resolve();

function writeFrame(frame: OutboundFrame): void {
  const line = `${JSON.stringify({ v: PROTOCOL_VERSION, seq: seq++, ...frame })}\n`;
  // Chain writes because stdout accepts writes after `false` but queues them
  // unboundedly. Awaiting `drain` provides real backpressure.
  writeChain = writeChain.then(
    () =>
      new Promise<void>((resolve) => {
        if (process.stdout.write(line)) {
          resolve();
          return;
        }
        process.stdout.once("drain", resolve);
      }),
  );
}

function emit(id: string, event: StepEvent): void {
  writeFrame({ type: "event", id, event });
}

// ---------------------------------------------------------------- claude-agent family
//
// Not an AI SDK dialect: `claude-agent.ts` drives the Claude Code executable and
// owns its parked sessions. It shares this file's frame writer and in-flight
// bookkeeping so `cancel`, `shutdown` and heartbeats behave identically.

const claudeAgent = createClaudeAgentRuntime({
  emit,
  done: (id, result) => writeFrame({ type: "done", id, result }),
  fail: (id, error) => writeFrame({ type: "error", id, error }),
  begin: (id, controller, isStreaming) => {
    inflight.set(id, controller);
    if (isStreaming) {
      streaming.add(id);
      startHeartbeat();
    }
  },
  end: (id) => {
    inflight.delete(id);
    streaming.delete(id);
    stopHeartbeatIfIdle();
  },
});

// ---------------------------------------------------------------- in-flight requests
//
// `inflight` holds every cancellable request, giving `cancel` and `shutdown` one
// path. Heartbeats target only streaming steps.

const inflight = new Map<string, AbortController>();
const streaming = new Set<string>();
let heartbeat: NodeJS.Timeout | undefined;

/** Maximum retained sources per step. More results are a data dump, not citations. */
const MAX_SOURCES = 64;

function startHeartbeat(): void {
  if (heartbeat) return;
  heartbeat = setInterval(() => {
    for (const id of streaming) emit(id, { k: "heartbeat" });
  }, HEARTBEAT_INTERVAL_MS);
  // The heartbeat must not keep the process alive after the host closes stdin.
  heartbeat.unref();
}

function stopHeartbeatIfIdle(): void {
  if (streaming.size === 0 && heartbeat) {
    clearInterval(heartbeat);
    heartbeat = undefined;
  }
}

// ---------------------------------------------------------------- failure classification
//
// The host retry loop uses this classification. Prefer duck typing to `instanceof`:
// duplicate `@ai-sdk/provider` installations make `instanceof APICallError` false.

function retryAfterMs(headers: Record<string, string> | undefined): number | undefined {
  const raw = Object.entries(headers ?? {}).find(([name]) => name.toLowerCase() === "retry-after")?.[1]?.trim();
  if (!raw) return undefined;
  // Do not let Date.parse interpret invalid numeric hints as calendar years.
  const value = /^\d+(?:\.\d+)?$/.test(raw)
    ? Number(raw) * 1000
    : /[a-z]/i.test(raw) ? Date.parse(raw) - Date.now() : NaN;
  return Number.isFinite(value) && value <= Number.MAX_SAFE_INTEGER
    ? Math.max(0, Math.ceil(value)) : undefined;
}

function classify(error: unknown): import("./protocol.js").StepError {
  if (isAbort(error)) {
    return { kind: "cancelled", message: "请求已取消" };
  }
  const candidate = error as { name?: string; isRetryable?: boolean; statusCode?: number; message?: string; responseHeaders?: Record<string, string> };
  // Stream errors arrive after HTTP 200. Some dialects provide only `type` or `code`,
  // so `StreamProviderError` cannot infer retryability from a status code. Treat the
  // known transient code/type forms as transient; validation and auth failures remain
  // permanent.
  if (candidate?.name === "AI_StreamProviderError" && candidate.isRetryable !== true) {
    const stream = error as { code?: unknown; type?: unknown };
    const facets = [stream.code, stream.type]
      .filter((value): value is string => typeof value === "string")
      .join(" ");
    if (RETRYABLE_STREAM_ERROR_PATTERN.test(facets)) {
      return {
        kind: "transient",
        message: candidate.message ?? "上游在流中报告了临时故障",
        status: candidate.statusCode,
      };
    }
  }
  const looksLikeApiCallError =
    APICallError.isInstance?.(error) === true ||
    candidate?.name === "AI_APICallError" ||
    typeof candidate?.isRetryable === "boolean";
  if (looksLikeApiCallError) {
    return {
      kind: candidate.isRetryable ? "transient" : "permanent",
      message: candidate.message ?? "上游 API 调用失败",
      status: candidate.statusCode,
      retryAfterMs: retryAfterMs(candidate.responseHeaders),
    };
  }
  // Network failures such as resets, DNS failures, and timeouts are not always
  // wrapped as `APICallError`; they are normally retryable.
  //
  // `fetch` wraps undici failures in `TypeError: fetch failed`; the real retryable
  // code is on `error.cause`, not the top-level error.
  const code = retryableCauseCode(error);
  if (code) {
    return { kind: "transient", message: `${code}: ${candidate?.message ?? "网络错误"}` };
  }
  return { kind: "permanent", message: candidate?.message ?? String(error) };
}

/** Retryable stream `code` and `type` variants. Rate limits, overload, server errors,
 * and timeouts are retryable; `invalid_*`, `*_not_found`, and auth failures are not.
 */
const RETRYABLE_STREAM_ERROR_PATTERN = /rate.?limit|overload|server_error|service_unavailable|timeout|temporar|too_many|try.?again/i;

/** Retryable network error codes. `UND_ERR_*` belongs to undici; the rest belong to libuv. */
const RETRYABLE_ERROR_CODES = new Set([
  "ECONNRESET",
  "ECONNREFUSED",
  "ETIMEDOUT",
  "EPIPE",
  "ENOTFOUND",
  "EAI_AGAIN",
  "UND_ERR_SOCKET",
  "UND_ERR_CONNECT_TIMEOUT",
  "UND_ERR_HEADERS_TIMEOUT",
  "UND_ERR_BODY_TIMEOUT",
  "UND_ERR_RES_CONTENT_LENGTH_MISMATCH",
]);

/**
 * Finds the first retryable error code in the `cause` chain.
 *
 * Bound traversal because `cause` is arbitrary data and can contain a cycle.
 */
function retryableCauseCode(error: unknown): string | undefined {
  let current = error;
  for (let depth = 0; current != null && depth < 8; depth += 1) {
    const code = (current as { code?: unknown }).code;
    if (typeof code === "string" && RETRYABLE_ERROR_CODES.has(code)) return code;
    current = (current as { cause?: unknown }).cause;
  }
  return undefined;
}

function isAbort(error: unknown): boolean {
  const name = (error as { name?: string })?.name;
  return name === "AbortError" || name === "TimeoutError" || name === "ResponseAborted";
}

/**
 * Converts a server-side tool failure object into readable bounded text.
 *
 * Anthropic uses `{type:"web_search_tool_result_error", errorCode:"…"}`; unknown
 * shapes fall back to bounded JSON because this text enters the host result envelope.
 */
function describeProviderToolError(error: unknown): string {
  if (typeof error === "string") return error.slice(0, 500);
  if (typeof error === "object" && error !== null) {
    const code = (error as { errorCode?: unknown }).errorCode;
    if (typeof code === "string") return code;
    try {
      return JSON.stringify(error).slice(0, 500);
    } catch {
      return String(error).slice(0, 500);
    }
  }
  return String(error).slice(0, 500);
}

// ---------------------------------------------------------------- one step

/**
 * Claude Code's default `max_tokens` for the Messages protocol.
 *
 * The SDK would otherwise send the model's whole output ceiling (128k for current
 * models), and Anthropic counts `max_tokens` against the context window: a long
 * conversation then fails with "input length and max_tokens exceed context
 * limit" long before the window is full. 32k is what Claude Code sends unless
 * the user overrides it; the SDK still caps it at a known model's ceiling.
 */
const ANTHROPIC_DEFAULT_OUTPUT_TOKENS = 32_000;

function defaultOutputTokens(family: StepRequest["family"]): number | undefined {
  return family === "anthropic" || family === "bedrock" ? ANTHROPIC_DEFAULT_OUTPUT_TOKENS : undefined;
}

function normalizeUsage(usage: {
  inputTokens?: number;
  outputTokens?: number;
  totalTokens?: number;
  inputTokenDetails?: { cacheReadTokens?: number; cacheWriteTokens?: number };
  outputTokenDetails?: { reasoningTokens?: number };
}): Usage {
  return {
    inputTokens: usage.inputTokens,
    outputTokens: usage.outputTokens,
    totalTokens: usage.totalTokens,
    reasoningTokens: usage.outputTokenDetails?.reasoningTokens,
    cacheReadTokens: usage.inputTokenDetails?.cacheReadTokens,
    cacheWriteTokens: usage.inputTokenDetails?.cacheWriteTokens,
  };
}

/** Metadata keys under which providers carry an encrypted reasoning payload. */
const ENCRYPTED_REASONING_KEYS = ["redactedData", "signature"] as const;

/**
 * Whether provider metadata carries an encrypted reasoning payload.
 *
 * Scan metadata by value rather than by provider name because providers label their
 * bucket differently. An empty string is upstream scaffolding rather than a payload:
 * Anthropic opens every thinking block with an empty signature before filling it.
 */
function hasEncryptedReasoning(metadata: Record<string, unknown> | undefined): boolean {
  if (metadata == null) return false;
  return Object.values(metadata).some((entry) => {
    if (typeof entry !== "object" || entry === null) return false;
    return ENCRYPTED_REASONING_KEYS.some((key) => {
      const value = (entry as Record<string, unknown>)[key];
      return typeof value === "string" ? value.length > 0 : value != null;
    });
  });
}

function buildTools(request: StepRequest, provider: unknown): ToolSet {
  const tools: ToolSet = {};
  for (const spec of request.tools ?? []) {
    // `inputSchema` is authoritative host data from builtin schemas and descriptors;
    // do not rederive it in the sidecar.
    tools[spec.name] = tool({
      description: spec.description,
      inputSchema: jsonSchema(spec.inputSchema as Parameters<typeof jsonSchema>[0]),
      // No `execute`: the host owns the tool loop.
    });
  }
  if (request.nativeSearch) {
    const native = nativeSearchTool(request.family, provider, request.nativeSearch.maxUses);
    if (native) Object.assign(tools, native);
  }
  return tools;
}

async function runStep(id: string, request: StepRequest): Promise<void> {
  const controller = new AbortController();
  inflight.set(id, controller);
  streaming.add(id);
  startHeartbeat();

  const clientToolNames = new Set((request.tools ?? []).map((spec) => spec.name));
  let text = "";
  const filterSearchPhases = request.nativeSearch != null
    && ["openai-responses", "openai-codex", "azure"].includes(request.family);
  let textStep = 0;
  const textItems = new Map<string, { text: string; phase?: string }>();
  const textItem = (itemId: string) => {
    const key = `${textStep}:${itemId}`;
    let item = textItems.get(key);
    if (!item) {
      item = { text: "" };
      textItems.set(key, item);
    }
    return item;
  };
  const reasoning: string[] = [];
  // Bucket reasoning by AI SDK part ID. In Responses `store:false`, multiple items can
  // interleave; one buffer would merge their text. Ordinals are assigned only when an
  // item passes the evidence gate, making them stable downstream identities.
  //
  // Keep item state for the entire step and emit settled reasoning by ordinal rather
  // than end-arrival order, which may be out of order.
  interface ReasoningItemState {
    ordinal: number | null;
    text: string;
    emitted: boolean;
    open: boolean;
    /** An encrypted payload — a signature or redacted data — arrived for this item. */
    encrypted: boolean;
    redacted?: boolean;
    /** A plaintext-channel delta arrived, even an empty one. See the evidence gate. */
    sawTextFrame: boolean;
  }
  const reasoningItems = new Map<string, ReasoningItemState>();
  let reasoningOrdinals = 0;
  const reasoningItem = (key: string): ReasoningItemState => {
    let state = reasoningItems.get(key);
    if (!state) {
      state = { ordinal: null, text: "", emitted: false, open: false, encrypted: false, sawTextFrame: false };
      reasoningItems.set(key, state);
    }
    return state;
  };
  // Measure reasoning duration as the union of open item intervals. Start when the
  // first item opens and close only when the last closes; include an interval only if
  // at least one item passed the evidence gate. Closing at any end would omit the
  // tail of still-open interleaved items.
  //
  // `candidate` runs the same clock for items that carry an encrypted payload but have
  // not passed the gate, because the deciding evidence for those — the provider's
  // reasoning-token count — only arrives at the end of the stream.
  let reasoningOpenedAt: number | null = null;
  let openReasoningItems = 0;
  let reasoningIntervalEmitted = false;
  let reasoningIntervalCandidate = false;
  let reasoningMs = 0;
  let candidateReasoningMs = 0;
  const closeReasoningInterval = (): void => {
    if (reasoningOpenedAt === null) return;
    const elapsed = Date.now() - reasoningOpenedAt;
    reasoningOpenedAt = null;
    if (reasoningIntervalEmitted) reasoningMs += elapsed;
    if (reasoningIntervalCandidate) candidateReasoningMs += elapsed;
    reasoningIntervalEmitted = false;
    reasoningIntervalCandidate = false;
  };
  const openReasoningInterval = (state: ReasoningItemState): void => {
    if (!state.open) {
      state.open = true;
      openReasoningItems += 1;
      if (reasoningOpenedAt === null) reasoningOpenedAt = Date.now();
    }
    if (state.emitted) reasoningIntervalEmitted = true;
    if (state.emitted || state.encrypted) reasoningIntervalCandidate = true;
  };
  // `reasoningSeen` means that a reasoning card should exist. Do not substitute
  // `reasoningMs > 0`: sub-millisecond reasoning still needs a card.
  let reasoningSeen = false;
  // Evidence gate for the reasoning lifecycle.
  //
  // Encrypted reasoning emits eagerly because its start may be the sole evidence.
  // Other models delay lifecycle emission until a non-empty text delta or `redactedData`
  // proves the item has content. This suppresses empty provider scaffolding without
  // adding empty cards or duration.
  const reasoningEager = request.reasoningContent === "encrypted";
  const emitReasoningStart = (state: ReasoningItemState): void => {
    state.emitted = true;
    state.ordinal = reasoningOrdinals;
    reasoningOrdinals += 1;
    reasoningSeen = true;
    reasoningIntervalEmitted = true;
    reasoningIntervalCandidate = true;
    emit(id, { k: "reasoning-start", item: state.ordinal,
      form: state.redacted ? "encrypted" : state.text.length > 0 && !reasoningEager ? "plaintext" : undefined });
  };
  const calls: StepResult["calls"] = [];
  const sources: StepResult["sources"] = [];
  const sourceKeys = new Set<string>();
  const addSource = (source: { sourceType: "url" | "document"; id: string; url?: string; title?: string }) => {
    const bounded = { id: source.id.slice(0, 256), url: source.url?.slice(0, 2048), title: source.title?.slice(0, 512) };
    const key = bounded.url ?? `document:${bounded.id}`;
    if (sources.length >= MAX_SOURCES || sourceKeys.has(key)) return;
    sourceKeys.add(key);
    sources.push(bounded);
    emit(id, { k: "source", sourceType: source.sourceType, ...bounded });
  };
  const addSearchUrl = (value: unknown) => {
    if (typeof value !== "string" || value.length > 2048) return;
    try {
      const url = new URL(value);
      if (url.protocol !== "http:" && url.protocol !== "https:") return;
      // A retrieved page is a source, not a claim that the answer cited it.
      // No invented title; the URL itself supplies stable identity.
      addSource({ sourceType: "url", id: `search:${value}`, url: value });
    } catch { /* Provider output is untrusted; malformed URLs are not sources. */ }
  };
  const providerToolErrors: NonNullable<StepResult["providerToolErrors"]> = [];
  const announced = new Set<string>();
  let usage: Usage = {};
  let finishReason: string | undefined;
  let rawFinishReason: string | undefined;

  try {
    const { model, provider } = resolveModel(request);
    // History replays the provider's own signed reasoning parts, each tagged by
    // the host with the model that produced it. Anthropic binds a signature to
    // that model, so a switched conversation drops them the way Claude Code
    // does; the tag itself never reaches a provider.
    if (request.family === "anthropic" || request.family === "bedrock") {
      dropForeignSignedReasoning(request.messages, request.modelId);
    }
    stripReplayTags(request.messages);
    // Replay only original Anthropic payloads; never manufacture a signature from
    // the endpoint URL or the absence of signed history. Preserve empty turns.
    if (request.family === "anthropic") {
      dropUnsignedReasoning(request.messages);
    }
    const tools = buildTools(request, provider);
    const priorSearchIds = new Set<string>(request.nativeSearch?.previousCallIds ?? []);
    const searchIds = new Set<string>();
    const isSearchCall = (call: { toolName?: unknown; providerExecuted?: unknown; toolCallId?: unknown }) =>
      call.providerExecuted === true && ["web_search", "google_search"].includes(String(call.toolName))
        && typeof call.toolCallId === "string";
    for (const message of request.messages as ModelMessage[]) {
      if (!Array.isArray(message.content)) continue;
      for (const part of message.content) {
        if (part.type === "tool-call" && isSearchCall(part)) priorSearchIds.add(part.toolCallId);
      }
    }
    const countSearch = (call: { toolName?: unknown; providerExecuted?: unknown; toolCallId?: unknown }) => {
      if (request.nativeSearch && isSearchCall(call) && !priorSearchIds.has(call.toolCallId as string)) {
        searchIds.add(call.toolCallId as string);
      }
    };
    const searchLimit = request.nativeSearch?.maxUses ?? 0;
    const result = streamText({
      model,
      system: fullSystemPrompt(request),
      messages: request.messages as ModelMessage[],
      tools,
      // prepareStep runs before tool serialization. Replace the provider tool in
      // this request-local ToolSet, not in the shared provider or SDK internals.
      prepareStep: request.nativeSearch ? ({ steps }) => {
        for (const step of steps) for (const call of step.toolCalls) countSearch(call);
        if (searchLimit === 0) return;
        const remaining = Math.max(0, searchLimit - searchIds.size);
        if (remaining === 0) return { activeTools: [], toolChoice: "none" as const };
        Object.assign(tools, nativeSearchTool(request.family, provider, remaining));
        return undefined;
      } : undefined,
      toolChoice: request.toolChoice,
      // Never begin another SDK request after consuming the finite call budget.
      stopWhen: [stepCountIs(Math.max(1, request.maxSteps)), ({ steps }) => {
        for (const step of steps) for (const call of step.toolCalls) countSearch(call);
        return searchLimit > 0 && searchIds.size >= searchLimit;
      }],
      maxOutputTokens: request.maxOutputTokens ?? defaultOutputTokens(request.family),
      temperature: request.temperature,
      // Providers translate the shared reasoning profile to family-specific controls.
      reasoning: request.reasoning,
      providerOptions: request.providerOptions,
      // Retrying is a host policy; retries on both sides multiply billable calls.
      maxRetries: 0,
      abortSignal: controller.signal,
      // The default `streamText` handler logs `APICallError.requestBodyValues`, which
      // contains the complete request body. The stream's `error` part reaches the loop
      // below, so suppress the default dump to keep prompts and tool arguments out of logs.
      onError: () => {},
    });

    for await (const part of result.fullStream) {
      switch (part.type) {
        case "start-step":
          textStep += 1;
          break;
        case "text-start":
        case "text-end": {
          if (!filterSearchPhases) break;
          const item = textItem(part.id);
          const metadata = part.providerMetadata;
          const phase = metadata?.openai?.phase ?? metadata?.azure?.phase;
          // The final item can carry phase even when the start did not. Never
          // decide from deltas or alter the opaque continuation / live stream.
          if (typeof phase === "string") item.phase = phase;
          break;
        }
        case "text-delta": {
          if (text.length + part.text.length > MAX_STREAM_TEXT) {
            throw new Error(`单轮可见文本超过 ${MAX_STREAM_TEXT} 字节上限`);
          }
          text += part.text;
          if (filterSearchPhases) textItem(part.id).text += part.text;
          emit(id, { k: "text-delta", delta: part.text });
          break;
        }
        case "reasoning-start": {
          // Timing starts at item start. The evidence gate controls emission, not the
          // measured interval for an item that is emitted.
          const state = reasoningItem(part.id);
          openReasoningInterval(state);
          // Redacted thinking is an alternative encrypted representation: it has no
          // delta but proves reasoning occurred.
          const metadata = (part as { providerMetadata?: Record<string, unknown> }).providerMetadata;
          if (hasEncryptedReasoning(metadata)) {
            state.redacted = Object.values(metadata ?? {}).some((entry) =>
              typeof entry === "object" && entry !== null
              && typeof (entry as Record<string, unknown>).redactedData === "string"
              && ((entry as Record<string, unknown>).redactedData as string).length > 0);
            state.encrypted = true;
            reasoningIntervalCandidate = true;
            // `redacted_thinking` is a whole encrypted item announced at its start with
            // no delta of any kind, so the start itself is the evidence.
            if (!state.emitted) emitReasoningStart(state);
          } else if (!state.emitted && reasoningEager) {
            emitReasoningStart(state);
          }
          break;
        }
        case "reasoning-delta": {
          // Some families emit deltas without starts. Treat the first delta as a start
          // so their reasoning duration is measured.
          const state = reasoningItem(part.id);
          openReasoningInterval(state);
          state.text += part.text;
          const metadata = (part as { providerMetadata?: Record<string, unknown> }).providerMetadata;
          if (hasEncryptedReasoning(metadata)) {
            state.encrypted = true;
            reasoningIntervalCandidate = true;
          } else {
            state.sawTextFrame = true;
          }
          if (part.text.length === 0) {
            // An empty delta is not reasoning text, but two different upstreams send
            // one and they must not be confused:
            //
            // - A relay in front of Claude forwards the thinking frames with their
            //   text stripped and puts the whole item in the signature. The empty
            //   plaintext frame proves a thinking channel existed, so the signature
            //   that follows is real reasoning and earns a card.
            // - DeepSeek's Anthropic-compatible endpoint opens a bare thinking block
            //   on every tool continuation whose only delta is a pseudo-signature,
            //   with no plaintext frame and no reasoning tokens. Nothing was thought
            //   and no card may appear.
            //
            // Requiring both signals separates them without inspecting the payload.
            // An item that only ever carries a signature is settled at the end of the
            // stream, where the provider's reasoning-token count decides.
            if (!state.emitted && state.encrypted && state.sawTextFrame) emitReasoningStart(state);
            break;
          }
          if (!state.emitted) emitReasoningStart(state);
          emit(id, { k: "reasoning-delta", item: state.ordinal ?? 0, delta: part.text });
          break;
        }
        case "reasoning-end": {
          const state = reasoningItem(part.id);
          if (state.open) {
            state.open = false;
            openReasoningItems -= 1;
            // Close only when the last open item ends so interleaved items retain
            // their tail duration. Intervals with no emitted items are discarded.
            if (openReasoningItems === 0) closeReasoningInterval();
          }
          if (state.emitted) {
            emit(id, { k: "reasoning-done", item: state.ordinal ?? 0, durationMs: reasoningMs });
          }
          break;
        }
        case "tool-input-start": {
          // Never report provider-executed tools to the host: it cannot execute them,
          // and turning one into a `ToolCall` rejects the turn as an unknown tool and
          // breaks `pause_turn`.
          if (part.providerExecuted === true || !clientToolNames.has(part.toolName)) break;
          if (!announced.has(part.id)) {
            announced.add(part.id);
            emit(id, { k: "tool-call-announced", callId: part.id, toolName: part.toolName });
          }
          break;
        }
        case "tool-call": {
          const call = part as unknown as {
            toolCallId: string;
            toolName: string;
            input: unknown;
            providerExecuted?: boolean;
          };
          countSearch(call);
          if (call.providerExecuted === true || !clientToolNames.has(call.toolName)) break;
          const encoded = JSON.stringify(call.input ?? {});
          if (encoded.length > MAX_TOOL_ARGUMENTS) {
            throw new Error(`工具 ${call.toolName} 的参数超过 ${MAX_TOOL_ARGUMENTS} 字节上限`);
          }
          if (!announced.has(call.toolCallId)) {
            announced.add(call.toolCallId);
            emit(id, { k: "tool-call-announced", callId: call.toolCallId, toolName: call.toolName });
          }
          calls.push({ callId: call.toolCallId, toolName: call.toolName, input: call.input });
          emit(id, { k: "tool-call", callId: call.toolCallId, toolName: call.toolName, input: call.input });
          break;
        }
        case "source": {
          addSource(part as unknown as { sourceType: "url" | "document"; id: string; url?: string; title?: string });
          break;
        }
        case "tool-result": {
          if (part.providerExecuted !== true || part.toolName !== "web_search"
            || !["openai-responses", "openai-codex", "azure"].includes(request.family)) break;
          const output = part.output as { action?: { type?: string; url?: unknown }; sources?: unknown[] } | null;
          if (!output || typeof output !== "object") break;
          if (output.action?.type === "openPage" || output.action?.type === "findInPage") {
            addSearchUrl(output.action.url);
          } else if (output.action?.type === "search" && Array.isArray(output.sources)) {
            for (const source of output.sources) {
              if (sources.length >= MAX_SOURCES) break;
              if (source && typeof source === "object" && "url" in source) addSearchUrl(source.url);
            }
          }
          break;
        }
        case "tool-error": {
          // Failed provider-executed tools arrive as this part after `ai` translates
          // an error tool result. Preserve the failure so native search does not claim
          // completion; client tools have no `execute` and should not reach this path.
          const failure = part as unknown as {
            toolCallId?: string;
            toolName?: string;
            providerExecuted?: boolean;
            error?: unknown;
          };
          if (failure.providerExecuted !== true) break;
          providerToolErrors.push({
            callId: typeof failure.toolCallId === "string" ? failure.toolCallId : "",
            toolName: typeof failure.toolName === "string" ? failure.toolName : "",
            message: redactSecrets(describeProviderToolError(failure.error), secretsOf(request)),
          });
          break;
        }
        case "finish-step": {
          usage = normalizeUsage(part.usage);
          emit(id, { k: "usage", usage });
          break;
        }
        case "finish": {
          usage = normalizeUsage(part.totalUsage);
          finishReason = part.finishReason;
          // Preserve the upstream value: `pause_turn` normalizes to `stop`, so host
          // continuation logic must inspect the raw reason.
          rawFinishReason = (part as { rawFinishReason?: string }).rawFinishReason;
          emit(id, { k: "usage", usage });
          break;
        }
        case "abort": {
          throw Object.assign(new Error("请求已取消"), { name: "AbortError" });
        }
        case "error": {
          throw part.error;
        }
        default:
          break;
      }
    }

    // Settle reasoning in ordinal order because end events can arrive out of order.
    // If a provider omits an end event, close the remaining interval at stream end;
    // items that did not pass the evidence gate remain absent.
    closeReasoningInterval();
    // Last chance for encrypted-only items. An endpoint that strips the plaintext
    // thinking frames entirely leaves the signature as the stream's only trace, and
    // the reasoning-token count the provider billed is then the deciding evidence.
    // It is only known now, which is why these items get their card at settle rather
    // than live.
    if (!reasoningSeen && (usage.reasoningTokens ?? 0) > 0) {
      const encryptedOnly = [...reasoningItems.values()].filter((state) => state.encrypted);
      if (encryptedOnly.length > 0) {
        reasoningMs = candidateReasoningMs;
        for (const state of encryptedOnly) {
          emitReasoningStart(state);
          emit(id, { k: "reasoning-done", item: state.ordinal ?? 0, durationMs: reasoningMs });
        }
      }
    }
    const orderedReasoning = [...reasoningItems.values()]
      .filter((state) => state.emitted)
      .sort(
        (left, right) =>
          (left.ordinal ?? Number.MAX_SAFE_INTEGER) - (right.ordinal ?? Number.MAX_SAFE_INTEGER),
      );
    for (const state of orderedReasoning) reasoning.push(state.text);

    if (filterSearchPhases) {
      const items = [...textItems.values()];
      const sawFinalAnswer = items.some((item) => item.phase === "final_answer");
      text = items.filter((item) => sawFinalAnswer ? item.phase === "final_answer" : item.phase !== "commentary")
        .map((item) => item.text).join("");
    }
    const response = await result.response;
    const payload: StepResult = {
      text,
      reasoning,
      // Omit the field when no reasoning item exists. The host must distinguish that
      // state from reasoning that lasted less than one millisecond.
      ...(reasoningSeen ? { reasoningMs } : {}),
      calls,
      usage,
      model: response.modelId,
      finishReason,
      ...(rawFinishReason != null ? { rawFinishReason } : {}),
      // Opaque continuation blocks preserve Anthropic encrypted content and signatures
      // and Responses reasoning items across turns. The host stores and replays them
      // without interpretation.
      responseMessages: (response as unknown as { messages?: unknown[] }).messages ?? [],
      sources,
      ...(request.nativeSearch ? { nativeSearchUses: searchIds.size, nativeSearchCallIds: [...searchIds] } : {}),
      // Server-side tool failure facts. Absence means no provider-executed tool
      // failed; native search uses this to report failures truthfully.
      ...(providerToolErrors.length > 0 ? { providerToolErrors } : {}),
    };
    writeFrame({ type: "done", id, result: payload });
  } catch (error) {
    writeFrame({ type: "error", id, error: redactError(classify(error), secretsOf(request)) });
  } finally {
    inflight.delete(id);
    streaming.delete(id);
    stopHeartbeatIfIdle();
  }
}

// ---------------------------------------------------------------- frame dispatch

function handleFrame(frame: HostFrame): void {
  switch (frame.type) {
    case "hello":
      writeFrame({
        type: "ready",
        protocol: PROTOCOL_VERSION,
        versions: { node: process.versions.node },
      });
      break;
    case "step":
      if (frame.payload.family === "claude-agent") void claudeAgent.step(frame.id, frame.payload);
      else void runStep(frame.id, frame.payload);
      break;
    case "release":
      claudeAgent.release(frame.session);
      break;
    case "cancel": {
      inflight.get(frame.id)?.abort();
      break;
    }
    case "shutdown":
      void exitAfterShutdown();
      break;
    default:
      break;
  }
}

/** Aborts in-flight requests, tears down Claude Code sessions, flushes frames, exits. */
async function exitAfterShutdown(): Promise<void> {
  for (const controller of inflight.values()) controller.abort();
  await claudeAgent.shutdown();
  // Flush queued frames before exiting.
  await writeChain;
  process.exit(0);
}

const split = createLineSplitter(
  (line) => {
    try {
      handleFrame(decodeHostFrame(line));
    } catch (error) {
      if (error instanceof ProtocolError) {
        // A protocol violation means the two sides use incompatible generations, not
        // that one request failed. Exit so the host can restart the sidecar.
        process.stderr.write(`[aisdk] 协议违例：${error.message}\n`);
        process.exit(2);
      }
      process.stderr.write(`[aisdk] 帧处理失败：${String(error)}\n`);
    }
  },
  (bytes) => {
    process.stderr.write(`[aisdk] 宿主帧超过行长上限（${bytes} 字节），已断开\n`);
    process.exit(2);
  },
);

process.stdin.on("data", split);
process.stdin.on("end", () => {
  void exitAfterShutdown();
});
process.stdin.resume();
