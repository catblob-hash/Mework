import type { CSSProperties, PointerEvent as ReactPointerEvent, KeyboardEvent as ReactKeyboardEvent } from "react";
import { forwardRef, useCallback, useEffect, useImperativeHandle, useMemo, useRef, useState } from "react";
import { useI18n } from "../../i18n";
import {
  EMPTY_SKETCH_HISTORY,
  historyState,
  pushHistory,
  redoHistory,
  undoHistory
} from "./history";
import type { SketchHistory, SketchHistoryState } from "./history";
import {
  SKETCH_FONT_STACK,
  SKETCH_TEXT_SIZE,
  constrainPoint,
  drawStroke
} from "./strokes";
import type { SketchPoint, SketchStroke, SketchToolName } from "./strokes";
import "./Sketch.css";

export interface SketchCanvasHandle {
  getStrokes: () => SketchStroke[];
  undo: () => number;
  redo: () => number;
  clear: () => void;
  toPNG: (background?: string | null) => string;
  toComposite: (backdropDataUrl: string) => Promise<string>;
}

export interface SketchCanvasProps {
  className?: string;
  tool?: SketchToolName;
  color: string;
  strokeWidth: number;
  onHistoryChange?: (state: SketchHistoryState) => void;
}

interface TextDraft {
  x: number;
  y: number;
  value: string;
}

