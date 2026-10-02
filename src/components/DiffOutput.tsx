import { type CSSProperties, useMemo } from "react";
import { useI18n } from "../i18n";
import { PathText } from "./PathText";

export type DiffLineKind = "file" | "hunk" | "context" | "addition" | "deletion" | "meta";

export interface DiffOutputProps {
  value: string;
  path?: string;
}

export interface ParsedDiffLine {
  kind: DiffLineKind;
  text: string;
  oldLineNumber: number | null;
  newLineNumber: number | null;
}

export interface ParsedUnifiedDiff {
  lines: ParsedDiffLine[];
  additions: number;
  deletions: number;
  path: string | null;
}

const MAX_RENDERED_LINES = 800;
const HEAD_LINES = 500;
const TAIL_LINES = 250;

function diffPath(header: string): string | null {
  const raw = header.slice(4).split("\t", 1)[0]?.trim();
  if (!raw || raw === "/dev/null") return null;
  return raw.startsWith("a/") || raw.startsWith("b/") ? raw.slice(2) : raw;
}

/** Parses the subset of unified diff syntax needed for a read-only file result view. */
export function parseUnifiedDiff(value: string): ParsedUnifiedDiff {
  const rawLines = value.replace(/\r\n/g, "\n").replace(/\r/g, "\n").split("\n");
  if (rawLines.at(-1) === "") rawLines.pop();

  const lines: ParsedDiffLine[] = [];
  let oldLine = 0;
  let newLine = 0;
  let inHunk = false;
  let additions = 0;
  let deletions = 0;
  let path: string | null = null;

  for (const rawLine of rawLines) {
    const hunk = /^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@(.*)$/.exec(rawLine);
    if (hunk) {
      oldLine = Number(hunk[1]);
      newLine = Number(hunk[2]);
      inHunk = true;
      lines.push({ kind: "hunk", text: rawLine, oldLineNumber: null, newLineNumber: null });
      continue;
    }

    if (!inHunk && (rawLine.startsWith("--- ") || rawLine.startsWith("+++ "))) {
      path = diffPath(rawLine) ?? path;
      lines.push({ kind: "file", text: rawLine, oldLineNumber: null, newLineNumber: null });
      continue;
    }

    if (inHunk && rawLine.startsWith("+")) {
      additions += 1;
      lines.push({ kind: "addition", text: rawLine.slice(1), oldLineNumber: null, newLineNumber: newLine });
      newLine += 1;
      continue;
    }

    if (inHunk && rawLine.startsWith("-")) {
      deletions += 1;
      lines.push({ kind: "deletion", text: rawLine.slice(1), oldLineNumber: oldLine, newLineNumber: null });
      oldLine += 1;
      continue;
    }

    if (inHunk && rawLine.startsWith(" ")) {
      lines.push({ kind: "context", text: rawLine.slice(1), oldLineNumber: oldLine, newLineNumber: newLine });
      oldLine += 1;
      newLine += 1;
      continue;
    }

    lines.push({ kind: "meta", text: rawLine, oldLineNumber: null, newLineNumber: null });
  }

  return { lines, additions, deletions, path };
}

function limitLines(lines: ParsedDiffLine[], omittedLabel: (count: number) => string): ParsedDiffLine[] {
  if (lines.length <= MAX_RENDERED_LINES) return lines;
  const omitted = lines.length - HEAD_LINES - TAIL_LINES;
  return [
    ...lines.slice(0, HEAD_LINES),
    {
      kind: "meta",
      text: omittedLabel(omitted),
      oldLineNumber: null,
      newLineNumber: null
    },
    ...lines.slice(-TAIL_LINES)
  ];
}

/** File and hunk headers are diff transport, not content, so the card drops them. */
function isRenderedKind(kind: DiffLineKind): boolean {
  return kind !== "file" && kind !== "hunk";
}

/**
 * The one number a row carries now that both sides share a single column: a
 * deletion exists only in the old file, everything else is addressed in the new
 * one.
 */
function displayedLineNumber(line: ParsedDiffLine): number | null {
  return line.kind === "deletion" ? line.oldLineNumber : line.newLineNumber;
}

/**
 * Reads a replacement as "what was there, then what replaced it": each run of
 * changed lines is regrouped with its deletions first, whatever order the
 * producer emitted them in. Context, hunk, and file rows end a run, so nothing
 * moves across a boundary.
 */
function deletionsBeforeAdditions(lines: ParsedDiffLine[]): ParsedDiffLine[] {
  const ordered: ParsedDiffLine[] = [];
  let run: ParsedDiffLine[] = [];
  const flushRun = () => {
    if (!run.length) return;
    for (const line of run) if (line.kind === "deletion") ordered.push(line);
    for (const line of run) if (line.kind === "addition") ordered.push(line);
    run = [];
  };
  for (const line of lines) {
    if (line.kind === "addition" || line.kind === "deletion") {
      run.push(line);
      continue;
    }
    flushRun();
    ordered.push(line);
  }
  flushRun();
  return ordered;
}

/**
 * Width of the number gutter, in characters of the widest number it has to
 * hold and no more, so the numbers sit against the left edge instead of inside
 * a fixed column sized for a file nobody opened.
 */
function gutterCharacters(lines: ParsedDiffLine[]): number {
  let widest = 0;
  for (const line of lines) {
    const number = displayedLineNumber(line);
    if (number !== null && number > widest) widest = number;
  }
  return String(widest).length;
}

export function DiffOutput({
  value,
  path
}: DiffOutputProps) {
  const { t } = useI18n();
  const parsed = useMemo(() => parseUnifiedDiff(value), [value]);
  const renderedLines = useMemo(
    () => limitLines(
      deletionsBeforeAdditions(parsed.lines).filter((line) => isRenderedKind(line.kind)),
      (count) => t("… {count} 行差异未显示 …", "… {count} diff lines omitted …", { count })
    ),
    [parsed.lines, t]
  );
  const gutter = useMemo(() => gutterCharacters(renderedLines), [renderedLines]);
  const displayPath = path || parsed.path || t("文件", "File");
  const renderLineNumber = (line: ParsedDiffLine) => (
    <span className="diff-output__line-number" aria-hidden="true">{displayedLineNumber(line) ?? ""}</span>
  );

  return (
    <div
      className="diff-output"
      role="region"
      aria-label={t("{path} 文件差异", "{path} file diff", { path: displayPath })}
      style={{ "--diff-gutter": `${gutter}ch` } as CSSProperties}
    >
      <div className="diff-output__header">
        <PathText className="diff-output__path" path={displayPath} />
        <span className="diff-output__stats" aria-label={t("新增 {additions} 行，删除 {deletions} 行", "{additions} lines added, {deletions} lines deleted", { additions: parsed.additions, deletions: parsed.deletions })}>
          <b className="diff-output__stat diff-output__stat--addition">+{parsed.additions}</b>
          <b className="diff-output__stat diff-output__stat--deletion">−{parsed.deletions}</b>
        </span>
      </div>
      <div className="diff-output__body">
        {renderedLines.length > 0 ? renderedLines.map((line, index) => (
          <div key={`${index}:${line.kind}`} className={`diff-output__line diff-output__line--${line.kind}`}>
            {renderLineNumber(line)}
            <span className="diff-output__content">{line.text}</span>
          </div>
        )) : (
          <div className="diff-output__empty">{t("没有行级变化", "No line-level changes")}</div>
        )}
      </div>
    </div>
  );
}
