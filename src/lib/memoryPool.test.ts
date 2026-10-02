import { describe, expect, it, vi } from "vitest";
import { createMemoryPool, pooledFetch } from "./memoryPool";

describe("memory pool", () => {
  it("unloads low-priority data first, even when it was used more recently", () => {
    const pool = createMemoryPool(100);
    const unloaded = vi.fn();
    pool.onUnload(unloaded);
    pool.set("conversationBody", "old-body", true, 40);
    pool.set("imageData", "new-image", "data:", 40);
    pool.set("conversationBody", "new-body", true, 40);
    expect(unloaded).toHaveBeenCalledWith("imageData", "new-image");
    expect(pool.keys("conversationBody").sort()).toEqual(["new-body", "old-body"]);
  });

  it("within a tier unloads the least recently used", () => {
    const pool = createMemoryPool(100);
    pool.set("conversationBody", "a", true, 40);
    pool.set("imageThumbnail", "b", "thumb", 40);
    pool.touch("conversationBody", "a");
    pool.set("conversationBody", "c", true, 40);
    expect(pool.has("imageThumbnail", "b")).toBe(false);
    expect(pool.keys("conversationBody").sort()).toEqual(["a", "c"]);
  });

  it("a resize or a peek is not a use", () => {
    const pool = createMemoryPool(100);
    pool.set("conversationBody", "a", true, 40);
    pool.set("conversationBody", "b", true, 40);
    pool.resize("conversationBody", "a", 45);
    pool.peek("conversationBody", "a");
    pool.set("conversationBody", "c", true, 40);
    expect(pool.keys("conversationBody").sort()).toEqual(["b", "c"]);
  });

  it("keeps pinned entries over the cap and enforces it once they are released", () => {
    const pool = createMemoryPool(100);
    const release = pool.pin("conversationBody", "active");
    pool.set("conversationBody", "active", true, 90);
    pool.set("conversationBody", "other", true, 40);
    expect(pool.keys("conversationBody")).toEqual(["active"]);
    pool.resize("conversationBody", "active", 150);
    expect(pool.stats().totalBytes).toBe(150);
    release();
    expect(pool.stats().totalBytes).toBe(0);
  });

  it("counts bytes by kind", () => {
    const pool = createMemoryPool(1000);
    pool.set("conversationBody", "a", true, 10);
    pool.set("conversationBody", "b", true, 5);
    pool.set("fileData", "f", "data:", 7);
    expect(pool.stats()).toEqual({
      capBytes: 1000,
      totalBytes: 22,
      pinnedEntries: 0,
      byKind: { conversationBody: { entries: 2, bytes: 15 }, fileData: { entries: 1, bytes: 7 } }
    });
  });

  it("shares one fetch between readers, keeps what arrives and forgets what fails", async () => {
    const pool = createMemoryPool(1000);
    const load = vi.fn().mockResolvedValue("data:image/png;base64,AAAA");
    const [first, second] = await Promise.all([
      pooledFetch(pool, "imageThumbnail", "k", load),
      pooledFetch(pool, "imageThumbnail", "k", load)
    ]);
    expect(first).toBe(second);
    expect(load).toHaveBeenCalledTimes(1);
    expect(pool.stats().totalBytes).toBe("data:image/png;base64,AAAA".length);
    await pooledFetch(pool, "imageThumbnail", "k", load);
    expect(load).toHaveBeenCalledTimes(1);

    const failing = vi.fn().mockRejectedValue(new Error("gone"));
    await expect(pooledFetch(pool, "imageData", "x", failing)).rejects.toThrow("gone");
    expect(pool.has("imageData", "x")).toBe(false);
  });
});
