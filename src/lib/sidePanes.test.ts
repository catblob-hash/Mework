import { afterEach, describe, expect, it, vi } from "vitest";
import {
  CHAT_TILE_FLEX, MIN_SIDE_FLEX, MAX_SIDE_FLEX, initialSidePanesState,
  sidePanesReducer, sidePaneLayoutFor, previewSessionsFor, paneKind, paneTarget,
  previewPaneId, subagentPaneId, shellPaneId, paneIsOpen, openPreviewSession,
  focusedPane, defaultSideFlexForKind, sidePaneDomId, previewTabSessionId,
  expandedPane,
  previewSessionBelongsToConversation, isPrimaryPreviewSession,
  loadSidePanesState, persistSidePanesState
} from "./sidePanes";
import type { SidePaneId, SidePaneKind, SidePanesAction, SidePanesState } from "./sidePanes";

const conversationId = "conversation-1";
function open(state: SidePanesState, pane: SidePaneId, id = conversationId) {
  return sidePanesReducer(state, { type: "open", conversationId: id, pane });
}
function layout(state: SidePanesState) { return sidePaneLayoutFor(state, conversationId); }
function apply(state: SidePanesState, action: SidePanesAction) { return sidePanesReducer(state, action); }
function stack(...panes: SidePaneId[]) { return panes.reduce((state, pane) => open(state, pane), initialSidePanesState); }

afterEach(() => vi.unstubAllGlobals());

describe("side pane helpers", () => {
  it("shares a deeply frozen absent layout across conversations and null", () => {
    const empty = layout(initialSidePanesState);
    expect(empty).toBe(sidePaneLayoutFor(initialSidePanesState, "other"));
    expect(empty).toBe(sidePaneLayoutFor(initialSidePanesState, null));
    expect(Object.isFrozen(empty)).toBe(true);
    expect(Object.isFrozen(empty.panes)).toBe(true);
    expect(Object.isFrozen(empty.paneFlex)).toBe(true);
    expect(empty.panes).toEqual([]);
  });

  it("shares a frozen empty roster but returns an existing roster verbatim", () => {
    const empty = previewSessionsFor(initialSidePanesState, conversationId);
    expect(empty).toBe(previewSessionsFor(initialSidePanesState, null));
    expect(empty).toBe(previewSessionsFor(initialSidePanesState, "other"));
    expect(Object.isFrozen(empty)).toBe(true);
    const state = stack("preview:a");
    expect(previewSessionsFor(state, conversationId)).toBe(state.previewSessions[conversationId]);
    expect(layout(state)).toBe(state.layoutByConversation[conversationId]);
  });

  it("does not resolve prototype properties as layouts or rosters", () => {
    expect(sidePaneLayoutFor(initialSidePanesState, "constructor")).toBe(layout(initialSidePanesState));
    expect(previewSessionsFor(initialSidePanesState, "__proto__")).toEqual([]);
  });

  it.each<[SidePaneKind, number]>([
    ["terminal", 1], ["review", 3], ["preview", 3], ["files", 3], ["settings", 3],
    ["tasks", 1], ["plan", 1], ["history", 1], ["subagent", 1], ["shell", 1]
  ])("defaults %s to flex %s", (kind, flex) => {
    expect(defaultSideFlexForKind(kind)).toBe(flex);
    expect(CHAT_TILE_FLEX).toBe(2);
    expect(MIN_SIDE_FLEX).toBe(0.25);
    expect(MAX_SIDE_FLEX).toBe(8);
  });

  it("constructs and splits resource ids without dropping colons in the target", () => {
    expect(previewPaneId("a:b")).toBe("preview:a:b");
    expect(subagentPaneId("a")).toBe("subagent:a");
    expect(shellPaneId("s")).toBe("shell:s");
    expect(paneKind("preview:a:b")).toBe("preview");
    expect(paneTarget("preview:a:b")).toBe("a:b");
    expect(paneTarget("preview:")).toBe("");
    expect(paneKind("terminal")).toBe("terminal");
    expect(paneTarget("terminal")).toBeNull();
  });

  it("reports open panes and the current native preview session", () => {
    const value = layout(stack("terminal", "preview:a:b", "tasks"));
    expect(paneIsOpen(value, "terminal")).toBe(true);
    expect(paneIsOpen(value, "review")).toBe(false);
    expect(openPreviewSession(value)).toBe("a:b");
    expect(openPreviewSession(layout(stack("terminal")))).toBeNull();
  });

  it("falls back from missing or stale focus to the last pane, then null", () => {
    const value = layout(stack("terminal", "tasks"));
    expect(focusedPane({ ...value, focused: "terminal" })).toBe("terminal");
    expect(focusedPane({ ...value, focused: "review" })).toBe("tasks");
    expect(focusedPane({ ...value, focused: null })).toBe("tasks");
    expect(focusedPane({ ...value, panes: [] })).toBeNull();
  });

  it("preserves preview tab ownership and primary-session semantics", () => {
    expect(previewTabSessionId("c", "a")).toBe("c#a");
    expect(previewSessionBelongsToConversation("c", "c")).toBe(true);
    expect(previewSessionBelongsToConversation("c#a", "c")).toBe(true);
    expect(previewSessionBelongsToConversation("cc#a", "c")).toBe(false);
    expect(previewSessionBelongsToConversation("d#a", "c")).toBe(false);
    expect(isPrimaryPreviewSession("c", "c")).toBe(true);
    expect(isPrimaryPreviewSession("c#a", "c")).toBe(false);
  });

  it("encodes DOM ids by base-36 Unicode code points with an unambiguous prefix", () => {
    expect(sidePaneDomId("preview:A😀")).toBe("side-pane-34-36-2t-3a-2x-2t-3b-1m-1t-2r5s");
    expect(sidePaneDomId("preview:a-b")).not.toBe(sidePaneDomId("preview:a:b"));
    expect(sidePaneDomId("terminal")).toBe(sidePaneDomId("terminal"));
    expect(sidePaneDomId("shell: /#中")).toMatch(/^side-pane-[0-9a-z-]+$/);
  });
});

