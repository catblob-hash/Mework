/**
 * `items` put into the order `ids` names, for a list the user has just rearranged by hand.
 *
 * The request comes from a view that can be a render behind the list it rearranges: a tab that
 * closed while another was being dragged, or one that opened meanwhile. So an id the list does not
 * hold is ignored, a repeated id counts once, and an item the request leaves out keeps its place
 * relative to the other left-out items, after every named one — a stale request can neither drop
 * nor invent an item. Null when the order would not change, so a reducer can hand back the state
 * it was given.
 */
export function arrangeByIds<T>(
  items: readonly T[],
  ids: readonly string[],
  getId: (item: T) => string
): T[] | null {
  const byId = new Map(items.map((item) => [getId(item), item]));
  const placed = new Set<string>();
  const next: T[] = [];
  for (const id of ids) {
    if (!byId.has(id) || placed.has(id)) continue;
    placed.add(id);
    next.push(byId.get(id) as T);
  }
  for (const item of items) {
    if (!placed.has(getId(item))) next.push(item);
  }
  return next.every((item, index) => item === items[index]) ? null : next;
}
