import { arrangeByIds } from "./reorder";

export type SidePaneKind ="terminal" | "review" | "preview" | "files" | "tasks" | "plan" | "history" | "settings" | "subagent";
export type SidePaneId =
  | "terminal" | "review" | "files" | "tasks" | "plan" | "settings" | "subagent"
  | `preview:${string}` | `history:${string}`;
/**
 * The `tasks` pane's two pages, as its tabs: the conversation's tasks and its history. Neither
 * closes, so the pane is open exactly while one of them is shown; the tasks tab is the left one.
 */
export type TasksPaneTab = "tasks" | "history";
const TASKS_PANE_TABS: readonly TasksPaneTab[] = Object.freeze(["tasks", "history"] satisfies TasksPaneTab[]);

export interface SidePaneLayout {
  /** Every open pane, in the order it was opened. */
  panes: SidePaneId[];
  /**
   * Where those panes sit: the side area's columns left → right, each listing its panes top →
   * bottom. Every open pane is in exactly one column and no column is empty.
   */
  columns: SidePaneId[][];
  /** The whole side area's width weight against the chat's `CHAT_TILE_FLEX`. */
  sideFlex: number;
  /** The columns' widths relative to one another, index for index. */
  columnFlex: number[];
  /** Each pane's height relative to the others in its column. */
  paneFlex: Record<string, number>;
  focused: SidePaneId | null;
  /**
   * The agents the `subagent` pane has a tab for, in tab order. Empty exactly while that pane is
   * closed: closing the pane drops its tabs, and the last tab leaving closes the pane.
   */
  subagentTabs: string[];
  /** The tab the `subagent` pane shows: one of `subagentTabs`, or null while there are none. */
  activeSubagent: string | null;
  /** The tab the `tasks` pane shows. Kept while the pane is closed; every way in names its tab. */
  tasksTab: TasksPaneTab;
  /**
   * The pane shown alone over the whole workspace, chat column included. Never persisted, and
   * dropped by anything that would leave the user staring at a pane they did not ask for.
   */
  expanded: SidePaneId | null;
}

export interface SidePanesState {
  layoutByConversation: Record<string, SidePaneLayout>;
  /**
   * Each conversation's preview pages, in tab order. A page is a native browser session: the
   * conversation's own page is keyed by the conversation id, every further page by
   * `<conversation>#<token>`.
   */
  previewSessions: Record<string, string[]>;
  /**
   * Which of its conversation's workspaces (1-based, the model's numbering) each page belongs to,
   * by session id: the workspace whose `.mework/launch.json` its start page lists and whose
   * servers it runs. Absent is workspace 1.
   */
  previewWorkspaces: Record<string, number>;
  lastSideFlexByKind: Partial<Record<SidePaneKind, number>>;
}

