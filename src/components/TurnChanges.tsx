import { ChevronRight, FileDiff } from "lucide-react";
import { memo, useState } from "react";
import { useI18n } from "../i18n";
import type { ConversationTurn } from "../lib/conversationTurns";
import { isStructuredOutputNudge } from "../lib/orchestration";
import { openPath } from "../lib/pathLinks";
import type { ContextItem, ToolContext, ToolResult } from "../types";
import { parseUnifiedDiff } from "./DiffOutput";
import { EntryIcon } from "./DiffViewer";
import "./TurnChanges.css";

/** Rows a long list shows before "Show N more". */
const COLLAPSED_ROW_COUNT = 3;

/**
 * A stretch of finished work whose file changes are summed at its end: a
 * conversation turn, or one run of a subagent. `contextIds` are the contexts it
 * owns, in any order.
 */
export interface ChangeSpan {
  key: string;
  contextIds: readonly string[];
}

export interface TurnFileChange {
  key: string;
  /** As the first call that changed it wrote it: workspace-relative or absolute. */
  path: string;
  /** The workspace number the calls named, or null for the surface's own. */
  workspace: number | null;
  additions: number;
  deletions: number;
}

export interface TurnChangeSummary {
  key: string;
  /** In the order the span first touched them. */
  files: TurnFileChange[];
  additions: number;
  deletions: number;
  /** Changes exactly when something the card shows does. */
  signature: string;
}

/**
 * Line counts of a result's diff. Keyed on the result, which keeps its identity
 * while a stream rebuilds the contexts around it, so a turn is not re-parsed on
 * every flush of the next one.
 */
const diffCounts = new WeakMap<ToolResult, { additions: number; deletions: number }>();

function countsOf(result: ToolResult & { diff: string }) {
  let counts = diffCounts.get(result);
  if (!counts) {
    const parsed = parseUnifiedDiff(result.diff);
    counts = { additions: parsed.additions, deletions: parsed.deletions };
    diffCounts.set(result, counts);
  }
  return counts;
}

/** A call that changed a file: it succeeded and carries the diff it made. */
function fileChangeOf(item: ContextItem): (ToolContext & { result: ToolResult & { diff: string } }) | null {
  if (item.kind !== "tool" || !item.result.success || !item.result.diff) return null;
  return typeof item.input.path === "string" && item.input.path.trim()
    ? item as ToolContext & { result: ToolResult & { diff: string } }
    : null;
}

function workspaceOf(item: ToolContext): number | null {
  const value = item.input.workspace;
  return typeof value === "number" && Number.isInteger(value) && value > 0 ? value : null;
}

/** Folds the spellings of one path a model is likely to mix within a turn. */
function pathKey(path: string): string {
  let key = path.trim().replace(/\\/g, "/");
  while (key.startsWith("./")) key = key.slice(2);
  return key;
}

/**
 * Every file the span's calls changed, with their line counts summed.
 *
 * Summing is exact for separate hunks and an upper bound where one edit rewrites
 * another's lines; the calls carry no whole-file snapshots to diff end to end.
 */
export function summarizeSpanChanges(
  span: ChangeSpan,
  contextFor: (id: string) => ContextItem | undefined
): TurnChangeSummary | null {
  const files = new Map<string, TurnFileChange>();
  for (const id of span.contextIds) {
    const context = contextFor(id);
    const item = context ? fileChangeOf(context) : null;
    if (!item) continue;
    const path = item.input.path as string;
    const workspace = workspaceOf(item);
    const key = `${workspace ?? 1}:${pathKey(path)}`;
    const { additions, deletions } = countsOf(item.result);
    const current = files.get(key);
    files.set(key, current
      ? { ...current, additions: current.additions + additions, deletions: current.deletions + deletions }
      : { key, path, workspace, additions, deletions });
  }
  if (!files.size) return null;
  const list = [...files.values()];
  return {
    key: span.key,
    files: list,
    additions: list.reduce((sum, file) => sum + file.additions, 0),
    deletions: list.reduce((sum, file) => sum + file.deletions, 0),
    signature: JSON.stringify(list.map((file) => [file.key, file.path, file.additions, file.deletions]))
  };
}

/**
 * The turns that have ended. A running turn is still writing its list, and one
 * paused on `ask_user` resumes as the same turn once answered.
 */
export function turnChangeSpans(turns: readonly ConversationTurn[]): ChangeSpan[] {
  return turns
    .filter((turn) => turn.status === "completed" || turn.status === "interrupted")
    .map((turn) => ({ key: `turn:${turn.id}`, contextIds: turn.contextIds }));
}

/** Whether `item` records the schema-bound result that ends a child's run on the spot. */
function isStructuredResult(item: ContextItem): boolean {
  return item.kind === "tool" && item.toolName === "structured_output" && item.result.success;
}

/**
 * A subagent transcript's runs, which it has no turn records for.
 *
 * A message opens a run when the work before it had stopped: a mailbox message
 * that lands mid-run arrives between tool batches, so it follows a call, while
 * one that wakes an idle child follows the reply its last run ended on. The
 * reminder a schema-bound child gets is part of the run it interrupts.
 *
 * `live` withholds the last run, which is still writing its list — unless it
 * already returned its structured result: the host ends such a run the moment
 * the result validates, whatever the child's status still says.
 */
