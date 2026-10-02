import { afterEach, describe, expect, it, vi } from "vitest";
import {
  CHAT_TILE_FLEX, MIN_SIDE_FLEX, MAX_SIDE_FLEX, initialSidePanesState,
  sidePanesReducer, sidePaneLayoutFor, previewSessionsFor, paneKind, paneTarget,
  previewPaneId, paneIsOpen, openPreviewSession, shownSubagent, shownTasksTab,
  focusedPane, defaultSideFlexForKind, sidePaneDomId, previewTabSessionId,
  expandedPane,
  previewSessionBelongsToConversation, isPrimaryPreviewSession,
  loadSidePanesState, persistSidePanesState, newPreviewPageSessionId, previewWorkspaceOf,
  MAX_SIDE_PANES, MAX_PANES_PER_COLUMN, PANE_KEEP_ORDER, paneClosedForRoom, sideColumnLimit
} from "./sidePanes";
import type { SidePaneId, SidePaneKind, SidePaneLayout, SidePanesAction, SidePanesState } from "./sidePanes";

const conversationId = "conversation-1";
function open(state: SidePanesState, pane: SidePaneId, id = conversationId) {
  return sidePanesReducer(state, { type: "open", conversationId: id, pane });
}
function layout(state: SidePanesState) { return sidePaneLayoutFor(state, conversationId); }
function apply(state: SidePanesState, action: SidePanesAction) { return sidePanesReducer(state, action); }
function stack(...panes: SidePaneId[]) { return panes.reduce((state, pane) => open(state, pane), initialSidePanesState); }
function openAgent(state: SidePanesState, subagentId: string) {
  return sidePanesReducer(state, { type: "open_subagent", conversationId, subagentId });
}

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
    ["tasks", 1], ["plan", 1], ["history", 1], ["subagent", 1]
  ])("defaults %s to flex %s", (kind, flex) => {
    expect(defaultSideFlexForKind(kind)).toBe(flex);
    expect(CHAT_TILE_FLEX).toBe(2);
    expect(MIN_SIDE_FLEX).toBe(0.25);
    expect(MAX_SIDE_FLEX).toBe(8);
  });

  it("constructs and splits resource ids without dropping colons in the target", () => {
    expect(previewPaneId("a:b")).toBe("preview:a:b");
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
    expect(sidePaneDomId("history: /#中")).toMatch(/^side-pane-[0-9a-z-]+$/);
  });
});

describe("sidePanesReducer", () => {
  it("appends panes, focuses the newest and keeps the first pane's column ratio", () => {
    const state = stack("terminal", "review");
    expect(layout(state)).toEqual({
      panes: ["terminal", "review"], columns: [["terminal", "review"]], focused: "review",
      sideFlex: 1, columnFlex: [1], paneFlex: {}, subagentTabs: [], activeSubagent: null, tasksTab: "tasks",
      expanded: null
    });
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
    expect(layout(next)).toEqual({
      panes: [], columns: [], focused: null, sideFlex: 3, columnFlex: [], paneFlex: {},
      subagentTabs: [], activeSubagent: null, tasksTab: "tasks", expanded: null
    });
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
      panes: ["terminal", "preview:b", "tasks"], columns: [["terminal", "preview:b"], ["tasks"]],
      sideFlex: 2, columnFlex: [1, 1], subagentTabs: [], activeSubagent: null, tasksTab: "tasks", expanded: null,
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
    // A page named after the placeholder id belongs to no conversation, so carrying its pane
    // would leave a tile pointing at a session nothing owns.
    const state = open(open(initialSidePanesState, "settings", "__draft__"), "preview:__draft__#1", "__draft__");
    const next = apply(state, { type: "adopt_conversation", conversationId, from: "__draft__" });
    expect(layout(next).panes).toEqual(["settings"]);
    expect(layout(next).focused).toBe("settings");
    expect(layout(next).expanded).toBeNull();
    expect(next.previewSessions).toEqual({});
  });

  it("carries the draft's own preview page, which is already named after the conversation", () => {
    // The draft opens its page under the id it will materialize as, so the page and its roster
    // entry are the conversation's the moment it is real.
    const state = open(initialSidePanesState, previewPaneId(conversationId), "__draft__");
    expect(previewSessionsFor(state, "__draft__")).toEqual([conversationId]);
    const next = apply(state, { type: "adopt_conversation", conversationId, from: "__draft__" });
    expect(layout(next).panes).toEqual([previewPaneId(conversationId)]);
    expect(previewSessionsFor(next, conversationId)).toEqual([conversationId]);
    expect(next.previewSessions).not.toHaveProperty("__draft__");
  });
});

