import type { SketchStroke } from "./strokes";

export interface SketchHistory {
  past: SketchStroke[][];
  present: SketchStroke[];
  future: SketchStroke[][];
}

export interface SketchHistoryState {
  count: number;
  canUndo: boolean;
  canRedo: boolean;
}

export const SKETCH_HISTORY_LIMIT = 100;

export const EMPTY_SKETCH_HISTORY: SketchHistory = { past: [], present: [], future: [] };

export const EMPTY_SKETCH_HISTORY_STATE: SketchHistoryState = { count: 0, canUndo: false, canRedo: false };

export function pushHistory(history: SketchHistory, present: SketchStroke[]): SketchHistory {
  const past = [...history.past, history.present];
  return {
    past: past.length > SKETCH_HISTORY_LIMIT ? past.slice(-SKETCH_HISTORY_LIMIT) : past,
    present,
    future: []
  };
}

/**
 * Undo past the cap still removes the newest stroke: once `past` has been trimmed there is no
 * recorded predecessor, so the present is peeled back one entry rather than restored wholesale.
 */
export function undoHistory(history: SketchHistory): SketchHistory {
  const previous = history.past.at(-1);
  if (previous === undefined) {
    if (history.present.length === 0) return history;
    return { past: [], present: history.present.slice(0, -1), future: [history.present, ...history.future] };
  }
  return { past: history.past.slice(0, -1), present: previous, future: [history.present, ...history.future] };
}

export function redoHistory(history: SketchHistory): SketchHistory {
  const [next, ...rest] = history.future;
  if (next === undefined) return history;
  return { past: [...history.past, history.present], present: next, future: rest };
}

export function historyState(history: SketchHistory): SketchHistoryState {
  return {
    count: history.present.length,
    canUndo: history.past.length > 0 || history.present.length > 0,
    canRedo: history.future.length > 0
  };
}
