import {
  createContext,
  Fragment,
  type ReactNode,
  useContext,
  useId,
  useLayoutEffect,
  useState,
  useSyncExternalStore
} from "react";

/**
 * Windows mounted at the application root, whichever component opens them.
 *
 * Global settings is a `Dialog` rendered by `App` itself. A preset's window and a
 * role's window are opened from deep inside the conversation-settings side pane
 * (and a role's from inside a preset's window), so rendered in place they would
 * be that pane's React children: every event inside them bubbling through it,
 * every render of it re-rendering them. They are meant to be the same window as
 * global settings, so they are mounted where it is — the opener keeps the state
 * and builds the window, and `WindowLayerOutlet`, placed beside global settings,
 * renders it.
 *
 * Without a provider (a component rendered on its own, as in its tests) a
 * `HostedWindow` renders in place.
 */
interface WindowStore {
  subscribe: (listener: () => void) => () => void;
  snapshot: () => ReadonlyArray<readonly [string, ReactNode]>;
  set: (id: string, node: ReactNode) => void;
  remove: (id: string) => void;
}

function createWindowStore(): WindowStore {
  let entries: ReadonlyArray<readonly [string, ReactNode]> = [];
  const listeners = new Set<() => void>();
  const emit = () => { for (const listener of listeners) listener(); };
  return {
    subscribe: (listener) => {
      listeners.add(listener);
      return () => { listeners.delete(listener); };
    },
    snapshot: () => entries,
    // In place when already open, so a window keeps its stacking order, and at
    // the end when new, so a window opened over another is drawn over it.
    set: (id, node) => {
      const index = entries.findIndex(([key]) => key === id);
      entries = index === -1
        ? [...entries, [id, node]]
        : entries.map((entry, at) => (at === index ? [id, node] as const : entry));
      emit();
    },
    remove: (id) => {
      if (!entries.some(([key]) => key === id)) return;
      entries = entries.filter(([key]) => key !== id);
      emit();
    }
  };
}

const WindowLayerContext = createContext<WindowStore | null>(null);

export function WindowLayerProvider({ children }: { children: ReactNode }) {
  const [store] = useState(createWindowStore);
  return <WindowLayerContext.Provider value={store}>{children}</WindowLayerContext.Provider>;
}

/** Where hosted windows are mounted: beside global settings. */
export function WindowLayerOutlet() {
  const store = useContext(WindowLayerContext);
  const entries = useSyncExternalStore(
    store?.subscribe ?? noSubscription,
    store?.snapshot ?? noEntries
  );
  return <>{entries.map(([id, node]) => <Fragment key={id}>{node}</Fragment>)}</>;
}

/**
 * A window its opener owns but the application root mounts. Mount it while the
 * window is open and unmount it to close; its children are handed over on every
 * render, in a layout effect, so the window is current before the frame paints.
 */
export function HostedWindow({ children }: { children: ReactNode }) {
  const store = useContext(WindowLayerContext);
  const id = useId();
  useLayoutEffect(() => {
    store?.set(id, children);
  });
  useLayoutEffect(() => () => store?.remove(id), [store, id]);
  return store ? null : children;
}

const NO_ENTRIES: ReadonlyArray<readonly [string, ReactNode]> = [];
const noEntries = () => NO_ENTRIES;
const noSubscription = () => () => undefined;
