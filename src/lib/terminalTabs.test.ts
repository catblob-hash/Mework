import { describe, expect, it } from "vitest";
import {
  initialTerminalTabsState,
  terminalShellKey,
  terminalTabId,
  terminalTabsFor,
  terminalTabsReducer
} from "./terminalTabs";
import type { TerminalTabsAction, TerminalTabsState } from "./terminalTabs";
import type { TerminalLaunchChoice } from "./terminal";

const conversationId = "conversation-1";
const zsh: TerminalLaunchChoice = { workspace: 1, shell: "zsh" };
const bash: TerminalLaunchChoice = { workspace: 1, shell: "bash" };

function apply(state: TerminalTabsState, ...actions: TerminalTabsAction[]): TerminalTabsState {
  return actions.reduce(terminalTabsReducer, state);
}
function layout(state: TerminalTabsState, id = conversationId) {
  return terminalTabsFor(state, id);
}
function ids(state: TerminalTabsState, id = conversationId) {
  return layout(state, id).tabs.map((tab) => tab.id);
}
function add(state: TerminalTabsState, launch: TerminalLaunchChoice | null = zsh, id = conversationId) {
  return apply(state, { type: "add", conversationId: id, launch });
}
function close(state: TerminalTabsState, terminalId: string, id = conversationId) {
  return apply(state, { type: "close", conversationId: id, terminalId });
}
/** Each tab's shell and number, which is what its derived name is made of. */
function numbered(state: TerminalTabsState, id = conversationId) {
  return layout(state, id).tabs.map((tab) => `${tab.launch?.shell ?? "?"} ${tab.number}`);
}

