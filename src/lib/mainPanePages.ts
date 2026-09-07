import type { GitReviewView } from "./git";

/**
 * What the main pane is showing for one conversation.
 *
 * There is exactly one of these per conversation, and it replaces both the old
 * right-sidebar tab container and the separate "selected subagent" flag: the
 * question "what is the message area showing" now has a single answer instead
 * of two that had to be kept from contradicting each other.
 */
export type MainPaneView =
  | { kind: "conversation" }
  | { kind: "subagent"; subagentId: string }
  | { kind: "preview"; sessionId: string }
  | { kind: "shell"; shellTaskId: string }
  | { kind: "review"; view: GitReviewView }
  /** The conversation's plan document. One plan per conversation, so no payload. */
  | { kind: "plan" };

export type MainPaneViewKind = MainPaneView["kind"];

export const CONVERSATION_VIEW: MainPaneView = { kind: "conversation" };

export interface MainPaneState {
  viewByConversation: Record<string, MainPaneView>;
  /**
   * Live preview sessions per conversation, in the order they appeared. This is
   * the roster the tab strip used to carry: the native browser sessions a
   * conversation owns, including the ones an Agent opened and the user has not
   * looked at yet. A conversation's first session reuses the conversation id.
   */
  previewSessions: Record<string, string[]>;
}

export type MainPaneAction =
  | { type: "show"; conversationId: string; view: MainPaneView }
  | { type: "back"; conversationId: string }
  /**
   * Adds a preview session without stealing the main pane — an Agent-opened tab
   * becomes a task row the user can navigate to, and nothing more.
   */
  | { type: "register_preview"; conversationId: string; sessionId: string }
  | { type: "forget_preview"; conversationId: string; sessionId: string }
  | { type: "remove_conversation"; conversationId: string };

export const initialMainPaneState: MainPaneState = {
  viewByConversation: {},
  previewSessions: {}
};

export function mainPaneViewFor(
  state: MainPaneState,
  conversationId: string | null
): MainPaneView {
  if (!conversationId) return CONVERSATION_VIEW;
  return state.viewByConversation[conversationId] ?? CONVERSATION_VIEW;
}

/**
 * Shared empty roster. A fresh `[]` per call would give every render a new identity, which
 * cascades through the memos that derive task rows and the browser polling key.
 */
const NO_PREVIEW_SESSIONS: readonly string[] = Object.freeze([]);

export function previewSessionsFor(
  state: MainPaneState,
  conversationId: string | null
): readonly string[] {
  if (!conversationId) return NO_PREVIEW_SESSIONS;
  return state.previewSessions[conversationId] ?? NO_PREVIEW_SESSIONS;
}

/** True while the conversation timeline is hidden behind one of the task pages. */
export function mainPaneShowsPage(view: MainPaneView): boolean {
  return view.kind !== "conversation";
}

export function sameMainPaneView(left: MainPaneView, right: MainPaneView): boolean {
  if (left.kind !== right.kind) return false;
  if (left.kind === "subagent" && right.kind === "subagent") {
    return left.subagentId === right.subagentId;
  }
  if (left.kind === "preview" && right.kind === "preview") {
    return left.sessionId === right.sessionId;
  }
  if (left.kind === "shell" && right.kind === "shell") {
    return left.shellTaskId === right.shellTaskId;
  }
  if (left.kind === "review" && right.kind === "review") return left.view === right.view;
  return true;
}

/**
 * Native session id for an extra preview tab. The `#` separator is reserved:
 * conversation ids never contain one, so the native side can recover the owning
 * conversation — and therefore the shared Chromium profile — from any tab id.
 */
export function previewTabSessionId(conversationId: string, token: string): string {
  return `${conversationId}#${token}`;
}

export function previewSessionBelongsToConversation(
  sessionId: string,
  conversationId: string
): boolean {
  return sessionId === conversationId || sessionId.startsWith(`${conversationId}#`);
}

/**
 * The session an Agent's browser actions drive until it calls playwright
 * tab_select, and the one the trusted interface owns outright: automation
 * indicators and close guards belong to it.
 */
export function isPrimaryPreviewSession(sessionId: string, conversationId: string): boolean {
  return sessionId === conversationId;
}

/** Stable DOM id shared by a page and whatever labels it. */
export function mainPanePageDomId(key: string): string {
  const encoded = Array.from(key, (character) => (
    character.codePointAt(0)?.toString(36) ?? "0"
  )).join("-");
  return `main-pane-page-${encoded}`;
}

/** The key a mounted page is addressed by; one page per key stays mounted. */
export function mainPaneViewKey(view: MainPaneView): string {
  if (view.kind === "preview") return `preview:${view.sessionId}`;
  if (view.kind === "shell") return `shell:${view.shellTaskId}`;
  if (view.kind === "subagent") return `subagent:${view.subagentId}`;
  if (view.kind === "review") return "review";
  if (view.kind === "plan") return "plan";
  return "conversation";
}

function withView(
  state: MainPaneState,
  conversationId: string,
  view: MainPaneView
): MainPaneState {
  const current = state.viewByConversation[conversationId];
  if (current && sameMainPaneView(current, view)) return state;
  return {
    ...state,
    viewByConversation: { ...state.viewByConversation, [conversationId]: view }
  };
}

export function mainPaneReducer(
  state: MainPaneState,
  action: MainPaneAction
): MainPaneState {
  switch (action.type) {
    case "show": {
      // Showing a preview implies the session exists: an Agent tab the user
      // reached from the task bar must not have to be registered twice.
      const next = action.view.kind === "preview"
        ? mainPaneReducer(state, {
          type: "register_preview",
          conversationId: action.conversationId,
          sessionId: action.view.sessionId
        })
        : state;
      return withView(next, action.conversationId, action.view);
    }
    case "back": {
      if (!state.viewByConversation[action.conversationId]) return state;
      const viewByConversation = { ...state.viewByConversation };
      delete viewByConversation[action.conversationId];
      return { ...state, viewByConversation };
    }
    case "register_preview": {
      const sessions = state.previewSessions[action.conversationId] ?? [];
      if (sessions.includes(action.sessionId)) return state;
      return {
        ...state,
        previewSessions: {
          ...state.previewSessions,
          [action.conversationId]: [...sessions, action.sessionId]
        }
      };
    }
    case "forget_preview": {
      const sessions = state.previewSessions[action.conversationId] ?? [];
      const remaining = sessions.filter((sessionId) => sessionId !== action.sessionId);
      const known = remaining.length !== sessions.length;
      const current = state.viewByConversation[action.conversationId];
      // A closed session cannot stay on screen. There is no tab strip left to
      // fall back along, so the way out is the conversation itself.
      const strandedView = current?.kind === "preview" && current.sessionId === action.sessionId;
      if (!known && !strandedView) return state;
      const previewSessions = { ...state.previewSessions };
      if (remaining.length) previewSessions[action.conversationId] = remaining;
      else delete previewSessions[action.conversationId];
      const viewByConversation = { ...state.viewByConversation };
      if (strandedView) delete viewByConversation[action.conversationId];
      return { previewSessions, viewByConversation };
    }
    case "remove_conversation": {
      const hasView = action.conversationId in state.viewByConversation;
      const hasSessions = action.conversationId in state.previewSessions;
      if (!hasView && !hasSessions) return state;
      const viewByConversation = { ...state.viewByConversation };
      const previewSessions = { ...state.previewSessions };
      delete viewByConversation[action.conversationId];
      delete previewSessions[action.conversationId];
      return { viewByConversation, previewSessions };
    }
  }
}
