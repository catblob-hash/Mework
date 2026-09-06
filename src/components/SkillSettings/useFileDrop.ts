import { useEffect, useRef, useState } from "react";
import type { RefObject } from "react";
import { isTauriRuntime } from "../../lib/backend";

/**
 * Receive dropped files' real disk paths within a target area.
 *
 * HTML5 `File` objects in a webview have no path, but skill installation requires paths for host-side directory copying. Tauri's window-level drag-and-drop event supplies `paths`; because it applies to the whole window, coordinates must be hit-tested against the target rectangle.
 */
export function useFileDrop(
  target: RefObject<HTMLElement | null>,
  enabled: boolean,
  onDrop: (paths: string[]) => void
): { over: boolean } {
  const [over, setOver] = useState(false);
  const onDropRef = useRef(onDrop);
  onDropRef.current = onDrop;

  useEffect(() => {
    if (!enabled || !isTauriRuntime()) {
      setOver(false);
      return;
    }
    let unlisten: (() => void) | null = null;
    let disposed = false;

    const inside = (position: { x: number; y: number }): boolean => {
      const element = target.current;
      if (!element) return false;
      const rect = element.getBoundingClientRect();
      // Events use physical pixels while DOM rectangles use CSS pixels.
      const ratio = window.devicePixelRatio || 1;
      const x = position.x / ratio;
      const y = position.y / ratio;
      return x >= rect.left && x <= rect.right && y >= rect.top && y <= rect.bottom;
    };

    void import("@tauri-apps/api/webview")
      .then(({ getCurrentWebview }) => getCurrentWebview().onDragDropEvent((event) => {
        const payload = event.payload;
        if (payload.type === "leave") {
          setOver(false);
          return;
        }
        if (payload.type === "over") {
          setOver(inside(payload.position));
          return;
        }
        if (payload.type !== "drop") return;
        const hit = inside(payload.position);
        setOver(false);
        if (hit && payload.paths.length) onDropRef.current(payload.paths);
      }))
      .then((stop) => {
        if (disposed) stop();
        else unlisten = stop;
      })
      .catch(() => {
        // Without window drag-and-drop events, the area remains a click-only target.
      });

    return () => {
      disposed = true;
      unlisten?.();
      setOver(false);
    };
  }, [enabled, target]);

  return { over };
}
