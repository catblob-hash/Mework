import type { KeyboardEvent, ReactNode } from "react";
import { useI18n } from "../i18n";
import type { LockTone } from "../lib/toolLock";
import { Switch } from "./Common";
import { LockMark } from "./LockTone";
import { findReorderDropTarget, usePointerDrag } from "./usePointerDrag";
import type { ReorderDropTarget } from "./usePointerDrag";
import "./CatalogRow.css";

/**
 * One line in a catalog the conversation composes with — a skill, an MCP server,
 * a hook, a role, a template, a preset.
 *
 * Every catalog draws the same row, so a reader learns the shape once: what the
 * row IS goes on the left (a switch that enables it, or the arrow that applies it
 * here), what the row is CALLED fills the middle, and what can be done TO it sits
 * on the right, ending in delete. A row has no frame of its own — it is chrome-
 * free until the pointer is on it, and then it takes a grey wash. It is also
 * exactly one line tall, which means the second line the old cards carried — a
 * path on disk, a message count, a model binding — rides `detail` into `title`
 * instead.
 *
 * Nothing happens when the row itself is clicked. The row body is the drag handle
 * that reorders the list, and a surface that both opens an entry on click and
 * moves it on drag can only get one of the two wrong.
 */

/**
 * The drag-sort a catalog list is under, shared by its `CatalogList` and every
 * `CatalogRow` in it.
 *
 * `listId` must be unique among the lists on screen at once: the preset editor is
 * this same pane opened in a dialog, so two skills lists can be mounted together
 * and `findReorderDropTarget` resolves a list id to the first element carrying
 * it. Callers key it by the conversation the pane is editing.
 */
export type CatalogSort = ReturnType<typeof useCatalogSort>;

export function useCatalogSort({ listId, ids, enabled = true, onReorder }: {
  listId: string;
  /** The row ids in their current visible order, for keyboard reordering. */
  ids: string[];
  /** False while a search or filter is on: adjacency means nothing then. */
  enabled?: boolean;
  onReorder: (sourceId: string, targetId: string, position: "before" | "after") => void;
}) {
  const drag = usePointerDrag<string, ReorderDropTarget>({
    getTarget: (point, id) => findReorderDropTarget(listId, id, point),
    onDrop: (id, target) => onReorder(id, target.id, target.position)
  });
  const moveByKeyboard = (id: string, direction: -1 | 1) => {
    const index = ids.indexOf(id);
    if (index < 0) return;
    const neighbour = ids[index + direction];
    if (!neighbour) return;
    onReorder(id, neighbour, direction < 0 ? "before" : "after");
  };
  return { listId, enabled, drag, moveByKeyboard };
}

/**
 * A catalog entry that is only ever on or off — a skill, an MCP server, a hook.
 *
 * It is an ordinary row whose left slot is the switch: there is nothing else to
 * do with one of these except remove it from the catalog, which is what the
 * right slot carries. The applied-entry marking is deliberately not used,
 * because the switch already says so and a second marker on every selected skill
 * would be noise rather than news.
 */
export function CatalogToggleRow({
  id,
  name,
  detail,
  badge,
  actions,
  checked,
  disabled = false,
  tone = null,
  onChange,
  sort
}: {
  /** Identity for drag-sorting. Omitted for rows that cannot be arranged. */
  id?: string;
  name: string;
  /** What the row no longer prints — a path, a reason it is inert. Goes to `title`. */
  detail?: string;
  badge?: ReactNode;
  /** What can be done to the entry itself, ending in delete. */
  actions?: ReactNode;
  checked: boolean;
  disabled?: boolean;
  /** How the conversation's lock draws the row (`LockTone.tsx`): gray cannot move, orange warns first. */
  tone?: LockTone | null;
  onChange: (checked: boolean) => void;
  sort?: CatalogSort;
}) {
  return (
    <CatalogRow
      id={id}
      name={name}
      detail={detail}
      badge={badge}
      actions={tone ? <><LockMark tone={tone} className="catalog-row__lock" />{actions}</> : actions}
      tone={tone}
      sort={sort}
      lead={(
        <Switch
          checked={checked}
          disabled={disabled || tone === "hard"}
          tone={tone === "cache" ? "cache" : undefined}
          label={name}
          onChange={onChange}
        />
      )}
    />
  );
}

