import { describe, expect, it, vi } from "vitest";
import type { AppDocument, ContextItem, Conversation } from "../types";
import { createSeedDocument } from "../seed";
import {
  conversationBodyBytes,
  conversationHasNoContexts,
  createConversationBodyCache,
  hollowConversation,
  isBodyUnloaded,
  keepUnloadedBody
} from "./conversationBodies";
import { createMemoryPool } from "./memoryPool";

function user(id: string, content: string): ContextItem {
  return { kind: "user", id, content, createdAt: "2026-09-30T00:00:00.000Z" } as ContextItem;
}

function documentWith(conversations: Conversation[]): AppDocument {
  const seed = createSeedDocument();
  return {
    ...seed,
    workspaces: [{ ...seed.workspaces[0]!, conversations }]
  };
}

function conversation(id: string, contexts: ContextItem[]): Conversation {
  const template = createSeedDocument().workspaces[0]!.conversations[0]!;
  return { ...template, id, contexts, branches: [], queuedMessages: [] };
}

/** A document store in miniature: the cache's install and unload write here. */
function harness(initial: AppDocument, capBytes: number, load = vi.fn()) {
  let document: AppDocument | null = initial;
  const replace = (id: string, map: (conversation: Conversation) => Conversation) => {
    if (!document) return;
    document = {
      ...document,
      workspaces: document.workspaces.map((workspace) => ({
        ...workspace,
        conversations: workspace.conversations.map((candidate) => (
          candidate.id === id ? map(candidate) : candidate
        ))
      }))
    };
  };
  const pool = createMemoryPool(capBytes);
  const cache = createConversationBodyCache({
    pool,
    current: () => document,
    install: (loaded) => replace(loaded.id, () => loaded),
    unload: (ids) => {
      for (const id of ids) replace(id, hollowConversation);
    },
    load,
    enabled: () => true
  });
  const find = (id: string) => document?.workspaces[0]?.conversations.find((candidate) => candidate.id === id);
  return { cache, pool, find, current: () => document };
}

describe("conversation bodies", () => {
  it("an unloaded body is not an empty one", () => {
    const loaded = conversation("c", [user("u", "hi")]);
    const hollow = hollowConversation(loaded);
    expect(isBodyUnloaded(hollow)).toBe(true);
    expect(hollow.contexts).toEqual([]);
    expect(conversationHasNoContexts(hollow)).toBe(false);
    expect(conversationHasNoContexts(conversation("e", []))).toBe(true);
  });

  it("an update to an unloaded conversation keeps its metadata and drops any body built from nothing", () => {
    const hollow = hollowConversation(conversation("c", [user("u", "hi")]));
    const warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    const renamed = keepUnloadedBody(hollow, { ...hollow, title: "renamed" });
    expect(renamed.title).toBe("renamed");
    expect(isBodyUnloaded(renamed)).toBe(true);
    const spliced = keepUnloadedBody(hollow, { ...hollow, contexts: [user("x", "from nothing")], bodyUnloaded: undefined });
    expect(spliced.contexts).toEqual([]);
    expect(isBodyUnloaded(spliced)).toBe(true);
    expect(warn).toHaveBeenCalled();
    warn.mockRestore();
    const loaded = conversation("d", []);
    const next = { ...loaded, contexts: [user("u", "new")] };
    expect(keepUnloadedBody(loaded, next)).toBe(next);
  });

  it("measures a body once per item and reuses unchanged lists", () => {
    const items = [user("a", "x".repeat(100)), user("b", "y".repeat(50))];
    const first = conversation("c", items);
    const bytes = conversationBodyBytes(first);
    expect(bytes).toBe(JSON.stringify(items[0]).length + JSON.stringify(items[1]).length);
    const stringify = vi.spyOn(JSON, "stringify");
    expect(conversationBodyBytes({ ...first, contexts: [...items] })).toBe(bytes);
    expect(stringify).not.toHaveBeenCalled();
    stringify.mockRestore();
  });

  it("unloads the least recently opened body past the cap, never a pinned one", () => {
    const big = (id: string) => conversation(id, [user(`${id}-u`, "z".repeat(1000))]);
    const { cache, find, current } = harness(documentWith([big("old"), big("mid"), big("new")]), 2500);
    cache.sync(current(), new Set(["new"]));
    // Three bodies of ~1 KiB against 2.5 KiB: the oldest unpinned one goes.
    expect(isBodyUnloaded(find("old")!)).toBe(true);
    expect(isBodyUnloaded(find("mid")!)).toBe(false);
    expect(isBodyUnloaded(find("new")!)).toBe(false);
  });

  it("opening a conversation fetches its body once and keeps it", async () => {
    const stored = conversation("c", [user("u", "hello")]);
    const load = vi.fn().mockResolvedValue(stored);
    const { cache, pool, find } = harness(documentWith([hollowConversation(stored)]), 10_000, load);
    const [first, second] = await Promise.all([cache.ensureLoaded("c"), cache.ensureLoaded("c")]);
    expect(load).toHaveBeenCalledTimes(1);
    expect(first).toEqual(stored);
    expect(second).toEqual(stored);
    expect(find("c")?.contexts).toEqual(stored.contexts);
    expect(isBodyUnloaded(find("c")!)).toBe(false);
    expect(pool.has("conversationBody", "c")).toBe(true);
    await cache.ensureLoaded("c");
    expect(load).toHaveBeenCalledTimes(1);
  });

  it("a conversation deleted while its body was on the way stays deleted", async () => {
    const stored = conversation("c", [user("u", "hello")]);
    let resolve: (value: Conversation) => void = () => undefined;
    const load = vi.fn(() => new Promise<Conversation>((done) => { resolve = done; }));
    const { cache, current } = harness(documentWith([hollowConversation(stored)]), 10_000, load);
    const pending = cache.ensureLoaded("c");
    const document = current()!;
    document.workspaces[0]!.conversations = [];
    resolve(stored);
    expect(await pending).toBeNull();
  });
});
