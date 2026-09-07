/**
 * Turn file paths in model-rendered Markdown into clickable nodes.
 *
 * The transform runs on mdast so it happens once per parse rather than once per
 * React render, and so link, code-block, HTML, and math nodes can be skipped
 * wholesale. Clicks are handled by the document-level interceptor in
 * `./pathLinks`, which reads the `data-mework-path` attribute emitted here.
 *
 * Prose is deliberately restrictive: only absolute paths are recognized there,
 * because a loose rule turns `and/or` and `5.0/6.0` into links. Inline code
 * spans carry most paths models emit and are matched as a whole, which is what
 * makes relative paths safe to recognize.
 */

/** A segment of a prose run: either plain text or a recognized path. */
export type PathRun =
  | { kind: "text"; value: string }
  | { kind: "path"; display: string; target: string };

/** Longest path accepted; matches the host's own bound. */
const MAX_PATH_LENGTH = 400;

/**
 * Characters that terminate a path match in prose. Full-width punctuation is
 * included because it ends a Chinese sentence and never appears in a file name,
 * while ASCII `.` and `,` do and are trimmed afterwards instead.
 */
const PROSE_STOP = "\\s\"'`<>|*?，。、；：！？（）【】《》「」『』…“”‘’";

/**
 * Absolute paths in prose. The POSIX branch requires a complete first segment
 * so that `and/or`, `24/7`, and `5.0/6.0` cannot match; the Windows branch
 * needs no such guard because a drive letter and colon are already specific.
 */
const ABSOLUTE_IN_PROSE = new RegExp(
  `[A-Za-z]:[\\\\/][^${PROSE_STOP}]*|/(?=[\\w.@+~-]+/)[^${PROSE_STOP}]*`,
  "g"
);

