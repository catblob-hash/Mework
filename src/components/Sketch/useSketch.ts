import type { KeyboardEvent as ReactKeyboardEvent, RefObject } from "react";
import { useCallback, useMemo, useRef, useState } from "react";
import { EMPTY_SKETCH_HISTORY_STATE } from "./history";
import type { SketchHistoryState } from "./history";
import { SKETCH_COLORS } from "./strokes";
import type { SketchToolName } from "./strokes";
import type { SketchCanvasHandle } from "./SketchCanvas";

export type SketchCommand = "undo" | "redo";

interface SketchKeyEvent {
  key: string;
  altKey: boolean;
  shiftKey: boolean;
  ctrlKey: boolean;
  metaKey: boolean;
}

function isApplePlatform(platform: string = typeof navigator === "undefined" ? "" : navigator.platform): boolean {
  return /mac|iphone|ipad|ipod/i.test(platform);
}

/** Cmd/Ctrl+Z undoes, Cmd/Ctrl+Shift+Z redoes, and Ctrl+Y also redoes off Apple platforms. */
export function sketchCommandForEvent(event: SketchKeyEvent, apple: boolean): SketchCommand | null {
  if (event.altKey) return null;
  const primary = apple ? event.metaKey : event.ctrlKey;
  if (!primary) return null;
  if (apple && event.ctrlKey) return null;
  const key = event.key.toLowerCase();
  if (key === "z") return event.shiftKey ? "redo" : "undo";
  if (!apple && key === "y" && !event.shiftKey) return "redo";
  return null;
}

function sketchShortcutLabel(command: SketchCommand, apple: boolean): string {
  if (apple) return command === "undo" ? "⌘Z" : "⇧⌘Z";
  return command === "undo" ? "Ctrl+Z" : "Ctrl+Shift+Z";
}

/** A shortcut aimed at a text field belongs to that field, not to the sketch. */
export function isTypingTarget(target: EventTarget | null): boolean {
  const element = target as HTMLElement | null;
  if (!element || typeof element.closest !== "function") return false;
  return element.closest('input, textarea, [contenteditable]:not([contenteditable="false"])') !== null;
}

export interface UseSketchResult {
  canvasRef: RefObject<SketchCanvasHandle | null>;
  tool: SketchToolName;
  setTool: (tool: SketchToolName) => void;
  color: string;
  setColor: (color: string) => void;
  hasStrokes: boolean;
  canUndo: boolean;
  canRedo: boolean;
  handleHistoryChange: (state: SketchHistoryState) => void;
  handleUndo: () => void;
  handleRedo: () => void;
  handleKeyDown: (event: ReactKeyboardEvent<HTMLElement>) => void;
  undoShortcut: string;
  redoShortcut: string;
  handleClear: () => void;
  exportSketch: (run: (handle: SketchCanvasHandle) => Promise<string> | string) => Promise<string | null>;
  cancel: () => void;
}

export function useSketch(): UseSketchResult {
  const canvasRef = useRef<SketchCanvasHandle | null>(null);
  const [tool, setTool] = useState<SketchToolName>("pen");
  const [color, setColor] = useState<string>(SKETCH_COLORS[0]!);
  const [history, setHistory] = useState<SketchHistoryState>(EMPTY_SKETCH_HISTORY_STATE);
  const exportSeq = useRef(0);
  const apple = useMemo(() => isApplePlatform(), []);

  const handleUndo = useCallback(() => {
    canvasRef.current?.undo();
  }, []);

  const handleRedo = useCallback(() => {
    canvasRef.current?.redo();
  }, []);

  const handleClear = useCallback(() => {
    canvasRef.current?.clear();
  }, []);

  const handleKeyDown = useCallback((event: ReactKeyboardEvent<HTMLElement>) => {
    const target = event.target as HTMLElement | null;
    if (event.defaultPrevented || !target || isTypingTarget(target)) return;
    const command = sketchCommandForEvent(event, apple);
    if (command === null) return;
    event.preventDefault();
    event.stopPropagation();
    const root = event.currentTarget;
    if (!root.contains(target)) return;
    // A dialog or menu opened inside the overlay owns its own keyboard.
    const nested = typeof target.closest === "function"
      ? target.closest('[role="dialog"], [role="alertdialog"], [role="menu"]')
      : null;
    if (nested !== null && nested !== root && root.contains(nested)) return;
    if (command === "undo" && history.canUndo) canvasRef.current?.undo();
    else if (command === "redo" && history.canRedo) canvasRef.current?.redo();
  }, [apple, history.canRedo, history.canUndo]);

  // Every export takes a ticket. `cancel` invalidates outstanding tickets so a composite that
  // resolves after the surface was dismissed never reaches the caller.
  const exportSketch = useCallback(async (run: (handle: SketchCanvasHandle) => Promise<string> | string) => {
    const handle = canvasRef.current;
    if (!handle) return null;
    const ticket = ++exportSeq.current;
    const result = await run(handle);
    return exportSeq.current === ticket ? result : null;
  }, []);

  const cancel = useCallback(() => {
    exportSeq.current += 1;
  }, []);

  return {
    canvasRef,
    tool,
    setTool,
    color,
    setColor,
    hasStrokes: history.count > 0,
    canUndo: history.canUndo,
    canRedo: history.canRedo,
    handleHistoryChange: setHistory,
    handleUndo,
    handleRedo,
    handleKeyDown,
    undoShortcut: sketchShortcutLabel("undo", apple),
    redoShortcut: sketchShortcutLabel("redo", apple),
    handleClear,
    exportSketch,
    cancel
  };
}
