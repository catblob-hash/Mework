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
 *
 * A conversation has no terminal until one is asked for: the first is made when the pane opens
 * with nothing in it, in whatever shell the caller chose for it.
 */

export interface TerminalTab {
  /** The host's terminal id, unique within the conversation and never reused. */
  id: string;
  /**
   * Creation order within the conversation. Ids are minted from it and never reused, so a
   * torn-down shell's address can never come back. The derived name's number is not this:
   * that is `number`, counted per shell.
   */
  ordinal: number;
  /**
   * The number the derived name carries: how many terminals of this tab's shell the
   * conversation had opened when this one was, itself included. Each shell counts on its own and
   * never counts down, so `zsh 2` stays `zsh 2` whatever else opens or closes.
   */
  number: number;
  /** The user's name for this terminal; null follows the derived one. */
  name: string | null;
  /**
   * The workspace and shell this tab's terminal was asked for. `null` leaves both
   * to the host — workspace 1 and its machine's default shell.
   */
  launch: TerminalLaunchChoice | null;
}

export interface TerminalTabsLayout {
  tabs: TerminalTab[];
  activeId: string | null;
  /** Monotonic: an ordinal is spent when its tab is created and never minted again. */
  nextOrdinal: number;
  /** The number each shell's next tab takes, by `terminalShellKey`; absent is 1. */
  nextNumbers: Readonly<Record<string, number>>;
}

export interface TerminalTabsState {
  byConversation: Record<string, TerminalTabsLayout>;
}

export type TerminalTabsAction =
  /**
   * Gives the conversation a tab, started with `launch`, if it has none; the way opening the
   * pane finds something to show.
   */
  | { type: "ensure"; conversationId: string; launch?: TerminalLaunchChoice | null }
  | { type: "add"; conversationId: string; launch?: TerminalLaunchChoice | null }
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

/** What a conversation starts with: no terminal until one is asked for. */
const EMPTY_LAYOUT: TerminalTabsLayout = {
  tabs: [],
  activeId: null,
  nextOrdinal: 1,
  nextNumbers: {}
};
Object.freeze(EMPTY_LAYOUT.tabs);
Object.freeze(EMPTY_LAYOUT.nextNumbers);
Object.freeze(EMPTY_LAYOUT);

export function terminalTabsFor(
  state: TerminalTabsState,
  conversationId: string | null
): TerminalTabsLayout {
  if (!conversationId) return EMPTY_LAYOUT;
  return Object.prototype.hasOwnProperty.call(state.byConversation, conversationId)
    ? state.byConversation[conversationId] : EMPTY_LAYOUT;
}

/**
 * Which count a tab's number is drawn from: its shell's, or — for a tab that left the shell to
 * the host — the count of those.
 */
export function terminalShellKey(launch: TerminalLaunchChoice | null | undefined): string {
  return launch?.shell ?? "";
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
  const key = terminalShellKey(launch);
  const number = layout.nextNumbers[key] ?? 1;
  return {
    tabs: [...layout.tabs, { id, ordinal, number, name: null, launch }],
    activeId: id,
    nextOrdinal: ordinal + 1,
    nextNumbers: { ...layout.nextNumbers, [key]: number + 1 }
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
      return layout.tabs.length > 0
        ? state
        : withLayout(state, conversationId, added(layout, action.launch ?? null));
    case "add":
      return withLayout(state, conversationId, added(layout, action.launch ?? null));
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
