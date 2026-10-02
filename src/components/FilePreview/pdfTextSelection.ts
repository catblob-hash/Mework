/**
 * Keeps a drag across a PDF page's blank space from jumping.
 *
 * A text layer is a page of absolutely placed spans over the drawing. Where the
 * pointer is over none of them — past the end of a line, between two lines —
 * WebKit, and Chromium before 148, resolve it to the layer's first or last
 * position, so a drag that strays off the words selects back to the top of the
 * page. pdf.js's own viewer answers with an `endOfContent` box: while a selection
 * is being made it covers the whole layer (`.selecting`), and it is moved in the
 * DOM to sit beside whichever end of the selection is moving, so blank space
 * resolves to where the selection already is. `PdfViewer` builds its text layers
 * without pdf.js's viewer, so this is that part of it (`TextLayerBuilder`).
 */

const layers = new Map<HTMLElement, HTMLElement>();
let stopListening: (() => void) | null = null;
let needsEnd: boolean | null = null;

/** Firefox and Chromium 148 resolve blank space on their own, by pdf.js's own test of them. */
function engineNeedsEnd(layer: HTMLElement): boolean {
  if (needsEnd === null) {
    const firefox = window.getComputedStyle(layer).getPropertyValue("-moz-user-select") === "none";
    const chromium = /\bChrome\/(\d+)\b/.exec(navigator.userAgent)?.[1];
    needsEnd = !firefox && (!chromium || Number.parseInt(chromium, 10) < 148);
  }
  return needsEnd;
}

function reset(end: HTMLElement, layer: HTMLElement) {
  layer.append(end);
  end.style.width = "";
  end.style.height = "";
  layer.classList.remove("selecting");
}

function resetAll() {
  layers.forEach(reset);
}

/** The element beside which the moving end of `range` sits. */
function movingEnd(range: Range, modifyStart: boolean): Element | null {
  let anchor: Node | null = modifyStart ? range.startContainer : range.endContainer;
  if (anchor.nodeType === Node.TEXT_NODE) anchor = anchor.parentNode;
  if (!modifyStart && range.endOffset === 0) {
    // An end at the very start of a node closes the text before it.
    do {
      while (anchor && !anchor.previousSibling) anchor = anchor.parentNode;
      anchor = anchor?.previousSibling ?? null;
    } while (anchor && !anchor.childNodes.length);
  }
  return anchor instanceof Element ? anchor : null;
}

function listen(): () => void {
  const controller = new AbortController();
  const { signal } = controller;
  let pointerDown = false;
  let previous: Range | null = null;
  document.addEventListener("pointerdown", () => {
    pointerDown = true;
  }, { signal });
  document.addEventListener("pointerup", () => {
    pointerDown = false;
    resetAll();
  }, { signal });
  window.addEventListener("blur", () => {
    pointerDown = false;
    resetAll();
  }, { signal });
  document.addEventListener("keyup", () => {
    if (!pointerDown) resetAll();
  }, { signal });
  document.addEventListener("selectionchange", () => {
    const selection = document.getSelection();
    if (!selection || selection.rangeCount === 0) {
      resetAll();
      return;
    }
    const range = selection.getRangeAt(0);
    for (const [layer, end] of layers) {
      if (range.intersectsNode(layer)) layer.classList.add("selecting");
      else reset(end, layer);
    }
    const first = layers.keys().next().value;
    if (!first || !engineNeedsEnd(first)) return;
    const modifyStart = previous !== null && (
      range.compareBoundaryPoints(Range.END_TO_END, previous) === 0
      || range.compareBoundaryPoints(Range.START_TO_END, previous) === 0
    );
    const anchor = movingEnd(range, modifyStart);
    const layer = anchor?.parentElement?.closest<HTMLElement>(".textLayer");
    const end = layer ? layers.get(layer) : undefined;
    if (anchor && layer && end) {
      end.style.width = layer.style.width;
      end.style.height = layer.style.height;
      anchor.parentElement!.insertBefore(end, modifyStart ? anchor : anchor.nextSibling);
    }
    previous = range.cloneRange();
  }, { signal });
  return () => controller.abort();
}

/** Gives a rendered text layer its `endOfContent` box. Returns the undo. */
export function attachTextLayerSelection(layer: HTMLElement): () => void {
  const end = document.createElement("div");
  end.className = "endOfContent";
  layer.append(end);
  const onMouseDown = () => layer.classList.add("selecting");
  layer.addEventListener("mousedown", onMouseDown);
  layers.set(layer, end);
  stopListening ??= listen();
  return () => {
    layer.removeEventListener("mousedown", onMouseDown);
    layer.classList.remove("selecting");
    end.remove();
    layers.delete(layer);
    if (layers.size === 0) {
      stopListening?.();
      stopListening = null;
    }
  };
}
