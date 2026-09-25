import {
  ChevronRight,
  Code2,
  Copy,
  Database,
  Eye,
  File,
  FileArchive,
  FileCode2,
  FileImage,
  FolderOpen,
  FolderTree,
  LoaderCircle,
  MoreVertical,
  PanelLeftClose,
  PanelLeftOpen,
  Presentation,
  RotateCw,
  Scroll,
  Search,
  Sheet,
  X
} from "lucide-react";
import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState
} from "react";
import type { KeyboardEvent, MouseEvent, ReactNode } from "react";
import { useI18n } from "../i18n";
import { hasBackendRuntime } from "../lib/backend";
import { localLinkTarget, scrollIntoContainer, scrollToFragment } from "../lib/documentLinks";
import { externalHttpUrl } from "../lib/externalLinks";
import { fileIconKind } from "../lib/fileIcons";
import {
  binaryMediaType,
  codeLanguage,
  fileViewerKind,
  hasSourceForm,
  imageMediaType,
  readsBytes,
  resolveDocumentReference
} from "../lib/fileViewers";
import type { FileViewerKind } from "../lib/fileViewers";
import { splitFrontMatter } from "../lib/frontMatter";
import { gitTargetKey } from "../lib/git";
import { parseNotebook } from "../lib/notebook";
import { revealPath } from "../lib/pathLinks";
import type { GitTarget } from "../lib/git";
import { readStoredFlag, writeStoredFlag } from "../lib/paneSettings";
import { arrangeByIds } from "../lib/reorder";
import type { SidePaneId } from "../lib/sidePanes";
import {
  basename,
  joinRelativePath,
  listWorkspaceDirectory,
  parentRelativePath,
  readWorkspaceFile,
  readWorkspaceFileBytes,
  searchWorkspaceFiles,
  sortEntries
} from "../lib/workspaceFiles";
import type {
  WorkspaceEntry,
  WorkspaceEntryKind,
  WorkspaceSearchMatch
} from "../lib/workspaceFiles";
import { MarkdownCodeBlock, NumberedCode } from "./CodeBlock";
import { IconButton } from "./Common";
import { AudioPlayer } from "./FilePreview/AudioPlayer";
import { CsvTable } from "./FilePreview/CsvTable";
import { FontPreview } from "./FilePreview/FontPreview";
import { HtmlPreview } from "./FilePreview/HtmlPreview";
import type { HtmlPreviewResources } from "./FilePreview/HtmlPreview";
import { ImageViewer } from "./FilePreview/ImageViewer";
import { NotebookView } from "./FilePreview/NotebookView";
import { PdfViewer } from "./FilePreview/PdfViewer";
import { UnsupportedNotice } from "./FilePreview/UnsupportedNotice";
import { dataUrlByteLength } from "./FilePreview/format";
import { MarkdownContent } from "./MarkdownContent";
import { PageTabs } from "./PageTabs";
import { PopoverMenu } from "./PopoverMenu";
import type { PopoverMenuSection } from "./PopoverMenu";
import { SidePane } from "./SidePane";
import "./FilePreview/FilePreview.css";
import "./FilesPane.css";

/** A file the pane has been asked to show from somewhere outside it. */
export interface FilesPaneOpenRequest {
  /** Workspace-relative path, `/` separated. */
  path: string;
  /** The line the reference named, scrolled to and lit once the file is on screen. */
  line: number | null;
  /** Bumped per request, so asking twice for the same file asks twice. */
  nonce: number;
  /**
   * The pane was not open when the file was asked for: it opens on the file
   * alone, with the tree folded away. A pane that was already open keeps the tree
   * the way the reader left it.
   */
  collapseTree?: boolean;
}

export interface FilesPaneProps {
  paneId: SidePaneId;
  target: GitTarget;
  /** What the pane is called while nothing is open — the workspace's own name. */
  rootLabel: string;
  /** Absolute directory behind `target`, shown as a tooltip when known. */
  workspacePath: string | null;
  /** False while the pane is not on screen; nothing is fetched until it is. */
  active: boolean;
  /** A file the timeline asked for, or null while nothing has been clicked. */
  openRequest?: FilesPaneOpenRequest | null;
  /** Told once `openRequest` has been acted on, so a later mount does not act on it again. */
  onOpenRequestHandled?: (nonce: number) => void;
  expanded: boolean;
  onToggleExpand: () => void;
  onPaneFocus: () => void;
  onPaneClose: () => void;
}

type DirectoryState =
  | { status: "loading" }
  | { status: "ready"; entries: WorkspaceEntry[] }
  | { status: "error"; message: string };

type ViewerState =
  | { status: "loading" }
  | { status: "ready"; content: string; binary: boolean; truncated: boolean }
  | { status: "error"; message: string };

/**
 * One file read as bytes rather than as text: a picture, a PDF, a recording, a
 * font. Held as a `data:` URL, which is what the pictures need anyway and what
 * the other viewers decode from.
 */
type MediaState =
  | { status: "loading" }
  | { status: "ready"; source: string }
  | { status: "tooLarge" }
  | { status: "error"; message: string };

type SearchState =
  | { status: "idle" }
  | { status: "loading" }
  | { status: "ready"; matches: WorkspaceSearchMatch[]; truncated: boolean }
  | { status: "error"; message: string };

/**
 * One open file.
 *
 * `preview` marks the italic slot every single click reuses: opening a second
 * file from the tree replaces it rather than stacking, so browsing a directory
 * does not leave a row of tabs behind. Double-clicking, or editing in the
 * reference shell, promotes the tab and the next click opens beside it.
 */
interface FileTab {
  path: string;
  preview: boolean;
}

type TreeRow =
  | {
      type: "entry";
      path: string;
      name: string;
      kind: WorkspaceEntryKind;
      depth: number;
      expanded: boolean;
    }
  | { type: "message"; id: string; depth: number; text: string; failed: boolean };

/** Row indent, matching the reference shell's `8 + depth * 12`. */
const INDENT_PER_LEVEL = 12;
const ROOT_INDENT = 8;
/** Fixed width of the tree column while it sits beside a file. */
const TREE_COLUMN_WIDTH = 240;
/** Below this body width the tree and the file cannot share the pane. */
const TREE_SIDE_BY_SIDE_MIN_WIDTH = 400;
const FILTER_DEBOUNCE_MS = 120;
/** How long a revealed row stays lit after the tree scrolls to it. */
const REVEAL_FLASH_MS = 1200;
const SHOW_TREE_STORAGE_KEY = "mework.filesPane.showTree";

