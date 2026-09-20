export type SidePaneKind = "terminal" | "review" | "preview" | "files" | "tasks" | "plan" | "history" | "settings" | "subagent" | "shell";
export type SidePaneId =
  | "terminal" | "review" | "files" | "tasks" | "plan" | "history" | "settings"
  | `preview:${string}` | `subagent:${string}` | `shell:${string}` | `history:${string}`;

export interface SidePaneLayout {
  panes: SidePaneId[];
  sideFlex: number;
  paneFlex: Record<string, number>;
  focused: SidePaneId | null;
  /**
   * The pane shown alone over the whole workspace, chat column included. Never persisted, and
   * dropped by anything that would leave the user staring at a pane they did not ask for.
   */
  expanded: SidePaneId | null;
}

export interface SidePanesState {
  layoutByConversation: Record<string, SidePaneLayout>;
  previewSessions: Record<string, string[]>;
  lastSideFlexByKind: Partial<Record<SidePaneKind, number>>;
}

export type SidePanesAction =
  | { type: "open"; conversationId: string; pane: SidePaneId }
  | { type: "toggle"; conversationId: string; pane: SidePaneId }
  | { type: "close"; conversationId: string; pane: SidePaneId }
  | { type: "close_last"; conversationId: string }
  | { type: "focus"; conversationId: string; pane: SidePaneId }
  | { type: "toggle_expand"; conversationId: string; pane: SidePaneId }
  | { type: "set_side_flex"; conversationId: string; sideFlex: number }
  | { type: "set_pane_flex"; conversationId: string; paneFlex: Record<string, number> }
  | { type: "register_preview"; conversationId: string; sessionId: string }
  | { type: "forget_preview"; conversationId: string; sessionId: string }
  /**
   * Moves the layout a draft accumulated under its placeholder id onto the real
   * conversation it just became, the way the composer's text and attachments move.
   * Without it a pane opened in a draft is dropped on send and left behind to
   * reappear, unasked, in the next draft.
   */
  | { type: "adopt_conversation"; conversationId: string; from: string }
  | { type: "remove_conversation"; conversationId: string };

export const CHAT_TILE_FLEX = 2;
export const MIN_SIDE_FLEX = 0.25;
export const MAX_SIDE_FLEX = 8;
export const initialSidePanesState: SidePanesState = {
  layoutByConversation: {},
  previewSessions: {},
  lastSideFlexByKind: {}
};

const EMPTY_LAYOUT: SidePaneLayout = { panes: [], sideFlex: 1, paneFlex: {}, focused: null, expanded: null };
Object.freeze(EMPTY_LAYOUT.panes);
Object.freeze(EMPTY_LAYOUT.paneFlex);
Object.freeze(EMPTY_LAYOUT);
const NO_PREVIEW_SESSIONS: readonly string[] = Object.freeze([]);
const STORAGE_KEY = "mework.sidePanes.sideFlexByKind";
const KINDS: SidePaneKind[] = ["terminal", "review", "preview", "files", "tasks", "plan", "history", "settings", "subagent", "shell"];

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

export function paneKind(id: SidePaneId): SidePaneKind {
  return id.split(":", 1)[0] as SidePaneKind;
}

export function paneTarget(id: SidePaneId): string | null {
  const colon = id.indexOf(":");
  return colon < 0 ? null : id.slice(colon + 1);
}

export function previewPaneId(sessionId: string): SidePaneId { return `preview:${sessionId}`; }
export function subagentPaneId(id: string): SidePaneId { return `subagent:${id}`; }
export function shellPaneId(id: string): SidePaneId { return `shell:${id}`; }
/**
 * One agent's request ledger. Targeted rather than bare because a conversation
 * holds one ledger per agent it spawned plus its own, and the bare `history`
 * pane is the conversation's: the two open side by side.
 */
