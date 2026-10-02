import { Fragment, useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import type { CSSProperties, KeyboardEvent as ReactKeyboardEvent, PointerEvent as ReactPointerEvent, ReactNode } from "react";
import { useI18n } from "../i18n";
import { CHAT_TILE_FLEX, defaultSideFlexForKind, MAX_SIDE_FLEX, MIN_SIDE_FLEX, NEW_COLUMN_FLEX, paneKind } from "../lib/sidePanes";
import type { SidePaneId } from "../lib/sidePanes";
import { PaneTileGeometryContext } from "./paneTileGeometry";
import "./PaneTiles.css";

export interface PaneTilesProps {
  chat: ReactNode;
  /**
   * Every pane to draw. The tiles are siblings in one element, placed by style alone, so a pane
   * that moves to another column keeps its React identity — its surface may own a live host
   * session whose teardown would be reported as a stop. A `hidden` entry stays mounted too, but
   * takes no place, no handle and no part in the resize arithmetic.
   */
  panes: { id: SidePaneId; node: ReactNode; hidden?: boolean }[];
  /**
   * The side area's columns, left → right, each top → bottom. A column keeps its index into
   * `columnFlex` even while none of its panes is drawn, and a visible pane no column names gets
   * a column of its own on the right. Absent, the visible panes stack in a single column.
   */
  columns?: readonly (readonly SidePaneId[])[];
  sideFlex: number;
  /** The columns' widths relative to one another, index for index; a missing weight is 1. */
  columnFlex?: readonly number[];
  paneFlex: Record<string, number>;
  /**
   * Shown alone over the whole tile area, chat column included. Every other tile and the chat
   * keep their box and stay mounted — a pane may own a live shell or a native page — but are
   * made invisible and inert.
   */
  expanded?: SidePaneId | null;
  onSideFlexChange: (sideFlex: number) => void;
  /** The whole weight list after a divider between columns moved, indexed like `columns`. */
  onColumnFlexChange?: (columnFlex: number[]) => void;
  onPaneFlexChange: (paneFlex: Record<string, number>) => void;
  onResizeStateChange?: (resizing: boolean) => void;
}

interface ResizeMeasure {
  apply: (delta: number) => void;
  restore: () => void;
  minimum: number;
  maximum: number;
}

interface ResizeSession extends ResizeMeasure {
  pointerId: number;
  start: number;
  horizontal: boolean;
  element: HTMLDivElement;
  layoutKey: string;
}

interface TileColumn {
  /** The column's index in `columns`, which `columnFlex` follows. */
  index: number;
  weight: number;
  panes: SidePaneId[];
}

interface Share {
  start: number;
  size: number;
}

/** The gutter between tiles; the stylesheet's placement formulas spell the same 12px. */
const GAP = 12;
const MIN_CHAT_WIDTH = 320;
const MIN_SIDE_WIDTH = 280;
const MIN_COLUMN_WIDTH = 200;
const MIN_TILE_HEIGHT = 100;

/** The narrowest the side area may be dragged while it shows `columns` columns. */
function minimumSideWidth(columns: number): number {
  return Math.max(MIN_SIDE_WIDTH, columns * MIN_COLUMN_WIDTH + (columns - 1) * GAP);
}

function clamp(value: number, min: number, max: number): number {
  return Math.max(min, Math.min(max, value));
}

function weightOf(value: number | undefined): number {
  return typeof value === "number" && Number.isFinite(value) && value > 0 ? value : 1;
}

function sharesOf(weights: readonly number[]): Share[] {
  const total = weights.reduce((sum, weight) => sum + weight, 0);
  let start = 0;
  return weights.map((weight) => {
    const share = { start: start / total, size: weight / total };
    start += weight;
    return share;
  });
}

function fraction(value: number): string {
  return String(Math.round(value * 1e6) / 1e6);
}

/** Groups the visible panes into the columns they are drawn in, dropping columns with none. */
function arrangeColumns(
  panes: PaneTilesProps["panes"],
  columns: PaneTilesProps["columns"],
  columnFlex: PaneTilesProps["columnFlex"]
): TileColumn[] {
  const visible = new Set(panes.filter((pane) => !pane.hidden).map((pane) => pane.id));
  const placed = new Set<SidePaneId>();
  const arranged: TileColumn[] = [];
  (columns ?? []).forEach((column, index) => {
    const ids = column.filter((id) => visible.has(id) && !placed.has(id));
    for (const id of ids) placed.add(id);
    if (ids.length) arranged.push({ index, weight: weightOf(columnFlex?.[index]), panes: ids });
  });
  const rest = panes.filter((pane) => !pane.hidden && !placed.has(pane.id)).map((pane) => pane.id);
  if (rest.length) arranged.push({ index: columns?.length ?? 0, weight: weightOf(columnFlex?.[columns?.length ?? 0]), panes: rest });
  return arranged;
}

export function PaneTiles({
  chat, panes, columns, sideFlex, columnFlex, paneFlex, expanded = null,
  onSideFlexChange, onColumnFlexChange, onPaneFlexChange, onResizeStateChange
}: PaneTilesProps) {
  const { t } = useI18n();
  const chatRef = useRef<HTMLDivElement>(null);
  const sideRef = useRef<HTMLDivElement>(null);
  const tilesRef = useRef(new Map<SidePaneId, HTMLDivElement>());
  const sessionRef = useRef<ResizeSession | null>(null);
  const [activeHandle, setActiveHandle] = useState<HTMLDivElement | null>(null);
  const callbacks = useRef({ onSideFlexChange, onColumnFlexChange, onPaneFlexChange, onResizeStateChange, paneFlex });
  useLayoutEffect(() => {
    callbacks.current = { onSideFlexChange, onColumnFlexChange, onPaneFlexChange, onResizeStateChange, paneFlex };
  });

  const visiblePanes = panes.filter((pane) => !pane.hidden);
  // A pane whose subject vanished renders nothing and is not in `panes`; expanding it would
  // leave an empty workspace, so an unrenderable id means no expansion at all.
  const solo = expanded !== null && visiblePanes.some((pane) => pane.id === expanded) ? expanded : null;
  const arranged = arrangeColumns(panes, columns, columnFlex);
  const columnShares = sharesOf(arranged.map((column) => column.weight));
  const rowShares = arranged.map((column) => sharesOf(column.panes.map((id) => weightOf(paneFlex[id]))));
  const places = new Map<SidePaneId, { column: number; row: number }>();
  arranged.forEach((column, columnIndex) => {
    column.panes.forEach((id, row) => { places.set(id, { column: columnIndex, row }); });
  });
  const layoutKey = JSON.stringify([panes.map((pane) => [pane.id, Boolean(pane.hidden)]), solo, arranged.map((column) => column.panes)]);

  const finishResize = useCallback((event?: PointerEvent, unmount = false) => {
    const session = sessionRef.current;
    if (!session || (event && event.pointerId !== session.pointerId)) return;
    sessionRef.current = null;
    try {
      if (session.element.hasPointerCapture?.(session.pointerId)) {
        session.element.releasePointerCapture(session.pointerId);
      }
    } catch {
      // Window-level listeners still complete the resize when capture is unavailable.
    }
    document.body.classList.remove("pane-tiles-resize-active", "pane-tiles-resize-active--column");
    session.element.classList.remove("is-active");
    if (!unmount) setActiveHandle(null);
    callbacks.current.onResizeStateChange?.(false);
  }, []);

  useLayoutEffect(() => {
    const session = sessionRef.current;
    if (session && (!session.element.isConnected || session.layoutKey !== layoutKey)) finishResize();
  });

  useEffect(() => {
    const loseCapture = (event: PointerEvent) => {
      if (event.target === sessionRef.current?.element) finishResize(event);
    };
    const moveResize = (event: PointerEvent) => {
      const session = sessionRef.current;
      if (!session || event.pointerId !== session.pointerId) return;
      if (event.cancelable) event.preventDefault();
      session.apply((session.horizontal ? event.clientX : event.clientY) - session.start);
    };
    const cancelWithEscape = (event: KeyboardEvent) => {
      const session = sessionRef.current;
      if (!session || event.key !== "Escape") return;
      event.preventDefault();
      event.stopPropagation();
      session.restore();
      finishResize();
    };
    window.addEventListener("pointermove", moveResize, { passive: false });
    window.addEventListener("pointerup", finishResize);
    window.addEventListener("pointercancel", finishResize);
    window.addEventListener("lostpointercapture", loseCapture, true);
    window.addEventListener("keydown", cancelWithEscape, true);
    return () => {
      window.removeEventListener("pointermove", moveResize);
      window.removeEventListener("pointerup", finishResize);
      window.removeEventListener("pointercancel", finishResize);
      window.removeEventListener("lostpointercapture", loseCapture, true);
      window.removeEventListener("keydown", cancelWithEscape, true);
      finishResize(undefined, true);
    };
  }, [finishResize]);

  const measureSide = (): ResizeMeasure | null => {
    const chatWidth = chatRef.current?.getBoundingClientRect().width ?? 0;
    const sideWidth = sideRef.current?.getBoundingClientRect().width ?? 0;
    const sideMinimum = minimumSideWidth(arranged.length);
    if (chatWidth < MIN_CHAT_WIDTH || sideWidth < sideMinimum) return null;
    const minimum = MIN_CHAT_WIDTH - chatWidth;
    const maximum = sideWidth - sideMinimum;
    return {
      minimum, maximum,
      apply: (delta) => {
        // The chat tile gives up exactly the pixels the side area gains, so the ratio must use
        // both post-drag widths or the handle drifts away from the pointer.
        const moved = clamp(delta, minimum, maximum);
        callbacks.current.onSideFlexChange(clamp(
          (sideWidth - moved) / (chatWidth + moved) * CHAT_TILE_FLEX,
          MIN_SIDE_FLEX, MAX_SIDE_FLEX
        ));
      },
      restore: () => callbacks.current.onSideFlexChange(sideFlex)
    };
  };

  /** Every column's weight as given, indexed like `columns`, the one a drawn-alone column included. */
  const givenColumnWeights = (): number[] => Array.from(
    { length: Math.max(columns?.length ?? 0, ...arranged.map((column) => column.index + 1)) },
    (_, index) => weightOf(columnFlex?.[index])
  );

  const measureColumns = (index: number): ResizeMeasure | null => {
    const first = arranged[index];
    const second = arranged[index + 1];
    const firstWidth = tilesRef.current.get(first.panes[0])?.getBoundingClientRect().width ?? 0;
    const secondWidth = tilesRef.current.get(second.panes[0])?.getBoundingClientRect().width ?? 0;
    if (firstWidth <= 0 || secondWidth <= 0) return null;
    const width = firstWidth + secondWidth;
    const sum = first.weight + second.weight;
    // A column already under its floor — the side area narrowed after the columns were set —
    // cannot be made narrower, but must still be draggable wider: refusing the whole drag left
    // the divider dead exactly when a column most needed room.
    const minimum = Math.min(0, MIN_COLUMN_WIDTH - firstWidth);
    const maximum = Math.max(0, secondWidth - MIN_COLUMN_WIDTH);
    const given = givenColumnWeights();
    return {
      minimum, maximum,
      apply: (delta) => {
        const firstWeight = (firstWidth + clamp(delta, minimum, maximum)) / width * sum;
        const next = [...given];
        next[first.index] = firstWeight;
        next[second.index] = sum - firstWeight;
        callbacks.current.onColumnFlexChange?.(next);
      },
      restore: () => callbacks.current.onColumnFlexChange?.(given)
    };
  };

  const measureRows = (columnIndex: number, index: number): ResizeMeasure | null => {
    const ids = arranged[columnIndex].panes;
    const first = ids[index];
    const second = ids[index + 1];
    const firstHeight = tilesRef.current.get(first)?.getBoundingClientRect().height ?? 0;
    const secondHeight = tilesRef.current.get(second)?.getBoundingClientRect().height ?? 0;
    if (firstHeight <= 0 || secondHeight <= 0) return null;
    const height = firstHeight + secondHeight;
    const sum = weightOf(paneFlex[first]) + weightOf(paneFlex[second]);
    // As between columns: a tile under its floor only refuses to shrink further.
    const minimum = Math.min(0, MIN_TILE_HEIGHT - firstHeight);
    const maximum = Math.max(0, secondHeight - MIN_TILE_HEIGHT);
    // A floor-constrained tile's measured height is not proportional to its flex. Use one
    // pixel-to-flex scale for the column's tiles to keep its unrelated separators fixed.
    const measuredFlex = Object.fromEntries(ids.map((id) => [
      id, (tilesRef.current.get(id)?.getBoundingClientRect().height ?? MIN_TILE_HEIGHT) / height * sum
    ]));
    return {
      minimum, maximum,
      apply: (delta) => {
        const movement = clamp(delta, minimum, maximum);
        if (movement === 0) {
          callbacks.current.onPaneFlexChange(paneFlex);
          return;
        }
        const firstFlex = (firstHeight + movement) / height * sum;
        callbacks.current.onPaneFlexChange({ ...callbacks.current.paneFlex, ...measuredFlex, [first]: firstFlex, [second]: sum - firstFlex });
      },
      restore: () => callbacks.current.onPaneFlexChange(paneFlex)
    };
  };

  const startResize = (event: ReactPointerEvent<HTMLDivElement>, horizontal: boolean, measure: ResizeMeasure | null) => {
    if (event.button !== 0 || event.isPrimary === false || sessionRef.current || !measure) return;
    event.preventDefault();
    sessionRef.current = {
      ...measure, pointerId: event.pointerId, start: horizontal ? event.clientX : event.clientY,
      horizontal, element: event.currentTarget, layoutKey
    };
    try {
      event.currentTarget.setPointerCapture?.(event.pointerId);
    } catch {
      // Some WebViews reject capture; window-level listeners keep resizing active.
    }
    document.body.classList.add("pane-tiles-resize-active");
    document.body.classList.toggle("pane-tiles-resize-active--column", !horizontal);
    setActiveHandle(event.currentTarget);
    callbacks.current.onResizeStateChange?.(true);
  };

  /**
   * Arrow keys move the handle; Home and End take the value it reports to its end. The chat's
   * handle reports the side area's share, which grows as the handle moves left, so its Home and
   * End run the other way.
   */
  const resizeWithKeyboard = (
    event: ReactKeyboardEvent<HTMLDivElement>,
    horizontal: boolean,
    measure: ResizeMeasure | null,
    reversed = false
  ) => {
    if (!measure || sessionRef.current) return;
    const step = event.shiftKey ? 48 : 24;
    const delta = event.key === (horizontal ? "ArrowLeft" : "ArrowUp") ? -step
      : event.key === (horizontal ? "ArrowRight" : "ArrowDown") ? step
        : event.key === "Home" ? (reversed ? measure.maximum : measure.minimum)
          : event.key === "End" ? (reversed ? measure.minimum : measure.maximum) : null;
    if (delta === null) return;
    event.preventDefault();
    measure.apply(delta);
  };

  const handleClass = (key: string, horizontal: boolean) => {
    const active = activeHandle?.dataset.handle === key;
    return `pane-tiles__handle pane-tiles__handle--${horizontal ? "row" : "column"}${active ? " is-active" : ""}`;
  };

  const columnPlacement = (index: number): Record<string, string> => ({
    "--tile-col": String(index),
    "--tile-cols": String(arranged.length),
    "--tile-x": fraction(columnShares[index].start),
    "--tile-w": fraction(columnShares[index].size)
  });

  const rowPlacement = (columnIndex: number, row: number): Record<string, string> => ({
    "--tile-row": String(row),
    "--tile-rows": String(arranged[columnIndex].panes.length),
    "--tile-y": fraction(rowShares[columnIndex][row].start),
    "--tile-h": fraction(rowShares[columnIndex][row].size)
  });

  /** The divider under a tile, between it and the next one down its column. */
  const rowDivider = (columnIndex: number, row: number) => {
    const ids = arranged[columnIndex].panes;
    const [upper, lower] = [weightOf(paneFlex[ids[row]]), weightOf(paneFlex[ids[row + 1]])];
    const key = `rows:${columnIndex}:${row}`;
    return <div
      className={handleClass(key, false)} data-handle={key}
      style={{ ...columnPlacement(columnIndex), ...rowPlacement(columnIndex, row + 1), "--tile-row": String(row) } as CSSProperties}
      role="separator" aria-orientation="horizontal" tabIndex={0}
      aria-label={t("调整面板高度", "Resize panes")}
      aria-valuemin={0} aria-valuemax={100}
      aria-valuenow={Math.round(100 * upper / (upper + lower))}
      onPointerDown={(event) => startResize(event, false, measureRows(columnIndex, row))}
      onKeyDown={(event) => resizeWithKeyboard(event, false, measureRows(columnIndex, row))}
      onDoubleClick={() => onPaneFlexChange({ ...paneFlex, [ids[row]]: 1, [ids[row + 1]]: 1 })}
    />;
  };

  /** The divider right of a column, between it and the next column. */
  const columnDivider = (columnIndex: number) => {
    const [left, right] = [arranged[columnIndex], arranged[columnIndex + 1]];
    const key = `columns:${columnIndex}`;
    return <div
      className={handleClass(key, true)} data-handle={key}
      style={{ ...columnPlacement(columnIndex + 1), "--tile-col": String(columnIndex) } as CSSProperties}
      role="separator" aria-orientation="vertical" tabIndex={0}
      aria-label={t("调整列宽度", "Resize columns")}
      aria-valuemin={0} aria-valuemax={100}
      aria-valuenow={Math.round(100 * left.weight / (left.weight + right.weight))}
      onPointerDown={(event) => startResize(event, true, measureColumns(columnIndex))}
      onKeyDown={(event) => resizeWithKeyboard(event, true, measureColumns(columnIndex))}
      onDoubleClick={() => {
        const next = givenColumnWeights();
        next[left.index] = next[right.index] = (left.weight + right.weight) / 2;
        onColumnFlexChange?.(next);
      }}
    />;
  };

  const firstPane = arranged[0]?.panes[0];
  return (
    <div className={`pane-tiles${activeHandle ? " pane-tiles--resizing" : ""}${solo ? " pane-tiles--solo" : ""}`}>
      <div
        ref={chatRef}
        className={`pane-tiles__chat${solo ? " is-solo-hidden" : ""}`}
        inert={solo ? true : undefined}
        style={{ flex: `${CHAT_TILE_FLEX} 1 0px` }}
      >{chat}</div>
      {panes.length > 0 && <>
        {firstPane !== undefined && !solo && <div
          className={handleClass("side", true)} data-handle="side"
          role="separator" aria-orientation="vertical" tabIndex={0}
          aria-label={t("调整面板宽度", "Resize panes")}
          aria-valuemin={0} aria-valuemax={100} aria-valuenow={Math.round(100 * sideFlex / (CHAT_TILE_FLEX + sideFlex))}
          onPointerDown={(event) => startResize(event, true, measureSide())}
          onKeyDown={(event) => resizeWithKeyboard(event, true, measureSide(), true)}
          onDoubleClick={() => onSideFlexChange(
            defaultSideFlexForKind(paneKind(firstPane)) + (arranged.length - 1) * NEW_COLUMN_FLEX
          )}
        />}
        <div
          ref={sideRef}
          className="pane-tiles__side"
          hidden={visiblePanes.length === 0 || undefined}
          style={{
            flex: solo ? "1 1 0px" : `${sideFlex} 1 0px`,
            // More columns need more room before any of them is too narrow to use.
            minWidth: solo ? undefined : `${minimumSideWidth(arranged.length)}px`
          }}
        >
          {panes.map((pane) => {
            const place = places.get(pane.id);
            const covered = solo !== null && pane.id !== solo && !pane.hidden;
            const placement = place && { ...columnPlacement(place.column), ...rowPlacement(place.column, place.row) };
            const geometry = pane.hidden || !placement ? "hidden"
              : JSON.stringify([pane.id === solo ? "solo" : covered ? "covered" : "tiled", placement]);
            return <Fragment key={pane.id}>
              <div
                ref={(element) => { if (element) tilesRef.current.set(pane.id, element); else tilesRef.current.delete(pane.id); }}
                className={`pane-tiles__tile${covered ? " is-solo-hidden" : ""}${pane.id === solo ? " is-solo" : ""}`}
                style={placement as CSSProperties | undefined}
                hidden={pane.hidden || undefined} inert={pane.hidden || covered || undefined}
              >
                <PaneTileGeometryContext.Provider value={geometry}>{pane.node}</PaneTileGeometryContext.Provider>
              </div>
              {place && !solo && place.row < arranged[place.column].panes.length - 1 && rowDivider(place.column, place.row)}
              {place && !solo && onColumnFlexChange && place.row === 0 && place.column < arranged.length - 1
                && columnDivider(place.column)}
            </Fragment>;
          })}
        </div>
      </>}
    </div>
  );
}