export type SidePanesAction =
  | { type: "open"; conversationId: string; pane: SidePaneId }
  | { type: "toggle"; conversationId: string; pane: SidePaneId }
  | { type: "close"; conversationId: string; pane: SidePaneId }
  | { type: "close_last"; conversationId: string }
  /**
   * Puts `replacement` where `pane` is — same column, same height, same focus — for a pane whose
   * subject turned out to go by another id. Closing and reopening would move it to wherever a new
   * pane goes. When `replacement` is already open, `pane` only closes.
   */
  | { type: "replace"; conversationId: string; pane: SidePaneId; replacement: SidePaneId }
  | { type: "focus"; conversationId: string; pane: SidePaneId }
  | { type: "toggle_expand"; conversationId: string; pane: SidePaneId }
  | { type: "set_side_flex"; conversationId: string; sideFlex: number }
  | { type: "set_column_flex"; conversationId: string; columnFlex: number[] }
  | { type: "set_pane_flex"; conversationId: string; paneFlex: Record<string, number> }
  /**
   * Shows an agent's transcript: gives it a tab in the `subagent` pane — after the others, unless
   * it has one already — selects that tab, and opens the pane where a new pane goes if it is not
   * open yet.
   */
  | { type: "open_subagent"; conversationId: string; subagentId: string }
  /**
   * Takes an agent's tab away. Closing the shown tab shows the one to its right, or to its left
   * off the end; closing the last one closes the pane.
   */
  | { type: "close_subagent"; conversationId: string; subagentId: string }
  /**
   * Points an agent's tab at the id the agent turned out to go by — a call id settling into the
   * stable name id — in the same place in the strip. When the new id has a tab already, the old
   * one only closes.
   */
  | { type: "retarget_subagent"; conversationId: string; from: string; to: string }
  /** Puts the tabs in the order they were dragged into; ids the strip does not hold are ignored. */
  | { type: "reorder_subagents"; conversationId: string; subagentIds: string[] }
  /** Selects one of the `tasks` pane's tabs, opening the pane where a new pane goes if it is closed. */
  | { type: "show_tasks_tab"; conversationId: string; tab: TasksPaneTab }
  /**
   * Adds a page to the conversation's roster. `workspace` records which workspace it belongs to;
   * without one a page keeps what it had, and a new page belongs to workspace 1.
   */
  | { type: "register_preview"; conversationId: string; sessionId: string; workspace?: number }
  | { type: "forget_preview"; conversationId: string; sessionId: string }
  /** Moves a page to another workspace — the one whose server it turned out to be showing. */
  | { type: "set_preview_workspace"; conversationId: string; sessionId: string; workspace: number }
  /**
   * Puts the conversation's pages in the order their tabs were dragged into. An id the roster does
   * not hold is ignored, and a page the request leaves out keeps its relative place after the rest.
   */
  | { type: "reorder_previews"; conversationId: string; sessionIds: string[] }
  /**
   * Moves the layout a draft accumulated under its placeholder id onto the real
   * conversation it just became, the way the composer's text and attachments move.
   * Without it a pane opened in a draft is dropped on send and left behind to
   * reappear, unasked, in the next draft. The draft's preview roster comes along
   * too: its page is already named after the conversation it became.
   */
  | { type: "adopt_conversation"; conversationId: string; from: string }
  | { type: "remove_conversation"; conversationId: string };

export const CHAT_TILE_FLEX = 2;
export const MIN_SIDE_FLEX = 0.25;
export const MAX_SIDE_FLEX = 8;
/** How much wider the side area grows for each column opened after the first: half the chat's weight. */
export const NEW_COLUMN_FLEX = CHAT_TILE_FLEX / 2;
/** A conversation shows at most this many side panes; asking for another closes one first. */
export const MAX_SIDE_PANES = 9;
/** The tallest a column grows: two columns hold six panes, three hold all nine. */
export const MAX_PANES_PER_COLUMN = 3;
/**
 * The order panes are kept in when the side area is full, most kept first: opening a pane past
 * `MAX_SIDE_PANES` closes the open pane furthest down this list, the oldest of its kind. Each of
 * the first eight kinds opens at most one pane — agents share the one `subagent` pane as its
 * tabs, and the conversation's history is a tab of the `tasks` pane — so nine open panes always
 * include an agent's history, and none of the first eight is ever closed to make room.
 */
export const PANE_KEEP_ORDER: readonly SidePaneKind[] = Object.freeze([
  "settings", "plan", "preview", "terminal", "review", "files", "tasks", "subagent", "history"
] satisfies SidePaneKind[]);
export const initialSidePanesState: SidePanesState = {
  layoutByConversation: {},
  previewSessions: {},
  previewWorkspaces: {},
  lastSideFlexByKind: {}
};

const EMPTY_LAYOUT: SidePaneLayout = {
  panes: [], columns: [], sideFlex: 1, columnFlex: [], paneFlex: {}, focused: null,
  subagentTabs: [], activeSubagent: null, tasksTab: "tasks", expanded: null
};
Object.freeze(EMPTY_LAYOUT.panes);
Object.freeze(EMPTY_LAYOUT.subagentTabs);
Object.freeze(EMPTY_LAYOUT.columns);
Object.freeze(EMPTY_LAYOUT.columnFlex);
Object.freeze(EMPTY_LAYOUT.paneFlex);
Object.freeze(EMPTY_LAYOUT);
const NO_PREVIEW_SESSIONS: readonly string[] = Object.freeze([]);
const STORAGE_KEY = "mework.sidePanes.sideFlexByKind";
const KINDS: SidePaneKind[] = ["terminal", "review", "preview", "files", "tasks", "plan", "history", "settings", "subagent"];

