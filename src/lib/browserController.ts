import { issueBrowserLifecycleIntentEpoch } from "./browserLifecycleIntent";
import type { BrowserStatus } from "./browser";

export type BrowserIntentDesired = "open" | "hidden" | "closed";

export interface BrowserIntent {
  epoch: number;
  desired: BrowserIntentDesired;
}

export interface BrowserControllerState {
  /** Native browser status per session id: one entry per open browser tab. */
  statuses: Record<string, BrowserStatus>;
  /** Whether the presented native surface finished opening. */
  runtimeReady: boolean;
}

export interface BrowserController {
  subscribe(listener: () => void): () => void;
  current(): BrowserControllerState;
  setRuntimeReady(ready: boolean): void;
  updateStatuses(
    updater: (current: Record<string, BrowserStatus>) => Record<string, BrowserStatus>
  ): void;
  /**
   * Session whose native surface is currently presented in the sidebar.
   * Deliberately non-reactive: it is consumed only inside async guards, and a
   * render must never depend on it.
   */
  visibleSession(): string | null;
  setVisibleSession(sessionId: string | null): void;
  /** Clears the presented session only if it is the given one. */
  clearVisibleSessionIf(sessionId: string): void;
  /** Clears and returns the presented session in one step (hide flow). */
  takeVisibleSession(): string | null;
  /**
   * Intent epoch fence: every native lifecycle transition claims a fresh epoch
   * and each async continuation re-checks that its epoch is still the newest
   * one for the session before committing UI state.
   */
  issueIntent(sessionId: string, desired: BrowserIntentDesired): number;
  intentIsCurrent(sessionId: string, epoch: number, desired: BrowserIntentDesired): boolean;
  currentIntent(sessionId: string): BrowserIntent | undefined;
  /** Rolls back to the pre-close intent after a rejected close disposition. */
  restoreIntent(sessionId: string, previous: BrowserIntent | undefined): void;
  closeInFlight(sessionId: string): boolean;
  /**
   * Deduplicates concurrent closes per session. `create` runs synchronously
   * when this call starts the close, so its intent claim happens before any
   * racing open can be issued; later callers share the first task untouched.
   */
  dedupClose(sessionId: string, create: () => Promise<void>): Promise<void>;
  /** Tracks an open call so a structured close can fence stragglers. */
  trackOpen(sessionId: string, open: Promise<unknown>): void;
  pendingOpens(sessionId: string): Promise<void>[];
}

export function createBrowserController(): BrowserController {
  let state: BrowserControllerState = {
    statuses: {},
    runtimeReady: false
  };
  const listeners = new Set<() => void>();
  let visibleSessionId: string | null = null;
  const intents = new Map<string, BrowserIntent>();
  const closePromises = new Map<string, Promise<void>>();
  const openPromises = new Map<string, Set<Promise<void>>>();

  const notify = () => {
    for (const listener of [...listeners]) listener();
  };

  return {
    subscribe(listener) {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    current() {
      return state;
    },
    setRuntimeReady(ready) {
      if (state.runtimeReady === ready) return;
      state = { ...state, runtimeReady: ready };
      notify();
    },
    updateStatuses(updater) {
      const next = updater(state.statuses);
      if (next === state.statuses) return;
      state = { ...state, statuses: next };
      notify();
    },
    visibleSession() {
      return visibleSessionId;
    },
    setVisibleSession(sessionId) {
      visibleSessionId = sessionId;
    },
    clearVisibleSessionIf(sessionId) {
      if (visibleSessionId === sessionId) visibleSessionId = null;
    },
    takeVisibleSession() {
      const sessionId = visibleSessionId;
      visibleSessionId = null;
      return sessionId;
    },
    issueIntent(sessionId, desired) {
      const epoch = issueBrowserLifecycleIntentEpoch();
      intents.set(sessionId, { epoch, desired });
      return epoch;
    },
    intentIsCurrent(sessionId, epoch, desired) {
      const current = intents.get(sessionId);
      return current?.epoch === epoch && current.desired === desired;
    },
    currentIntent(sessionId) {
      return intents.get(sessionId);
    },
    restoreIntent(sessionId, previous) {
      if (previous) {
        intents.set(sessionId, previous);
      } else {
        intents.delete(sessionId);
      }
    },
    closeInFlight(sessionId) {
      return closePromises.has(sessionId);
    },
    dedupClose(sessionId, create) {
      const existing = closePromises.get(sessionId);
      if (existing) return existing;
      let closeTask!: Promise<void>;
      closeTask = create().finally(() => {
        if (closePromises.get(sessionId) === closeTask) {
          closePromises.delete(sessionId);
        }
      });
      closePromises.set(sessionId, closeTask);
      return closeTask;
    },
    trackOpen(sessionId, open) {
      const settled = open.then(() => undefined, () => undefined);
      const tracked = openPromises.get(sessionId) ?? new Set<Promise<void>>();
      tracked.add(settled);
      openPromises.set(sessionId, tracked);
      void settled.finally(() => {
        const current = openPromises.get(sessionId);
        current?.delete(settled);
        if (current?.size === 0) openPromises.delete(sessionId);
      });
    },
    pendingOpens(sessionId) {
      return [...(openPromises.get(sessionId) ?? [])];
    }
  };
}
