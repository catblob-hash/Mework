import { Check, Copy } from "lucide-react";
import { memo, useEffect, useMemo, useRef, useState } from "react";
import { useI18n } from "../i18n";
import { writeClipboardText } from "../lib/clipboard";
import { highlightCodeLines } from "../lib/codeHighlight";
import type { CodeToken } from "../lib/codeHighlight";
import { IconButton } from "./Common";
import "./CodeBlock.css";

/** Past this many lines code is shown uncoloured: the spans cost more than the colour is worth. */
export const MAX_HIGHLIGHTED_LINES = 8000;

export function splitCodeLines(content: string): string[] {
  const lines = content.split("\n");
  // A file ending in a newline has no extra last line; numbering one would claim
  // a line the file does not have.
  if (lines.length > 1 && lines[lines.length - 1] === "") lines.pop();
  return lines.map((line) => (line.endsWith("\r") ? line.slice(0, -1) : line));
}

/** One line's tokens; plain runs stay text nodes so a line of prose is one node. */
export function CodeTokens({ tokens, fallback }: { tokens: readonly CodeToken[] | undefined; fallback: string }) {
  if (!tokens?.length) return <>{fallback}</>;
  return (
    <>
      {tokens.map((token, index) => (
        token.kind === "plain"
          ? token.value
          : <span className={`code-token code-token--${token.kind}`} key={index}>{token.value}</span>
      ))}
    </>
  );
}

function useHighlightedLines(language: string | null, lines: readonly string[]): CodeToken[][] | null {
  return useMemo(
    () => (language === null || lines.length > MAX_HIGHLIGHTED_LINES ? null : highlightCodeLines(language, lines)),
    [language, lines]
  );
}

/**
 * A fenced block in rendered Markdown: coloured by the language its fence
 * names, with the name and a copy button over its corner.
 *
 * The block stays a `<pre><code>` so the appearance settings that wrap or fold
 * long code — written against that shape — keep applying to it.
 */
export const MarkdownCodeBlock = memo(function MarkdownCodeBlock({
  code,
  language,
  label
}: {
  code: string;
  /** Grammar id, or null to show the code uncoloured. */
  language: string | null;
  /** What the fence called it, shown as written. */
  label: string | null;
}) {
  const { t } = useI18n();
  const lines = useMemo(() => splitCodeLines(code), [code]);
  const tokens = useHighlightedLines(language, lines);
  const [copied, setCopied] = useState(false);
  const resetTimer = useRef<number | null>(null);

  useEffect(() => () => {
    if (resetTimer.current !== null) window.clearTimeout(resetTimer.current);
  }, []);

  return (
    <div className="markdown-code">
      <pre>
        <code className={label ? `language-${label}` : undefined}>
          {lines.map((line, index) => (
            <span className="markdown-code__line" key={index}>
              <CodeTokens tokens={tokens?.[index]} fallback={line} />
              {index < lines.length - 1 ? "\n" : null}
            </span>
          ))}
        </code>
      </pre>
      <div className="markdown-code__bar">
        {label && <span className="markdown-code__language">{label}</span>}
        <IconButton
          className="markdown-code__copy"
          label={copied ? t("已复制", "Copied") : t("复制代码", "Copy code")}
          onClick={() => {
            void writeClipboardText(code).then((ok) => {
              if (!ok) return;
              setCopied(true);
              if (resetTimer.current !== null) window.clearTimeout(resetTimer.current);
              resetTimer.current = window.setTimeout(() => setCopied(false), 1400);
            });
          }}
        >
          {copied ? <Check size={12} aria-hidden="true" /> : <Copy size={12} aria-hidden="true" />}
        </IconButton>
      </div>
    </div>
  );
});

/**
 * A file as its own text, numbered and coloured.
 *
 * Wrapping is not a setting: the reference shell's viewer wraps, full stop, and
 * a pane this narrow has nowhere to put a horizontal scrollbar. The gutter shares
 * the code's own background, so a lit or hovered line reads as one band across
 * both.
 */
export function NumberedCode({
  content,
  language,
  label,
  litLine = null,
  className
}: {
  content: string;
  language: string | null;
  /** Names the region for assistive technology — the file's path. */
  label: string;
  litLine?: number | null;
  className?: string;
}) {
  const lines = useMemo(() => splitCodeLines(content), [content]);
  const tokens = useHighlightedLines(language, lines);
  const digits = String(lines.length).length;
  return (
    <pre
      className={`numbered-code${className ? ` ${className}` : ""}`}
      tabIndex={0}
      aria-label={label}
      // The gutter is sized for the widest number the file has, so it does not
      // shift when scrolling from line 99 to line 100.
      style={{ ["--numbered-code-digits" as string]: String(Math.max(2, digits)) }}
    >
      {lines.map((line, index) => (
        <span
          className={`numbered-code__line${litLine === index + 1 ? " numbered-code__line--lit" : ""}`}
          key={index}
          data-line={index + 1}
        >
          <span className="numbered-code__number" aria-hidden="true">{index + 1}</span>
          <span className="numbered-code__text">
            <CodeTokens tokens={tokens?.[index]} fallback={line} />
          </span>
        </span>
      ))}
    </pre>
  );
}