export function sidePaneLayoutFor(state: SidePanesState, conversationId: string | null): SidePaneLayout {
  if (!conversationId) return EMPTY_LAYOUT;
  return Object.prototype.hasOwnProperty.call(state.layoutByConversation, conversationId)
    ? state.layoutByConversation[conversationId] : EMPTY_LAYOUT;
}

export function previewSessionsFor(state: SidePanesState, conversationId: string | null): readonly string[] {
  if (!conversationId) return NO_PREVIEW_SESSIONS;
  return Object.prototype.hasOwnProperty.call(state.previewSessions, conversationId)
    ? state.previewSessions[conversationId] : NO_PREVIEW_SESSIONS;
}

/** The workspace a page belongs to, 1-based. A page nobody placed belongs to workspace 1. */
export function previewWorkspaceOf(state: SidePanesState, sessionId: string): number {
  const workspace = Object.prototype.hasOwnProperty.call(state.previewWorkspaces, sessionId)
    ? state.previewWorkspaces[sessionId] : undefined;
  return typeof workspace === "number" && Number.isInteger(workspace) && workspace >= 1 ? workspace : 1;
}

export function paneKind(id: SidePaneId): SidePaneKind {
  return id.split(":", 1)[0] as SidePaneKind;
}

export function paneTarget(id: SidePaneId): string | null {
  const colon = id.indexOf(":");
  return colon < 0 ? null : id.slice(colon + 1);
}

export function previewPaneId(sessionId: string): SidePaneId { return `preview:${sessionId}`; }
/**
 * One agent's history. Always targeted: a conversation's history holds each
 * agent it spawned apart from its own, and the conversation's own is the
 * `tasks` pane's history tab, so the two open side by side.
 */
export function subagentHistoryPaneId(id: string): SidePaneId { return `history:${id}`; }
export function paneIsOpen(layout: SidePaneLayout, id: SidePaneId): boolean { return layout.panes.includes(id); }

/** The `tasks` pane's tab on show, or null while the pane is closed. */
export function shownTasksTab(layout: SidePaneLayout): TasksPaneTab | null {
  return paneIsOpen(layout, "tasks") ? layout.tasksTab : null;
}

/** The agent whose transcript the `subagent` pane is showing, or null while the pane is closed. */
export function shownSubagent(layout: SidePaneLayout): string | null {
  return paneIsOpen(layout, "subagent") ? layout.activeSubagent : null;
}

export function openPreviewSession(layout: SidePaneLayout): string | null {
  const pane = layout.panes.find((id) => paneKind(id) === "preview");
  return pane === undefined ? null : paneTarget(pane);
}

export function focusedPane(layout: SidePaneLayout): SidePaneId | null {
  return layout.focused !== null && paneIsOpen(layout, layout.focused)
    ? layout.focused : layout.panes[layout.panes.length - 1] ?? null;
}

/** The expanded pane, or null once it is no longer open. Closing a pane un-expands it for free. */
export function expandedPane(layout: SidePaneLayout): SidePaneId | null {
  return layout.expanded !== null && paneIsOpen(layout, layout.expanded) ? layout.expanded : null;
}

export function defaultSideFlexForKind(kind: SidePaneKind): number {
  // Conversation settings is a two-column page of its own, so it opens as wide as the
  // surfaces that carry a document rather than as narrow as the stacked list panes.
  return kind === "review" || kind === "preview" || kind === "files" || kind === "settings" ? 3 : 1;
}

export function sidePaneDomId(id: SidePaneId): string {
  const encoded = Array.from(id, (character) => (
    character.codePointAt(0)?.toString(36) ?? "0"
  )).join("-");
  return `side-pane-${encoded}`;
}

export function previewTabSessionId(conversationId: string, token: string): string {
  return `${conversationId}#${token}`;
}

