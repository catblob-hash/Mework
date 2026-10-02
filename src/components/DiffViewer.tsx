import {
  ChevronDown,
  Database,
  File,
  FileArchive,
  FileCode2,
  FileImage,
  MoreVertical,
  Presentation,
  Scroll,
  Sheet,
  UnfoldVertical
} from "lucide-react";
import { Fragment, useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { ReactNode } from "react";
import { useI18n } from "../i18n";
import { fileIconKind } from "../lib/fileIcons";
import type { DiffFile, DiffHunk, DiffLine } from "../lib/unifiedDiff";
import { wordDiff } from "../lib/wordDiff";
import type { WordDiffSegment } from "../lib/wordDiff";
import { PopoverMenu } from "./PopoverMenu";
import type { PopoverMenuSection } from "./PopoverMenu";
import "./DiffViewer.css";

/** Fixed width of the file column while it sits beside the diff. */
const DIFF_TREE_WIDTH = 240;
/** Below this body width the file column and the diff cannot share the pane. */
const DIFF_TREE_MIN_WIDTH = 400;
/** Below this body width side-by-side is unreadable and the viewer falls back to unified. */
const DIFF_SPLIT_MIN_WIDTH = 560;
/** Changed lines past which every file starts collapsed. */
export const DIFF_LARGE_LINE_COUNT = 5000;

export type DiffEntryStatus =
  | "added"
  | "deleted"
  | "modified"
  | "renamed"
  | "copied"
  | "typeChanged"
  | "untracked"
  | "conflicted";

/**
 * One row of the file column.
 *
 * The column lists what the scope *contains*, which is not the same as what the
 * patch *carries*: an untracked or binary file is a row with no hunks behind it,
 * and a file whose patch has not been fetched yet is a row that is still waiting.
 */
export interface DiffEntry {
  path: string;
  oldPath?: string | null;
  status: DiffEntryStatus;
  additions: number;
  deletions: number;
  binary?: boolean;
  /** A note shown after the path in the file header, such as a staging state. */
  note?: string;
  loading?: boolean;
  /** Why this file has no patch, when that is expected rather than pending. */
  unavailable?: string;
}

export interface DiffViewerProps {
  entries: readonly DiffEntry[];
  /** Parsed patches, keyed by the same paths the entries use. */
  patches: ReadonlyMap<string, DiffFile>;
  showTree: boolean;
  onCanFitTreeChange?: (canFit: boolean) => void;
  diffStyle: "unified" | "split";
  onCanFitSplitChange?: (canFit: boolean) => void;
  wordWrap: boolean;
  wordDiff: boolean;
  hideWhitespace: boolean;
  onIsLargeChange?: (large: boolean) => void;
  /** Bumped to fold or unfold every file at once. */
  foldAllRequest?: { collapsed: boolean; seq: number };
  activePath: string | null;
  onSelectFile: (path: string) => void;
  /**
   * A file asked for from outside the viewer, opened and scrolled to the way a
   * click on its row does. Bumping `seq` asks again; a path not among the entries
   * yet waits for them.
   */
  revealRequest?: { path: string; seq: number };
  /** Rows for the file column's and the file header's `⋮`. */
  renderFileMenu?: (path: string) => PopoverMenuSection[];
  /** Chrome the caller owns at the top and bottom of the file column — a filter, a load-more. */
  listHeader?: ReactNode;
  listFooter?: ReactNode;
  /**
   * Called with a path whose card is open but whose patch is missing.
   *
   * A scope-wide patch cannot carry every file — an untracked file has to be
   * diffed against nothing, one file at a time — so the viewer asks for what it
   * is about to draw rather than the caller fetching everything up front.
   */
  onNeedPatch?: (path: string) => void;
  /** Given, each hunk boundary grows buttons that ask for more context. */
  onExpandContext?: (path: string) => void;
  expandingPaths?: ReadonlySet<string>;
  /** Paths whose patch already carries the whole file, so expanding is done. */
  fullyExpandedPaths?: ReadonlySet<string>;
  emptyLabel: string;
}

type TreeNode =
  | { kind: "directory"; name: string; path: string; children: TreeNode[] }
  | { kind: "file"; name: string; path: string; entry: DiffEntry };

/**
 * Groups the flat path list into a directory tree, then folds every chain of
 * single-child directories into one row.
 *
 * A tree of one file per directory is a staircase that says nothing; `src/lib/git`
 * on one row says the same thing in a sixth of the height.
 */
function buildTree(entries: readonly DiffEntry[]): TreeNode[] {
  const roots: TreeNode[] = [];
  const directories = new Map<string, TreeNode[]>([["", roots]]);
  for (const entry of entries) {
    const segments = entry.path.split("/");
    const name = segments.pop() ?? entry.path;
    let prefix = "";
    let children = roots;
    for (const segment of segments) {
      const path = prefix ? `${prefix}/${segment}` : segment;
      let next = directories.get(path);
      if (!next) {
        next = [];
        directories.set(path, next);
        children.push({ kind: "directory", name: segment, path, children: next });
      }
      prefix = path;
      children = next;
    }
    children.push({ kind: "file", name, path: entry.path, entry });
  }
  return collapseChains(roots);
}

function collapseChains(nodes: TreeNode[]): TreeNode[] {
  return nodes.map((node) => {
    if (node.kind !== "directory") return node;
    const children = collapseChains(node.children);
    if (children.length === 1 && children[0].kind === "directory") {
      const only = children[0];
      return { kind: "directory", name: `${node.name}/${only.name}`, path: only.path, children: only.children };
    }
    return { ...node, children };
  });
}

export function EntryIcon({ path, className }: { path: string; className?: string }) {
  const props = { size: 13, "aria-hidden": true as const, className };
  switch (fileIconKind(path)) {
    case "code": return <FileCode2 {...props} />;
    case "data": return <Database {...props} />;
    case "sheet": return <Sheet {...props} />;
    case "preso": return <Presentation {...props} />;
    case "image": return <FileImage {...props} />;
    case "archive": return <FileArchive {...props} />;
    case "skill": return <Scroll {...props} />;
    default: return <File {...props} />;
  }
}

/** `−` is the typographic minus the reference shell uses; a hyphen reads as a dash here. */
function StatCounts({ additions, deletions }: { additions: number; deletions: number }) {
  return (
    <span className="diff-viewer__stats">
      <b>{`+${additions}`}</b>
      <em>{`−${deletions}`}</em>
    </span>
  );
}

/** Whitespace-insensitive comparison, for deciding a change is only re-indentation. */
function withoutWhitespace(text: string): string {
  return text.replace(/\s+/g, "");
}

interface LinePair {
  deletion: DiffLine | null;
  addition: DiffLine | null;
}

/**
 * Pairs a hunk's changed lines so the two sides can sit opposite each other.
 *
 * A run of removals followed by a run of additions is one replacement: the nth
 * removal answers the nth addition, and whichever run is longer spills into rows
 * with an empty other side. Context lines pair with themselves.
 */
function pairHunkLines(lines: readonly DiffLine[]): LinePair[] {
  const pairs: LinePair[] = [];
  let index = 0;
  while (index < lines.length) {
    const line = lines[index];
    if (line.kind === "context") {
      pairs.push({ deletion: line, addition: line });
      index += 1;
      continue;
    }
    const deletions: DiffLine[] = [];
    const additions: DiffLine[] = [];
    while (index < lines.length && lines[index].kind === "deletion") deletions.push(lines[index++]);
    while (index < lines.length && lines[index].kind === "addition") additions.push(lines[index++]);
    const rows = Math.max(deletions.length, additions.length);
    for (let row = 0; row < rows; row += 1) {
      pairs.push({ deletion: deletions[row] ?? null, addition: additions[row] ?? null });
    }
  }
  return pairs;
}

/**
 * Drops the changes that are only whitespace.
 *
 * A pair whose two sides are equal once whitespace is removed becomes context —
 * it is still the file, it is just not the change being read. A hunk with nothing
 * left after that is dropped entirely, the way `git diff -w` drops it.
 */
function suppressWhitespaceOnly(hunk: DiffHunk): DiffHunk | null {
  const pairs = pairHunkLines(hunk.lines);
  const kept: DiffLine[] = [];
  let changed = false;
  for (const pair of pairs) {
    if (pair.deletion && pair.addition && pair.deletion !== pair.addition) {
      if (withoutWhitespace(pair.deletion.text) === withoutWhitespace(pair.addition.text)) {
        kept.push({ ...pair.addition, kind: "context", oldLine: pair.deletion.oldLine });
        continue;
      }
      changed = true;
      kept.push(pair.deletion, pair.addition);
      continue;
    }
    if (pair.deletion && pair.addition) {
      kept.push(pair.deletion);
      continue;
    }
    const only = pair.deletion ?? pair.addition;
    if (!only) continue;
    // A lone added or removed line is a real change unless it is blank on its own.
    if (withoutWhitespace(only.text) === "") {
      kept.push({ ...only, kind: "context" });
      continue;
    }
    changed = true;
    kept.push(only);
  }
  return changed ? { ...hunk, lines: kept } : null;
}

function renderSegments(segments: readonly WordDiffSegment[], tone: "addition" | "deletion"): ReactNode {
  return segments.map((segment, index) => (
    segment.changed
      ? <span className={`diff-viewer__word diff-viewer__word--${tone}`} key={index}>{segment.text}</span>
      : <Fragment key={index}>{segment.text}</Fragment>
  ));
}

/**
 * The reference shell's continuous diff: one scroller holding every file, each
 * behind a sticky 32px header, with the file column beside it.
 */
export function DiffViewer({
  entries,
  patches,
  showTree,
  onCanFitTreeChange,
  diffStyle,
  onCanFitSplitChange,
  wordWrap,
  wordDiff: wordDiffEnabled,
  hideWhitespace,
  onIsLargeChange,
  foldAllRequest,
  activePath,
  onSelectFile,
  revealRequest,
  renderFileMenu,
  listHeader,
  listFooter,
  onNeedPatch,
  onExpandContext,
  expandingPaths,
  fullyExpandedPaths,
  emptyLabel
}: DiffViewerProps) {
  const { t } = useI18n();
  const bodyRef = useRef<HTMLDivElement>(null);
  const scrollRef = useRef<HTMLDivElement>(null);
  const [canFitTree, setCanFitTree] = useState(true);
  const [canFitSplit, setCanFitSplit] = useState(true);
  const [collapsedDirectories, setCollapsedDirectories] = useState<ReadonlySet<string>>(() => new Set());
  /**
   * Files the reader has flipped away from the state they would have defaulted to.
   *
   * Membership is a flip rather than a state so the default can be decided while
   * rendering instead of written into a set by an effect. That ordering is
   * load-bearing: the effect that asks for missing patches runs in the same pass,
   * and a fold set one render behind means it asks for every file in the scope,
   * including the ones it is about to hide.
   */
  const [toggledFiles, setToggledFiles] = useState<ReadonlySet<string>>(() => new Set());
  /** Set by a fold-all request; overrides the per-file default until the next one. */
  const [foldAll, setFoldAll] = useState<boolean | null>(null);
  const foldSeqRef = useRef<number | undefined>(undefined);
  /**
   * The file the column asked to jump to, held until its card can be drawn.
   *
   * Scrolling to a card that is still folded lands the reader on a header with
   * nothing under it, and the card grows under them a moment later; the jump is
   * the last step of opening a file, not the first.
   */
  const [scrollTarget, setScrollTarget] = useState<string | null>(null);

  const totals = useMemo(() => entries.reduce(
    (sum, entry) => ({
      additions: sum.additions + entry.additions,
      deletions: sum.deletions + entry.deletions
    }),
    { additions: 0, deletions: 0 }
  ), [entries]);
  const isLarge = totals.additions + totals.deletions > DIFF_LARGE_LINE_COUNT;

  useEffect(() => {
    onIsLargeChange?.(isLarge);
  }, [isLarge, onIsLargeChange]);

  if (foldAllRequest && foldSeqRef.current !== foldAllRequest.seq) {
    foldSeqRef.current = foldAllRequest.seq;
    setFoldAll(foldAllRequest.collapsed);
    setToggledFiles(new Set());
  }

  /**
   * Whether the reader has asked for a file, folded or not.
   *
   * Every file starts folded: the pane is a reading surface for one file at a time,
   * and rendering the whole scope costs more than the reader gets from it before
   * they have picked a file. A fold-all request overrides this until the next one.
   */
  const isRequested = useCallback((path: string): boolean => {
    const foldedByDefault = foldAll ?? true;
    return toggledFiles.has(path) ? foldedByDefault : !foldedByDefault;
  }, [foldAll, toggledFiles]);

  /**
   * Whether a requested file has something to draw.
   *
   * A binary file and an unavailable one have a sentence instead of hunks, which
   * is an answer and draws immediately; anything else waits for its patch. Folding
   * until then is what replaces a placeholder row: a file the reader asked for
   * opens once, already carrying its diff, rather than opening onto a spinner.
   */
  const isReady = useCallback((entry: DiffEntry): boolean => (
    Boolean(entry.binary) || Boolean(entry.unavailable) || patches.has(entry.path)
  ), [patches]);

  /** Files the reader asked for, whether or not their patch has arrived. */
  const requestedPaths = useMemo(
    () => entries.filter((entry) => isRequested(entry.path)).map((entry) => entry.path),
    [entries, isRequested]
  );
  const openPaths = useMemo(
    () => entries
      .filter((entry) => isRequested(entry.path) && isReady(entry))
      .map((entry) => entry.path),
    [entries, isReady, isRequested]
  );

  // A requested card with no patch behind it is a request, not an empty file. The
  // call is repeated whenever the inputs change, so the caller must be idempotent —
  // asking twice for one path has to be free.
  useEffect(() => {
    if (!onNeedPatch) return;
    const byPath = new Map(entries.map((entry) => [entry.path, entry]));
    for (const path of requestedPaths) {
      const entry = byPath.get(path);
      if (!entry || entry.loading || entry.binary || entry.unavailable) continue;
      if (patches.has(path)) continue;
      onNeedPatch(path);
    }
  }, [entries, onNeedPatch, patches, requestedPaths]);

  useEffect(() => {
    const body = bodyRef.current;
    if (!body || typeof ResizeObserver === "undefined") return;
    const measure = (width: number) => {
      if (width <= 0) return;
      setCanFitTree(width >= DIFF_TREE_MIN_WIDTH);
      setCanFitSplit(width >= DIFF_SPLIT_MIN_WIDTH);
    };
    measure(body.clientWidth);
    const observer = new ResizeObserver(([entry]) => measure(entry?.contentRect.width ?? 0));
    observer.observe(body);
    return () => observer.disconnect();
  }, []);

  useEffect(() => {
    onCanFitTreeChange?.(canFitTree);
  }, [canFitTree, onCanFitTreeChange]);
  useEffect(() => {
    onCanFitSplitChange?.(canFitSplit);
  }, [canFitSplit, onCanFitSplitChange]);

  // Side by side needs room for two code columns; below it the setting stays on but
  // the viewer draws the only layout that fits.
  const effectiveStyle = canFitSplit ? diffStyle : "unified";
  const treeVisible = showTree && canFitTree;

  const tree = useMemo(() => buildTree(entries), [entries]);

  const toggleDirectory = useCallback((path: string) => {
    setCollapsedDirectories((current) => {
      const next = new Set(current);
      if (next.has(path)) next.delete(path);
      else next.add(path);
      return next;
    });
  }, []);

  const toggleFile = useCallback((path: string) => {
    setToggledFiles((current) => {
      const next = new Set(current);
      if (next.has(path)) next.delete(path);
      else next.add(path);
      return next;
    });
  }, []);

  /** Asks for a file without flipping one that is already asked for. */
  const requestFile = useCallback((path: string) => {
    const foldedByDefault = foldAll ?? true;
    setToggledFiles((current) => {
      if (current.has(path) === foldedByDefault) return current;
      const next = new Set(current);
      if (foldedByDefault) next.add(path);
      else next.delete(path);
      return next;
    });
  }, [foldAll]);

  const scrollToFile = useCallback((path: string) => {
    onSelectFile(path);
    requestFile(path);
    setScrollTarget(path);
  }, [onSelectFile, requestFile]);

  const revealSeqRef = useRef<number | undefined>(undefined);
  useEffect(() => {
    if (!revealRequest || revealSeqRef.current === revealRequest.seq) return;
    if (!entries.some((entry) => entry.path === revealRequest.path)) return;
    revealSeqRef.current = revealRequest.seq;
    scrollToFile(revealRequest.path);
  }, [entries, revealRequest, scrollToFile]);

  /**
   * Jumps to the file the column picked, once its card is open.
   *
   * A file already carrying its patch is open in this same commit, so the jump is
   * immediate; one still being read jumps when its patch lands. The target is
   * dropped if the reader folds it again or it leaves the scope, so a fetch that
   * never answers cannot move the scroller later.
   */
  useEffect(() => {
    if (!scrollTarget) return;
    if (!requestedPaths.includes(scrollTarget) || !entries.some((entry) => entry.path === scrollTarget)) {
      setScrollTarget(null);
      return;
    }
    if (!openPaths.includes(scrollTarget)) return;
    const target = scrollRef.current?.querySelector(`[data-diff-file="${CSS.escape(scrollTarget)}"]`);
    target?.scrollIntoView({ block: "start", behavior: "instant" as ScrollBehavior });
    setScrollTarget(null);
  }, [entries, openPaths, requestedPaths, scrollTarget]);

  const renderTreeNode = (node: TreeNode, depth: number): ReactNode => {
    const indent = { paddingLeft: 8 + depth * 12 };
    if (node.kind === "directory") {
      const collapsed = collapsedDirectories.has(node.path);
      return (
        <Fragment key={node.path}>
          <button
            type="button"
            className="diff-viewer__tree-row diff-viewer__tree-row--directory"
            style={indent}
            aria-expanded={!collapsed}
            data-drag-exclude
            onClick={() => toggleDirectory(node.path)}
          >
            <ChevronDown
              size={13}
              aria-hidden="true"
              className={`diff-viewer__tree-chevron${collapsed ? " diff-viewer__tree-chevron--collapsed" : ""}`}
            />
            <span className="diff-viewer__tree-name">{node.name}</span>
          </button>
          {!collapsed && node.children.map((child) => renderTreeNode(child, depth + 1))}
        </Fragment>
      );
    }
    const selected = node.path === activePath;
    const sections = renderFileMenu?.(node.path);
    return (
      <div
        className={`diff-viewer__tree-file${selected ? " diff-viewer__tree-file--selected" : ""}`}
        key={node.path}
      >
        <button
          type="button"
          className="diff-viewer__tree-row"
          style={indent}
          aria-current={selected || undefined}
          title={node.path}
          data-drag-exclude
          onClick={() => scrollToFile(node.path)}
        >
          <EntryIcon path={node.path} className="diff-viewer__tree-icon" />
          <span className="diff-viewer__tree-name">{node.name}</span>
          <StatCounts additions={node.entry.additions} deletions={node.entry.deletions} />
        </button>
        {sections && sections.length > 0 && (
          <PopoverMenu
            rootClassName="diff-viewer__tree-menu"
            triggerClassName="icon-button diff-viewer__tree-menu-trigger"
            trigger={<MoreVertical size={12} aria-hidden="true" />}
            triggerLabel={t("{name} 的操作", "Actions for {name}", { name: node.name })}
            menuLabel={t("{name} 的操作", "Actions for {name}", { name: node.name })}
            sections={sections}
            align="end"
            dense
          />
        )}
      </div>
    );
  };

  /**
   * What to say about a file beyond its path and its counts.
   *
   * Added, deleted and renamed files already read as such — a rename shows both
   * names, and a deletion is all removals. Untracked and conflicted are states
   * Git has and the diff cannot show, so those are the ones that get a word.
   */
  const statusNote = (status: DiffEntryStatus): string | null => {
    switch (status) {
      case "untracked": return t("未跟踪", "Untracked");
      case "conflicted": return t("冲突", "Conflicted");
      case "typeChanged": return t("类型变更", "Type changed");
      default: return null;
    }
  };

  return (
    <div className="diff-viewer" ref={bodyRef}>
      <div
        className="diff-viewer__files"
        data-diff-tree
        hidden={!treeVisible}
        style={{ width: DIFF_TREE_WIDTH }}
      >
        {listHeader}
        <nav className="diff-viewer__file-list" aria-label={t("变更文件", "Changed files")}>
          {tree.map((node) => renderTreeNode(node, 0))}
        </nav>
        {listFooter}
      </div>

      <div className="diff-viewer__body">
        <div className="diff-viewer__scroll" ref={scrollRef}>
          {entries.length === 0 && <p className="diff-viewer__empty">{emptyLabel}</p>}
          {entries.map((entry) => {
            const patch = patches.get(entry.path);
            const collapsed = !(isRequested(entry.path) && isReady(entry));
            const selected = entry.path === activePath;
            const separator = entry.path.lastIndexOf("/");
            const directory = separator > 0 ? entry.path.slice(0, separator + 1) : "";
            const name = entry.path.slice(separator + 1);
            const sections = renderFileMenu?.(entry.path);
            const hunks = (patch?.hunks ?? []).map((hunk) => (
              hideWhitespace ? suppressWhitespaceOnly(hunk) : hunk
            ));
            const visibleHunks = hunks.filter((hunk): hunk is DiffHunk => hunk !== null);
            const whitespaceOnly = hideWhitespace
              && (patch?.hunks.length ?? 0) > 0
              && visibleHunks.length === 0;
            return (
              <section
                className={`diff-viewer__file${selected ? " diff-viewer__file--selected" : ""}`}
                data-diff-file={entry.path}
                key={entry.path}
                aria-label={t("{path} 差异", "{path} diff", { path: entry.path })}
              >
                <div className="diff-viewer__file-header">
                  <button
                    type="button"
                    className="diff-viewer__file-toggle"
                    aria-expanded={!collapsed}
                    data-drag-exclude
                    onClick={() => {
                      onSelectFile(entry.path);
                      toggleFile(entry.path);
                    }}
                  >
                    <ChevronDown
                      size={14}
                      aria-hidden="true"
                      className={`diff-viewer__file-chevron${collapsed ? " diff-viewer__file-chevron--collapsed" : ""}`}
                    />
                    <EntryIcon path={entry.path} className="diff-viewer__file-icon" />
                    <span className="diff-viewer__file-path" title={entry.path}>
                      {entry.oldPath && entry.oldPath !== entry.path && (
                        <span className="diff-viewer__file-old">{`${entry.oldPath} → `}</span>
                      )}
                      <span className="diff-viewer__file-directory">{directory}</span>
                      <span className="diff-viewer__file-name">{name}</span>
                    </span>
                    {statusNote(entry.status) && (
                      <span className={`diff-viewer__file-note diff-viewer__file-note--${entry.status}`}>
                        {statusNote(entry.status)}
                      </span>
                    )}
                    {entry.note && <span className="diff-viewer__file-note">{entry.note}</span>}
                    {whitespaceOnly && (
                      <span className="diff-viewer__file-note">{t("仅空白改动", "Whitespace only")}</span>
                    )}
                    <StatCounts additions={entry.additions} deletions={entry.deletions} />
                  </button>
                  {sections && sections.length > 0 && (
                    <PopoverMenu
                      rootClassName="diff-viewer__file-menu"
                      triggerClassName="icon-button diff-viewer__file-menu-trigger"
                      trigger={<MoreVertical size={13} aria-hidden="true" />}
                      triggerLabel={t("{name} 的操作", "Actions for {name}", { name })}
                      menuLabel={t("{name} 的操作", "Actions for {name}", { name })}
                      sections={sections}
                      align="end"
                      dense
                    />
                  )}
                </div>

                {!collapsed && (
                  <div className="diff-viewer__file-body">
                    {entry.unavailable && (
                      <p className="diff-viewer__file-note-row">{entry.unavailable}</p>
                    )}
                    {!entry.unavailable && entry.binary && (
                      <p className="diff-viewer__file-note-row">{t("二进制文件，无法显示差异", "Binary file; no diff to show")}</p>
                    )}
                    {!entry.unavailable && !entry.binary && patch?.modeChangeOnly && (
                      <p className="diff-viewer__file-note-row">
                        {t("仅文件模式变更：{from} → {to}", "File mode only: {from} → {to}", {
                          from: patch.oldMode ?? "?",
                          to: patch.newMode ?? "?"
                        })}
                      </p>
                    )}
                    {!entry.unavailable && !entry.binary && whitespaceOnly && (
                      <p className="diff-viewer__file-note-row">
                        {t("这个文件只有空白改动，已按设置隐藏。", "This file changes only whitespace, which is hidden.")}
                      </p>
                    )}
                    {!entry.unavailable && !entry.binary && visibleHunks.length > 0 && (
                      <DiffFileBody
                        path={entry.path}
                        hunks={visibleHunks}
                        style={effectiveStyle}
                        wordWrap={wordWrap}
                        wordDiff={wordDiffEnabled}
                        onExpandContext={onExpandContext}
                        expanding={expandingPaths?.has(entry.path) ?? false}
                        fullyExpanded={fullyExpandedPaths?.has(entry.path) ?? false}
                      />
                    )}
                    {patch?.incomplete && (
                      <p className="diff-viewer__file-note-row">
                        {t("差异过大，已被截断。", "The diff was truncated.")}
                      </p>
                    )}
                  </div>
                )}
              </section>
            );
          })}
        </div>
      </div>
    </div>
  );
}

interface DiffFileBodyProps {
  path: string;
  hunks: readonly DiffHunk[];
  style: "unified" | "split";
  wordWrap: boolean;
  wordDiff: boolean;
  onExpandContext?: (path: string) => void;
  expanding: boolean;
  fullyExpanded: boolean;
}

/**
 * The lines of one file.
 *
 * Between two hunks sits a separator saying how many unmodified lines were left
 * out and offering to fetch them — the reference shell's shape. The count is
 * derived from the hunk headers: the old file's line numbers are contiguous, so
 * the gap between where one hunk ends and the next begins is exactly what is
 * missing. Before the first hunk the gap is everything above it; after the last
 * one the file's length is unknown from a patch alone, so the separator says only
 * that there may be more.
 */
function DiffFileBody({
  path,
  hunks,
  style,
  wordWrap,
  wordDiff: wordDiffEnabled,
  onExpandContext,
  expanding,
  fullyExpanded
}: DiffFileBodyProps) {
  const { t } = useI18n();
  const canExpand = onExpandContext !== undefined && !fullyExpanded;
  const expand = () => onExpandContext?.(path);

  const separator = (key: string, unmodified: number | null, heading: string) => {
    const label = unmodified === null
      ? t("下面可能还有未改动的内容", "More unchanged context may be available")
      : t("{count} 行未改动", "{count} unmodified lines", { count: unmodified });
    return (
    <div className="diff-viewer__hunk-separator" key={key}>
      {canExpand && (
        <button
          type="button"
          className="diff-viewer__expand"
          aria-label={t("展开更多上下文", "Expand more context")}
          title={t("展开更多上下文", "Expand more context")}
          disabled={expanding}
          data-drag-exclude
          onClick={expand}
        >
          <UnfoldVertical size={12} aria-hidden="true" />
        </button>
      )}
      {canExpand
        ? (
          <button
            type="button"
            className="diff-viewer__hunk-label diff-viewer__hunk-label--clickable"
            disabled={expanding}
            data-drag-exclude
            onClick={expand}
          >
            {label}
          </button>
        )
        : <span className="diff-viewer__hunk-label">{label}</span>}
      {heading && <span className="diff-viewer__hunk-heading">{heading}</span>}
    </div>
    );
  };

  return (
    <div
      className={`diff-viewer__code diff-viewer__code--${style}${wordWrap ? " diff-viewer__code--wrap" : ""}`}
      data-selection-columns={style === "split" || undefined}
    >
      {hunks.map((hunk, index) => {
        const previous = index === 0 ? null : hunks[index - 1];
        const gap = previous === null
          ? hunk.oldStart - 1
          : hunk.oldStart - (previous.oldStart + previous.oldLines);
        return (
          <Fragment key={`${hunk.oldStart}:${hunk.newStart}:${index}`}>
            {gap > 0 && separator(`gap:${index}`, gap, hunk.heading)}
            {gap <= 0 && hunk.heading !== "" && separator(`head:${index}`, 0, hunk.heading)}
            {style === "split"
              ? <SplitHunk hunk={hunk} wordDiff={wordDiffEnabled} />
              : <UnifiedHunk hunk={hunk} wordDiff={wordDiffEnabled} />}
          </Fragment>
        );
      })}
      {canExpand && hunks.length > 0 && separator("tail", null, "")}
    </div>
  );
}

/** Pairs used for word-level marking; a lone add or remove has nothing to compare to. */
function wordSegmentsFor(
  pairs: readonly LinePair[],
  enabled: boolean
): Map<DiffLine, WordDiffSegment[]> {
  const segments = new Map<DiffLine, WordDiffSegment[]>();
  if (!enabled) return segments;
  for (const pair of pairs) {
    if (!pair.deletion || !pair.addition || pair.deletion === pair.addition) continue;
    const result = wordDiff(pair.deletion.text, pair.addition.text);
    if (result.bailed) continue;
    segments.set(pair.deletion, result.before);
    segments.set(pair.addition, result.after);
  }
  return segments;
}

function lineContent(
  line: DiffLine,
  segments: Map<DiffLine, WordDiffSegment[]>,
  tone: "addition" | "deletion"
): ReactNode {
  const marked = segments.get(line);
  if (!marked) return line.text;
  return renderSegments(marked, tone);
}

/**
 * One number column, not two.
 *
 * The reference shell's unified gutter shows the new file's number, falling back
 * to the old one on a deletion — the line you would go to if you opened the file.
 * The other side's number rides along as an attribute rather than a second column
 * nobody reads.
 */
function UnifiedHunk({ hunk, wordDiff: enabled }: { hunk: DiffHunk; wordDiff: boolean }) {
  const pairs = useMemo(() => pairHunkLines(hunk.lines), [hunk]);
  const segments = useMemo(() => wordSegmentsFor(pairs, enabled), [pairs, enabled]);
  return (
    <>
      {hunk.lines.map((line, index) => (
        <div className={`diff-viewer__line diff-viewer__line--${line.kind}`} key={index}>
          <span className="diff-viewer__number" aria-hidden="true">
            {line.newLine ?? line.oldLine ?? ""}
          </span>
          <span
            className="diff-viewer__text"
            data-alt-line={line.kind === "addition" ? undefined : line.oldLine ?? undefined}
          >
            {line.kind === "addition"
              ? lineContent(line, segments, "addition")
              : line.kind === "deletion"
                ? lineContent(line, segments, "deletion")
                : line.text}
            {line.noNewline && <span className="diff-viewer__no-newline">{"↵"}</span>}
          </span>
        </div>
      ))}
    </>
  );
}

/**
 * Side by side, still one number column: the gutter sits at the left edge and
 * the two sides share the rest equally. It shows what the unified gutter does —
 * the new file's number, or the old one on a row with nothing on the new side —
 * and the old side's own number rides along as an attribute.
 */
function SplitHunk({ hunk, wordDiff: enabled }: { hunk: DiffHunk; wordDiff: boolean }) {
  const pairs = useMemo(() => pairHunkLines(hunk.lines), [hunk]);
  const segments = useMemo(() => wordSegmentsFor(pairs, enabled), [pairs, enabled]);
  return (
    <>
      {pairs.map((pair, index) => {
        const left = pair.deletion;
        const right = pair.addition;
        const context = left !== null && left === right;
        return (
          <div className="diff-viewer__row" key={index}>
            <span className="diff-viewer__number" aria-hidden="true">
              {right?.newLine ?? left?.oldLine ?? ""}
            </span>
            <span
              className={`diff-viewer__text diff-viewer__text--left${left === null ? " diff-viewer__text--absent" : context ? "" : " diff-viewer__text--deletion"}`}
              data-alt-line={left?.oldLine ?? undefined}
              data-selection-column="old"
            >
              {left === null ? "" : context ? left.text : lineContent(left, segments, "deletion")}
              {left?.noNewline && <span className="diff-viewer__no-newline">{"↵"}</span>}
            </span>
            <span
              className={`diff-viewer__text diff-viewer__text--right${right === null ? " diff-viewer__text--absent" : context ? "" : " diff-viewer__text--addition"}`}
              data-selection-column="new"
            >
              {right === null ? "" : context ? right.text : lineContent(right, segments, "addition")}
              {right?.noNewline && right !== left && <span className="diff-viewer__no-newline">{"↵"}</span>}
            </span>
          </div>
        );
      })}
    </>
  );
}
