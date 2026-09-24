/**
 * Formula rendering on demand: loads MathJax the first time it is needed and
 * remembers every formula it has drawn.
 *
 * A streamed reply re-parses its whole Markdown several times a second, and every
 * parse would otherwise re-typeset every formula already on screen. Keying the
 * result by source is what makes that parse cheap again: a formula that has not
 * changed is a map lookup.
 *
 * The API is synchronous on purpose. `peekMath` answers from what is loaded now
 * and starts whatever is missing — the engine, or a font range a glyph lives in —
 * and `subscribeMath` announces when a later `peekMath` will know more. That is
 * the shape a render wants: draw what exists, and draw again when told.
 */

import type { MathConversion, MathTreeNode } from "./mathJaxEngine";

export type { MathTreeNode };

export type MathRender =
  | { status: "ready"; tree: MathTreeNode }
  | { status: "error"; message: string };

type Engine = typeof import("./mathJaxEngine");

/** Enough for a long conversation's worth of distinct formulas, and bounded. */
const CACHE_LIMIT = 4000;

const cache = new Map<string, MathRender>();
const waiting = new Set<string>();
const listeners = new Set<() => void>();
let engine: Engine | null = null;
let engineLoad: Promise<void> | null = null;
let engineFailure: string | null = null;
let version = 0;

function announce(): void {
  version += 1;
  for (const listener of listeners) listener();
}

function remember(key: string, render: MathRender): MathRender {
  // Re-inserting moves the entry to the end, so the map's order is recency and the
  // first key is the one to drop.
  cache.delete(key);
  cache.set(key, render);
  if (cache.size > CACHE_LIMIT) {
    const oldest = cache.keys().next().value;
    if (oldest !== undefined) cache.delete(oldest);
  }
  return render;
}

/** Starts loading the engine if nothing has yet; safe to call as often as a render likes. */
export function loadMathEngine(): Promise<void> {
  if (engineLoad) return engineLoad;
  engineLoad = import("./mathJaxEngine").then((loaded) => {
    engine = loaded;
  }).catch((error: unknown) => {
    engineFailure = error instanceof Error ? error.message : String(error);
  }).finally(announce);
  return engineLoad;
}

function keyFor(source: string, display: boolean): string {
  return `${display ? "D" : "I"}${source}`;
}

/**
 * What can be drawn for this formula right now, or null while that is not known.
 *
 * Null starts the work that will answer it; `subscribeMath` fires when it has.
 */
export function peekMath(source: string, display: boolean): MathRender | null {
  const key = keyFor(source, display);
  const cached = cache.get(key);
  if (cached) return cached;
  if (engineFailure !== null) return { status: "error", message: engineFailure };
  if (!engine) {
    void loadMathEngine();
    return null;
  }
  const result: MathConversion = engine.convertTeX(source, display);
  if (result.status !== "retry") return remember(key, result);
  if (!waiting.has(key)) {
    waiting.add(key);
    result.ready.catch(() => undefined).finally(() => {
      waiting.delete(key);
      announce();
    });
  }
  return null;
}

/** Called whenever a formula that answered null may now answer something. */
export function subscribeMath(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

/** Changes whenever `subscribeMath` fires; the snapshot `useSyncExternalStore` compares. */
export function mathVersion(): number {
  return version;
}

/**
 * Resolves once this formula has an answer.
 *
 * For callers that are not renders — tests, and anything that wants the drawing
 * before showing a page — rather than for components, which subscribe instead.
 */
export async function renderMath(source: string, display: boolean): Promise<MathRender> {
  for (let attempt = 0; attempt < 64; attempt += 1) {
    const now = peekMath(source, display);
    if (now) return now;
    await new Promise<void>((resolve) => {
      const stop = subscribeMath(() => {
        stop();
        resolve();
      });
    });
  }
  return { status: "error", message: "MathJax did not finish loading this formula" };
}
