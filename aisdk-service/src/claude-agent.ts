//! `claude-agent` family: the locally installed Claude Code executable driven
//! through the official `@anthropic-ai/claude-agent-sdk`.
//!
//! Boundary. One host round is still one sidecar `step`, and inside the CLI it is
//! one Claude API call. Claude Code's own behaviour is switched off (no built-in
//! tools, no CLAUDE.md/settings/hooks, no user MCP servers, no compaction, no
//! background tasks); the host keeps owning approvals, tool execution, hooks and
//! the round loop. The CLI's tool loop is only borrowed: every host tool is
//! published by one in-process MCP server, and when the model calls one the
//! handler *parks*. The step then ends with `done` carrying `calls`; the host
//! executes them; the next step for the same `agent.session` resolves the parked
//! handlers with the results and streams the following model reply.
//!
//! Tool names. The CLI is started with `CLAUDE_AGENT_SDK_MCP_NO_PREFIX=1`, under
//! which in-process MCP tools register under their bare names, so the model sees
//! the host's tool names verbatim (no `mcp__mework__` prefix). The prefix is still
//! stripped defensively wherever a name comes back from the CLI.
//!
//! Compliance. Authentication is the user's own Claude Code login and nothing
//! else. Mework sends no credential for this family: every authentication channel
//! that could ride in from the sidecar's environment is stripped before the CLI is
//! spawned, and `apiKey` / `baseURL` on the request are not read at all. This
//! module never reads, copies or moves `~/.claude/.credentials.json`; when a
//! session resumes from a host-synthesized transcript, it is the SDK's own resume
//! path that materializes a temporary `CLAUDE_CONFIG_DIR` and carries the CLI's
//! credentials into it. The identity line and billing header the CLI prepends to a
//! custom system prompt are the SDK's; Mework neither writes nor alters them.
//!
//! Filesystem and environment. Unlike the AI SDK families this module checks that
//! the host-resolved executable exists and derives the CLI's environment from the
//! sidecar's own environment with the credential variables removed, plus the
//! profile-location variables the host sends in `agent.env`. `CLAUDE_CONFIG_DIR`
//! is never set here: the login lives in the CLI's default `~/.claude`, and on
//! macOS the SDK only falls through to the Keychain while that variable is absent.
//! The one upstream override this family accepts is a local test stub — an
//! `ANTHROPIC_BASE_URL` in `agent.env` addressing a loopback host, which alone
//! also lets a key from `agent.env` through; a remote one fails the request rather
//! than pointing the user's own login at a third party.

