import { describe, expect, it } from "vitest";
import type { AppDocument, Conversation, ConversationForkOrigin } from "../types";
import { forkOrigin, forkTitle, nextForkNumber, staleForkTitles } from "./conversationForks";

/** Only the fields fork naming reads; the rest of the shape is irrelevant here. */
function conversation(id: string, title: string, forkOf?: ConversationForkOrigin): Conversation {
  return { id, title, forkOf } as Conversation;
}

function documentOf(...workspaces: Conversation[][]): AppDocument {
  return {
    workspaces: workspaces.map((conversations, index) => ({ id: `ws${index}`, conversations }))
  } as AppDocument;
}

describe("conversation forks", () => {
  it("names a fork after its origin and counts per origin", () => {
    const origin = conversation("a", "修复登录");
    const first = conversation("f1", "修复登录-fork-1", { conversationId: "a", number: 1 });
    const document = documentOf([origin, first]);

    expect(forkTitle("修复登录", 2)).toBe("修复登录-fork-2");
    expect(forkOrigin(document, origin)).toEqual({ conversationId: "a", title: "修复登录" });
    expect(nextForkNumber(document, "a")).toBe(2);
  });

  it("forks a fork under the first origin's name and numbering", () => {
    const origin = conversation("a", "修复登录");
    const first = conversation("f1", "修复登录-fork-1", { conversationId: "a", number: 1 });
    const second = conversation("f2", "修复登录-fork-2", { conversationId: "a", number: 2 });
    const document = documentOf([origin, first], [second]);

    expect(forkOrigin(document, first)).toEqual({ conversationId: "a", title: "修复登录" });
    expect(nextForkNumber(document, "a")).toBe(3);
  });

  it("keeps counting separately for two origins that share a title", () => {
    const one = conversation("a", "同名");
    const other = conversation("b", "同名");
    const document = documentOf([
      one,
      other,
      conversation("fa1", "同名-fork-1", { conversationId: "a", number: 1 }),
      conversation("fa2", "同名-fork-2", { conversationId: "a", number: 2 })
    ]);

    expect(nextForkNumber(document, "a")).toBe(3);
    expect(nextForkNumber(document, "b")).toBe(1);
  });

  it("recovers the origin's name from a fork whose origin is gone", () => {
    const orphan = conversation("f2", "修复登录-fork-2", { conversationId: "gone", number: 2 });
    const document = documentOf([orphan]);

    expect(forkOrigin(document, orphan)).toEqual({ conversationId: "gone", title: "修复登录" });
    expect(nextForkNumber(document, "gone")).toBe(3);
  });

  it("lists forks whose title fell behind their origin's", () => {
    const document = documentOf(
      [conversation("a", "新标题"), conversation("f1", "旧标题-fork-1", { conversationId: "a", number: 1 })],
      [
        conversation("f2", "新标题-fork-2", { conversationId: "a", number: 2 }),
        // Its origin is gone, so it keeps the title it last had.
        conversation("f3", "别的-fork-1", { conversationId: "gone", number: 1 }),
        // Not a fork: renamed by the user.
        conversation("f4", "我的名字")
      ]
    );

    expect(staleForkTitles(document)).toEqual([
      { workspaceId: "ws0", conversationId: "f1", title: "新标题-fork-1" }
    ]);
  });

  it("does not follow an origin that is itself a fork", () => {
    const document = documentOf([
      conversation("x", "x-fork-1", { conversationId: "y", number: 1 }),
      conversation("y", "y-fork-1", { conversationId: "x", number: 1 })
    ]);

    expect(staleForkTitles(document)).toEqual([]);
  });
});
