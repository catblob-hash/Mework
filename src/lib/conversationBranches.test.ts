import { describe, expect, it } from "vitest";
import { createTestDocument as createSeedDocument } from "../test/fixtures";
import type { ContextItem, Conversation } from "../types";
import {
  contextBranchNavigations,
  forkConversationAtUser,
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

describe("conversation branches", () => {
  it("forks at an existing user without copying the common prefix", () => {
    const source = conversation([
      text("sys", "system"),
      text("u1", "user"),
      text("a1", "assistant"),
      text("u2", "user"),
      text("a2", "assistant")
    ]);
    const branchIds = ["old", "new"];
    const result = forkConversationAtUser(source, "u1", () => branchIds.shift()!, "2026-07-20T01:00:00Z")!;

    expect(ids(result.requestContexts)).toEqual(["sys", "u1"]);
    expect(ids(result.conversation.contexts)).toEqual(["sys", "u1"]);
    expect(result.conversation.contexts.filter((item) => item.id === "u1")).toHaveLength(1);
    expect(result.conversation.branches).toEqual([
      expect.objectContaining({ id: "old", forkContextId: "u1", active: false }),
      expect.objectContaining({ id: "new", forkContextId: "u1", active: true, contexts: [] })
    ]);
    expect(ids(result.conversation.branches[0].contexts)).toEqual(["a1", "u2", "a2"]);
    expect(source.branches).toEqual([]);
    expect(ids(source.contexts)).toEqual(["sys", "u1", "a1", "u2", "a2"]);
  });

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

  it("sends an unanswered last user directly", () => {
    const last = conversation([text("u1", "user")]);
    const direct = forkConversationAtUser(last, "u1", () => "unused", "2026-07-20T01:00:00Z")!;
    expect(direct.createdBranch).toBe(false);
    expect(direct.conversation).toBe(last);
  });

});