/** Every open pane in exactly one column, and no more columns, rows or panes than allowed. */
function expectWellFormed(pane: SidePaneLayout) {
  expect(pane.columns.flat().sort()).toEqual([...pane.panes].sort());
  expect(new Set(pane.panes).size).toBe(pane.panes.length);
  expect(pane.panes.length).toBeLessThanOrEqual(MAX_SIDE_PANES);
  expect(pane.columns.length).toBeLessThanOrEqual(pane.panes.length ? sideColumnLimit(pane.panes.length) : 0);
  for (const column of pane.columns) {
    expect(column.length).toBeGreaterThan(0);
    expect(column.length).toBeLessThanOrEqual(MAX_PANES_PER_COLUMN);
  }
  expect(pane.columnFlex).toHaveLength(pane.columns.length);
  for (const weight of pane.columnFlex) expect(weight).toBeGreaterThan(0);
  expect(pane.sideFlex).toBeGreaterThanOrEqual(MIN_SIDE_FLEX);
  expect(pane.sideFlex).toBeLessThanOrEqual(MAX_SIDE_FLEX);
  // The subagent pane is open exactly while it has a tab, and it shows one of them.
  expect(pane.subagentTabs.length > 0).toBe(paneIsOpen(pane, "subagent"));
  expect(new Set(pane.subagentTabs).size).toBe(pane.subagentTabs.length);
  if (pane.subagentTabs.length) expect(pane.subagentTabs).toContain(pane.activeSubagent);
  else expect(pane.activeSubagent).toBeNull();
}

/** Each column's width in the chat's units, the way the tiles are drawn. */
function columnWidths(pane: SidePaneLayout) {
  const total = pane.columnFlex.reduce((sum, weight) => sum + weight, 0);
  return pane.columnFlex.map((weight) => weight / total * pane.sideFlex);
}

const SEVEN: SidePaneId[] = ["settings", "plan", "preview:a", "terminal", "review", "files", "tasks"];

