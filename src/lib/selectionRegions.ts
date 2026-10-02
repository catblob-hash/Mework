/**
 * Text selection is opt-in, and a selection stays inside the region it began in.
 *
 * Most of the interface is controls and labels, so the page is `user-select: none`
 * and only content turns selection back on: a message, a reasoning body, one part
 * of a tool's result, a file preview. A *region* is the outermost element of one
 * such opted-in subtree — it begins where `user-select: text` is declared and takes
 * in everything beneath it, so a message is one region however many paragraphs and
 * code blocks it holds, while a command and its output, opted in separately under a
 * wrapper that is not, are two. Dragging past a region's edge, extending with the
 * keyboard, or Select All never carries a selection into the next message or the
 * other half of a tool: it stops at the edge of the region it started in.
 *
 * Two shapes of content need more than that. Text laid out in separate boxes with
 * unselectable space between them — a PDF's pages — names its region outright with
 * `data-selection-region`: a selection runs from one page into the next, and the
 * space between them still starts none. Text laid out in columns — a side-by-side
 * diff — marks its cells with `data-selection-column` under a
 * `data-selection-columns` host: a press in one column makes the others
 * unselectable until the next press (the host's CSS reads
 * `data-selection-column-active`), so a selection runs down one side only.
 *
 * The highlight is painted here too, through the CSS Custom Highlight API, rather
 * than by the engine. WebKit fills a selection's "gaps" in the selection colour —
 * the blank lines between paragraphs, a block's padding, the rest of every line out
 * to the edge of the window — so a selection of two sentences lights up a band the
 * width of the app. The highlight covers glyphs and nothing else, and leaves out any
 * text inside the region that opted back out (line numbers, labels, screen-reader
 * copies). Editable fields and `[data-native-selection]` subtrees keep the engine's
 * own paint (`base.css` hides it everywhere else while this is installed).
 */

const SELECTION_HIGHLIGHT = "mework-selection";

/** Fields that take typing, and so own their selection and Select All; a checkbox does not. */
const EDITABLE_SELECTOR = [
  "textarea",
  "select",
  "[contenteditable]:not([contenteditable=\"false\"])",
  "input:not([type=\"checkbox\"], [type=\"radio\"], [type=\"button\"], [type=\"submit\"], [type=\"reset\"], [type=\"range\"], [type=\"color\"], [type=\"file\"], [type=\"image\"])"
].join(", ");
const NATIVE_SELECTION_ATTRIBUTE = "data-native-selection";
const REGION_SELECTOR = "[data-selection-region]";
const COLUMN_ATTRIBUTE = "data-selection-column";
const COLUMN_HOST_SELECTOR = "[data-selection-columns]";
const ACTIVE_COLUMN_ATTRIBUTE = "data-selection-column-active";

type SelectableCache = Map<Element, boolean>;

function elementOf(node: Node | null | undefined): Element | null {
  if (!node) return null;
  return node.nodeType === Node.ELEMENT_NODE ? node as Element : node.parentElement;
}

/**
 * Whether text in `element` can be selected. WebKit only knows the prefixed
 * property, and an engine that follows the spec leaves `user-select` uninherited
 * with `auto` deferring to the parent, so `auto` is resolved by walking up.
 */
function isSelectable(element: Element, cache?: SelectableCache): boolean {
  const known = cache?.get(element);
  if (known !== undefined) return known;
  const style = window.getComputedStyle(element);
  const value = style.userSelect || style.webkitUserSelect || "auto";
  const parent = element.parentElement;
  const selectable = value === "auto"
    ? (element.matches(EDITABLE_SELECTOR) || !parent || isSelectable(parent, cache))
    : value !== "none";
  cache?.set(element, selectable);
  return selectable;
}

/**
 * The region `node` belongs to: the outermost element of its selectable subtree,
 * or the element that names a region around it.
 */
export function selectionRegion(node: Node | null | undefined, cache: SelectableCache = new Map()): Element | null {
  let region = elementOf(node);
  if (!region || !isSelectable(region, cache)) return null;
  for (let parent = region.parentElement; parent && isSelectable(parent, cache); parent = parent.parentElement) {
    region = parent;
  }
  return region.closest(REGION_SELECTOR) ?? region;
}

function isEditableElement(element: Element | null): boolean {
  return Boolean(element && ((element as HTMLElement).isContentEditable || element.matches(EDITABLE_SELECTOR)));
}

/** A selection an editable field owns: the field keeps it, and the engine paints it. */
function isEditableSelection(selection: Selection): boolean {
  if (isEditableElement(document.activeElement)) return true;
  const anchor = elementOf(selection.anchorNode);
  return Boolean(anchor && (anchor as HTMLElement).isContentEditable);
}

function hasVisibleText(node: Text): boolean {
  return node.data.trim().length > 0;
}

function lastDescendant(node: Node): Node {
  let last = node;
  while (last.lastChild) last = last.lastChild;
  return last;
}

