import { describe, expect, it } from "vitest";
import {
  CONVERSATION_VIEW,
  initialMainPaneState,
  mainPaneReducer,
  mainPaneShowsPage,
  mainPaneViewFor,
  mainPaneViewKey,
  previewSessionBelongsToConversation,
  previewSessionsFor,
  previewTabSessionId,
  sameMainPaneView
} from "./mainPanePages";
import type { MainPaneState } from "./mainPanePages";

function show(state: MainPaneState, conversationId: string, view: Parameters<typeof sameMainPaneView>[0]) {
  return mainPaneReducer(state, { type: "show", conversationId, view });
}

describe("mainPaneReducer", () => {
  it("starts every conversation on its own timeline", () => {
    expect(mainPaneViewFor(initialMainPaneState, "conversation-1")).toEqual(CONVERSATION_VIEW);
    expect(mainPaneShowsPage(mainPaneViewFor(initialMainPaneState, "conversation-1"))).toBe(false);
    expect(mainPaneViewFor(initialMainPaneState, null)).toEqual(CONVERSATION_VIEW);
  });

  it("keeps each conversation's page independent", () => {
    const first = show(initialMainPaneState, "conversation-1", { kind: "review", view: "changes" });
    const both = show(first, "conversation-2", { kind: "shell", shellTaskId: "shell-1" });

    expect(mainPaneViewFor(both, "conversation-1")).toEqual({ kind: "review", view: "changes" });
    expect(mainPaneViewFor(both, "conversation-2")).toEqual({ kind: "shell", shellTaskId: "shell-1" });
  });

  /** The review page is one page whose view changes, not five pages. */
  it("switches the review view in place", () => {
    const changes = show(initialMainPaneState, "conversation-1", { kind: "review", view: "changes" });
    const history = show(changes, "conversation-1", { kind: "review", view: "history" });

    expect(mainPaneViewFor(history, "conversation-1")).toEqual({ kind: "review", view: "history" });
  });

  it("returns to the conversation on back", () => {
    const shown = show(initialMainPaneState, "conversation-1", { kind: "preview", sessionId: "conversation-1" });
    const back = mainPaneReducer(shown, { type: "back", conversationId: "conversation-1" });

    expect(mainPaneViewFor(back, "conversation-1")).toEqual(CONVERSATION_VIEW);
    // Going back leaves the page's resource alone: the native session is still open and still a
    // task row, which is the only way the user gets back to it.
    expect(previewSessionsFor(back, "conversation-1")).toEqual(["conversation-1"]);
  });

  /**
   * An Agent tab must become reachable without taking the screen. The user may be reading the
   * conversation while the model opens three pages; none of them may interrupt that.
   */
  it("registers an Agent-opened preview without stealing the main pane", () => {
    const tab = previewTabSessionId("conversation-1", "agent-1");
    const registered = mainPaneReducer(initialMainPaneState, {
      type: "register_preview",
      conversationId: "conversation-1",
      sessionId: tab
    });

    expect(previewSessionsFor(registered, "conversation-1")).toEqual([tab]);
    expect(mainPaneViewFor(registered, "conversation-1")).toEqual(CONVERSATION_VIEW);
  });

  it("registers a preview implied by showing it, and does not duplicate it", () => {
    const shown = show(initialMainPaneState, "conversation-1", { kind: "preview", sessionId: "session-a" });
    const again = mainPaneReducer(shown, {
      type: "register_preview",
      conversationId: "conversation-1",
      sessionId: "session-a"
    });

    expect(previewSessionsFor(again, "conversation-1")).toEqual(["session-a"]);
    expect(again).toBe(shown);
  });

  it("keeps preview sessions in the order they appeared", () => {
    let state = mainPaneReducer(initialMainPaneState, {
      type: "register_preview",
      conversationId: "conversation-1",
      sessionId: "conversation-1"
    });
    state = mainPaneReducer(state, {
      type: "register_preview",
      conversationId: "conversation-1",
      sessionId: previewTabSessionId("conversation-1", "agent-1")
    });

    expect(previewSessionsFor(state, "conversation-1")).toEqual([
      "conversation-1",
      "conversation-1#agent-1"
    ]);
  });

  /**
   * There is no tab strip left to fall back along, so a closed page's only honest destination is
   * the conversation it belonged to.
   */
  it("falls back to the conversation when the shown preview is closed", () => {
    const shown = show(initialMainPaneState, "conversation-1", { kind: "preview", sessionId: "session-a" });
    const closed = mainPaneReducer(shown, {
      type: "forget_preview",
      conversationId: "conversation-1",
      sessionId: "session-a"
    });

    expect(previewSessionsFor(closed, "conversation-1")).toEqual([]);
    expect(mainPaneViewFor(closed, "conversation-1")).toEqual(CONVERSATION_VIEW);
  });

  it("leaves the shown page alone when a different preview closes", () => {
    let state = show(initialMainPaneState, "conversation-1", { kind: "preview", sessionId: "session-a" });
    state = mainPaneReducer(state, {
      type: "register_preview",
      conversationId: "conversation-1",
      sessionId: "session-b"
    });
    const closed = mainPaneReducer(state, {
      type: "forget_preview",
      conversationId: "conversation-1",
      sessionId: "session-b"
    });

    expect(previewSessionsFor(closed, "conversation-1")).toEqual(["session-a"]);
    expect(mainPaneViewFor(closed, "conversation-1")).toEqual({ kind: "preview", sessionId: "session-a" });
  });

  it("forgets everything a deleted conversation owned", () => {
    let state = show(initialMainPaneState, "conversation-1", { kind: "preview", sessionId: "session-a" });
    state = show(state, "conversation-2", { kind: "review", view: "changes" });
    const removed = mainPaneReducer(state, {
      type: "remove_conversation",
      conversationId: "conversation-1"
    });

    expect(previewSessionsFor(removed, "conversation-1")).toEqual([]);
    expect(mainPaneViewFor(removed, "conversation-1")).toEqual(CONVERSATION_VIEW);
    expect(mainPaneViewFor(removed, "conversation-2")).toEqual({ kind: "review", view: "changes" });
  });

  it("does not churn state for a no-op", () => {
    const shown = show(initialMainPaneState, "conversation-1", { kind: "shell", shellTaskId: "shell-1" });

    expect(show(shown, "conversation-1", { kind: "shell", shellTaskId: "shell-1" })).toBe(shown);
    expect(mainPaneReducer(shown, { type: "back", conversationId: "conversation-2" })).toBe(shown);
    expect(mainPaneReducer(shown, { type: "remove_conversation", conversationId: "conversation-9" })).toBe(shown);
  });
});