describe("terminal tabs", () => {
  it("shares one frozen, empty opening layout across conversations and null", () => {
    const first = layout(initialTerminalTabsState);
    expect(first).toBe(terminalTabsFor(initialTerminalTabsState, "other"));
    expect(first).toBe(terminalTabsFor(initialTerminalTabsState, null));
    expect(Object.isFrozen(first)).toBe(true);
    expect(Object.isFrozen(first.tabs)).toBe(true);
    expect(Object.isFrozen(first.nextNumbers)).toBe(true);
    // No conversation has a terminal until one is asked for.
    expect(ids(initialTerminalTabsState)).toEqual([]);
  });

  it("does not resolve prototype properties as layouts", () => {
    expect(terminalTabsFor(initialTerminalTabsState, "constructor"))
      .toBe(layout(initialTerminalTabsState));
  });

  it("numbers new tabs monotonically and never mints an ordinal twice", () => {
    const three = add(add(add(initialTerminalTabsState)));
    expect(ids(three)).toEqual([terminalTabId(1), terminalTabId(2), terminalTabId(3)]);
    // A reused id would address a shell the host is still tearing down, and a stale receipt
    // would reach the wrong one.
    const reopened = add(close(close(three, terminalTabId(2)), terminalTabId(3)));
    expect(ids(reopened)).toEqual([terminalTabId(1), terminalTabId(4)]);
  });

  it("selects the tab it just created", () => {
    const two = add(add(initialTerminalTabsState));
    expect(layout(two).activeId).toBe(terminalTabId(2));
  });

  it("keeps conversations apart", () => {
    const state = add(initialTerminalTabsState);
    expect(ids(state, "conversation-2")).toEqual([]);
    expect(layout(state, "conversation-2").nextOrdinal).toBe(1);
  });

  describe("closing", () => {
    it("moves the selection right, and off the end to the left", () => {
      const three = add(add(add(initialTerminalTabsState)));
      const middle = close(
        apply(three, { type: "activate", conversationId, terminalId: terminalTabId(2) }),
        terminalTabId(2)
      );
      expect(layout(middle).activeId).toBe(terminalTabId(3));
      expect(layout(close(middle, terminalTabId(3))).activeId).toBe(terminalTabId(1));
    });

    it("leaves the selection alone when another tab closes", () => {
      const three = add(add(add(initialTerminalTabsState)));
      expect(layout(close(three, terminalTabId(1))).activeId).toBe(terminalTabId(3));
    });

    it("empties the conversation once the last tab goes", () => {
      const empty = close(add(initialTerminalTabsState), terminalTabId(1));
      expect(ids(empty)).toEqual([]);
      expect(layout(empty).activeId).toBeNull();
    });

    it("ignores a tab that is not there", () => {
      const state = add(initialTerminalTabsState);
      expect(close(state, "terminal-9")).toBe(state);
    });
  });

  describe("ensure", () => {
    it("is a no-op while the conversation still has a tab", () => {
      const state = add(initialTerminalTabsState);
      expect(apply(state, { type: "ensure", conversationId, launch: bash })).toBe(state);
    });

    it("gives a conversation with no terminal one started as asked", () => {
      const state = apply(initialTerminalTabsState, { type: "ensure", conversationId, launch: bash });
      expect(ids(state)).toEqual([terminalTabId(1)]);
      expect(layout(state).tabs[0].launch).toEqual(bash);
      expect(layout(state).activeId).toBe(terminalTabId(1));
    });

    it("gives an emptied conversation a fresh terminal, keeping the spent ordinals", () => {
      const emptied = close(close(add(add(initialTerminalTabsState)), terminalTabId(2)), terminalTabId(1));
      const reopened = apply(emptied, { type: "ensure", conversationId, launch: zsh });
      expect(ids(reopened)).toEqual([terminalTabId(3)]);
      expect(layout(reopened).activeId).toBe(terminalTabId(3));
    });
  });

  describe("naming", () => {
    it("counts each shell on its own", () => {
      const state = add(add(add(initialTerminalTabsState, zsh), bash), zsh);
      expect(numbered(state)).toEqual(["zsh 1", "bash 1", "zsh 2"]);
    });

    it("never gives a closed terminal's number back", () => {
      const two = add(add(initialTerminalTabsState, zsh), zsh);
      const reopened = add(close(two, terminalTabId(1)), zsh);
      expect(numbered(reopened)).toEqual(["zsh 2", "zsh 3"]);
      // Not even once every terminal of that shell is gone.
      const emptied = close(close(reopened, terminalTabId(2)), terminalTabId(3));
      expect(numbered(apply(emptied, { type: "ensure", conversationId, launch: zsh })))
        .toEqual(["zsh 4"]);
    });

    it("counts the terminals that left the shell to the host together", () => {
      const state = add(add(add(initialTerminalTabsState, null), zsh), { workspace: 2, shell: null });
      expect(numbered(state)).toEqual(["? 1", "zsh 1", "? 2"]);
      expect(terminalShellKey(null)).toBe(terminalShellKey({ workspace: 2, shell: null }));
    });

    it("counts a shell across workspaces", () => {
      const state = add(add(initialTerminalTabsState, zsh), { workspace: 2, shell: "zsh" });
      expect(numbered(state)).toEqual(["zsh 1", "zsh 2"]);
    });

    it("keeps a trimmed name and drops an empty one back to the derived name", () => {
      const state = add(initialTerminalTabsState);
      const named = apply(state, {
        type: "rename", conversationId, terminalId: terminalTabId(1), name: "  build  "
      });
      expect(layout(named).tabs[0].name).toBe("build");
      const cleared = apply(named, {
        type: "rename", conversationId, terminalId: terminalTabId(1), name: "   "
      });
      expect(layout(cleared).tabs[0].name).toBeNull();
    });

    it("ignores a rename that changes nothing or names no tab", () => {
      const named = apply(add(initialTerminalTabsState), {
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
      const state = add(add(initialTerminalTabsState));
      expect(apply(state, { type: "activate", conversationId, terminalId: "terminal-9" }))
        .toBe(state);
      expect(apply(state, { type: "activate", conversationId, terminalId: terminalTabId(2) }))
        .toBe(state);
    });
  });

  it("records the workspace and shell a new tab was asked for", () => {
    const launch = { workspace: 2, shell: "sh" as const };
    const state = add(add(initialTerminalTabsState), launch);
    expect(layout(state).tabs.map((tab) => tab.launch)).toEqual([zsh, launch]);
    expect(layout(state).activeId).toBe(terminalTabId(2));
  });

  it("forgets a conversation, and says nothing changed when it never knew it", () => {
    const state = add(initialTerminalTabsState);
    expect(apply(state, { type: "remove_conversation", conversationId }))
      .toEqual(initialTerminalTabsState);
    expect(apply(state, { type: "remove_conversation", conversationId: "conversation-2" }))
      .toBe(state);
  });
});
