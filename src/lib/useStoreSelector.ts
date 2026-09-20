import { useRef, useSyncExternalStore } from "react";

interface SelectorCache<S, T> {
  state: S;
  selector: (state: S) => T;
  selected: T;
}

/**
 * Subscribe to an external store slice. Components re-render only when the
 * `selector` result changes under `isEqual`; equal values retain object identity.
 *
 * The selector may change on each render to capture render-time values. Store
 * notifications use the most recently committed selector.
 */
export function useStoreSelector<S, T>(
  subscribe: (listener: () => void) => () => void,
  getState: () => S,
  selector: (state: S) => T,
  isEqual: (previous: T, next: T) => boolean = Object.is
): T {
  const cacheRef = useRef<SelectorCache<S, T> | null>(null);
  const getSnapshot = () => {
    const state = getState();
    const cached = cacheRef.current;
    if (cached && cached.state === state && cached.selector === selector) {
      return cached.selected;
    }
    const selected = selector(state);
    const kept = cached && isEqual(cached.selected, selected) ? cached.selected : selected;
    cacheRef.current = { state, selector, selected: kept };
    return kept;
  };
  return useSyncExternalStore(subscribe, getSnapshot);
}
