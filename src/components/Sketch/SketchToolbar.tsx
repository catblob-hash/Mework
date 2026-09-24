import { Pencil, Redo2, Trash2, Undo2, X } from "lucide-react";
import type { ReactNode } from "react";
import { memo, useCallback, useLayoutEffect, useMemo, useRef, useState } from "react";
import { useI18n } from "../../i18n";
import type { TranslationFunction } from "../../i18n";
import { Dialog, IconButton } from "../Common";
import { PopoverMenu } from "../PopoverMenu";
import { SKETCH_TOOLS } from "./strokes";
import type { SketchToolName } from "./strokes";
import "./Sketch.css";

/** The five shape glyphs are the reference kit's own 16px paths; pen comes from lucide. */
const TOOL_ICON_PATHS: Record<Exclude<SketchToolName, "pen">, string> = {
  line: "M3.5 12.5L12.5 3.5",
  arrow: "M5.5 3.5H12.5V10.5M3.5 12.5L12.5 3.5",
  rect: "M4.25 3.5H11.75A0.75 0.75 0 0 1 12.5 4.25V11.75A0.75 0.75 0 0 1 11.75 12.5H4.25A0.75 0.75 0 0 1 3.5 11.75V4.25A0.75 0.75 0 0 1 4.25 3.5Z",
  ellipse: "M3 8A5 5 0 1 0 13 8A5 5 0 1 0 3 8Z",
  text: "M3.5 3.5H12.5M8 3.5V12.5"
};

