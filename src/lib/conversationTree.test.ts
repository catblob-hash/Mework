import { describe, expect, it } from "vitest";
import type { Conversation } from "../types";
import { detachAbsentParents, reparentChildren } from "./conversationTree";

/** Only the identity fields the tree reads; the rest of the shape is irrelevant here. */
function conversation(id: string, parentConversationId: string | null = null): Conversation {
  return { id, title: id, parentConversationId } as Conversation;
}

describe("detachAbsentParents", () => {
  it("detaches moved parent or child edges, preserving grandchildren and batch moves", () => {
    const parent = conversation("p");
    const child = conversation("c", "p");
    const grandchild = conversation("g", "c");
    const source = detachAbsentParents([child, grandchild]);
    expect(source[0].parentConversationId).toBeNull();
    expect(source[1]).toBe(grandchild);
    expect(detachAbsentParents([child])[0].parentConversationId).toBeNull();
    expect(detachAbsentParents([parent, child])[1]).toBe(child);
    expect(detachAbsentParents([grandchild, child, parent])).toEqual([grandchild, child, parent]);
  });
});

describe("reparentChildren", () => {
  it("moves children onto the grandparent and keeps untouched items by identity", () => {
    const root = conversation("root");
    const doomed = conversation("doomed", "root");
    const child = conversation("child", "doomed");
    const sibling = conversation("sibling", "root");
    const list = [root, doomed, child, sibling];

    const next = reparentChildren(list, "doomed");

    expect(next).not.toBe(list);
    expect(next[0]).toBe(root);
    expect(next[1]).toBe(doomed);
    expect(next[3]).toBe(sibling);
    expect(next[2]).not.toBe(child);
    expect(next[2].parentConversationId).toBe("root");
    expect(child.parentConversationId).toBe("doomed");
  });

  it("lifts children to the top level when the deleted conversation was a root", () => {
    const list = [conversation("root"), conversation("child", "root")];

    const next = reparentChildren(list, "root");

    expect(next[1].parentConversationId).toBeNull();
  });

  it("leaves every conversation untouched when the deleted id is unknown", () => {
    const list = [conversation("root"), conversation("child", "root")];

    const next = reparentChildren(list, "absent");

    expect(next[0]).toBe(list[0]);
    expect(next[1]).toBe(list[1]);
  });
});
