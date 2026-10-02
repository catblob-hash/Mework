//! Tools that join a conversation mid-way, handed over through each protocol's
//! own append interface.
//!
//! The host marks the point a tool joined with one message: a `system` message
//! with no text whose `providerOptions.mework.toolAddition` names the tools
//! (Rust `tool_append::marker_message`). It never reaches a provider as that.
//! Here it becomes:
//!
//! - Anthropic Messages: a mid-conversation `role: "system"` message carrying
//!   `tool_addition` blocks (beta `mid-conversation-tool-changes-2026-07-01`),
//!   with the tool itself declared `defer_loading: true`. A deferred tool is
//!   left out of the prompt-cache key, so the declared list keeps its cache.
//! - OpenAI Responses, Azure and the Codex backend: an `additional_tools` input
//!   item carrying the tool's definition, and the tool left out of `tools`.
//! - Everything else: nothing. The marker is dropped and the tool stays in the
//!   declared list, which is what every protocol without an append interface
//!   has always had.
//!
//! No text goes with an addition; the tool simply becomes available where the
//! marker stands. Whether the model at this endpoint takes one is the host's
//! word (`request.toolAppend`: the model's declared capability, which Mework
//! declares where it knows and the user for a relay that passes the interface
//! on); the sidecar does not second-guess the endpoint. Where the addition
//! cannot go — a model the host says does not take it, an endpoint that
//! refuses it all the same, a marker in a place the protocol forbids — the
//! tool falls back into the declared list, which costs that request its cache
//! and nothing else.

import type { ProviderFamily, ToolSpec } from "./protocol.js";
import { anthropicSystemPlacementHolds } from "./system-append.js";

type JsonObject = Record<string, unknown>;

