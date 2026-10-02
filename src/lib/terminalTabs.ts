import { arrangeByIds } from "./reorder";
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
 *
 * Beside the shells the strip can hold one read-only page: the output of a command the model ran,
 * opened from its task row. There is never more than one — opening another command's output
 * points that page at it — and it sits at the strip's start, ahead of every shell, whatever order
 * they are dragged into. It has no process of its own, so closing it ends nothing.
 */

/**
 * The read-only page's tab id. There is one page at most, so it needs no number, and no shell's
 * `terminal-{ordinal}` id can take this shape.
 */
export const READ_ONLY_TERMINAL_TAB_ID = "read-only";

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
  /** The shells, in strip order after the read-only page. */
  tabs: TerminalTab[];
  /** The shell task the read-only page shows, or null when the strip has no read-only page. */
  readOnly: string | null;
  /** A shell's id, `READ_ONLY_TERMINAL_TAB_ID`, or null with nothing in the strip. */
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
  /** Points the read-only page at a shell task, opening the page if there is none, and selects it. */
  | { type: "show_read_only"; conversationId: string; shellTaskId: string }
  /** Closes the read-only page if it shows this shell task: the host let go of its output. */
  | { type: "forget_read_only"; conversationId: string; shellTaskId: string }
  | { type: "close"; conversationId: string; terminalId: string }
  | { type: "activate"; conversationId: string; terminalId: string }
  | { type: "rename"; conversationId: string; terminalId: string; name: string }
  /**
   * Puts the tabs in the order the strip was dragged into. An id the conversation does not hold
   * is ignored, and a tab the request leaves out keeps its relative place after the rest.
   */
  | { type: "reorder"; conversationId: string; terminalIds: string[] }
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
  readOnly: null,
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

/** Every tab id the strip draws, in strip order: the read-only page first, then the shells. */
export function terminalStripIds(layout: TerminalTabsLayout): string[] {
  const shells = layout.tabs.map((tab) => tab.id);
  return layout.readOnly === null ? shells : [READ_ONLY_TERMINAL_TAB_ID, ...shells];
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
    ...layout,
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
      // The read-only page counts: a pane showing it already has something to show.
      return terminalStripIds(layout).length > 0
        ? state
        : withLayout(state, conversationId, added(layout, action.launch ?? null));
    case "add":
      return withLayout(state, conversationId, added(layout, action.launch ?? null));
    case "show_read_only":
      return layout.readOnly === action.shellTaskId && layout.activeId === READ_ONLY_TERMINAL_TAB_ID
        ? state
        : withLayout(state, conversationId, {
          ...layout,
          readOnly: action.shellTaskId,
          activeId: READ_ONLY_TERMINAL_TAB_ID
        });
    case "forget_read_only":
      return layout.readOnly !== action.shellTaskId
        ? state
        : terminalTabsReducer(state, {
          type: "close",
          conversationId,
          terminalId: READ_ONLY_TERMINAL_TAB_ID
        });
    case "close": {
      const strip = terminalStripIds(layout);
      const index = strip.indexOf(action.terminalId);
      if (index < 0) return state;
      const rest = strip.filter((id) => id !== action.terminalId);
      // Focus falls to the right, as a browser's does, and to the left off the end.
      const activeId = layout.activeId !== action.terminalId ? layout.activeId
        : rest.length === 0 ? null : rest[Math.min(index, rest.length - 1)];
      return withLayout(state, conversationId, action.terminalId === READ_ONLY_TERMINAL_TAB_ID
        ? { ...layout, readOnly: null, activeId }
        : { ...layout, tabs: layout.tabs.filter((tab) => tab.id !== action.terminalId), activeId });
    }
    case "activate":
      return layout.activeId === action.terminalId
        || !terminalStripIds(layout).includes(action.terminalId) ? state
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
    case "reorder": {
      // Only the shells move: the read-only page is not among them, so its id is ignored and it
      // keeps the strip's start.
      const tabs = arrangeByIds(layout.tabs, action.terminalIds, (tab) => tab.id);
      return tabs === null ? state : withLayout(state, conversationId, { ...layout, tabs });
    }
    case "remove_conversation": {
      if (!Object.prototype.hasOwnProperty.call(state.byConversation, conversationId)) return state;
      const byConversation = { ...state.byConversation };
      delete byConversation[conversationId];
      return { ...state, byConversation };
    }
  }
}
