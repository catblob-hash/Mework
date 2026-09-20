/**
 * Single authoritative store for the application document.
 *
 * It unifies the renderer snapshot, the latest published snapshot read by
 * asynchronous code, and the save pipeline: 420 ms debounce, serialized disk
 * writes, revision status guards, and immediate persistence.
 *
 * `current()` is the synchronous authoritative value. It is current when
 * `update` or `publish` returns, and asynchronous continuations must reread it.
 * React subscribes to that same value with `useSyncExternalStore`; rendering is
 * only a lagging projection. The save pipeline owns durability, bounded by the
 * promises returned from `flush` and `publish`.
 */
import { saveDocument } from "./runtime";
import type { AppDocument } from "../types";

export type DocumentSaveStatus = "idle" | "saving" | "saved" | "error";

export interface DocumentSaveFailure {
  /** `debounced` failures are background saves with no awaiting caller;
   * `publish` and `flush` failures are also thrown to their callers. */
  phase: "debounced" | "publish" | "flush";
  error: unknown;
}

interface DocumentStoreOptions {
  /** Injection point for replacing the disk writer in tests. */
  save?: (snapshot: AppDocument, options: { immutableSnapshot: true; durable?: boolean }) => Promise<unknown>;
  debounceMs?: number;
}

export interface DocumentStore {
  subscribe(listener: () => void): () => void;
  getSnapshot(): AppDocument | null;
  /** Synchronous authoritative snapshot. After `await`, asynchronous flows must
   * reread it instead of using a rendered snapshot captured in a closure. */
  current(): AppDocument | null;

  subscribeSaveStatus(listener: () => void): () => void;
  getSaveStatus(): DocumentSaveStatus;
  /** Notification channel for background-save failures, whose callers have no
   * promise to catch. */
  subscribeSaveFailures(listener: (failure: DocumentSaveFailure) => void): () => void;

  /** Publishes the initial document without scheduling a save; changes applied
   * before loading likewise schedule no save. */
  load(document: AppDocument): void;
  /** Applies and synchronously publishes a change, then schedules a debounced
   * background save. The updater receives the authoritative current value. */
  update(updater: (current: AppDocument | null) => AppDocument | null): void;
  /** Immediately persists a new snapshot after synchronously publishing it so
   * later changes build on it. Retain optimistic state and throw on failure. */
  publish(next: AppDocument, options?: { durable?: boolean }): Promise<void>;
  /** Immediately persists the current snapshot, canceling pending debounce. A
   * missing document is a no-op. */
  flush(options?: { durable?: boolean }): Promise<void>;
  /** Bridge for backend save results: failures immediately set error; successes
   * only restore error to saved without interrupting saving. */
  reportBackendSaveResult(result: "failure" | "success"): void;
  /** Called when the owner unmounts. Cancel pending debounce and stop scheduling
   * new background saves; in-flight `publish` and `flush` calls are unaffected. */
  dispose(): void;
  /** Re-enables scheduling when the owner remounts. This pairs with `dispose`:
   * React StrictMode may clean up a shared store before remounting it, and resume
   * schedules a save for changes made while scheduling was disabled. */
  resume(): void;
}

