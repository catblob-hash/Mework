/**
 * The renderer's one shared memory pool — the counterpart of the host's
 * `memory_pool.rs`, with the same rules.
 *
 * Every conversation keeps its body here, and attachments keep their chips
 * and raw bytes; all of them draw on one budget. Each entry has a priority,
 * and once the pool is over its cap it unloads the least recently used entry
 * of the lowest priority first:
 *
 * - high: what the timeline draws — message bodies, reasoning, tool output,
 *   pasted text, and the small chip an image shows. Opening a conversation or
 *   expanding a card must not wait on the host.
 * - low: the raw bytes of images and uploaded files. Only the full-size
 *   viewer and the file preview read them, and both can afford a fetch.
 *
 * Only data is managed here, never a process: what goes in is a copy of
 * something the host can hand back, so unloading it costs a fetch and nothing
 * else. Run state, the composer, terminals and the rest of the renderer's
 * live state are not entries and are never unloaded.
 *
 * A pinned entry (the conversation on screen, one with a run in flight) is
 * never unloaded, so the pool can sit over its cap until the pin goes.
 */

/** The pool's cap: 2 GiB across every tier, as on the host. */
const MEMORY_POOL_CAP_BYTES = 2 * 1024 * 1024 * 1024;

export type MemoryPoolKind = "conversationBody" | "imageThumbnail" | "imageData" | "fileData";

export type MemoryPriority = "low" | "high";

/** The kind fixes the priority, so raw bytes can never be filed as timeline content. */
function memoryPriority(kind: MemoryPoolKind): MemoryPriority {
  return kind === "imageData" || kind === "fileData" ? "low" : "high";
}

const PRIORITY_RANK: Record<MemoryPriority, number> = { low: 0, high: 1 };

export interface MemoryPoolEntryStats {
  entries: number;
  bytes: number;
}

export interface MemoryPoolStats {
  capBytes: number;
  totalBytes: number;
  pinnedEntries: number;
  byKind: Partial<Record<MemoryPoolKind, MemoryPoolEntryStats>>;
}

interface Entry<T> {
  kind: MemoryPoolKind;
  value: T;
  bytes: number;
  lastUsed: number;
}

/** Called with each key the pool unloaded, after it is gone. */
export type MemoryPoolUnloadListener = (kind: MemoryPoolKind, key: string) => void;

export interface MemoryPool {
  /** The cached value, marked as just used. */
  get<T>(kind: MemoryPoolKind, key: string): T | undefined;
  /** The cached value without counting it as a use. */
  peek<T>(kind: MemoryPoolKind, key: string): T | undefined;
  has(kind: MemoryPoolKind, key: string): boolean;
  /** Stores a value as just used, replacing any earlier one, then enforces the cap. */
  set<T>(kind: MemoryPoolKind, key: string, value: T, bytes: number): void;
  /** Re-counts an entry's bytes without counting it as a use, then enforces the cap. */
  resize(kind: MemoryPoolKind, key: string, bytes: number): void;
  /** Marks an entry as just used. */
  touch(kind: MemoryPoolKind, key: string): void;
  delete(kind: MemoryPoolKind, key: string): void;
  keys(kind: MemoryPoolKind): string[];
  /** Keeps the entry loaded until the returned release runs, loaded yet or not. Pins nest. */
  pin(kind: MemoryPoolKind, key: string): () => void;
  onUnload(listener: MemoryPoolUnloadListener): () => void;
  stats(): MemoryPoolStats;
}