describe("side pane columns", () => {
  it("pairs panes the way the reference shell does, and opens a third column only past six", () => {
    const opened: SidePaneId[] = ["settings", "plan", "preview:a", "terminal", "review", "files", "tasks", "history:0", "history:1"];
    const expected: SidePaneId[][][] = [
      [["settings"]],
      [["settings", "plan"]],
      [["settings", "plan"], ["preview:a"]],
      [["settings", "plan"], ["preview:a", "terminal"]],
      [["settings", "plan"], ["preview:a", "terminal", "review"]],
      [["settings", "plan", "files"], ["preview:a", "terminal", "review"]],
      [["settings", "plan", "files"], ["preview:a", "terminal", "review"], ["tasks"]],
      [["settings", "plan", "files"], ["preview:a", "terminal", "review"], ["tasks", "history:0"]],
      [["settings", "plan", "files"], ["preview:a", "terminal", "review"], ["tasks", "history:0", "history:1"]]
    ];
    let state = initialSidePanesState;
    opened.forEach((pane, index) => {
      state = open(state, pane);
      expect(layout(state).columns).toEqual(expected[index]);
      expectWellFormed(layout(state));
    });
  });

  it("allows two columns up to six panes and three beyond", () => {
    expect([1, 2, 6, 7, 9].map(sideColumnLimit)).toEqual([2, 2, 2, 3, 3]);
  });

  it("widens the side area by half the chat for a new column and gives it back when it closes", () => {
    const paired = stack("review", "terminal");
    expect(layout(paired).sideFlex).toBe(3);
    const widened = open(paired, "tasks");
    expect(layout(widened).sideFlex).toBe(4);
    // The widened side area is split evenly, whatever width the first column had.
    expect(columnWidths(layout(widened))[0]).toBeCloseTo(2);
    expect(columnWidths(layout(widened))[1]).toBeCloseTo(2);
    const narrowed = apply(widened, { type: "close", conversationId, pane: "tasks" });
    expect(layout(narrowed).columns).toEqual([["review", "terminal"]]);
    expect(layout(narrowed).sideFlex).toBeCloseTo(3);
    expect(layout(narrowed).columnFlex).toEqual([1]);
    // However often a column comes and goes, the area does not creep narrower.
    let cycled = narrowed;
    for (let round = 0; round < 3; round += 1) cycled = apply(open(cycled, "tasks"), { type: "close", conversationId, pane: "tasks" });
    expect(layout(cycled).sideFlex).toBeCloseTo(3);
  });

  it("gives the chat back no more than a closing column's own width", () => {
    const squeezed = apply(open(stack("review", "terminal"), "tasks"), { type: "set_side_flex", conversationId, sideFlex: 1.2 });
    const closed = apply(squeezed, { type: "close", conversationId, pane: "tasks" });
    // The remaining column keeps the 0.6 it was drawn at.
    expect(layout(closed).sideFlex).toBeCloseTo(0.6);
  });

  it("splits the side area evenly whenever a column opens, whatever widths the columns had", () => {
    // A terminal opens narrower than a document pane, and the second column still gets half.
    const narrow = open(stack("terminal", "review"), "tasks");
    expect(layout(narrow).columns).toEqual([["terminal", "review"], ["tasks"]]);
    expect(layout(narrow).columnFlex).toEqual([1, 1]);
    // Widths dragged between two columns give way to thirds once a third column opens.
    const six = apply(stack("terminal", "review", "tasks", "files", "plan", "settings"), {
      type: "set_column_flex", conversationId, columnFlex: [3, 1]
    });
    expect(layout(six).columns).toHaveLength(2);
    const seven = open(six, "history:0");
    expect(layout(seven).columns).toHaveLength(3);
    expect(layout(seven).columnFlex).toEqual([1, 1, 1]);
    expectWellFormed(layout(seven));
  });

  it("keeps a new column's share when the side area is already at its widest", () => {
    const state = apply(stack("review", "terminal"), { type: "set_side_flex", conversationId, sideFlex: MAX_SIDE_FLEX });
    const next = open(state, "tasks");
    expect(layout(next).sideFlex).toBe(MAX_SIDE_FLEX);
    expectWellFormed(layout(next));
  });

  it("leaves the other panes where they are when one closes, and fills the gap it left", () => {
    const state = stack("terminal", "review", "tasks", "files");
    expect(layout(state).columns).toEqual([["terminal", "review"], ["tasks", "files"]]);
    const closed = apply(state, { type: "close", conversationId, pane: "terminal" });
    expect(layout(closed).columns).toEqual([["review"], ["tasks", "files"]]);
    // Two columns are all four panes get; the pane joins the shorter one.
    expect(layout(open(closed, "plan")).columns).toEqual([["review", "plan"], ["tasks", "files"]]);
  });

  it("drops an emptied column rather than keeping an empty one", () => {
    const state = stack("terminal", "review", "tasks");
    const closed = apply(state, { type: "close", conversationId, pane: "tasks" });
    expect(layout(closed).columns).toEqual([["terminal", "review"]]);
    expect(layout(closed).columnFlex).toEqual([1]);
    expect(layout(closed).sideFlex).toBeCloseTo(1);
  });

  it("folds the third column back into two once six panes remain, moving the fewest", () => {
    const state = apply(
      stack("settings", "plan", "preview:a", "terminal", "review", "files", "tasks", "history:0"),
      { type: "set_pane_flex", conversationId, paneFlex: { tasks: 2, "history:0": 3, files: 4 } }
    );
    expect(layout(state).columns).toEqual([
      ["settings", "plan", "files"], ["preview:a", "terminal", "review"], ["tasks", "history:0"]
    ]);
    const seven = apply(state, { type: "close", conversationId, pane: "plan" });
    expect(layout(seven).columns).toEqual([
      ["settings", "files"], ["preview:a", "terminal", "review"], ["tasks", "history:0"]
    ]);
    const six = apply(seven, { type: "close", conversationId, pane: "settings" });
    // The column holding the fewest panes is dissolved and its pane joins the shortest column
    // left; a moved pane's height starts over among its new neighbours.
    expect(layout(six).columns).toEqual([["preview:a", "terminal", "review"], ["tasks", "history:0", "files"]]);
    expect(layout(six).paneFlex).toEqual({ tasks: 2, "history:0": 3 });
    expect(layout(six).sideFlex).toBeLessThan(layout(seven).sideFlex);
    expectWellFormed(layout(six));
    const last = apply(stack("settings", "plan", "preview:a", "terminal", "review", "files", "tasks"), {
      type: "close", conversationId, pane: "settings"
    });
    expect(layout(last).columns).toEqual([["plan", "files", "tasks"], ["preview:a", "terminal", "review"]]);
  });

  it("closes the least kept pane, the oldest of its kind, to open a tenth", () => {
    const full = stack(...SEVEN, "history:1", "history:2");
    expect(layout(full).panes).toHaveLength(MAX_SIDE_PANES);
    // The new pane is never the one given up, even when it is the least kept kind.
    const next = open(full, "history:3");
    expect(layout(next).panes).toEqual([...SEVEN, "history:2", "history:3"]);
    expect(layout(next).focused).toBe("history:3");
    expectWellFormed(layout(next));
    const agent = openAgent(next, "a");
    expect(layout(agent).panes).toEqual([...SEVEN, "history:3", "subagent"]);
    expectWellFormed(layout(agent));
  });

  it("ranks an agent's history below everything the list names", () => {
    expect(PANE_KEEP_ORDER).toEqual([
      "settings", "plan", "preview", "terminal", "review", "files", "tasks", "subagent", "history"
    ]);
    const full = stack(...SEVEN, "history:0", "history:1");
    expect(paneClosedForRoom(layout(full))).toBe("history:0");
    expect(layout(openAgent(full, "a")).panes).toEqual([...SEVEN, "history:1", "subagent"]);
    expect(paneClosedForRoom(layout(stack("history:agent", "history:other")))).toBe("history:agent");
    expect(paneClosedForRoom(layout(initialSidePanesState))).toBeNull();
  });

  it("never closes one of the first eight kinds to make room", () => {
    const eight: SidePaneId[] = [...SEVEN, "subagent"];
    let state = openAgent(stack(...SEVEN), "a");
    const more: SidePaneId[] = ["history:1", "history:0", "history:2", "history:x", "history:3", "history:y"];
    for (const [index, pane] of more.entries()) {
      state = openAgent(open(state, pane), `agent-${index}`);
      for (const kept of eight) expect(paneIsOpen(layout(state), kept)).toBe(true);
      expect(paneIsOpen(layout(state), pane)).toBe(true);
      expectWellFormed(layout(state));
    }
    // Every agent opened along the way is a tab of the one pane, not a pane of its own.
    expect(layout(state).subagentTabs).toEqual(["a", ...more.map((_, index) => `agent-${index}`)]);
  });

  it("does not close anything for a pane that is already open or a page that replaces another", () => {
    const full = openAgent(stack(...SEVEN, "history:1"), "a");
    expect(layout(openAgent(full, "a")).panes).toBe(layout(full).panes);
    // Another agent is another tab of the pane already open.
    expect(layout(openAgent(full, "b")).panes).toBe(layout(full).panes);
    const replaced = open(full, "preview:b");
    expect(layout(replaced).panes).toHaveLength(MAX_SIDE_PANES);
    expect(layout(replaced).columns.flat()).toContain("preview:b");
  });

  it("stays well formed through any mix of opening, closing and replacing", () => {
    const pool: SidePaneId[] = [
      ...SEVEN, "preview:b", "subagent", "history:0", "history:1", "history:2"
    ];
    const agents = ["a", "b", "c"];
    let seed = 7;
    const next = () => {
      seed = (seed * 1103515245 + 12345) % 2147483648;
      return seed / 2147483648;
    };
    let state = initialSidePanesState;
    for (let step = 0; step < 2000; step++) {
      const pane = pool[Math.floor(next() * pool.length)];
      const agent = agents[Math.floor(next() * agents.length)];
      const roll = next();
      const before = layout(state);
      state = roll < 0.4 ? open(state, pane)
        : roll < 0.55 ? openAgent(state, agent)
          : roll < 0.75 ? apply(state, { type: "close", conversationId, pane })
            : roll < 0.85 ? apply(state, { type: "close_subagent", conversationId, subagentId: agent })
              : roll < 0.9 ? apply(state, {
                type: "retarget_subagent", conversationId, from: agent, to: agents[Math.floor(next() * agents.length)]
              })
                : apply(state, { type: "replace", conversationId, pane, replacement: pool[Math.floor(next() * pool.length)] });
      const after = layout(state);
      expectWellFormed(after);
      if (roll < 0.4 && before.panes.length === MAX_SIDE_PANES) {
        for (const kept of before.panes.filter((id) => SEVEN.includes(id) || id === "preview:b")) {
          if (paneKind(kept) === "preview" && paneKind(pane) === "preview") continue;
          expect(paneIsOpen(after, kept)).toBe(true);
        }
      }
    }
  });
});