function describeError(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function FileKindIcon({ path, className }: { path: string; className?: string }) {
  const props = { size: 14, "aria-hidden": true as const, className };
  switch (fileIconKind(path)) {
    case "code":
      return <FileCode2 {...props} />;
    case "data":
      return <Database {...props} />;
    case "sheet":
      return <Sheet {...props} />;
    case "preso":
      return <Presentation {...props} />;
    case "image":
      return <FileImage {...props} />;
    case "archive":
      return <FileArchive {...props} />;
    case "skill":
      return <Scroll {...props} />;
    default:
      return <File {...props} />;
  }
}

/**
 * Splits `text` at `positions` so the matched characters can be emphasised.
 *
 * `positions` index the whole relative path, so a slice of it — the directory
 * prefix, the name — is matched by shifting the offsets rather than searching
 * again; a second search would find the wrong occurrence whenever the query
 * character repeats.
 */
function highlight(text: string, positions: readonly number[], offset: number): ReactNode {
  if (!positions.length) return text;
  const parts: ReactNode[] = [];
  let index = 0;
  let cursor = 0;
  while (cursor < text.length) {
    while (index < positions.length && positions[index] < cursor + offset) index += 1;
    if (index < positions.length && positions[index] === cursor + offset) {
      const start = cursor;
      while (index < positions.length && positions[index] === cursor + offset) {
        cursor += 1;
        index += 1;
      }
      parts.push(
        <span className="files-pane__match" key={start}>{text.slice(start, cursor)}</span>
      );
      continue;
    }
    const start = cursor;
    while (cursor < text.length && (index >= positions.length || positions[index] !== cursor + offset)) {
      cursor += 1;
    }
    parts.push(text.slice(start, cursor));
  }
  return parts;
}

/**
 * Depth-first flattening of the loaded listings.
 *
 * The tree is one flat list carrying `aria-level` rather than nested groups: a
 * row's position is derived from the listings, so an expansion whose directory
 * has not answered yet still has somewhere to show that it is loading.
 *
 * The cache is a `Map` because directory names are user data: a directory called
 * `constructor` or `toString` must not read back as something inherited from
 * `Object.prototype`.
 */
function buildRows(
  listings: ReadonlyMap<string, DirectoryState>,
  expanded: ReadonlySet<string>,
  loadingText: string,
  emptyText: string
): TreeRow[] {
  const rows: TreeRow[] = [];
  const walk = (parentPath: string, depth: number) => {
    const state = listings.get(parentPath);
    if (!state) return;
    if (state.status === "loading") {
      rows.push({ type: "message", id: `loading:${parentPath}`, depth, text: loadingText, failed: false });
      return;
    }
    if (state.status === "error") {
      rows.push({ type: "message", id: `error:${parentPath}`, depth, text: state.message, failed: true });
      return;
    }
    if (!state.entries.length) {
      rows.push({ type: "message", id: `empty:${parentPath}`, depth, text: emptyText, failed: false });
      return;
    }
    for (const entry of state.entries) {
      const path = joinRelativePath(parentPath, entry.name);
      const isExpanded = entry.kind === "directory" && expanded.has(path);
      rows.push({ type: "entry", path, name: entry.name, kind: entry.kind, depth, expanded: isExpanded });
      if (isExpanded) walk(path, depth + 1);
    }
  };
  walk("", 0);
  return rows;
}

/**
 * Whether a file's text has to be read to show it: everything but the files the
 * pane reads as bytes, and a video, which it cannot show at all. An SVG is both —
 * a picture, and markup its source toggle shows.
 */
function needsText(path: string): boolean {
  if (readsBytes(path)) return hasSourceForm(path);
  return fileViewerKind(path) !== "video";
}

/**
 * Inline image references in a Markdown source, plus the link-reference
 * definitions they may point at.
 *
 * The references are collected from the source rather than from the rendered
 * tree because the bytes have to be fetched before the tree can render: an
 * image the viewer has not read yet has no `data:` URL to be given.
 */
const MARKDOWN_IMAGE = /!\[[^\]]*\]\(\s*<?([^)\s>]+)>?[^)]*\)/g;
const MARKDOWN_IMAGE_DEFINITION = /^ {0,3}\[[^\]]+\]:\s*<?([^\s>]+)>?/gm;
/** `<img src="…">`, which READMEs use for anything that needs a width or a centre. */
const HTML_IMAGE = /<img\b[^>]*?\bsrc\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s>]+))/gi;

/** Every reference in `source` that names something in the workspace rather than the web. */
function documentImageReferences(source: string): string[] {
  const references = new Set<string>();
  for (const pattern of [MARKDOWN_IMAGE, MARKDOWN_IMAGE_DEFINITION, HTML_IMAGE]) {
    pattern.lastIndex = 0;
    for (;;) {
      const match = pattern.exec(source);
      if (!match) break;
      const reference = match[1] ?? match[2] ?? match[3] ?? "";
      // An address with a scheme, a protocol-relative one, and a bare fragment
      // are all somebody else's to resolve.
      if (/^[a-z][a-z0-9+.-]*:/i.test(reference)) continue;
      if (reference.startsWith("//") || reference.startsWith("#")) continue;
      references.add(reference);
    }
  }
  return [...references];
}

/** The Markdown a notebook's text cells hold, for the pictures they reference. */
function notebookMarkdown(content: string): string {
  try {
    return parseNotebook(content).cells
      .filter((cell) => cell.kind === "markdown")
      .map((cell) => cell.source)
      .join("\n\n");
  } catch {
    return "";
  }
}

/** Every directory on the way to `path`, so revealing a file can open all of them. */
function ancestorsOf(path: string): string[] {
  const ancestors: string[] = [];
  let parent = parentRelativePath(path);
  while (parent !== null && parent !== "") {
    ancestors.push(parent);
    parent = parentRelativePath(parent);
  }
  return ancestors;
}

/**
 * The workspace file browser.
 *
 * The pane is a tree column and an open file side by side, with the open files
 * as tabs in the pane's own title bar — the shape the reference shell uses.
 * Directories are requested one at a time, on expansion, and kept per path: the
 * tree the user built survives opening a file and coming back. A request counter
 * fences every response, so a listing answered after the target moved on — or
 * after a refresh — is dropped instead of contradicting the newer one; the cache
 * it would have written to is dropped with it, so nothing is left waiting on an
 * answer that will never be applied. Moving to another workspace throws the whole
 * cache away: the same relative path names a different file there.
 */