export function subagentHistoryPaneId(id: string): SidePaneId { return `history:${id}`; }
export function paneIsOpen(layout: SidePaneLayout, id: SidePaneId): boolean { return layout.panes.includes(id); }

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
      const next = kind === "preview" ? sidePanesReducer(state, {
        type: "register_preview", conversationId, sessionId: paneTarget(action.pane)!
      }) : state;
      if (paneIsOpen(layout, action.pane)) {
        // Asking for a pane that some other pane is currently covering means asking to see it.
        const expanded = layout.expanded === action.pane ? layout.expanded : null;
        return layout.focused === action.pane && layout.expanded === expanded ? next
          : withLayout(next, conversationId, { ...layout, focused: action.pane, expanded });
      }
      const previous = kind === "preview" ? layout.panes.find((id) => paneKind(id) === "preview") : undefined;
      const panes = previous === undefined ? [...layout.panes, action.pane]
        : layout.panes.map((id) => id === previous ? action.pane : id);
      let paneFlex = layout.paneFlex;
      if (previous !== undefined && Object.prototype.hasOwnProperty.call(paneFlex, previous)) {
        paneFlex = { ...paneFlex, [action.pane]: paneFlex[previous] };
        delete paneFlex[previous];
      }
      return withLayout(next, conversationId, {
        panes, paneFlex, focused: action.pane, expanded: null,
        sideFlex: layout.panes.length ? layout.sideFlex
          : state.lastSideFlexByKind[kind] ?? defaultSideFlexForKind(kind)
      });
    }
    case "toggle":
      return sidePanesReducer(state, { ...action, type: paneIsOpen(layout, action.pane) ? "close" : "open" });
    case "close": {
      if (!paneIsOpen(layout, action.pane)) return state;
      const panes = layout.panes.filter((id) => id !== action.pane);
      const paneFlex = { ...layout.paneFlex };
      delete paneFlex[action.pane];
      const remaining = { ...layout, panes, paneFlex };
      return withLayout(state, conversationId, { ...remaining, focused: focusedPane(remaining) });
    }
    case "close_last": {
      const pane = focusedPane(layout);
      return pane === null ? state : sidePanesReducer(state, { type: "close", conversationId, pane });
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
      const kind = layout.panes.length ? paneKind(layout.panes[0]) : null;
      const next = kind !== null && state.lastSideFlexByKind[kind] !== sideFlex
        ? { ...state, lastSideFlexByKind: { ...state.lastSideFlexByKind, [kind]: sideFlex } } : state;
      return layout.sideFlex === sideFlex ? next : withLayout(next, conversationId, { ...layout, sideFlex });
    }
    case "set_pane_flex": {
      const paneFlex = Object.fromEntries(Object.entries(action.paneFlex).map(([id, value]) => [id, clampFlex(value)]));
      if (Object.keys(paneFlex).length === Object.keys(layout.paneFlex).length
        && Object.keys(paneFlex).every((id) => paneFlex[id] === layout.paneFlex[id])) return state;
      return withLayout(state, conversationId, { ...layout, paneFlex });
    }
    case "register_preview": {
      const sessions = previewSessionsFor(state, conversationId);
      if (sessions.includes(action.sessionId)) return state;
      return { ...state, previewSessions: { ...state.previewSessions, [conversationId]: [...sessions, action.sessionId] } };
    }
    case "forget_preview": {
      const next = sidePanesReducer(state, { type: "close", conversationId, pane: previewPaneId(action.sessionId) });
      const sessions = previewSessionsFor(state, conversationId);
      if (!sessions.includes(action.sessionId)) return next;
      const remaining = sessions.filter((sessionId) => sessionId !== action.sessionId);
      const previewSessions = { ...state.previewSessions };
      if (remaining.length) previewSessions[conversationId] = remaining;
      else delete previewSessions[conversationId];
      return { ...next, previewSessions };
    }
    case "adopt_conversation": {
      const adopted = Object.prototype.hasOwnProperty.call(state.layoutByConversation, action.from)
        ? state.layoutByConversation[action.from] : null;
      if (adopted === null || action.from === conversationId) return state;
      const layoutByConversation = { ...state.layoutByConversation };
      delete layoutByConversation[action.from];
      // A draft never reaches a workspace, so it owns no native page. Dropping any preview
      // pane anyway keeps a session id minted under the placeholder id from following it here.
      const panes = adopted.panes.filter((pane) => paneKind(pane) !== "preview");
      const carried = { ...adopted, panes, expanded: null };
      layoutByConversation[conversationId] = { ...carried, focused: focusedPane(carried) };
      return { ...state, layoutByConversation };
    }
    case "remove_conversation": {
      if (!Object.prototype.hasOwnProperty.call(state.layoutByConversation, conversationId)
        && !Object.prototype.hasOwnProperty.call(state.previewSessions, conversationId)) return state;
      const layoutByConversation = { ...state.layoutByConversation };
      const previewSessions = { ...state.previewSessions };
      delete layoutByConversation[conversationId];
      delete previewSessions[conversationId];
      return { ...state, layoutByConversation, previewSessions };
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
