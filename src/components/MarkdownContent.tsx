import type { Element as HastElement, ElementContent } from "hast";
import { memo, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import type { ComponentProps, ImgHTMLAttributes, MouseEvent } from "react";
import ReactMarkdown, { defaultUrlTransform } from "react-markdown";
import type { Components, ExtraProps } from "react-markdown";
import rehypeRaw from "rehype-raw";
import rehypeSanitize from "rehype-sanitize";
import remarkBreaks from "remark-breaks";
import remarkGfm from "remark-gfm";
import remarkMath from "remark-math";
import { useAppearance } from "../lib/appearance";
import { localLinkTarget, scrollToFragment } from "../lib/documentLinks";
import { externalHttpUrl } from "../lib/externalLinks";
import { fenceLanguage } from "../lib/fileViewers";
import { MARKDOWN_HTML_SCHEMA, mayContainHtml } from "../lib/markdownHtml";
import remarkPathLinks from "../lib/remarkPathLinks";
import { rehypeStreamReveal } from "../lib/streamReveal";
import type { StreamRevealSplit } from "../lib/streamReveal";
import { MarkdownCodeBlock } from "./CodeBlock";
import { MathFormula } from "./MathFormula";

interface MarkdownContentProps {
  content: string;
  className?: string;
  /** Avoid parsing large historical messages until they are close to the viewport. */
  deferOffscreen?: boolean;
  /** Keep active streams mounted even when viewport deferral is enabled. */
  streaming?: boolean;
  /**
   * Turn file paths into clickable nodes. Callers opt in per surface so that
   * user-authored text is never parsed this way, including when the appearance
   * setting renders user messages as Markdown.
   */
  linkifyPaths?: boolean;
  /** Directory relative paths resolve against, read at click time. */
  pathBaseDir?: string | null;
  /**
   * Rewrites an image `src` before it is rendered, and drops the image when it
   * answers null.
   *
   * The file viewer uses it to turn a document's own relative image references
   * into `data:` URLs it has already read from the workspace. Nothing else
   * passes one: in a chat transcript an image reference is remote, and the
   * page's `img-src` is what decides whether it loads.
   */
  resolveImageSrc?: (src: string) => string | null;
  /**
   * Render the HTML the text carries, through GitHub's allow-list.
   *
   * Off by default for the same reason path linkification is: text a person
   * typed renders exactly as typed. Model output and repository documents opt in.
   */
  renderHtml?: boolean;
  /**
   * Leave relative links for the caller's own click handler.
   *
   * A link in a repository document is written against the document, which only
   * the file pane knows. Everywhere else a relative link is written against the
   * conversation's working directory, and opens like any other detected path.
   */
  documentLinks?: boolean;
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

function textOf(node: ElementContent | undefined): string {
  if (!node) return "";
  if (node.type === "text") return node.value;
  if (node.type !== "element") return "";
  return node.children.map(textOf).join("");
}

function classNames(node: HastElement | undefined): string[] {
  const value: unknown = node?.properties?.className;
  if (Array.isArray(value)) return value.map(String);
  return typeof value === "string" ? value.split(/\s+/) : [];
}

/**
 * A fenced block, or display math.
 *
 * Both reach here as `<pre><code class="language-…">`: remark-math writes `$$`
 * blocks that way, and a fence that says ```math means the same thing. The block
 * is drawn from the tree rather than from `children`, so the inline `code`
 * override below never sees the code inside it.
 */
function MarkdownPre({ node, children, ...props }: ComponentProps<"pre"> & ExtraProps) {
  const code = node?.children.find(
    (child): child is HastElement => child.type === "element" && child.tagName === "code"
  );
  if (!code) return <pre {...props}>{children}</pre>;
  const language = classNames(code).find((name) => name.startsWith("language-"))?.slice("language-".length) ?? null;
  const source = textOf(code).replace(/\n$/, "");
  if (language === "math") return <MathFormula source={source.trim()} display />;
  return <MarkdownCodeBlock code={source} language={fenceLanguage(language)} label={language} />;
}

function MarkdownCode({ node, children, className, ...props }: ComponentProps<"code"> & ExtraProps) {
  if (classNames(node).includes("language-math")) {
    return <MathFormula source={textOf(node?.children[0]).trim()} display={false} />;
  }
  return <code className={className} {...props}>{children}</code>;
}

function MarkdownTable({ node: _node, ...props }: ComponentProps<"table"> & ExtraProps) {
  return (
    <div className="markdown-content__table-scroll">
      <table {...props} />
    </div>
  );
}

/**
 * An image that says what it was when it cannot be drawn.
 *
 * The page's `img-src` refuses remote addresses, so a reply that links a picture
 * on the web would otherwise leave a broken-image glyph where its description
 * belongs.
 */
function MarkdownImage({ src, alt, ...props }: ImgHTMLAttributes<HTMLImageElement>) {
  const [failed, setFailed] = useState(false);
  useEffect(() => setFailed(false), [src]);
  if (failed || !src) return <span className="markdown-content__image-missing">{alt}</span>;
  return <img {...props} src={src} alt={alt} loading="lazy" referrerPolicy="no-referrer" onError={() => setFailed(true)} />;
}

/**
 * The URL filter every link and image passes through.
 *
 * react-markdown's default keeps the web and drops the rest. A few more schemes
 * mean something here: `file:` names a file this app can open, a `data:` image
 * is already a picture the page may draw, and `attachment:` is a picture stored
 * in a notebook cell.
 */
function markdownUrlTransform(url: string, key: string): string {
  if (key === "src" && /^data:image\//i.test(url.trim())) return url;
  // A notebook cell's pasted picture, which the notebook viewer resolves itself.
  if (key === "src" && /^attachment:/i.test(url.trim())) return url;
  if (key === "href" && /^file:/i.test(url.trim())) return url;
  return defaultUrlTransform(url);
}

const MATH_AND_CODE_COMPONENTS = {
  pre: MarkdownPre,
  code: MarkdownCode,
  table: MarkdownTable
} satisfies Components;

/** Safe shared renderer for assistant replies and visible reasoning. */
export const MarkdownContent = memo(function MarkdownContent({
  content,
  className,
  deferOffscreen = false,
  streaming = false,
  linkifyPaths = false,
  pathBaseDir = null,
  resolveImageSrc,
  renderHtml = false,
  documentLinks = false
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
    () => {
      const plugins: NonNullable<ComponentProps<typeof ReactMarkdown>["remarkPlugins"]> = [
        remarkGfm,
        remarkBreaks,
        [remarkMath, { singleDollarTextMath: singleDollarMath }]
      ];
      // Last, so autolinked URLs and math are already their own nodes and the
      // path transform can skip them.
      if (linkifyPaths) plugins.push(remarkPathLinks);
      return plugins;
    },
    [singleDollarMath, linkifyPaths]
  );
  const normalizedContent = useMemo(
    () => shouldRender ? normalizeMathDelimiters(content) : content,
    [content, shouldRender]
  );
  // The raw-HTML pass costs a second parse of the whole tree, so it only joins
  // the pipeline once the text has something that could be a tag.
  const withHtml = renderHtml && shouldRender && mayContainHtml(content);
  // The streaming reveal, last so it sees the tree the page will: the split it
  // took is kept for the commit that follows, and only a commit moves it on —
  // a render React throws away, or StrictMode's second one, has to take the
  // same split as the first.
  const shownSplit = useRef<StreamRevealSplit | null>(null);
  const renderedSplit = useRef<StreamRevealSplit | null>(null);
  const previousSplit = streaming ? shownSplit.current : null;
  const rehypePlugins = useMemo<ComponentProps<typeof ReactMarkdown>["rehypePlugins"]>(
    () => {
      const plugins: NonNullable<ComponentProps<typeof ReactMarkdown>["rehypePlugins"]> = withHtml
        ? [rehypeRaw, [rehypeSanitize, MARKDOWN_HTML_SCHEMA]]
        : [];
      if (streaming) {
        plugins.push([rehypeStreamReveal, {
          previous: previousSplit,
          onSplit: (split: StreamRevealSplit) => { renderedSplit.current = split; }
        }]);
      }
      return plugins;
    },
    [withHtml, streaming, previousSplit]
  );

  // Links and images depend on how this surface resolves them; the rest of the
  // overrides are fixed. Kept stable between renders, because a new component
  // identity makes React remount every link, block and formula in the reply on
  // each streamed token.
  const components = useMemo<Components>(() => ({
    ...MATH_AND_CODE_COMPONENTS,
    a: ({ node: _node, href, children, ...props }) => {
      const onFragmentClick = (event: MouseEvent<HTMLAnchorElement>, fragment: string) => {
        event.preventDefault();
        scrollToFragment(hostRef.current, fragment);
      };
      if (!href) return <a {...props}>{children}</a>;
      if (href.startsWith("#")) {
        return <a {...props} href={href} onClick={(event) => onFragmentClick(event, href.slice(1))}>{children}</a>;
      }
      if (externalHttpUrl(href) !== null || /^(?:mailto|xmpp|ircs?):/i.test(href)) {
        return <a {...props} href={href} target="_blank" rel="noreferrer noopener">{children}</a>;
      }
      const local = localLinkTarget(href);
      if (documentLinks || !local?.path) {
        // The file pane resolves document links itself; anything else that names
        // no file has nowhere to go, and must not navigate the app away.
        return (
          <a {...props} href={href} onClick={documentLinks ? undefined : (event) => event.preventDefault()}>
            {children}
          </a>
        );
      }
      if (!linkifyPaths) return <a {...props} href={href} onClick={(event) => event.preventDefault()}>{children}</a>;
      // A link to a file is a detected path with a label of its own: the
      // document-level path interceptor opens it, and the default is suppressed
      // for the click it declines.
      return (
        <a
          {...props}
          href={href}
          data-mework-path={local.path}
          data-mework-path-line={local.line ?? undefined}
          title={local.line === null ? local.path : `${local.path}:${local.line}`}
          onClick={(event) => event.preventDefault()}
        >
          {children}
        </a>
      );
    },
    img: ({ node: _node, ...props }) => {
      if (!resolveImageSrc) return <MarkdownImage {...props} />;
      const source = resolveImageSrc(props.src ?? "");
      // A resolver that has nothing for this reference yet — or will never
      // have anything — leaves the alternative text standing rather than a
      // broken-image glyph.
      if (source === null) return <span className="markdown-content__image-missing">{props.alt}</span>;
      return <MarkdownImage {...props} src={source} />;
    }
  }), [documentLinks, linkifyPaths, resolveImageSrc]);

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

  useLayoutEffect(() => {
    shownSplit.current = streaming ? renderedSplit.current : null;
  });

  return (
    <div
      ref={hostRef}
      className={`markdown-content${className ? ` ${className}` : ""}`}
      data-markdown-deferred={!shouldRender || undefined}
      data-mework-path-base={(linkifyPaths && pathBaseDir) || undefined}
      style={!shouldRender ? { minHeight: `${measuredHeight || estimatedHeight}px` } : undefined}
    >
      {shouldRender && (
        <ReactMarkdown
          remarkPlugins={remarkPlugins}
          rehypePlugins={rehypePlugins}
          urlTransform={markdownUrlTransform}
          components={components}
        >
          {normalizedContent}
        </ReactMarkdown>
      )}
    </div>
  );
});
