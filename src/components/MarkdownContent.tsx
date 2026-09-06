import { memo, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import type { ComponentProps } from "react";
import ReactMarkdown from "react-markdown";
import rehypeKatex from "rehype-katex";
import remarkBreaks from "remark-breaks";
import remarkGfm from "remark-gfm";
import remarkMath from "remark-math";
import { useAppearance } from "../lib/appearance";

interface MarkdownContentProps {
  content: string;
  className?: string;
  /** Avoid parsing large historical messages until they are close to the viewport. */
  deferOffscreen?: boolean;
  /** Keep active streams mounted even when viewport deferral is enabled. */
  streaming?: boolean;
}

type VisibilityCallback = (visible: boolean) => void;

const visibilityCallbacks = new WeakMap<Element, VisibilityCallback>();
let nearViewportObserver: IntersectionObserver | null = null;

function observeNearViewport(element: Element, callback: VisibilityCallback): () => void {
  if (typeof IntersectionObserver === "undefined") {
    callback(true);
    return () => undefined;
  }
  if (!nearViewportObserver) {
    nearViewportObserver = new IntersectionObserver((entries) => {
      entries.forEach((entry) => visibilityCallbacks.get(entry.target)?.(entry.isIntersecting));
    }, { rootMargin: "1200px 0px" });
  }
  visibilityCallbacks.set(element, callback);
  nearViewportObserver.observe(element);
  return () => {
    visibilityCallbacks.delete(element);
    nearViewportObserver?.unobserve(element);
  };
}

function estimateMarkdownHeight(content: string): number {
  if (!content) return 20;
  // A bounded sample avoids allocating a full array of lines for multi-megabyte
  // historical messages that have not entered the viewport yet.
  const sampleLength = Math.min(content.length, 16_384);
  let sampledVisualLines = 0;
  let lineLength = 0;
  for (let index = 0; index < sampleLength; index += 1) {
    if (content.charCodeAt(index) === 10) {
      sampledVisualLines += Math.max(1, Math.ceil(lineLength / 64));
      lineLength = 0;
    } else lineLength += 1;
  }
  sampledVisualLines += Math.max(1, Math.ceil(lineLength / 64));
  const visualLines = Math.ceil(sampledVisualLines * (content.length / sampleLength));
  return Math.max(20, Math.min(2_000_000, visualLines * 22));
}

function backslashIsEscaped(value: string, index: number) {
  let count = 0;
  for (let cursor = index - 1; cursor >= 0 && value[cursor] === "\\"; cursor -= 1) count += 1;
  return count % 2 === 1;
}

/**
 * remark-math understands dollar delimiters. Models also commonly emit the
 * LaTeX-style \(...\) and \[...\] forms, so normalize those while leaving
 * fenced and inline code untouched.
 */
export function normalizeMathDelimiters(content: string) {
  let result = "";
  let cursor = 0;
  let inlineTicks = 0;
  let fence: { marker: "`" | "~"; length: number } | null = null;
  let lineStart = true;
  let closeFenceAtLineEnd = false;

  while (cursor < content.length) {
    if (lineStart && inlineTicks === 0) {
      const lineEnd = content.indexOf("\n", cursor);
      const currentLine = content.slice(cursor, lineEnd === -1 ? content.length : lineEnd);
      const fenceMatch = currentLine.match(/^ {0,3}(`{3,}|~{3,})/);
      if (fenceMatch) {
        const marker = fenceMatch[1][0] as "`" | "~";
        if (!fence) fence = { marker, length: fenceMatch[1].length };
        else if (
          fence.marker === marker
          && fenceMatch[1].length >= fence.length
          && currentLine.slice(fenceMatch[0].length).trim() === ""
        ) closeFenceAtLineEnd = true;
      }
    }

    const character = content[cursor];
    if (!fence && character === "`") {
      let runLength = 1;
      while (content[cursor + runLength] === "`") runLength += 1;
      if (inlineTicks === 0) inlineTicks = runLength;
      else if (inlineTicks === runLength) inlineTicks = 0;
      result += content.slice(cursor, cursor + runLength);
      cursor += runLength;
      lineStart = false;
      continue;
    }

    if (!fence && inlineTicks === 0 && character === "\\" && !backslashIsEscaped(content, cursor)) {
      const delimiter = content[cursor + 1];
      if (delimiter === "(" || delimiter === ")") {
        result += "$";
        cursor += 2;
        lineStart = false;
        continue;
      }
      if (delimiter === "[" || delimiter === "]") {
        result += "\n$$\n";
        cursor += 2;
        lineStart = false;
        continue;
      }
    }

    if (!fence && inlineTicks === 0 && character === "$" && content[cursor + 1] === "$" && !backslashIsEscaped(content, cursor)) {
      result += "\n$$\n";
      cursor += 2;
      lineStart = false;
      continue;
    }

    result += character;
    cursor += 1;
    lineStart = character === "\n";
    if (lineStart && closeFenceAtLineEnd) {
      fence = null;
      closeFenceAtLineEnd = false;
    }
  }

  return result;
}

/** Safe shared renderer for assistant replies and visible reasoning. */
export const MarkdownContent = memo(function MarkdownContent({
  content,
  className,
  deferOffscreen = false,
  streaming = false
}: MarkdownContentProps) {
  const hostRef = useRef<HTMLDivElement>(null);
  const [nearViewport, setNearViewport] = useState(() => (
    !deferOffscreen || typeof IntersectionObserver === "undefined"
  ));
  const [measuredHeight, setMeasuredHeight] = useState(0);
  const estimatedHeight = useMemo(() => estimateMarkdownHeight(content), [content]);
  const shouldRender = streaming || !deferOffscreen || nearViewport;
  const { singleDollarMath } = useAppearance();
  // Memoize plugin arrays: a new identity makes ReactMarkdown rebuild its mdast
  // pipeline for every streaming token.
  //
  // Obtain the type from ReactMarkdown props rather than importing unified's
  // `PluggableList`; unified is transitive and may change independently.
  const remarkPlugins = useMemo<ComponentProps<typeof ReactMarkdown>["remarkPlugins"]>(
    () => [remarkGfm, remarkBreaks, [remarkMath, { singleDollarTextMath: singleDollarMath }]],
    [singleDollarMath]
  );
  const normalizedContent = useMemo(
    () => shouldRender ? normalizeMathDelimiters(content) : content,
    [content, shouldRender]
  );

  useEffect(() => {
    const host = hostRef.current;
    if (!deferOffscreen || !host) {
      setNearViewport(true);
      return;
    }
    return observeNearViewport(host, setNearViewport);
  }, [deferOffscreen]);

  useLayoutEffect(() => {
    const host = hostRef.current;
    if (!host || !shouldRender) return;
    let frame: number | null = null;
    const measure = () => {
      if (frame !== null) return;
      frame = window.requestAnimationFrame(() => {
        frame = null;
        const height = Math.ceil(host.getBoundingClientRect().height);
        if (height > 0) setMeasuredHeight((current) => current === height ? current : height);
      });
    };
    measure();
    const observer = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(measure);
    observer?.observe(host);
    return () => {
      observer?.disconnect();
      if (frame !== null) window.cancelAnimationFrame(frame);
    };
  }, [content, shouldRender, streaming]);

  // Drives the streaming reveal, writing straight to the DOM rather than
  // through state: it is paint-only decoration, and a re-render here would
  // re-run exactly the Markdown parse that makes streaming expensive.
  //
  // `tick` alternates 0/1 because a CSS animation only restarts when its name
  // changes — the two keyframe sets are identical, and alternating is what
  // makes each commit sweep again. `--stream-reveal-from` is where the previous
  // commit's text ended, so only the newly arrived tail is revealed instead of
  // the whole block flashing every 100 ms.
  const revealedLength = useRef(0);
  const revealTick = useRef(0);
  useLayoutEffect(() => {
    const host = hostRef.current;
    if (!host) return;
    if (!streaming || !shouldRender) {
      revealedLength.current = content.length;
      host.removeAttribute("data-stream-tick");
      host.style.removeProperty("--stream-reveal-from");
      return;
    }
    const previous = revealedLength.current;
    if (content.length <= previous) return;
    revealedLength.current = content.length;
    revealTick.current = revealTick.current === 0 ? 1 : 0;
    const from = Math.max(0, Math.min(100, Math.round((previous / content.length) * 100)));
    host.style.setProperty("--stream-reveal-from", `${from}%`);
    host.setAttribute("data-stream-tick", String(revealTick.current));
  }, [content, shouldRender, streaming]);

  return (
    <div
      ref={hostRef}
      className={`markdown-content${className ? ` ${className}` : ""}`}
      data-markdown-deferred={!shouldRender || undefined}
      style={!shouldRender ? { minHeight: `${measuredHeight || estimatedHeight}px` } : undefined}
    >
      {shouldRender && (
        <ReactMarkdown
          remarkPlugins={remarkPlugins}
          rehypePlugins={[[rehypeKatex, { strict: false, throwOnError: false }]]}
          components={{
            a: ({ node: _node, ...props }) => <a {...props} target="_blank" rel="noreferrer noopener" />,
            table: ({ node: _node, ...props }) => (
              <div className="markdown-content__table-scroll">
                <table {...props} />
              </div>
            ),
            img: ({ node: _node, ...props }) => <img {...props} loading="lazy" referrerPolicy="no-referrer" />
          }}
        >
          {normalizedContent}
        </ReactMarkdown>
      )}
    </div>
  );
});