describe("replacing a pane", () => {
  it("puts the replacement in the pane's column, height and focus", () => {
    const state = apply(
      apply(stack("terminal", "history:call", "tasks"), { type: "set_pane_flex", conversationId, paneFlex: { "history:call": 3 } }),
      { type: "focus", conversationId, pane: "history:call" }
    );
    const next = apply(state, { type: "replace", conversationId, pane: "history:call", replacement: "history:name" });
    expect(layout(next)).toEqual({
      ...layout(state),
      panes: ["terminal", "history:name", "tasks"],
      columns: [["terminal", "history:name"], ["tasks"]],
      paneFlex: { "history:name": 3 },
      focused: "history:name"
    });
  });

  it("carries an expansion over, and leaves an unrelated focus alone", () => {
    const state = apply(stack("history:call", "tasks"), { type: "toggle_expand", conversationId, pane: "history:call" });
    const focusedElsewhere = apply(state, { type: "focus", conversationId, pane: "tasks" });
    const next = apply(focusedElsewhere, { type: "replace", conversationId, pane: "history:call", replacement: "history:name" });
    expect(expandedPane(layout(next))).toBe("history:name");
    expect(layout(next).focused).toBe("tasks");
  });

  it("only closes the pane when its replacement is already open", () => {
    const state = stack("history:call", "history:name", "tasks");
    const next = apply(state, { type: "replace", conversationId, pane: "history:call", replacement: "history:name" });
    expect(layout(next).panes).toEqual(["history:name", "tasks"]);
    expectWellFormed(layout(next));
  });

  it("does nothing for a pane that is not open or a replacement that is itself", () => {
    const state = stack("tasks");
    expect(apply(state, { type: "replace", conversationId, pane: "history:a", replacement: "history:b" })).toBe(state);
    expect(apply(state, { type: "replace", conversationId, pane: "tasks", replacement: "tasks" })).toBe(state);
  });
});

