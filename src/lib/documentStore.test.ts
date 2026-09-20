import { describe, expect, it, vi } from "vitest";
import { createDocumentStore, type DocumentSaveFailure } from "./documentStore";
import { createTestDocument } from "../test/fixtures";
import type { AppDocument } from "../types";

function deferredSave() {
  const calls: Array<{
    snapshot: AppDocument;
    options: { immutableSnapshot: true; durable?: boolean };
    resolve: () => void;
    reject: (error: unknown) => void;
  }> = [];
  const save = vi.fn(
    (snapshot: AppDocument, options: { immutableSnapshot: true; durable?: boolean }) =>
      new Promise<void>((resolve, reject) => {
        calls.push({ snapshot, options, resolve, reject });
      })
  );
  return { save, calls };
}

const tick = () => new Promise<void>((resolve) => setTimeout(resolve, 0));

describe("createDocumentStore", () => {
  it("publishes updates synchronously to current() before React re-renders", () => {
    const { save } = deferredSave();
    const store = createDocumentStore({ save, debounceMs: 10 });
    store.load(createTestDocument());
    const before = store.current();
    store.update((current) => current && { ...current, schemaVersion: current.schemaVersion });
    // A new object returned by the updater is immediately visible to `current()`.
    expect(store.current()).not.toBe(before);
  });

  it("evaluates chained functional updates against the authoritative value", () => {
    const store = createDocumentStore({ save: deferredSave().save, debounceMs: 10 });
    store.load(createTestDocument());
    const seen: Array<string | undefined> = [];
    store.update(
      (current) =>
        current && {
          ...current,
          globalSettings: { ...current.globalSettings, defaultConversationPresetId: "first" }
        }
    );
    store.update((current) => {
      seen.push(current?.globalSettings.defaultConversationPresetId);
      return current;
    });
    expect(seen).toEqual(["first"]);
  });

  it("does not schedule saves before load, and debounces after it", async () => {
    vi.useFakeTimers();
    try {
      const { save } = deferredSave();
      const store = createDocumentStore({ save, debounceMs: 420 });
      store.update(() => createTestDocument());
      vi.advanceTimersByTime(1000);
      expect(save).not.toHaveBeenCalled();

      store.load(createTestDocument());
      store.update((current) => current && { ...current });
      store.update((current) => current && { ...current });
      expect(store.getSaveStatus()).toBe("saving");
      vi.advanceTimersByTime(419);
      await vi.advanceTimersByTimeAsync(0);
      expect(save).not.toHaveBeenCalled();
      vi.advanceTimersByTime(1);
      await vi.advanceTimersByTimeAsync(0);
      // Multiple updates coalesce into one write of the final snapshot.
      expect(save).toHaveBeenCalledTimes(1);
      expect(save.mock.calls[0][0]).toBe(store.current());
    } finally {
      vi.useRealTimers();
    }
  });

  it("publish persists immediately, keeps optimistic state on failure, and rethrows", async () => {
    const { save, calls } = deferredSave();
    const store = createDocumentStore({ save, debounceMs: 10 });
    store.load(createTestDocument());
    const failures: DocumentSaveFailure[] = [];
    store.subscribeSaveFailures((failure) => failures.push(failure));

    const next = { ...createTestDocument() };
    const published = store.publish(next, { durable: true });
    expect(store.current()).toBe(next);
    expect(store.getSaveStatus()).toBe("saving");
    await tick();
    expect(calls[0].options.durable).toBe(true);

    calls[0].reject(new Error("disk full"));
    await expect(published).rejects.toThrow("disk full");
    expect(store.current()).toBe(next);
    // Keep optimistic state and schedule a background retry after failure.
    await new Promise((resolve) => setTimeout(resolve, 25));
    expect(calls).toHaveLength(2);
    calls[1].resolve();
    await tick();
    expect(store.getSaveStatus()).toBe("saved");
    // A publish failure is handled by the caller and must not also use the background channel.
    expect(failures).toEqual([]);
  });

  it("serializes disk writes in order even when a save fails", async () => {
    const order: string[] = [];
    const results = [Promise.reject(new Error("first fails")), Promise.resolve()];
    results[0].catch(() => undefined);
    let index = 0;
    const save = vi.fn((snapshot: AppDocument) => {
      order.push(`start-${index}`);
      return results[index++] ?? Promise.resolve();
    });
    const store = createDocumentStore({ save, debounceMs: 1 });
    store.load(createTestDocument());
    const first = store.publish({ ...createTestDocument() }).catch(() => "failed");
    const second = store.publish({ ...createTestDocument() });
    expect(await first).toBe("failed");
    await second;
    store.dispose();
    // A failed first publish may schedule a retry, but the first two writes stay ordered.
    expect(order.slice(0, 2)).toEqual(["start-0", "start-1"]);
  });

  it("flush cancels the pending debounce and saves the current snapshot", async () => {
    vi.useFakeTimers();
    try {
      const { save, calls } = deferredSave();
      const store = createDocumentStore({ save, debounceMs: 420 });
      store.load(createTestDocument());
      store.update((current) => current && { ...current });
      const flushed = store.flush({ durable: true });
      await vi.advanceTimersByTimeAsync(0);
      expect(save).toHaveBeenCalledTimes(1);
      calls[0].resolve();
      await flushed;
      expect(store.getSaveStatus()).toBe("saved");
      vi.advanceTimersByTime(1000);
      // The cancelled debounce must not produce a second write.
      expect(save).toHaveBeenCalledTimes(1);
    } finally {
      vi.useRealTimers();
    }
  });

  it("flush reschedules the debounce it cancelled when the save fails", async () => {
    vi.useFakeTimers();
    try {
      const { save, calls } = deferredSave();
      const store = createDocumentStore({ save, debounceMs: 420 });
      store.load(createTestDocument());
      const dirty = { ...createTestDocument(), schemaVersion: 70 };
      store.update(() => dirty);

      // `flush` cancelled the pending debounced save.
      const flushed = store.flush();
      await vi.advanceTimersByTimeAsync(0);
      expect(save).toHaveBeenCalledTimes(1);

      calls[0].reject(new Error("io"));
      await expect(flushed).rejects.toThrow("io");
      // Restore the cancelled write; otherwise the dirty document remains memory-only.
      expect(store.current()).toBe(dirty);
      await vi.advanceTimersByTimeAsync(420);
      expect(save).toHaveBeenCalledTimes(2);
      expect(save.mock.calls[1][0]).toBe(dirty);

      calls[1].resolve();
      await vi.advanceTimersByTimeAsync(0);
      expect(store.getSaveStatus()).toBe("saved");
    } finally {
      vi.useRealTimers();
    }
  });

  it("reports debounced save failures through the failure channel", async () => {
    vi.useFakeTimers();
    try {
      const { save, calls } = deferredSave();
      const store = createDocumentStore({ save, debounceMs: 5 });
      store.load(createTestDocument());
      const failures: DocumentSaveFailure[] = [];
      store.subscribeSaveFailures((failure) => failures.push(failure));
      store.update((current) => current && { ...current });
      vi.advanceTimersByTime(5);
      await vi.advanceTimersByTimeAsync(0);
      calls[0].reject(new Error("io"));
      await vi.advanceTimersByTimeAsync(0);
      expect(failures).toHaveLength(1);
      expect(failures[0].phase).toBe("debounced");
      expect(store.getSaveStatus()).toBe("error");
    } finally {
      vi.useRealTimers();
    }
  });

  it("a newer revision's outcome owns the status; stale outcomes are ignored", async () => {
    vi.useFakeTimers();
    try {
      const { save, calls } = deferredSave();
      const store = createDocumentStore({ save, debounceMs: 5 });
      store.load(createTestDocument());
      store.update((current) => current && { ...current });
      vi.advanceTimersByTime(5);
      await vi.advanceTimersByTimeAsync(0);
      // A new publish begins before the older debounced save reaches disk.
      const published = store.publish({ ...createTestDocument() });
      await vi.advanceTimersByTimeAsync(0);
      calls[0].resolve();
      await vi.advanceTimersByTimeAsync(0);
      // A stale revision must not change `saving` to `saved`.
      expect(store.getSaveStatus()).toBe("saving");
      calls[1].resolve();
      await published;
      expect(store.getSaveStatus()).toBe("saved");
    } finally {
      vi.useRealTimers();
    }
  });

  it("bridges backend push results: failure turns red, success only clears error", () => {
    const store = createDocumentStore({ save: deferredSave().save });
    store.load(createTestDocument());
    store.reportBackendSaveResult("failure");
    expect(store.getSaveStatus()).toBe("error");
    store.reportBackendSaveResult("success");
    expect(store.getSaveStatus()).toBe("saved");
    // A successful backend push must not interrupt an active save.
    store.update((current) => current && { ...current });
    expect(store.getSaveStatus()).toBe("saving");
    store.reportBackendSaveResult("success");
    expect(store.getSaveStatus()).toBe("saving");
  });

  it("dispose cancels the pending debounce and stops future background saves", async () => {
    vi.useFakeTimers();
    try {
      const { save } = deferredSave();
      const store = createDocumentStore({ save, debounceMs: 420 });
      store.load(createTestDocument());
      store.update((current) => current && { ...current });
      store.dispose();
      // Disposal prevents both the pending save and future save scheduling.
      store.update((current) => current && { ...current });
      vi.advanceTimersByTime(2000);
      await vi.advanceTimersByTimeAsync(0);
      expect(save).not.toHaveBeenCalled();
    } finally {
      vi.useRealTimers();
    }
  });

  it("resume after dispose re-arms background saves (StrictMode double-mount)", async () => {
    vi.useFakeTimers();
    try {
      const { save, calls } = deferredSave();
      const store = createDocumentStore({ save, debounceMs: 420 });
      // StrictMode mounts, disposes, then resumes the same instance. Resume must restore saves.
      store.load(createTestDocument());
      store.dispose();
      store.update((current) => current && { ...current });
      vi.advanceTimersByTime(2000);
      await vi.advanceTimersByTimeAsync(0);
      expect(save).not.toHaveBeenCalled();

      store.resume();
      // Resume must schedule a write for changes made while inactive.
      expect(store.getSaveStatus()).toBe("saving");
      vi.advanceTimersByTime(420);
      await vi.advanceTimersByTimeAsync(0);
      expect(save).toHaveBeenCalledTimes(1);
      calls[0].resolve();
      await vi.advanceTimersByTimeAsync(0);
      expect(store.getSaveStatus()).toBe("saved");

      // Subsequent changes schedule writes normally after resuming.
      store.update((current) => current && { ...current });
      vi.advanceTimersByTime(420);
      await vi.advanceTimersByTimeAsync(0);
      expect(save).toHaveBeenCalledTimes(2);

      // Calling resume while active is a no-op.
      calls[1].resolve();
      await vi.advanceTimersByTimeAsync(0);
      store.resume();
      expect(store.getSaveStatus()).toBe("saved");
    } finally {
      vi.useRealTimers();
    }
  });
});
