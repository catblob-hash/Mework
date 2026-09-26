/**
 * The streaming reveal: the text each commit adds sweeps in from left to right,
 * so a stream reads as being written instead of as runs of text blinking into
 * place ten times a second.
 *
 * "What this commit added" is decided on the text as rendered, not on the
 * source. A Markdown source and its rendering are not the same string (`**bo`
 * renders as `**bo` until the closing `**` turns it into bold `bo`), and the
 * text that changed is what has to move — so the split is the longest prefix
 * this render shares with the one already on screen.
 *
 * The new text is wrapped in `.stream-fresh` spans, whose mask the stylesheet
 * animates. Wrapping text rather than masking the whole block is what keeps the
 * sweep on the new words: a block-wide mask runs across the right-hand side of
 * every line at once, settled text included.
 */
import type { Element, ElementContent, Root, RootContent, Text } from "hast";
import { MODEL_STREAM_COMMIT_INTERVAL_MS } from "./modelStream";

/**
 * One sweep lasts one commit, so each commit's text finishes arriving just as
 * the next begins and the stream moves continuously.
 */
export const STREAM_REVEAL_MS = MODEL_STREAM_COMMIT_INTERVAL_MS;

export interface StreamRevealSplit {
  /** The text the split was taken against. */
  text: string;
  /** How much of `text` was already on screen; everything after it is new. */
  settled: number;
  /**
   * Alternates each time new text arrives. A CSS animation only replays when
   * its name changes, and React keeps the same span from one commit to the
   * next, so the two tick classes carry identical keyframes under two names.
   */
  tick: 0 | 1;
}

function commonPrefixLength(left: string, right: string): number {
  const limit = Math.min(left.length, right.length);
  let index = 0;
  while (index < limit && left.charCodeAt(index) === right.charCodeAt(index)) index += 1;
  // Never split a surrogate pair: half a character is not a glyph to sweep.
  if (index > 0 && index < right.length) {
    const code = right.charCodeAt(index - 1);
    if (code >= 0xd800 && code <= 0xdbff) index -= 1;
  }
  return index;
}

/**
 * The split for `text`, given the split last put on screen.
 *
 * Nothing on screen yet means everything is new. The same text again — a
 * render for some other reason — keeps the split it had, so a sweep still in
 * flight is not cut short.
 */
export function nextStreamRevealSplit(previous: StreamRevealSplit | null, text: string): StreamRevealSplit {
  if (!previous) return { text, settled: 0, tick: 0 };
  if (previous.text === text) return previous;
  return {
    text,
    settled: commonPrefixLength(previous.text, text),
    tick: previous.tick === 0 ? 1 : 0
  };
}

export function streamFreshClassName(tick: 0 | 1): string {
  return `stream-fresh stream-fresh--${tick}`;
}

/** Elements whose text is drawn from the tree by their own renderer, or is not text at all. */
const OPAQUE_TAGS = new Set(["pre", "code", "svg", "math", "script", "style", "textarea"]);

/** Parents whose content model has no room for a span, only for the elements they list. */
const LIST_LIKE_TAGS = new Set(["ul", "ol", "table", "thead", "tbody", "tfoot", "tr", "colgroup"]);

interface TextSlot {
  node: Text;
  parent: Element;
  /** Where the node starts in the rendered text. */
  offset: number;
}

/**
 * The rehype side of the reveal, for Markdown.
 *
 * Reports the split through `onSplit` so the caller can hand it back as
 * `previous` on the next commit. The new text is spread across one sweep in
 * reading order: each span starts when the text before it has finished, and
 * takes its share of the commit, so text that crosses a paragraph or a bold run
 * still arrives as one stroke.
 *
 * Code and math are left alone. Their renderers read the tree's text rather
 * than its children, and a span there would either be dropped or break the
 * highlighter.
 */
export function rehypeStreamReveal(options: {
  previous: StreamRevealSplit | null;
  onSplit: (split: StreamRevealSplit) => void;
}) {
  return (tree: Root) => {
    const slots: TextSlot[] = [];
    let text = "";
    const visit = (parent: Root | Element, opaque: boolean) => {
      for (const child of parent.children as Array<RootContent | ElementContent>) {
        if (child.type === "text") {
          if (!opaque && parent.type === "element" && !LIST_LIKE_TAGS.has(parent.tagName) && /\S/.test(child.value)) {
            slots.push({ node: child, parent, offset: text.length });
          }
          text += child.value;
        } else if (child.type === "element") {
          visit(child, opaque || OPAQUE_TAGS.has(child.tagName));
        }
      }
    };
    visit(tree, false);

    const split = nextStreamRevealSplit(options.previous, text);
    options.onSplit(split);
    const fresh = text.length - split.settled;
    if (fresh <= 0) return;

    for (const { node, parent, offset } of slots) {
      if (offset + node.value.length <= split.settled) continue;
      const cut = Math.max(0, split.settled - offset);
      const head = node.value.slice(0, cut);
      const tail = node.value.slice(cut);
      const start = offset + cut - split.settled;
      const span: Element = {
        type: "element",
        tagName: "span",
        properties: {
          className: streamFreshClassName(split.tick).split(" "),
          style: `animation-delay: ${(start / fresh) * STREAM_REVEAL_MS}ms; `
            + `animation-duration: ${(tail.length / fresh) * STREAM_REVEAL_MS}ms`
        },
        children: [{ type: "text", value: tail }]
      };
      const index = parent.children.indexOf(node);
      parent.children.splice(index, 1, ...(head ? [{ type: "text", value: head } as Text, span] : [span]));
    }
  };
}