export function FilesPane({
  paneId,
  target,
  rootLabel,
  workspacePath,
  active,
  openRequest = null,
  onOpenRequestHandled,
  expanded: paneExpanded,
  onToggleExpand,
  onPaneFocus,
  onPaneClose
}: FilesPaneProps) {
  const { t } = useI18n();
  const [listings, setListings] = useState<ReadonlyMap<string, DirectoryState>>(
    () => new Map<string, DirectoryState>()
  );
  const [expanded, setExpanded] = useState<ReadonlySet<string>>(() => new Set<string>());
  const [viewers, setViewers] = useState<ReadonlyMap<string, ViewerState>>(
    () => new Map<string, ViewerState>()
  );
  const [media, setMedia] = useState<ReadonlyMap<string, MediaState>>(
    () => new Map<string, MediaState>()
  );
  const [tabs, setTabs] = useState<readonly FileTab[]>([]);
  const [activePath, setActivePath] = useState<string | null>(null);
  const [focusedPath, setFocusedPath] = useState<string | null>(null);
  const [revealNonce, setRevealNonce] = useState(0);
  const [revealedPath, setRevealedPath] = useState<string | null>(null);
  const [filter, setFilter] = useState("");
  const [debouncedFilter, setDebouncedFilter] = useState("");
  const [search, setSearch] = useState<SearchState>({ status: "idle" });
  const [showTree, setShowTree] = useState(() => readStoredFlag(SHOW_TREE_STORAGE_KEY, true));
  const [canFitTree, setCanFitTree] = useState(true);
  /**
   * The files being read as source rather than as what they are.
   *
   * A set of the exceptions rather than a mode per tab: every renderable file
   * opens rendered, and the toggle is per file because deciding to read one
   * document's Markdown source says nothing about the next one.
   */
  const [sourcePaths, setSourcePaths] = useState<ReadonlySet<string>>(() => new Set<string>());
  /**
   * Where a request asked to land once its file is on screen — a line, or a
   * heading in a document — and the bump that makes a repeat ask again.
   */
  const [pendingJump, setPendingJump] = useState<
    { path: string; line: number | null; fragment: string | null; nonce: number } | null
  >(null);
  const jumpNonceRef = useRef(0);
  const [litLine, setLitLine] = useState<{ path: string; line: number } | null>(null);

  const targetRef = useRef(target);
  const listingsRef = useRef(listings);
  const expandedRef = useRef(expanded);
  const tabsRef = useRef(tabs);
  /**
   * Files opened since the tabs last rendered. A reload in the same tick — the
   * mount's own, which React runs twice under StrictMode — would otherwise read
   * the tabs from before the open, fence off the read the open started, and
   * leave the file waiting on an answer it has already thrown away.
   */
  const openedSinceRenderRef = useRef(new Set<string>());
  const generationRef = useRef(0);
  const fileRequestRef = useRef(new Map<string, number>());
  const mediaRequestRef = useRef(new Map<string, number>());
  const searchRequestRef = useRef(0);
  const loadedTargetRef = useRef<string | null>(null);
  const openRequestRef = useRef<number | null>(null);
  const rowRefs = useRef(new Map<string, HTMLLIElement>());
  const bodyRef = useRef<HTMLDivElement>(null);
  const viewerRef = useRef<HTMLDivElement>(null);
  const filterRef = useRef<HTMLInputElement>(null);

  useLayoutEffect(() => {
    targetRef.current = target;
    listingsRef.current = listings;
    expandedRef.current = expanded;
    tabsRef.current = tabs;
    // Cleared by what the tabs now hold rather than wholesale: StrictMode replays
    // this effect before the reload it is there for.
    for (const tab of tabs) openedSinceRenderRef.current.delete(tab.path);
  });

  const loadDirectory = useCallback((relativePath: string, generation: number) => {
    setListings((current) => {
      // Entries already on screen stay there while they are re-read, so a refresh
      // does not blank the tree the user is pointing at.
      if (current.get(relativePath)?.status === "ready") return current;
      return new Map(current).set(relativePath, { status: "loading" });
    });
    listWorkspaceDirectory(targetRef.current, relativePath).then((listing) => {
      if (generation !== generationRef.current) return;
      setListings((current) => (
        new Map(current).set(relativePath, { status: "ready", entries: sortEntries(listing.entries) })
      ));
    }).catch((error: unknown) => {
      if (generation !== generationRef.current) return;
      setListings((current) => (
        new Map(current).set(relativePath, { status: "error", message: describeError(error) })
      ));
    });
  }, []);

  const loadFile = useCallback((relativePath: string, generation: number) => {
    // Two reads of the same path can be in flight at once — open, close, open
    // again — so the fence is the request's own identity, not the path it asked
    // for; otherwise the older answer would land on top of the newer one.
    const request = (fileRequestRef.current.get(relativePath) ?? 0) + 1;
    fileRequestRef.current.set(relativePath, request);
    setViewers((current) => (
      current.get(relativePath)?.status === "ready"
        ? current
        : new Map(current).set(relativePath, { status: "loading" })
    ));
    const settled = (next: ViewerState) => {
      if (generation !== generationRef.current) return;
      if (fileRequestRef.current.get(relativePath) !== request) return;
      setViewers((current) => new Map(current).set(relativePath, next));
    };
    readWorkspaceFile(targetRef.current, relativePath).then((file) => {
      settled({
        status: "ready",
        content: file.content,
        binary: file.binary,
        truncated: file.truncated
      });
    }).catch((error: unknown) => {
      settled({ status: "error", message: describeError(error) });
    });
  }, []);

  /**
   * Reads a file as bytes: a picture, a PDF, a recording, a font.
   *
   * Fenced the same way as `loadFile`, and for the same reason: a document's
   * images are requested as the document is parsed, so several reads of the same
   * path can be outstanding while the reader clicks through a directory.
   */
  const loadMedia = useCallback((relativePath: string, generation: number) => {
    const request = (mediaRequestRef.current.get(relativePath) ?? 0) + 1;
    mediaRequestRef.current.set(relativePath, request);
    setMedia((current) => (
      current.has(relativePath) ? current : new Map(current).set(relativePath, { status: "loading" })
    ));
    const settled = (next: MediaState) => {
      if (generation !== generationRef.current) return;
      if (mediaRequestRef.current.get(relativePath) !== request) return;
      setMedia((current) => new Map(current).set(relativePath, next));
    };
    const mediaType = binaryMediaType(relativePath);
    if (mediaType === null) {
      settled({ status: "error", message: t("这不是可显示的文件格式", "This is not a displayable file format") });
      return;
    }
    readWorkspaceFileBytes(targetRef.current, relativePath).then((file) => {
      settled(file.tooLarge
        ? { status: "tooLarge" }
        : { status: "ready", source: `data:${mediaType};base64,${file.data}` });
    }).catch((error: unknown) => {
      settled({ status: "error", message: describeError(error) });
    });
  }, [t]);

  /**
   * Re-reads everything currently on screen under a fresh generation.
   *
   * `discardCache` is for a target change: every cached listing belongs to the
   * workspace that was left behind. Otherwise only the directories still waiting
   * on the previous generation are dropped — their answers are about to be
   * ignored, and a cached `loading` nobody is driving would show the next
   * expansion a reader that never finishes.
   */
  const reload = useCallback((discardCache: boolean) => {
    const generation = ++generationRef.current;
    const expandedPaths = [...expandedRef.current];
    const openPaths = [...new Set([...tabsRef.current.map((tab) => tab.path), ...openedSinceRenderRef.current])];
    setListings((current) => {
      if (discardCache) return new Map<string, DirectoryState>();
      const next = new Map(current);
      for (const [path, state] of next) {
        if (state.status === "loading") next.delete(path);
      }
      return next;
    });
    if (discardCache) {
      setViewers(new Map(openPaths.filter(needsText).map((path) => [path, { status: "loading" } as ViewerState])));
    }
    // Pictures are re-read from whatever the open files turn out to reference,
    // so the cache is emptied rather than refilled here.
    setMedia(new Map<string, MediaState>());
    loadDirectory("", generation);
    for (const path of expandedPaths) loadDirectory(path, generation);
    for (const path of openPaths) if (needsText(path)) loadFile(path, generation);
  }, [loadDirectory, loadFile]);

  const refresh = useCallback(() => reload(false), [reload]);

  const targetKey = gitTargetKey(target);
  useEffect(() => {
    if (!active) return;
    const movedWorkspace = loadedTargetRef.current !== null && loadedTargetRef.current !== targetKey;
    loadedTargetRef.current = targetKey;
    reload(movedWorkspace);
  }, [active, targetKey, reload]);

  // The body decides whether the tree can sit beside a file; below the threshold
  // the tree takes the whole pane and the file is the one that steps aside.
  useEffect(() => {
    const body = bodyRef.current;
    if (!body || typeof ResizeObserver === "undefined") return;
    const measure = (width: number) => {
      if (width <= 0) return;
      setCanFitTree(width >= TREE_SIDE_BY_SIDE_MIN_WIDTH);
    };
    measure(body.clientWidth);
    const observer = new ResizeObserver(([entry]) => measure(entry?.contentRect.width ?? 0));
    observer.observe(body);
    return () => observer.disconnect();
  }, []);

  useEffect(() => {
    const timer = window.setTimeout(() => setDebouncedFilter(filter.trim()), FILTER_DEBOUNCE_MS);
    return () => window.clearTimeout(timer);
  }, [filter]);

  useEffect(() => {
    if (!active) return;
    const query = debouncedFilter;
    const request = ++searchRequestRef.current;
    if (!query) {
      setSearch({ status: "idle" });
      return;
    }
    setSearch((current) => (current.status === "ready" ? current : { status: "loading" }));
    searchWorkspaceFiles(targetRef.current, query).then((results) => {
      if (request !== searchRequestRef.current) return;
      setSearch({ status: "ready", matches: results.matches, truncated: results.truncated });
    }).catch((error: unknown) => {
      if (request !== searchRequestRef.current) return;
      setSearch({ status: "error", message: describeError(error) });
    });
  }, [active, debouncedFilter, targetKey]);

  const searching = debouncedFilter.length > 0;

  const rows = useMemo(() => buildRows(
    listings,
    expanded,
    t("正在读取…", "Loading…"),
    t("文件夹为空", "Folder is empty")
  ), [expanded, listings, t]);

  const treeRows = useMemo(
    () => rows.filter((row): row is Extract<TreeRow, { type: "entry" }> => row.type === "entry"),
    [rows]
  );
  const rowIndexByPath = useMemo(
    () => new Map(treeRows.map((row, index) => [row.path, index])),
    [treeRows]
  );
  const searchRows = search.status === "ready" ? search.matches : [];
  const navigablePaths = searching
    ? searchRows.map((match) => match.path)
    : treeRows.map((row) => row.path);

  const registerRow = (path: string) => (element: HTMLLIElement | null) => {
    if (element) rowRefs.current.set(path, element);
    else rowRefs.current.delete(path);
  };

  const focusRow = (path: string | undefined) => {
    if (path === undefined) return;
    setFocusedPath(path);
    rowRefs.current.get(path)?.focus();
  };

  const toggleDirectory = useCallback((path: string) => {
    const open = expandedRef.current.has(path);
    const next = new Set(expandedRef.current);
    if (open) next.delete(path);
    else next.add(path);
    expandedRef.current = next;
    setExpanded(next);
    const cached = listingsRef.current.get(path);
    // A directory that failed is retried on the next expansion; a cached one is not
    // re-read, which is the whole point of keeping the listing.
    if (!open && (!cached || cached.status === "error")) {
      loadDirectory(path, generationRef.current);
    }
  }, [loadDirectory]);

  /**
   * Opens `path` as a tab.
   *
   * `keep` is the difference between a glance and a decision: a single click
   * reuses the one preview slot, a double click — or opening the same file
   * again — turns it into a tab of its own.
   */
  const openFile = useCallback((path: string, keep: boolean) => {
    setTabs((current) => {
      const existing = current.findIndex((tab) => tab.path === path);
      if (existing >= 0) {
        if (!keep || !current[existing].preview) return current;
        const next = [...current];
        next[existing] = { path, preview: false };
        return next;
      }
      const previewIndex = current.findIndex((tab) => tab.preview);
      const opened: FileTab = { path, preview: !keep };
      if (previewIndex >= 0) {
        const next = [...current];
        next[previewIndex] = opened;
        return next;
      }
      return [...current, opened];
    });
    setActivePath(path);
    openedSinceRenderRef.current.add(path);
    if (needsText(path) && viewers.get(path)?.status !== "ready") loadFile(path, generationRef.current);
  }, [loadFile, viewers]);

  const keepTab = useCallback((path: string) => {
    setTabs((current) => {
      const index = current.findIndex((tab) => tab.path === path);
      if (index < 0 || !current[index].preview) return current;
      const next = [...current];
      next[index] = { path, preview: false };
      return next;
    });
  }, []);

  const closeTabs = useCallback((doomed: (tab: FileTab) => boolean) => {
    setTabs((current) => {
      for (const tab of current) if (doomed(tab)) openedSinceRenderRef.current.delete(tab.path);
      const next = current.filter((tab) => !doomed(tab));
      if (next.length === current.length) return current;
      setActivePath((currentActive) => {
        if (currentActive === null) return null;
        if (next.some((tab) => tab.path === currentActive)) return currentActive;
        if (!next.length) return null;
        // Closing the active tab lands on its neighbour rather than the start of
        // the strip: the file next to the one being dismissed is the one being
        // worked through.
        const removedAt = current.findIndex((tab) => tab.path === currentActive);
        const fallback = next[Math.min(removedAt, next.length - 1)];
        return fallback.path;
      });
      setViewers((currentViewers) => {
        const keep = new Map(currentViewers);
        for (const tab of current) if (!next.includes(tab)) keep.delete(tab.path);
        return keep;
      });
      return next;
    });
  }, []);

  const closeTab = useCallback((path: string) => {
    closeTabs((tab) => tab.path === path);
  }, [closeTabs]);

  /** Puts the tabs in the order the strip was dragged or keyed into. */
  const reorderTabs = useCallback((paths: string[]) => {
    setTabs((current) => arrangeByIds(current, paths, (tab) => tab.path) ?? current);
  }, []);

  /** Opens every directory on the way to `path` and scrolls its row into view. */
  const revealInTree = useCallback((path: string) => {
    const ancestors = ancestorsOf(path);
    const next = new Set(expandedRef.current);
    for (const ancestor of ancestors) {
      if (!next.has(ancestor)) {
        next.add(ancestor);
        const cached = listingsRef.current.get(ancestor);
        if (!cached || cached.status === "error") loadDirectory(ancestor, generationRef.current);
      }
    }
    expandedRef.current = next;
    setExpanded(next);
    setFilter("");
    setShowTree(true);
    writeStoredFlag(SHOW_TREE_STORAGE_KEY, true);
    setFocusedPath(path);
    setRevealedPath(path);
    setRevealNonce((current) => current + 1);
  }, [loadDirectory]);

  useEffect(() => {
    if (!revealNonce || focusedPath === null) return;
    rowRefs.current.get(focusedPath)?.scrollIntoView({ block: "nearest" });
  }, [revealNonce, focusedPath, rows]);

  // The row lights up so the eye can find where the tree jumped to, then lets go.
  useEffect(() => {
    if (revealedPath === null) return;
    const timer = window.setTimeout(() => setRevealedPath(null), REVEAL_FLASH_MS);
    return () => window.clearTimeout(timer);
  }, [revealNonce, revealedPath]);

  const activateRow = (path: string, kind: WorkspaceEntryKind, keep: boolean) => {
    setFocusedPath(path);
    if (kind === "directory") toggleDirectory(path);
    else openFile(path, keep);
  };

  const onRowKeyDown = (
    event: KeyboardEvent<HTMLLIElement>,
    path: string,
    kind: WorkspaceEntryKind,
    rowExpanded: boolean,
    depth: number
  ) => {
    const index = navigablePaths.indexOf(path);
    switch (event.key) {
      case "ArrowDown":
        focusRow(navigablePaths[index + 1]);
        break;
      case "ArrowUp":
        focusRow(navigablePaths[index - 1]);
        break;
      case "ArrowRight": {
        if (searching || kind !== "directory") return;
        if (!rowExpanded) toggleDirectory(path);
        else if ((treeRows[index + 1]?.depth ?? depth) > depth) focusRow(navigablePaths[index + 1]);
        break;
      }
      case "ArrowLeft": {
        if (searching) return;
        if (kind === "directory" && rowExpanded) toggleDirectory(path);
        else {
          const parent = parentRelativePath(path);
          if (parent) focusRow(parent);
        }
        break;
      }
      case "Enter":
        activateRow(path, kind, true);
        break;
      default:
        return;
    }
    event.preventDefault();
  };

  const rootState = listings.get("");
  const activeViewer = activePath === null ? null : viewers.get(activePath) ?? null;
  const activeKind = activePath === null ? null : fileViewerKind(activePath);
  const activeSource = activePath !== null && sourcePaths.has(activePath);
  // Only a file with two readings offers the switch, and only once it is known
  // to have a text form — a PNG's "source" is the binary notice.
  const canReadSource = activePath !== null
    && hasSourceForm(activePath)
    && activeViewer?.status === "ready"
    && !activeViewer.binary;

  /**
   * The bytes the open files need: a picture, PDF, recording or font tab needs its
   * own, and a rendered document — Markdown, or a notebook's text cells — needs
   * every picture it points at.
   *
   * Derived rather than accumulated so the set shrinks when a tab closes; the
   * bytes are held as `data:` URLs, and the host will hand over eight megabytes
   * of one before it refuses.
   */
  const neededMedia = useMemo(() => {
    const needed = new Set<string>();
    for (const tab of tabs) {
      if (sourcePaths.has(tab.path)) continue;
      if (readsBytes(tab.path)) {
        needed.add(tab.path);
        continue;
      }
      const kind = fileViewerKind(tab.path);
      if (kind !== "markdown" && kind !== "notebook") continue;
      const viewer = viewers.get(tab.path);
      if (viewer?.status !== "ready" || viewer.binary) continue;
      const markdown = kind === "notebook" ? notebookMarkdown(viewer.content) : viewer.content;
      for (const reference of documentImageReferences(markdown)) {
        const resolved = resolveDocumentReference(tab.path, reference.split("#")[0]);
        if (resolved !== null && imageMediaType(resolved) !== null) needed.add(resolved);
      }
    }
    return needed;
  }, [sourcePaths, tabs, viewers]);

  useEffect(() => {
    if (!active) return;
    for (const path of neededMedia) {
      if (!media.has(path)) loadMedia(path, generationRef.current);
    }
    if (![...media.keys()].some((path) => !neededMedia.has(path))) return;
    setMedia((current) => {
      const next = new Map([...current].filter(([path]) => neededMedia.has(path)));
      return next.size === current.size ? current : next;
    });
  }, [active, media, loadMedia, neededMedia]);

  /**
   * Opens `path` and lands on a place in it once it is on screen.
   *
   * A line names a place in the source, so a file that has a rendered form shows
   * its source for it; a heading names a place in the rendered form, so it stays.
   */
  const openAt = useCallback((path: string, line: number | null, fragment: string | null, keep: boolean) => {
    openFile(path, keep);
    if (line !== null && hasSourceForm(path) && !readsBytes(path)) {
      setSourcePaths((current) => (current.has(path) ? current : new Set(current).add(path)));
    }
    if (line === null && fragment === null) return;
    jumpNonceRef.current += 1;
    setPendingJump({ path, line, fragment, nonce: jumpNonceRef.current });
  }, [openFile]);

  /**
   * Opens what the timeline asked for.
   *
   * The nonce is what makes a second click on the same path ask again, and it is
   * kept in a ref so a request that arrived while the pane was closed is still
   * honoured on the mount that follows; the owner is told once it has been, so a
   * later mount does not replay it. Opening as a preview tab matches a single
   * click in the tree: following a reference is a glance, not a decision.
   */
  useEffect(() => {
    if (!openRequest || openRequestRef.current === openRequest.nonce) return;
    openRequestRef.current = openRequest.nonce;
    // A pane opened just to show this file shows the file: the tree folds away
    // for this visit without changing what the reader chose for the pane itself.
    if (openRequest.collapseTree) setShowTree(false);
    openAt(openRequest.path, openRequest.line, null, false);
    onOpenRequestHandled?.(openRequest.nonce);
  }, [onOpenRequestHandled, openAt, openRequest]);

  // The jump waits for the file it is a place in: the rows do not exist until the
  // read lands, and scrolling before then would settle on the wrong offset.
  useEffect(() => {
    if (pendingJump === null || activePath !== pendingJump.path) return;
    if (needsText(pendingJump.path) && viewers.get(pendingJump.path)?.status !== "ready") return;
    if (pendingJump.line !== null) {
      const row = viewerRef.current?.querySelector(`[data-line="${pendingJump.line}"]`);
      if (row) scrollIntoContainer(row, "center");
      setLitLine(row ? { path: pendingJump.path, line: pendingJump.line } : null);
    } else if (pendingJump.fragment !== null) {
      scrollToFragment(viewerRef.current, pendingJump.fragment);
    }
    setPendingJump(null);
  }, [activePath, pendingJump, viewers]);

  useEffect(() => {
    if (litLine === null) return;
    const timer = window.setTimeout(() => setLitLine(null), REVEAL_FLASH_MS);
    return () => window.clearTimeout(timer);
  }, [litLine]);

  /**
   * Turns a document's own image reference into bytes already read.
   *
   * Synchronous because it runs while the Markdown renders; anything not read
   * yet answers null and is drawn as its alternative text until the read that
   * `neededMedia` started lands and the document renders again.
   */
  const resolveImageSrc = useCallback((source: string) => {
    if (activePath === null) return source;
    if (/^[a-z][a-z0-9+.-]*:/i.test(source) || source.startsWith("//")) return source;
    const resolved = resolveDocumentReference(activePath, source.split("#")[0]);
    if (resolved === null) return null;
    const picture = media.get(resolved);
    return picture?.status === "ready" ? picture.source : null;
  }, [activePath, media]);

  /**
   * Whether `path` is a directory as far as the listings already read can tell.
   * A link written with a trailing slash says so itself.
   */
  const isKnownDirectory = useCallback((path: string): boolean => {
    const parent = parentRelativePath(path) ?? "";
    const listing = listingsRef.current.get(parent);
    if (listing?.status !== "ready") return false;
    const name = basename(path);
    return listing.entries.some((entry) => entry.name === name && entry.kind === "directory");
  }, []);

  /**
   * Follows a link inside a rendered document.
   *
   * Every address that is not the open web is handled here, because the
   * alternative is the default one: a relative `href` resolves against the app's
   * own origin, and letting it through navigates the whole window away from the
   * app. External addresses are left to the document-level interceptor, which
   * hands them to the system browser. A link to a file opens it, at the line or
   * heading it names; a link to a directory shows it in the tree.
   */
  const onDocumentClick = useCallback((event: MouseEvent<HTMLDivElement>) => {
    if (event.defaultPrevented || event.button !== 0) return;
    if (event.ctrlKey || event.metaKey || event.shiftKey || event.altKey) return;
    const node = event.target instanceof Element ? event.target.closest("a[href]") : null;
    if (!(node instanceof HTMLAnchorElement)) return;
    const href = node.getAttribute("href") ?? "";
    if (externalHttpUrl(href) !== null) return;
    event.preventDefault();
    const target = localLinkTarget(href);
    if (!target) return;
    if (!target.path) {
      if (target.fragment) scrollToFragment(viewerRef.current, target.fragment);
      return;
    }
    if (activePath === null) return;
    const resolved = resolveDocumentReference(activePath, target.path);
    if (resolved === null) return;
    if (target.path.endsWith("/") || isKnownDirectory(resolved)) {
      revealInTree(resolved);
      if (!expandedRef.current.has(resolved)) toggleDirectory(resolved);
      return;
    }
    openAt(resolved, target.line, target.fragment, false);
  }, [activePath, isKnownDirectory, openAt, revealInTree, toggleDirectory]);

  /** What a page in an HTML preview reads from the workspace: its stylesheets and its pictures. */
  const htmlResources = useMemo<HtmlPreviewResources>(() => ({
    readText: async (path) => {
      try {
        const file = await readWorkspaceFile(targetRef.current, path);
        return file.binary ? null : file.content;
      } catch {
        return null;
      }
    },
    readImage: async (path) => {
      const mediaType = imageMediaType(path);
      if (mediaType === null) return null;
      try {
        const file = await readWorkspaceFileBytes(targetRef.current, path);
        return file.tooLarge ? null : `data:${mediaType};base64,${file.data}`;
      } catch {
        return null;
      }
    }
  }), []);

  const onOpenFileFromPage = useCallback((path: string, line: number | null) => {
    openAt(path, line, null, false);
  }, [openAt]);

  const revealActiveFile = useMemo(() => {
    if (activePath === null || !workspacePath || !hasBackendRuntime()) return undefined;
    return () => {
      void revealPath(activePath, workspacePath).catch(() => undefined);
    };
  }, [activePath, workspacePath]);

  const setShowTreePreference = useCallback((next: boolean) => {
    setShowTree(next);
    writeStoredFlag(SHOW_TREE_STORAGE_KEY, next);
  }, []);

  // The tree folds away on request; with a file open it also steps aside when
  // the pane is too narrow for the two to share it.
  const treeVisible = activePath === null ? showTree : canFitTree && showTree;
  const treeIsColumn = treeVisible && canFitTree && activePath !== null;
  const viewerVisible = activePath !== null;
  const emptyStateVisible = !viewerVisible && (!treeVisible || canFitTree);
  const treeToggleDisabled = activePath !== null && !canFitTree;

  const fileMenuSections = useCallback((path: string, inTab: boolean): PopoverMenuSection[] => {
    const absolute = workspacePath ? `${workspacePath}/${path}` : path;
    const sections: PopoverMenuSection[] = [
      {
        id: "path",
        items: [
          {
            id: "copy-relative",
            label: t("复制相对路径", "Copy relative path"),
            icon: <Copy size={13} aria-hidden="true" />,
            onSelect: () => void navigator.clipboard?.writeText(path)
          },
          {
            id: "copy-absolute",
            label: t("复制路径", "Copy path"),
            icon: <Copy size={13} aria-hidden="true" />,
            disabled: !workspacePath,
            onSelect: () => void navigator.clipboard?.writeText(absolute)
          }
        ]
      }
    ];
    if (inTab) {
      sections.push({
        id: "reveal",
        items: [
          {
            id: "reveal-in-tree",
            label: t("在文件树中显示", "Reveal in file tree"),
            onSelect: () => revealInTree(path)
          }
        ]
      }, {
        id: "close",
        items: [
          ...(tabs.some((tab) => tab.path === path && tab.preview)
            ? [{
                id: "keep-open",
                label: t("固定打开", "Keep open"),
                onSelect: () => keepTab(path)
              }]
            : []),
          { id: "close", label: t("关闭文件", "Close file"), onSelect: () => closeTab(path) },
          {
            id: "close-others",
            label: t("关闭其他文件", "Close other files"),
            disabled: tabs.length < 2,
            onSelect: () => closeTabs((tab) => tab.path !== path)
          },
          {
            id: "close-all",
            label: t("关闭所有文件", "Close all files"),
            onSelect: () => closeTabs(() => true)
          }
        ]
      });
    } else {
      sections.push({
        id: "open",
        items: [
          {
            id: "open-keep",
            label: t("打开文件", "Open file"),
            onSelect: () => openFile(path, true)
          }
        ]
      });
    }
    return sections;
  }, [closeTab, closeTabs, keepTab, openFile, revealInTree, t, tabs, workspacePath]);

  const paneMenuSections = useMemo<PopoverMenuSection[]>(() => {
    const sections: PopoverMenuSection[] = [];
    sections.push({
      id: "files-tree",
      items: [
        {
          id: "show-tree",
          label: t("显示文件树", "Show file tree"),
          checked: treeVisible,
          checkedRole: "checkbox",
          disabled: treeToggleDisabled,
          description: treeToggleDisabled ? t("面板太窄，放不下文件树", "The pane is too narrow for the tree") : undefined,
          onSelect: () => setShowTreePreference(!showTree)
        }
      ]
    });
    sections.push({
      id: "files-refresh",
      items: [{ id: "refresh", label: t("刷新", "Refresh"), onSelect: refresh }]
    });
    return sections;
  }, [
    refresh,
    setShowTreePreference,
    showTree,
    t,
    treeToggleDisabled,
    treeVisible
  ]);

  const treeToggleLabel = treeVisible
    ? t("收起文件目录", "Collapse file tree")
    : t("展开文件目录", "Expand file tree");

  // The title stays put with the drawer handle right after it, whatever the tab
  // strip beside them is doing, so folding the tree away is always one reach.
  const header = (
    <div className="files-pane__header">
      <span className="files-pane__pane-title" title={workspacePath ?? rootLabel}>{t("文件", "Files")}</span>
      <IconButton
        className="files-pane__tree-toggle"
        label={treeToggleLabel}
        aria-expanded={treeVisible}
        disabled={treeToggleDisabled}
        onMouseDown={(event) => event.preventDefault()}
        onClick={() => setShowTreePreference(!showTree)}
      >
        {treeVisible
          ? <PanelLeftClose size={14} aria-hidden="true" />
          : <PanelLeftOpen size={14} aria-hidden="true" />}
      </IconButton>
      {tabs.length > 0 && (
          <FileTabStrip
            tabs={tabs}
            activePath={activePath}
            workspacePath={workspacePath}
            onActivate={(path) => {
              setActivePath(path);
              if (needsText(path) && viewers.get(path)?.status !== "ready") loadFile(path, generationRef.current);
            }}
            onKeep={keepTab}
            onClose={closeTab}
            onReorder={reorderTabs}
            menuSections={fileMenuSections}
          />
        )}
    </div>
  );

  return (
    <SidePane
      id={paneId}
      title={t("文件", "Files")}
      header={header}
      menuSections={paneMenuSections}
      expanded={paneExpanded}
      onToggleExpand={onToggleExpand}
      onFocus={onPaneFocus}
      onClose={onPaneClose}
    >
      <div className="files-pane" ref={bodyRef}>
        <div
          className={`files-pane__tree-column${treeIsColumn ? " files-pane__tree-column--aside" : ""}`}
          data-files-tree
          hidden={!treeVisible}
          style={treeIsColumn ? { width: TREE_COLUMN_WIDTH } : undefined}
        >
          <div className="files-pane__filter">
            <span className="files-pane__filter-icon" aria-hidden="true">
              {search.status === "loading"
                ? <LoaderCircle size={13} className="spin" />
                : <Search size={13} />}
            </span>
            <input
              ref={filterRef}
              type="text"
              className="files-pane__filter-input"
              value={filter}
              spellCheck={false}
              autoComplete="off"
              aria-label={t("筛选文件", "Filter files")}
              placeholder={t("筛选文件…", "Filter files…")}
              onChange={(event) => setFilter(event.target.value)}
              onKeyDown={(event) => {
                if (event.key !== "Escape" || !filter) return;
                event.preventDefault();
                setFilter("");
              }}
            />
            {filter && (
              <IconButton
                className="files-pane__filter-clear"
                label={t("清除筛选", "Clear filter")}
                onClick={() => {
                  setFilter("");
                  filterRef.current?.focus();
                }}
              >
                <X size={12} aria-hidden="true" />
              </IconButton>
            )}
          </div>

          {searching
            ? (
              <ul className="files-pane__tree" role="listbox" aria-label={t("匹配的文件", "Matching files")}>
                {search.status === "error" && (
                  <li className="files-pane__row files-pane__row--message files-pane__row--failed" role="none">
                    <span className="files-pane__name">{search.message}</span>
                  </li>
                )}
                {search.status === "ready" && searchRows.length === 0 && (
                  <li className="files-pane__row files-pane__row--message" role="none">
                    <span className="files-pane__name">{t("没有匹配的文件", "No matching files")}</span>
                  </li>
                )}
                {searchRows.map((match) => {
                  const separator = match.path.lastIndexOf("/");
                  const directory = separator > 0 ? match.path.slice(0, separator) : "";
                  const selected = match.path === activePath;
                  return (
                    <li
                      key={match.path}
                      ref={registerRow(match.path)}
                      role="option"
                      aria-selected={selected}
                      tabIndex={match.path === (focusedPath ?? navigablePaths[0]) ? 0 : -1}
                      className={`files-pane__row${selected ? " files-pane__row--current" : ""}`}
                      style={{ paddingLeft: ROOT_INDENT }}
                      onClick={(event) => activateRow(match.path, match.kind, event.detail !== 1)}
                      onFocus={() => setFocusedPath(match.path)}
                      onKeyDown={(event) => onRowKeyDown(event, match.path, match.kind, false, 0)}
                      title={match.path}
                    >
                      {match.kind === "directory"
                        ? <FolderOpen size={14} aria-hidden="true" className="files-pane__icon" />
                        : <FileKindIcon path={match.path} className="files-pane__icon" />}
                      <span className="files-pane__name">
                        {highlight(match.name, match.positions, separator + 1)}
                      </span>
                      {directory && (
                        <span className="files-pane__detail">
                          {highlight(directory, match.positions, 0)}
                        </span>
                      )}
                      <RowMenu sections={fileMenuSections(match.path, false)} label={t("文件操作", "File actions")} />
                    </li>
                  );
                })}
                {search.status === "ready" && search.truncated && (
                  <li className="files-pane__row files-pane__row--message" role="none">
                    <span className="files-pane__name">
                      {t("结果过多，只显示了一部分", "Too many results; only some are shown")}
                    </span>
                  </li>
                )}
              </ul>
            )
            : rootState?.status === "error"
              ? <p className="files-pane__error" role="alert">{rootState.message}</p>
              : (
                <ul className="files-pane__tree" role="tree" aria-label={t("项目文件", "Project files")}>
                  {rows.map((row) => {
                    if (row.type === "message") {
                      return (
                        <li
                          key={row.id}
                          role="none"
                          className={`files-pane__row files-pane__row--message${row.failed ? " files-pane__row--failed" : ""}`}
                          style={{ paddingLeft: ROOT_INDENT + row.depth * INDENT_PER_LEVEL }}
                        >
                          <span className="files-pane__name">{row.text}</span>
                        </li>
                      );
                    }
                    const directory = row.kind === "directory";
                    const current = row.path === activePath;
                    const rowFocus = focusedPath !== null && rowIndexByPath.has(focusedPath)
                      ? focusedPath
                      : treeRows[0]?.path ?? null;
                    return (
                      <li
                        key={row.path}
                        ref={registerRow(row.path)}
                        role="treeitem"
                        aria-level={row.depth + 1}
                        aria-expanded={directory ? row.expanded : undefined}
                        aria-current={current || undefined}
                        tabIndex={row.path === rowFocus ? 0 : -1}
                        className={`files-pane__row${current ? " files-pane__row--current" : ""}${row.path === revealedPath ? " files-pane__row--revealed" : ""}`}
                        style={{ paddingLeft: ROOT_INDENT + row.depth * INDENT_PER_LEVEL }}
                        onClick={(event) => activateRow(row.path, row.kind, event.detail !== 1)}
                        onFocus={() => setFocusedPath(row.path)}
                        onKeyDown={(event) => onRowKeyDown(event, row.path, row.kind, row.expanded, row.depth)}
                        title={row.path}
                      >
                        {directory
                          ? (
                            <ChevronRight
                              size={14}
                              aria-hidden="true"
                              className={`files-pane__chevron${row.expanded ? " files-pane__chevron--open" : ""}`}
                            />
                          )
                          : <FileKindIcon path={row.path} className="files-pane__icon" />}
                        <span className="files-pane__name">{row.name}</span>
                        <RowMenu sections={fileMenuSections(row.path, false)} label={t("文件操作", "File actions")} />
                      </li>
                    );
                  })}
                </ul>
              )}
        </div>

        {viewerVisible && activePath !== null && (
          <div className="files-pane__viewer" ref={viewerRef}>
            {/* The reference shell gives the open file its own breadcrumb row with the
                actions that belong to the file rather than to the pane. */}
            <div className="files-pane__viewer-bar">
              <span className="files-pane__viewer-path" title={workspacePath ? `${workspacePath}/${activePath}` : activePath}>
                <span className="files-pane__viewer-directory">
                  {activePath.slice(0, activePath.lastIndexOf("/") + 1)}
                </span>
                <span className="files-pane__viewer-name">{basename(activePath)}</span>
              </span>
              {canReadSource && (
                <IconButton
                  className="files-pane__viewer-button"
                  label={activeSource
                    ? t("显示渲染结果", "Show rendered")
                    : t("显示源码", "Show source")}
                  aria-pressed={activeSource}
                  onClick={() => setSourcePaths((current) => {
                    const next = new Set(current);
                    if (!next.delete(activePath)) next.add(activePath);
                    return next;
                  })}
                >
                  {activeSource
                    ? <Eye size={13} aria-hidden="true" />
                    : <Code2 size={13} aria-hidden="true" />}
                </IconButton>
              )}
              <IconButton
                className="files-pane__viewer-button"
                label={t("重新读取文件", "Reload file")}
                onClick={() => {
                  if (needsText(activePath)) loadFile(activePath, generationRef.current);
                  // The bytes behind a picture are cached separately, and a
                  // reload that left them alone would keep showing the old one.
                  setMedia((current) => {
                    if (!current.size) return current;
                    const next = new Map(current);
                    next.delete(activePath);
                    for (const path of neededMedia) next.delete(path);
                    return next;
                  });
                }}
              >
                <RotateCw size={13} aria-hidden="true" />
              </IconButton>
              <IconButton
                className="files-pane__viewer-button"
                label={t("在文件树中显示", "Reveal in file tree")}
                onClick={() => revealInTree(activePath)}
              >
                <FolderTree size={13} aria-hidden="true" />
              </IconButton>
              <IconButton
                className="files-pane__viewer-button"
                label={t("复制文件内容", "Copy file contents")}
                disabled={activeViewer?.status !== "ready" || activeViewer.binary}
                onClick={() => {
                  if (activeViewer?.status !== "ready") return;
                  void navigator.clipboard?.writeText(activeViewer.content);
                }}
              >
                <Copy size={13} aria-hidden="true" />
              </IconButton>
            </div>
            {activeViewer?.status === "ready" && activeViewer.truncated && needsText(activePath) && (
              <p className="files-pane__notice">
                {t("文件过大，只显示了开头部分", "This file is too large; only its beginning is shown")}
              </p>
            )}
            <FileViewerBody
              path={activePath}
              kind={activeKind ?? "text"}
              source={activeSource}
              viewer={activeViewer}
              picture={media.get(activePath) ?? null}
              litLine={litLine !== null && litLine.path === activePath ? litLine.line : null}
              pathBaseDir={workspacePath}
              onDocumentClick={onDocumentClick}
              resolveImageSrc={resolveImageSrc}
              htmlResources={htmlResources}
              onOpenFile={onOpenFileFromPage}
              onReveal={revealActiveFile}
            />
          </div>
        )}

        {emptyStateVisible && (
          <div className="files-pane__placeholder">
            <div className="files-pane__placeholder-icon" aria-hidden="true">
              <FolderOpen size={20} />
            </div>
            <h3>
              {tabs.length > 0
                ? t("未选择文件", "No file selected")
                : t("打开的文件会出现在这里", "Open files appear here")}
            </h3>
            <p>
              {tabs.length > 0
                ? t("从上方的标签中选一个文件。", "Pick an open file above.")
                : treeVisible
                  ? t("在文件树中选择一个文件。", "Pick a file in the tree.")
                  : t("打开文件树来浏览这个工作区。", "Show the file tree to browse this workspace.")}
            </p>
          </div>
        )}
      </div>
    </SidePane>
  );
}