describe("sidePanesReducer", () => {
  it("appends panes, focuses the newest and keeps the first pane's column ratio", () => {
    const state = stack("terminal", "review", "tasks");
    expect(layout(state)).toEqual({ panes: ["terminal", "review", "tasks"], focused: "tasks", sideFlex: 1, paneFlex: {}, expanded: null });
    expect(layout(stack("review", "terminal")).sideFlex).toBe(3);
    expect(initialSidePanesState.layoutByConversation).toEqual({});
  });

  it("keeps conversations independent with structural sharing", () => {
    const first = stack("review");
    const next = open(first, "terminal", "other");
    expect(layout(next)).toBe(layout(first));
    expect(sidePaneLayoutFor(next, "other").panes).toEqual(["terminal"]);
    expect(next.previewSessions).toBe(first.previewSessions);
  });

  it("opening an open pane only refocuses it and preserves stack and flex identity", () => {
    const state = stack("terminal", "tasks");
    const next = open(state, "terminal");
    expect(next).not.toBe(state);
    expect(layout(next).focused).toBe("terminal");
    expect(layout(next).panes).toBe(layout(state).panes);
    expect(layout(next).paneFlex).toBe(layout(state).paneFlex);
    expect(open(next, "terminal")).toBe(next);
  });

  it("toggles a missing pane on and an open pane off", () => {
    const action = { type: "toggle", conversationId, pane: "preview:a" } as const;
    const state = apply(initialSidePanesState, action);
    expect(layout(state).panes).toEqual(["preview:a"]);
    const closed = apply(state, action);
    expect(layout(closed).panes).toEqual([]);
    expect(previewSessionsFor(closed, conversationId)).toEqual(["a"]);
  });

  it("closes a pane, removes its vertical slot and retains column ratio", () => {
    const state = apply(stack("review"), { type: "set_pane_flex", conversationId, paneFlex: { review: 2 } });
    const next = apply(state, { type: "close", conversationId, pane: "review" });
    expect(layout(next)).toEqual({ panes: [], focused: null, sideFlex: 3, paneFlex: {}, expanded: null });
    expect(layout(state).paneFlex).toEqual({ review: 2 });
  });

  it("closing an unfocused pane keeps focus and closing focus falls back to last", () => {
    const state = stack("terminal", "review", "tasks");
    const next = apply(state, { type: "close", conversationId, pane: "review" });
    expect(layout(next).focused).toBe("tasks");
    expect(layout(apply(next, { type: "close", conversationId, pane: "tasks" })).focused).toBe("terminal");
  });

  it("close_last removes the focused pane rather than necessarily the last", () => {
    const state = apply(stack("terminal", "review", "tasks"), { type: "focus", conversationId, pane: "terminal" });
    const next = apply(state, { type: "close_last", conversationId });
    expect(layout(next).panes).toEqual(["review", "tasks"]);
    expect(layout(next).focused).toBe("tasks");
  });

  it("close_last falls back to the final pane for stale focus", () => {
    const state = stack("terminal", "tasks");
    const stale = { ...state, layoutByConversation: { [conversationId]: { ...layout(state), focused: "review" as SidePaneId } } };
    expect(layout(apply(stale, { type: "close_last", conversationId })).panes).toEqual(["terminal"]);
  });

  it("focus changes only for an open pane with a different focus", () => {
    const state = stack("terminal", "review");
    expect(apply(state, { type: "focus", conversationId, pane: "tasks" })).toBe(state);
    expect(apply(state, { type: "focus", conversationId, pane: "review" })).toBe(state);
    const next = apply(state, { type: "focus", conversationId, pane: "terminal" });
    expect(layout(next).focused).toBe("terminal");
    expect(layout(next).panes).toBe(layout(state).panes);
  });

  it("preserves identity for absent close, empty close_last and absent removal", () => {
    for (const action of [
      { type: "close", conversationId, pane: "terminal" },
      { type: "close_last", conversationId },
      { type: "remove_conversation", conversationId },
      { type: "focus", conversationId, pane: "terminal" },
      { type: "forget_preview", conversationId, sessionId: "unknown" }
    ] satisfies SidePanesAction[]) expect(apply(initialSidePanesState, action)).toBe(initialSidePanesState);
    const closed = apply(stack("terminal"), { type: "close_last", conversationId });
    expect(apply(closed, { type: "close_last", conversationId })).toBe(closed);
    expect(apply(stack("tasks"), { type: "close", conversationId, pane: "terminal" }).layoutByConversation[conversationId].panes).toEqual(["tasks"]);
  });

  it("replaces previews in place, renames their flex slot, and keeps both sessions", () => {
    const state = apply(stack("terminal", "preview:a", "tasks"), {
      type: "set_pane_flex", conversationId, paneFlex: { terminal: 2, "preview:a": 4, tasks: 3 }
    });
    const next = open(state, "preview:b");
    expect(layout(next)).toEqual({
      panes: ["terminal", "preview:b", "tasks"], sideFlex: 1, expanded: null,
      focused: "preview:b", paneFlex: { terminal: 2, "preview:b": 4, tasks: 3 }
    });
    expect(previewSessionsFor(next, conversationId)).toEqual(["a", "b"]);
    expect(layout(state).paneFlex["preview:a"]).toBe(4);
    expect(open(next, "preview:b")).toBe(next);
  });

  it("replaces a preview without inventing an explicit default flex slot", () => {
    const next = open(stack("preview:a", "review"), "preview:b");
    expect(layout(next).panes).toEqual(["preview:b", "review"]);
    expect(layout(next).paneFlex).toEqual({});
    expect(layout(next).sideFlex).toBe(3);
  });

  it("uses remembered kind widths on first open and after closing the last pane", () => {
    const state = { ...initialSidePanesState, lastSideFlexByKind: { review: 5, terminal: 2 } };
    const shown = open(state, "review");
    expect(layout(shown).sideFlex).toBe(5);
    expect(layout(open(shown, "terminal")).sideFlex).toBe(5);
    const closed = apply(shown, { type: "close_last", conversationId });
    expect(layout(open(closed, "terminal")).sideFlex).toBe(2);
    expect(layout(open(closed, "tasks")).sideFlex).toBe(1);
  });

  it("clamps column flex and remembers it under the first pane rather than focus", () => {
    const state = stack("preview:a", "tasks");
    const max = apply(state, { type: "set_side_flex", conversationId, sideFlex: 20 });
    expect(layout(max).sideFlex).toBe(8);
    expect(max.lastSideFlexByKind).toEqual({ preview: 8 });
    const min = apply(max, { type: "set_side_flex", conversationId, sideFlex: -1 });
    expect(layout(min).sideFlex).toBe(0.25);
    expect(min.lastSideFlexByKind).toEqual({ preview: 0.25 });
    expect(apply(min, { type: "set_side_flex", conversationId, sideFlex: -9 })).toBe(min);
  });

  it("records a resize even when equal to the default, then stops identity churn", () => {
    const state = stack("terminal");
    const action = { type: "set_side_flex", conversationId, sideFlex: 1 } as const;
    const next = apply(state, action);
    expect(next.lastSideFlexByKind).toEqual({ terminal: 1 });
    expect(layout(next)).toBe(layout(state));
    expect(apply(next, action)).toBe(next);
  });

  it("sets column flex without remembering a kind when no panes are open", () => {
    const next = apply(initialSidePanesState, { type: "set_side_flex", conversationId, sideFlex: 4 });
    expect(layout(next).sideFlex).toBe(4);
    expect(next.lastSideFlexByKind).toBe(initialSidePanesState.lastSideFlexByKind);
    expect(layout(open(next, "terminal")).sideFlex).toBe(1);
  });

  it("replaces rather than merges pane flex and compares clamped map values", () => {
    const state = apply(stack("terminal", "tasks"), { type: "set_pane_flex", conversationId, paneFlex: { terminal: 2, tasks: 3 } });
    const input = { tasks: 100, review: -5 };
    const next = apply(state, { type: "set_pane_flex", conversationId, paneFlex: input });
    expect(layout(next).paneFlex).toEqual({ tasks: 8, review: 0.25 });
    expect(input).toEqual({ tasks: 100, review: -5 });
    expect(apply(next, { type: "set_pane_flex", conversationId, paneFlex: { review: -1, tasks: 9 } })).toBe(next);
    expect(layout(apply(next, { type: "set_pane_flex", conversationId, paneFlex: {} })).paneFlex).toEqual({});
  });

  it("keeps invalid numeric flex from poisoning layout math", () => {
    const next = apply(stack("terminal"), { type: "set_pane_flex", conversationId, paneFlex: { terminal: NaN, tasks: Infinity, review: -Infinity } });
    expect(layout(next).paneFlex).toEqual({ terminal: 0.25, tasks: 8, review: 0.25 });
    expect(layout(apply(next, { type: "set_side_flex", conversationId, sideFlex: NaN })).sideFlex).toBe(0.25);
  });

  it("registers previews in arrival order without changing any layout", () => {
    const first = apply(stack("terminal"), { type: "register_preview", conversationId, sessionId: "b" });
    const second = apply(first, { type: "register_preview", conversationId, sessionId: "a" });
    expect(second.layoutByConversation).toBe(first.layoutByConversation);
    expect(previewSessionsFor(second, conversationId)).toEqual(["b", "a"]);
    expect(apply(second, { type: "register_preview", conversationId, sessionId: "a" })).toBe(second);
    expect(previewSessionsFor(open(second, "preview:a"), conversationId)).toBe(previewSessionsFor(second, conversationId));
  });

  it("forgets an open preview, deletes an empty roster and focuses a surviving pane", () => {
    const next = apply(stack("tasks", "preview:a"), { type: "forget_preview", conversationId, sessionId: "a" });
    expect(layout(next).panes).toEqual(["tasks"]);
    expect(layout(next).focused).toBe("tasks");
    expect(next.previewSessions).not.toHaveProperty(conversationId);
  });

  it("forgetting a hidden preview leaves the current layout identity alone", () => {
    const state = stack("preview:a", "preview:b");
    const next = apply(state, { type: "forget_preview", conversationId, sessionId: "a" });
    expect(layout(next)).toBe(layout(state));
    expect(previewSessionsFor(next, conversationId)).toEqual(["b"]);
    expect(apply(next, { type: "forget_preview", conversationId, sessionId: "unknown" })).toBe(next);
  });

  it("forgetting an unregistered but referenced preview still closes its pane", () => {
    const state = { ...stack("preview:a"), previewSessions: {} };
    const next = apply(state, { type: "forget_preview", conversationId, sessionId: "a" });
    expect(layout(next).panes).toEqual([]);
    expect(next.previewSessions).toBe(state.previewSessions);
  });

  it("removes a conversation layout and roster but retains others and remembered widths", () => {
    const state = apply(open(stack("preview:a"), "review", "other"), { type: "set_side_flex", conversationId, sideFlex: 5 });
    const next = apply(state, { type: "remove_conversation", conversationId });
    expect(next.layoutByConversation).not.toHaveProperty(conversationId);
    expect(next.previewSessions).not.toHaveProperty(conversationId);
    expect(sidePaneLayoutFor(next, "other")).toBe(sidePaneLayoutFor(state, "other"));
    expect(next.lastSideFlexByKind).toBe(state.lastSideFlexByKind);
    expect(apply(next, { type: "remove_conversation", conversationId })).toBe(next);
  });

  it("removes a roster-only conversation", () => {
    const state = apply(initialSidePanesState, { type: "register_preview", conversationId, sessionId: "a" });
    expect(apply(state, { type: "remove_conversation", conversationId })).toEqual(initialSidePanesState);
  });

  it("carries a draft's layout onto the conversation it becomes, leaving nothing behind", () => {
    // A settings pane opened in a draft must follow the draft into the real
    // conversation, and must not reappear in the next brand-new one.
    const state = open(initialSidePanesState, "settings", "__draft__");
    const next = apply(state, { type: "adopt_conversation", conversationId, from: "__draft__" });
    expect(next.layoutByConversation).not.toHaveProperty("__draft__");
    expect(layout(next).panes).toEqual(["settings"]);
    expect(layout(next).focused).toBe("settings");
    expect(layout(next).sideFlex).toBe(3);
  });

  it("adopts nothing when the draft never opened a pane, and never adopts onto itself", () => {
    expect(apply(initialSidePanesState, {
      type: "adopt_conversation", conversationId, from: "__draft__"
    })).toBe(initialSidePanesState);
    const state = open(initialSidePanesState, "settings", "__draft__");
    expect(apply(state, {
      type: "adopt_conversation", conversationId: "__draft__", from: "__draft__"
    })).toBe(state);
  });

  it("drops a preview pane while adopting, so a draft-minted session id cannot follow", () => {
    // The draft can never reach a workspace, so such a pane addresses a page that
    // does not exist; carrying it would leave a tile pointing at a dead session.
    const state = open(open(initialSidePanesState, "settings", "__draft__"), "preview:__draft__#1", "__draft__");
    const next = apply(state, { type: "adopt_conversation", conversationId, from: "__draft__" });
    expect(layout(next).panes).toEqual(["settings"]);
    expect(layout(next).focused).toBe("settings");
    expect(layout(next).expanded).toBeNull();
  });
});

