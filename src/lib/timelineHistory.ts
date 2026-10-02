import type { ContextItem } from "../types";

/**
 * Undo and redo for edits made on a timeline.
 *
 * Each conversation keeps its own two stacks, so an undo only ever reaches the
 * timeline it was pressed in; the stacks are keyed by conversation id, which is
 * also why a fork or a hand-over starts with none — it is a new id. They live in
 * memory only: nothing is written to disk, and a restart forgets them.
 *
 * The timelines share one budget of {@link TIMELINE_HISTORY_LIMIT} entries,
 * counting both stacks. Past it, the entry touched longest ago goes, whichever
 * conversation it belongs to. Every push takes a fresh tick and every stack
 * grows at its top, so that entry is always at the bottom of some stack: the
 * oldest edit there is still to undo, or the last one there is to redo.
 *
 * An edit is recorded as a patch — what it took out, put in and rewrote, each
 * by id — rather than as two snapshots of the whole list. A model run appends to
 * the same list between an edit and its undo, and restoring a snapshot would
 * take those replies away with it.
 */

const TIMELINE_HISTORY_LIMIT = 64;

/** A context as it sat in a list: where to put it back. */
export interface PlacedContext {
  context: ContextItem;
  /** Its position in that list, the fallback when its neighbour is gone. */
  index: number;
  /** The context right before it, or null at the head of the list. */
  afterId: string | null;
}

/** One edit on a timeline, readable in both directions. */
export interface TimelinePatch {
  /** Contexts the edit took out, as they sat before it. */
  removed: PlacedContext[];
  /** Contexts the edit put in, as they sit after it. */
  inserted: PlacedContext[];
  /** Contexts the edit rewrote in place. */
  replaced: Array<{ before: ContextItem; after: ContextItem }>;
}

export const EMPTY_TIMELINE_PATCH: TimelinePatch = { removed: [], inserted: [], replaced: [] };

/** Where `ids` sit in `contexts`, in list order, with what to put each one back after. */
export function placedContexts(contexts: readonly ContextItem[], ids: ReadonlySet<string>): PlacedContext[] {
  return contexts.flatMap((context, index) => (
    ids.has(context.id)
      ? [{ context, index, afterId: index > 0 ? contexts[index - 1].id : null }]
      : []
  ));
}

/** The patch for putting `context` at `index` of `contexts`. */
export function insertionPatch(contexts: readonly ContextItem[], context: ContextItem, index: number): TimelinePatch {
  const at = Math.max(0, Math.min(index, contexts.length));
  return {
    ...EMPTY_TIMELINE_PATCH,
    inserted: [{ context, index: at, afterId: at > 0 ? contexts[at - 1].id : null }]
  };
}

/** The same edit, the other way round. */
export function invertTimelinePatch(patch: TimelinePatch): TimelinePatch {
  return {
    removed: patch.inserted,
    inserted: patch.removed,
    replaced: patch.replaced.map(({ before, after }) => ({ before: after, after: before }))
  };
}

/** The ids a patch takes out of a list. */
export function removedIds(patch: TimelinePatch): string[] {
  return patch.removed.map((entry) => entry.context.id);
}

/**
 * Applies a patch to `contexts`, or returns null when none of it lands any more:
 * everything it would take out is already gone, everything it would rewrite has
 * been deleted, and everything it would put back is already there.
 *
 * What does land, lands by id. A context comes back after the neighbour it had,
 * wherever that neighbour is now, and only falls back to its old position when
 * the neighbour is gone too. Contexts put back together go in list order, so one
 * restored context can be the neighbour of the next.
 */
export function applyTimelinePatch(contexts: readonly ContextItem[], patch: TimelinePatch): ContextItem[] | null {
  let changed = false;
  const dropped = new Set(removedIds(patch));
  let next = contexts.filter((context) => {
    if (!dropped.has(context.id)) return true;
    changed = true;
    return false;
  });
  const rewrites = new Map(patch.replaced.map(({ after }) => [after.id, after]));
  next = next.map((context) => {
    const rewrite = rewrites.get(context.id);
    if (!rewrite || rewrite === context) return context;
    changed = true;
    return rewrite;
  });
  const present = new Set(next.map((context) => context.id));
  for (const entry of [...patch.inserted].sort((left, right) => left.index - right.index)) {
    if (present.has(entry.context.id)) continue;
    const anchor = entry.afterId === null ? -1 : next.findIndex((context) => context.id === entry.afterId);
    const at = entry.afterId === null
      ? 0
      : anchor >= 0 ? anchor + 1 : Math.min(entry.index, next.length);
    next = [...next.slice(0, at), entry.context, ...next.slice(at)];
    present.add(entry.context.id);
    changed = true;
  }
  return changed ? next : null;
}

