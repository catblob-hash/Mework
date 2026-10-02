/**
 * Conversation bodies as pooled data.
 *
 * The document keeps every conversation, but a conversation's body — its
 * contexts on every branch — is high-priority data in the shared memory pool
 * (`memoryPool.ts`). Past the pool's cap the least recently opened bodies are
 * unloaded: the conversation stays in the document, marked `bodyUnloaded`,
 * with its metadata, settings and queued messages, and its contexts are
 * fetched from the host again when it is opened (`load_conversation`, which
 * reads through the host's own pool and falls back to its database).
 *
 * The host is the only writer of bodies, so unloading and loading are purely
 * local: neither is ever written back. A body that is not loaded must never
 * reach the host as if it were the conversation's timeline — see
 * {@link UNLOADED_BODY_EXPECTED_IDS} and `conversationSync.ts`.
 */
import type { AppDocument, ContextItem, Conversation } from "../types";
import type { MemoryPool } from "./memoryPool";

/**
 * The expected-context-ids a write sends for a conversation whose body is not
 * loaded. It matches no real timeline, so the host takes the write's metadata
 * and keeps its own body — never the empty one the renderer holds.
 */
export const UNLOADED_BODY_EXPECTED_IDS: readonly string[] = ["\u0000unloaded-body"];

export function isBodyUnloaded(conversation: Conversation): boolean {
  return conversation.bodyUnloaded === true;
}

/**
 * The conversation without its body — contexts and branches — keeping metadata, settings and
 * queue. Branch records are body: each hangs off a user message of the timeline, and the host
 * refuses a conversation holding branches without it (`attachment_refs::strip_body`).
 */
export function hollowConversation(conversation: Conversation): Conversation {
  return { ...conversation, contexts: [], branches: [], bodyUnloaded: true };
}

/**
 * Whether a conversation has no contexts. Never true of one whose body is not
 * loaded: an unloaded body is not an empty one.
 */
export function conversationHasNoContexts(conversation: Conversation): boolean {
  return !isBodyUnloaded(conversation) && conversation.contexts.length === 0;
}

/**
 * `next` with the body of `base` when `base`'s is not loaded: what an update
 * may change about an unloaded conversation is its metadata. Anything an
 * updater derived from the empty stand-in body is dropped, not applied.
 */
export function keepUnloadedBody(base: Conversation, next: Conversation): Conversation {
  if (!isBodyUnloaded(base)) return next;
  if (next.contexts === base.contexts && next.branches === base.branches && next.bodyUnloaded) return next;
  if (next.contexts.length > 0 || next.branches.length > 0) {
    console.warn(`对话 ${base.id} 的正文未载入，这次改动只保留元数据`);
  }
  return { ...next, contexts: base.contexts, branches: base.branches, bodyUnloaded: true };
}

const itemBytes = new WeakMap<object, number>();
const listBytes = new WeakMap<object, number>();

/**
 * What a list of contexts costs, as its JSON length. Items are immutable and
 * every update replaces the list, so each item is measured once and an
 * unchanged list costs a lookup.
 */
function contextsBytes(contexts: readonly ContextItem[]): number {
  const cached = listBytes.get(contexts);
  if (cached !== undefined) return cached;
  let total = 0;
  for (const item of contexts) {
    let size = itemBytes.get(item);
    if (size === undefined) {
      size = JSON.stringify(item)?.length ?? 0;
      itemBytes.set(item, size);
    }
    total += size;
  }
  listBytes.set(contexts, total);
  return total;
}

export function conversationBodyBytes(conversation: Conversation): number {
  let total = contextsBytes(conversation.contexts);
  for (const branch of conversation.branches) total += contextsBytes(branch.contexts ?? []);
  return total;
}

export interface ConversationBodyCache {
  /**
   * Mirrors the document into the pool: each loaded body's size, the pins,
   * conversations that went away. Unloads what the cap then requires.
   */
  sync(document: AppDocument | null, pinnedIds: ReadonlySet<string>): void;
  /** Marks a conversation's body as just used — the user opened it. */
  touch(conversationId: string): void;
  /**
   * The conversation with its body, fetching the body from the host when it
   * is not loaded. Concurrent callers share one fetch; `null` when the
   * conversation is gone.
   */
  ensureLoaded(conversationId: string): Promise<Conversation | null>;
  /** Keeps a body loaded (once loaded) until the returned release runs. */
  hold(conversationId: string): () => void;
  isLoading(conversationId: string): boolean;
  /** Stops listening to the pool and lets go of every pin. */
  dispose(): void;
}