describe("expanded pane", () => {
  it("toggles a pane on and off and focuses it while expanding", () => {
    const state = stack("terminal", "review");
    const expanded = apply(state, { type: "toggle_expand", conversationId, pane: "terminal" });
    expect(expandedPane(layout(expanded))).toBe("terminal");
    expect(layout(expanded).focused).toBe("terminal");
    expect(expandedPane(layout(apply(expanded, { type: "toggle_expand", conversationId, pane: "terminal" })))).toBeNull();
  });

  it("moves the expansion straight to another open pane", () => {
    const state = apply(stack("terminal", "review"), { type: "toggle_expand", conversationId, pane: "terminal" });
    expect(expandedPane(layout(apply(state, { type: "toggle_expand", conversationId, pane: "review" })))).toBe("review");
  });

  it("ignores a pane that is not open", () => {
    const state = stack("terminal");
    expect(apply(state, { type: "toggle_expand", conversationId, pane: "tasks" })).toBe(state);
  });

  it("reports no expansion once the expanded pane is closed", () => {
    const state = apply(stack("terminal", "review"), { type: "toggle_expand", conversationId, pane: "review" });
    const closed = apply(state, { type: "close", conversationId, pane: "review" });
    expect(closed.layoutByConversation[conversationId].expanded).toBe("review");
    expect(expandedPane(layout(closed))).toBeNull();
  });

  it("drops an expansion that would hide the pane the user just asked for", () => {
    const state = apply(stack("terminal", "review"), { type: "toggle_expand", conversationId, pane: "terminal" });
    expect(expandedPane(layout(open(state, "tasks")))).toBeNull();
    expect(expandedPane(layout(open(state, "review")))).toBeNull();
    expect(expandedPane(layout(open(state, "terminal")))).toBe("terminal");
  });

  it("survives a resize and does not reach other conversations", () => {
    const state = apply(stack("review"), { type: "toggle_expand", conversationId, pane: "review" });
    const resized = apply(state, { type: "set_side_flex", conversationId, sideFlex: 5 });
    expect(expandedPane(layout(resized))).toBe("review");
    expect(sidePaneLayoutFor(resized, "other").expanded).toBeNull();
  });
});


