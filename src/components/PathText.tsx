import { useLayoutEffect, useRef, useState } from "react";
import { elidePath } from "../lib/pathDisplay";
import "./PathText.css";

export interface PathTextProps {
  path: string;
  /** Text kept whole in front of the path, such as the shell a command ran in. */
  prefix?: string;
  className?: string;
  /** Hover text; defaults to the whole of what is drawn. `null` draws none. */
  title?: string | null;
}

let measuringContext: CanvasRenderingContext2D | null | undefined;

/** Measures text in `element`'s font, or null where nothing can be measured. */
function textMeasurer(element: HTMLElement): ((text: string) => number) | null {
  if (measuringContext === undefined) {
    // Asked once: a document that cannot draw text will not learn to.
    measuringContext = document.createElement("canvas").getContext("2d") ?? null;
  }
  const context = measuringContext;
  if (!context) return null;
  const style = getComputedStyle(element);
  const font = `${style.fontStyle} ${style.fontWeight} ${style.fontSize} ${style.fontFamily}`;
  const spacing = Number.parseFloat(style.letterSpacing) || 0;
  return (text) => {
    context.font = font;
    return context.measureText(text).width + spacing * Array.from(text).length;
  };
}

/**
 * A path that gives way in the middle when it does not fit — whole directories first, keeping the
 * first and last names, then those two names in their own middles. See `lib/pathDisplay.ts`. Every
 * absolute path the app draws goes through here, so they all shorten the same way.
 *
 * The box is as wide as the whole path would be, wherever its container allows: an invisible copy
 * of the full text sizes it, so what is drawn never changes the width it is fitted to. It needs a
 * width from outside — a block, or a flex item with `min-width: 0` — to have anything to give way to.
 * A shortened path keeps the whole one for screen readers, and the hover text shows it.
 */
export function PathText({ path, prefix = "", className, title }: PathTextProps) {
  const rootRef = useRef<HTMLSpanElement>(null);
  const [shown, setShown] = useState(path);
  const full = `${prefix}${path}`;

  useLayoutEffect(() => {
    const root = rootRef.current;
    if (!root) return undefined;
    let alive = true;
    const refit = () => {
      if (!alive) return;
      const style = getComputedStyle(root);
      const width = root.clientWidth
        - (Number.parseFloat(style.paddingLeft) || 0)
        - (Number.parseFloat(style.paddingRight) || 0);
      // No layout (a hidden subtree, a test document): draw it whole and let CSS clip it.
      const measure = width > 0 ? textMeasurer(root) : null;
      if (!measure) {
        setShown(path);
        return;
      }
      // Half a pixel of slack for the rounding between canvas and layout.
      const room = width - measure(prefix) - 0.5;
      setShown(elidePath(path, (text) => measure(text) <= room));
    };
    refit();
    if (typeof ResizeObserver === "undefined") return () => { alive = false; };
    const observer = new ResizeObserver(refit);
    observer.observe(root);
    // A web font that arrives later changes every width without resizing the box.
    void document.fonts?.ready.then(refit);
    return () => {
      alive = false;
      observer.disconnect();
    };
  }, [path, prefix]);

  const shortened = shown !== path;
  return (
    <span
      ref={rootRef}
      className={`path-text${className ? ` ${className}` : ""}`}
      title={title === null ? undefined : (title ?? full)}
    >
      <span className="path-text__sizer" data-text={full} aria-hidden="true" />
      <span className="path-text__shown" aria-hidden={shortened || undefined}>{`${prefix}${shown}`}</span>
      {shortened && <span className="sr-only">{full}</span>}
    </span>
  );
}
