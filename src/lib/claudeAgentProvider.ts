import type { ApiProvider } from "../types";
import { createId } from "./id";

export const CLAUDE_AGENT_PROVIDER_FAMILY = "claude_agent" as const;
export const CLAUDE_AGENT_PROVIDER_NAME = "Claude Agent";
/** The command the user runs in a terminal to sign the CLI in. */
export const CLAUDE_AGENT_LOGIN_COMMAND = "claude auth login";
/** Anthropic's terms for driving Claude Code as an agent runtime. */
export const CLAUDE_AGENT_LEGAL_URL = "https://code.claude.com/docs/en/legal-and-compliance";

/**
 * Model catalog for this family, mirroring the host's built-in registry: there is
 * no `GET /models` behind a local CLI, so both sides carry the same rows.
 *
 * `[1m]` is a Claude Code CLI concept (a per-session 1M context budget) that
 * never reaches the Messages API; it is passed through as the model id. Fable 5 /
 * 5.1 and Sonnet 5 are natively 1M and therefore have no twin.
 */
export const CLAUDE_AGENT_REGISTRY: ReadonlyArray<{
  id: string;
  name: string;
  contextWindow: number;
  maxOutputTokens: number;
}> = [
  { id: "claude-fable-5-1", name: "Claude Fable 5.1", contextWindow: 1000000, maxOutputTokens: 128000 },
  { id: "claude-fable-5", name: "Claude Fable 5", contextWindow: 1000000, maxOutputTokens: 128000 },
  { id: "claude-opus-5", name: "Claude Opus 5", contextWindow: 200000, maxOutputTokens: 128000 },
  { id: "claude-opus-5[1m]", name: "Claude Opus 5 (1M context)", contextWindow: 1000000, maxOutputTokens: 128000 },
  { id: "claude-sonnet-5", name: "Claude Sonnet 5", contextWindow: 1000000, maxOutputTokens: 128000 },
  { id: "claude-opus-4-8", name: "Claude Opus 4.8", contextWindow: 200000, maxOutputTokens: 128000 },
  { id: "claude-opus-4-8[1m]", name: "Claude Opus 4.8 (1M context)", contextWindow: 1000000, maxOutputTokens: 128000 },
  { id: "claude-opus-4-7", name: "Claude Opus 4.7", contextWindow: 200000, maxOutputTokens: 128000 },
  { id: "claude-opus-4-7[1m]", name: "Claude Opus 4.7 (1M context)", contextWindow: 1000000, maxOutputTokens: 128000 },
  { id: "claude-opus-4-6", name: "Claude Opus 4.6", contextWindow: 200000, maxOutputTokens: 128000 },
  { id: "claude-opus-4-6[1m]", name: "Claude Opus 4.6 (1M context)", contextWindow: 1000000, maxOutputTokens: 128000 },
  { id: "claude-sonnet-4-6", name: "Claude Sonnet 4.6", contextWindow: 200000, maxOutputTokens: 128000 },
  { id: "claude-sonnet-4-6[1m]", name: "Claude Sonnet 4.6 (1M context)", contextWindow: 1000000, maxOutputTokens: 128000 },
  { id: "claude-opus-4-5", name: "Claude Opus 4.5", contextWindow: 200000, maxOutputTokens: 64000 },
  { id: "claude-opus-4-1", name: "Claude Opus 4.1", contextWindow: 200000, maxOutputTokens: 32000 },
  { id: "claude-sonnet-4-5", name: "Claude Sonnet 4.5", contextWindow: 200000, maxOutputTokens: 64000 },
  { id: "claude-haiku-4-5", name: "Claude Haiku 4.5", contextWindow: 200000, maxOutputTokens: 64000 },
];

/**
 * The Claude Agent family drives the locally installed Claude Code executable
 * through the official Claude Agent SDK instead of speaking HTTP itself, and it
 * reuses that CLI's own login: it has neither an API key nor a base URL. Like
 * Codex it is a built-in row identified by family, not by a fixed id: ids stay
 * random UUIDs so credentials never collide across data domains.
 */
export function isClaudeAgentProvider(provider: Pick<ApiProvider, "family">): boolean {
  return provider.family === CLAUDE_AGENT_PROVIDER_FAMILY;
}

/**
 * Ensure exactly one Claude Agent row: keep the first existing one (and drop
 * later duplicates), or append a fresh disabled row. Existing row order is
 * retained. A `baseUrl` left over from the version that still offered one is
 * flattened rather than rejected — the host ignores it either way.
 */
export function ensureClaudeAgentProvider(providers: ApiProvider[]): ApiProvider[] {
  let found = false;
  const deduplicated = providers.filter((provider) => {
    if (!isClaudeAgentProvider(provider)) {
      return true;
    }
    if (found) {
      return false;
    }
    found = true;
    return true;
  }).map((provider) => (
    isClaudeAgentProvider(provider) && provider.baseUrl !== ""
      ? { ...provider, baseUrl: "" }
      : provider
  ));

  if (found) {
    return deduplicated;
  }

  return [
    ...deduplicated,
    {
      id: createId("provider"),
      name: CLAUDE_AGENT_PROVIDER_NAME,
      enabled: false,
      family: CLAUDE_AGENT_PROVIDER_FAMILY,
      baseUrl: "",
      familySettings: {},
      endpointBaseUrls: {},
      notes: "",
      models: [],
      activeModelId: null,
    },
  ];
}
