import { describe, expect, it } from "vitest";
import {
  activeTerminalTab,
  initialTerminalTabsState,
  terminalDisplayNumber,
  terminalTabId,
  terminalTabsFor,
  terminalTabsReducer
} from "./terminalTabs";
import type { TerminalTabsAction, TerminalTabsState } from "./terminalTabs";

const conversationId = "conversation-1";

function apply(state: TerminalTabsState, ...actions: TerminalTabsAction[]): TerminalTabsState {
  return actions.reduce(terminalTabsReducer, state);
}
function layout(state: TerminalTabsState, id = conversationId) {
  return terminalTabsFor(state, id);
}
function ids(state: TerminalTabsState, id = conversationId) {
  return layout(state, id).tabs.map((tab) => tab.id);
}
function add(state: TerminalTabsState, id = conversationId) {
  return apply(state, { type: "add", conversationId: id });
}
function close(state: TerminalTabsState, terminalId: string, id = conversationId) {
  return apply(state, { type: "close", conversationId: id, terminalId });
}

describe("terminal tabs", () => {
  it("shares one frozen opening layout across conversations and null", () => {
    const first = layout(initialTerminalTabsState);
    expect(first).toBe(terminalTabsFor(initialTerminalTabsState, "other"));
    expect(first).toBe(terminalTabsFor(initialTerminalTabsState, null));
    expect(Object.isFrozen(first)).toBe(true);
    expect(Object.isFrozen(first.tabs)).toBe(true);
    // Every conversation has a terminal before anyone asks for one, so the pane always has
    // something to park and the panel keeps a stable element to reopen into.
    expect(ids(initialTerminalTabsState)).toEqual([terminalTabId(1)]);
    expect(activeTerminalTab(first)?.id).toBe(terminalTabId(1));
  });

  it("does not resolve prototype properties as layouts", () => {
    expect(terminalTabsFor(initialTerminalTabsState, "constructor"))
      .toBe(layout(initialTerminalTabsState));
  });

  it("numbers new tabs monotonically and never mints an ordinal twice", () => {
    const three = add(add(initialTerminalTabsState));
    expect(ids(three)).toEqual([terminalTabId(1), terminalTabId(2), terminalTabId(3)]);
    // A reused id would address a shell the host is still tearing down, and a stale receipt
    // would reach the wrong one.
    const reopened = add(close(close(three, terminalTabId(2)), terminalTabId(3)));
    expect(ids(reopened)).toEqual([terminalTabId(1), terminalTabId(4)]);
  });

  it("selects the tab it just created", () => {
    const two = add(initialTerminalTabsState);
    expect(layout(two).activeId).toBe(terminalTabId(2));
  });

  it("keeps conversations apart", () => {
    const state = add(initialTerminalTabsState);
    expect(ids(state, "conversation-2")).toEqual([terminalTabId(1)]);
    expect(layout(state, "conversation-2").nextOrdinal).toBe(2);
  });

  describe("closing", () => {
    it("moves the selection right, and off the end to the left", () => {
      const three = add(add(initialTerminalTabsState));
      const middle = close(
        apply(three, { type: "activate", conversationId, terminalId: terminalTabId(2) }),
        terminalTabId(2)
      );
      expect(layout(middle).activeId).toBe(terminalTabId(3));
      expect(layout(close(middle, terminalTabId(3))).activeId).toBe(terminalTabId(1));
    });

    it("leaves the selection alone when another tab closes", () => {
      const three = add(add(initialTerminalTabsState));
      expect(layout(close(three, terminalTabId(1))).activeId).toBe(terminalTabId(3));
    });

    it("empties the conversation once the last tab goes", () => {
      const empty = close(initialTerminalTabsState, terminalTabId(1));
      expect(ids(empty)).toEqual([]);
      expect(layout(empty).activeId).toBeNull();
      expect(activeTerminalTab(layout(empty))).toBeNull();
    });

    it("ignores a tab that is not there", () => {
      const state = add(initialTerminalTabsState);
      expect(close(state, "terminal-9")).toBe(state);
    });
  });

  describe("ensure", () => {
    it("is a no-op while the conversation still has a tab", () => {
      const state = add(initialTerminalTabsState);
      expect(apply(state, { type: "ensure", conversationId })).toBe(state);
      expect(apply(initialTerminalTabsState, { type: "ensure", conversationId }))
        .toBe(initialTerminalTabsState);
    });

    it("gives an emptied conversation a fresh terminal, keeping the spent ordinals", () => {
      const emptied = close(add(initialTerminalTabsState), terminalTabId(2));
      const reopened = apply(
        close(emptied, terminalTabId(1)),
        { type: "ensure", conversationId }
      );
      expect(ids(reopened)).toEqual([terminalTabId(3)]);
      expect(layout(reopened).activeId).toBe(terminalTabId(3));
    });
  });

  describe("naming", () => {
    it("numbers a tab by its place in the strip, so a closed one gives its number back", () => {
      const three = add(add(initialTerminalTabsState));
      expect(three.byConversation[conversationId].tabs.map(
        (tab) => terminalDisplayNumber(layout(three), tab.id)
      )).toEqual([1, 2, 3]);
      const two = close(three, terminalTabId(2));
      expect(two.byConversation[conversationId].tabs.map(
        (tab) => terminalDisplayNumber(layout(two), tab.id)
      )).toEqual([1, 2]);
    });

    it("keeps a trimmed name and drops an empty one back to the derived name", () => {
      const named = apply(initialTerminalTabsState, {
        type: "rename", conversationId, terminalId: terminalTabId(1), name: "  build  "
      });
      expect(layout(named).tabs[0].name).toBe("build");
      const cleared = apply(named, {
        type: "rename", conversationId, terminalId: terminalTabId(1), name: "   "
      });
      expect(layout(cleared).tabs[0].name).toBeNull();
    });

    it("ignores a rename that changes nothing or names no tab", () => {
      const named = apply(initialTerminalTabsState, {
        type: "rename", conversationId, terminalId: terminalTabId(1), name: "build"
      });
      expect(apply(named, {
        type: "rename", conversationId, terminalId: terminalTabId(1), name: "build"
      })).toBe(named);
      expect(apply(named, {
        type: "rename", conversationId, terminalId: "terminal-9", name: "build"
      })).toBe(named);
    });
  });

  describe("activate", () => {
    it("ignores an unknown tab and a selection that is already current", () => {
      const state = add(initialTerminalTabsState);
      expect(apply(state, { type: "activate", conversationId, terminalId: "terminal-9" }))
        .toBe(state);
      expect(apply(state, { type: "activate", conversationId, terminalId: terminalTabId(2) }))
        .toBe(state);
    });
  });

  it("forgets a conversation, and says nothing changed when it never knew it", () => {
    const state = add(initialTerminalTabsState);
    expect(apply(state, { type: "remove_conversation", conversationId }))
      .toEqual(initialTerminalTabsState);
    expect(apply(state, { type: "remove_conversation", conversationId: "conversation-2" }))
      .toBe(state);
  });
});
