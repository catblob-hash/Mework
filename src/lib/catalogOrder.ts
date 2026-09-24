import { useCallback, useMemo, useRef, useState } from "react";
import { reorderItems } from "../components/usePointerDrag";

/**
 * Where the user put the rows of a catalog this pane cannot write to.
 *
 * Skills, MCP servers and hooks are scanned off disk; templates and presets are
 * owned by the host document and carry no sort field. None of them has anywhere
 * to keep a position, so the order the user drags them into is a view preference
 * rather than data — the same class of thing as the sidebar width — and lives in
 * `localStorage` beside those. A row the catalog gains later is not in the stored
 * order and sits at the end in catalog order; a row it loses simply drops out.
 *
 * Roles are deliberately NOT here: their array is writable from the pane and the
 * model reads it in order, so dragging a role rewrites the real list instead.
 */
const STORAGE_KEY = "mework.catalog-order.v1";

export type CatalogOrderScope = "skills" | "mcp" | "hooks" | "templates" | "presets";

type StoredOrders = Partial<Record<CatalogOrderScope, string[]>>;

function readStored(): StoredOrders {
  if (typeof window === "undefined") return {};
  try {
    const raw = window.localStorage.getItem(STORAGE_KEY);
    if (!raw) return {};
    const parsed: unknown = JSON.parse(raw);
    if (!parsed || typeof parsed !== "object") return {};
    return parsed as StoredOrders;
  } catch {
    return {};
  }
}

function readCatalogOrder(scope: CatalogOrderScope): string[] {
  const stored = readStored()[scope];
  return Array.isArray(stored) ? stored.filter((id): id is string => typeof id === "string") : [];
}

function writeCatalogOrder(scope: CatalogOrderScope, ids: string[]): void {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.setItem(STORAGE_KEY, JSON.stringify({ ...readStored(), [scope]: ids }));
  } catch {
    // A full or unavailable store costs the arrangement, never the catalog.
  }
}

function sortByOrder<T>(items: T[], order: string[], getId: (item: T) => string): T[] {
  if (!order.length) return items;
  const rank = new Map(order.map((id, index) => [id, index]));
  /* Stable by construction: an unranked row keeps its catalog position relative
     to the other unranked rows, and the whole unranked group follows the ranked
     one rather than being scattered through it. */
  return items
    .map((item, index) => ({ item, index, rank: rank.get(getId(item)) }))
    .sort((left, right) => {
      if (left.rank === undefined && right.rank === undefined) return left.index - right.index;
      if (left.rank === undefined) return 1;
      if (right.rank === undefined) return -1;
      return left.rank - right.rank;
    })
    .map((entry) => entry.item);
}

/**
 * A catalog list in the order the user arranged it, and the one call that
 * rearranges it.
 *
 * The stored order is rewritten from the whole visible list on every drop, so it
 * re-anchors to what the catalog actually holds instead of accumulating ids of
 * things that are long gone.
 */
export function useCatalogOrder<T>(
  scope: CatalogOrderScope,
  items: T[],
  getId: (item: T) => string
): { ordered: T[]; reorder: (sourceId: string, targetId: string, position: "before" | "after") => void } {
  const [order, setOrder] = useState(() => readCatalogOrder(scope));
  const getIdRef = useRef(getId);
  getIdRef.current = getId;

  const ordered = useMemo(() => sortByOrder(items, order, getIdRef.current), [items, order]);
  const orderedRef = useRef(ordered);
  orderedRef.current = ordered;

  const reorder = useCallback((
    sourceId: string,
    targetId: string,
    position: "before" | "after"
  ) => {
    const next = reorderItems(orderedRef.current, sourceId, targetId, position, getIdRef.current)
      .map(getIdRef.current);
    setOrder(next);
    writeCatalogOrder(scope, next);
  }, [scope]);

  return { ordered, reorder };
}
