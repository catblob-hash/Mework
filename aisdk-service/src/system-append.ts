//! System prompts the host appended mid-conversation, handed to each protocol
//! as its own mid-conversation system message.
//!
//! The host records the point an instruction starts to apply — plan mode
//! switched on or off again, the request to hand off, a continuation's notes —
//! as one message: a `system` message whose
//! `providerOptions.mework.systemAppend` is `true` (Rust
//! `system_append::marker_message`). It never reaches a provider as that.
//! Here it becomes:
//!
//! - Anthropic Messages, on a model the host says takes it at this endpoint
//!   (`request.systemAppend`): a mid-conversation `role: "system"` message,
//!   where Anthropic's placement rule allows one — right after a user turn (a
//!   tool result is one), before an assistant turn or at the end. Consecutive
//!   system messages count as one section, so a tool addition beside it does
//!   not break the rule.
//! - OpenAI Responses, Azure and the Codex backend: a `system` / `developer`
//!   input item in place.
//! - The Chat protocols (OpenAI's, compatible endpoints, xAI), where the host
//!   says the model takes one: a `system` message in place.
//! - Everything else, and a marker that breaks a placement rule: its text is
//!   lifted into the tail of the system prompt — correct, and uncached from
//!   there on. The host appends none where the model does not take one (Rust
//!   `system_append::appends_system`; it sends the instruction in `box`
//!   instead), so this is left to a marker recorded before the conversation
//!   switched models.
//!
//! The sidecar does not second-guess the endpoint: `systemAppend` is the
//! model's declared capability, which the user declares for a relay that
//! keeps the message and Mework only where it knows.
//!
//! The Anthropic dialect keeps one more fallback for an endpoint that refuses
//! the message anyway (`anthropic-dialect.ts`, `liftSystemMessages`).

import type { StepRequest } from "./protocol.js";

type JsonObject = Record<string, unknown>;

function isObject(value: unknown): value is JsonObject {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/** Mirrors Rust `system_append::MARKER_OPTIONS_KEY` / `MARKER_KEY`. */
const MARKER_OPTIONS_KEY = "mework";
const MARKER_KEY = "systemAppend";

/** Separator between system prompt sections; mirrors the host's. */
const SYSTEM_SECTION_SEPARATOR = "\n\n";

/** The text of an appended system prompt, or `null` when `message` is not one. */
function systemAppendText(message: unknown): string | null {
  if (!isObject(message) || message.role !== "system") return null;
  const options = message.providerOptions;
  if (!isObject(options)) return null;
  const marker = options[MARKER_OPTIONS_KEY];
  if (!isObject(marker) || marker[MARKER_KEY] !== true) return null;
  return typeof message.content === "string" ? message.content : "";
}

type AppendMode = "anthropic" | "inline" | "lift";

/**
 * How this request carries an appended system prompt. Whether the model at
 * this endpoint takes one is the host's word (`request.systemAppend`: the
 * model's declared capability, which a relay's user declares and Mework
 * declares only where it knows); the protocol decides only the shape.
 */
function appendMode(request: Pick<StepRequest, "family" | "systemAppend">): AppendMode {
  if (request.systemAppend !== true) return "lift";
  switch (request.family) {
    case "anthropic":
      return "anthropic";
    case "openai-responses":
    case "openai-codex":
    case "azure":
    case "openai-chat":
    case "openai-compatible":
    case "xai":
      return "inline";
    default:
      // Google's, Bedrock's and Vertex's prompts put every system message at
      // the head, and the CLI writes its own requests.
      return "lift";
  }
}

/**
 * Anthropic's placement rule for a mid-conversation system message, applied to
 * the run of system messages `index` belongs to: the first message before the
 * run is a user turn (or a tool result), and the first after it is an
 * assistant turn or there is none. Shared with the tool-addition check, whose
 * messages are system messages too.
 */
export function anthropicSystemPlacementHolds(messages: readonly unknown[], index: number): boolean {
  const role = (at: number) => (isObject(messages[at]) ? messages[at].role : undefined);
  let before = index - 1;
  while (before >= 0 && role(before) === "system") before -= 1;
  let after = index + 1;
  while (after < messages.length && role(after) === "system") after += 1;
  const previous = before >= 0 ? role(before) : undefined;
  const next = after < messages.length ? role(after) : undefined;
  return (previous === "user" || previous === "tool") && (next === undefined || next === "assistant");
}

export interface PreparedSystemAppend {
  /** The messages with every appended system prompt kept as a plain system message or removed. */
  messages: unknown[];
  /** Texts to add to the tail of the system prompt, in transcript order. */
  lifted: string[];
}

/**
 * Turns every appended system prompt in `request.messages` into the
 * protocol's own mid-conversation system message, or lifts it into the system
 * prompt where the protocol, the endpoint or the placement cannot take it.
 */
function prepareSystemAppend(
  request: Pick<StepRequest, "family" | "systemAppend" | "messages">,
): PreparedSystemAppend {
  const mode = appendMode(request);
  const messages = request.messages;
  const out: unknown[] = [];
  const lifted: string[] = [];
  messages.forEach((message, index) => {
    const text = systemAppendText(message);
    if (text === null) {
      out.push(message);
      return;
    }
    if (text.trim().length === 0) return;
    const inPlace = mode === "inline" || (mode === "anthropic" && anthropicSystemPlacementHolds(messages, index));
    if (inPlace) out.push({ role: "system", content: text });
    else lifted.push(text);
  });
  return { messages: out, lifted };
}

/**
 * Applies `prepareSystemAppend` to the request itself: the messages are
 * replaced and lifted texts join `systemDynamic`, after whatever tail the host
 * already sent, so the stable prefix keeps its own cache breakpoint.
 */
export function applySystemAppend(request: StepRequest): void {
  if (!request.messages.some((message) => systemAppendText(message) !== null)) return;
  const prepared = prepareSystemAppend(request);
  request.messages = prepared.messages;
  if (prepared.lifted.length === 0) return;
  request.systemDynamic = [request.systemDynamic, ...prepared.lifted]
    .filter((part): part is string => typeof part === "string" && part.length > 0)
    .join(SYSTEM_SECTION_SEPARATOR);
}