export interface TimelineHistoryEntry {
  patch: TimelinePatch;
  /** What the edit did, in words, for the notice an undo or a redo shows. */
  label: string;
}

interface StoredEntry extends TimelineHistoryEntry {
  tick: number;
}

interface Stacks {
  undo: StoredEntry[];
  redo: StoredEntry[];
}

export class TimelineHistory {
  private readonly stacks = new Map<string, Stacks>();
  private clock = 0;

  constructor(private readonly limit = TIMELINE_HISTORY_LIMIT) {}

  /** Records an edit made on `key`'s timeline. Whatever was there to redo is gone. */
  record(key: string, entry: TimelineHistoryEntry): void {
    const stacks = this.stacksOf(key);
    stacks.undo.push({ ...entry, tick: this.tick() });
    stacks.redo = [];
    this.trim();
  }

  /** The edit an undo would revert, still in place. */
  nextUndo(key: string): TimelineHistoryEntry | null {
    return this.stacks.get(key)?.undo.at(-1) ?? null;
  }

  /** The edit a redo would make again, still in place. */
  nextRedo(key: string): TimelineHistoryEntry | null {
    return this.stacks.get(key)?.redo.at(-1) ?? null;
  }

  /** The last edit was reverted: it moves over to be redone. */
  undone(key: string): void {
    const stacks = this.stacks.get(key);
    const entry = stacks?.undo.pop();
    if (!stacks || !entry) return;
    stacks.redo.push({ ...entry, tick: this.tick() });
  }

  /** The last reverted edit was made again: it moves back to be undone. */
  redone(key: string): void {
    const stacks = this.stacks.get(key);
    const entry = stacks?.redo.pop();
    if (!stacks || !entry) return;
    stacks.undo.push({ ...entry, tick: this.tick() });
  }

  /** Drops the next undo without applying it: it no longer lands anywhere. */
  dropUndo(key: string): void {
    this.stacks.get(key)?.undo.pop();
  }

  /** Drops the next redo without applying it. */
  dropRedo(key: string): void {
    this.stacks.get(key)?.redo.pop();
  }

  /** Forgets everything recorded for `key`. */
  forget(key: string): void {
    this.stacks.delete(key);
  }

  /** Entries held across every timeline. */
  get size(): number {
    let size = 0;
    for (const stacks of this.stacks.values()) size += stacks.undo.length + stacks.redo.length;
    return size;
  }

  private stacksOf(key: string): Stacks {
    let stacks = this.stacks.get(key);
    if (!stacks) {
      stacks = { undo: [], redo: [] };
      this.stacks.set(key, stacks);
    }
    return stacks;
  }

  private tick(): number {
    this.clock += 1;
    return this.clock;
  }

  private trim(): void {
    while (this.size > this.limit) {
      let oldest: { stack: StoredEntry[]; key: string } | null = null;
      let oldestTick = Number.POSITIVE_INFINITY;
      for (const [key, stacks] of this.stacks) {
        for (const stack of [stacks.undo, stacks.redo]) {
          const bottom = stack[0];
          if (bottom && bottom.tick < oldestTick) {
            oldestTick = bottom.tick;
            oldest = { stack, key };
          }
        }
      }
      if (!oldest) return;
      oldest.stack.shift();
      const stacks = this.stacks.get(oldest.key);
      if (stacks && !stacks.undo.length && !stacks.redo.length) this.stacks.delete(oldest.key);
    }
  }
}

const isApplePlatform = typeof navigator !== "undefined" && /^(Mac|iPhone|iPad)/.test(navigator.platform ?? "");

/**
 * Whether a keydown is the timeline's undo or redo: Ctrl+Z and Ctrl+X, with ⌘ in
 * place of Ctrl on a Mac, where Ctrl+click is a right-click and ⌘ is what every
 * other shortcut there is held with.
 */
export function timelineHistoryKey(event: Pick<KeyboardEvent, "key" | "ctrlKey" | "metaKey" | "altKey" | "shiftKey">): "undo" | "redo" | null {
  if (event.altKey || event.shiftKey) return null;
  const primary = isApplePlatform ? event.metaKey && !event.ctrlKey : event.ctrlKey && !event.metaKey;
  if (!primary) return null;
  const key = event.key.toLowerCase();
  if (key === "z") return "undo";
  if (key === "x") return "redo";
  return null;
}

/** Whether a press holds the modifier that draws a selection box over the timeline. */
export function timelineSelectionModifier(event: Pick<MouseEvent, "ctrlKey" | "metaKey">): boolean {
  return isApplePlatform ? event.metaKey : event.ctrlKey;
}

/** The keycaps of the timeline's undo or redo, as this platform writes them. */
export function timelineHistoryShortcut(action: "undo" | "redo"): string {
  const letter = action === "undo" ? "Z" : "X";
  return isApplePlatform ? `⌘${letter}` : `Ctrl+${letter}`;
}
