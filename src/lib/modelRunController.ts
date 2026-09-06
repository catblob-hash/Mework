import type { ModelRuns, ModelRunState } from "./modelStream";
import { browserAutomationToolForRun } from "./runContexts";
import { supportsVision as modelSupportsVision } from "./modelCapabilities";

/**
 * Coarse-grained run summary containing fields that remain stable or change
 * infrequently during a run. App subscribes to this instead of full ModelRuns,
 * so streaming text, reasoning, and tool-output deltas do not re-render the App tree every frame.
 */
export interface ModelRunSummary {
  requestId: string;
  providerName: string;
  modelName: string;
  workspaceId: string;
  /** Model profile used for the request; the steer guard uses it to allow images. */
  supportsVision: boolean;
  /** Queued message IDs steered into this request, in delivery order. */
  steeredMessageIds: readonly string[];
  /** Most recent tool driving browser automation, or null. */
  browserAutomationTool: string | null;
}

export type ModelRunSummaries = Partial<Record<string, ModelRunSummary>>;

export interface ModelRunController {
  /** Fine-grained subscription notified after every run-state commit, including streaming flushes. */
  subscribe(listener: () => void): () => void;
  current(): ModelRuns;
  /**
   * The sole run-state commit point. Publishes synchronously so `current()` is
   * immediately readable; does not notify subscribers when the updater returns the same reference.
   */
  update(updater: (current: ModelRuns) => ModelRuns): void;
  /** Coarse-grained subscription notified only when a conversation summary changes. */
  subscribeSummaries(listener: () => void): () => void;
  summaries(): ModelRunSummaries;
  /** Non-reactive run-ownership token used by stream callbacks and cleanup. */
  runToken(conversationId: string): string | undefined;
  setRunToken(conversationId: string, requestId: string): void;
  deleteRunToken(conversationId: string): void;
  hasRunToken(conversationId: string): boolean;
  /** Resolves only after this exact run generation has left both live stores. */
  waitForRunExit(conversationId: string, requestId: string): Promise<void>;
  /**
   * Immediately fold this conversation's uncommitted buffered stream events into run state.
   *
   * Events commit at 10 FPS, so an interrupted run must flush before persistence:
   * the final visible prose is the most likely data still buffered. Each run registers its flusher at startup.
   */
  flushPendingEvents(conversationId: string): void;
  setPendingEventFlusher(conversationId: string, flush: () => void): void;
  deletePendingEventFlusher(conversationId: string): void;
  /** Non-reactive prepare/perform guards for the send pipeline. */
  addPreparingRun(conversationId: string): void;
  deletePreparingRun(conversationId: string): void;
  hasPreparingRun(conversationId: string): boolean;
  addPerformingRun(conversationId: string): void;
  deletePerformingRun(conversationId: string): void;
  hasPerformingRun(conversationId: string): boolean;
}

function deriveSummary(run: ModelRunState): ModelRunSummary {
  return {
    requestId: run.requestId,
    providerName: run.providerName,
    modelName: run.modelName,
    workspaceId: run.workspaceId,
    supportsVision: run.request.model ? modelSupportsVision(run.request.model) : false,
    steeredMessageIds: Object.values(run.steeredInputsByRound).flat().map((message) => message.id),
    browserAutomationTool: browserAutomationToolForRun(run)
  };
}

function summaryEquals(previous: ModelRunSummary, next: ModelRunSummary): boolean {
  return previous.requestId === next.requestId
    && previous.providerName === next.providerName
    && previous.modelName === next.modelName
    && previous.workspaceId === next.workspaceId
    && previous.supportsVision === next.supportsVision
    && previous.browserAutomationTool === next.browserAutomationTool
    && previous.steeredMessageIds.length === next.steeredMessageIds.length
    && previous.steeredMessageIds.every((id, index) => next.steeredMessageIds[index] === id);
}

export function createModelRunController(): ModelRunController {
  let runs: ModelRuns = {};
  let summaries: ModelRunSummaries = {};
  const listeners = new Set<() => void>();
  const summaryListeners = new Set<() => void>();
  const runTokens = new Map<string, string>();
  const pendingEventFlushers = new Map<string, () => void>();
  const preparingRuns = new Set<string>();
  const performingRuns = new Set<string>();

  const notify = (targets: Set<() => void>) => {
    for (const listener of [...targets]) listener();
  };

  /** Recompute summaries with structural sharing, retaining unchanged entries and maps. */
  const recomputeSummaries = (): boolean => {
    let changed = false;
    const next: ModelRunSummaries = {};
    for (const [conversationId, run] of Object.entries(runs)) {
      if (!run) continue;
      const derived = deriveSummary(run);
      const previous = summaries[conversationId];
      if (previous && summaryEquals(previous, derived)) {
        next[conversationId] = previous;
      } else {
        next[conversationId] = derived;
        changed = true;
      }
    }
    if (!changed) {
      const previousIds = Object.keys(summaries);
      changed = previousIds.length !== Object.keys(next).length;
    }
    if (changed) summaries = next;
    return changed;
  };

  return {
    subscribe(listener) {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    current() {
      return runs;
    },
    update(updater) {
      const next = updater(runs);
      if (next === runs) return;
      runs = next;
      const summariesChanged = recomputeSummaries();
      notify(listeners);
      if (summariesChanged) notify(summaryListeners);
    },
    subscribeSummaries(listener) {
      summaryListeners.add(listener);
      return () => summaryListeners.delete(listener);
    },
    summaries() {
      return summaries;
    },
    runToken(conversationId) {
      return runTokens.get(conversationId);
    },
    setRunToken(conversationId, requestId) {
      runTokens.set(conversationId, requestId);
    },
    deleteRunToken(conversationId) {
      runTokens.delete(conversationId);
    },
    flushPendingEvents(conversationId) {
      pendingEventFlushers.get(conversationId)?.();
    },
    setPendingEventFlusher(conversationId, flush) {
      pendingEventFlushers.set(conversationId, flush);
    },
    deletePendingEventFlusher(conversationId) {
      pendingEventFlushers.delete(conversationId);
    },
    hasRunToken(conversationId) {
      return runTokens.has(conversationId);
    },
    waitForRunExit(conversationId, requestId) {
      const hasGeneration = () => (
        runs[conversationId]?.requestId === requestId
        || runTokens.get(conversationId) === requestId
      );
      if (!hasGeneration()) return Promise.resolve();
      return new Promise<void>((resolve) => {
        let unsubscribe: () => void = () => undefined;
        const settleIfExited = () => {
          if (hasGeneration()) return;
          unsubscribe();
          resolve();
        };
        listeners.add(settleIfExited);
        unsubscribe = () => {
          listeners.delete(settleIfExited);
        };
        // Keep the check beside subscription so a future asynchronous store
        // implementation cannot open a lost-wakeup window here.
        settleIfExited();
      });
    },
    addPreparingRun(conversationId) {
      preparingRuns.add(conversationId);
    },
    deletePreparingRun(conversationId) {
      preparingRuns.delete(conversationId);
    },
    hasPreparingRun(conversationId) {
      return preparingRuns.has(conversationId);
    },
    addPerformingRun(conversationId) {
      performingRuns.add(conversationId);
    },
    deletePerformingRun(conversationId) {
      performingRuns.delete(conversationId);
    },
    hasPerformingRun(conversationId) {
      return performingRuns.has(conversationId);
    }
  };
}
