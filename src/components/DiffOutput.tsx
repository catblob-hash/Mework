import { useMemo } from "react";
import { useI18n } from "../i18n";

export type DiffLineKind = "file" | "hunk" | "context" | "addition" | "deletion" | "meta";
export type DiffLineSide = "LEFT" | "RIGHT";
export type SelectableDiffLineKind = Extract<DiffLineKind, "context" | "addition" | "deletion">;

export interface DiffLineSelection {
  path: string;
  line: number;
  side: DiffLineSide;
  text: string;
  kind: SelectableDiffLineKind;
}

export interface DiffOutputProps {
  value: string;
  path?: string;
  summary?: string;
  /** Controlled selected line. Clicking only emits `onLineSelect`. */
  selectedLine?: DiffLineSelection | null;
  onLineSelect?: (selection: DiffLineSelection) => void;
}

export interface ParsedDiffLine {
  kind: DiffLineKind;
  text: string;
  marker: string;
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
      lines.push({ kind: "hunk", text: rawLine, marker: "", oldLineNumber: null, newLineNumber: null });
      continue;
    }

    if (!inHunk && (rawLine.startsWith("--- ") || rawLine.startsWith("+++ "))) {
      path = diffPath(rawLine) ?? path;
      lines.push({ kind: "file", text: rawLine, marker: "", oldLineNumber: null, newLineNumber: null });
      continue;
    }

    if (inHunk && rawLine.startsWith("+")) {
      additions += 1;
      lines.push({ kind: "addition", text: rawLine.slice(1), marker: "+", oldLineNumber: null, newLineNumber: newLine });
      newLine += 1;
      continue;
    }

    if (inHunk && rawLine.startsWith("-")) {
      deletions += 1;
      lines.push({ kind: "deletion", text: rawLine.slice(1), marker: "−", oldLineNumber: oldLine, newLineNumber: null });
      oldLine += 1;
      continue;
    }

    if (inHunk && rawLine.startsWith(" ")) {
      lines.push({ kind: "context", text: rawLine.slice(1), marker: "", oldLineNumber: oldLine, newLineNumber: newLine });
      oldLine += 1;
      newLine += 1;
      continue;
    }

    lines.push({ kind: "meta", text: rawLine, marker: "", oldLineNumber: null, newLineNumber: null });
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
      marker: "",
      oldLineNumber: null,
      newLineNumber: null
    },
    ...lines.slice(-TAIL_LINES)
  ];
}

function selectionForSide(
  line: ParsedDiffLine,
  side: DiffLineSide,
  path: string
): DiffLineSelection | null {
  if (
    side === "LEFT"
    && (line.kind === "deletion" || line.kind === "context")
    && line.oldLineNumber !== null
  ) {
    return {
      path,
      line: line.oldLineNumber,
      side,
      text: line.text,
      kind: line.kind
    };
  }
  if (
    side === "RIGHT"
    && (line.kind === "addition" || line.kind === "context")
    && line.newLineNumber !== null
  ) {
    return {
      path,
      line: line.newLineNumber,
      side,
      text: line.text,
      kind: line.kind
    };
  }
  return null;
}

function isSelectedLine(
  selectedLine: DiffLineSelection | null | undefined,
  selection: DiffLineSelection
): boolean {
  return Boolean(
    selectedLine
    && selectedLine.path === selection.path
    && selectedLine.line === selection.line
    && selectedLine.side === selection.side
  );
}

export function DiffOutput({
  value,
  path,
  summary,
  selectedLine,
  onLineSelect
}: DiffOutputProps) {
  const { t } = useI18n();
  const parsed = useMemo(() => parseUnifiedDiff(value), [value]);
  const renderedLines = useMemo(
    () => limitLines(parsed.lines, (count) => t("… {count} 行差异未显示 …", "… {count} diff lines omitted …", { count })),
    [parsed.lines, t]
  );
  const selectablePath = path || parsed.path;
  const displayPath = selectablePath || t("文件", "File");
  const renderLineNumber = (line: ParsedDiffLine, side: DiffLineSide) => {
    const number = side === "LEFT" ? line.oldLineNumber : line.newLineNumber;
    const selection = onLineSelect && selectablePath
      ? selectionForSide(line, side, selectablePath)
      : null;
    if (!selection) {
      return <span className="diff-output__line-number" aria-hidden="true">{number ?? ""}</span>;
    }
    const sideLabel = side === "LEFT" ? t("旧文件", "old side") : t("新文件", "new side");
    const label = t(
      "选择 {path} {side}第 {line} 行",
      "Select {path} line {line} on the {side}",
      { path: displayPath, side: sideLabel, line: selection.line }
    );
    return (
      <button
        type="button"
        className="diff-output__line-number diff-output__line-select"
        aria-label={label}
        aria-pressed={isSelectedLine(selectedLine, selection)}
        title={label}
        onClick={() => onLineSelect?.(selection)}
      >
        {selection.line}
      </button>
    );
  };

  return (
    <div className="diff-output" role="region" aria-label={t("{path} 文件差异", "{path} file diff", { path: displayPath })}>
      <div className="diff-output__header">
        <code title={displayPath}>{displayPath}</code>
        <span className="diff-output__stats" aria-label={t("新增 {additions} 行，删除 {deletions} 行", "{additions} lines added, {deletions} lines deleted", { additions: parsed.additions, deletions: parsed.deletions })}>
          <b className="diff-output__stat diff-output__stat--addition">+{parsed.additions}</b>
          <b className="diff-output__stat diff-output__stat--deletion">−{parsed.deletions}</b>
        </span>
      </div>
      <div className="diff-output__body">
        {renderedLines.length > 0 ? renderedLines.map((line, index) => (
          <div key={`${index}:${line.kind}`} className={`diff-output__line diff-output__line--${line.kind}`}>
            {renderLineNumber(line, "LEFT")}
            {renderLineNumber(line, "RIGHT")}
            <span className="diff-output__marker">{line.marker}</span>
            <span className="diff-output__content">{line.text}</span>
          </div>
        )) : (
          <div className="diff-output__empty">{t("没有行级变化", "No line-level changes")}</div>
        )}
      </div>
      {summary && <div className="diff-output__summary">{summary}</div>}
    </div>
  );
}
