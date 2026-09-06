import { closeTerminal } from "./terminal";
import type { TerminalSessionState } from "./terminal";

export function terminalSessionIsEmpty(state: TerminalSessionState | undefined): boolean {
  return !state || (!state.hasHistory && !state.busy);
}

/**
 * The store key of one terminal. A terminal id is only unique within its
 * conversation — every composer drawer uses the same one — so the pair is the
 * identity, as it is for the host.
 */
export function terminalSessionKey(conversationId: string, terminalId: string): string {
  return `${conversationId}/${terminalId}`;
}

export interface TerminalCloseHandlers {
  /** Runs once per close task, after the session is removed from the store. */
  onClosed?: () => void;
  /** Runs once per close task; the returned promise still rejects afterwards. */
  onError?: (error: unknown) => void;
}

export interface TerminalController {
  subscribe(listener: () => void): () => void;
  /** Sessions by `terminalSessionKey`. */
  current(): Record<string, TerminalSessionState>;
  /**
   * Upserts a live session; skips notifying when every field is unchanged. A
   * report that the session is not live — idle, exited, failed — removes it
   * instead: the store only holds terminals the renderer is observing, so a
   * busy flag can never outlive the panel that could clear it. The host is the
   * authority on the rest (its command lease still blocks Git writes).
   */
  update(next: TerminalSessionState): void;
  register(session: TerminalSessionState): void;
  /** Marks the session busy with history; returns false for unknown sessions. */
  markCommandStarted(conversationId: string, terminalId: string): boolean;
  /**
   * Closes the backend terminal and removes the session. Concurrent calls for
   * the same terminal share one task: only the caller that created the task
   * gets its handlers run, matching the original first-caller-wins semantics.
   */
  requestClose(
    conversationId: string,
    terminalId: string,
    handlers?: TerminalCloseHandlers
  ): Promise<void>;
}

export function terminalSessionIsLive(state: TerminalSessionState): boolean {
  return state.phase === "connecting" || state.phase === "running" || state.phase === "closing";
}

function sessionsEqual(a: TerminalSessionState, b: TerminalSessionState): boolean {
  return a.terminalId === b.terminalId
    && a.conversationId === b.conversationId
    && a.label === b.label
    && a.phase === b.phase
    && a.busy === b.busy
    && a.hasHistory === b.hasHistory
    && a.cwd === b.cwd
    && a.shell === b.shell
    && a.sessionId === b.sessionId;
}

export function createTerminalController(): TerminalController {
  let sessions: Record<string, TerminalSessionState> = {};
  const listeners = new Set<() => void>();
  const closePromises = new Map<string, Promise<void>>();

  const notify = () => {
    for (const listener of [...listeners]) listener();
  };

  const removeSession = (key: string) => {
    if (!sessions[key]) return;
    const next = { ...sessions };
    delete next[key];
    sessions = next;
    notify();
  };

  return {
    subscribe(listener) {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    current() {
      return sessions;
    },
    update(next) {
      const key = terminalSessionKey(next.conversationId, next.terminalId);
      if (!terminalSessionIsLive(next)) {
        removeSession(key);
        return;
      }
      const previous = sessions[key];
      if (previous && sessionsEqual(previous, next)) return;
      sessions = { ...sessions, [key]: next };
      notify();
    },
    register(session) {
      sessions = { ...sessions, [terminalSessionKey(session.conversationId, session.terminalId)]: session };
      notify();
    },
    markCommandStarted(conversationId, terminalId) {
      const key = terminalSessionKey(conversationId, terminalId);
      const session = sessions[key];
      if (!session) return false;
      sessions = {
        ...sessions,
        [key]: { ...session, busy: true, hasHistory: true }
      };
      notify();
      return true;
    },
    requestClose(conversationId, terminalId, handlers) {
      const key = terminalSessionKey(conversationId, terminalId);
      const existing = closePromises.get(key);
      if (existing) return existing;
      let closeTask!: Promise<void>;
      closeTask = Promise.resolve()
        .then(() => closeTerminal(conversationId, terminalId))
        .then(() => {
          removeSession(key);
          handlers?.onClosed?.();
        })
        .catch((error) => {
          handlers?.onError?.(error);
          throw error;
        })
        .finally(() => {
          if (closePromises.get(key) === closeTask) {
            closePromises.delete(key);
          }
        });
      closePromises.set(key, closeTask);
      return closeTask;
    }
  };
}
