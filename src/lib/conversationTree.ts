import type { Conversation } from "../types";

export function detachAbsentParents(conversations: Conversation[]): Conversation[] {
  const ids = new Set(conversations.map((conversation) => conversation.id));
  return conversations.map((conversation) => conversation.parentConversationId
    && !ids.has(conversation.parentConversationId)
    ? { ...conversation, parentConversationId: null }
    : conversation);
}

/**
 * Re-parents the children of `deletedId` onto the deleted conversation's own
 * parent (or to the top level when it had none). The deleted conversation is
 * left in place; callers remove it separately. Conversations that keep their
 * parent are returned by identity so React can skip them.
 */
export function reparentChildren(conversations: Conversation[], deletedId: string): Conversation[] {
  const deleted = conversations.find((conversation) => conversation.id === deletedId);
  const grandparentId = deleted?.parentConversationId ?? null;
  return conversations.map((conversation) => {
    if (conversation.parentConversationId !== deletedId) return conversation;
    // A conversation may never become its own parent.
    const nextParent = grandparentId === conversation.id ? null : grandparentId;
    return { ...conversation, parentConversationId: nextParent };
  });
}