interface ConversationBodyCacheOptions {
  pool: MemoryPool;
  current: () => AppDocument | null;
  /** Puts a loaded conversation into the document without writing it back. */
  install: (conversation: Conversation) => void;
  /** Drops these conversations' bodies from the document without writing it back. */
  unload: (conversationIds: string[]) => void;
  load: (conversationId: string) => Promise<Conversation | null>;
  /** Whether bodies can be fetched again once dropped. Without a host they cannot. */
  enabled: () => boolean;
  onLoadingChange?: () => void;
}

function findConversation(document: AppDocument | null, conversationId: string): Conversation | null {
  for (const workspace of document?.workspaces ?? []) {
    const found = workspace.conversations.find((conversation) => conversation.id === conversationId);
    if (found) return found;
  }
  return null;
}

export function createConversationBodyCache(options: ConversationBodyCacheOptions): ConversationBodyCache {
  const { pool } = options;
  const syncPins = new Map<string, () => void>();
  const holds = new Map<string, number>();
  const holdPins = new Map<string, () => void>();
  const loads = new Map<string, Promise<Conversation | null>>();

  const stopListening = pool.onUnload((kind, conversationId) => {
    if (kind !== "conversationBody" || !options.enabled()) return;
    options.unload([conversationId]);
  });

  const cache: ConversationBodyCache = {
    sync(document, pinnedIds) {
      if (!options.enabled() || !document) return;
      // Pins first, so the sizes that follow never unload what must stay.
      for (const conversationId of pinnedIds) {
        if (!syncPins.has(conversationId)) syncPins.set(conversationId, pool.pin("conversationBody", conversationId));
      }
      const present = new Set<string>();
      for (const workspace of document.workspaces) {
        for (const conversation of workspace.conversations) {
          if (isBodyUnloaded(conversation)) continue;
          present.add(conversation.id);
          const bytes = conversationBodyBytes(conversation);
          if (pool.has("conversationBody", conversation.id)) pool.resize("conversationBody", conversation.id, bytes);
          else pool.set("conversationBody", conversation.id, true, bytes);
        }
      }
      for (const conversationId of pool.keys("conversationBody")) {
        if (!present.has(conversationId)) pool.delete("conversationBody", conversationId);
      }
      for (const [conversationId, release] of [...syncPins]) {
        if (pinnedIds.has(conversationId)) continue;
        syncPins.delete(conversationId);
        release();
      }
    },

    touch(conversationId) {
      pool.touch("conversationBody", conversationId);
    },

    ensureLoaded(conversationId) {
      const existing = findConversation(options.current(), conversationId);
      if (!existing) return Promise.resolve(null);
      if (!isBodyUnloaded(existing)) return Promise.resolve(existing);
      const inflight = loads.get(conversationId);
      if (inflight) return inflight;
      const release = cache.hold(conversationId);
      const request = options.load(conversationId)
        .then((loaded) => {
          const stillThere = findConversation(options.current(), conversationId);
          if (!loaded || !stillThere) return null;
          if (!isBodyUnloaded(stillThere)) return stillThere;
          const { bodyUnloaded: _unloaded, ...installed } = loaded;
          options.install(installed);
          pool.set("conversationBody", conversationId, true, conversationBodyBytes(installed));
          return installed;
        })
        .finally(() => {
          loads.delete(conversationId);
          release();
          options.onLoadingChange?.();
        });
      loads.set(conversationId, request);
      options.onLoadingChange?.();
      return request;
    },

    hold(conversationId) {
      holds.set(conversationId, (holds.get(conversationId) ?? 0) + 1);
      if (!holdPins.has(conversationId)) holdPins.set(conversationId, pool.pin("conversationBody", conversationId));
      let released = false;
      return () => {
        if (released) return;
        released = true;
        const count = (holds.get(conversationId) ?? 1) - 1;
        if (count > 0) {
          holds.set(conversationId, count);
          return;
        }
        holds.delete(conversationId);
        const unpin = holdPins.get(conversationId);
        holdPins.delete(conversationId);
        unpin?.();
      };
    },

    isLoading: (conversationId) => loads.has(conversationId),

    dispose() {
      stopListening();
      for (const release of [...syncPins.values(), ...holdPins.values()]) release();
      syncPins.clear();
      holdPins.clear();
      holds.clear();
    }
  };
  return cache;
}
