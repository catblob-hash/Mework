import type { ContextItem, Conversation, ConversationBranch } from "../types";

export interface ContextBranchNavigation {
  activeIndex: number;
  branchIds: string[];
}

export interface ForkConversationResult {
  conversation: Conversation;
  requestContexts: ContextItem[];
  createdBranch: boolean;
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

/** Creates a new active branch while retaining the previous suffix verbatim. */
export function forkConversationAtUser(
  conversation: Conversation,
  contextId: string,
  createBranchId: () => string,
  now: string
): ForkConversationResult | null {
  const index = regularUserIndex(conversation.contexts, contextId);
  if (index < 0) return null;

  const requestContexts = conversation.contexts.slice(0, index + 1);
  const suffix = conversation.contexts.slice(index + 1);
  const siblings = branchesAt(conversation, contextId);
  const active = siblings.find((branch) => branch.active);

  // A last, unanswered user message can be sent directly. Once a fork point
  // exists, however, an empty suffix is still a meaningful branch (for
  // example, a failed run) and must be retained before another run.
  if (!suffix.length && !siblings.length) {
    return { conversation, requestContexts, createdBranch: false };
  }
  if (siblings.length && !active) return null;

  const archivedId = active?.id ?? createBranchId();
  const nextActiveId = createBranchId();
  if (archivedId === nextActiveId) return null;

  const branches = conversation.branches.map((branch) => branch.id === active?.id ? {
    ...branch,
    active: false,
    contexts: suffix,
    updatedAt: now
  } : branch);
  if (!active) {
    branches.push({
      id: archivedId,
      forkContextId: contextId,
      active: false,
      contexts: suffix,
      createdAt: conversation.createdAt,
      updatedAt: now
    });
  }
  branches.push({
    id: nextActiveId,
    forkContextId: contextId,
    active: true,
    contexts: [],
    createdAt: now,
    updatedAt: now
  });

  return {
    conversation: {
      ...conversation,
      contexts: requestContexts,
      branches,
      updatedAt: now
    },
    requestContexts,
    createdBranch: true
  };
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