/** The `⋮` that only appears while its row is hovered or holds focus. */
function RowMenu({ sections, label }: { sections: PopoverMenuSection[]; label: string }) {
  return (
    <PopoverMenu
      rootClassName="files-pane__row-menu"
      triggerClassName="icon-button files-pane__row-menu-trigger"
      trigger={<MoreVertical size={13} aria-hidden="true" />}
      triggerLabel={label}
      menuLabel={label}
      sections={sections}
      align="end"
      dense
    />
  );
}

interface FileViewerBodyProps {
  path: string;
  kind: FileViewerKind;
  /** True while the reader has asked for the source of a file that has another form. */
  source: boolean;
  viewer: ViewerState | null;
  /** The file's bytes, for the kinds read that way. */
  picture: MediaState | null;
  /** The line to light, already narrowed to this file. */
  litLine: number | null;
  /** Workspace root, so a path written inside a document resolves the way it was meant. */
  pathBaseDir: string | null;
  onDocumentClick: (event: MouseEvent<HTMLDivElement>) => void;
  resolveImageSrc: (src: string) => string | null;
  htmlResources: HtmlPreviewResources;
  onOpenFile: (path: string, line: number | null) => void;
  /** Shows the file in the system file manager; absent where there is none to reach. */
  onReveal?: () => void;
}