/**
 * The session a new page opens under: the owner's own page when the roster does not hold it — it
 * is the page the model's preview tools drive, so it is the first one to fill — and otherwise a
 * fresh tab. `ownerId` is the host identity the pages are named after (for the draft, the id it
 * will materialize as), which is not always the key its roster is filed under. `token` is only
 * consulted for a tab, and has to be one the host accepts after `#`: letters, digits, hyphens and
 * underscores.
 */
export function newPreviewPageSessionId(
  roster: readonly string[],
  ownerId: string,
  token: string
): string {
  if (!roster.includes(ownerId)) return ownerId;
  return previewTabSessionId(ownerId, token);
}

export function previewSessionBelongsToConversation(
  sessionId: string,
  conversationId: string
): boolean {
  return sessionId === conversationId || sessionId.startsWith(`${conversationId}#`);
}

export function isPrimaryPreviewSession(sessionId: string, conversationId: string): boolean {
  return sessionId === conversationId;
}

function clampFlex(value: number): number {
  return Number.isNaN(value) ? MIN_SIDE_FLEX : Math.min(MAX_SIDE_FLEX, Math.max(MIN_SIDE_FLEX, value));
}

/** How many columns `count` side panes may spread over: two, and a third once there are more than six. */
export function sideColumnLimit(count: number): number {
  return count > 2 * MAX_PANES_PER_COLUMN ? 3 : 2;
}

function keepRank(pane: SidePaneId): number {
  const rank = PANE_KEEP_ORDER.indexOf(paneKind(pane));
  return rank < 0 ? PANE_KEEP_ORDER.length : rank;
}

/** The pane a full side area gives up for a new one: the least kept kind, and of those the first opened. */
export function paneClosedForRoom(layout: SidePaneLayout): SidePaneId | null {
  let chosen: SidePaneId | null = null;
  for (const pane of layout.panes) {
    if (chosen === null || keepRank(pane) > keepRank(chosen)) chosen = pane;
  }
  return chosen;
}

/** The column with the fewest panes; the rightmost of equals, next to where a new column would open. */
function shortestColumn(columns: readonly (readonly SidePaneId[])[]): number {
  let shortest = -1;
  columns.forEach((column, index) => {
    if (shortest < 0 || column.length <= columns[shortest].length) shortest = index;
  });
  return shortest;
}

/** Scales weights to average one, so they stay comparable to the clamp range however columns come and go. */
function normalizedWeights(weights: readonly number[]): number[] {
  const total = weights.reduce((sum, weight) => sum + weight, 0);
  return total > 0 ? weights.map((weight) => weight * weights.length / total) : weights.map(() => 1);
}

type ColumnGeometry = Pick<SidePaneLayout, "columns" | "columnFlex" | "sideFlex">;

function columnWeights(layout: ColumnGeometry): number[] {
  return layout.columns.map((_, index) => {
    const weight = layout.columnFlex[index];
    return typeof weight === "number" && Number.isFinite(weight) && weight > 0 ? weight : 1;
  });
}

/**
 * Opens a column on the right. The chat gives up the room, the way the reference shell opens one
 * — the side area grows by `NEW_COLUMN_FLEX` — and the whole side area is then split evenly
 * between the columns, whatever widths they had: weighing the new column against the old ones
 * left it a sliver beside a document-wide first column (2026-10-02, user's call).
 */
function withColumn(layout: ColumnGeometry, pane: SidePaneId): ColumnGeometry {
  const columns = [...layout.columns, [pane]];
  return {
    columns,
    columnFlex: columns.map(() => 1),
    sideFlex: clampFlex(layout.sideFlex + NEW_COLUMN_FLEX)
  };
}

/**
 * Removes a column; the others close up over its room in proportion. The chat takes back what
 * opening a column took from it — or the column's whole width, once the area has been narrowed
 * below that — so a column opened and closed again leaves the side area as it was. Shrinking by
 * the column's own width would not: opening splits the area evenly, so every open and close
 * would leave it narrower.
 */