describe("side pane persistence", () => {
  const key = "mework.sidePanes.sideFlexByKind";
  function storage(raw?: string) {
    const values = new Map<string, string>(raw === undefined ? [] : [[key, raw]]);
    const fake = {
      getItem: vi.fn((name: string) => values.get(name) ?? null),
      setItem: vi.fn((name: string, value: string) => { values.set(name, value); })
    };
    vi.stubGlobal("localStorage", fake);
    return fake;
  }

  it("round trips only remembered widths through the exact storage key", () => {
    const fake = storage();
    const state = apply(stack("preview:a", "tasks"), { type: "set_side_flex", conversationId, sideFlex: 4.5 });
    persistSidePanesState(state);
    expect(fake.setItem).toHaveBeenCalledWith(key, '{"preview":4.5}');
    expect(loadSidePanesState()).toEqual({ ...initialSidePanesState, lastSideFlexByKind: { preview: 4.5 } });
    expect(fake.getItem).toHaveBeenCalledWith(key);
  });

  it("clamps stored values and ignores unknown kinds and nonnumbers", () => {
    storage('{"review":100,"terminal":-2,"files":2.5,"preview":"3","tasks":null,"unknown":4}');
    expect(loadSidePanesState().lastSideFlexByKind).toEqual({ review: 8, terminal: 0.25, files: 2.5 });
  });

  it("sanitizes remembered widths on write as well", () => {
    const fake = storage();
    persistSidePanesState({ ...initialSidePanesState, lastSideFlexByKind: { terminal: -3, files: 100, preview: NaN } });
    expect(JSON.parse(fake.setItem.mock.calls[0][1])).toEqual({ terminal: 0.25, files: 8 });
  });

  it.each([undefined, "{broken", "null", "[]", "3", '"text"', "true"])("ignores missing or malformed storage %s", (raw) => {
    storage(raw);
    expect(loadSidePanesState()).toEqual(initialSidePanesState);
  });

  it("survives storage read and write failures", () => {
    vi.stubGlobal("localStorage", {
      getItem() { throw new Error("denied"); },
      setItem() { throw new Error("quota"); }
    });
    expect(loadSidePanesState()).toEqual(initialSidePanesState);
    expect(() => persistSidePanesState(initialSidePanesState)).not.toThrow();
  });

  it("works when localStorage does not exist", () => {
    vi.stubGlobal("localStorage", undefined);
    expect(loadSidePanesState()).toEqual(initialSidePanesState);
    expect(() => persistSidePanesState(initialSidePanesState)).not.toThrow();
  });
});
