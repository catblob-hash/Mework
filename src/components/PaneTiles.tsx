import { Fragment, useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import type { KeyboardEvent as ReactKeyboardEvent, PointerEvent as ReactPointerEvent, ReactNode } from "react";
import { useI18n } from "../i18n";
import { CHAT_TILE_FLEX, defaultSideFlexForKind, MAX_SIDE_FLEX, MIN_SIDE_FLEX, paneKind } from "../lib/sidePanes";
import type { SidePaneId } from "../lib/sidePanes";
import "./PaneTiles.css";

export interface PaneTilesProps {
  chat: ReactNode;
  /**
   * Ordered top → bottom. A `hidden` entry stays mounted and keeps its React identity — its
   * surface may own a live host session whose teardown would be reported as a stop — but it
   * takes no space, no handle and no place in the resize arithmetic.
   */
  panes: { id: SidePaneId; node: ReactNode; hidden?: boolean }[];
  sideFlex: number;
  paneFlex: Record<string, number>;
  /**
   * Shown alone over the whole tile area, chat column included. Every other tile and the chat
   * keep their box and stay mounted — a pane may own a live shell or a native page — but are
   * taken out of the flow, made invisible and inert.
   */
  expanded?: SidePaneId | null;
  onSideFlexChange: (sideFlex: number) => void;
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
  row: boolean;
  element: HTMLDivElement;
  layoutKey: string;
}

function clamp(value: number, min: number, max: number): number {
  return Math.max(min, Math.min(max, value));
}

export function PaneTiles({ chat, panes, sideFlex, paneFlex, expanded = null, onSideFlexChange, onPaneFlexChange, onResizeStateChange }: PaneTilesProps) {
  const { t } = useI18n();
  const chatRef = useRef<HTMLDivElement>(null);
  const sideRef = useRef<HTMLDivElement>(null);
  const tilesRef = useRef(new Map<SidePaneId, HTMLDivElement>());
  const sessionRef = useRef<ResizeSession | null>(null);
  const [activeHandle, setActiveHandle] = useState<HTMLDivElement | null>(null);
  const callbacks = useRef({ onSideFlexChange, onPaneFlexChange, onResizeStateChange, paneFlex });
  useLayoutEffect(() => {
    callbacks.current = { onSideFlexChange, onPaneFlexChange, onResizeStateChange, paneFlex };
  });

  const visiblePanes = panes.filter((pane) => !pane.hidden);
  // A pane whose subject vanished renders nothing and is not in `panes`; expanding it would
  // leave an empty workspace, so an unrenderable id means no expansion at all.
  const solo = expanded !== null && visiblePanes.some((pane) => pane.id === expanded) ? expanded : null;
  const layoutKey = JSON.stringify([panes.map((pane) => [pane.id, Boolean(pane.hidden)]), solo]);
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
      session.apply((session.row ? event.clientX : event.clientY) - session.start);
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

  const measureRow = (): ResizeMeasure | null => {
    const chatWidth = chatRef.current?.getBoundingClientRect().width ?? 0;
    const sideWidth = sideRef.current?.getBoundingClientRect().width ?? 0;
    if (chatWidth < 320 || sideWidth < 280) return null;
    const minimum = 320 - chatWidth;
    const maximum = sideWidth - 280;
    return {
      minimum, maximum,
      apply: (delta) => {
        // The chat tile gives up exactly the pixels the side column gains, so the ratio must use
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

  const measureColumn = (index: number): ResizeMeasure | null => {
    const first = visiblePanes[index].id;
    const second = visiblePanes[index + 1].id;
    const firstHeight = tilesRef.current.get(first)?.getBoundingClientRect().height ?? 0;
    const secondHeight = tilesRef.current.get(second)?.getBoundingClientRect().height ?? 0;
    if (firstHeight < 100 || secondHeight < 100) return null;
    const height = firstHeight + secondHeight;
    const sum = (paneFlex[first] ?? 1) + (paneFlex[second] ?? 1);
    const minimum = 100 - firstHeight;
    const maximum = secondHeight - 100;
    // A minimum-constrained tile's measured height is not proportional to its flex.
    // Use one pixel-to-flex scale for all open tiles to keep unrelated separators fixed.
    const measuredFlex = Object.fromEntries(visiblePanes.map((pane) => [
      pane.id, (tilesRef.current.get(pane.id)?.getBoundingClientRect().height ?? 100) / height * sum
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

  const startResize = (event: ReactPointerEvent<HTMLDivElement>, row: boolean, measure: ResizeMeasure | null) => {
    if (event.button !== 0 || event.isPrimary === false || sessionRef.current || !measure) return;
    event.preventDefault();
    sessionRef.current = { ...measure, pointerId: event.pointerId, start: row ? event.clientX : event.clientY, row, element: event.currentTarget, layoutKey };
    try {
      event.currentTarget.setPointerCapture?.(event.pointerId);
    } catch {
      // Some WebViews reject capture; window-level listeners keep resizing active.
    }
    document.body.classList.add("pane-tiles-resize-active");
    document.body.classList.toggle("pane-tiles-resize-active--column", !row);
    setActiveHandle(event.currentTarget);
    callbacks.current.onResizeStateChange?.(true);
  };

  const resizeWithKeyboard = (event: ReactKeyboardEvent<HTMLDivElement>, row: boolean, measure: ResizeMeasure | null) => {
    if (!measure || sessionRef.current) return;
    const step = event.shiftKey ? 48 : 24;
    const delta = event.key === (row ? "ArrowLeft" : "ArrowUp") ? -step
      : event.key === (row ? "ArrowRight" : "ArrowDown") ? step
        : event.key === "Home" ? (row ? measure.maximum : measure.minimum)
          : event.key === "End" ? (row ? measure.minimum : measure.maximum) : null;
    if (delta === null) return;
    event.preventDefault();
    measure.apply(delta);
  };

  const handleClass = (row: boolean, index?: number) => {
    const active = activeHandle && (row ? activeHandle.dataset.axis === "row" : activeHandle.dataset.index === String(index));
    return `pane-tiles__handle pane-tiles__handle--${row ? "row" : "column"}${active ? " is-active" : ""}`;
  };

  return (
    <div className={`pane-tiles${activeHandle ? " pane-tiles--resizing" : ""}${solo ? " pane-tiles--solo" : ""}`}>
      <div
        ref={chatRef}
        className={`pane-tiles__chat${solo ? " is-solo-hidden" : ""}`}
        inert={solo ? true : undefined}
        style={{ flex: `${CHAT_TILE_FLEX} 1 0px` }}
      >{chat}</div>
      {panes.length > 0 && <>
        {visiblePanes.length > 0 && !solo && <div
          className={handleClass(true)} data-axis="row"
          role="separator" aria-orientation="vertical" tabIndex={0}
          aria-label={t("调整面板宽度", "Resize panes")}
          aria-valuemin={0} aria-valuemax={100} aria-valuenow={Math.round(100 * sideFlex / (CHAT_TILE_FLEX + sideFlex))}
          onPointerDown={(event) => startResize(event, true, measureRow())}
          onKeyDown={(event) => resizeWithKeyboard(event, true, measureRow())}
          onDoubleClick={() => onSideFlexChange(defaultSideFlexForKind(paneKind(visiblePanes[0].id)))}
        />}
        <div
          ref={sideRef}
          className="pane-tiles__side"
          hidden={visiblePanes.length === 0 || undefined}
          style={{ flex: solo ? "1 1 0px" : `${sideFlex} 1 0px` }}
        >
          {panes.map((pane) => {
            const index = pane.hidden ? -1 : visiblePanes.indexOf(pane);
            const next = index >= 0 && index < visiblePanes.length - 1 ? visiblePanes[index + 1] : null;
            const covered = solo !== null && pane.id !== solo && !pane.hidden;
            return <Fragment key={pane.id}>
              <div
                ref={(element) => { if (element) tilesRef.current.set(pane.id, element); else tilesRef.current.delete(pane.id); }}
                className={`pane-tiles__tile${covered ? " is-solo-hidden" : ""}`}
                style={{ flex: pane.id === solo ? "1 1 0px" : `${paneFlex[pane.id] ?? 1} 1 0px` }}
                hidden={pane.hidden || undefined} inert={pane.hidden || covered || undefined}
              >{pane.node}</div>
              {next && !solo && <div
                className={handleClass(false, index)} data-axis="column" data-index={index}
                role="separator" aria-orientation="horizontal" tabIndex={0}
                aria-label={t("调整面板高度", "Resize panes")}
                aria-valuemin={0} aria-valuemax={100}
                aria-valuenow={Math.round(100 * (paneFlex[pane.id] ?? 1) / ((paneFlex[pane.id] ?? 1) + (paneFlex[next.id] ?? 1)))}
                onPointerDown={(event) => startResize(event, false, measureColumn(index))}
                onKeyDown={(event) => resizeWithKeyboard(event, false, measureColumn(index))}
                onDoubleClick={() => onPaneFlexChange({ ...paneFlex, [pane.id]: 1, [next.id]: 1 })}
              />}
            </Fragment>;
          })}
        </div>
      </>}
    </div>
  );
}