function SketchToolIcon({ tool }: { tool: SketchToolName }) {
  if (tool === "pen") return <Pencil size={16} aria-hidden="true" />;
  return (
    <svg width="16" height="16" viewBox="0 0 16 16" fill="none" aria-hidden="true">
      <path
        d={TOOL_ICON_PATHS[tool]}
        stroke="currentColor"
        strokeWidth="1"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}

function SketchSwatch({ color, active = false }: { color: string; active?: boolean }) {
  return (
    <span
      aria-hidden="true"
      className={`sketch-toolbar__swatch${active ? " is-active" : ""}`}
      style={{ background: color }}
    />
  );
}

function sketchToolLabel(tool: SketchToolName, t: TranslationFunction): string {
  switch (tool) {
    case "pen": return t("画笔", "Pen");
    case "line": return t("直线", "Line");
    case "arrow": return t("箭头", "Arrow");
    case "rect": return t("矩形", "Rectangle");
    case "ellipse": return t("椭圆", "Ellipse");
    case "text": return t("文字", "Text");
  }
}

function sketchColorLabel(color: string, t: TranslationFunction): string {
  switch (color.toUpperCase()) {
    case "#E03131": return t("红色", "Red");
    case "#1971C2": return t("蓝色", "Blue");
    case "#2F9E44": return t("绿色", "Green");
    case "#1F1E1D": return t("黑色", "Black");
    default: return color;
  }
}

export interface SketchToolbarProps {
  tool: SketchToolName;
  onToolChange: (tool: SketchToolName) => void;
  colors: readonly string[];
  color: string;
  onColorChange: (color: string) => void;
  hasStrokes: boolean;
  canUndo: boolean;
  canRedo: boolean;
  onUndo: () => void;
  onRedo: () => void;
  undoShortcut?: string;
  redoShortcut?: string;
  onClear: () => void;
  /** Called once the discard confirmation — shown only when marks exist — has been accepted. */
  onClose: () => void;
  positiveAction?: ReactNode;
  className?: string;
}

function SketchToolbarView({
  tool,
  onToolChange,
  colors,
  color,
  onColorChange,
  hasStrokes,
  canUndo,
  canRedo,
  onUndo,
  onRedo,
  undoShortcut,
  redoShortcut,
  onClear,
  onClose,
  positiveAction,
  className
}: SketchToolbarProps) {
  const { t } = useI18n();
  const containerRef = useRef<HTMLDivElement>(null);
  const pillRef = useRef<HTMLDivElement>(null);
  const fullWidthRef = useRef(0);
  const [compact, setCompact] = useState(false);
  const [confirming, setConfirming] = useState(false);

  const requestClose = useCallback(() => {
    if (hasStrokes) {
      setConfirming(true);
      return;
    }
    onClose();
  }, [hasStrokes, onClose]);

  useLayoutEffect(() => {
    const container = containerRef.current;
    const pill = pillRef.current;
    if (!container || !pill) return;
    const measure = () => {
      if (pill.dataset.layout === "full") fullWidthRef.current = pill.offsetWidth;
      setCompact(fullWidthRef.current > container.clientWidth);
    };
    measure();
    if (typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(measure);
    observer.observe(container);
    observer.observe(pill);
    return () => observer.disconnect();
  }, []);

  // Undo, redo and clear disable themselves mid-session. Focus would land on <body> and the
  // overlay would stop receiving the sketch shortcuts, so hand it back to the overlay root.
  const lastFocusedRef = useRef<HTMLElement | null>(null);
  useLayoutEffect(() => {
    const element = lastFocusedRef.current;
    if (!element?.matches(":disabled")) return;
    lastFocusedRef.current = null;
    const owner = element.ownerDocument;
    const active = owner.activeElement;
    if (active !== element && active !== owner.body && active !== null) return;
    const fallback = element.parentElement?.closest<HTMLElement>("[tabindex]");
    fallback?.focus({ preventScroll: true });
  }, [canUndo, canRedo, hasStrokes]);

  const toolSections = useMemo(() => [{
    id: "tools",
    items: SKETCH_TOOLS.map((candidate) => ({
      id: candidate,
      label: sketchToolLabel(candidate, t),
      icon: <SketchToolIcon tool={candidate} />,
      checked: candidate === tool,
      onSelect: () => onToolChange(candidate)
    }))
  }], [onToolChange, t, tool]);

  const colorSections = useMemo(() => [{
    id: "colors",
    items: colors.map((candidate) => ({
      id: candidate,
      label: sketchColorLabel(candidate, t),
      icon: <SketchSwatch color={candidate} />,
      checked: candidate === color,
      onSelect: () => onColorChange(candidate)
    }))
  }], [color, colors, onColorChange, t]);

  const toolGroupLabel = t("绘图工具", "Drawing tool");
  const colorGroupLabel = t("墨水颜色", "Ink color");
  const undoLabel = canUndo ? t("撤销", "Undo") : t("暂无可撤销的内容", "Nothing to undo yet");
  const redoLabel = canRedo ? t("重做", "Redo") : t("暂无可重做的内容", "Nothing to redo yet");
  const clearLabel = hasStrokes ? t("清除全部", "Clear all") : t("暂无可清除的内容", "Nothing to clear yet");
  const closeLabel = t("关闭", "Close");

  return (
    <div ref={containerRef} className={`sketch-toolbar${className ? ` ${className}` : ""}`}>
      <div
        ref={pillRef}
        data-layout={compact ? "compact" : "full"}
        className="sketch-toolbar__pill"
        onFocus={(event) => {
          lastFocusedRef.current = event.target as HTMLElement;
        }}
      >
        {compact ? (
          <>
            <PopoverMenu
              rootClassName="sketch-toolbar__menu"
              triggerClassName="sketch-toolbar__tool"
              trigger={<SketchToolIcon tool={tool} />}
              triggerLabel={toolGroupLabel}
              triggerTitle={sketchToolLabel(tool, t)}
              menuLabel={toolGroupLabel}
              sections={toolSections}
              dense
            />
            <PopoverMenu
              rootClassName="sketch-toolbar__menu"
              triggerClassName="sketch-toolbar__tool"
              trigger={<SketchSwatch color={color} active />}
              triggerLabel={colorGroupLabel}
              triggerTitle={sketchColorLabel(color, t)}
              menuLabel={colorGroupLabel}
              sections={colorSections}
              dense
            />
          </>
        ) : (
          <>
            <div role="group" aria-label={toolGroupLabel} className="sketch-toolbar__group">
              {SKETCH_TOOLS.map((candidate) => {
                const label = sketchToolLabel(candidate, t);
                return (
                  <button
                    key={candidate}
                    type="button"
                    aria-label={label}
                    title={label}
                    aria-pressed={candidate === tool}
                    className="sketch-toolbar__tool"
                    onClick={() => onToolChange(candidate)}
                  >
                    <SketchToolIcon tool={candidate} />
                  </button>
                );
              })}
            </div>
            <span role="separator" aria-orientation="vertical" className="sketch-toolbar__divider" />
            <div role="group" aria-label={colorGroupLabel} className="sketch-toolbar__group sketch-toolbar__group--colors">
              {colors.map((candidate) => {
                const label = sketchColorLabel(candidate, t);
                return (
                  <button
                    key={candidate}
                    type="button"
                    aria-label={label}
                    title={label}
                    aria-pressed={candidate === color}
                    className="sketch-toolbar__swatch-button"
                    onClick={() => onColorChange(candidate)}
                  >
                    <SketchSwatch color={candidate} active={candidate === color} />
                  </button>
                );
              })}
            </div>
          </>
        )}
        <span role="separator" aria-orientation="vertical" className="sketch-toolbar__divider" />
        <div className="sketch-toolbar__group">
          <IconButton
            className="sketch-toolbar__action"
            label={undoLabel}
            title={undoShortcut ? `${undoLabel} · ${undoShortcut}` : undoLabel}
            disabled={!canUndo}
            onClick={onUndo}
          >
            <Undo2 size={15} aria-hidden="true" />
          </IconButton>
          <IconButton
            className="sketch-toolbar__action"
            label={redoLabel}
            title={redoShortcut ? `${redoLabel} · ${redoShortcut}` : redoLabel}
            disabled={!canRedo}
            onClick={onRedo}
          >
            <Redo2 size={15} aria-hidden="true" />
          </IconButton>
          <IconButton
            className="sketch-toolbar__action"
            label={clearLabel}
            disabled={!hasStrokes}
            onClick={onClear}
          >
            <Trash2 size={15} aria-hidden="true" />
          </IconButton>
        </div>
        {compact ? (
          <IconButton className="sketch-toolbar__action" label={closeLabel} onClick={requestClose}>
            <X size={15} aria-hidden="true" />
          </IconButton>
        ) : (
          <button type="button" className="button button--ghost button--small sketch-toolbar__close" onClick={requestClose}>
            {closeLabel}
          </button>
        )}
        {positiveAction}
      </div>
      {confirming && (
        <Dialog
          title={t("放弃这些标注？", "Discard your annotations?")}
          description={t(
            "这张图片上还有未保存的标记，放弃后会一并删除。",
            "You have unsaved marks on this image. Discarding removes them."
          )}
          width="400px"
          onClose={() => setConfirming(false)}
          footer={
            <>
              <button type="button" className="button button--secondary button--small" onClick={() => setConfirming(false)}>
                {t("继续编辑", "Keep editing")}
              </button>
              <button
                type="button"
                className="button button--danger button--small"
                onClick={() => {
                  setConfirming(false);
                  onClose();
                }}
              >
                {t("放弃", "Discard")}
              </button>
            </>
          }
        />
      )}
    </div>
  );
}

export const SketchToolbar = memo(SketchToolbarView);
