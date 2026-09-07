//! Sidecar-host wire protocol over stdio NDJSON.
//!
//! stdout carries protocol frames only; logs and warnings must use stderr. Each
//! frame occupies one newline-terminated line, with line limits enforced on both
//! sides. `seq` increases for the entire process so complete lost requests are
//! also detectable.

/**
 * Protocol generation. The host and sidecar must match exactly; do not negotiate
 * compatibility. Version gates turn stale sidecar artifacts into explicit startup
 * failures rather than silent loss of reasoning and pause-turn data. Any
 * wire-shape change bumps this constant and the matching host-side literal.
 */
export const PROTOCOL_VERSION = 10;

/** Maximum line size (16 MiB); mirrors the host constant `MAX_SSE_LINE`. */
export const MAX_LINE_BYTES = 16 * 1024 * 1024;

/** Maximum accumulated visible text per request (16 MiB); mirrors the host constant `MAX_STREAM_TEXT`. */
export const MAX_STREAM_TEXT = 16 * 1024 * 1024;

/** Maximum JSON argument size for one tool call (4 MiB); mirrors the host constant `MAX_STREAM_TOOL_ARGUMENTS`. */
export const MAX_TOOL_ARGUMENTS = 4 * 1024 * 1024;

/** Heartbeat interval when no events are available; the host uses it to probe cancellation. */
export const HEARTBEAT_INTERVAL_MS = 500;

// ----------------------------------------------------------------- Host → sidecar

export type HostFrame =
  | { v: number; type: "hello" }
  | { v: number; type: "step"; id: string; payload: StepRequest }
  | { v: number; type: "cancel"; id: string }
  /**
   * The host run that owned `session` ended (finish, failure, or cancel). The
   * sidecar tears down the parked Claude Code session; there is no reply frame
   * and a release for an unknown session is a no-op.
   */
  | { v: number; type: "release"; session: string }
  | { v: number; type: "shutdown" };

/** Provider adapter family. Must match Rust `ProviderFamily` value-for-value. */
export type ProviderFamily =
  | "openai-responses"
  | "openai-codex"
  | "openai-chat"
  | "anthropic"
  | "claude-agent"
  | "google"
  | "xai"
  | "azure"
  | "bedrock"
  | "vertex"
  | "openai-compatible";

/** Any JSON value carried by the protocol, structurally matching AI SDK `JSONValue`. */
export type JsonValue = null | string | number | boolean | JsonValue[] | { [key: string]: JsonValue };

export interface ToolSpec {
  name: string;
  description: string;
  /** Verbatim host schema; the sidecar must not rewrite it. */
  inputSchema: unknown;
}

/**
 * Claude Code session parameters, present only for the `claude-agent` family.
 *
 * The host mints `session` once per run and sends `release` when the run ends;
 * the sidecar keys its parked CLI session by it. `executable` is host-resolved
 * so the sidecar never searches the disk. `env` carries the profile-location
 * variables the CLI needs to find its own configuration (the sidecar itself runs
 * with a cleared environment). Authentication is the CLI's own login and nothing
 * else: `apiKey` and `baseURL` are not read for this family. The single
 * exception is a local test stub — an `ANTHROPIC_BASE_URL` in `env` is honoured
 * only when it addresses a loopback host, and an `ANTHROPIC_API_KEY` /
 * `ANTHROPIC_AUTH_TOKEN` in `env` only alongside one.
 */
export interface AgentSession {
  session: string;
  executable: string;
  cwd: string;
  env?: Record<string, string>;
}