/**
 * What an open file looks like.
 *
 * One file, one reading: a document is rendered, a picture drawn, a PDF laid out,
 * a recording played, a font set, a table tabulated — and everything else is its
 * own text, numbered and coloured. Kinds that are pictures of bytes are read as
 * bytes; the rest are read as text, which is also what says whether there is
 * anything to show at all.
 */
function FileViewerBody({
  path,
  kind,
  source,
  viewer,
  picture,
  litLine,
  pathBaseDir,
  onDocumentClick,
  resolveImageSrc,
  htmlResources,
  onOpenFile,
  onReveal
}: FileViewerBodyProps) {
  const { t } = useI18n();
  const name = basename(path);

  if (kind === "video") {
    return (
      <UnsupportedNotice
        message={t(
          "视频无法在应用内播放：界面的安全策略不允许加载媒体。",
          "Video cannot be played inside the app: the window's security policy allows no media."
        )}
        onReveal={onReveal}
      />
    );
  }

  if (readsBytes(path) && !source) {
    if (picture === null || picture.status === "loading") {
      return <p className="files-pane__notice">{t("正在读取…", "Loading…")}</p>;
    }
    if (picture.status === "tooLarge") {
      return (
        <UnsupportedNotice
          message={t("文件过大（超过 8 MB），无法在面板中预览。", "This file is larger than 8 MB and cannot be previewed in the pane.")}
          onReveal={onReveal}
        />
      );
    }
    if (picture.status === "error") {
      return <p className="files-pane__error" role="alert">{picture.message}</p>;
    }
    const bytes = dataUrlByteLength(picture.source);
    switch (kind) {
      case "image":
        return <ImageViewer key={path} source={picture.source} name={name} bytes={bytes} onReveal={onReveal} />;
      case "pdf":
        return <PdfViewer key={path} source={picture.source} bytes={bytes} />;
      case "audio":
        return <AudioPlayer key={path} source={picture.source} name={name} bytes={bytes} />;
      case "font":
        return <FontPreview key={path} source={picture.source} name={name} bytes={bytes} />;
      default:
        break;
    }
  }

  if (viewer === null || viewer.status === "loading") {
    return <p className="files-pane__notice">{t("正在读取…", "Loading…")}</p>;
  }
  if (viewer.status === "error") {
    return <p className="files-pane__error" role="alert">{viewer.message}</p>;
  }
  if (viewer.binary) {
    return <UnsupportedNotice message={t("二进制文件，无法显示", "Binary file cannot be shown")} onReveal={onReveal} />;
  }
  if (viewer.content === "") {
    return <p className="files-pane__notice">{t("这个文件是空的。", "This file is empty.")}</p>;
  }

  if (!source) {
    switch (kind) {
      case "markdown": {
        const frontMatter = splitFrontMatter(viewer.content);
        return (
          <div className="files-pane__document" onClickCapture={onDocumentClick}>
            {frontMatter && (
              <div className="files-pane__front-matter markdown-content">
                <MarkdownCodeBlock code={frontMatter.source} language={frontMatter.language} label={frontMatter.language} />
              </div>
            )}
            <MarkdownContent
              content={frontMatter ? frontMatter.body : viewer.content}
              // A path written in a document is written against the checkout, the
              // way a repository's own prose writes one; a link is written against
              // the document, and is resolved by the click handler instead.
              linkifyPaths
              documentLinks
              renderHtml
              pathBaseDir={pathBaseDir}
              resolveImageSrc={resolveImageSrc}
            />
          </div>
        );
      }
      case "html":
        return <HtmlPreview key={path} path={path} content={viewer.content} resources={htmlResources} onOpenFile={onOpenFile} />;
      case "csv":
        return <CsvTable key={path} path={path} content={viewer.content} />;
      case "notebook":
        return (
          <NotebookView
            content={viewer.content}
            pathBaseDir={pathBaseDir}
            resolveImageSrc={resolveImageSrc}
            onDocumentClick={onDocumentClick}
          />
        );
      default:
        break;
    }
  }

  return (
    <NumberedCode
      className="files-pane__code"
      content={viewer.content}
      language={codeLanguage(path)}
      label={path}
      litLine={litLine}
    />
  );
}