export function subagentChangeSpans(contexts: readonly ContextItem[], live: boolean): ChangeSpan[] {
  const spans: { key: string; contextIds: string[]; settled: boolean }[] = [];
  let current: string[] = [];
  let settled = false;
  let previous: ContextItem | null = null;
  const close = () => {
    if (current.length) spans.push({ key: `run:${current[0]}`, contextIds: current, settled });
    current = [];
    settled = false;
  };
  for (const item of contexts) {
    if (
      item.kind === "user"
      && !isStructuredOutputNudge(item)
      && previous?.kind !== "tool"
    ) close();
    current.push(item.id);
    if (isStructuredResult(item)) settled = true;
    // Reasoning and empty protocol replies sit between a call and the message
    // after it without ending anything.
    if (item.kind !== "reasoning" && !(item.kind === "assistant" && !item.content)) previous = item;
  }
  close();
  const last = spans.at(-1);
  if (live && last && !last.settled) spans.pop();
  return spans.map(({ key, contextIds }) => ({ key, contextIds }));
}

/** A path's last segment, and the directory before it. */
function splitPath(path: string): { name: string; directory: string } {
  const trimmed = path.replace(/[\\/]+$/, "");
  const cut = Math.max(trimmed.lastIndexOf("/"), trimmed.lastIndexOf("\\"));
  return cut < 0
    ? { name: trimmed, directory: "" }
    : { name: trimmed.slice(cut + 1) || trimmed, directory: trimmed.slice(0, cut) };
}

/** `−` is the typographic minus the review pane uses; a hyphen reads as a dash. */
function Counts({ additions, deletions }: { additions: number; deletions: number }) {
  return (
    <span className="turn-changes__counts">
      <b>{`+${additions}`}</b>
      <em>{`−${deletions}`}</em>
    </span>
  );
}

export interface TurnChangesProps {
  summary: TurnChangeSummary;
  /** Directory the surface's relative paths resolve against. */
  pathBaseDir: string | null;
}

/**
 * The files one span of work changed, at its end.
 *
 * The heading folds the whole list; "Show N more" is its own choice and outlives
 * the fold, so a list opened in full comes back in full. A row opens its file
 * where it can be read: the review pane when Git tracks the change, otherwise
 * the file pane.
 */
export const TurnChanges = memo(function TurnChanges({ summary, pathBaseDir }: TurnChangesProps) {
  const { t } = useI18n();
  const [open, setOpen] = useState(true);
  const [showAll, setShowAll] = useState(false);
  const count = summary.files.length;
  // One more row costs the same space as the row that would offer it.
  const foldable = count > COLLAPSED_ROW_COUNT + 1;
  const shown = foldable && !showAll ? summary.files.slice(0, COLLAPSED_ROW_COUNT) : summary.files;
  // A row reads as its file name; only names the list holds twice say where they are.
  const names = new Map<string, number>();
  for (const file of summary.files) {
    const { name } = splitPath(file.path);
    names.set(name, (names.get(name) ?? 0) + 1);
  }
  const title = count === 1
    ? t("编辑了 1 个文件", "Edited 1 file")
    : t("编辑了 {count} 个文件", "Edited {count} files", { count });

  return (
    <section className="turn-changes" aria-label={title}>
      <button
        type="button"
        className="turn-changes__row turn-changes__heading"
        aria-expanded={open}
        onClick={() => setOpen((current) => !current)}
      >
        <FileDiff className="turn-changes__icon" size={14} aria-hidden="true" />
        <span className="turn-changes__name">{title}</span>
        <Counts additions={summary.additions} deletions={summary.deletions} />
        <ChevronRight className="turn-changes__chevron" size={14} aria-hidden="true" />
      </button>
      {open && (
        <ul className="turn-changes__list">
          {shown.map((file) => {
            const { name, directory } = splitPath(file.path);
            return (
              <li key={file.key}>
                <button
                  type="button"
                  className="turn-changes__row"
                  title={file.path}
                  onClick={() => {
                    // A path written against another workspace does not resolve
                    // against this surface's directory; the app knows where it is.
                    const own = file.workspace === null || file.workspace === 1;
                    openPath({
                      path: file.path,
                      baseDir: own ? pathBaseDir : null,
                      line: null,
                      workspace: file.workspace,
                      review: true
                    });
                  }}
                >
                  <EntryIcon path={file.path} className="turn-changes__icon" />
                  <span className="turn-changes__name">{name}</span>
                  {directory && (names.get(name) ?? 0) > 1 && (
                    <span className="turn-changes__directory">{directory}</span>
                  )}
                  <Counts additions={file.additions} deletions={file.deletions} />
                  <ChevronRight className="turn-changes__chevron" size={14} aria-hidden="true" />
                </button>
              </li>
            );
          })}
          {foldable && (
            <li>
              <button
                type="button"
                className="turn-changes__row turn-changes__more"
                aria-expanded={showAll}
                onClick={() => setShowAll((current) => !current)}
              >
                <span className="turn-changes__name">
                  {showAll
                    ? t("收起", "Show less")
                    : t("再显示 {count} 个", "Show {count} more", { count: count - COLLAPSED_ROW_COUNT })}
                </span>
                <ChevronRight className="turn-changes__chevron" size={14} aria-hidden="true" />
              </button>
            </li>
          )}
        </ul>
      )}
    </section>
  );
}, (previous, next) => (
  previous.summary.signature === next.summary.signature
  && previous.pathBaseDir === next.pathBaseDir
));
