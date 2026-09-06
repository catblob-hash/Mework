import { describe, expect, it, vi } from "vitest";
import { createComposerController } from "./composerController";
import type { ImageAttachment } from "../types";

function image(id: string): ImageAttachment {
  return { id, bytes: 1, width: 1, height: 1 } as ImageAttachment;
}

function deferred<T = void>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

describe("createComposerController", () => {
  it("commits draft, image, steering, and failed-promotion updates reactively", () => {
    const controller = createComposerController();
    const listener = vi.fn();
    controller.subscribe(listener);

    controller.updateDrafts((current) => ({ ...current, "conversation-1": "hello" }));
    controller.updateImageDrafts((current) => ({ ...current, "conversation-1": [image("image-1")] }));
    controller.updateSteeringMessageIds((current) => new Set(current).add("message-1"));
    controller.updateFailedQueuedPromotionIds((current) => new Set(current).add("message-2"));

    expect(listener).toHaveBeenCalledTimes(4);
    const state = controller.current();
    expect(state.drafts["conversation-1"]).toBe("hello");
    expect(state.imageDrafts["conversation-1"]).toEqual([image("image-1")]);
    expect(state.steeringMessageIds.has("message-1")).toBe(true);
    expect(state.failedQueuedPromotionIds.has("message-2")).toBe(true);
  });

  it("serializes upload batches per conversation and tracks the loading flag", async () => {
    const controller = createComposerController();
    const first = deferred();
    const order: string[] = [];
    const firstUpload = controller.enqueueImageUpload("conversation-1", async () => {
      order.push("first:start");
      await first.promise;
      order.push("first:end");
    });
    const secondUpload = controller.enqueueImageUpload("conversation-1", async () => {
      order.push("second:start");
    });
    expect(controller.current().imageLoadingIds.has("conversation-1")).toBe(true);
    first.resolve();
    await firstUpload;
    await secondUpload;
    expect(order).toEqual(["first:start", "first:end", "second:start"]);
    expect(controller.current().imageLoadingIds.has("conversation-1")).toBe(false);
  });

  it("keeps the loading flag while a later batch is still queued", async () => {
    const controller = createComposerController();
    const first = deferred();
    const second = deferred();
    const firstUpload = controller.enqueueImageUpload("conversation-1", async () => {
      await first.promise;
    });
    const secondUpload = controller.enqueueImageUpload("conversation-1", async () => {
      await second.promise;
    });
    first.resolve();
    await firstUpload;
    expect(controller.current().imageLoadingIds.has("conversation-1")).toBe(true);
    second.resolve();
    await secondUpload;
    expect(controller.current().imageLoadingIds.has("conversation-1")).toBe(false);
  });

  it("runs a queued batch even after the previous batch rejected", async () => {
    const controller = createComposerController();
    const failing = controller.enqueueImageUpload("conversation-1", async () => {
      throw new Error("batch failed");
    });
    await expect(failing).rejects.toThrow("batch failed");
    const ran = vi.fn(async () => {});
    await controller.enqueueImageUpload("conversation-1", ran);
    expect(ran).toHaveBeenCalledTimes(1);
    expect(controller.current().imageLoadingIds.has("conversation-1")).toBe(false);
  });

  it("invalidateImages flips uploadStillCurrent, drops drafts, and clears loading", async () => {
    const controller = createComposerController();
    const gate = deferred();
    const observed: boolean[] = [];
    const upload = controller.enqueueImageUpload("conversation-1", async (uploadStillCurrent) => {
      observed.push(uploadStillCurrent());
      await gate.promise;
      observed.push(uploadStillCurrent());
    });
    // The batch starts on a microtask; let it record its first probe before invalidating.
    await vi.waitFor(() => expect(observed).toHaveLength(1));
    controller.updateImageDrafts(() => ({
      "conversation-1": [image("image-1")],
      "conversation-2": [image("image-2")]
    }));
    controller.invalidateImages(["conversation-1"]);
    expect(controller.current().imageDrafts["conversation-1"]).toBeUndefined();
    expect(controller.current().imageDrafts["conversation-2"]).toEqual([image("image-2")]);
    expect(controller.current().imageLoadingIds.has("conversation-1")).toBe(false);
    gate.resolve();
    await upload;
    expect(observed).toEqual([true, false]);
  });

  it("clears save tracking when the tracked save resolves", async () => {
    const controller = createComposerController();
    const save = deferred();
    controller.markQueuedMessageUnsaved("message-1");
    controller.trackQueuedMessageSave("message-1", save.promise);
    expect(controller.queuedMessageSave("message-1")).toBe(save.promise);
    expect(controller.queuedMessageNeedsSave("message-1")).toBe(true);
    save.resolve();
    await save.promise;
    expect(controller.queuedMessageSave("message-1")).toBeUndefined();
    expect(controller.queuedMessageNeedsSave("message-1")).toBe(false);
  });

  it("keeps a rejected save tracked for the steer barrier to observe", async () => {
    const controller = createComposerController();
    const save = deferred();
    controller.markQueuedMessageUnsaved("message-1");
    controller.trackQueuedMessageSave("message-1", save.promise);
    save.reject(new Error("save failed"));
    await save.promise.catch(() => undefined);
    expect(controller.queuedMessageSave("message-1")).toBe(save.promise);
    controller.dropQueuedMessageSaveIfCurrent("message-1", save.promise);
    expect(controller.queuedMessageSave("message-1")).toBeUndefined();
  });

  it("does not let a superseded save clear a newer one", async () => {
    const controller = createComposerController();
    const stale = deferred();
    const fresh = deferred();
    controller.trackQueuedMessageSave("message-1", stale.promise);
    controller.trackQueuedMessageSave("message-1", fresh.promise);
    stale.resolve();
    await stale.promise;
    expect(controller.queuedMessageSave("message-1")).toBe(fresh.promise);
    controller.dropQueuedMessageSaveIfCurrent("message-1", stale.promise);
    expect(controller.queuedMessageSave("message-1")).toBe(fresh.promise);
  });

  it("forgetQueuedMessages drops saves and failed-promotion ids in one sweep", () => {
    const controller = createComposerController();
    const listener = vi.fn();
    controller.markQueuedMessageUnsaved("message-1");
    controller.trackQueuedMessageSave("message-1", Promise.resolve());
    controller.updateFailedQueuedPromotionIds(() => new Set(["message-1", "message-2"]));
    controller.subscribe(listener);

    controller.forgetQueuedMessages(["message-1"]);
    expect(controller.queuedMessageSave("message-1")).toBeUndefined();
    expect(controller.queuedMessageNeedsSave("message-1")).toBe(false);
    expect([...controller.current().failedQueuedPromotionIds]).toEqual(["message-2"]);
    expect(listener).toHaveBeenCalledTimes(1);

    // No failed ids touched: no extra notification.
    controller.forgetQueuedMessages(["message-3"]);
    expect(listener).toHaveBeenCalledTimes(1);
  });
});
