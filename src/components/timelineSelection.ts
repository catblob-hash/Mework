import { createContext, useContext } from "react";

const NONE: ReadonlySet<string> = new Set();

/**
 * The contexts a selection box has picked out on the timeline, by id.
 *
 * Read by each card and row for itself rather than handed down as a prop: the
 * cards are memoized, and a prop threaded through every block would rerender
 * the whole transcript to light up one row.
 */
export const TimelineSelectionContext = createContext<ReadonlySet<string>>(NONE);

/** Whether the selection box has picked out the context `id`. */
export function useTimelineSelected(id: string): boolean {
  return useContext(TimelineSelectionContext).has(id);
}
