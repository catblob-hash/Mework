import type { ApiProvider, AppDocument, ContextItem, GlobalSettings, ModelProfile } from "../types";

export type GlobalSettingsChange = GlobalSettings | ((current: GlobalSettings) => GlobalSettings);

/**
 * Replaces the addressed contexts with the exact local-only markers committed
 * by the host, including transcripts nested inside subagent records.
 *
 * Used after host quarantine: the renderer still holds the refused tool card.
 * Installing (rather than deleting) the committed replacement keeps the
 * visible audit evidence and prevents the next save from offering the card
 * again.
 */
export interface ContextLocation {
  conversationId: string;
  contextId: string;
}

export interface QuarantinedContextReplacement extends ContextLocation {
  replacement: ContextItem;
}

export function applyQuarantinedContextReplacements(
  document: AppDocument,
  locations: readonly QuarantinedContextReplacement[]
): AppDocument {
  if (locations.length === 0) return document;
  const replacementsByConversation = new Map<string, Map<string, ContextItem>>();
  for (const location of locations) {
    const replacements =
      replacementsByConversation.get(location.conversationId) ?? new Map<string, ContextItem>();
    replacements.set(location.contextId, location.replacement);
    replacementsByConversation.set(location.conversationId, replacements);
  }
  let changed = false;

  const replace = (
    contexts: ContextItem[],
    replacements: ReadonlyMap<string, ContextItem>
  ): ContextItem[] => {
    const next = contexts.map((context) => {
      const replacement = replacements.get(context.id);
      if (replacement) {
        changed = true;
        return replacement;
      }
      if (context.kind !== "tool" || !context.subagent) return context;
      const nested = replace(context.subagent.contexts, replacements);
      if (nested === context.subagent.contexts) return context;
      return { ...context, subagent: { ...context.subagent, contexts: nested } };
    });
    return !next.some((item, index) => item !== contexts[index]) ? contexts : next;
  };

  const workspaces = document.workspaces.map((workspace) => ({
    ...workspace,
    conversations: workspace.conversations.map((conversation) => {
      const replacements = replacementsByConversation.get(conversation.id);
      if (!replacements) return conversation;
      return {
        ...conversation,
        contexts: replace(conversation.contexts, replacements),
        branches: conversation.branches.map((branch) => ({
          ...branch,
          contexts: replace(branch.contexts, replacements)
        }))
      };
    })
  }));
  return changed ? { ...document, workspaces } : document;
}

export function findConversation(
  document: AppDocument | null,
  workspaceId: string | null,
  conversationId: string | null
) {
  const workspace = document?.workspaces.find((item) => item.id === workspaceId) ?? null;
  const conversation = workspace?.conversations.find((item) => item.id === conversationId) ?? null;
  return { workspace, conversation };
}

/**
 * Replace or remove a conversation with the host-authoritative snapshot after `conversationSaveRejected`. The renderer must discard a rejected state so debounced saves do not repeatedly resubmit it; remove the local conversation when no authoritative version exists.
 */
export function conversationMatchesPersistenceGeneration(
  document: AppDocument | null,
  conversationId: string,
  updatedAt: string
): boolean {
  return (
    document?.workspaces
      .flatMap((workspace) => workspace.conversations)
      .some((conversation) => conversation.id === conversationId && conversation.updatedAt === updatedAt) ??
    false
  );
}

export function replaceConversationFromAuthority(
  current: AppDocument,
  authority: AppDocument,
  conversationId: string
): AppDocument {
  const authorityLocation = authority.workspaces
    .flatMap((workspace) =>
      workspace.conversations.map((conversation, index) => ({
        workspaceId: workspace.id,
        conversation,
        index
      }))
    )
    .find((item) => item.conversation.id === conversationId);
  const withoutRejected = current.workspaces.map((workspace) => ({
    ...workspace,
    conversations: workspace.conversations.filter((conversation) => conversation.id !== conversationId)
  }));
  if (!authorityLocation) {
    return { ...current, workspaces: withoutRejected };
  }
  return {
    ...current,
    workspaces: withoutRejected.map((workspace) => {
      if (workspace.id !== authorityLocation.workspaceId) return workspace;
      const conversations = [...workspace.conversations];
      conversations.splice(
        Math.min(authorityLocation.index, conversations.length),
        0,
        authorityLocation.conversation
      );
      return { ...workspace, conversations };
    })
  };
}

/** The document-wide provider and model choice; conversations do not own model selections. */
export function modelChoiceForConversation(latest: AppDocument): {
  provider: ApiProvider | undefined;
  model: ModelProfile | undefined;
} {
  const provider = latest.globalSettings.apiProviders.find(
    (item) => item.id === latest.globalSettings.activeProviderId
  );
  const model = provider?.models.find((item) => item.id === provider.activeModelId);
  return { provider, model };
}

/**
 * Apply a global-settings change. Functional changes must be evaluated against the newest settings in `current`; expanding them from a renderer snapshot could lose two updates in the same tick.
 */
export function applyGlobalSettingsChange(current: AppDocument, change: GlobalSettingsChange): AppDocument {
  const changed = typeof change === "function" ? change(current.globalSettings) : change;
  const next = changed.conversationPresets.some((preset) => preset.id === changed.defaultConversationPresetId)
    ? changed
    : { ...changed, defaultConversationPresetId: changed.conversationPresets[0]?.id ?? "" };
  // Presets are templates: editing or deleting one never propagates to conversations, whose settings are independent.
  return { ...current, globalSettings: next };
}
