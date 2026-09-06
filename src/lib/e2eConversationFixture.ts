import type { Conversation, ConversationSettings } from "../types";
import { defaultConversationWebSearchSettings } from "./runtime";

// Shared conversation construction for the browser E2E runners.
//
// Both runners used to start from `documentValue.workspaces.flatMap(w => w.conversations).at(0)`
// and clone whatever they found. The product seed has no conversations at all — both workspaces
// in `createSeedDocument` ship `conversations: []`, and the Rust `product_default_document` does
// the same — so that read has nothing to return and each runner aborts during initialization,
// before it reaches a single mock request. Cloning a found conversation was also silently
// inheriting whatever settings it carried, which is the opposite of what a deterministic
// harness wants.
//
// This module builds a conversation from an explicit, complete `ConversationSettings` instead.
// It touches no DOM and reads no document, so both the runners and Vitest execute the very same
// construction path. Product behaviour is untouched: a fresh install still opens with no
// conversation.

/**
 * Every field of `ConversationSettings`, spelled out.
 *
 * The two defaults that differ from the product's own are deliberate and belong to the harness:
 * `securityLevel: "full_access"` because no human is present to answer an approval prompt, and
 * `reasoningEffort: "disabled"` because a thinking budget makes the transcript non-deterministic.
 * Everything else starts from the most restrictive value, so a runner that needs a tool, a
 * memory tier or a skill has to name it.
 */
export function e2eConversationSettings(
  overrides: Partial<ConversationSettings> = {}
): ConversationSettings {
  return {
    systemPrompt: "",
    includeAppDataPath: false,
    enabledTools: [],
    hookIds: [],
    skillIds: [],
    mcpIds: [],
    toolDescriptionFileId: null,
    agentDefinitions: [],
    allowRolelessSubagents: false,
    webSearch: defaultConversationWebSearchSettings(),
    reasoningEffort: "disabled",
    securityLevel: "full_access",
    // Memory tools derive from these two switches rather than from `enabledTools`; leaving
    // either on would add three tools to every request and break exact-allowlist assertions.
    globalMemoryEnabled: false,
    projectMemoryEnabled: false,
    skillToolEnabled: false,
    ...overrides,
    // Nested values are rebuilt so a caller's partial override cannot alias — or half-fill —
    // the shared default object.
    ...(overrides.webSearch
      ? {
        webSearch: {
          ...defaultConversationWebSearchSettings(),
          ...overrides.webSearch,
          provider: { ...overrides.webSearch.provider }
        }
      }
      : {})
  };
}

export interface E2eConversationOptions {
  id: string;
  title: string;
  /** ISO timestamp used for both `createdAt` and `updatedAt`. Defaults to now. */
  createdAt?: string;
  settings?: Partial<ConversationSettings>;
}

/**
 * A conversation with no history: empty contexts, queue and branches, no worktree, no parent.
 * The caller decides which workspace it belongs to.
 */
export function createE2eConversation({
  id,
  title,
  createdAt = new Date().toISOString(),
  settings = {}
}: E2eConversationOptions): Conversation {
  return {
    id,
    title,
    createdAt,
    updatedAt: createdAt,
    settings: e2eConversationSettings(settings),
    contexts: [],
    queuedMessages: [],
    branches: [],
    userAbortedTasks: [],
    worktree: null,
    runTarget: null,
    parentConversationId: null
  };
}