/**
 * A catalog entry with a state, a name and a few actions.
 *
 * `lead` is the one control that says what this entry is to this conversation —
 * a switch for something that is on or off, an apply arrow for something that is
 * copied in — and `actions` are the things done to the entry itself, which end in
 * delete. The name between them is text, not a button.
 */
export function CatalogRow({
  id,
  name,
  detail,
  badge,
  icon,
  lead,
  actions,
  nameEditor,
  on = false,
  tone = null,
  sort
}: {
  /** Identity for drag-sorting. Omitted for rows that cannot be arranged. */
  id?: string;
  name: string;
  /** What the row no longer prints. Goes to `title` on the row. */
  detail?: string;
  badge?: ReactNode;
  icon?: ReactNode;
  /** The leading control: an on/off switch, or the button that applies this entry. */
  lead?: ReactNode;
  actions?: ReactNode;
  /**
   * A field drawn where the name is, for a row being renamed in place.
   *
   * The rest of the row stays exactly as it was — the lead control, the icon and
   * the actions all keep their columns — so renaming moves nothing and the field
   * starts where the name it replaces started. The caller is what disables the
   * action that opened it, the way the sidebar does.
   */
  nameEditor?: ReactNode;
  /** The entry this conversation currently carries, marked rather than framed. */
  on?: boolean;
  tone?: LockTone | null;
  sort?: CatalogSort;
}) {
  const { t } = useI18n();
  const sortable = Boolean(id && sort?.enabled);
  const dragging = id !== undefined && sort?.drag.activeItem === id;
  const dropTarget = id !== undefined && sort?.drag.dropTarget?.id === id
    ? ` drop-target--${sort.drag.dropTarget.position}`
    : "";
  const title = [
    detail,
    sortable && t(
      "拖动整行排序 · Alt + ↑/↓ 键盘排序",
      "Drag the row to reorder · Alt + ↑/↓ to reorder with the keyboard"
    )
  ].filter(Boolean).join("\n");

  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    if (!sortable || !id || !sort) return;
    if (event.currentTarget !== event.target) return;
    if (!event.altKey || (event.key !== "ArrowUp" && event.key !== "ArrowDown")) return;
    event.preventDefault();
    sort.moveByKeyboard(id, event.key === "ArrowUp" ? -1 : 1);
  };

  return (
    // biome-ignore lint/a11y/noNoninteractiveElementInteractions: The handlers reorder the row, they do not activate it — there is no role for "drag handle", and role="button" would promise the activation this row deliberately no longer has.
    // biome-ignore lint/a11y/noStaticElementInteractions: Same handlers; the keyboard path is Alt + arrow, announced through aria-keyshortcuts.
    <div
      className={[
        "catalog-row",
        on ? "catalog-row--on" : "",
        tone ? `catalog-row--${tone}` : "",
        nameEditor ? "catalog-row--editing" : "",
        sortable ? "sortable-surface" : "",
        dragging ? "sortable-surface--dragging" : ""
      ].filter(Boolean).join(" ") + dropTarget}
      data-sortable-id={id}
      title={title || undefined}
      tabIndex={sortable ? 0 : undefined}
      aria-keyshortcuts={sortable ? "Alt+ArrowUp Alt+ArrowDown" : undefined}
      onKeyDown={onKeyDown}
      {...(sortable && id && sort ? sort.drag.bind(id) : {})}
    >
      {lead}
      <span className="catalog-row__main">
        {icon}
        {nameEditor ?? <span className="catalog-row__name">{name}</span>}
        {badge}
      </span>
      {actions && <span className="catalog-row__actions">{actions}</span>}
    </div>
  );
}

/** The list every catalog page draws its rows into. */
export function CatalogList({ children, sort }: { children: ReactNode; sort?: CatalogSort }) {
  return (
    <div className="catalog-list" data-sortable-list={sort?.listId}>
      {children}
    </div>
  );
}