/**
 * Where a selection pinned to one edge of `region` ends: the edge of its first or
 * last selectable text, so a clamped selection does not reach past the region's
 * words into whatever trails them.
 */
function regionEdge(region: Element, edge: "start" | "end", cache: SelectableCache = new Map()): [Node, number] {
  const walker = document.createTreeWalker(region, NodeFilter.SHOW_TEXT);
  const accept = (node: Node | null): node is Text => (
    node !== null && node.nodeType === Node.TEXT_NODE && hasVisibleText(node as Text)
      && node.parentElement !== null && isSelectable(node.parentElement, cache)
  );
  if (edge === "start") {
    for (let node = walker.nextNode(); node; node = walker.nextNode()) {
      if (accept(node)) return [node, 0];
    }
    return [region, 0];
  }
  walker.currentNode = lastDescendant(region);
  for (let node: Node | null = walker.currentNode; node && node !== region; node = walker.previousNode()) {
    if (accept(node)) return [node, node.data.length];
  }
  return [region, region.childNodes.length];
}

/**
 * Pulls the selection's moving end back to the edge of the region its anchor is
 * in. Returns whether it had to: the selection changes, and so does its range.
 */
export function clampSelectionToRegion(selection: Selection, cache: SelectableCache = new Map()): boolean {
  const { anchorNode, anchorOffset, focusNode, focusOffset } = selection;
  if (!anchorNode || !focusNode || selection.isCollapsed) return false;
  const region = selectionRegion(anchorNode, cache);
  if (!region) return false;
  const bounds = document.createRange();
  bounds.selectNodeContents(region);
  let side: number;
  try {
    side = bounds.comparePoint(focusNode, focusOffset);
  } catch {
    return false;
  }
  if (side === 0) return false;
  const [edgeNode, edgeOffset] = regionEdge(region, side > 0 ? "end" : "start", cache);
  selection.setBaseAndExtent(anchorNode, anchorOffset, edgeNode, edgeOffset);
  return true;
}

/**
 * The pieces of `range` this module paints: each run of selectable text inside
 * it, minus text the engine keeps painting itself and placeholder text that only
 * holds a line open (a blank diff line's single space).
 */
export function paintedTextRanges(range: Range): Range[] {
  const cache: SelectableCache = new Map();
  const native = new Map<Element, boolean>();
  const paintedByEngine = (element: Element): boolean => {
    const known = native.get(element);
    if (known !== undefined) return known;
    const result = element.hasAttribute(NATIVE_SELECTION_ATTRIBUTE)
      || (element.parentElement !== null && paintedByEngine(element.parentElement));
    native.set(element, result);
    return result;
  };
  const root = range.commonAncestorContainer;
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
  let node: Node | null;
  const start = range.startContainer;
  if (start.nodeType === Node.TEXT_NODE) {
    walker.currentNode = start;
    node = start;
  } else {
    const boundary = start.childNodes[range.startOffset];
    walker.currentNode = boundary ?? lastDescendant(start);
    node = boundary?.nodeType === Node.TEXT_NODE ? boundary : walker.nextNode();
  }
  const pieces: Range[] = [];
  for (; node; node = walker.nextNode()) {
    if (!range.intersectsNode(node)) break;
    const text = node as Text;
    const parent = text.parentElement;
    if (!parent || !isSelectable(parent, cache) || paintedByEngine(parent)) continue;
    if (!hasVisibleText(text) && parent.childNodes.length === 1) continue;
    const from = text === range.startContainer ? range.startOffset : 0;
    const to = text === range.endContainer ? range.endOffset : text.data.length;
    if (from >= to) continue;
    const piece = document.createRange();
    piece.setStart(text, from);
    piece.setEnd(text, to);
    pieces.push(piece);
  }
  return pieces;
}

/** Selects all of `region`'s selectable text. */
function selectRegion(selection: Selection, region: Element): void {
  const cache: SelectableCache = new Map();
  const [startNode, startOffset] = regionEdge(region, "start", cache);
  const [endNode, endOffset] = regionEdge(region, "end", cache);
  selection.setBaseAndExtent(startNode, startOffset, endNode, endOffset);
}

const isMac = typeof navigator !== "undefined" && navigator.platform.startsWith("Mac");

/**
 * A press on a scroll container's own scrollbar. Clicking one never touches the
 * selection in the engine either, and dragging it is how a reader gets to the
 * end of what they selected.
 */
function isOnScrollbar(event: PointerEvent, element: Element): boolean {
  if (!(element instanceof HTMLElement) || !element.clientWidth || !element.clientHeight) return false;
  const box = element.getBoundingClientRect();
  return event.clientX - box.left - element.clientLeft >= element.clientWidth
    || event.clientY - box.top - element.clientTop >= element.clientHeight;
}