function isObject(value: unknown): value is JsonObject {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/** Mirrors Rust `tool_append::MARKER_OPTIONS_KEY` / `MARKER_TOOLS_KEY`. */
const MARKER_OPTIONS_KEY = "mework";
const MARKER_TOOLS_KEY = "toolAddition";

/**
 * The text a Responses-bound marker carries through the SDK, which has no
 * `additional_tools` item of its own: the SDK turns the message into a
 * developer item, and `toolAppendResponsesFetch` finds it by this prefix and
 * replaces it. The NUL keeps it from ever matching text a person wrote.
 */
const RESPONSES_SENTINEL = "\u0000mework:additional_tools:";

/** The tools a host marker names, or `null` when `message` is not one. */
export function markerTools(message: unknown): string[] | null {
  if (!isObject(message) || message.role !== "system") return null;
  const options = message.providerOptions;
  if (!isObject(options)) return null;
  const marker = options[MARKER_OPTIONS_KEY];
  if (!isObject(marker)) return null;
  const tools = marker[MARKER_TOOLS_KEY];
  if (!Array.isArray(tools)) return null;
  return tools.filter((name): name is string => typeof name === "string");
}

type AppendMode = "anthropic" | "responses" | "none";

function appendMode(family: ProviderFamily, toolAppend: boolean): AppendMode {
  if (!toolAppend) return "none";
  switch (family) {
    case "anthropic":
      return "anthropic";
    case "openai-responses":
    case "openai-codex":
    case "azure":
      return "responses";
    default:
      return "none";
  }
}

export interface PreparedToolAppend {
  /** The messages with every marker replaced or removed. */
  messages: unknown[];
  /** Tools to declare `defer_loading` (Anthropic only). */
  deferred: Set<string>;
  /** Whether any system message remains among `messages`. */
  systemInMessages: boolean;
}

/**
 * Anthropic's placement rule for a tool-change message: right after a user
 * turn (a tool result is one) and before an assistant turn or at the end,
 * counting a run of system messages — an appended system prompt beside it —
 * as one. A marker the host put anywhere else — after the model's own text,
 * when a run started with no message of its own — falls back to the declared
 * list.
 */
function anthropicPlacementHolds(messages: unknown[], index: number): boolean {
  return anthropicSystemPlacementHolds(messages, index);
}

/**
 * Turns every host marker in `messages` into the family's own addition, or
 * drops it where `toolAppend` (the host's word for this model at this
 * endpoint) is off.
 *
 * Only tools the request actually declares count: a marker keeps its place in
 * the history after one of its tools is withdrawn, and an addition naming a
 * tool the request does not carry would be refused.
 */
export function prepareToolAppend(
  family: ProviderFamily,
  messages: unknown[],
  tools: readonly ToolSpec[],
  toolAppend: boolean,
): PreparedToolAppend {
  const declared = new Set(tools.map((tool) => tool.name));
  const mode = appendMode(family, toolAppend);
  const deferred = new Set<string>();
  const out: unknown[] = [];
  let systemInMessages = false;
  messages.forEach((message, index) => {
    const named = markerTools(message);
    if (named === null) {
      out.push(message);
      return;
    }
    const names = named.filter((name) => declared.has(name));
    if (names.length === 0 || mode === "none") return;
    if (mode === "anthropic") {
      if (!anthropicPlacementHolds(messages, index)) return;
      for (const name of names) deferred.add(name);
      out.push({
        role: "system",
        content: "",
        providerOptions: {
          anthropic: { toolChanges: names.map((toolName) => ({ type: "tool_addition", toolName })) },
        },
      });
    } else {
      out.push({ role: "system", content: `${RESPONSES_SENTINEL}${JSON.stringify(names)}` });
    }
    systemInMessages = true;
  });
  return { messages: out, deferred, systemInMessages };
}

// ------------------------------------------------------------ Responses

/** Endpoint-and-model pairs that refused an `additional_tools` item once. */
const refusedAdditionalTools = new Set<string>();

function requestUrlOf(input: Parameters<typeof globalThis.fetch>[0]): string {
  try {
    const url = typeof input === "object" && "url" in input ? input.url : String(input);
    const parsed = new URL(url);
    parsed.hash = "";
    parsed.search = "";
    return parsed.toString();
  } catch {
    return "";
  }
}

function itemText(item: JsonObject): string | null {
  const content = item.content;
  if (typeof content === "string") return content;
  if (!Array.isArray(content) || content.length !== 1) return null;
  const only = content[0];
  return isObject(only) && typeof only.text === "string" ? only.text : null;
}

/** The tools a sentinel item names, or `null` when `item` is not one. */
function sentinelTools(item: unknown): string[] | null {
  if (!isObject(item) || (item.role !== "developer" && item.role !== "system")) return null;
  const text = itemText(item);
  if (text === null || !text.startsWith(RESPONSES_SENTINEL)) return null;
  try {
    const names = JSON.parse(text.slice(RESPONSES_SENTINEL.length));
    return Array.isArray(names) ? names.filter((name): name is string => typeof name === "string") : [];
  } catch {
    return [];
  }
}

function toolName(tool: unknown): string | undefined {
  return isObject(tool) && tool.type === "function" && typeof tool.name === "string" ? tool.name : undefined;
}

/**
 * The request with each sentinel item replaced by an `additional_tools` item
 * and the tools it hands over left out of `tools`; or, when `append` is off,
 * with the sentinels dropped and every tool left declared.
 */
function rewriteResponsesBody(body: JsonObject, append: boolean): boolean {
  const input = body.input;
  if (!Array.isArray(input) || !input.some((item) => sentinelTools(item) !== null)) return false;
  const tools = Array.isArray(body.tools) ? body.tools : [];
  const byName = new Map<string, unknown>();
  for (const tool of tools) {
    const name = toolName(tool);
    if (name !== undefined) byName.set(name, tool);
  }
  const handed = new Set<string>();
  const next: unknown[] = [];
  for (const item of input) {
    const names = sentinelTools(item);
    if (names === null) {
      next.push(item);
      continue;
    }
    if (!append) continue;
    const definitions = names.flatMap((name) => {
      const tool = byName.get(name);
      return tool === undefined ? [] : [tool];
    });
    if (definitions.length === 0) continue;
    for (const name of names) if (byName.has(name)) handed.add(name);
    next.push({ type: "additional_tools", role: "developer", tools: definitions });
  }
  body.input = next;
  if (handed.size > 0) {
    body.tools = tools.filter((tool) => {
      const name = toolName(tool);
      return name === undefined || !handed.has(name);
    });
  }
  return true;
}

/** Whether a 400 is the endpoint refusing the item rather than anything else. */
function blamesAdditionalTools(text: string): boolean {
  const message = text.toLowerCase();
  return message.includes("additional_tools");
}

/**
 * Wraps a Responses `fetch` so a host marker reaches the endpoint as an
 * `additional_tools` item. `append` is the host's word that the model at this
 * endpoint takes one (`StepRequest.toolAppend`); off, a stray sentinel is
 * dropped and every tool stays declared. A 400 that names the item turns
 * appends off for that endpoint and model and resends the request with the
 * tools declared.
 */
export function toolAppendResponsesFetch(
  inner: typeof globalThis.fetch = globalThis.fetch,
  appendTools = false,
): typeof globalThis.fetch {
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
    const key = `${url}|${typeof body.model === "string" ? body.model : ""}`;
    const append = appendTools && !refusedAdditionalTools.has(key);
    const fallback = JSON.parse(JSON.stringify(body)) as JsonObject;
    if (!rewriteResponsesBody(body, append)) return inner(input, init);
    const response = await inner(input, { ...init, body: JSON.stringify(body) });
    if (!append || response.status !== 400) return response;
    const errorText = await response.text();
    if (!blamesAdditionalTools(errorText)) {
      return new Response(errorText, { status: response.status, statusText: response.statusText, headers: response.headers });
    }
    refusedAdditionalTools.add(key);
    rewriteResponsesBody(fallback, false);
    return inner(input, { ...init, body: JSON.stringify(fallback) });
  };
}