describe("subagent tabs", () => {
  it("opens one pane for every agent, each a tab of it", () => {
    const first = openAgent(stack("tasks"), "a");
    expect(layout(first).panes).toEqual(["tasks", "subagent"]);
    expect(layout(first).focused).toBe("subagent");
    expect(shownSubagent(layout(first))).toBe("a");
    const second = openAgent(apply(first, { type: "focus", conversationId, pane: "tasks" }), "b");
    expect(layout(second).panes).toEqual(["tasks", "subagent"]);
    expect(layout(second).subagentTabs).toEqual(["a", "b"]);
    expect(shownSubagent(layout(second))).toBe("b");
    // Asking for an agent is asking to see it, so the pane takes the focus back.
    expect(layout(second).focused).toBe("subagent");
    expectWellFormed(layout(second));
  });

  it("selects an agent's tab where it is rather than adding another", () => {
    const state = openAgent(openAgent(openAgent(initialSidePanesState, "a"), "b"), "c");
    const back = openAgent(state, "a");
    expect(layout(back).subagentTabs).toEqual(["a", "b", "c"]);
    expect(shownSubagent(layout(back))).toBe("a");
    expect(openAgent(back, "a")).toBe(back);
  });

  it("shows the tab to the right of a closed one, to the left off the end, and closes the pane with the last", () => {
    let state = openAgent(openAgent(openAgent(stack("tasks"), "a"), "b"), "c");
    state = openAgent(state, "b");
    state = apply(state, { type: "close_subagent", conversationId, subagentId: "b" });
    expect(layout(state).subagentTabs).toEqual(["a", "c"]);
    expect(shownSubagent(layout(state))).toBe("c");
    state = apply(state, { type: "close_subagent", conversationId, subagentId: "c" });
    expect(shownSubagent(layout(state))).toBe("a");
    expectWellFormed(layout(state));
    state = apply(state, { type: "close_subagent", conversationId, subagentId: "a" });
    expect(layout(state).panes).toEqual(["tasks"]);
    expect(shownSubagent(layout(state))).toBeNull();
    expectWellFormed(layout(state));
    expect(apply(state, { type: "close_subagent", conversationId, subagentId: "a" })).toBe(state);
  });

  it("leaves the shown tab alone when another one closes", () => {
    const state = openAgent(openAgent(openAgent(initialSidePanesState, "a"), "b"), "c");
    const next = apply(state, { type: "close_subagent", conversationId, subagentId: "a" });
    expect(layout(next).subagentTabs).toEqual(["b", "c"]);
    expect(shownSubagent(layout(next))).toBe("c");
  });

  it("drops the tabs with the pane, so opening it again starts from the agent asked for", () => {
    const state = openAgent(openAgent(stack("tasks"), "a"), "b");
    const closed = apply(state, { type: "close", conversationId, pane: "subagent" });
    expect(layout(closed).subagentTabs).toEqual([]);
    expect(shownSubagent(layout(closed))).toBeNull();
    expectWellFormed(layout(closed));
    const reopened = openAgent(closed, "c");
    expect(layout(reopened).subagentTabs).toEqual(["c"]);
    // The pane given up for room takes its tabs with it too.
    const full = openAgent(stack(...SEVEN, "history:0"), "a");
    const roomMade = open(full, "history:1");
    expect(paneIsOpen(layout(roomMade), "subagent")).toBe(true);
    const crowded = open(open(stack(...SEVEN), "history:1"), "history:2");
    expectWellFormed(layout(openAgent(crowded, "a")));
  });

  it("points a tab at the id its agent settled into, in place, and merges it into a tab that id already has", () => {
    const state = openAgent(openAgent(openAgent(initialSidePanesState, "call-a"), "b"), "call-a");
    const moved = apply(state, { type: "retarget_subagent", conversationId, from: "call-a", to: "a" });
    expect(layout(moved).subagentTabs).toEqual(["a", "b"]);
    expect(shownSubagent(layout(moved))).toBe("a");
    expect(layout(moved).panes).toBe(layout(state).panes);
    const merged = apply(
      openAgent(moved, "call-b"),
      { type: "retarget_subagent", conversationId, from: "call-b", to: "b" }
    );
    expect(layout(merged).subagentTabs).toEqual(["a", "b"]);
    expect(shownSubagent(layout(merged))).toBe("b");
    expectWellFormed(layout(merged));
    expect(apply(merged, { type: "retarget_subagent", conversationId, from: "gone", to: "a" })).toBe(merged);
  });

  it("puts the tabs in the order they were dragged into", () => {
    const state = openAgent(openAgent(openAgent(initialSidePanesState, "a"), "b"), "c");
    const next = apply(state, { type: "reorder_subagents", conversationId, subagentIds: ["c", "a", "b", "ghost"] });
    expect(layout(next).subagentTabs).toEqual(["c", "a", "b"]);
    expect(shownSubagent(layout(next))).toBe("c");
    expect(apply(next, { type: "reorder_subagents", conversationId, subagentIds: ["c", "a", "b"] })).toBe(next);
  });

  it("opens no subagent pane without a tab, and never swaps one with another pane", () => {
    const state = stack("tasks");
    expect(open(state, "subagent")).toBe(state);
    const withAgent = openAgent(state, "a");
    expect(apply(withAgent, { type: "replace", conversationId, pane: "subagent", replacement: "history:1" })).toBe(withAgent);
    expect(apply(withAgent, { type: "replace", conversationId, pane: "tasks", replacement: "subagent" })).toBe(withAgent);
  });

  it("carries a draft's tabs onto the conversation it becomes", () => {
    const draft = sidePanesReducer(
      sidePanesReducer(initialSidePanesState, { type: "open_subagent", conversationId: "__draft__", subagentId: "a" }),
      { type: "open_subagent", conversationId: "__draft__", subagentId: "b" }
    );
    const next = apply(draft, { type: "adopt_conversation", conversationId, from: "__draft__" });
    expect(layout(next).subagentTabs).toEqual(["a", "b"]);
    expect(shownSubagent(layout(next))).toBe("b");
  });
});

