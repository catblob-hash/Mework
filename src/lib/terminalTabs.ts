import type { TerminalLaunchChoice } from "./terminal";

/**
 * The terminal pane's tab strip, one entry per PTY.
 *
 * The pane is a window onto a set of shells the way a browser window is a window onto a set of
 * pages: the tabs own the sessions, not the pane. Closing the pane only takes the window away —
 * every shell keeps running and comes back with the pane. Ending a shell is a per-tab act, and
 * the last tab leaving is what takes the pane with it.
 *
 * Live session state is not here: `terminalController` holds that, and it drops a session the
 * moment it stops being live. A tab has to outlive its session — a shell that failed sits on
 * screen with its verdict until the user retries or closes it — so the two are separate.
 */

export interface TerminalTab {
  /** The host's terminal id, unique within the conversation and never reused. */
  id: string;
  /**
   * Creation order within the conversation. Ids are minted from it and never reused, so a
   * torn-down shell's address can never come back; the derived name's number is not this —
   * it is the tab's place in the strip, and a closed shell's number goes back into the pool.
   */
  ordinal: number;
  /** The user's name for this terminal; null follows the derived one. */
  name: string | null;
  /**
   * The workspace and shell this tab's terminal was asked for. `null` leaves both
   * to the host — workspace 1 and its machine's default shell — which is what a
   * tab nobody chose for (the one every conversation starts with) gets.
   */
  launch: TerminalLaunchChoice | null;
}

export interface TerminalTabsLayout {
  tabs: TerminalTab[];
  activeId: string | null;
  /** Monotonic: an ordinal is spent when its tab is created and never minted again. */
  nextOrdinal: number;
}

export interface TerminalTabsState {
  byConversation: Record<string, TerminalTabsLayout>;
}

export type TerminalTabsAction =
  /** Gives the conversation a tab if it has none; the way opening the pane finds something to show. */
  | { type: "ensure"; conversationId: string }
  | { type: "add"; conversationId: string; launch?: TerminalLaunchChoice | null }
  /**
   * Gives a tab that has not started a shell yet the workspace and shell it should start. The
   * way a menu choice lands in the tab every conversation starts with instead of beside it.
   */
  | { type: "configure"; conversationId: string; terminalId: string; launch: TerminalLaunchChoice }
  | { type: "close"; conversationId: string; terminalId: string }
  | { type: "activate"; conversationId: string; terminalId: string }
  | { type: "rename"; conversationId: string; terminalId: string; name: string }
  | { type: "remove_conversation"; conversationId: string };

export const initialTerminalTabsState: TerminalTabsState = { byConversation: {} };

const ID_PREFIX = "terminal-";

/** Ids are per conversation, as the host's `(conversationId, terminalId)` key already is. */
export function terminalTabId(ordinal: number): string {
  return `${ID_PREFIX}${ordinal}`;
}

/**
 * What a conversation starts with. Every conversation has one terminal before anyone asks for
 * it, so the pane has something to park: the panel renders its region — and so keeps a stable
 * element to reopen into — long before it is ever expanded into a live shell.
 */
const FIRST_LAYOUT: TerminalTabsLayout = {
  tabs: [{ id: terminalTabId(1), ordinal: 1, name: null, launch: null }],
  activeId: terminalTabId(1),
  nextOrdinal: 2
};
Object.freeze(FIRST_LAYOUT.tabs[0]);
Object.freeze(FIRST_LAYOUT.tabs);
Object.freeze(FIRST_LAYOUT);

export function terminalTabsFor(
  state: TerminalTabsState,
  conversationId: string | null
): TerminalTabsLayout {
  if (!conversationId) return FIRST_LAYOUT;
  return Object.prototype.hasOwnProperty.call(state.byConversation, conversationId)
    ? state.byConversation[conversationId] : FIRST_LAYOUT;
}

export function activeTerminalTab(layout: TerminalTabsLayout): TerminalTab | null {
  return layout.tabs.find((tab) => tab.id === layout.activeId) ?? null;
}

/**
 * The number a tab shows when the user has not named it: its place in the strip, as the
 * reference shell numbers its tabs. Not the ordinal — that is spent the moment a shell is
 * torn down and never minted again, which keeps ids unique but would make every closed
 * terminal cost its number forever.
 */
export function terminalDisplayNumber(layout: TerminalTabsLayout, terminalId: string): number {
  return layout.tabs.findIndex((tab) => tab.id === terminalId) + 1;
}

function withLayout(
  state: TerminalTabsState,
  conversationId: string,
  layout: TerminalTabsLayout
): TerminalTabsState {
  if (terminalTabsFor(state, conversationId) === layout) return state;
  return { ...state, byConversation: { ...state.byConversation, [conversationId]: layout } };
}

function added(
  layout: TerminalTabsLayout,
  launch: TerminalLaunchChoice | null = null
): TerminalTabsLayout {
  const ordinal = layout.nextOrdinal;
  const id = terminalTabId(ordinal);
  return {
    tabs: [...layout.tabs, { id, ordinal, name: null, launch }],
    activeId: id,
    nextOrdinal: ordinal + 1
  };
}

export function terminalTabsReducer(
  state: TerminalTabsState,
  action: TerminalTabsAction
): TerminalTabsState {
  const { conversationId } = action;
  const layout = terminalTabsFor(state, conversationId);
  switch (action.type) {
    case "ensure":
      return layout.tabs.length > 0 ? state : withLayout(state, conversationId, added(layout));
    case "add":
      return withLayout(state, conversationId, added(layout, action.launch ?? null));
    case "configure": {
      const tab = layout.tabs.find((candidate) => candidate.id === action.terminalId);
      if (!tab) return state;
      return withLayout(state, conversationId, {
        ...layout,
        activeId: tab.id,
        tabs: layout.tabs.map((candidate) => (
          candidate.id === action.terminalId ? { ...candidate, launch: action.launch } : candidate
        ))
      });
    }
    case "close": {
      const index = layout.tabs.findIndex((tab) => tab.id === action.terminalId);
      if (index < 0) return state;
      const tabs = layout.tabs.filter((tab) => tab.id !== action.terminalId);
      // Focus falls to the right, as a browser's does, and to the left off the end.
      const activeId = layout.activeId !== action.terminalId ? layout.activeId
        : tabs.length === 0 ? null : tabs[Math.min(index, tabs.length - 1)].id;
      return withLayout(state, conversationId, { ...layout, tabs, activeId });
    }
    case "activate":
      return layout.activeId === action.terminalId
        || !layout.tabs.some((tab) => tab.id === action.terminalId) ? state
        : withLayout(state, conversationId, { ...layout, activeId: action.terminalId });
    case "rename": {
      const name = action.name.trim() || null;
      const tab = layout.tabs.find((candidate) => candidate.id === action.terminalId);
      if (!tab || tab.name === name) return state;
      return withLayout(state, conversationId, {
        ...layout,
        tabs: layout.tabs.map((candidate) => (
          candidate.id === action.terminalId ? { ...candidate, name } : candidate
        ))
      });
    }
    case "remove_conversation": {
      if (!Object.prototype.hasOwnProperty.call(state.byConversation, conversationId)) return state;
      const byConversation = { ...state.byConversation };
      delete byConversation[conversationId];
      return { ...state, byConversation };
    }
  }
}