function withoutColumn(layout: ColumnGeometry, index: number): ColumnGeometry {
  const weights = columnWeights(layout);
  const total = weights.reduce((sum, weight) => sum + weight, 0);
  const columns = layout.columns.filter((_, column) => column !== index);
  const remaining = weights.filter((_, column) => column !== index);
  const width = layout.sideFlex * weights[index] / total;
  return {
    columns,
    columnFlex: normalizedWeights(remaining),
    // With nothing left the width is kept, the way closing the last pane always kept it.
    sideFlex: columns.length ? clampFlex(layout.sideFlex - Math.min(NEW_COLUMN_FLEX, width)) : layout.sideFlex
  };
}

/**
 * Places a newly opened pane. The first opens the side area; after that the reference shell's
 * rule: a column holding a single pane takes the new one beneath it, and a column already
 * holding more is left alone for a new column beside it. Once the columns allowed for this many
 * panes are all open, the pane joins the shortest.
 */
function withPanePlaced(layout: SidePaneLayout, pane: SidePaneId, firstSideFlex: number): SidePaneLayout {
  const panes = [...layout.panes, pane];
  if (layout.columns.length === 0) return { ...layout, panes, columns: [[pane]], columnFlex: [1], sideFlex: firstSideFlex };
  const last = layout.columns.length - 1;
  const target = layout.columns[last].length === 1 ? last
    : layout.columns.length < sideColumnLimit(panes.length) ? -1
      : shortestColumn(layout.columns);
  if (target < 0) return { ...layout, panes, ...withColumn(layout, pane) };
  return { ...layout, panes, columns: layout.columns.map((column, index) => index === target ? [...column, pane] : column) };
}

/**
 * Takes a pane out of the side area. Its column closes once empty, and when fewer panes no longer
 * earn the columns open, the column holding the fewest is dissolved into the shortest others —
 * a third column only lasts while more than six panes are open.
 */
function withoutPane(layout: SidePaneLayout, pane: SidePaneId): SidePaneLayout {
  if (!paneIsOpen(layout, pane)) return layout;
  const panes = layout.panes.filter((id) => id !== pane);
  const paneFlex = { ...layout.paneFlex };
  delete paneFlex[pane];
  let geometry: ColumnGeometry = {
    ...layout,
    columns: layout.columns.map((column) => column.includes(pane) ? column.filter((id) => id !== pane) : column)
  };
  const emptied = geometry.columns.findIndex((column) => column.length === 0);
  if (emptied >= 0) geometry = withoutColumn(geometry, emptied);
  while (geometry.columns.length > sideColumnLimit(panes.length)) {
    const dissolved = shortestColumn(geometry.columns);
    const moving = geometry.columns[dissolved];
    geometry = withoutColumn(geometry, dissolved);
    for (const id of moving) {
      const into = shortestColumn(geometry.columns);
      geometry = { ...geometry, columns: geometry.columns.map((column, index) => index === into ? [...column, id] : column) };
      // A height weighed against its old neighbours means nothing among new ones.
      delete paneFlex[id];
    }
  }
  // The tabs are the pane's: they go with it, and opening it again starts from the one asked for.
  const tabs = pane === "subagent" ? { subagentTabs: [], activeSubagent: null } : {};
  return { ...layout, ...geometry, panes, paneFlex, ...tabs };
}

/** Gives `from`'s place — column, height, focus, expansion — to `to`, which must not be open. */
function withPaneRenamed(layout: SidePaneLayout, from: SidePaneId, to: SidePaneId): SidePaneLayout {
  const rename = (id: SidePaneId) => id === from ? to : id;
  let paneFlex = layout.paneFlex;
  if (Object.prototype.hasOwnProperty.call(paneFlex, from)) {
    paneFlex = { ...paneFlex, [to]: paneFlex[from] };
    delete paneFlex[from];
  }
  return {
    ...layout,
    panes: layout.panes.map(rename),
    columns: layout.columns.map((column) => column.includes(from) ? column.map(rename) : column),
    paneFlex,
    focused: layout.focused === null ? null : rename(layout.focused),
    expanded: layout.expanded === null ? null : rename(layout.expanded)
  };
}

