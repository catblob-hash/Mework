import type { AppDocument, Conversation, ConversationForkOrigin } from "../types";

/**
 * Naming for conversations forked from the timeline's context menu.
 *
 * A fork is titled `<origin title>-fork-<number>` and follows the origin's
 * title for as long as both exist. The origin of a fork of a fork is the first
 * conversation's, so every fork of one conversation shares one numbering, and
 * that numbering is keyed by the origin's id: two conversations that happen to
 * share a title count their forks separately.
 */

/** Not localized: the suffix is part of the name, and a language switch must not rename forks. */
export function forkTitle(originTitle: string, number: number): string {
  return `${originTitle}-fork-${number}`;
}

function conversationsById(document: AppDocument): Map<string, Conversation> {
  const byId = new Map<string, Conversation>();
  for (const workspace of document.workspaces) {
    for (const conversation of workspace.conversations) byId.set(conversation.id, conversation);
  }
  return byId;
}

/** Which conversation a new fork of `source` counts under, and the title it is named after. */
export function forkOrigin(document: AppDocument, source: Conversation): { conversationId: string; title: string } {
  const origin = source.forkOf;
  if (!origin) return { conversationId: source.id, title: source.title };
  const originConversation = conversationsById(document).get(origin.conversationId);
  if (originConversation) return { conversationId: originConversation.id, title: originConversation.title };
  // The origin is gone; the title this fork still carries is all that is left of its name.
  const suffix = `-fork-${origin.number}`;
  return {
    conversationId: origin.conversationId,
    title: source.title.endsWith(suffix) ? source.title.slice(0, -suffix.length) : source.title
  };
}

/** One past the highest number any living fork of the origin holds. */
export function nextForkNumber(document: AppDocument, originConversationId: string): number {
  let highest = 0;
  for (const workspace of document.workspaces) {
    for (const conversation of workspace.conversations) {
      if (conversation.forkOf?.conversationId === originConversationId) {
        highest = Math.max(highest, conversation.forkOf.number);
      }
    }
  }
  return highest + 1;
}

export interface ForkTitleUpdate {
  workspaceId: string;
  conversationId: string;
  title: string;
}

/** Forks whose title has fallen behind their origin's, with the title each should have now. */
export function staleForkTitles(document: AppDocument): ForkTitleUpdate[] {
  const byId = conversationsById(document);
  const updates: ForkTitleUpdate[] = [];
  for (const workspace of document.workspaces) {
    for (const conversation of workspace.conversations) {
      const forkOf: ConversationForkOrigin | null | undefined = conversation.forkOf;
      if (!forkOf) continue;
      const origin = byId.get(forkOf.conversationId);
      // A deleted origin leaves the fork the title it last had. An origin that is
      // itself a fork was not made here, and following it could chase a cycle.
      if (!origin || origin.forkOf) continue;
      const title = forkTitle(origin.title, forkOf.number);
      if (conversation.title !== title) {
        updates.push({ workspaceId: workspace.id, conversationId: conversation.id, title });
      }
    }
  }
  return updates;
}