export interface StepRequest {
  family: ProviderFamily;
  /**
   * A host-validated address. The sidecar must not validate URLs because it is
   * not authoritative. Omission selects the provider default; Vertex and
   * Bedrock derive endpoints from `settings`.
   */
  baseURL?: string;
  apiKey?: string;
  headers?: Record<string, string>;
  /** Provider-family identity fields: `region`, `project`, `location`, and `apiVersion`. */
  settings?: Record<string, string>;
  modelId: string;
  /** The stable part of the system prompt: everything assembled once at run start. */
  system?: string;
  /**
   * The per-step tail of the system prompt, which the sidecar appends to
   * `system` after a blank line for every provider. Anthropic's dialect layer
   * also uses it as Claude Code's dynamic boundary, so the stable prefix and the
   * tail become separate cache breakpoints.
   */
  systemDynamic?: string;
  /** AI SDK `ModelMessage[]` projected from the host canonical context. */
  messages: unknown[];
  tools?: ToolSpec[];
  toolChoice?: "auto" | "none" | "required" | { type: "tool"; toolName: string };
  /**
   * Allowed model-step count. Ordinary rounds are always one because the host
   * owns the tool loop; only one-shot native-search requests may exceed one to
   * advance provider-executed search and Anthropic `pause_turn` continuation.
   */
  maxSteps: number;
  maxOutputTokens?: number;
  temperature?: number;
  /**
   * Reasoning effort in the AI SDK 7 vocabulary, not a provider dialect. Each
   * provider maps it to its own controls, including unsupported-level fallback.
   * The host supplies only the level and never branches by model name.
   */
  reasoning?: "provider-default" | "none" | "minimal" | "low" | "medium" | "high" | "xhigh";
  /**
   * Reasoning representation for this model. The host resolves protocol defaults,
   * so this is determinate when present; absence means the family has no consumer.
   * `responses-dialect.ts` strips encrypted-content includes for `plaintext`.
   */
  reasoningContent?: "plaintext" | "encrypted";
  /**
   * The model's prompt-cache attribute, sent only to families whose dialect
   * places cache breakpoints (today: `anthropic`). `false` turns Claude Code's
   * breakpoints off for this model; absence means the family has no consumer.
   */
  promptCache?: boolean;
  providerOptions?: Record<string, Record<string, JsonValue>>;
  /** Native `web_search` server tool, present only on `native_search_call` requests. */
  nativeSearch?: { maxUses: number; previousCallIds?: string[] };
  /** Present only for the `claude-agent` family; see [`AgentSession`]. */
  agent?: AgentSession;
}

/** Separator between the stable system prompt and its tail; mirrors the host's. */
export const SYSTEM_SECTION_SEPARATOR = "\n\n";

/**
 * The whole system prompt a provider receives: the stable part followed by the
 * per-step tail. Byte-identical to what the host sent before the split existed,
 * so every family other than Anthropic sees no change.
 */
export function fullSystemPrompt(request: Pick<StepRequest, "system" | "systemDynamic">): string | undefined {
  const parts = [request.system, request.systemDynamic].filter(
    (part): part is string => typeof part === "string" && part.length > 0,
  );
  return parts.length > 0 ? parts.join(SYSTEM_SECTION_SEPARATOR) : request.system;
}

// ----------------------------------------------------------------- Sidecar → host

export type SidecarFrame =
  | { v: number; type: "ready"; seq: number; protocol: number; versions: Record<string, string> }
  | { v: number; type: "event"; seq: number; id: string; event: StepEvent }
  | { v: number; type: "done"; seq: number; id: string; result: StepResult }
  | { v: number; type: "error"; seq: number; id: string; error: StepError };

/**
 * Outbound frames omit fields filled by `writeFrame`.
 *
 * Do not use `Omit<SidecarFrame, "v" | "seq">`: `Omit` does not distribute
 * over unions and would erase variant-specific fields. The conditional type
 * below forces distribution.
 */
export type OutboundFrame = SidecarFrame extends infer F
  ? F extends SidecarFrame
    ? Omit<F, "v" | "seq">
    : never
  : never;

/** Stream events correspond one-for-one with Rust `ModelStreamEvent`; the host
 * forwards them without a second translation layer. */
export type StepEvent =
  | { k: "text-delta"; delta: string }
  /**
   * A provider opened a reasoning item. It is distinct from `reasoning-delta`:
   * a provider may emit encrypted reasoning without summary text. `item` is the
   * zero-based ordinal among evidence-gated items and identifies each live and
   * persisted reasoning segment.
   */
  | { k: "reasoning-start"; item: number; form?: "plaintext" | "encrypted" }
  | { k: "reasoning-delta"; item: number; delta: string }
  /**
   * Cumulative reasoning wall-clock milliseconds for the step, not this item.
   * The sidecar owns stream timing and emits the same value live and at
   * settlement, preventing divergent durations after reconnect or reload.
   */
  | { k: "reasoning-done"; item: number; durationMs?: number }
  | { k: "tool-call-announced"; callId: string; toolName: string }
  | { k: "tool-call"; callId: string; toolName: string; input: unknown }
  | { k: "usage"; usage: Usage }
  | { k: "source"; sourceType: "url" | "document"; id: string; url?: string; title?: string }
  | { k: "heartbeat" };

export interface Usage {
  inputTokens?: number;
  outputTokens?: number;
  totalTokens?: number;
  reasoningTokens?: number;
  cacheReadTokens?: number;
  cacheWriteTokens?: number;
}

