import { useLayoutEffect, useRef } from "react";
import { nextStreamRevealSplit, streamFreshClassName } from "../lib/streamReveal";
import type { StreamRevealSplit } from "../lib/streamReveal";

/**
 * Plain text that is still arriving, with what each commit added sweeping in
 * from the left — the one-line counterpart of the Markdown reveal.
 *
 * When the text is a continuation, only the continuation moves; when it is a
 * different line altogether, the whole line is new and sweeps in.
 */
export function StreamRevealText({ text }: { text: string }) {
  // Only a commit moves the split on, so a render React discards, or
  // StrictMode's second one, takes the same split as the first.
  const shown = useRef<StreamRevealSplit | null>(null);
  const split = nextStreamRevealSplit(shown.current, text);
  useLayoutEffect(() => {
    shown.current = split;
  });
  const fresh = text.slice(split.settled);
  return (
    <>
      {text.slice(0, split.settled)}
      {fresh && <span className={streamFreshClassName(split.tick)}>{fresh}</span>}
    </>
  );
}