describe("main pane view identity", () => {
  it("distinguishes pages of the same kind", () => {
    expect(sameMainPaneView(
      { kind: "preview", sessionId: "a" },
      { kind: "preview", sessionId: "b" }
    )).toBe(false);
    expect(sameMainPaneView(
      { kind: "shell", shellTaskId: "shell-1" },
      { kind: "shell", shellTaskId: "shell-1" }
    )).toBe(true);
    expect(sameMainPaneView(
      { kind: "subagent", subagentId: "agent-1" },
      { kind: "preview", sessionId: "agent-1" }
    )).toBe(false);
  });

  /** One key per mounted page: preview pages stay mounted per session, review is a single page. */
  it("keys mounted pages by their resource", () => {
    expect(mainPaneViewKey({ kind: "preview", sessionId: "session-a" })).toBe("preview:session-a");
    expect(mainPaneViewKey({ kind: "shell", shellTaskId: "shell-1" })).toBe("shell:shell-1");
    expect(mainPaneViewKey({ kind: "review", view: "history" })).toBe("review");
    expect(mainPaneViewKey(CONVERSATION_VIEW)).toBe("conversation");
  });
});

describe("preview session ownership", () => {
  /**
   * The `#` separator is what lets the host recover the owning conversation — and therefore the
   * shared Chromium profile — from any tab id, so conversation ids never contain one.
   */
  it("recognises a conversation's own sessions and rejects a neighbour's", () => {
    expect(previewSessionBelongsToConversation("conversation-1", "conversation-1")).toBe(true);
    expect(previewSessionBelongsToConversation("conversation-1#agent-1", "conversation-1")).toBe(true);
    expect(previewSessionBelongsToConversation("conversation-12", "conversation-1")).toBe(false);
    expect(previewSessionBelongsToConversation("conversation-2#agent-1", "conversation-1")).toBe(false);
  });
});