export function createDocumentStore(options: DocumentStoreOptions = {}): DocumentStore {
  const save = options.save ?? saveDocument;
  const debounceMs = options.debounceMs ?? 420;

  let document: AppDocument | null = null;
  let loaded = false;
  let disposed = false;
  let saveStatus: DocumentSaveStatus = "idle";
  let saveRevision = 0;
  let debounceTimer: ReturnType<typeof setTimeout> | null = null;
  /** Serialized disk-write queue. It swallows earlier failures while preserving
   * write order. */
  let saveQueue: Promise<unknown> = Promise.resolve();

  const documentListeners = new Set<() => void>();
  const statusListeners = new Set<() => void>();
  const failureListeners = new Set<(failure: DocumentSaveFailure) => void>();

  const notifyDocument = () => {
    for (const listener of [...documentListeners]) listener();
  };
  const setStatus = (next: DocumentSaveStatus) => {
    if (saveStatus === next) return;
    saveStatus = next;
    for (const listener of [...statusListeners]) listener();
  };
  const reportFailure = (failure: DocumentSaveFailure) => {
    for (const listener of [...failureListeners]) listener(failure);
  };

  const cancelDebounce = () => {
    if (debounceTimer === null) return;
    clearTimeout(debounceTimer);
    debounceTimer = null;
  };

  const enqueueSave = (snapshot: AppDocument, saveOptions: { durable?: boolean } = {}) => {
    const queued = saveQueue
      .catch(() => undefined)
      .then(() =>
        save(snapshot, {
          immutableSnapshot: true,
          ...(saveOptions.durable ? { durable: true } : {})
        })
      );
    saveQueue = queued.catch(() => undefined);
    return queued;
  };

  const scheduleDebouncedSave = () => {
    if (!loaded || !document || disposed) return;
    cancelDebounce();
    const revision = ++saveRevision;
    setStatus("saving");
    debounceTimer = setTimeout(() => {
      debounceTimer = null;
      const snapshot = document;
      if (!snapshot) return;
      enqueueSave(snapshot)
        .then(() => {
          if (saveRevision === revision) setStatus("saved");
        })
        .catch((error) => {
          if (saveRevision !== revision) return;
          setStatus("error");
          reportFailure({ phase: "debounced", error });
        });
    }, debounceMs);
  };

  return {
    subscribe(listener) {
      documentListeners.add(listener);
      return () => documentListeners.delete(listener);
    },
    getSnapshot: () => document,
    current: () => document,

    subscribeSaveStatus(listener) {
      statusListeners.add(listener);
      return () => statusListeners.delete(listener);
    },
    getSaveStatus: () => saveStatus,
    subscribeSaveFailures(listener) {
      failureListeners.add(listener);
      return () => failureListeners.delete(listener);
    },

    load(next) {
      document = next;
      loaded = true;
      notifyDocument();
      // Loading may migrate the schema, so persist its result without waiting for
      // the first user edit.
      scheduleDebouncedSave();
    },

    update(updater) {
      const next = updater(document);
      if (next === document) return;
      document = next;
      notifyDocument();
      scheduleDebouncedSave();
    },

    async publish(next, publishOptions = {}) {
      cancelDebounce();
      const revision = ++saveRevision;
      setStatus("saving");
      // Publish before I/O so later changes build on this snapshot. Disk writes
      // share one serialized queue.
      document = next;
      notifyDocument();
      try {
        await enqueueSave(next, { durable: publishOptions.durable });
        if (saveRevision === revision) setStatus("saved");
      } catch (error) {
        if (saveRevision === revision) setStatus("error");
        // Retain optimistic state and throw to the caller. Schedule one ordinary
        // background retry for the current snapshot; it cannot retry infinitely
        // because another failure only reports through the failure channel.
        scheduleDebouncedSave();
        throw error;
      }
    },

    async flush(flushOptions = {}) {
      const latest = document;
      if (!latest) return;
      cancelDebounce();
      const revision = ++saveRevision;
      setStatus("saving");
      try {
        await enqueueSave(latest, { durable: flushOptions.durable });
        if (saveRevision === revision) setStatus("saved");
      } catch (error) {
        if (saveRevision === revision) setStatus("error");
        // Flush canceled a pending debounce. Schedule a background retry on
        // failure so that canceled write is not lost when the application exits.
        scheduleDebouncedSave();
        throw error;
      }
    },

    reportBackendSaveResult(result) {
      if (result === "failure") {
        setStatus("error");
        return;
      }
      if (saveStatus === "error") setStatus("saved");
    },

    dispose() {
      disposed = true;
      cancelDebounce();
    },

    resume() {
      if (!disposed) return;
      disposed = false;
      scheduleDebouncedSave();
    }
  };
}