export function createMemoryPool(capBytes = MEMORY_POOL_CAP_BYTES): MemoryPool {
  const entries = new Map<string, Entry<unknown>>();
  const pins = new Map<string, number>();
  const listeners = new Set<MemoryPoolUnloadListener>();
  let total = 0;
  let clock = 0;

  const id = (kind: MemoryPoolKind, key: string) => `${kind}\u0000${key}`;
  const keyOf = (poolId: string) => poolId.slice(poolId.indexOf("\u0000") + 1);

  const remove = (poolId: string): Entry<unknown> | undefined => {
    const entry = entries.get(poolId);
    if (!entry) return undefined;
    entries.delete(poolId);
    total -= entry.bytes;
    return entry;
  };

  /** Unloads unpinned entries, lowest priority and least recently used first. */
  const enforce = () => {
    if (total <= capBytes) return;
    const candidates = [...entries.entries()]
      .filter(([poolId]) => !pins.has(poolId))
      .sort(([, left], [, right]) => (
        PRIORITY_RANK[memoryPriority(left.kind)] - PRIORITY_RANK[memoryPriority(right.kind)]
        || left.lastUsed - right.lastUsed
      ));
    const unloaded: Array<[MemoryPoolKind, string]> = [];
    for (const [poolId, entry] of candidates) {
      if (total <= capBytes) break;
      remove(poolId);
      unloaded.push([entry.kind, keyOf(poolId)]);
    }
    for (const [kind, key] of unloaded) {
      for (const listener of [...listeners]) listener(kind, key);
    }
  };

  return {
    get<T>(kind: MemoryPoolKind, key: string) {
      const entry = entries.get(id(kind, key));
      if (!entry) return undefined;
      entry.lastUsed = ++clock;
      return entry.value as T;
    },
    peek<T>(kind: MemoryPoolKind, key: string) {
      return entries.get(id(kind, key))?.value as T | undefined;
    },
    has: (kind, key) => entries.has(id(kind, key)),
    set(kind, key, value, bytes) {
      const poolId = id(kind, key);
      remove(poolId);
      const size = Math.max(0, bytes);
      entries.set(poolId, { kind, value, bytes: size, lastUsed: ++clock });
      total += size;
      enforce();
    },
    resize(kind, key, bytes) {
      const entry = entries.get(id(kind, key));
      if (!entry) return;
      const size = Math.max(0, bytes);
      total += size - entry.bytes;
      entry.bytes = size;
      enforce();
    },
    touch(kind, key) {
      const entry = entries.get(id(kind, key));
      if (entry) entry.lastUsed = ++clock;
    },
    delete(kind, key) {
      remove(id(kind, key));
    },
    keys(kind) {
      const prefix = `${kind}\u0000`;
      return [...entries.keys()].filter((poolId) => poolId.startsWith(prefix)).map(keyOf);
    },
    pin(kind, key) {
      const poolId = id(kind, key);
      pins.set(poolId, (pins.get(poolId) ?? 0) + 1);
      let released = false;
      return () => {
        if (released) return;
        released = true;
        const count = (pins.get(poolId) ?? 1) - 1;
        if (count <= 0) pins.delete(poolId);
        else pins.set(poolId, count);
        enforce();
      };
    },
    onUnload(listener) {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    stats() {
      const byKind: MemoryPoolStats["byKind"] = {};
      for (const entry of entries.values()) {
        const usage = byKind[entry.kind] ?? { entries: 0, bytes: 0 };
        usage.entries += 1;
        usage.bytes += entry.bytes;
        byKind[entry.kind] = usage;
      }
      return { capBytes, totalBytes: total, pinnedEntries: pins.size, byKind };
    }
  };
}

const pendingFetches = new Map<string, Promise<string>>();

/** The pool every renderer cache shares. */
export const memoryPool: MemoryPool = createMemoryPool();

/**
 * Reads through the pool: the cached promise, or `load`'s, which is kept once
 * it resolves (its size is known then) and forgotten if it fails. Concurrent
 * readers of one key share a single load.
 */
export function pooledFetch(
  pool: MemoryPool,
  kind: MemoryPoolKind,
  key: string,
  load: () => Promise<string>
): Promise<string> {
  const cached = pool.get<Promise<string>>(kind, key);
  if (cached) return cached;
  const inflightKey = `${kind}\u0000${key}`;
  const inflight = pendingFetches.get(inflightKey);
  if (inflight) return inflight;
  const request = load().then(
    (value) => {
      pendingFetches.delete(inflightKey);
      // A string's characters are what it costs; data URLs are ASCII.
      pool.set(kind, key, Promise.resolve(value), value.length);
      return value;
    },
    (error) => {
      pendingFetches.delete(inflightKey);
      throw error;
    }
  );
  pendingFetches.set(inflightKey, request);
  return request;
}
