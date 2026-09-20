//! Native, provider-executed `web_search` tool.
//!
//! Select factories by wire-protocol family rather than vendor or model ID. The native search
//! backend can only be the conversation's model provider, not a configured search provider.
//!
//! - Anthropic spells the version into the tool's own `type`, so the host may name one:
//!   `web_search_20250305` is basic search and `web_search_20260209` filters results with
//!   model-written code first. An unnamed or unknown version falls back to the basic one,
//!   because the SDK silently drops a provider tool it cannot map and the request would then
//!   go out with no search at all.
//! - Responses has no per-request max_uses parameter. The step driver still counts
//!   reported search calls and stops granting search across continuation requests.
//! - `openai-chat` and `openai-compatible` return `null`: Chat Completions has no
//!   provider-executed tool concept, and compatible providers discard those tools. The host can
//!   then return a recoverable rejection rather than silently skipping search.

import type { ProviderFamily } from "./protocol.js";

/** Name of a provider-defined AI SDK tool. */
const TOOL_NAME = "web_search";

interface ToolHost {
  tools?: Record<string, (options?: Record<string, unknown>) => unknown>;
}

/**
 * The Messages `web_search` versions this build can emit, by wire `type`.
 *
 * Kept as a table rather than derived from the `type` string because the set is
 * bounded by the installed AI SDK, not by the API: a version the provider
 * cannot map is dropped from the request with a warning, so the host must not
 * be able to offer one. `web_search.rs::NATIVE_SEARCH_TOOL_TYPES` holds the
 * same list and a parity test reads this file to keep the two in step.
 */
const SEARCH_TOOL_FACTORIES: Record<string, string> = {
  web_search_20250305: "webSearch_20250305",
  web_search_20260209: "webSearch_20260209",
};

/** The same table for `web_fetch`; see [`SEARCH_TOOL_FACTORIES`]. */
const FETCH_TOOL_FACTORIES: Record<string, string> = {
  web_fetch_20250910: "webFetch_20250910",
  web_fetch_20260209: "webFetch_20260209",
};

/**
 * Resolves a requested wire `type` to a factory on the provider, falling back to
 * the basic version. Both failure modes — a version this build does not know and
 * a version the installed SDK dropped — take the fallback rather than returning
 * nothing, because a conversation with web access switched on is better served
 * by an older search tool than by none.
 */
function messagesToolFactory(
  tools: NonNullable<ToolHost["tools"]>,
  table: Record<string, string>,
  fallbackType: string,
  requestedType: string | undefined,
): ((options?: Record<string, unknown>) => unknown) | undefined {
  const requested = requestedType ? table[requestedType] : undefined;
  const fallback = table[fallbackType];
  return (requested ? tools[requested] : undefined)
    ?? (fallback ? tools[fallback] : undefined);
}

/**
 * Returns the native search tool for this request, or `null` when the family lacks one.
 *
 * The result can be spread directly into `streamText({ tools })`.
 */
export function nativeSearchTool(
  family: ProviderFamily,
  provider: unknown,
  maxUses: number,
  toolType?: string,
): Record<string, unknown> | null {
  const tools = (provider as ToolHost | undefined)?.tools;
  if (!tools) return null;

  switch (family) {
    case "openai-responses":
    case "openai-codex":
    case "azure": {
      // Responses has no `max_uses` equivalent.
      const factory = tools.webSearch ?? tools.webSearchPreview;
      return factory ? { [TOOL_NAME]: factory({}) } : null;
    }
    case "anthropic":
    case "bedrock": {
      const factory = messagesToolFactory(
        tools,
        SEARCH_TOOL_FACTORIES,
        "web_search_20250305",
        toolType,
      );
      if (!factory) return null;
      // Zero means unlimited, so omit the field.
      return { [TOOL_NAME]: factory(maxUses > 0 ? { maxUses } : {}) };
    }
    case "google":
    case "vertex": {
      const factory = tools.googleSearch;
      return factory ? { google_search: factory({}) } : null;
    }
    case "xai": {
      const factory = tools.webSearch;
      return factory ? { [TOOL_NAME]: factory({}) } : null;
    }
    case "openai-chat":
    case "openai-compatible":
      return null;
    case "claude-agent":
      // Claude Code's own WebSearch is a built-in tool the host switches off; the
      // host performs searches itself for this family.
      return null;
    default: {
      const exhaustive: never = family;
      throw new Error(`未知的适配家族：${String(exhaustive)}`);
    }
  }
}

/** Whether this family supports native search. The host checks it before deriving a task. */
export function familySupportsNativeSearch(family: ProviderFamily): boolean {
  return (
    family === "openai-responses" ||
    family === "openai-codex" ||
    family === "azure" ||
    family === "anthropic" ||
    family === "bedrock" ||
    family === "google" ||
    family === "vertex" ||
    family === "xai"
  );
}

/**
 * Returns the native page-fetch tool for this request, or `null` when the family
 * lacks one.
 *
 * Only Anthropic has a server-side fetch tool whose result the host can read as
 * text: a `web_fetch_result` carries `content.source.{type:"text", data}`. The
 * Responses API has no page-fetch tool at all — its `open_page` is an internal
 * navigation action of the search tool, and its result never reaches the caller
 * — and neither do Google or xAI. Citations are left off because the host reads
 * the document body directly rather than asking the model to quote it.
 */
export function nativeFetchTool(
  family: ProviderFamily,
  provider: unknown,
  maxUses: number,
  toolType?: string,
): Record<string, unknown> | null {
  const tools = (provider as ToolHost | undefined)?.tools;
  if (!tools) return null;
  if (family !== "anthropic" && family !== "bedrock") return null;
  const factory = messagesToolFactory(
    tools,
    FETCH_TOOL_FACTORIES,
    "web_fetch_20250910",
    toolType,
  );
  if (!factory) return null;
  return { web_fetch: factory(maxUses > 0 ? { maxUses } : {}) };
}

/**
 * Whether this family has a server-side fetch tool the host can read text out of.
 * Must match the host's `web_search.rs::family_supports_native_fetch` exactly.
 */
export function familySupportsNativeFetch(family: ProviderFamily): boolean {
  return family === "anthropic" || family === "bedrock";
}
