import { memo, useCallback } from "react";
import { useI18n } from "../../i18n";
import { SketchCanvas } from "./SketchCanvas";
import { SketchToolbar } from "./SketchToolbar";
import { SKETCH_COLORS, SKETCH_STROKE_WIDTH } from "./strokes";
import { useSketch } from "./useSketch";
import "./Sketch.css";

export interface SketchOverlayProps {
  /** The captured page, drawn under the strokes and used as the composite's resolution. */
  backdropDataUrl: string | null;
  onCancel: () => void;
  onAttach: (dataUrl: string) => void | Promise<void>;
  /** Label of the primary action; defaults to the browser pane's "Add to chat". */
  attachLabel?: string;
  className?: string;
}

function SketchOverlayView({ backdropDataUrl, onCancel, onAttach, attachLabel, className }: SketchOverlayProps) {
  const { t } = useI18n();
  const {
    canvasRef,
    tool,
    setTool,
    color,
    setColor,
    hasStrokes,
    canUndo,
    canRedo,
    handleHistoryChange,
    handleUndo,
    handleRedo,
    handleKeyDown,
    undoShortcut,
    redoShortcut,
    handleClear,
    exportSketch,
    cancel
  } = useSketch();

  const attach = useCallback(async () => {
    const dataUrl = await exportSketch((handle) => (
      backdropDataUrl ? handle.toComposite(backdropDataUrl) : handle.toPNG("#ffffff")
    ));
    if (dataUrl !== null) await onAttach(dataUrl);
  }, [backdropDataUrl, exportSketch, onAttach]);

  const close = useCallback(() => {
    cancel();
    onCancel();
  }, [cancel, onCancel]);

  return (
    <div
      tabIndex={-1}
      onKeyDown={handleKeyDown}
      className={`sketch-overlay${className ? ` ${className}` : ""}`}
    >
      {backdropDataUrl
        ? <img src={backdropDataUrl} alt="" draggable={false} className="sketch-overlay__backdrop" />
        : <div className="sketch-overlay__backdrop sketch-overlay__backdrop--blank" />}
      <SketchCanvas
        ref={canvasRef}
        className="sketch-overlay__canvas"
        tool={tool}
        color={color}
        strokeWidth={SKETCH_STROKE_WIDTH}
        onHistoryChange={handleHistoryChange}
      />
      <SketchToolbar
        tool={tool}
        onToolChange={setTool}
        colors={SKETCH_COLORS}
        color={color}
        onColorChange={setColor}
        hasStrokes={hasStrokes}
        canUndo={canUndo}
        canRedo={canRedo}
        onUndo={handleUndo}
        onRedo={handleRedo}
        undoShortcut={undoShortcut}
        redoShortcut={redoShortcut}
        onClear={handleClear}
        onClose={close}
        positiveAction={
          <button
            type="button"
            className="button button--primary button--small sketch-toolbar__attach"
            disabled={!hasStrokes}
            onClick={() => void attach()}
          >
            {attachLabel ?? t("添加到对话", "Add to chat")}
          </button>
        }
      />
    </div>
  );
}

export const SketchOverlay = memo(SketchOverlayView);