export interface StepResult {
  text: string;
  reasoning: string[];
  /**
   * Cumulative reasoning wall-clock milliseconds for the step. Unlike
   * `reasoning`, this is present for encrypted reasoning with no summary text;
   * absence means the step contained no reasoning item.
   */
  reasoningMs?: number;
  /** Client tool calls only; provider-executed calls never appear here. */
  calls: Array<{ callId: string; toolName: string; input: unknown }>;
  usage: Usage;
  model?: string;
  finishReason?: string;
  /**
   * Unnormalized upstream stop reason. `pause_turn` continuation depends on it:
   * AI SDK normalizes it to `finishReason: "stop"`, which cannot distinguish a
   * pause from a final response. Semantic interpretation remains with the host.
   */
  rawFinishReason?: string;
  /**
   * Opaque AI SDK `response.messages` replayed verbatim by the host. Encrypted
   * Anthropic reasoning and Responses reasoning items depend on it across steps.
   */
  responseMessages: unknown[];
  /** Unique newly executed native search call IDs counted in this step, including
   * failures; replayed history is excluded. Host subtracts this from its call budget. */
  nativeSearchUses?: number;
  /** IDs underlying nativeSearchUses; the host carries them across SDK/host pauses. */
  nativeSearchCallIds?: string[];
  /** Normalized references consumed by the native-search path. */
  sources: Array<{ id: string; url?: string; title?: string }>;
  /**
   * Failure facts for provider-executed tools. These are not `calls`: the host
   * must never execute them again. Native search consumes them to report search
   * failures rather than treating pause-period text as a successful result.
   */
  providerToolErrors?: Array<{ callId: string; toolName: string; message: string }>;
}

/**
 * Failure classification. The host retry loop relies solely on this field:
 * `transient` retries with backoff and `permanent` fails immediately. Only the
 * sidecar can classify AI SDK error types.
 */
export interface StepError {
  kind: "transient" | "permanent" | "cancelled";
  message: string;
  status?: number;
  /** Upstream Retry-After lower bound, in nonnegative integer milliseconds. */
  retryAfterMs?: number;
}

// ----------------------------------------------------------------- Encoding and decoding

export class ProtocolError extends Error {}

/** Decode one host frame; reject malformed shapes rather than guessing. */
export function decodeHostFrame(line: string): HostFrame {
  if (Buffer.byteLength(line, "utf8") > MAX_LINE_BYTES) {
    throw new ProtocolError(`宿主帧超过 ${MAX_LINE_BYTES} 字节上限`);
  }
  let value: unknown;
  try {
    value = JSON.parse(line);
  } catch (error) {
    throw new ProtocolError(`宿主帧不是有效 JSON: ${(error as Error).message}`);
  }
  if (typeof value !== "object" || value === null) {
    throw new ProtocolError("宿主帧必须是 JSON 对象");
  }
  const frame = value as Record<string, unknown>;
  if (frame.v !== PROTOCOL_VERSION) {
    throw new ProtocolError(`协议世代不匹配：侧车 ${PROTOCOL_VERSION}，宿主 ${String(frame.v)}`);
  }
  const type = frame.type;
  if (
    type !== "hello" &&
    type !== "step" &&
    type !== "cancel" &&
    type !== "release" &&
    type !== "shutdown"
  ) {
    throw new ProtocolError(`未知的宿主帧类型：${String(type)}`);
  }
  if ((type === "step" || type === "cancel") && typeof frame.id !== "string") {
    throw new ProtocolError(`${type} 帧缺少字符串 id`);
  }
  if (type === "step" && (typeof frame.payload !== "object" || frame.payload === null)) {
    throw new ProtocolError(`${type} 帧缺少 payload`);
  }
  if (type === "release" && typeof frame.session !== "string") {
    throw new ProtocolError("release 帧缺少字符串 session");
  }
  return frame as unknown as HostFrame;
}

/**
 * Split stdin bytes on protocol newlines.
 *
 * Do not use `readline`: it also splits on `\r\n` and `\r`, while the protocol
 * delimiter is only `\n`. A JSON body containing `\r` must remain one frame.
 */
export function createLineSplitter(onLine: (line: string) => void, onOverflow: (bytes: number) => void) {
  let buffer: Buffer = Buffer.alloc(0);
  return (chunk: Buffer) => {
    buffer = buffer.length === 0 ? chunk : Buffer.concat([buffer, chunk]);
    let start = 0;
    for (;;) {
      const index = buffer.indexOf(0x0a, start);
      if (index === -1) break;
      const line = buffer.subarray(start, index).toString("utf8");
      start = index + 1;
      if (line.length > 0) onLine(line);
    }
    buffer = buffer.subarray(start);
    if (buffer.length > MAX_LINE_BYTES) {
      const bytes = buffer.length;
      buffer = Buffer.alloc(0);
      onOverflow(bytes);
    }
  };
}