export const SketchCanvas = forwardRef<SketchCanvasHandle, SketchCanvasProps>(function SketchCanvas(
  {
    className,
    tool = "pen",
    color,
    strokeWidth,
    onHistoryChange
  },
  ref
) {
  const { t } = useI18n();
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const wrapperRef = useRef<HTMLDivElement>(null);
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const contextRef = useRef<CanvasRenderingContext2D | null>(null);
  const historyRef = useRef<SketchHistory>(EMPTY_SKETCH_HISTORY);
  const pendingRef = useRef<SketchStroke | null>(null);
  const sizeRef = useRef({ w: 0, h: 0 });
  const [draft, setDraft] = useState<TextDraft | null>(null);
  const draftRef = useRef<TextDraft | null>(null);
  const onHistoryChangeRef = useRef(onHistoryChange);

  useEffect(() => {
    onHistoryChangeRef.current = onHistoryChange;
  }, [onHistoryChange]);

  useEffect(() => {
    draftRef.current = draft;
  }, [draft]);

  // An uncommitted text box is undoable even though it is not a stroke yet.
  const notify = useCallback(() => {
    const state = historyState(historyRef.current);
    const pendingText = Boolean(draftRef.current?.value.trim());
    onHistoryChangeRef.current?.(pendingText ? { ...state, canUndo: true, canRedo: false } : state);
  }, []);

  const setHistory = useCallback((next: SketchHistory) => {
    if (next === historyRef.current) return;
    historyRef.current = next;
    notify();
  }, [notify]);

  const hasDraftText = Boolean(draft?.value.trim());
  useEffect(() => {
    notify();
  }, [hasDraftText, notify]);

  const wrapperStyle = useMemo<CSSProperties>(
    () => ({ position: "relative", touchAction: "none", outline: "none" }),
    []
  );
  const canvasStyle = useMemo<CSSProperties>(
    () => ({ cursor: tool === "text" ? "text" : "crosshair" }),
    [tool]
  );

  const redraw = useCallback(() => {
    const context = contextRef.current;
    if (!context) return;
    const { w, h } = sizeRef.current;
    context.clearRect(0, 0, w, h);
    for (const stroke of historyRef.current.present) drawStroke(context, stroke);
    if (pendingRef.current) drawStroke(context, pendingRef.current);
  }, []);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const measure = () => {
      const width = canvas.offsetWidth;
      const height = canvas.offsetHeight;
      if (width === 0 || height === 0) return;
      // The backing store is device pixels; the transform keeps every stroke in CSS pixels.
      const dpr = window.devicePixelRatio || 1;
      canvas.width = Math.round(width * dpr);
      canvas.height = Math.round(height * dpr);
      sizeRef.current = { w: width, h: height };
      const context = canvas.getContext("2d");
      if (!context) return;
      context.setTransform(dpr, 0, 0, dpr, 0, 0);
      contextRef.current = context;
      redraw();
    };
    measure();
    if (typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(measure);
    observer.observe(canvas);
    return () => observer.disconnect();
  }, [redraw]);

  const commitStroke = useCallback((stroke: SketchStroke) => {
    const previous = historyRef.current.present;
    setHistory(pushHistory(historyRef.current, [...previous, stroke]));
  }, [setHistory]);

  const commitText = useCallback(() => {
    const pending = draftRef.current;
    draftRef.current = null;
    setDraft(null);
    if (!pending || !pending.value.trim()) return;
    const stroke: SketchStroke = {
      kind: "text",
      x: pending.x,
      y: pending.y,
      text: pending.value,
      size: SKETCH_TEXT_SIZE,
      color
    };
    commitStroke(stroke);
    const context = contextRef.current;
    if (context) drawStroke(context, stroke);
  }, [color, commitStroke]);

  const commitTextRef = useRef(commitText);
  useEffect(() => {
    commitTextRef.current = commitText;
  }, [commitText]);

  useEffect(() => {
    if (tool !== "text") commitTextRef.current();
  }, [tool]);

  const localPoint = (event: ReactPointerEvent<HTMLCanvasElement>): SketchPoint => {
    const rect = canvasRef.current!.getBoundingClientRect();
    return { x: event.clientX - rect.left, y: event.clientY - rect.top };
  };

  const onPointerDown = (event: ReactPointerEvent<HTMLCanvasElement>) => {
    if (event.button !== 0) return;
    const point = localPoint(event);
    if (tool === "text") {
      event.preventDefault();
      commitText();
      setDraft({ x: point.x, y: point.y, value: "" });
      return;
    }
    event.currentTarget.setPointerCapture(event.pointerId);
    if (tool === "pen") {
      pendingRef.current = { kind: "freehand", points: [point], color, width: strokeWidth };
      const context = contextRef.current;
      if (context) drawStroke(context, pendingRef.current);
      return;
    }
    pendingRef.current = { kind: tool, x1: point.x, y1: point.y, x2: point.x, y2: point.y, color, width: strokeWidth };
  };

  const onPointerMove = (event: ReactPointerEvent<HTMLCanvasElement>) => {
    const pending = pendingRef.current;
    const context = contextRef.current;
    if (!pending || !context) return;
    if (pending.kind === "freehand") {
      // Freehand redraws only the newest segment: a full repaint per pointer move drops frames.
      const point = localPoint(event);
      const last = pending.points[pending.points.length - 1]!;
      pending.points.push(point);
      context.lineCap = "round";
      context.strokeStyle = pending.color;
      context.lineWidth = pending.width;
      context.beginPath();
      context.moveTo(last.x, last.y);
      context.lineTo(point.x, point.y);
      context.stroke();
      return;
    }
    if (pending.kind === "text") return;
    const next = constrainPoint(pending.kind, { x: pending.x1, y: pending.y1 }, localPoint(event), event.shiftKey);
    pending.x2 = next.x;
    pending.y2 = next.y;
    redraw();
  };

  const onPointerEnd = () => {
    const pending = pendingRef.current;
    if (!pending) return;
    pendingRef.current = null;
    if (pending.kind !== "freehand" && pending.kind !== "text" && pending.x1 === pending.x2 && pending.y1 === pending.y2) {
      redraw();
      return;
    }
    commitStroke(pending);
  };

  const onDraftChange = useCallback((event: { target: { value: string } }) => {
    const value = event.target.value;
    setDraft((current) => (current && { ...current, value }));
  }, []);

  const onDraftKeyDown = useCallback((event: ReactKeyboardEvent<HTMLTextAreaElement>) => {
    event.stopPropagation();
    if (event.nativeEvent.isComposing || event.keyCode === 229) return;
    if (event.key === "Escape") {
      event.preventDefault();
      draftRef.current = null;
      setDraft(null);
    } else if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) {
      event.preventDefault();
      commitTextRef.current();
    } else {
      return;
    }
    wrapperRef.current?.focus({ preventScroll: true });
  }, []);

  useImperativeHandle(ref, () => ({
    getStrokes: () => historyRef.current.present,
    undo: () => {
      if (!pendingRef.current) {
        setHistory(undoHistory(historyRef.current));
        redraw();
      }
      return historyRef.current.present.length;
    },
    redo: () => {
      if (!pendingRef.current) {
        setHistory(redoHistory(historyRef.current));
        redraw();
      }
      return historyRef.current.present.length;
    },
    clear: () => {
      if (historyRef.current.present.length > 0) setHistory(pushHistory(historyRef.current, []));
      pendingRef.current = null;
      draftRef.current = null;
      setDraft(null);
      redraw();
    },
    toPNG: (background) => {
      const canvas = canvasRef.current;
      const context = contextRef.current;
      if (!canvas || !context) return "";
      const { w, h } = sizeRef.current;
      context.clearRect(0, 0, w, h);
      if (background) {
        context.fillStyle = background;
        context.fillRect(0, 0, w, h);
      }
      for (const stroke of historyRef.current.present) drawStroke(context, stroke);
      const dataUrl = canvas.toDataURL("image/png");
      redraw();
      return dataUrl;
    },
    // Composites at the backdrop's own pixel size, so the attachment keeps the screenshot's
    // resolution rather than the pane's CSS size.
    toComposite: async (backdropDataUrl) => {
      const image = new Image();
      image.src = backdropDataUrl;
      await image.decode();
      const target = document.createElement("canvas");
      target.width = image.naturalWidth;
      target.height = image.naturalHeight;
      const context = target.getContext("2d");
      if (!context) return backdropDataUrl;
      const { w, h } = sizeRef.current;
      if (w <= 0 || h <= 0) return backdropDataUrl;
      context.drawImage(image, 0, 0);
      const scale = Math.min(image.naturalWidth / w, image.naturalHeight / h);
      context.scale(scale, scale);
      for (const stroke of historyRef.current.present) drawStroke(context, stroke);
      return target.toDataURL("image/png");
    }
  }), [redraw, setHistory]);

  return (
    <div ref={wrapperRef} tabIndex={-1} className={`sketch-canvas${className ? ` ${className}` : ""}`} style={wrapperStyle}>
      <canvas
        ref={canvasRef}
        role="application"
        aria-label={t("绘图画布", "Sketch canvas")}
        className="sketch-canvas__surface"
        style={canvasStyle}
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={onPointerEnd}
        onPointerCancel={onPointerEnd}
      />
      {draft && (
        <textarea
          ref={textareaRef}
          autoFocus
          rows={1}
          value={draft.value}
          aria-label={t("文字标注", "Text label")}
          className="sketch-canvas__text"
          onChange={onDraftChange}
          onBlur={commitText}
          onPointerDown={(event) => event.stopPropagation()}
          onKeyDown={onDraftKeyDown}
          style={{
            left: draft.x,
            top: draft.y,
            width: `${Math.max(4, draft.value.length + 2)}ch`,
            color,
            font: `${SKETCH_TEXT_SIZE}px/1.2 ${SKETCH_FONT_STACK}`
          }}
        />
      )}
    </div>
  );
});