/** Installs region-scoped selection on the document. Returns its uninstaller. */
export function installSelectionRegions(): () => void {
  const highlights = typeof CSS !== "undefined" ? CSS.highlights : undefined;
  const highlight = highlights && typeof Highlight === "function" ? new Highlight() : null;
  if (highlight) {
    highlights!.set(SELECTION_HIGHLIGHT, highlight);
    document.documentElement.dataset.selectionPaint = "highlight";
  }
  // The region of the last press, for Select All after a click that left no caret.
  let pressedRegion: Element | null = null;
  // The column host whose active column the last press chose.
  let columnHost: Element | null = null;
  // A press outside every region that has not been released yet.
  let pressOutsideRegions = false;
  let frame = 0;

  const update = () => {
    frame = 0;
    highlight?.clear();
    const selection = document.getSelection();
    if (!selection || selection.rangeCount === 0 || selection.isCollapsed || isEditableSelection(selection)) return;
    clampSelectionToRegion(selection);
    if (!highlight || selection.rangeCount === 0) return;
    for (const piece of paintedTextRanges(selection.getRangeAt(0))) highlight.add(piece);
  };
  const scheduleUpdate = () => {
    if (!frame) frame = window.requestAnimationFrame(update);
  };

  // A press on text starts a new selection there; the engine does that. A press
  // anywhere else would leave the old one standing, because unselectable content
  // cannot start a selection of its own, so it is let go here.
  // A press in a column picks that column before anything reads whether the
  // press landed on selectable text: the column it picks may be the one that
  // was unselectable until now. Extending with Shift keeps the column it has.
  const pressColumn = (target: Element | null) => {
    const column = target?.closest(`[${COLUMN_ATTRIBUTE}]`) ?? null;
    const host = column?.closest(COLUMN_HOST_SELECTOR) ?? null;
    if (columnHost && columnHost !== host) columnHost.removeAttribute(ACTIVE_COLUMN_ATTRIBUTE);
    columnHost = host;
    host?.setAttribute(ACTIVE_COLUMN_ATTRIBUTE, column?.getAttribute(COLUMN_ATTRIBUTE) ?? "");
  };

  const onPointerDown = (event: PointerEvent) => {
    if (event.button !== 0) return;
    const target = event.target instanceof Element ? event.target : null;
    if (!event.shiftKey) pressColumn(target);
    pressedRegion = selectionRegion(target);
    pressOutsideRegions = false;
    if (!target || pressedRegion || event.shiftKey || isEditableElement(target.closest(EDITABLE_SELECTOR))) return;
    pressOutsideRegions = true;
    if (isOnScrollbar(event, target)) return;
    const selection = document.getSelection();
    if (selection && selection.rangeCount > 0 && !isEditableSelection(selection)) selection.removeAllRanges();
  };
  const onPointerUp = () => {
    pressOutsideRegions = false;
  };

  // Chromium never starts a selection on unselectable content, but WebKit does,
  // from whatever text lies nearest the press: a drag through a message's
  // padding, or the gutter beside it, would select the message. No selection
  // starts outside a region, and none starts later in a drag that began outside
  // one — WebKit tries again once the pointer reaches text.
  const onSelectStart = (event: Event) => {
    const target = event.target instanceof Node ? event.target : null;
    if (isEditableElement(elementOf(target)?.closest(EDITABLE_SELECTOR) ?? null)) return;
    if (pressOutsideRegions || !selectionRegion(target)) event.preventDefault();
  };

  // Select All selects the region the selection or the last press is in, and
  // nothing when there is neither: the engine's would select every region at once.
  const onKeyDown = (event: KeyboardEvent) => {
    if (event.defaultPrevented || event.altKey || event.shiftKey || event.key.toLowerCase() !== "a") return;
    if (isMac ? !event.metaKey || event.ctrlKey : !event.ctrlKey || event.metaKey) return;
    const target = event.target instanceof Element ? event.target : null;
    if (isEditableElement(target)) return;
    const selection = document.getSelection();
    if (!selection || isEditableSelection(selection)) return;
    event.preventDefault();
    const region = (selection.rangeCount > 0 ? selectionRegion(selection.anchorNode) : null)
      ?? (pressedRegion?.isConnected ? pressedRegion : null);
    if (region) selectRegion(selection, region);
  };

  document.addEventListener("selectionchange", scheduleUpdate);
  document.addEventListener("selectstart", onSelectStart);
  document.addEventListener("pointerdown", onPointerDown, true);
  document.addEventListener("pointerup", onPointerUp, true);
  document.addEventListener("pointercancel", onPointerUp, true);
  document.addEventListener("keydown", onKeyDown);
  return () => {
    document.removeEventListener("selectionchange", scheduleUpdate);
    document.removeEventListener("selectstart", onSelectStart);
    document.removeEventListener("pointerdown", onPointerDown, true);
    document.removeEventListener("pointerup", onPointerUp, true);
    document.removeEventListener("pointercancel", onPointerUp, true);
    document.removeEventListener("keydown", onKeyDown);
    if (frame) window.cancelAnimationFrame(frame);
    columnHost?.removeAttribute(ACTIVE_COLUMN_ATTRIBUTE);
    if (highlight) {
      highlights!.delete(SELECTION_HIGHLIGHT);
      delete document.documentElement.dataset.selectionPaint;
    }
  };
}
