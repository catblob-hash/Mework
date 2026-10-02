import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type ClipboardEvent as ReactClipboardEvent,
  type ReactNode,
  type RefObject
} from "react";
import { useI18n } from "../i18n";
import {
  DRAFT_CLIPBOARD_TYPE,
  adoptDraftClipboard,
  draftClipboard,
  foldsPastedText,
  nextPastedTextId,
  normalizePastedText,
  parseDraftClipboard,
  pastedTextExtraLines,
  pastedTextRangeAround,
  pastedTextRanges,
  type DraftClipboard,
  type PastedText,
  type PastedTextLabeler,
  type PastedTextRange
} from "../lib/pastedText";

const NBSP = " ";

/**
 * The last copy out of any box. A clipboard that kept the plain text but dropped
 * the box's own type still pastes back as the tags it was copied as.
 */
let lastDraftCopy: { plain: string; draft: DraftClipboard } | null = null;

/** The textarea properties that decide where its text wraps, copied onto the layer. */
const MIRRORED_STYLE = [
  "fontFamily",
  "fontSize",
  "fontStyle",
  "fontVariant",
  "fontWeight",
  "fontStretch",
  "fontFeatureSettings",
  "fontVariationSettings",
  "fontKerning",
  "lineHeight",
  "letterSpacing",
  "wordSpacing",
  "textIndent",
  "textTransform",
  "textRendering",
  "tabSize",
  "whiteSpace",
  "overflowWrap",
  "wordBreak",
  "direction",
  "textAlign",
  "paddingTop",
  "paddingRight",
  "paddingBottom",
  "paddingLeft"
] as const;

function syncLayer(textarea: HTMLTextAreaElement, layer: HTMLDivElement): void {
  const style = window.getComputedStyle(textarea);
  for (const property of MIRRORED_STYLE) layer.style[property] = style[property];
  // The text column, not the box: a classic scrollbar narrows where the text wraps.
  layer.style.width = `${textarea.clientWidth}px`;
  layer.style.marginLeft = `${textarea.clientLeft}px`;
  layer.style.marginTop = `${textarea.clientTop}px`;
  layer.scrollTop = textarea.scrollTop;
  layer.scrollLeft = textarea.scrollLeft;
}

/**
 * Replaces the selection the way typing would. `insertText` keeps the edit on
 * the textarea's own undo stack; the fallback (an engine without `execCommand`)
 * still reaches React's `onChange` through the input event.
 */
function replaceSelection(textarea: HTMLTextAreaElement, text: string): void {
  if (document.activeElement !== textarea) textarea.focus();
  const { selectionStart, selectionEnd } = textarea;
  if (!text && selectionStart === selectionEnd) return;
  const done = typeof document.execCommand === "function"
    && document.execCommand(text ? "insertText" : "delete", false, text);
  if (done) return;
  textarea.setRangeText(text, selectionStart, selectionEnd, "end");
  textarea.dispatchEvent(new Event("input", { bubbles: true }));
}

function readClipboardType(data: DataTransfer, type: string): string {
  try {
    return data.getData(type);
  } catch {
    return "";
  }
}