function withLayout(state: SidePanesState, conversationId: string, layout: SidePaneLayout): SidePanesState {
  if (sidePaneLayoutFor(state, conversationId) === layout) return state;
  return { ...state, layoutByConversation: { ...state.layoutByConversation, [conversationId]: layout } };
}

export function sidePanesReducer(state: SidePanesState, action: SidePanesAction): SidePanesState {
  const { conversationId } = action;
  const layout = sidePaneLayoutFor(state, conversationId);
  switch (action.type) {
    case "open": {
      const kind = paneKind(action.pane);
      // A subagent pane without a tab has no transcript to show; `open_subagent` opens it.
      if (action.pane === "subagent" && layout.subagentTabs.length === 0) return state;
      const next = kind === "preview" ? sidePanesReducer(state, {
        type: "register_preview", conversationId, sessionId: paneTarget(action.pane)!
      }) : state;
      if (paneIsOpen(layout, action.pane)) {
        // Asking for a pane that some other pane is currently covering means asking to see it.
        const expanded = layout.expanded === action.pane ? layout.expanded : null;
        return layout.focused === action.pane && layout.expanded === expanded ? next
          : withLayout(next, conversationId, { ...layout, focused: action.pane, expanded });
      }
      // A conversation shows one page at a time, so a second one takes the first one's place —
      // its expansion included: switching pages or adding one inside a maximized preview keeps
      // it maximized. A page opened while another pane covers the preview is uncovered instead.
      const previous = kind === "preview" ? layout.panes.find((id) => paneKind(id) === "preview") : undefined;
      if (previous !== undefined) {
        return withLayout(next, conversationId, {
          ...withPaneRenamed(layout, previous, action.pane),
          focused: action.pane,
          expanded: layout.expanded === previous ? action.pane : null
        });
      }
      let room = layout;
      while (room.panes.length >= MAX_SIDE_PANES) room = withoutPane(room, paneClosedForRoom(room)!);
      return withLayout(next, conversationId, {
        ...withPanePlaced(room, action.pane, state.lastSideFlexByKind[kind] ?? defaultSideFlexForKind(kind)),
        focused: action.pane, expanded: null
      });
    }
    case "toggle":
      return sidePanesReducer(state, { ...action, type: paneIsOpen(layout, action.pane) ? "close" : "open" });
    case "close": {
      if (!paneIsOpen(layout, action.pane)) return state;
      const remaining = withoutPane(layout, action.pane);
      return withLayout(state, conversationId, { ...remaining, focused: focusedPane(remaining) });
    }
    case "close_last": {
      const pane = focusedPane(layout);
      return pane === null ? state : sidePanesReducer(state, { type: "close", conversationId, pane });
    }
    case "replace": {
      if (!paneIsOpen(layout, action.pane) || action.pane === action.replacement) return state;
      // The subagent pane's subject is its shown tab, which `retarget_subagent` moves; the pane
      // itself is never another pane, and no other pane turns into it without a tab to show.
      if (action.pane === "subagent" || action.replacement === "subagent") return state;
      if (paneIsOpen(layout, action.replacement)) {
        return sidePanesReducer(state, { type: "close", conversationId, pane: action.pane });
      }
      return withLayout(state, conversationId, withPaneRenamed(layout, action.pane, action.replacement));
    }
    case "focus":
      return !paneIsOpen(layout, action.pane) || layout.focused === action.pane ? state
        : withLayout(state, conversationId, { ...layout, focused: action.pane });
    case "toggle_expand": {
      if (!paneIsOpen(layout, action.pane)) return state;
      return withLayout(state, conversationId, {
        ...layout,
        focused: action.pane,
        expanded: layout.expanded === action.pane ? null : action.pane
      });
    }
    case "set_side_flex": {
      const sideFlex = clampFlex(action.sideFlex);
      // Only a single column's width says how wide that kind of pane likes to open; several
      // columns' combined width would open the next lone pane far too wide.
      const kind = layout.panes.length && layout.columns.length === 1 ? paneKind(layout.panes[0]) : null;
      const next = kind !== null && state.lastSideFlexByKind[kind] !== sideFlex
        ? { ...state, lastSideFlexByKind: { ...state.lastSideFlexByKind, [kind]: sideFlex } } : state;
      return layout.sideFlex === sideFlex ? next : withLayout(next, conversationId, { ...layout, sideFlex });
    }
    case "set_column_flex": {
      const columnFlex = layout.columns.map((_, index) => clampFlex(action.columnFlex[index] ?? layout.columnFlex[index] ?? 1));
      if (columnFlex.length === layout.columnFlex.length
        && columnFlex.every((weight, index) => weight === layout.columnFlex[index])) return state;
      return withLayout(state, conversationId, { ...layout, columnFlex });
    }
    case "set_pane_flex": {
      const paneFlex = Object.fromEntries(Object.entries(action.paneFlex).map(([id, value]) => [id, clampFlex(value)]));
      if (Object.keys(paneFlex).length === Object.keys(layout.paneFlex).length
        && Object.keys(paneFlex).every((id) => paneFlex[id] === layout.paneFlex[id])) return state;
      return withLayout(state, conversationId, { ...layout, paneFlex });
    }
    case "open_subagent": {
      const subagentTabs = layout.subagentTabs.includes(action.subagentId)
        ? layout.subagentTabs : [...layout.subagentTabs, action.subagentId];
      const selected = subagentTabs === layout.subagentTabs && layout.activeSubagent === action.subagentId
        ? state : withLayout(state, conversationId, { ...layout, subagentTabs, activeSubagent: action.subagentId });
      return sidePanesReducer(selected, { type: "open", conversationId, pane: "subagent" });
    }
    case "close_subagent": {
      const index = layout.subagentTabs.indexOf(action.subagentId);
      if (index < 0) return state;
      const subagentTabs = layout.subagentTabs.filter((id) => id !== action.subagentId);
      if (subagentTabs.length === 0) return sidePanesReducer(state, { type: "close", conversationId, pane: "subagent" });
      // Focus falls to the right, as a browser's does, and to the left off the end.
      const activeSubagent = layout.activeSubagent !== action.subagentId ? layout.activeSubagent
        : subagentTabs[Math.min(index, subagentTabs.length - 1)];
      return withLayout(state, conversationId, { ...layout, subagentTabs, activeSubagent });
    }
    case "retarget_subagent": {
      if (action.from === action.to || !layout.subagentTabs.includes(action.from)) return state;
      const subagentTabs = layout.subagentTabs.includes(action.to)
        ? layout.subagentTabs.filter((id) => id !== action.from)
        : layout.subagentTabs.map((id) => id === action.from ? action.to : id);
      const activeSubagent = layout.activeSubagent === action.from ? action.to : layout.activeSubagent;
      return withLayout(state, conversationId, { ...layout, subagentTabs, activeSubagent });
    }
    case "reorder_subagents": {
      const subagentTabs = arrangeByIds(layout.subagentTabs, action.subagentIds, (id) => id);
      return subagentTabs === null ? state : withLayout(state, conversationId, { ...layout, subagentTabs });
    }
    case "show_tasks_tab": {
      if (!TASKS_PANE_TABS.includes(action.tab)) return state;
      const selected = layout.tasksTab === action.tab
        ? state : withLayout(state, conversationId, { ...layout, tasksTab: action.tab });
      return sidePanesReducer(selected, { type: "open", conversationId, pane: "tasks" });
    }
    case "register_preview": {
      const sessions = previewSessionsFor(state, conversationId);
      const placed = action.workspace === undefined ? state
        : sidePanesReducer(state, {
          type: "set_preview_workspace", conversationId, sessionId: action.sessionId, workspace: action.workspace
        });
      if (sessions.includes(action.sessionId)) return placed;
      return { ...placed, previewSessions: { ...placed.previewSessions, [conversationId]: [...sessions, action.sessionId] } };
    }
    case "forget_preview": {
      const next = sidePanesReducer(state, { type: "close", conversationId, pane: previewPaneId(action.sessionId) });
      const sessions = previewSessionsFor(state, conversationId);
      const placed = Object.prototype.hasOwnProperty.call(next.previewWorkspaces, action.sessionId);
      if (!sessions.includes(action.sessionId) && !placed) return next;
      const remaining = sessions.filter((sessionId) => sessionId !== action.sessionId);
      const previewSessions = { ...state.previewSessions };
      if (remaining.length) previewSessions[conversationId] = remaining;
      else delete previewSessions[conversationId];
      const previewWorkspaces = { ...next.previewWorkspaces };
      delete previewWorkspaces[action.sessionId];
      return { ...next, previewSessions, previewWorkspaces };
    }
    case "set_preview_workspace": {
      if (!Number.isInteger(action.workspace) || action.workspace < 1) return state;
      if (state.previewWorkspaces[action.sessionId] === action.workspace
        && Object.prototype.hasOwnProperty.call(state.previewWorkspaces, action.sessionId)) return state;
      return { ...state, previewWorkspaces: { ...state.previewWorkspaces, [action.sessionId]: action.workspace } };
    }
    case "reorder_previews": {
      const sessions = arrangeByIds(previewSessionsFor(state, conversationId), action.sessionIds, (id) => id);
      return sessions === null ? state
        : { ...state, previewSessions: { ...state.previewSessions, [conversationId]: sessions } };
    }
    case "adopt_conversation": {
      const adopted = Object.prototype.hasOwnProperty.call(state.layoutByConversation, action.from)
        ? state.layoutByConversation[action.from] : null;
      if (adopted === null || action.from === conversationId) return state;
      const layoutByConversation = { ...state.layoutByConversation };
      delete layoutByConversation[action.from];
      // The draft's page is keyed by the id it materializes as, so its pane follows. A session
      // id minted under the placeholder id names nothing the conversation owns and stays behind.
      const follows = (sessionId: string) => previewSessionBelongsToConversation(sessionId, conversationId);
      const carried = adopted.panes.reduce<SidePaneLayout>((kept, pane) => (
        paneKind(pane) !== "preview" || follows(paneTarget(pane) ?? "") ? kept : withoutPane(kept, pane)
      ), { ...adopted, expanded: null });
      layoutByConversation[conversationId] = { ...carried, focused: focusedPane(carried) };
      const previewSessions = { ...state.previewSessions };
      const roster = previewSessionsFor(state, action.from).filter(follows);
      delete previewSessions[action.from];
      if (roster.length) {
        previewSessions[conversationId] = [
          ...previewSessionsFor(state, conversationId).filter((sessionId) => !roster.includes(sessionId)),
          ...roster
        ];
      }
      return { ...state, layoutByConversation, previewSessions };
    }
    case "remove_conversation": {
      if (!Object.prototype.hasOwnProperty.call(state.layoutByConversation, conversationId)
        && !Object.prototype.hasOwnProperty.call(state.previewSessions, conversationId)) return state;
      const layoutByConversation = { ...state.layoutByConversation };
      const previewSessions = { ...state.previewSessions };
      const previewWorkspaces = { ...state.previewWorkspaces };
      for (const sessionId of previewSessionsFor(state, conversationId)) delete previewWorkspaces[sessionId];
      delete layoutByConversation[conversationId];
      delete previewSessions[conversationId];
      return { ...state, layoutByConversation, previewSessions, previewWorkspaces };
    }
  }
}

function storedFlex(value: unknown): Partial<Record<SidePaneKind, number>> {
  const result: Partial<Record<SidePaneKind, number>> = {};
  if (!value || typeof value !== "object" || Array.isArray(value)) return result;
  for (const kind of KINDS) {
    const flex = (value as Record<string, unknown>)[kind];
    if (typeof flex === "number" && !Number.isNaN(flex)) result[kind] = clampFlex(flex);
  }
  return result;
}

export function loadSidePanesState(): SidePanesState {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    return { ...initialSidePanesState, lastSideFlexByKind: storedFlex(raw === null ? null : JSON.parse(raw)) };
  } catch {
    return initialSidePanesState;
  }
}

export function persistSidePanesState(state: SidePanesState): void {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(storedFlex(state.lastSideFlexByKind)));
  } catch {
    // Layout remains usable when storage is unavailable or full.
  }
}