describe("tasks pane tabs", () => {
  function showTab(state: SidePanesState, tab: "tasks" | "history") {
    return sidePanesReducer(state, { type: "show_tasks_tab", conversationId, tab });
  }

  it("opens the one pane on the tab asked for, and selects the other tab in place", () => {
    const history = showTab(stack("terminal"), "history");
    expect(layout(history).panes).toEqual(["terminal", "tasks"]);
    expect(shownTasksTab(layout(history))).toBe("history");
    expect(layout(history).focused).toBe("tasks");
    const tasks = showTab(apply(history, { type: "focus", conversationId, pane: "terminal" }), "tasks");
    // The other tab is the same pane: nothing moves, and the pane comes forward.
    expect(layout(tasks).panes).toBe(layout(history).panes);
    expect(layout(tasks).columns).toBe(layout(history).columns);
    expect(shownTasksTab(layout(tasks))).toBe("tasks");
    expect(layout(tasks).focused).toBe("tasks");
    expect(showTab(tasks, "tasks")).toBe(tasks);
  });

  it("shows no tab while the pane is closed, and reopens on the tab the way in names", () => {
    expect(shownTasksTab(layout(initialSidePanesState))).toBeNull();
    const closed = apply(showTab(initialSidePanesState, "history"), { type: "close", conversationId, pane: "tasks" });
    expect(shownTasksTab(layout(closed))).toBeNull();
    expect(shownTasksTab(layout(showTab(closed, "tasks")))).toBe("tasks");
    expect(shownTasksTab(layout(showTab(closed, "history")))).toBe("history");
  });

  it("brings an expanded pane's view back to the tasks pane when another pane covers it", () => {
    const expanded = apply(stack("tasks", "terminal"), { type: "toggle_expand", conversationId, pane: "terminal" });
    const next = showTab(expanded, "history");
    expect(expandedPane(layout(next))).toBeNull();
    expect(shownTasksTab(layout(next))).toBe("history");
  });

  it("ignores a tab the pane does not have", () => {
    const state = stack("tasks");
    expect(sidePanesReducer(state, {
      type: "show_tasks_tab", conversationId, tab: "plan" as unknown as "tasks"
    })).toBe(state);
  });
});

