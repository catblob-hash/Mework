import type { ContextItem, Conversation, ConversationBranch } from "../types";

export interface ContextBranchNavigation {
  activeIndex: number;
  branchIds: string[];
}

function regularUserIndex(contexts: ContextItem[], contextId: string): number {
  return contexts.findIndex((context) => (
    context.id === contextId
    && context.kind === "user"
  ));
}

function branchesAt(conversation: Conversation, forkContextId: string): ConversationBranch[] {
  return conversation.branches.filter((branch) => branch.forkContextId === forkContextId);
}

/** Swaps the current suffix with one inactive slot at the same fork point. */
export function switchConversationBranch(
  conversation: Conversation,
  forkContextId: string,
  targetBranchId: string,
  now: string
): Conversation | null {
  const index = regularUserIndex(conversation.contexts, forkContextId);
  if (index < 0) return null;
  const siblings = branchesAt(conversation, forkContextId);
  const active = siblings.find((branch) => branch.active);
  const target = siblings.find((branch) => branch.id === targetBranchId && !branch.active);
  if (!active || !target) return null;

  const prefix = conversation.contexts.slice(0, index + 1);
  const currentSuffix = conversation.contexts.slice(index + 1);
  const branches = conversation.branches.map((branch) => {
    if (branch.id === active.id) {
      return { ...branch, active: false, contexts: currentSuffix, updatedAt: now };
    }
    if (branch.id === target.id) {
      return { ...branch, active: true, contexts: [], updatedAt: now };
    }
    return branch;
  });

  return {
    ...conversation,
    contexts: [...prefix, ...target.contexts],
    branches,
    updatedAt: now
  };
}

/** Navigation metadata only for fork messages visible on the active timeline. */
export function contextBranchNavigations(conversation: Conversation): Record<string, ContextBranchNavigation> {
  const visible = new Set(conversation.contexts
    .filter((context) => context.kind === "user")
    .map((context) => context.id));
  const grouped = new Map<string, ConversationBranch[]>();
  for (const branch of conversation.branches) {
    if (!visible.has(branch.forkContextId)) continue;
    const siblings = grouped.get(branch.forkContextId) ?? [];
    siblings.push(branch);
    grouped.set(branch.forkContextId, siblings);
  }

  return Object.fromEntries([...grouped].flatMap(([forkContextId, siblings]) => {
    const activeIndex = siblings.findIndex((branch) => branch.active);
    return siblings.length > 1 && activeIndex >= 0
      ? [[forkContextId, { activeIndex, branchIds: siblings.map((branch) => branch.id) }]]
      : [];
  }));
}

export function isConversationBranchFork(conversation: Conversation, contextId: string): boolean {
  return conversation.branches.some((branch) => branch.forkContextId === contextId);
}
