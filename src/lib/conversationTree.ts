import type { Conversation } from "../types";

export function detachAbsentParents(conversations: Conversation[]): Conversation[] {
  const ids = new Set(conversations.map((conversation) => conversation.id));
  return conversations.map((conversation) => conversation.parentConversationId
    && !ids.has(conversation.parentConversationId)
    ? { ...conversation, parentConversationId: null }
    : conversation);
}

export interface ConversationTree {
  /** Conversations rendered at the top level, in input order. */
  roots: Conversation[];
  /** Children of a parent id, in input order. Roots never appear here. */
  childrenOf: ReadonlyMap<string, Conversation[]>;
}

/**
 * Walks up the parent chain from `conversation` and reports whether the walk
 * returns to the starting conversation, i.e. the conversation sits on a cycle.
 * The walk always terminates: it stops at a missing parent or at the first id
 * it has already seen.
 */
function sitsOnCycle(byId: ReadonlyMap<string, Conversation>, conversation: Conversation): boolean {
  const seen = new Set<string>([conversation.id]);
  let current = conversation.parentConversationId;
  while (current) {
    if (current === conversation.id) return true;
    if (seen.has(current)) return false;
    seen.add(current);
    const parent = byId.get(current);
    if (!parent) return false;
    current = parent.parentConversationId;
  }
  return false;
}

/**
 * Groups conversations into a parent/child forest. A conversation is a root
 * when its parent id is null, names an id absent from the list, or sits on a
 * cycle (including a self-parent). Because every member of a cycle becomes a
 * root and roots never keep their incoming edge, the retained edges are acyclic
 * and every conversation is reachable exactly once.
 */
export function buildConversationTree(conversations: Conversation[]): ConversationTree {
  const byId = new Map<string, Conversation>();
  for (const conversation of conversations) byId.set(conversation.id, conversation);

  const roots: Conversation[] = [];
  const childrenOf = new Map<string, Conversation[]>();
  for (const conversation of conversations) {
    const parentId = conversation.parentConversationId;
    const parent = parentId ? byId.get(parentId) : undefined;
    if (!parent || parent.id === conversation.id || sitsOnCycle(byId, conversation)) {
      roots.push(conversation);
      continue;
    }
    const siblings = childrenOf.get(parent.id);
    if (siblings) siblings.push(conversation);
    else childrenOf.set(parent.id, [conversation]);
  }
  return { roots, childrenOf };
}

/**
 * Parent, grandparent, ... of `conversationId`, nearest first. Stops at a
 * missing parent and never repeats an id, so a cycle yields a finite list.
 */
export function conversationAncestorIds(conversations: Conversation[], conversationId: string): string[] {
  const byId = new Map<string, Conversation>();
  for (const conversation of conversations) byId.set(conversation.id, conversation);

  const ancestors: string[] = [];
  const seen = new Set<string>([conversationId]);
  let current = byId.get(conversationId)?.parentConversationId ?? null;
  while (current && !seen.has(current)) {
    const parent = byId.get(current);
    if (!parent) break;
    ancestors.push(parent.id);
    seen.add(parent.id);
    current = parent.parentConversationId;
  }
  return ancestors;
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