describe("column widths", () => {
  it("clamps each weight, follows the columns open and says nothing changed for the same weights", () => {
    const state = stack("terminal", "review", "tasks");
    const next = apply(state, { type: "set_column_flex", conversationId, columnFlex: [20, -1, 5] });
    expect(layout(next).columnFlex).toEqual([MAX_SIDE_FLEX, MIN_SIDE_FLEX]);
    expect(apply(next, { type: "set_column_flex", conversationId, columnFlex: [9, 0] })).toBe(next);
    expect(layout(apply(next, { type: "set_column_flex", conversationId, columnFlex: [2] })).columnFlex).toEqual([2, MIN_SIDE_FLEX]);
    expect(layout(next).sideFlex).toBe(layout(state).sideFlex);
  });

  it("remembers a kind's width only while the side area is one column", () => {
    const paired = apply(stack("review", "terminal"), { type: "set_side_flex", conversationId, sideFlex: 5 });
    expect(paired.lastSideFlexByKind).toEqual({ review: 5 });
    const columns = apply(open(paired, "tasks"), { type: "set_side_flex", conversationId, sideFlex: 7 });
    expect(layout(columns).sideFlex).toBe(7);
    expect(columns.lastSideFlexByKind).toEqual({ review: 5 });
  });

  it("carries a draft's columns over without the page that cannot follow", () => {
    const draft = "__draft__";
    let state = initialSidePanesState;
    for (const pane of ["terminal", "preview:__draft__#x", "tasks"] as SidePaneId[]) state = open(state, pane, draft);
    expect(sidePaneLayoutFor(state, draft).columns).toEqual([["terminal", "preview:__draft__#x"], ["tasks"]]);
    const adopted = apply(state, { type: "adopt_conversation", conversationId, from: draft });
    expect(layout(adopted).columns).toEqual([["terminal"], ["tasks"]]);
    expectWellFormed(layout(adopted));
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

describe("preview pages across workspaces", () => {
  it("files each page under the workspace it was opened for, and forgets it with the page", () => {
    let state = apply(initialSidePanesState, {
      type: "register_preview", conversationId, sessionId: conversationId, workspace: 2
    });
    state = apply(state, {
      type: "register_preview", conversationId, sessionId: `${conversationId}#tab_a`, workspace: 3
    });
    expect(previewSessionsFor(state, conversationId)).toEqual([conversationId, `${conversationId}#tab_a`]);
    expect(previewWorkspaceOf(state, conversationId)).toBe(2);
    expect(previewWorkspaceOf(state, `${conversationId}#tab_a`)).toBe(3);
    // A page nobody placed is workspace 1's, and re-registering without a workspace keeps the one it has.
    expect(previewWorkspaceOf(state, "unknown")).toBe(1);
    expect(apply(state, { type: "register_preview", conversationId, sessionId: conversationId })).toBe(state);

    state = apply(state, { type: "forget_preview", conversationId, sessionId: conversationId });
    expect(previewSessionsFor(state, conversationId)).toEqual([`${conversationId}#tab_a`]);
    expect(previewWorkspaceOf(state, conversationId)).toBe(1);
    expect(Object.keys(state.previewWorkspaces)).toEqual([`${conversationId}#tab_a`]);
  });

  it("moves a page to another workspace, and refuses a number that names none", () => {
    const state = apply(initialSidePanesState, {
      type: "register_preview", conversationId, sessionId: conversationId, workspace: 1
    });
    const moved = apply(state, { type: "set_preview_workspace", conversationId, sessionId: conversationId, workspace: 2 });
    expect(previewWorkspaceOf(moved, conversationId)).toBe(2);
    expect(apply(moved, { type: "set_preview_workspace", conversationId, sessionId: conversationId, workspace: 2 })).toBe(moved);
    expect(apply(moved, { type: "set_preview_workspace", conversationId, sessionId: conversationId, workspace: 0 })).toBe(moved);
    expect(apply(moved, { type: "set_preview_workspace", conversationId, sessionId: conversationId, workspace: 1.5 })).toBe(moved);
  });

  it("drops a removed conversation's page placements with its roster", () => {
    let state = apply(initialSidePanesState, {
      type: "register_preview", conversationId, sessionId: conversationId, workspace: 2
    });
    state = apply(state, { type: "register_preview", conversationId: "other", sessionId: "other", workspace: 3 });
    state = apply(state, { type: "remove_conversation", conversationId });
    expect(state.previewWorkspaces).toEqual({ other: 3 });
  });

  it("puts the pages in the order their tabs were dragged into, leaving the layout alone", () => {
    const second = `${conversationId}#tab_a`;
    const third = `${conversationId}#tab_b`;
    const state = stack(previewPaneId(conversationId), previewPaneId(second), previewPaneId(third));
    const moved = apply(state, { type: "reorder_previews", conversationId, sessionIds: [third, conversationId, second] });
    expect(previewSessionsFor(moved, conversationId)).toEqual([third, conversationId, second]);
    expect(moved.layoutByConversation).toBe(state.layoutByConversation);
    expect(moved.previewWorkspaces).toBe(state.previewWorkspaces);

    // A strip a render behind can name a page that just closed, or miss one that just opened.
    const stale = apply(state, {
      type: "reorder_previews", conversationId, sessionIds: ["gone", third, third, second]
    });
    expect(previewSessionsFor(stale, conversationId)).toEqual([third, second, conversationId]);
  });

  it("says nothing changed for the order a roster already has, or a roster that does not exist", () => {
    const state = stack(previewPaneId(conversationId), previewPaneId(`${conversationId}#tab_a`));
    const roster = [...previewSessionsFor(state, conversationId)];
    expect(apply(state, { type: "reorder_previews", conversationId, sessionIds: roster })).toBe(state);
    expect(apply(state, { type: "reorder_previews", conversationId, sessionIds: roster.slice(0, 1) })).toBe(state);
    expect(apply(state, { type: "reorder_previews", conversationId, sessionIds: [] })).toBe(state);
    expect(apply(state, {
      type: "reorder_previews", conversationId: "other", sessionIds: roster.reverse()
    })).toBe(state);
  });

  it("fills the conversation's own page first, and opens every further page as a tab", () => {
    expect(newPreviewPageSessionId([], "conv", "tab_x")).toBe("conv");
    expect(newPreviewPageSessionId(["conv#tab_a"], "conv", "tab_x")).toBe("conv");
    expect(newPreviewPageSessionId(["conv"], "conv", "tab_x")).toBe("conv#tab_x");
  });
});
