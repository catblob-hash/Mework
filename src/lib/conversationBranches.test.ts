import { describe, expect, it } from "vitest";
import { createTestDocument as createSeedDocument } from "../test/fixtures";
import type { ContextItem, Conversation } from "../types";
import {
  contextBranchNavigations,
  switchConversationBranch
} from "./conversationBranches";

function text(id: string, kind: "system" | "user" | "assistant", content = id): ContextItem {
  return { id, kind, content, createdAt: `2026-07-20T00:00:0${id.length}Z` };
}

function conversation(contexts: ContextItem[]): Conversation {
  return {
    ...createSeedDocument().workspaces[0].conversations[0],
    id: "conversation-branches",
    contexts,
    branches: []
  };
}

function ids(contexts: ContextItem[]): string[] {
  return contexts.map((context) => context.id);
}

/** Builds branch fixtures: creates a new active branch while retaining the previous suffix verbatim. */
function forkConversationAtUser(
  conversation: Conversation,
  contextId: string,
  createBranchId: () => string,
  now: string
): { conversation: Conversation; requestContexts: ContextItem[]; createdBranch: boolean } | null {
  const index = conversation.contexts.findIndex((context) => context.id === contextId && context.kind === "user");
  if (index < 0) return null;

  const requestContexts = conversation.contexts.slice(0, index + 1);
  const suffix = conversation.contexts.slice(index + 1);
  const siblings = conversation.branches.filter((branch) => branch.forkContextId === contextId);
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

describe("conversation branches", () => {
  it("switches suffixes reversibly while branch positions stay stable", () => {
    const first = forkConversationAtUser(
      conversation([text("u1", "user"), text("old-answer", "assistant")]),
      "u1",
      (() => {
        const values = ["old", "new"];
        return () => values.shift()!;
      })(),
      "2026-07-20T01:00:00Z"
    )!.conversation;
    const withNewAnswer = { ...first, contexts: [...first.contexts, text("new-answer", "assistant")] };
    const oldActive = switchConversationBranch(withNewAnswer, "u1", "old", "2026-07-20T02:00:00Z")!;

    expect(ids(oldActive.contexts)).toEqual(["u1", "old-answer"]);
    expect(contextBranchNavigations(oldActive).u1).toEqual({
      activeIndex: 0,
      branchIds: ["old", "new"]
    });
    const newActive = switchConversationBranch(oldActive, "u1", "new", "2026-07-20T03:00:00Z")!;
    expect(ids(newActive.contexts)).toEqual(["u1", "new-answer"]);
    expect(contextBranchNavigations(newActive).u1.activeIndex).toBe(1);
  });

  it("keeps nested fork suffixes reachable through their parent branch", () => {
    const idsToCreate = ["root", "child", "child-old", "grandchild"];
    let current = forkConversationAtUser(
      conversation([text("u1", "user"), text("a1", "assistant"), text("u2", "user"), text("a2", "assistant")]),
      "u1",
      () => idsToCreate.shift()!,
      "2026-07-20T01:00:00Z"
    )!.conversation;
    current = {
      ...current,
      contexts: [...current.contexts, text("b1", "assistant"), text("u3", "user"), text("b2", "assistant")]
    };
    current = forkConversationAtUser(
      current,
      "u3",
      () => idsToCreate.shift()!,
      "2026-07-20T02:00:00Z"
    )!.conversation;
    current = { ...current, contexts: [...current.contexts, text("b3", "assistant")] };

    const root = switchConversationBranch(current, "u1", "root", "2026-07-20T03:00:00Z")!;
    expect(ids(root.contexts)).toEqual(["u1", "a1", "u2", "a2"]);
    expect(contextBranchNavigations(root).u3).toBeUndefined();
    const child = switchConversationBranch(root, "u1", "child", "2026-07-20T04:00:00Z")!;
    expect(ids(child.contexts)).toContain("u3");
    expect(contextBranchNavigations(child).u3).toBeDefined();
  });

});