interface FileTabStripProps {
  tabs: readonly FileTab[];
  activePath: string | null;
  workspacePath: string | null;
  onActivate: (path: string) => void;
  onKeep: (path: string) => void;
  onClose: (path: string) => void;
  onReorder: (paths: string[]) => void;
  menuSections: (path: string, inTab: boolean) => PopoverMenuSection[];
}

/**
 * The open files, as tabs in the pane's title bar: the shared page strip, with a file's own
 * touches. The one reusable preview tab a single click opens into is drawn in italics, and a
 * double click on it keeps it. A file costs nothing to reopen, so Delete closes the focused tab.
 */
function FileTabStrip({
  tabs,
  activePath,
  workspacePath,
  onActivate,
  onKeep,
  onClose,
  onReorder,
  menuSections
}: FileTabStripProps) {
  const { t } = useI18n();
  return (
    <PageTabs
      tabs={tabs.map((tab) => {
        const name = basename(tab.path);
        return {
          id: tab.path,
          label: name,
          content: tab.preview ? <>{name}<span className="sr-only">{t("预览", "preview")}</span></> : undefined,
          title: workspacePath ? `${workspacePath}/${tab.path}` : tab.path,
          icon: <FileKindIcon path={tab.path} />,
          hint: parentRelativePath(tab.path) || undefined,
          className: tab.preview ? "files-pane__tab--preview" : undefined
        };
      })}
      activeId={activePath}
      ariaLabel={t("打开的文件", "Open files")}
      moreLabel={t("更多文件", "More files")}
      onSelect={onActivate}
      onClose={onClose}
      closeLabel={(tab) => t("关闭 {name}", "Close {name}", { name: tab.label })}
      closeOnDeleteKey
      onReorder={onReorder}
      onTabDoubleClick={onKeep}
      tabMenu={(path) => menuSections(path, true)}
      tabMenuLabel={(tab) => t("{name} 的文件操作", "Actions for {name}", { name: tab.label })}
    />
  );
}
