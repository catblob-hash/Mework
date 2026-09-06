import type { ApiProvider } from "../types";
import { isClaudeAgentProvider } from "./claudeAgentProvider";
import { createId } from "./id";

export const CODEX_PROVIDER_FAMILY = "openai_codex" as const;
export const CODEX_PROVIDER_NAME = "OpenAI Codex";
export const CODEX_DEFAULT_BASE_URL = "https://chatgpt.com/backend-api/codex";
/** The host's rejection message for a sign-in the user cancelled; it is an outcome, not an error to display. */
export const CODEX_SIGN_IN_CANCELLED_MESSAGE = "Codex 登录已取消";

/** The built-in row is identified by family, not by id: ids stay random UUIDs so credentials never collide across data domains. */
export function isCodexProvider(provider: Pick<ApiProvider, "family">): boolean {
  return provider.family === CODEX_PROVIDER_FAMILY;
}

/** Alias with the product meaning: built-in ⇔ a row the user cannot create or delete. */
export function isBuiltinProvider(provider: Pick<ApiProvider, "family">): boolean {
  return isCodexProvider(provider) || isClaudeAgentProvider(provider);
}

/**
 * Ensure exactly one Codex row: keep the first existing one (and drop later
 * duplicates), or append a fresh disabled row. Existing row order is retained.
 */
export function ensureCodexProvider(providers: ApiProvider[]): ApiProvider[] {
  let foundCodex = false;
  const deduplicated = providers.filter((provider) => {
    if (!isCodexProvider(provider)) {
      return true;
    }
    if (foundCodex) {
      return false;
    }
    foundCodex = true;
    return true;
  });

  if (foundCodex) {
    return deduplicated;
  }

  return [
    ...deduplicated,
    {
      id: createId("provider"),
      name: CODEX_PROVIDER_NAME,
      enabled: false,
      family: CODEX_PROVIDER_FAMILY,
      baseUrl: "",
      familySettings: {},
      endpointBaseUrls: {},
      notes: "",
      models: [],
      activeModelId: null,
    },
  ];
}
