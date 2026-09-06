//! Native, provider-executed `web_search` tool.
//!
//! Select factories by wire-protocol family rather than vendor or model ID. The native search
//! backend can only be the conversation's model provider, not a configured search provider.
//!
//! - Anthropic uses the base `webSearch_20250305`; newer versions require a code-execution
//!   container that compatible endpoints may not provide.
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
 * Returns the native search tool for this request, or `null` when the family lacks one.
 *
 * The result can be spread directly into `streamText({ tools })`.
 */
export function nativeSearchTool(
  family: ProviderFamily,
  provider: unknown,
  maxUses: number,
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
      const factory = tools.webSearch_20250305;
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