/** Characters that never appear in a path we are willing to hand to the shell. */
const FORBIDDEN = /["'`<>|*?\n\r\t]/;

/** Any scheme-qualified address, which belongs to the external-link path instead. */
const URL_LIKE = /^[a-z][a-z0-9+.-]*:\/\//i;

/** A trailing `:line` or `:line:col` reference, shown but not sent to the host. */
const LINE_REFERENCE = /:\d+(?::\d+)?$/;

const CLOSERS: Record<string, string> = {
  ")": "(",
  "]": "[",
  "}": "{",
  "）": "（",
  "】": "【",
  "》": "《"
};

/** Sentence punctuation, including the full-width forms Chinese prose ends with. */
const SENTENCE_PUNCTUATION = ".,;:!?。，、；：！？";

function occurrences(value: string, character: string): number {
  let count = 0;
  for (let index = 0; index < value.length; index += 1) {
    if (value[index] === character) count += 1;
  }
  return count;
}

/**
 * Drop trailing punctuation that belongs to the sentence rather than the path.
 *
 * Brackets are removed only when unbalanced, so `C:\Program Files (x86)\node.exe`
 * survives while `(see /usr/bin/env)` does not keep its closing parenthesis.
 */
function trimSentencePunctuation(value: string): string {
  let result = value;
  for (;;) {
    const last = result.at(-1);
    if (!last) break;
    if (SENTENCE_PUNCTUATION.includes(last)) {
      result = result.slice(0, -1);
      continue;
    }
    const opener = CLOSERS[last];
    if (opener && occurrences(result, last) > occurrences(result, opener)) {
      result = result.slice(0, -1);
      continue;
    }
    break;
  }
  return result;
}

function separatorCount(value: string): number {
  let count = 0;
  for (let index = 0; index < value.length; index += 1) {
    if (value[index] === "/" || value[index] === "\\") count += 1;
  }
  return count;
}

function isWindowsAbsolute(value: string): boolean {
  return /^[A-Za-z]:[\\/]/.test(value);
}

function isPosixAbsolute(value: string): boolean {
  return value.startsWith("/") && separatorCount(value) >= 2;
}

/** Build the display and host-facing forms, or reject the candidate. */
function toPath(raw: string): { display: string; target: string } | null {
  const display = trimSentencePunctuation(raw);
  if (!display || display.length > MAX_PATH_LENGTH) return null;
  // A network or device path must not be offered at all; the host rejects it too.
  if (display.startsWith("//") || display.startsWith("\\\\")) return null;
  if (URL_LIKE.test(display)) return null;
  const target = display.replace(LINE_REFERENCE, "");
  if (!target || separatorCount(target) === 0) return null;
  return { display, target };
}

/**
 * Split a prose run into plain text and absolute-path segments.
 *
 * Exported for tests; the plugin uses it through the tree walk.
 */
export function splitPathRuns(value: string): PathRun[] {
  const runs: PathRun[] = [];
  let consumed = 0;
  ABSOLUTE_IN_PROSE.lastIndex = 0;
  for (;;) {
    const match = ABSOLUTE_IN_PROSE.exec(value);
    if (!match) break;
    const start = match.index;
    const previous = start > 0 ? value[start - 1] : "";
    // Without this guard the drive-letter branch matches `s:/` inside `https://`
    // and the POSIX branch matches the tail of any unlinked URL.
    if (previous && /[:/\\\w]/.test(previous)) {
      ABSOLUTE_IN_PROSE.lastIndex = start + 1;
      continue;
    }
    const candidate = toPath(match[0]);
    if (!candidate) {
      ABSOLUTE_IN_PROSE.lastIndex = start + 1;
      continue;
    }
    if (start > consumed) runs.push({ kind: "text", value: value.slice(consumed, start) });
    runs.push({ kind: "path", display: candidate.display, target: candidate.target });
    consumed = start + candidate.display.length;
    ABSOLUTE_IN_PROSE.lastIndex = consumed;
  }
  if (consumed < value.length) runs.push({ kind: "text", value: value.slice(consumed) });
  return runs;
}

/**
 * Match a whole inline-code span as a path.
 *
 * Splitting inside a code span would break its meaning, so the entire value
 * must qualify. Relative paths are accepted here because backticks are the
 * signal that the author meant a path rather than prose.
 */
export function inlineCodePath(value: string): { display: string; target: string } | null {
  const trimmed = value.trim();
  if (!trimmed || trimmed.length > MAX_PATH_LENGTH) return null;
  if (FORBIDDEN.test(trimmed)) return null;
  const candidate = toPath(trimmed);
  if (!candidate) return null;
  const { target } = candidate;
  const absolute = isWindowsAbsolute(target) || isPosixAbsolute(target);
  // A relative path containing a space is far more often a command line than a
  // path, so only the absolute forms may contain one.
  if (!absolute && target.includes(" ")) return null;
  if (absolute) return candidate;
  if (/^\.{1,2}[\\/]/.test(target)) return candidate;
  // The extension must start with a letter, or `5.0/6.0` reads as a path.
  if (/\.[A-Za-z][A-Za-z0-9]{0,7}$/.test(target)) return candidate;
  return separatorCount(target) >= 2 ? candidate : null;
}

interface MarkdownNode {
  type: string;
  value?: string;
  children?: MarkdownNode[];
  data?: Record<string, unknown>;
}

/** Nodes whose contents are already a link, code, math, or raw markup. */
const SKIPPED = new Set([
  "link",
  "linkReference",
  "definition",
  "image",
  "imageReference",
  "code",
  "html",
  "math",
  "inlineMath"
]);

/**
 * mdast has no node type for a link that is not a URL, and reusing `link` would
 * route the path through react-markdown's URL transform and the existing anchor
 * override. A custom node carried to hast by `hName`/`hProperties` avoids both.
 */
function pathLinkNode(display: string, target: string, code: boolean): MarkdownNode {
  return {
    type: "pathLink",
    children: [code ? { type: "inlineCode", value: display } : { type: "text", value: display }],
    data: {
      hName: "button",
      hProperties: {
        type: "button",
        className: code ? ["md-path-link", "md-path-link--code"] : ["md-path-link"],
        "data-mework-path": target,
        title: target
      }
    }
  };
}

function hasSeparator(value: string): boolean {
  return value.includes("/") || value.includes("\\");
}

function visitChildren(parent: MarkdownNode): void {
  const children = parent.children;
  if (!children?.length) return;
  const next: MarkdownNode[] = [];
  let changed = false;

  for (const child of children) {
    if (SKIPPED.has(child.type)) {
      next.push(child);
      continue;
    }
    if (child.type === "text" || child.type === "inlineCode") {
      const value = child.value ?? "";
      // Cheap rejection before any regex: this runs on every streaming parse.
      if (!hasSeparator(value)) {
        next.push(child);
        continue;
      }
      if (child.type === "inlineCode") {
        const match = inlineCodePath(value);
        if (!match) next.push(child);
        else {
          changed = true;
          next.push(pathLinkNode(match.display, match.target, true));
        }
        continue;
      }
      const runs = splitPathRuns(value);
      if (!runs.some((run) => run.kind === "path")) {
        next.push(child);
        continue;
      }
      changed = true;
      for (const run of runs) {
        next.push(
          run.kind === "text"
            ? { type: "text", value: run.value }
            : pathLinkNode(run.display, run.target, false)
        );
      }
      continue;
    }
    visitChildren(child);
    next.push(child);
  }

  if (changed) parent.children = next;
}

/**
 * The remark plugin.
 *
 * It takes no options so that its identity stays stable across renders, which
 * is what lets `MarkdownContent` keep its plugin array memoized while
 * streaming. The base directory relative paths resolve against travels as a DOM
 * attribute instead.
 */
export default function remarkPathLinks() {
  return (tree: MarkdownNode) => {
    visitChildren(tree);
  };
}
