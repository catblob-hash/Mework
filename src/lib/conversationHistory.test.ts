import { describe, expect, it } from "vitest";
import {
  conversationHistoryTarget,
  EMPTY_CONVERSATION_HISTORY,
  visitConversation
} from "./conversationHistory";
import type { ConversationHistory } from "./conversationHistory";

function visitAll(ids: string[], workspaceId = "w"): ConversationHistory {
  return ids.reduce(
    (history, conversationId) => visitConversation(history, { workspaceId, conversationId }),
    EMPTY_CONVERSATION_HISTORY
  );
}

const everywhere = () => "w";

describe("conversation history", () => {
  it("appends visits and ignores landing on the current entry again", () => {
    const history = visitAll(["a", "b", "b", "c"]);
    expect(history.entries.map((entry) => entry.conversationId)).toEqual(["a", "b", "c"]);
    expect(history.index).toBe(2);
  });

  it("steps back and forward, and has nowhere to go past either end", () => {
    const history = visitAll(["a", "b", "c"]);
    expect(conversationHistoryTarget(history, "c", -1, everywhere)).toMatchObject({ index: 1, conversationId: "b" });
    expect(conversationHistoryTarget(history, "c", 1, everywhere)).toBeNull();

    const back = { ...history, index: 0 };
    expect(conversationHistoryTarget(back, "a", -1, everywhere)).toBeNull();
    expect(conversationHistoryTarget(back, "a", 1, everywhere)).toMatchObject({ index: 1, conversationId: "b" });
  });

  it("drops the entries ahead when a conversation is opened after going back", () => {
    const history = visitConversation({ ...visitAll(["a", "b", "c"]), index: 0 }, { workspaceId: "w", conversationId: "d" });
    expect(history.entries.map((entry) => entry.conversationId)).toEqual(["a", "d"]);
    expect(conversationHistoryTarget(history, "d", 1, everywhere)).toBeNull();
  });

  it("returns from the draft to the conversation it was opened from", () => {
    const history = visitAll(["a", "b"]);
    // The draft is not an entry, so the cursor still points at b.
    expect(conversationHistoryTarget(history, "draft", -1, everywhere)).toMatchObject({ index: 1, conversationId: "b" });
  });

  it("steps over conversations that were deleted and follows ones that moved", () => {
    const history = visitAll(["a", "gone", "moved", "c"]);
    const locate = (id: string) => (id === "gone" ? null : id === "moved" ? "elsewhere" : "w");
    expect(conversationHistoryTarget(history, "c", -1, locate)).toEqual({ index: 2, workspaceId: "elsewhere", conversationId: "moved" });
    expect(conversationHistoryTarget({ ...history, index: 2 }, "moved", -1, locate)).toMatchObject({ index: 0, conversationId: "a" });
  });
});