function PastedTextLayer({
  textareaRef,
  layerRef,
  value,
  ranges
}: {
  textareaRef: RefObject<HTMLTextAreaElement | null>;
  layerRef: RefObject<HTMLDivElement | null>;
  value: string;
  ranges: readonly PastedTextRange[];
}) {
  useLayoutEffect(() => {
    if (textareaRef.current && layerRef.current) syncLayer(textareaRef.current, layerRef.current);
  });
  useEffect(() => {
    const textarea = textareaRef.current;
    if (!textarea || typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(() => {
      if (layerRef.current) syncLayer(textarea, layerRef.current);
    });
    observer.observe(textarea);
    return () => observer.disconnect();
  }, [layerRef, textareaRef]);

  const parts: ReactNode[] = [];
  let at = 0;
  for (const range of ranges) {
    if (range.start > at) parts.push(value.slice(at, range.start));
    parts.push(
      <span key={range.start} className="pasted-text-tag">{value.slice(range.start, range.end)}</span>
    );
    at = range.end;
  }
  // The rest of the text still decides where the last tag's line breaks.
  parts.push(value.slice(at));
  return (
    <div ref={layerRef} className="pasted-text-layer" aria-hidden="true">{parts}</div>
  );
}

export interface PastedTextTags {
  /**
   * Draws the tags behind the textarea. Render it just before the textarea,
   * inside an element with the `pasted-text-host` class.
   */
  layer: ReactNode;
  /**
   * Folds a long paste into a tag, and brings text copied out of a box back as
   * it was copied. Returns whether it took the paste; the caller handles files
   * first.
   */
  onPaste: (event: ReactClipboardEvent<HTMLTextAreaElement>) => boolean;
}

/**
 * Long pastes as tags in a textarea (`lib/pastedText.ts`). A tag is its label's
 * text, so the textarea selects, copies, drags and undoes it natively; this
 * hook keeps the caret from stopping inside one, deletes one whole, and puts
 * the text behind a tag on the clipboard when it is copied.
 */
export function usePastedTextTags({
  textareaRef,
  value,
  pastes,
  onPastesChange
}: {
  textareaRef: RefObject<HTMLTextAreaElement | null>;
  value: string;
  pastes: readonly PastedText[];
  onPastesChange: (update: (current: readonly PastedText[]) => PastedText[]) => void;
}): PastedTextTags {
  const { t } = useI18n();
  const ranges = useMemo(() => pastedTextRanges(value, pastes), [pastes, value]);
  const rangesRef = useRef(ranges);
  rangesRef.current = ranges;
  const pastesRef = useRef(pastes);
  pastesRef.current = pastes;
  const layerRef = useRef<HTMLDivElement>(null);
  const [textarea, setTextarea] = useState<HTMLTextAreaElement | null>(null);
  useLayoutEffect(() => {
    if (textareaRef.current !== textarea) setTextarea(textareaRef.current);
  });

  const labelFor = useCallback<PastedTextLabeler>((id, extraLines) => {
    const name = extraLines > 0
      ? t("粘贴文本 #{id} +{lines} 行", "Pasted text #{id} +{lines} lines", { id, lines: extraLines })
      : t("粘贴文本 #{id}", "Pasted text #{id}", { id });
    // No-break spaces pad the tag and keep a label from wrapping partway through.
    return `${NBSP}${name.replace(/ /g, NBSP)}${NBSP}`;
  }, [t]);

  useEffect(() => {
    if (!textarea) return;
    let composing = false;
    let pointerDown = false;
    let lastFocus = textarea.selectionEnd;

    const copySelection = (event: ClipboardEvent): boolean => {
      const { selectionStart: start, selectionEnd: end } = textarea;
      if (start === end || !event.clipboardData) return false;
      const payload = draftClipboard(textarea.value, start, end, pastesRef.current);
      event.preventDefault();
      event.clipboardData.setData("text/plain", payload.plain);
      try {
        event.clipboardData.setData(DRAFT_CLIPBOARD_TYPE, JSON.stringify(payload.draft));
      } catch {
        // An engine that refuses custom types still has `lastDraftCopy`.
      }
      lastDraftCopy = payload;
      return true;
    };
    const onCopy = (event: ClipboardEvent) => {
      copySelection(event);
    };
    const onCut = (event: ClipboardEvent) => {
      if (copySelection(event)) replaceSelection(textarea, "");
    };

    /**
     * A caret never rests inside a label, and a selection never ends partway
     * through one. A caret the keyboard moved in carries on through in the
     * direction it came from; one a click put there goes to the nearer edge.
     */
    const snap = (clicked = false) => {
      if (composing || pointerDown || document.activeElement !== textarea) return;
      const current = rangesRef.current;
      const { selectionStart: start, selectionEnd: end, selectionDirection } = textarea;
      if (start === end) {
        const range = pastedTextRangeAround(current, start);
        let caret = start;
        if (range) {
          caret = !clicked && lastFocus <= range.start
            ? range.end
            : !clicked && lastFocus >= range.end
              ? range.start
              : start - range.start < range.end - start ? range.start : range.end;
          textarea.setSelectionRange(caret, caret);
        }
        lastFocus = caret;
        return;
      }
      const from = pastedTextRangeAround(current, start)?.start ?? start;
      const to = pastedTextRangeAround(current, end)?.end ?? end;
      if (from !== start || to !== end) textarea.setSelectionRange(from, to, selectionDirection);
      lastFocus = selectionDirection === "backward" ? from : to;
    };

    const onKeyDown = (event: KeyboardEvent) => {
      if (composing || event.isComposing) return;
      const current = rangesRef.current;
      if (!current.length) return;
      const { selectionStart: start, selectionEnd: end, selectionDirection } = textarea;
      const collapsed = start === end;
      // Selecting the whole label first lets the key's own default action delete
      // it, so the deletion stays one step on the undo stack.
      if (event.key === "Backspace" && collapsed) {
        const range = current.find((candidate) => candidate.start < start && start <= candidate.end);
        if (range) textarea.setSelectionRange(range.start, range.end);
        return;
      }
      if (event.key === "Delete" && collapsed) {
        const range = current.find((candidate) => candidate.start <= start && start < candidate.end);
        if (range) textarea.setSelectionRange(range.start, range.end);
        return;
      }
      if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return;
      if (event.altKey || event.metaKey || event.ctrlKey) return;
      // Without Shift a selection collapses to its edge, which snapping already put outside a label.
      if (!collapsed && !event.shiftKey) return;
      const backward = selectionDirection === "backward";
      const anchor = backward ? end : start;
      const focus = backward ? start : end;
      const left = event.key === "ArrowLeft";
      const range = left
        ? current.find((candidate) => candidate.start < focus && focus <= candidate.end)
        : current.find((candidate) => candidate.start <= focus && focus < candidate.end);
      if (!range) return;
      event.preventDefault();
      const next = left ? range.start : range.end;
      if (event.shiftKey) {
        textarea.setSelectionRange(Math.min(anchor, next), Math.max(anchor, next), next < anchor ? "backward" : "forward");
      } else {
        textarea.setSelectionRange(next, next);
      }
      lastFocus = next;
    };

    const onMouseDown = () => {
      pointerDown = true;
    };
    const release = () => {
      if (!pointerDown) return;
      pointerDown = false;
      snap(true);
    };
    const onKeyUp = () => snap();
    const onSelect = () => snap();
    const onSelectionChange = () => {
      if (document.activeElement === textarea) snap();
    };
    const onCompositionStart = () => {
      composing = true;
    };
    const onCompositionEnd = () => {
      composing = false;
    };
    const onScroll = () => {
      if (!layerRef.current) return;
      layerRef.current.scrollTop = textarea.scrollTop;
      layerRef.current.scrollLeft = textarea.scrollLeft;
    };

    textarea.addEventListener("copy", onCopy);
    textarea.addEventListener("cut", onCut);
    textarea.addEventListener("keydown", onKeyDown);
    textarea.addEventListener("keyup", onKeyUp);
    textarea.addEventListener("select", onSelect);
    textarea.addEventListener("mousedown", onMouseDown);
    textarea.addEventListener("dragend", release);
    textarea.addEventListener("drop", release);
    textarea.addEventListener("blur", release);
    textarea.addEventListener("compositionstart", onCompositionStart);
    textarea.addEventListener("compositionend", onCompositionEnd);
    textarea.addEventListener("scroll", onScroll);
    window.addEventListener("mouseup", release);
    document.addEventListener("selectionchange", onSelectionChange);
    return () => {
      textarea.removeEventListener("copy", onCopy);
      textarea.removeEventListener("cut", onCut);
      textarea.removeEventListener("keydown", onKeyDown);
      textarea.removeEventListener("keyup", onKeyUp);
      textarea.removeEventListener("select", onSelect);
      textarea.removeEventListener("mousedown", onMouseDown);
      textarea.removeEventListener("dragend", release);
      textarea.removeEventListener("drop", release);
      textarea.removeEventListener("blur", release);
      textarea.removeEventListener("compositionstart", onCompositionStart);
      textarea.removeEventListener("compositionend", onCompositionEnd);
      textarea.removeEventListener("scroll", onScroll);
      window.removeEventListener("mouseup", release);
      document.removeEventListener("selectionchange", onSelectionChange);
    };
  }, [textarea]);

  const onPaste = useCallback((event: ReactClipboardEvent<HTMLTextAreaElement>): boolean => {
    const target = event.currentTarget;
    const plain = normalizePastedText(event.clipboardData.getData("text/plain"));
    const copied = parseDraftClipboard(readClipboardType(event.clipboardData, DRAFT_CLIPBOARD_TYPE))
      ?? (lastDraftCopy && plain && normalizePastedText(lastDraftCopy.plain) === plain ? lastDraftCopy.draft : null);
    if (copied) {
      // Text copied out of a box comes back as it was copied, however long.
      if (!copied.pastes.length && copied.text === plain) return true;
      event.preventDefault();
      const { text, added } = adoptDraftClipboard(copied, pastesRef.current, labelFor);
      if (added.length) {
        pastesRef.current = [...pastesRef.current, ...added];
        onPastesChange((current) => [...current, ...added]);
      }
      replaceSelection(target, text);
      return true;
    }
    if (!foldsPastedText(plain)) return false;
    event.preventDefault();
    const id = nextPastedTextId(pastesRef.current);
    const paste: PastedText = { id, label: labelFor(id, pastedTextExtraLines(plain)), content: plain };
    pastesRef.current = [...pastesRef.current, paste];
    onPastesChange((current) => [...current, paste]);
    replaceSelection(target, paste.label);
    return true;
  }, [labelFor, onPastesChange]);

  const layer = ranges.length
    ? <PastedTextLayer textareaRef={textareaRef} layerRef={layerRef} value={value} ranges={ranges} />
    : null;
  return { layer, onPaste };
}