import { spawn, type ChildProcess } from "node:child_process";
import { existsSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { randomUUID } from "node:crypto";

import {
  query,
  type McpSdkServerConfigWithInstance,
  type Options,
  type Query,
  type SpawnOptions,
  type SDKMessage,
  type SDKUserMessage,
  type SessionStoreEntry,
} from "@anthropic-ai/claude-agent-sdk";

import { redactError, redactSecrets, secretsOf } from "./error-redaction.js";
import { dropForeignSignedReasoning, stripReplayTags } from "./anthropic-dialect.js";
import {
  MAX_STREAM_TEXT,
  MAX_TOOL_ARGUMENTS,
  fullSystemPrompt,
  type AgentSession,
  type StepError,
  type StepEvent,
  type StepRequest,
  type StepResult,
  type ToolSpec,
  type Usage,
} from "./protocol.js";

// ---------------------------------------------------------------- host-facing surface

/** Frame writers and request bookkeeping supplied by `main.ts`. */
export interface AgentIo {
  emit(id: string, event: StepEvent): void;
  done(id: string, result: StepResult): void;
  fail(id: string, error: StepError): void;
  /** Registers a cancellable request; `streaming` requests also receive heartbeats. */
  begin(id: string, controller: AbortController, streaming: boolean): void;
  end(id: string): void;
}

export interface ClaudeAgentRuntime {
  step(id: string, request: StepRequest): Promise<void>;
  release(session: string): void;
  shutdown(): Promise<void>;
}

/** Name of the in-process MCP server that publishes the host's tools. */
const SERVER_NAME = "mework";
/** Prefix the CLI would apply without `CLAUDE_AGENT_SDK_MCP_NO_PREFIX`. */
const TOOL_PREFIX = `mcp__${SERVER_NAME}__`;
/** How long a parked tool handler may wait for the host: a week, i.e. never in practice. */
const PARK_TIMEOUT_MS = 7 * 24 * 60 * 60 * 1000;
/** Parked sessions the host never released are evicted after this idle period. */
const IDLE_EVICTION_MS = 24 * 60 * 60 * 1000;
const EVICTION_SWEEP_MS = 60 * 60 * 1000;
/** Fallback CLI version stamped on synthesized transcript entries before any `init` was seen. */
const FALLBACK_CLI_VERSION = "2.1.258";
/**
 * Prompt used when a tool round must continue in a fresh CLI session (the parked
 * session is gone). The transcript then already ends with the tool results, and
 * the CLI does not resume a turn without a prompt. The text lives only in that
 * CLI session: the host's own history never contains it.
 */
const CONTINUE_NOTICE = "[SYSTEM NOTIFICATION - NOT USER INPUT]\nThe tool results above are complete. Continue the task.";
/** Host markers that identify carrier user messages riding behind tool results. */
const HOST_NOTICE_MARKER = "[SYSTEM NOTIFICATION - NOT USER INPUT]";
const IMAGE_BRIDGE_MARKER = "[Mework tool image]";
/** Placeholder Claude Code writes for an assistant message emptied by repair. */
const NO_CONTENT_PLACEHOLDER = "(no content)";
/** Lines of CLI stderr retained for error messages. */
const STDERR_TAIL_LINES = 40;
/** Authentication channels that must never reach the CLI from the sidecar's own environment. */
const CREDENTIAL_ENV = [
  "ANTHROPIC_API_KEY",
  "ANTHROPIC_AUTH_TOKEN",
  "ANTHROPIC_BASE_URL",
  "ANTHROPIC_CUSTOM_HEADERS",
  "CLAUDE_CODE_OAUTH_TOKEN",
];
/** Variables `agent.env` may only supply together with a loopback endpoint. */
const UPSTREAM_OVERRIDE_ENV = ["ANTHROPIC_BASE_URL", "ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN"];

const log = (line: string): void => {
  process.stderr.write(`[claude-agent] ${line}\n`);
};

type JsonObject = Record<string, unknown>;

function isObject(value: unknown): value is JsonObject {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function stripToolPrefix(name: string): string {
  return name.startsWith(TOOL_PREFIX) ? name.slice(TOOL_PREFIX.length) : name;
}

function errorMessage(error: unknown): string {
  if (error instanceof Error) return error.message;
  return String(error);
}

function isAbortError(error: unknown): boolean {
  const name = (error as { name?: string })?.name;
  return name === "AbortError" || name === "TimeoutError";
}

// ---------------------------------------------------------------- in-process MCP server
//
// Hand-written JSON-RPC rather than the SDK's `tool()` helper: the helper needs
// zod and rewrites schemas, while the host schema must reach the model verbatim.
// The SDK only requires an object with `connect(transport)`; the transport it hands
// over has `onmessage`, `send`, `start` and `close`.

type JsonRpcId = string | number | null;

interface JsonRpcMessage {
  jsonrpc: "2.0";
  id?: JsonRpcId;
  method?: string;
  params?: JsonObject;
  result?: unknown;
  error?: { code: number; message: string };
}

interface McpTransport {
  onmessage?: (message: JsonRpcMessage) => void;
  onclose?: () => void;
  onerror?: (error: Error) => void;
  send(message: JsonRpcMessage): Promise<void>;
  start(): Promise<void>;
  close(): Promise<void>;
}

/** MCP `CallToolResult` content items this sidecar produces. */
type McpContent = { type: "text"; text: string } | { type: "image"; data: string; mimeType: string };

export interface McpToolResult {
  content: McpContent[];
  isError?: boolean;
}

/** The MCP protocol revision answered when the client's own is not a string. */
const MCP_PROTOCOL_FALLBACK = "2025-06-18";
const MCP_METHOD_NOT_FOUND = -32601;
const MCP_INTERNAL_ERROR = -32603;

class MeworkMcpServer {
  private transport: McpTransport | null = null;

  constructor(
    private readonly tools: ToolSpec[],
    private readonly onCall: (toolUseId: string | undefined, name: string, args: unknown) => Promise<McpToolResult>,
  ) {}

  /** Called by the SDK with its in-process transport. */
  async connect(transport: McpTransport): Promise<void> {
    this.transport = transport;
    transport.onmessage = (message) => this.handle(message);
    await transport.start();
  }

  async close(): Promise<void> {
    const transport = this.transport;
    this.transport = null;
    if (transport) await transport.close().catch(() => {});
  }

  private respond(id: JsonRpcId, result: unknown): void {
    void this.transport?.send({ jsonrpc: "2.0", id, result }).catch((error) => {
      log(`MCP 回复写入失败：${errorMessage(error)}`);
    });
  }

  private respondError(id: JsonRpcId, code: number, message: string): void {
    void this.transport?.send({ jsonrpc: "2.0", id, error: { code, message } }).catch(() => {});
  }

  private handle(message: JsonRpcMessage): void {
    if (typeof message.method !== "string") return;
    const id = message.id ?? null;
    const isRequest = message.id !== undefined && message.id !== null;
    const params = isObject(message.params) ? message.params : {};
    switch (message.method) {
      case "initialize": {
        const requested = params.protocolVersion;
        this.respond(id, {
          protocolVersion: typeof requested === "string" ? requested : MCP_PROTOCOL_FALLBACK,
          capabilities: { tools: {} },
          serverInfo: { name: SERVER_NAME, version: "1.0.0" },
        });
        break;
      }
      case "ping":
        this.respond(id, {});
        break;
      case "tools/list":
        this.respond(id, {
          tools: this.tools.map((tool) => ({
            name: tool.name,
            description: tool.description,
            // Verbatim host schema; the CLI validates it against what the API accepts.
            inputSchema: tool.inputSchema,
            // Keep every host tool in the model's context regardless of the CLI's
            // deferred-tool heuristics.
            _meta: { "anthropic/alwaysLoad": true },
          })),
        });
        break;
      case "tools/call": {
        const meta = isObject(params._meta) ? params._meta : {};
        const toolUseId = meta["claudecode/toolUseId"];
        const name = typeof params.name === "string" ? params.name : "";
        this.onCall(typeof toolUseId === "string" ? toolUseId : undefined, name, params.arguments).then(
          (result) => this.respond(id, result),
          (error) => this.respondError(id, MCP_INTERNAL_ERROR, errorMessage(error)),
        );
        break;
      }
      default:
        // Notifications (`notifications/initialized`, `notifications/cancelled`) need
        // no reply; unknown requests are refused rather than guessed.
        if (isRequest) this.respondError(id, MCP_METHOD_NOT_FOUND, `Method not found: ${message.method}`);
        break;
    }
  }
}

// ---------------------------------------------------------------- message shapes
//
// Host messages arrive in AI SDK `ModelMessage` shape; the CLI speaks Anthropic
// Messages blocks. Both directions are handled here so the rest of the module can
// stay in one vocabulary.

interface ToolResultPart {
  toolCallId: string;
  toolName: string;
  output: unknown;
}

function toolResultParts(message: unknown): ToolResultPart[] {
  if (!isObject(message) || message.role !== "tool" || !Array.isArray(message.content)) return [];
  const parts: ToolResultPart[] = [];
  for (const part of message.content) {
    if (!isObject(part) || part.type !== "tool-result" || typeof part.toolCallId !== "string") continue;
    parts.push({
      toolCallId: part.toolCallId,
      toolName: typeof part.toolName === "string" ? part.toolName : "",
      output: part.output,
    });
  }
  return parts;
}

function parseDataUrl(value: unknown): { mediaType: string; data: string } | null {
  if (typeof value !== "string") return null;
  const match = /^data:([^;,]+);base64,(.*)$/s.exec(value);
  if (!match) return null;
  return { mediaType: match[1] ?? "application/octet-stream", data: match[2] ?? "" };
}

/** Bounded JSON text for a value that must become a text block. */
function jsonText(value: unknown): string {
  try {
    return typeof value === "string" ? value : JSON.stringify(value ?? null);
  } catch {
    return String(value);
  }
}

/** AI SDK tool-result `output` → MCP result content. */
function mcpResultOf(output: unknown): McpToolResult {
  if (!isObject(output)) return { content: [{ type: "text", text: jsonText(output) }] };
  const value = output.value;
  switch (output.type) {
    case "text":
      return { content: [{ type: "text", text: typeof value === "string" ? value : jsonText(value) }] };
    case "error-text":
      return { content: [{ type: "text", text: typeof value === "string" ? value : jsonText(value) }], isError: true };
    case "json":
      return { content: [{ type: "text", text: jsonText(value) }] };
    case "error-json":
      return { content: [{ type: "text", text: jsonText(value) }], isError: true };
    case "content": {
      const content: McpContent[] = [];
      for (const item of Array.isArray(value) ? value : []) {
        if (!isObject(item)) continue;
        if (item.type === "text" && typeof item.text === "string") {
          content.push({ type: "text", text: item.text });
        } else if (item.type === "media" && typeof item.data === "string" && typeof item.mediaType === "string") {
          content.push({ type: "image", data: item.data, mimeType: item.mediaType });
        }
      }
      return { content };
    }
    default:
      return { content: [{ type: "text", text: jsonText(output) }] };
  }
}

/** AI SDK user-message parts → MCP content items (used to fold carriers into a tool result). */
function mcpContentOfUserMessage(message: unknown): McpContent[] {
  if (!isObject(message)) return [];
  if (typeof message.content === "string") {
    return message.content.length > 0 ? [{ type: "text", text: message.content }] : [];
  }
  const content: McpContent[] = [];
  for (const part of Array.isArray(message.content) ? message.content : []) {
    if (!isObject(part)) continue;
    if (part.type === "text" && typeof part.text === "string") {
      content.push({ type: "text", text: part.text });
    } else if (part.type === "image") {
      const parsed = parseDataUrl(part.image);
      if (parsed) content.push({ type: "image", data: parsed.data, mimeType: parsed.mediaType });
    }
  }
  return content;
}

/** Anthropic content blocks of a user message; `null` when it carries nothing. */
function anthropicUserBlocks(message: JsonObject): JsonObject[] | null {
  if (typeof message.content === "string") {
    return message.content.length > 0 ? [{ type: "text", text: message.content }] : null;
  }
  const blocks: JsonObject[] = [];
  for (const part of Array.isArray(message.content) ? message.content : []) {
    if (!isObject(part)) continue;
    if (part.type === "text" && typeof part.text === "string") {
      blocks.push({ type: "text", text: part.text });
    } else if (part.type === "image") {
      const parsed = parseDataUrl(part.image);
      if (parsed) {
        blocks.push({ type: "image", source: { type: "base64", media_type: parsed.mediaType, data: parsed.data } });
      } else if (typeof part.image === "string") {
        blocks.push({ type: "image", source: { type: "url", url: part.image } });
      }
    }
  }
  return blocks.length > 0 ? blocks : null;
}

function anthropicToolResultBlock(part: ToolResultPart): JsonObject {
  const result = mcpResultOf(part.output);
  const content = result.content.map((item) =>
    item.type === "text"
      ? { type: "text", text: item.text }
      : { type: "image", source: { type: "base64", media_type: item.mimeType, data: item.data } },
  );
  return {
    type: "tool_result",
    tool_use_id: part.toolCallId,
    content,
    ...(result.isError ? { is_error: true } : {}),
  };
}

/** Whether a user message is a host carrier (image bridge or task notice), not a prompt. */
function isCarrierMessage(message: unknown): boolean {
  if (!isObject(message) || message.role !== "user") return false;
  let text: string | undefined;
  if (typeof message.content === "string") {
    text = message.content;
  } else if (Array.isArray(message.content)) {
    const first = message.content.find((part) => isObject(part) && part.type === "text");
    text = isObject(first) && typeof first.text === "string" ? first.text : undefined;
  }
  if (text === undefined) return false;
  const trimmed = text.trimStart();
  return trimmed.startsWith(HOST_NOTICE_MARKER) || trimmed.startsWith(IMAGE_BRIDGE_MARKER);
}

/**
 * Splits the request messages into what the CLI must already know (`history`),
 * the results owed for a parked tool round (`results`, with carriers folded in),
 * and the user prompt that starts a new turn (`prompt`).
 */
interface SplitMessages {
  history: unknown[];
  /** Tool results after the last assistant message, by tool_use id. */
  results: Map<string, McpToolResult>;
  /** Carrier content (bridge images, notices) that rides behind the results. */
  carriers: McpContent[];
  /** Genuine trailing user message(s) merged into one prompt, or `null`. */
  prompt: JsonObject[] | null;
}

function splitMessages(messages: unknown[]): SplitMessages {
  let lastAssistant = -1;
  for (let index = messages.length - 1; index >= 0; index -= 1) {
    const message = messages[index];
    if (isObject(message) && message.role === "assistant") {
      lastAssistant = index;
      break;
    }
  }
  const tail = messages.slice(lastAssistant + 1);
  const results = new Map<string, McpToolResult>();
  const carriers: McpContent[] = [];
  const promptBlocks: JsonObject[] = [];
  let promptStart = -1;
  tail.forEach((message, offset) => {
    if (!isObject(message)) return;
    if (message.role === "tool") {
      for (const part of toolResultParts(message)) results.set(part.toolCallId, mcpResultOf(part.output));
      return;
    }
    if (message.role !== "user") return;
    if (promptStart === -1 && isCarrierMessage(message)) {
      carriers.push(...mcpContentOfUserMessage(message));
      return;
    }
    if (promptStart === -1) promptStart = offset;
    const blocks = anthropicUserBlocks(message);
    if (blocks) promptBlocks.push(...blocks);
  });
  const history = promptStart === -1 ? messages : messages.slice(0, lastAssistant + 1 + promptStart);
  return { history, results, carriers, prompt: promptBlocks.length > 0 ? promptBlocks : null };
}

// ---------------------------------------------------------------- transcript synthesis
//
// The CLI resumes from a Claude Code transcript (JSONL entries) supplied through
// `sessionStore.load()`. Host `ModelMessage[]` history is rewritten into that
// shape: signed reasoning becomes `thinking`, redacted reasoning becomes
// `redacted_thinking`, unsigned reasoning is dropped (the CLI would strip it), and
// tool messages become `tool_result` user entries with any following carrier
// messages folded into the same entry.

interface TranscriptContext {
  sessionId: string;
  cwd: string;
  cliVersion: string;
  /** Model name stamped on assistant entries. */
  model: string;
}

function reasoningPayload(part: JsonObject): { signature?: string; redactedData?: string } {
  const options = part.providerOptions;
  if (!isObject(options)) return {};
  for (const bucket of Object.values(options)) {
    if (!isObject(bucket)) continue;
    if (typeof bucket.signature === "string" && bucket.signature.length > 0) return { signature: bucket.signature };
    if (typeof bucket.redactedData === "string" && bucket.redactedData.length > 0) {
      return { redactedData: bucket.redactedData };
    }
  }
  return {};
}

function anthropicAssistantBlocks(message: JsonObject): JsonObject[] {
  if (typeof message.content === "string") {
    return message.content.length > 0 ? [{ type: "text", text: message.content }] : [];
  }
  const blocks: JsonObject[] = [];
  for (const part of Array.isArray(message.content) ? message.content : []) {
    if (!isObject(part)) continue;
    switch (part.type) {
      case "reasoning": {
        const payload = reasoningPayload(part);
        if (payload.signature) {
          blocks.push({ type: "thinking", thinking: typeof part.text === "string" ? part.text : "", signature: payload.signature });
        } else if (payload.redactedData) {
          blocks.push({ type: "redacted_thinking", data: payload.redactedData });
        }
        break;
      }
      case "text":
        if (typeof part.text === "string" && part.text.length > 0) blocks.push({ type: "text", text: part.text });
        break;
      case "tool-call":
        if (typeof part.toolCallId === "string" && typeof part.toolName === "string") {
          blocks.push({
            type: "tool_use",
            id: part.toolCallId,
            name: stripToolPrefix(part.toolName),
            input: isObject(part.input) ? part.input : {},
          });
        }
        break;
      default:
        break;
    }
  }
  return blocks;
}

function synthesizeTranscript(history: unknown[], context: TranscriptContext): JsonObject[] {
  const entries: JsonObject[] = [];
  let parentUuid: string | null = null;
  // Timestamps only need to be monotonic; place them in the past so the CLI never
  // sees a future clock.
  let clock = Date.now() - history.length * 2000;
  const push = (entry: JsonObject): void => {
    const uuid = randomUUID();
    entries.push({
      parentUuid,
      isSidechain: false,
      userType: "external",
      cwd: context.cwd,
      sessionId: context.sessionId,
      version: context.cliVersion,
      gitBranch: "HEAD",
      ...entry,
      uuid,
      timestamp: new Date(clock).toISOString(),
    });
    parentUuid = uuid;
    clock += 1000;
  };
  // Consecutive user-side messages (tool results, carriers, a new prompt) are
  // laid out the way the CLI writes them: one entry per tool result, then one
  // entry for the remaining blocks.
  let pendingBlocks: JsonObject[] = [];
  const flushUser = (): void => {
    if (pendingBlocks.length > 0) push({ type: "user", message: { role: "user", content: pendingBlocks } });
    pendingBlocks = [];
  };
  // Consecutive assistant messages become one entry, as the AI SDK merges them
  // for the API. The host projects a round's reasoning card after that round's
  // tool exchange, i.e. as an assistant message of its own; left alone, the CLI
  // drops a thinking-only message and the signed reasoning is lost. Thinking
  // blocks lead the merged message because the API requires it.
  let assistantCount = 0;
  let pendingAssistant: JsonObject[] | null = null;
  const flushAssistant = (): void => {
    if (pendingAssistant === null) return;
    const thinking = pendingAssistant.filter((block) => block.type === "thinking" || block.type === "redacted_thinking");
    const rest = pendingAssistant.filter((block) => block.type !== "thinking" && block.type !== "redacted_thinking");
    const blocks = [...thinking, ...rest];
    if (blocks.length === 0) blocks.push({ type: "text", text: NO_CONTENT_PLACEHOLDER });
    assistantCount += 1;
    push({
      type: "assistant",
      message: {
        id: `msg_mework_${assistantCount}`,
        type: "message",
        role: "assistant",
        model: context.model,
        content: blocks,
        stop_reason: blocks.some((block) => block.type === "tool_use") ? "tool_use" : "end_turn",
        stop_sequence: null,
        usage: { input_tokens: 0, output_tokens: 0 },
      },
    });
    pendingAssistant = null;
  };
  for (const message of history) {
    if (!isObject(message)) continue;
    switch (message.role) {
      case "user": {
        flushAssistant();
        const blocks = anthropicUserBlocks(message);
        if (blocks) pendingBlocks.push(...blocks);
        break;
      }
      case "tool": {
        flushAssistant();
        flushUser();
        for (const part of toolResultParts(message)) {
          const block = anthropicToolResultBlock(part);
          push({ type: "user", message: { role: "user", content: [block] }, toolUseResult: block.content });
        }
        break;
      }
      case "assistant": {
        flushUser();
        pendingAssistant ??= [];
        pendingAssistant.push(...anthropicAssistantBlocks(message));
        break;
      }
      default:
        break;
    }
  }
  flushAssistant();
  flushUser();
  return entries;
}

// ---------------------------------------------------------------- CLI environment and options

function defaultProfileEnv(): Record<string, string> {
  const home = os.homedir();
  if (process.platform === "win32") {
    const parsed = path.win32.parse(home);
    return {
      USERPROFILE: home,
      HOMEDRIVE: parsed.root.replace(/[\\/]+$/, ""),
      HOMEPATH: home.slice(parsed.root.length - 1) || "\\",
      APPDATA: path.win32.join(home, "AppData", "Roaming"),
      LOCALAPPDATA: path.win32.join(home, "AppData", "Local"),
    };
  }
  return { HOME: home };
}

/** Behaviour switches: everything Claude Code would do on its own is off. */
const CLI_CONTROL_ENV: Record<string, string> = {
  CLAUDE_AGENT_SDK_MCP_NO_PREFIX: "1",
  CLAUDE_AGENT_SDK_CLIENT_APP: "mework/1.0",
  CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC: "1",
  DISABLE_AUTOUPDATER: "1",
  DISABLE_ERROR_REPORTING: "1",
  DISABLE_TELEMETRY: "1",
  DISABLE_BUG_COMMAND: "1",
  DISABLE_COST_WARNINGS: "1",
  CLAUDE_CODE_SKIP_PROMPT_HISTORY: "1",
  DISABLE_AUTO_COMPACT: "1",
  DISABLE_COMPACT: "1",
  CLAUDE_CODE_DISABLE_ATTACHMENTS: "1",
  CLAUDE_CODE_DISABLE_AUTO_MEMORY: "1",
  CLAUDE_CODE_DISABLE_CLAUDE_MDS: "1",
  DISABLE_BUILTIN_AGENTS: "1",
  CLAUDE_AGENT_SDK_DISABLE_BUILTIN_AGENTS: "1",
  CLAUDE_CODE_DISABLE_BACKGROUND_TASKS: "1",
  CLAUDE_CODE_DISABLE_TERMINAL_TITLE: "1",
  MCP_TOOL_TIMEOUT: String(PARK_TIMEOUT_MS),
  CLAUDE_CODE_TOTAL_TOKENS_REMINDER: "off",
};

interface CliEnvKnobs {
  maxOutputTokens?: number;
}

/**
 * Whether an endpoint address belongs to this machine. `0.0.0.0` is a wildcard
 * bind address rather than a destination and does not qualify.
 */
function isLoopbackEndpoint(value: string): boolean {
  let url: URL;
  try {
    url = new URL(value);
  } catch {
    return false;
  }
  const host = url.hostname.toLowerCase().replace(/^\[/, "").replace(/\]$/, "");
  if (host === "localhost" || host === "::1") return true;
  const octets = /^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$/.exec(host);
  if (!octets) return false;
  return octets.slice(1).every((octet) => Number(octet) <= 255) && octets[1] === "127";
}

/**
 * `agent.env` after the upstream rule. Everything but the three authentication
 * variables passes through; those exist for one purpose only, a local test stub,
 * so a remote endpoint fails the whole request instead of being dropped quietly,
 * and a key without such an endpoint is discarded.
 */
function agentEnvOverrides(agent: AgentSession): Record<string, string> {
  const source = agent.env ?? {};
  const overrides: Record<string, string> = {};
  for (const [name, value] of Object.entries(source)) {
    if (typeof value === "string" && !UPSTREAM_OVERRIDE_ENV.includes(name)) overrides[name] = value;
  }
  const baseURL = typeof source.ANTHROPIC_BASE_URL === "string" ? source.ANTHROPIC_BASE_URL.trim() : "";
  if (baseURL.length > 0) {
    if (!isLoopbackEndpoint(baseURL)) {
      throw new Error("agent.env 里的 ANTHROPIC_BASE_URL 只允许本机测试桩");
    }
    overrides.ANTHROPIC_BASE_URL = baseURL;
  }
  for (const name of ["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN"] as const) {
    const key = source[name];
    if (typeof key !== "string" || key.length === 0) continue;
    if (baseURL.length === 0) {
      log(`丢弃 agent.env 里的 ${name}：没有随附本机测试桩地址`);
      continue;
    }
    overrides[name] = key;
  }
  return overrides;
}

/**
 * The four model variables the CLI consults for its own routing. Only a real
 * model id is pinned: an alias (`sonnet`, `opus[1m]`) is the CLI's own
 * vocabulary, and feeding it back through the environment would only make the
 * alias resolve to itself.
 */
function pinnedModelEnv(modelId: string): Record<string, string> {
  if (!/^claude-/i.test(modelId)) return {};
  return {
    ANTHROPIC_MODEL: modelId,
    ANTHROPIC_DEFAULT_OPUS_MODEL: modelId,
    ANTHROPIC_DEFAULT_SONNET_MODEL: modelId,
    ANTHROPIC_DEFAULT_HAIKU_MODEL: modelId,
  };
}

function cliEnv(agent: AgentSession, modelId: string, knobs: CliEnvKnobs = {}) {
  const env: Record<string, string> = {};
  for (const [name, value] of Object.entries(process.env)) {
    if (typeof value === "string") env[name] = value;
  }
  // The CLI must authenticate with its own login. A developer shell's ambient
  // credentials would override it silently, and the SDK only falls back to the
  // stored login when no credential is forced through the environment.
  for (const name of CREDENTIAL_ENV) delete env[name];
  Object.assign(env, defaultProfileEnv(), CLI_CONTROL_ENV);
  env.CLAUDE_CODE_USE_BEDROCK = "0";
  env.CLAUDE_CODE_USE_VERTEX = "0";
  env.CLAUDE_CODE_USE_FOUNDRY = "0";
  Object.assign(env, agentEnvOverrides(agent), pinnedModelEnv(modelId));
  const { maxOutputTokens } = knobs;
  if (maxOutputTokens !== undefined && Number.isFinite(maxOutputTokens) && maxOutputTokens > 0) {
    env.CLAUDE_CODE_MAX_OUTPUT_TOKENS = String(Math.floor(maxOutputTokens));
  }
  return env;
}

function reasoningOptions(level: StepRequest["reasoning"]): Pick<Options, "thinking" | "effort"> {
  switch (level) {
    case "none":
      return { thinking: { type: "disabled" } };
    case "minimal":
    case "low":
      return { effort: "low" };
    case "medium":
      return { effort: "medium" };
    case "high":
      return { effort: "high" };
    case "xhigh":
      return { effort: "xhigh" };
    default:
      return {};
  }
}

function validateAgent(agent: AgentSession | undefined): AgentSession {
  if (!agent) throw new Error("claude-agent 请求缺少 agent 会话参数");
  if (typeof agent.executable !== "string" || agent.executable.length === 0) {
    throw new Error("claude-agent 请求缺少 Claude Code 可执行文件路径");
  }
  if (!existsSync(agent.executable)) {
    throw new Error(
      `找不到 Claude Code 可执行文件：${agent.executable}。请安装原生版 Claude Code（https://code.claude.com/docs/en/setup），或在提供商设置里填写其路径。`,
    );
  }
  if (typeof agent.cwd !== "string" || agent.cwd.length === 0) throw new Error("claude-agent 请求缺少工作目录");
  return agent;
}

/**
 * Spawns the CLI ourselves so teardown can kill it at once. The SDK's own abort
 * path first closes stdin and only kills after a grace window; during that
 * window the CLI finishes its turn gracefully — including one more billable
 * API request when a tool round was in flight. A run the host ended must not
 * cost anything more.
 */
class CliProcess {
  private child: ChildProcess | null = null;

  constructor(private readonly stderr: StderrTail) {}

  spawnHook(): NonNullable<Options["spawnClaudeCodeProcess"]> {
    return (options: SpawnOptions) => {
      const child = spawn(options.command, options.args, {
        cwd: options.cwd,
        env: options.env,
        stdio: ["pipe", "pipe", "pipe"],
        windowsHide: true,
      });
      child.stderr?.setEncoding("utf8");
      child.stderr?.on("data", (data: string) => this.stderr.push(data));
      this.child = child;
      return child as unknown as ReturnType<NonNullable<Options["spawnClaudeCodeProcess"]>>;
    };
  }

  kill(): void {
    const child = this.child;
    this.child = null;
    if (child && child.exitCode === null && child.signalCode === null) {
      try {
        // SIGKILL, not the default SIGTERM: this is the immediate-termination
        // contract, and a TERM the CLI survives would also mark the child
        // `killed` and make the SDK skip its own KILL escalation. Windows
        // terminates on either signal.
        child.kill("SIGKILL");
      } catch {
        // Already gone.
      }
    }
  }
}

/** Ring buffer of CLI stderr for diagnostics. */
class StderrTail {
  private lines: string[] = [];

  push(chunk: string): void {
    for (const line of chunk.split(/\r?\n/)) {
      if (line.length === 0) continue;
      this.lines.push(line.slice(0, 500));
      if (this.lines.length > STDERR_TAIL_LINES) this.lines.shift();
    }
  }

  text(): string {
    return this.lines.join("\n");
  }
}

/** A prompt stream the sidecar can feed and close explicitly. */
class InputQueue implements AsyncIterable<SDKUserMessage> {
  private queue: Array<SDKUserMessage | null> = [];
  private wake: (() => void) | null = null;

  push(message: SDKUserMessage): void {
    this.queue.push(message);
    this.wake?.();
  }

  end(): void {
    this.queue.push(null);
    this.wake?.();
  }

  async *[Symbol.asyncIterator](): AsyncGenerator<SDKUserMessage, void> {
    for (;;) {
      if (this.queue.length === 0) {
        await new Promise<void>((resolve) => {
          this.wake = resolve;
        });
        this.wake = null;
        continue;
      }
      const next = this.queue.shift();
      if (next === null || next === undefined) return;
      yield next;
    }
  }
}

// ---------------------------------------------------------------- step translation
//
// Raw Anthropic stream events (the SDK's `stream_event` messages) are translated to
// host `StepEvent`s the same way `main.ts` translates the AI SDK stream. A step is
// the one API message the CLI makes for this round; it ends either when that
// message stops at `tool_use` (the handlers park) or when the CLI's `result`
// arrives for a final reply.

interface ThinkingBlock {
  kind: "thinking";
  text: string;
  signature: string;
  ordinal: number | null;
  sawTextFrame: boolean;
  openedAt: number;
}

interface RedactedBlock {
  kind: "redacted";
  data: string;
  ordinal: number;
}

interface TextBlock {
  kind: "text";
  text: string;
}

interface ToolUseBlock {
  kind: "tool_use";
  id: string;
  name: string;
  json: string;
  input: unknown;
  parsed: boolean;
  /** Whether the host publishes this tool; other names belong to the CLI's own loop. */
  known: boolean;
}

type Block = ThinkingBlock | RedactedBlock | TextBlock | ToolUseBlock;

interface StepOutcome {
  kind: "done";
  result: StepResult;
}

class StepTranslator {
  private text = "";
  private readonly reasoning: string[] = [];
  private reasoningOrdinals = 0;
  private reasoningMs = 0;
  private reasoningSeen = false;
  private readonly calls: StepResult["calls"] = [];
  private readonly parts: JsonObject[] = [];
  private blocks = new Map<number, Block>();
  private usage: Usage = {};
  private messageUsage: Usage = {};
  private model: string | undefined;
  private stopReason: string | null = null;
  private messageStopped = false;
  private settled = false;
  private readonly finish: (outcome: StepOutcome | StepError) => void;
  readonly outcome: Promise<StepOutcome | StepError>;

  constructor(
    private readonly id: string,
    private readonly io: AgentIo,
    private readonly knownTools: ReadonlySet<string>,
  ) {
    let resolve!: (outcome: StepOutcome | StepError) => void;
    this.outcome = new Promise<StepOutcome | StepError>((done) => {
      resolve = done;
    });
    this.finish = resolve;
  }

  /** Tool_use ids of this step's reported calls. */
  get callIds(): string[] {
    return this.calls.map((call) => call.callId);
  }

  get modelName(): string | undefined {
    return this.model;
  }

  fail(error: StepError): void {
    if (this.settled) return;
    this.settled = true;
    this.finish(error);
  }

  /** Supplies a tool input the stream could not parse, from the CLI's own call. */
  supplyToolInput(toolUseId: string, input: unknown): void {
    for (const block of this.blocks.values()) {
      if (block.kind === "tool_use" && block.id === toolUseId && !block.parsed) {
        block.input = isObject(input) ? input : {};
        block.parsed = true;
        this.emitToolCall(block);
      }
    }
    this.maybeSettleToolRound();
  }

  handle(message: SDKMessage): void {
    if (this.settled) return;
    switch (message.type) {
      case "stream_event":
        if (message.parent_tool_use_id === null) this.handleEvent(message.event as unknown as JsonObject);
        break;
      case "assistant": {
        if (message.parent_tool_use_id !== null) break;
        const error = (message as { error?: string }).error;
        const content = (message.message as { content?: unknown }).content;
        for (const block of Array.isArray(content) ? content : []) {
          if (isObject(block) && block.type === "tool_use" && typeof block.id === "string") {
            this.supplyToolInput(block.id, block.input);
          }
        }
        if (error) {
          const text = Array.isArray(content)
            ? content
                .filter((block): block is JsonObject => isObject(block) && block.type === "text")
                .map((block) => String(block.text ?? ""))
                .join("")
            : "";
          this.fail({ kind: "permanent", message: text.length > 0 ? text : `Claude Code 报告错误：${error}` });
        }
        break;
      }
      case "result": {
        const failed = message.is_error || message.subtype !== "success";
        if (failed) {
          const detail = message.subtype === "success"
            ? message.result
            : [message.subtype, ...(message.errors ?? [])].join(": ");
          this.fail({ kind: "permanent", message: detail.length > 0 ? detail : "Claude Code 回合失败" });
          break;
        }
        this.settle("stop");
        break;
      }
      default:
        break;
    }
  }

  private handleEvent(event: JsonObject): void {
    switch (event.type) {
      case "message_start": {
        const message = isObject(event.message) ? event.message : {};
        if (typeof message.model === "string") this.model = message.model;
        this.blocks = new Map();
        this.messageStopped = false;
        this.stopReason = null;
        this.messageUsage = usageOf(isObject(message.usage) ? message.usage : {});
        break;
      }
      case "content_block_start": {
        const index = typeof event.index === "number" ? event.index : this.blocks.size;
        const block = isObject(event.content_block) ? event.content_block : {};
        this.startBlock(index, block);
        break;
      }
      case "content_block_delta": {
        const index = typeof event.index === "number" ? event.index : -1;
        const delta = isObject(event.delta) ? event.delta : {};
        this.applyDelta(index, delta);
        break;
      }
      case "content_block_stop": {
        const index = typeof event.index === "number" ? event.index : -1;
        this.stopBlock(index);
        break;
      }
      case "message_delta": {
        const delta = isObject(event.delta) ? event.delta : {};
        if (typeof delta.stop_reason === "string") this.stopReason = delta.stop_reason;
        if (isObject(event.usage)) this.messageUsage = mergeUsage(this.messageUsage, usageOf(event.usage));
        break;
      }
      case "message_stop": {
        this.messageStopped = true;
        this.usage = addUsage(this.usage, this.messageUsage);
        this.io.emit(this.id, { k: "usage", usage: this.usage });
        this.maybeSettleToolRound();
        break;
      }
      default:
        break;
    }
  }

  private startBlock(index: number, block: JsonObject): void {
    switch (block.type) {
      case "text":
        this.blocks.set(index, { kind: "text", text: "" });
        break;
      case "thinking":
        this.blocks.set(index, {
          kind: "thinking",
          text: "",
          signature: typeof block.signature === "string" ? block.signature : "",
          ordinal: null,
          sawTextFrame: false,
          openedAt: Date.now(),
        });
        break;
      case "redacted_thinking": {
        // A whole encrypted item: its start is the only evidence, so the card opens
        // and closes at once.
        if (typeof block.data !== "string" || block.data.length === 0) break;
        const ordinal = this.openReasoning("encrypted");
        this.blocks.set(index, { kind: "redacted", data: typeof block.data === "string" ? block.data : "", ordinal });
        this.io.emit(this.id, { k: "reasoning-done", item: ordinal, durationMs: this.reasoningMs });
        break;
      }
      case "tool_use": {
        const id = typeof block.id === "string" ? block.id : "";
        const name = stripToolPrefix(typeof block.name === "string" ? block.name : "");
        const known = this.knownTools.has(name);
        this.blocks.set(index, { kind: "tool_use", id, name, json: "", input: undefined, parsed: false, known });
        if (known) this.io.emit(this.id, { k: "tool-call-announced", callId: id, toolName: name });
        else log(`模型调用了宿主未发布的工具 ${name}（${id}），交由 Claude Code 自行回绝`);
        break;
      }
      default:
        break;
    }
  }

  private openReasoning(form?: "plaintext" | "encrypted"): number {
    const ordinal = this.reasoningOrdinals;
    this.reasoningOrdinals += 1;
    this.reasoningSeen = true;
    this.reasoning[ordinal] = "";
    this.io.emit(this.id, { k: "reasoning-start", item: ordinal, form });
    return ordinal;
  }

  private applyDelta(index: number, delta: JsonObject): void {
    const block = this.blocks.get(index);
    if (!block) return;
    switch (delta.type) {
      case "text_delta": {
        if (block.kind !== "text" || typeof delta.text !== "string") break;
        if (this.text.length + delta.text.length > MAX_STREAM_TEXT) {
          throw new Error(`单轮可见文本超过 ${MAX_STREAM_TEXT} 字节上限`);
        }
        block.text += delta.text;
        this.text += delta.text;
        this.io.emit(this.id, { k: "text-delta", delta: delta.text });
        break;
      }
      case "thinking_delta": {
        if (block.kind !== "thinking" || typeof delta.thinking !== "string") break;
        block.sawTextFrame = true;
        if (delta.thinking.length === 0) break;
        if (block.ordinal === null) block.ordinal = this.openReasoning("plaintext");
        block.text += delta.thinking;
        this.io.emit(this.id, { k: "reasoning-delta", item: block.ordinal, delta: delta.thinking });
        break;
      }
      case "signature_delta": {
        if (block.kind === "thinking" && typeof delta.signature === "string") block.signature += delta.signature;
        break;
      }
      case "input_json_delta": {
        if (block.kind !== "tool_use" || typeof delta.partial_json !== "string") break;
        if (block.json.length + delta.partial_json.length > MAX_TOOL_ARGUMENTS) {
          throw new Error(`工具 ${block.name} 的参数超过 ${MAX_TOOL_ARGUMENTS} 字节上限`);
        }
        block.json += delta.partial_json;
        break;
      }
      default:
        break;
    }
  }

  private stopBlock(index: number): void {
    const block = this.blocks.get(index);
    if (!block) return;
    switch (block.kind) {
      case "text":
        if (block.text.length > 0) this.parts.push({ type: "text", text: block.text });
        break;
      case "thinking": {
        // An item whose text was stripped by a relay still earns a card when the
        // thinking channel existed and a signature proves the model thought.
        if (block.ordinal === null && block.sawTextFrame && block.signature.length > 0) {
          block.ordinal = this.openReasoning();
        }
        if (block.ordinal !== null) {
          this.reasoningMs += Date.now() - block.openedAt;
          this.reasoning[block.ordinal] = block.text;
          this.io.emit(this.id, { k: "reasoning-done", item: block.ordinal, durationMs: this.reasoningMs });
        }
        if (block.signature.length > 0) {
          this.parts.push({
            type: "reasoning",
            text: block.text,
            providerOptions: { anthropic: { signature: block.signature } },
          });
        } else if (block.text.length > 0) {
          this.parts.push({ type: "reasoning", text: block.text });
        }
        break;
      }
      case "redacted":
        this.parts.push({ type: "reasoning", text: "", providerOptions: { anthropic: { redactedData: block.data } } });
        break;
      case "tool_use": {
        const json = block.json.trim();
        if (json.length === 0) {
          block.input = {};
          block.parsed = true;
        } else {
          try {
            const parsed: unknown = JSON.parse(json);
            block.input = isObject(parsed) ? parsed : {};
            block.parsed = true;
          } catch {
            // The CLI's own `assistant` message or its tools/call carries the full input.
            block.parsed = false;
          }
        }
        if (block.parsed) this.emitToolCall(block);
        break;
      }
      default:
        break;
    }
  }

  private emitToolCall(block: ToolUseBlock): void {
    if (!block.known) return;
    if (this.calls.some((call) => call.callId === block.id)) return;
    this.calls.push({ callId: block.id, toolName: block.name, input: block.input });
    this.parts.push({ type: "tool-call", toolCallId: block.id, toolName: block.name, input: block.input });
    this.io.emit(this.id, { k: "tool-call", callId: block.id, toolName: block.name, input: block.input });
  }

  /**
   * Ends the step once the message is complete and every known call has its input.
   *
   * The stop reason is deliberately not consulted: the CLI invokes the handler of
   * every complete `tool_use` block even when the message was cut off by
   * `max_tokens`, so waiting for `tool_use` there would park the handler against a
   * host round that never starts. The raw stop reason still travels in the result.
   */
  private maybeSettleToolRound(): void {
    if (!this.messageStopped) return;
    const toolBlocks = [...this.blocks.values()].filter((block): block is ToolUseBlock => block.kind === "tool_use");
    if (toolBlocks.some((block) => block.known && !block.parsed)) return;
    // A message whose only calls target tools the host does not publish is the CLI's
    // own business: it answers them itself and keeps streaming into this step.
    if (!toolBlocks.some((block) => block.known)) return;
    this.settle("tool-calls");
  }

  private settle(finishReason: "stop" | "tool-calls"): void {
    if (this.settled) return;
    this.settled = true;
    const orderedReasoning = [...this.reasoning];
    const result: StepResult = {
      text: this.text,
      reasoning: orderedReasoning,
      ...(this.reasoningSeen ? { reasoningMs: this.reasoningMs } : {}),
      calls: this.calls,
      usage: this.usage,
      model: this.model,
      finishReason: finishReason === "tool-calls" ? "tool-calls" : stopReasonOf(this.stopReason),
      ...(this.stopReason ? { rawFinishReason: this.stopReason } : {}),
      responseMessages: this.parts.length > 0 ? [{ role: "assistant", content: this.parts }] : [],
      sources: [],
    };
    this.finish({ kind: "done", result });
  }
}

function stopReasonOf(stopReason: string | null): string {
  switch (stopReason) {
    case "end_turn":
    case "stop_sequence":
    case null:
      return "stop";
    case "max_tokens":
      return "length";
    case "tool_use":
      return "tool-calls";
    default:
      return "other";
  }
}

function number(value: unknown): number | undefined {
  return typeof value === "number" && Number.isFinite(value) ? value : undefined;
}

/** Anthropic `usage` object → host usage, with cache reads and writes counted as input. */
function usageOf(usage: JsonObject): Usage {
  const input = number(usage.input_tokens);
  const cacheWrite = number(usage.cache_creation_input_tokens);
  const cacheRead = number(usage.cache_read_input_tokens);
  const output = number(usage.output_tokens);
  const details = isObject(usage.output_tokens_details) ? usage.output_tokens_details : {};
  const reasoning = number(details.thinking_tokens);
  const inputTotal = input === undefined && cacheWrite === undefined && cacheRead === undefined
    ? undefined
    : (input ?? 0) + (cacheWrite ?? 0) + (cacheRead ?? 0);
  return {
    inputTokens: inputTotal,
    outputTokens: output,
    totalTokens: inputTotal === undefined && output === undefined ? undefined : (inputTotal ?? 0) + (output ?? 0),
    reasoningTokens: reasoning,
    cacheReadTokens: cacheRead,
    cacheWriteTokens: cacheWrite,
  };
}

/** `message_delta` usage restates counters; later values win, absent ones keep the earlier. */
function mergeUsage(base: Usage, update: Usage): Usage {
  const merged: Usage = { ...base };
  for (const key of Object.keys(update) as Array<keyof Usage>) {
    if (update[key] !== undefined) merged[key] = update[key];
  }
  const input = merged.inputTokens;
  const output = merged.outputTokens;
  merged.totalTokens = input === undefined && output === undefined ? undefined : (input ?? 0) + (output ?? 0);
  return merged;
}

/** Sums two usage records; a step spanning several API messages reports the total. */
function addUsage(base: Usage, next: Usage): Usage {
  const sum: Usage = {};
  for (const key of ["inputTokens", "outputTokens", "totalTokens", "reasoningTokens", "cacheReadTokens", "cacheWriteTokens"] as const) {
    const left = base[key];
    const right = next[key];
    if (left === undefined && right === undefined) continue;
    sum[key] = (left ?? 0) + (right ?? 0);
  }
  return sum;
}

// ---------------------------------------------------------------- sessions

interface ParkedCall {
  name: string;
  resolve: (result: McpToolResult) => void;
}

interface Session {
  key: string;
  query: Query;
  abort: AbortController;
  input: InputQueue;
  server: MeworkMcpServer;
  stderr: StderrTail;
  process: CliProcess;
  /** Request secrets to strip from any failure text this session produces. */
  secrets: string[];
  /** Handlers that arrived before their result. */
  parked: Map<string, ParkedCall>;
  /** Results that arrived before their handler. */
  results: Map<string, McpToolResult>;
  /** Tool_use ids the last step reported; the next step must answer them. */
  awaiting: Set<string>;
  /** Carrier content folded into the last result once every result is known. */
  step: StepTranslator | null;
  /** Messages received while no step was attached. */
  inbox: SDKMessage[];
  /** Set once the query iterator finished, with the failure if any. */
  ended: StepError | "ok" | null;
  pump: Promise<void>;
  lastUsedAt: number;
  cliVersion: string | undefined;
  knownTools: ReadonlySet<string>;
  /** Model id the host requested; keys the served-model cache. */
  modelId: string;
}

/** Messages worth buffering between steps. Everything else is chatter. */
function isStepMessage(message: SDKMessage): boolean {
  return message.type === "stream_event" || message.type === "assistant" || message.type === "result";
}

export function createClaudeAgentRuntime(io: AgentIo): ClaudeAgentRuntime {
  const sessions = new Map<string, Session>();
  /** Requested model id → model name the CLI actually served, for transcript stamps. */
  const servedModels = new Map<string, string>();
  let lastCliVersion: string | undefined;

  const sweep = setInterval(() => {
    const now = Date.now();
    for (const session of sessions.values()) {
      if (session.step === null && now - session.lastUsedAt > IDLE_EVICTION_MS) {
        log(`会话 ${session.key} 闲置超过 24 小时，回收`);
        teardown(session);
      }
    }
  }, EVICTION_SWEEP_MS);
  sweep.unref();

  function teardown(session: Session): void {
    if (sessions.get(session.key) === session) sessions.delete(session.key);
    // Kill first: a parked handler must never be answered, or the CLI would run
    // one more API call before the SDK's graceful shutdown reaches it.
    session.process.kill();
    session.abort.abort();
    session.input.end();
    session.parked.clear();
    session.results.clear();
    session.awaiting.clear();
    void session.server.close();
  }

  function deliver(session: Session, message: SDKMessage): void {
    session.lastUsedAt = Date.now();
    if (message.type === "system" && message.subtype === "init") {
      session.cliVersion = message.claude_code_version;
      lastCliVersion = message.claude_code_version;
      log(
        `会话 ${session.key} 就绪：Claude Code ${message.claude_code_version}，模型 ${message.model}，` +
          `凭据来源 ${message.apiKeySource}，工具 ${message.tools.length} 个`,
      );
      return;
    }
    if (message.type === "stream_event" && message.parent_tool_use_id === null) {
      const event = message.event as unknown as JsonObject;
      if (event.type === "message_start" && isObject(event.message) && typeof event.message.model === "string") {
        servedModels.set(session.modelId, event.message.model);
      }
    }
    if (message.type === "system" && message.subtype === "api_retry") {
      log(`会话 ${session.key}：Claude Code 正在重试上游请求`);
    }
    if (session.step) {
      try {
        session.step.handle(message);
      } catch (error) {
        session.step.fail({ kind: "permanent", message: errorMessage(error) });
        teardown(session);
      }
      return;
    }
    if (isStepMessage(message)) session.inbox.push(message);
  }

  async function pump(session: Session): Promise<void> {
    try {
      for await (const message of session.query) deliver(session, message);
      session.ended = "ok";
    } catch (error) {
      session.ended = classifyQueryError(error, session.stderr);
    }
    // A step still attached when the iterator ends never got its message.
    if (session.step) {
      session.step.fail(
        session.ended === "ok" ? { kind: "permanent", message: "Claude Code 会话在回复完成前结束" } : session.ended,
      );
    }
    if (sessions.get(session.key) === session) sessions.delete(session.key);
  }

  function classifyQueryError(error: unknown, stderr: StderrTail): StepError {
    if (isAbortError(error)) return { kind: "cancelled", message: "请求已取消" };
    const tail = stderr.text();
    const message = errorMessage(error);
    return { kind: "permanent", message: tail.length > 0 ? `${message}\n${tail}` : message };
  }

  function startSession(
    request: StepRequest,
    agent: AgentSession,
    prompt: SDKUserMessage,
    history: unknown[],
  ): Session {
    const tools = request.tools ?? [];
    const knownTools = new Set(tools.map((tool) => tool.name));
    const abort = new AbortController();
    const input = new InputQueue();
    const stderr = new StderrTail();
    const cli = new CliProcess(stderr);
    const sessionRef: { current: Session | null } = { current: null };
    const server = new MeworkMcpServer(tools, (toolUseId, name, args) => {
      const session = sessionRef.current;
      if (!session) return Promise.reject(new Error("session is gone"));
      const id = toolUseId ?? "";
      const bare = stripToolPrefix(name);
      session.step?.supplyToolInput(id, args);
      const ready = session.results.get(id);
      if (ready) {
        session.results.delete(id);
        return Promise.resolve(ready);
      }
      return new Promise<McpToolResult>((resolve) => {
        session.parked.set(id, { name: bare, resolve });
      });
    });

    const resumed = history.length > 0;
    const sessionId = randomUUID();
    const transcript = resumed
      ? synthesizeTranscript(history, {
          sessionId,
          cwd: agent.cwd,
          cliVersion: lastCliVersion ?? FALLBACK_CLI_VERSION,
          model: servedModels.get(request.modelId) ?? request.modelId,
        })
      : [];

    const options: Options = {
      abortController: abort,
      cwd: agent.cwd,
      pathToClaudeCodeExecutable: agent.executable,
      spawnClaudeCodeProcess: cli.spawnHook(),
      env: cliEnv(agent, request.modelId, { maxOutputTokens: request.maxOutputTokens }),
      systemPrompt: fullSystemPrompt(request) ?? "",
      tools: [],
      settingSources: [],
      strictMcpConfig: true,
      mcpServers: {
        [SERVER_NAME]: {
          type: "sdk",
          name: SERVER_NAME,
          // The SDK only ever calls `instance.connect(transport)`; the hand-written
          // server satisfies that contract without the MCP SDK's `McpServer` class.
          instance: server as unknown as McpSdkServerConfigWithInstance["instance"],
          timeout: PARK_TIMEOUT_MS,
        },
      },
      // The host has already approved every call it sends back, so the CLI's own
      // permission layer answers "allow" for everything. The callback rather than
      // `allowedTools` rules: rules are matched by name syntax, the callback is not.
      canUseTool: async (_toolName, toolInput) => ({ behavior: "allow", updatedInput: toolInput }),
      permissionMode: "default",
      includePartialMessages: true,
      model: request.modelId,
      ...reasoningOptions(request.reasoning),
      settings: { totalTokensReminder: "off" },
      ...(resumed
        ? {
            resume: sessionId,
            persistSession: true,
            sessionStore: {
              load: async () => transcript as unknown as SessionStoreEntry[],
              append: async () => {},
            },
          }
        : { persistSession: false }),
    };

    input.push(prompt);
    const created = query({ prompt: input, options });
    const session: Session = {
      key: agent.session,
      query: created,
      abort,
      input,
      server,
      stderr,
      process: cli,
      secrets: secretsOf(request),
      parked: new Map(),
      results: new Map(),
      awaiting: new Set(),
      step: null,
      inbox: [],
      ended: null,
      pump: Promise.resolve(),
      lastUsedAt: Date.now(),
      cliVersion: undefined,
      knownTools,
      modelId: request.modelId,
    };
    sessionRef.current = session;
    session.pump = pump(session);
    sessions.set(agent.session, session);
    return session;
  }

  /** Runs one step against a session: attach, drain the inbox, wait for the outcome. */
  async function runAttached(session: Session, id: string, controller: AbortController): Promise<void> {
    const translator = new StepTranslator(id, io, session.knownTools);
    session.step = translator;
    const onAbort = (): void => {
      translator.fail({ kind: "cancelled", message: "请求已取消" });
      teardown(session);
    };
    controller.signal.addEventListener("abort", onAbort, { once: true });
    try {
      if (session.ended) {
        translator.fail(session.ended === "ok" ? { kind: "permanent", message: "Claude Code 会话已结束" } : session.ended);
      }
      const inbox = session.inbox.splice(0);
      for (const message of inbox) deliver(session, message);
      const outcome = await translator.outcome;
      if (outcome.kind === "done") {
        session.awaiting = new Set(translator.callIds);
        io.done(id, outcome.result);
      } else {
        io.fail(id, redactError(outcome, session.secrets));
        if (outcome.kind !== "cancelled") teardown(session);
      }
    } finally {
      controller.signal.removeEventListener("abort", onAbort);
      if (session.step === translator) session.step = null;
      session.lastUsedAt = Date.now();
    }
  }

  async function step(id: string, request: StepRequest): Promise<void> {
    const controller = new AbortController();
    io.begin(id, controller, true);
    try {
      const agent = validateAgent(request.agent);
      // Replayed reasoning parts are tagged by the host with the model that signed
      // them; Anthropic binds a signature to that model, so a switched conversation
      // drops them and the tag itself never reaches the CLI.
      dropForeignSignedReasoning(request.messages, request.modelId);
      stripReplayTags(request.messages);
      const split = splitMessages(request.messages);
      const live = sessions.get(agent.session);

      // Continuation: the parked round gets its results.
      if (live && live.ended === null && live.awaiting.size > 0 && split.prompt === null) {
        const missing = [...live.awaiting].filter((callId) => !split.results.has(callId));
        if (missing.length === 0) {
          const ordered = [...live.awaiting];
          live.awaiting = new Set();
          const carriers = split.carriers;
          const attach = runAttached(live, id, controller);
          ordered.forEach((callId, index) => {
            const base = split.results.get(callId) ?? { content: [] };
            const result: McpToolResult = index === ordered.length - 1 && carriers.length > 0
              ? { ...base, content: [...base.content, ...carriers] }
              : base;
            const parked = live.parked.get(callId);
            if (parked) {
              live.parked.delete(callId);
              parked.resolve(result);
            } else {
              live.results.set(callId, result);
            }
          });
          await attach;
          return;
        }
        log(`会话 ${agent.session} 缺少工具结果 ${missing.join(", ")}，改为重建会话`);
      }

      if (live) teardown(live);
      let prompt: SDKUserMessage;
      let history: unknown[];
      if (split.prompt) {
        prompt = { type: "user", message: { role: "user", content: split.prompt as never }, parent_tool_use_id: null };
        history = split.history;
      } else if (split.results.size > 0) {
        // The parked session is gone; the transcript already carries the results.
        prompt = { type: "user", message: { role: "user", content: CONTINUE_NOTICE }, parent_tool_use_id: null };
        history = request.messages;
      } else {
        throw new Error("Claude Code 需要一条用户消息才能开始回合");
      }
      const session = startSession(request, agent, prompt, history);
      await runAttached(session, id, controller);
    } catch (error) {
      io.fail(
        id,
        isAbortError(error)
          ? { kind: "cancelled", message: "请求已取消" }
          : redactError({ kind: "permanent", message: errorMessage(error) }, secretsOf(request)),
      );
    } finally {
      io.end(id);
    }
  }

  function release(key: string): void {
    const session = sessions.get(key);
    if (session) teardown(session);
  }

  async function shutdown(): Promise<void> {
    clearInterval(sweep);
    const pumps = [...sessions.values()].map((session) => session.pump);
    for (const session of [...sessions.values()]) teardown(session);
    await Promise.race([
      Promise.allSettled(pumps),
      new Promise<void>((resolve) => setTimeout(resolve, 3000).unref()),
    ]);
  }

  return { step, release, shutdown };
}
