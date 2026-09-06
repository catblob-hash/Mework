import { describe, expect, it } from "vitest";
import type { Conversation } from "../types";
import { buildConversationTree, conversationAncestorIds, detachAbsentParents, reparentChildren } from "./conversationTree";

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

describe("buildConversationTree", () => {
  it("keeps roots and children in input order", () => {
    const first = conversation("a");
    const second = conversation("b");
    const childB = conversation("b-1", "b");
    const childA = conversation("a-1", "a");
    const secondChildB = conversation("b-2", "b");
    const grandchild = conversation("a-1-1", "a-1");

    const { roots, childrenOf } = buildConversationTree([first, second, childB, childA, secondChildB, grandchild]);

    expect(roots.map((item) => item.id)).toEqual(["a", "b"]);
    expect(childrenOf.get("b")?.map((item) => item.id)).toEqual(["b-1", "b-2"]);
    expect(childrenOf.get("a")?.map((item) => item.id)).toEqual(["a-1"]);
    expect(childrenOf.get("a-1")?.map((item) => item.id)).toEqual(["a-1-1"]);
    expect(childrenOf.get("missing")).toBeUndefined();
  });

  it("treats a dangling parent id as a root", () => {
    const orphan = conversation("orphan", "gone");
    const { roots, childrenOf } = buildConversationTree([orphan]);

    expect(roots.map((item) => item.id)).toEqual(["orphan"]);
    expect(childrenOf.size).toBe(0);
  });

  it("renders every conversation exactly once when parents form a cycle", () => {
    const left = conversation("left", "right");
    const right = conversation("right", "left");
    const selfParent = conversation("self", "self");
    const belowCycle = conversation("below", "left");

    const { roots, childrenOf } = buildConversationTree([left, right, selfParent, belowCycle]);

    // Cycle members become roots, so nothing is stranded and nothing renders twice.
    expect(roots.map((item) => item.id)).toEqual(["left", "right", "self"]);
    expect(childrenOf.get("left")?.map((item) => item.id)).toEqual(["below"]);
    const placed = [
      ...roots.map((item) => item.id),
      ...Array.from(childrenOf.values()).flatMap((items) => items.map((item) => item.id))
    ];
    expect(placed.slice().sort()).toEqual(["below", "left", "right", "self"]);
  });
});

describe("conversationAncestorIds", () => {
  it("lists parent then grandparent and stops at a dangling id", () => {
    const list = [
      conversation("root"),
      conversation("mid", "root"),
      conversation("leaf", "mid"),
      conversation("orphan", "gone")
    ];

    expect(conversationAncestorIds(list, "leaf")).toEqual(["mid", "root"]);
    expect(conversationAncestorIds(list, "root")).toEqual([]);
    expect(conversationAncestorIds(list, "orphan")).toEqual([]);
    expect(conversationAncestorIds(list, "absent")).toEqual([]);
  });

  it("terminates on a cycle", () => {
    const list = [conversation("left", "right"), conversation("right", "left"), conversation("below", "left")];

    expect(conversationAncestorIds(list, "below")).toEqual(["left", "right"]);
    expect(conversationAncestorIds(list, "left")).toEqual(["right"]);
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
