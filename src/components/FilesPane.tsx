import {
  ChevronLeft,
  ChevronRight,
  Code2,
  Copy,
  Database,
  Eye,
  File,
  FileArchive,
  FileCode2,
  FileImage,
  FilePlus2,
  FolderInput,
  FolderOpen,
  FolderPlus,
  FolderTree,
  HardDrive,
  Link2,
  ListX,
  LoaderCircle,
  Monitor,
  MoreHorizontal,
  MoreVertical,
  PanelLeftClose,
  PanelLeftOpen,
  PenLine,
  Pin,
  Plus,
  Presentation,
  RotateCw,
  Scroll,
  Search,
  Server,
  Sheet,
  SquareArrowOutUpRight,
  SquareX,
  Trash2,
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
import {
  browseAncestors,
  browseCreateDirectory,
  browseDeletePath,
  browseListDirectory,
  browseMachineLabel,
  browseName,
  browseOpenInFileManager,
  browseOpenWith,
  browseOpenWithChoices,
  browseOpenWithChooser,
  browsePath,
  browseProbePaths,
  browseReadFile,
  browseReadFileBytes,
  browseRenamePath,
  browseSearchFiles,
  formatAddress,
  isWindowsPath,
  joinBrowsePath,
  locationKey,
  machineKey,
  parentBrowsePath,
  parseAddress,
  relativeBrowsePath,
  sameBrowseMachine
} from "../lib/fileBrowser";
import type {
  BrowseEntryKind,
  BrowseListing,
  BrowseMachine,
  BrowseSearchMatch
} from "../lib/fileBrowser";
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
import { parseNotebook } from "../lib/notebook";
import { readStoredFlag, writeStoredFlag } from "../lib/paneSettings";
import { arrangeByIds } from "../lib/reorder";
import { listWslDistros } from "../lib/runtime";
import type { SidePaneId } from "../lib/sidePanes";
import type { SshMachineConfig } from "../types";
import { MarkdownCodeBlock, NumberedCode } from "./CodeBlock";
import { IconButton } from "./Common";
import { ContextMenu } from "./ContextMenu";
import type { ContextMenuAnchor, ContextMenuItem, ContextMenuSection } from "./ContextMenu";
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
import { PathText } from "./PathText";
import { PopoverMenu } from "./PopoverMenu";
import type { PopoverMenuSection } from "./PopoverMenu";
import { SidePane } from "./SidePane";
import "./FilePreview/FilePreview.css";
import "./FilesPane.css";

/** A file the pane has been asked to show from somewhere outside it. */
export interface FilesPaneOpenRequest {
  machine: BrowseMachine;
  /** Absolute on that machine. */
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
  /**
   * The file gets a page of its own, kept like one opened from a menu, instead
   * of taking over the preview page. A page already showing it is still reused.
   */
  newPage?: boolean;
}

/** One of the conversation's workspaces, as the pane offers it. */
export interface FilesPaneWorkspace {
  /** The number the conversation addresses it by, 1-based. */
  number: number;
  machine: BrowseMachine;
  /** Its root as the conversation records it: the worktree standing in for it when there is one. */
  path: string;
  /** Its short name, the directory's own. */
  label: string;
}

export interface FilesPaneProps {
  paneId: SidePaneId;
  /** The conversation's workspaces, in its numbering; the first is where a new page opens. */
  workspaces: readonly FilesPaneWorkspace[];
  /** The SSH catalog, for naming machines and reading addresses. */
  sshMachines: readonly SshMachineConfig[];
  /** Whether this computer is Windows: its paths print with backslashes, and it may have WSL. */
  hostWindows: boolean;
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

/** A place on a machine. */
interface Location {
  machine: BrowseMachine;
  path: string;
}

type DirectoryState =
  | { status: "loading" }
  | { status: "ready"; listing: BrowseListing }
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
  | { status: "ready"; root: string; matches: BrowseSearchMatch[]; truncated: boolean }
  | { status: "error"; message: string };

/**
 * One page of the pane: a directory in the tree beside a file, the way a tab of
 * a file manager is one window onto the disk.
 *
 * `preview` marks the page the transcript reuses: following a second path
 * replaces it rather than stacking another page, so reading through a reply
 * does not leave a row of tabs behind. Double-clicking the tab, or a file in its
 * tree, keeps it.
 */
interface FilePage {
  id: string;
  /** The directory the tree shows; null is the list of machines. */
  directory: Location | null;
  /** The file shown beside the tree. */
  file: Location | null;
  preview: boolean;
  /** The directories open in this page's tree, by location key. */
  expanded: ReadonlySet<string>;
}

type TreeRow =
  | {
      type: "entry";
      key: string;
      path: string;
      name: string;
      kind: BrowseEntryKind;
      link: boolean;
      depth: number;
      expanded: boolean;
    }
  | { type: "machine"; key: string; machine: BrowseMachine; name: string; detail: string | null }
  | { type: "message"; id: string; depth: number; text: string; failed: boolean };

type EntryRow = Extract<TreeRow, { type: "entry" }>;

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
/**
 * How soon a second click on a folder has to follow the first for the two to go into it. A single
 * click opens or closes the folder only once this has passed without one, so a double click never
 * opens the folder on its way in.
 */
const DOUBLE_CLICK_MS = 200;
/** How much longer an opening folder waits for its listing before it opens on a "reading" row. */
const EXPAND_WAIT_MS = 300;
/** How long the pointer rests on a folder before the folder is read ahead of a click. */
const PREFETCH_HOVER_MS = 80;
const SHOW_TREE_STORAGE_KEY = "mework.filesPane.showTree";

function describeError(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

let pageCounter = 0;
function nextPageId(): string {
  pageCounter += 1;
  return `page-${pageCounter}`;
}

function newPage(directory: Location | null, file: Location | null = null, preview = false, expanded: Iterable<string> = []): FilePage {
  return { id: nextPageId(), directory, file, preview, expanded: new Set(expanded) };
}

function sameLocation(left: Location | null, right: Location | null): boolean {
  if (!left || !right) return left === right;
  return sameBrowseMachine(left.machine, right.machine) && left.path === right.path;
}

/** Whether `path` is `root` or under it, on the same machine. */
function within(root: Location, location: Location): boolean {
  return sameBrowseMachine(root.machine, location.machine) && relativeBrowsePath(root.path, location.path) !== null;
}

/** `path` with `from` swapped for `to` when it is `from` or under it. */
function moved(path: string, from: string, to: string): string | null {
  const relative = relativeBrowsePath(from, path);
  if (relative === null) return null;
  return relative ? joinBrowsePath(to, relative) : to;
}

/** A path as its machine writes it: backslashes on Windows. */
function nativeSpelling(path: string): string {
  return isWindowsPath(path) ? path.replace(/\//g, "\\") : path;
}

/** The top of the filesystem a path is on: `/`, or its drive. */
function filesystemRoot(path: string): string {
  return isWindowsPath(path) ? `${path.slice(0, 2)}/` : "/";
}

function copyText(text: string): void {
  void navigator.clipboard?.writeText(text).catch(() => undefined);
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

function MachineIcon({ machine, className }: { machine: BrowseMachine; className?: string }) {
  if (!machine) return <Monitor size={14} aria-hidden="true" className={className} />;
  if (machine.kind === "wsl") return <HardDrive size={14} aria-hidden="true" className={className} />;
  return <Server size={14} aria-hidden="true" className={className} />;
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
 * Depth-first flattening of the loaded listings under a page's directory.
 *
 * The tree is one flat list carrying `aria-level` rather than nested groups: a
 * row's position is derived from the listings, so an expansion whose directory
 * has not answered yet still has somewhere to show that it is loading. The cache
 * is a `Map` because directory names are user data: one called `constructor`
 * must not read back as something inherited from `Object.prototype`.
 */
function buildRows(
  listings: ReadonlyMap<string, DirectoryState>,
  root: Location,
  expanded: ReadonlySet<string>,
  loadingText: string,
  emptyText: string
): TreeRow[] {
  const rows: TreeRow[] = [];
  const walk = (path: string, depth: number) => {
    const state = listings.get(locationKey(root.machine, path));
    if (!state || state.status === "loading") {
      rows.push({ type: "message", id: `loading:${path}`, depth, text: loadingText, failed: false });
      return;
    }
    if (state.status === "error") {
      rows.push({ type: "message", id: `error:${path}`, depth, text: state.message, failed: true });
      return;
    }
    if (!state.listing.entries.length) {
      rows.push({ type: "message", id: `empty:${path}`, depth, text: emptyText, failed: false });
      return;
    }
    for (const entry of state.listing.entries) {
      const key = locationKey(root.machine, entry.path);
      const isExpanded = entry.kind === "directory" && expanded.has(key);
      rows.push({
        type: "entry",
        key,
        path: entry.path,
        name: entry.name,
        kind: entry.kind,
        link: entry.link,
        depth,
        expanded: isExpanded
      });
      if (isExpanded) walk(entry.path, depth + 1);
    }
  };
  walk(root.path, 0);
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

/** Every reference in `source` that names a file rather than something on the web. */
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

/**
 * The file manager in the side pane.
 *
 * Each tab is a page: a directory in the tree column beside one open file, on
 * any machine the reader can reach — this computer, a WSL distribution, an SSH
 * machine — and anywhere on it, not only inside a workspace. The address bar
 * above the tree names the directory with its machine and takes a typed one;
 * going up from a machine's top leads to the list of machines. The workspaces
 * are one menu away, at the foot of the tree and behind the tab strip's `+`.
 *
 * Directories are listed one at a time, on expansion, and kept per machine and
 * path, so every page shares what any page has read. A request counter fences
 * every response, so a listing answered after a refresh is dropped instead of
 * contradicting the newer one.
 */
export function FilesPane({
  paneId,
  workspaces,
  sshMachines,
  hostWindows,
  active,
  openRequest = null,
  onOpenRequestHandled,
  expanded: paneExpanded,
  onToggleExpand,
  onPaneFocus,
  onPaneClose
}: FilesPaneProps) {
  const { t } = useI18n();
  const addressContext = useMemo(() => ({ sshMachines, hostWindows }), [hostWindows, sshMachines]);
  const thisComputer = t("本机", "This computer");
  const machineLabel = useCallback(
    (machine: BrowseMachine) => browseMachineLabel(machine, sshMachines, thisComputer),
    [sshMachines, thisComputer]
  );

  /** Each workspace's root as its machine spells it, once the host has said; the recorded path until then. */
  const [resolvedRoots, setResolvedRoots] = useState<ReadonlyMap<number, string>>(() => new Map());
  const workspaceRoots = useMemo(() => workspaces.map((workspace) => ({
    ...workspace,
    root: resolvedRoots.get(workspace.number) ?? browsePath(workspace.path)
  })), [resolvedRoots, workspaces]);
  const workspaceRootsRef = useRef(workspaceRoots);

  /** Where a new page opens: the first workspace, or this computer's home. */
  const homeLocation = useCallback((): Location => {
    const first = workspaceRootsRef.current[0];
    return first ? { machine: first.machine, path: first.root } : { machine: null, path: "~" };
  }, []);

  /** The workspace `location` is in, the innermost when they nest. */
  const containingWorkspace = useCallback((location: Location) => {
    let best: (typeof workspaceRoots)[number] | null = null;
    for (const workspace of workspaceRootsRef.current) {
      if (!within({ machine: workspace.machine, path: workspace.root }, location)) continue;
      if (!best || workspace.root.length > best.root.length) best = workspace;
    }
    return best;
  }, []);

  /** The page a file opens into when nothing else is said: its workspace's root, or its own folder. */
  const pageFor = useCallback((file: Location, preview: boolean): FilePage => {
    const workspace = containingWorkspace(file);
    const directory: Location = workspace
      ? { machine: file.machine, path: workspace.root }
      : { machine: file.machine, path: parentBrowsePath(file.path) ?? file.path };
    const expanded = browseAncestors(directory.path, file.path).map((path) => locationKey(file.machine, path));
    return newPage(directory, file, preview, expanded);
  }, [containingWorkspace]);

  const [pages, setPages] = useState<readonly FilePage[]>(() => {
    const first = workspaces[0];
    const directory: Location = first ? { machine: first.machine, path: browsePath(first.path) } : { machine: null, path: "~" };
    return [newPage(directory)];
  });
  /**
   * The page the pane opened with, until the reader does anything in it: the
   * transcript's first file takes it over instead of opening beside it.
   */
  const pristinePageRef = useRef<string | null>(pages[0]?.id ?? null);
  const isPristine = useCallback((page: FilePage): boolean => (
    page.id === pristinePageRef.current && !page.file && page.expanded.size === 0
  ), []);
  const [activePageId, setActivePageId] = useState<string | null>(() => pages[0]?.id ?? null);
  const [listings, setListings] = useState<ReadonlyMap<string, DirectoryState>>(() => new Map());
  const [viewers, setViewers] = useState<ReadonlyMap<string, ViewerState>>(() => new Map());
  const [media, setMedia] = useState<ReadonlyMap<string, MediaState>>(() => new Map());
  const [focusedPath, setFocusedPath] = useState<string | null>(null);
  const [revealNonce, setRevealNonce] = useState(0);
  const [revealedKey, setRevealedKey] = useState<string | null>(null);
  const [filter, setFilter] = useState("");
  const [debouncedFilter, setDebouncedFilter] = useState("");
  const [search, setSearch] = useState<SearchState>({ status: "idle" });
  const [showTree, setShowTree] = useState(() => readStoredFlag(SHOW_TREE_STORAGE_KEY, true));
  const [canFitTree, setCanFitTree] = useState(true);
  /**
   * The files being read as source rather than as what they are, by location
   * key. A set of the exceptions rather than a mode per page: every renderable
   * file opens rendered, and deciding to read one document's Markdown source
   * says nothing about the next one.
   */
  const [sourceKeys, setSourceKeys] = useState<ReadonlySet<string>>(() => new Set<string>());
  /** Where a request asked to land once its file is on screen, and the bump that makes a repeat ask again. */
  const [pendingJump, setPendingJump] = useState<
    { key: string; line: number | null; fragment: string | null; nonce: number } | null
  >(null);
  const jumpNonceRef = useRef(0);
  const [litLine, setLitLine] = useState<{ key: string; line: number } | null>(null);
  const [address, setAddress] = useState<{ text: string; error: string | null; busy: boolean } | null>(null);
  const [menu, setMenu] = useState<{ anchor: ContextMenuAnchor; sections: ContextMenuSection[] } | null>(null);
  /** A rename under way: in the tree's row, or in the tab of page `tab` when the tab's menu asked. */
  const [renaming, setRenaming] = useState<
    { machine: BrowseMachine; path: string; draft: string; busy: boolean; tab?: string } | null
  >(null);
  const [creating, setCreating] = useState<{ machine: BrowseMachine; parent: string; draft: string; busy: boolean } | null>(null);
  const [confirmDelete, setConfirmDelete] = useState<{ machine: BrowseMachine; path: string; busy: boolean } | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [wslDistros, setWslDistros] = useState<string[] | null>(null);

  const pagesRef = useRef(pages);
  const activePageIdRef = useRef(activePageId);
  const listingsRef = useRef(listings);
  const viewersRef = useRef(viewers);
  const generationRef = useRef(0);
  const directoryRequestRef = useRef(new Map<string, number>());
  /** Each directory read still out, by location key, settling once its answer is in the listings. */
  const directoryLoadsRef = useRef(new Map<string, Promise<void>>());
  /** A single click on a folder, waiting out the double click's window. */
  const pendingClickRef = useRef<{ key: string; timer: number; settle: () => void } | null>(null);
  const hoverPrefetchRef = useRef<number | null>(null);
  const fileRequestRef = useRef(new Map<string, number>());
  const mediaRequestRef = useRef(new Map<string, number>());
  const searchRequestRef = useRef(0);
  const openRequestRef = useRef<number | null>(null);
  const rowRefs = useRef(new Map<string, HTMLLIElement>());
  const bodyRef = useRef<HTMLDivElement>(null);
  const viewerRef = useRef<HTMLDivElement>(null);
  const filterRef = useRef<HTMLInputElement>(null);
  const addressRef = useRef<HTMLInputElement>(null);

  useLayoutEffect(() => {
    pagesRef.current = pages;
    activePageIdRef.current = activePageId;
    listingsRef.current = listings;
    viewersRef.current = viewers;
    workspaceRootsRef.current = workspaceRoots;
  });

  const activePage = pages.find((page) => page.id === activePageId) ?? null;
  const directory = activePage?.directory ?? null;
  const activeFile = activePage?.file ?? null;
  const activeFileKey = activeFile ? locationKey(activeFile.machine, activeFile.path) : null;

  /** A change the reader made to a page; the first page is no longer the one the pane opened with. */
  const updatePage = useCallback((id: string, update: (page: FilePage) => FilePage) => {
    if (pristinePageRef.current === id) pristinePageRef.current = null;
    setPages((current) => current.map((page) => (page.id === id ? update(page) : page)));
  }, []);

  // ---- reading ------------------------------------------------------------

  const loadDirectory = useCallback((machine: BrowseMachine, path: string) => {
    const key = locationKey(machine, path);
    const request = (directoryRequestRef.current.get(key) ?? 0) + 1;
    directoryRequestRef.current.set(key, request);
    const generation = generationRef.current;
    setListings((current) => {
      // Entries already on screen stay there while they are re-read, so a refresh
      // does not blank the tree the user is pointing at.
      if (current.get(key)?.status === "ready") return current;
      return new Map(current).set(key, { status: "loading" });
    });
    const read = browseListDirectory(machine, path).then((listing) => {
      if (generation !== generationRef.current || directoryRequestRef.current.get(key) !== request) return;
      setListings((current) => {
        const next = new Map(current).set(key, { status: "ready", listing });
        if (listing.path !== path) next.set(locationKey(machine, listing.path), { status: "ready", listing });
        return next;
      });
      // `~`, a trailing slash, a `..`: the page takes the spelling the machine gave.
      if (listing.path !== path) {
        setPages((current) => current.map((page) => (
          page.directory && sameBrowseMachine(page.directory.machine, machine) && page.directory.path === path
            ? { ...page, directory: { machine, path: listing.path } }
            : page
        )));
      }
    }).catch((error: unknown) => {
      if (generation !== generationRef.current || directoryRequestRef.current.get(key) !== request) return;
      setListings((current) => new Map(current).set(key, { status: "error", message: describeError(error) }));
    });
    directoryLoadsRef.current.set(key, read);
    void read.finally(() => {
      if (directoryLoadsRef.current.get(key) === read) directoryLoadsRef.current.delete(key);
    });
  }, []);

  const loadFile = useCallback((machine: BrowseMachine, path: string) => {
    // Two reads of the same file can be in flight at once — open, close, open
    // again — so the fence is the request's own identity, not the path it asked
    // for; otherwise the older answer would land on top of the newer one.
    const key = locationKey(machine, path);
    const request = (fileRequestRef.current.get(key) ?? 0) + 1;
    fileRequestRef.current.set(key, request);
    const generation = generationRef.current;
    setViewers((current) => (
      current.get(key)?.status === "ready" ? current : new Map(current).set(key, { status: "loading" })
    ));
    const settled = (next: ViewerState) => {
      if (generation !== generationRef.current || fileRequestRef.current.get(key) !== request) return;
      setViewers((current) => new Map(current).set(key, next));
    };
    browseReadFile(machine, path).then((file) => {
      settled({ status: "ready", content: file.content, binary: file.binary, truncated: file.truncated });
    }).catch((error: unknown) => {
      settled({ status: "error", message: describeError(error) });
    });
  }, []);

  /**
   * Reads a file as bytes: a picture, a PDF, a recording, a font.
   *
   * Fenced the same way as `loadFile`, and for the same reason: a document's
   * images are requested as the document is parsed, so several reads of the same
   * file can be outstanding while the reader clicks through a directory.
   */
  const loadMedia = useCallback((machine: BrowseMachine, path: string) => {
    const key = locationKey(machine, path);
    const request = (mediaRequestRef.current.get(key) ?? 0) + 1;
    mediaRequestRef.current.set(key, request);
    const generation = generationRef.current;
    setMedia((current) => (current.has(key) ? current : new Map(current).set(key, { status: "loading" })));
    const settled = (next: MediaState) => {
      if (generation !== generationRef.current || mediaRequestRef.current.get(key) !== request) return;
      setMedia((current) => new Map(current).set(key, next));
    };
    const mediaType = binaryMediaType(path);
    if (mediaType === null) {
      settled({ status: "error", message: t("这不是可显示的文件格式", "This is not a displayable file format") });
      return;
    }
    browseReadFileBytes(machine, path).then((file) => {
      settled(file.tooLarge ? { status: "tooLarge" } : { status: "ready", source: `data:${mediaType};base64,${file.data}` });
    }).catch((error: unknown) => {
      settled({ status: "error", message: describeError(error) });
    });
  }, [t]);

  /** Reads what the active page shows that nobody has read yet. */
  useEffect(() => {
    if (!active || !activePage) return;
    const { directory: root, expanded, file } = activePage;
    if (root) {
      const wanted = [root.path, ...[...expanded].map((key) => key.slice(key.indexOf("\u0000") + 1))];
      for (const path of wanted) {
        const key = locationKey(root.machine, path);
        // Only what this tree can show: a folder opened under another root waits for it.
        if (path !== root.path && (!expanded.has(key) || !relativeBrowsePath(root.path, path))) continue;
        if (!listingsRef.current.has(key)) loadDirectory(root.machine, path);
      }
    }
    if (file && needsText(file.path) && !viewersRef.current.has(locationKey(file.machine, file.path))) {
      loadFile(file.machine, file.path);
    }
  }, [active, activePage, loadDirectory, loadFile]);

  // The roots as the machines spell them: a remote workspace may be recorded as
  // `~/…`, and this computer's as the host canonicalized it.
  const workspaceSignature = workspaces.map((workspace) => locationKey(workspace.machine, workspace.path)).join("\n");
  // biome-ignore lint/correctness/useExhaustiveDependencies: the signature is what decides the answer; the array is rebuilt every render.
  useEffect(() => {
    if (!active || !workspaces.length || !hasBackendRuntime()) return;
    let cancelled = false;
    browseProbePaths(workspaces.map((workspace) => ({ machine: workspace.machine, path: workspace.path })))
      .then((results) => {
        if (cancelled) return;
        setResolvedRoots(new Map(results.flatMap((result, index) => (
          result.reached ? [[workspaces[index]!.number, result.path] as const] : []
        ))));
      })
      .catch(() => undefined);
    return () => { cancelled = true; };
  }, [active, workspaceSignature]);

  /**
   * Re-reads what the active page shows under a fresh generation. Directories
   * still waiting on the previous one are dropped — their answers are about to
   * be ignored, and a cached `loading` nobody is driving would show the next
   * expansion a reader that never finishes.
   */
  const refresh = useCallback(() => {
    generationRef.current += 1;
    setListings((current) => {
      const next = new Map(current);
      for (const [key, state] of next) if (state.status === "loading") next.delete(key);
      return next;
    });
    setMedia(new Map<string, MediaState>());
    const page = pagesRef.current.find((candidate) => candidate.id === activePageIdRef.current);
    if (!page) return;
    if (page.directory) {
      loadDirectory(page.directory.machine, page.directory.path);
      for (const key of page.expanded) {
        const separator = key.indexOf("\u0000");
        if (key.slice(0, separator) !== machineKey(page.directory.machine)) continue;
        loadDirectory(page.directory.machine, key.slice(separator + 1));
      }
    }
    if (page.file && needsText(page.file.path)) loadFile(page.file.machine, page.file.path);
  }, [loadDirectory, loadFile]);

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

  // ---- search -------------------------------------------------------------

  useEffect(() => {
    const timer = window.setTimeout(() => setDebouncedFilter(filter.trim()), FILTER_DEBOUNCE_MS);
    return () => window.clearTimeout(timer);
  }, [filter]);

  // A filter belongs to the directory it searched.
  const directoryKey = directory ? locationKey(directory.machine, directory.path) : null;
  // biome-ignore lint/correctness/useExhaustiveDependencies: a new directory, or another page, is the moment to clear it, though nothing here reads either.
  useEffect(() => {
    setFilter("");
    setDebouncedFilter("");
  }, [directoryKey, activePageId]);

  // biome-ignore lint/correctness/useExhaustiveDependencies: `directory` is read through its key; a page re-rendering is not a new search.
  useEffect(() => {
    if (!active) return;
    const query = debouncedFilter;
    const request = ++searchRequestRef.current;
    if (!query || !directory) {
      setSearch({ status: "idle" });
      return;
    }
    setSearch((current) => (current.status === "ready" ? current : { status: "loading" }));
    browseSearchFiles(directory.machine, directory.path, query).then((results) => {
      if (request !== searchRequestRef.current) return;
      setSearch({ status: "ready", root: results.root, matches: results.matches, truncated: results.truncated });
    }).catch((error: unknown) => {
      if (request !== searchRequestRef.current) return;
      setSearch({ status: "error", message: describeError(error) });
    });
  }, [active, debouncedFilter, directoryKey]);

  const searching = debouncedFilter.length > 0 && directory !== null;

  // ---- machines -----------------------------------------------------------

  const showingMachines = activePage !== null && directory === null;
  useEffect(() => {
    if (!showingMachines || !hostWindows || wslDistros !== null || !hasBackendRuntime()) return;
    void listWslDistros()
      .then((distros) => setWslDistros(distros.map((distro) => distro.name)))
      .catch(() => setWslDistros([]));
  }, [hostWindows, showingMachines, wslDistros]);

  const machineRows = useMemo((): TreeRow[] => {
    const machines: BrowseMachine[] = [
      null,
      ...(wslDistros ?? []).map((distro): BrowseMachine => ({ kind: "wsl", distro })),
      ...sshMachines.map((machine): BrowseMachine => ({ kind: "ssh", machineId: machine.id }))
    ];
    return machines.map((machine) => ({
      type: "machine",
      key: machineKey(machine),
      machine,
      name: machineLabel(machine),
      detail: machine?.kind === "ssh"
        ? sshMachines.find((entry) => entry.id === machine.machineId)?.host ?? null
        : null
    }));
  }, [machineLabel, sshMachines, wslDistros]);

  // ---- rows ---------------------------------------------------------------

  const rows = useMemo((): TreeRow[] => {
    if (!activePage) return [];
    if (!activePage.directory) return machineRows;
    return buildRows(
      listings,
      activePage.directory,
      activePage.expanded,
      t("正在读取…", "Loading…"),
      t("文件夹为空", "Folder is empty")
    );
  }, [activePage, listings, machineRows, t]);

  const treeRows = useMemo(
    () => rows.filter((row): row is EntryRow | Extract<TreeRow, { type: "machine" }> => row.type !== "message"),
    [rows]
  );
  const searchRows = search.status === "ready" ? search.matches : [];
  const searchRoot = search.status === "ready" ? search.root : directory?.path ?? "";
  const navigableKeys = searching
    ? searchRows.map((match) => locationKey(directory?.machine ?? null, joinBrowsePath(searchRoot, match.path)))
    : treeRows.map((row) => row.key);

  const registerRow = (key: string) => (element: HTMLLIElement | null) => {
    if (element) rowRefs.current.set(key, element);
    else rowRefs.current.delete(key);
  };

  const focusRow = (key: string | undefined) => {
    if (key === undefined) return;
    setFocusedPath(key);
    rowRefs.current.get(key)?.focus();
  };

  // ---- page actions -------------------------------------------------------

  const setShowTreePreference = useCallback((next: boolean) => {
    setShowTree(next);
    writeStoredFlag(SHOW_TREE_STORAGE_KEY, next);
  }, []);

  /** Shows `target` in the active page's tree; null is the list of machines. */
  const navigate = useCallback((target: Location | null) => {
    const id = activePageIdRef.current;
    if (id === null) {
      const page = newPage(target);
      setPages((current) => [...current, page]);
      setActivePageId(page.id);
      return;
    }
    updatePage(id, (page) => ({ ...page, directory: target }));
    setAddress(null);
    setFocusedPath(null);
  }, [updatePage]);

  const goUp = useCallback(() => {
    const page = pagesRef.current.find((candidate) => candidate.id === activePageIdRef.current);
    if (!page?.directory) return;
    const parent = parentBrowsePath(page.directory.path);
    const child = locationKey(page.directory.machine, page.directory.path);
    navigate(parent === null ? null : { machine: page.directory.machine, path: parent });
    // The directory just left is where the reader's eye goes back to.
    setFocusedPath(parent === null ? machineKey(page.directory.machine) : child);
    setRevealedKey(parent === null ? machineKey(page.directory.machine) : child);
    setRevealNonce((current) => current + 1);
  }, [navigate]);

  /** Opens or closes the folder `path` in the page `id`. */
  const setDirectoryOpen = useCallback((id: string, machine: BrowseMachine, path: string, open: boolean) => {
    const key = locationKey(machine, path);
    updatePage(id, (current) => {
      if (current.expanded.has(key) === open) return current;
      const next = new Set(current.expanded);
      if (open) next.add(key);
      else next.delete(key);
      return { ...current, expanded: next };
    });
    const cached = listingsRef.current.get(key);
    // A directory that failed is retried on the next expansion; a cached one is
    // not re-read, which is the whole point of keeping the listing.
    if (open && (!cached || cached.status === "error") && !directoryLoadsRef.current.has(key)) loadDirectory(machine, path);
  }, [loadDirectory, updatePage]);

  const toggleDirectory = useCallback((machine: BrowseMachine, path: string) => {
    const id = activePageIdRef.current;
    if (id === null) return;
    const page = pagesRef.current.find((candidate) => candidate.id === id);
    setDirectoryOpen(id, machine, path, !(page?.expanded.has(locationKey(machine, path)) ?? false));
  }, [setDirectoryOpen]);

  /**
   * Reads a folder before it is asked to open — the pointer resting on it, a press on it — so that
   * by the time a click has waited out the double click's window its listing is usually in.
   */
  const prefetchDirectory = useCallback((machine: BrowseMachine, path: string) => {
    const key = locationKey(machine, path);
    if (listingsRef.current.has(key) || directoryLoadsRef.current.has(key)) return;
    loadDirectory(machine, path);
  }, [loadDirectory]);

  /**
   * A single click on a folder, once the double click's window has passed: a closed folder opens
   * with its listing in place — waiting up to `EXPAND_WAIT_MS` for one still being read, so the
   * read is hidden rather than shown as a "reading" row that is replaced a moment later.
   */
  const settleDirectoryClick = useCallback((id: string, location: Location) => {
    const key = locationKey(location.machine, location.path);
    const page = pagesRef.current.find((candidate) => candidate.id === id);
    if (page?.expanded.has(key)) {
      setDirectoryOpen(id, location.machine, location.path, false);
      return;
    }
    prefetchDirectory(location.machine, location.path);
    let opened = false;
    const open = () => {
      if (opened) return;
      opened = true;
      window.clearTimeout(timer);
      setDirectoryOpen(id, location.machine, location.path, true);
    };
    const timer = window.setTimeout(open, EXPAND_WAIT_MS);
    const read = directoryLoadsRef.current.get(key);
    if (read) void read.then(open);
    else open();
  }, [prefetchDirectory, setDirectoryOpen]);

  useEffect(() => () => {
    if (pendingClickRef.current) window.clearTimeout(pendingClickRef.current.timer);
    if (hoverPrefetchRef.current !== null) window.clearTimeout(hoverPrefetchRef.current);
  }, []);

  /**
   * Shows `file` in the page `id`. `keep` is the difference between a glance and
   * a decision: a double click — or opening it from a menu — keeps the page.
   */
  const showFile = useCallback((id: string, file: Location, keep: boolean) => {
    updatePage(id, (page) => ({ ...page, file, preview: keep ? false : page.preview }));
    const key = locationKey(file.machine, file.path);
    if (needsText(file.path) && viewersRef.current.get(key)?.status !== "ready") loadFile(file.machine, file.path);
  }, [loadFile, updatePage]);

  /** Opens every directory on the way to `file` in the active page and scrolls its row into view. */
  const revealInTree = useCallback((target: Location) => {
    const id = activePageIdRef.current;
    const page = pagesRef.current.find((candidate) => candidate.id === id);
    if (!page || id === null) return;
    let root = page.directory;
    if (!root || !within(root, target) || sameLocation(root, target)) {
      const workspace = containingWorkspace(target);
      root = workspace && !sameLocation({ machine: target.machine, path: workspace.root }, target)
        ? { machine: target.machine, path: workspace.root }
        : { machine: target.machine, path: parentBrowsePath(target.path) ?? target.path };
    }
    const ancestors = browseAncestors(root.path, target.path).map((path) => locationKey(target.machine, path));
    updatePage(id, (current) => ({
      ...current,
      directory: root,
      expanded: new Set([...current.expanded, ...ancestors])
    }));
    for (const key of ancestors) {
      const cached = listingsRef.current.get(key);
      if (!cached || cached.status === "error") loadDirectory(target.machine, key.slice(key.indexOf("\u0000") + 1));
    }
    const key = locationKey(target.machine, target.path);
    setFilter("");
    setShowTreePreference(true);
    setFocusedPath(key);
    setRevealedKey(key);
    setRevealNonce((current) => current + 1);
  }, [containingWorkspace, loadDirectory, setShowTreePreference, updatePage]);

  // A reveal scrolls its row into view once, as soon as the row is drawn — its folder may still be
  // being read when the reveal is asked for. Only once: rows change all the time afterwards (a
  // folder opened, a folder read ahead under the pointer), and the tree must stay where the reader
  // scrolled it rather than be pulled back to the revealed row each time.
  const pendingRevealRef = useRef<string | null>(null);
  // biome-ignore lint/correctness/useExhaustiveDependencies: a new reveal is what arms the scroll; a focus moved by a click is not one.
  useEffect(() => {
    pendingRevealRef.current = revealNonce ? focusedPath : null;
  }, [revealNonce]);
  // biome-ignore lint/correctness/useExhaustiveDependencies: `rows` is what draws the row the reveal is waiting for.
  useEffect(() => {
    const key = pendingRevealRef.current;
    const row = key === null ? undefined : rowRefs.current.get(key);
    if (!row) return;
    pendingRevealRef.current = null;
    // The tree's own scroll only: `scrollIntoView` would also move the pane's chrome.
    scrollIntoContainer(row, "nearest");
  }, [revealNonce, rows]);

  // The row lights up so the eye can find where the tree jumped to, then lets go.
  useEffect(() => {
    if (revealedKey === null) return;
    const timer = window.setTimeout(() => setRevealedKey(null), REVEAL_FLASH_MS);
    return () => window.clearTimeout(timer);
  }, [revealNonce, revealedKey]);

  /** A new page, made the active one. */
  const openPage = useCallback((page: FilePage) => {
    setPages((current) => [...current, page]);
    setActivePageId(page.id);
    if (page.file && needsText(page.file.path)) {
      const key = locationKey(page.file.machine, page.file.path);
      if (viewersRef.current.get(key)?.status !== "ready") loadFile(page.file.machine, page.file.path);
    }
  }, [loadFile]);

  /** `target` in a page of its own: a directory as the page's tree, a file beside its folder. */
  const openInNewPage = useCallback((target: Location, kind: BrowseEntryKind | "machine") => {
    if (kind === "directory" || kind === "machine") {
      openPage(newPage(target));
      return;
    }
    const current = pagesRef.current.find((page) => page.id === activePageIdRef.current)?.directory ?? null;
    if (current && within(current, target)) {
      const expanded = browseAncestors(current.path, target.path).map((path) => locationKey(target.machine, path));
      openPage(newPage(current, target, false, expanded));
    } else {
      openPage(pageFor(target, false));
    }
  }, [openPage, pageFor]);

  const keepPage = useCallback((id: string) => {
    updatePage(id, (page) => (page.preview ? { ...page, preview: false } : page));
  }, [updatePage]);

  const closePages = useCallback((doomed: (page: FilePage) => boolean) => {
    const current = pagesRef.current;
    const next = current.filter((page) => !doomed(page));
    if (next.length === current.length) return;
    setPages(next);
    setActivePageId((currentActive) => {
      if (currentActive !== null && next.some((page) => page.id === currentActive)) return currentActive;
      if (!next.length) return null;
      // Closing the active page lands on its neighbour rather than the start of
      // the strip: the page next to the one being dismissed is the one being
      // worked through.
      const removedAt = current.findIndex((page) => page.id === currentActive);
      return next[Math.min(Math.max(removedAt, 0), next.length - 1)]!.id;
    });
  }, []);

  const reorderPages = useCallback((ids: string[]) => {
    setPages((current) => arrangeByIds(current, ids, (page) => page.id) ?? current);
  }, []);

  // ---- requests from outside ---------------------------------------------

  /**
   * Opens `file` and lands on a place in it once it is on screen.
   *
   * A line names a place in the source, so a file that has a rendered form shows
   * its source for it; a heading names a place in the rendered form, so it stays.
   */
  const jumpTo = useCallback((file: Location, line: number | null, fragment: string | null) => {
    const key = locationKey(file.machine, file.path);
    if (line !== null && hasSourceForm(file.path) && !readsBytes(file.path)) {
      setSourceKeys((current) => (current.has(key) ? current : new Set(current).add(key)));
    }
    if (line === null && fragment === null) return;
    jumpNonceRef.current += 1;
    setPendingJump({ key, line, fragment, nonce: jumpNonceRef.current });
  }, []);

  /**
   * Opens what the timeline asked for: on the page already showing it, or on the
   * preview page — reused, the way a single click in a tree is a glance — or on
   * a new preview page. A request for a page of its own skips the preview page
   * and opens a kept one.
   *
   * The nonce is what makes a second click on the same path ask again, and it is
   * kept in a ref so a request that arrived while the pane was closed is still
   * honoured on the mount that follows; the owner is told once it has been, so a
   * later mount does not replay it.
   */
  useEffect(() => {
    if (!openRequest || openRequestRef.current === openRequest.nonce) return;
    openRequestRef.current = openRequest.nonce;
    // A pane opened just to show this file shows the file: the tree folds away
    // for this visit without changing what the reader chose for the pane itself.
    if (openRequest.collapseTree) setShowTree(false);
    const file: Location = { machine: openRequest.machine, path: browsePath(openRequest.path) };
    const current = pagesRef.current;
    const showing = current.find((page) => sameLocation(page.file, file));
    if (showing) {
      setActivePageId(showing.id);
    } else {
      const preview = openRequest.newPage ? undefined : current.find((page) => page.preview);
      const fresh = pageFor(file, !openRequest.newPage);
      if (preview) {
        setPages((pages) => pages.map((page) => (page.id === preview.id ? { ...fresh, id: preview.id } : page)));
        setActivePageId(preview.id);
      } else if (current.length === 1 && isPristine(current[0]!)) {
        // The pane's untouched first page is where the file goes, not beside it.
        setPages([{ ...fresh, id: current[0]!.id }]);
        setActivePageId(current[0]!.id);
      } else {
        setPages((pages) => [...pages, fresh]);
        setActivePageId(fresh.id);
      }
      for (const key of fresh.expanded) {
        if (!listingsRef.current.has(key)) loadDirectory(file.machine, key.slice(key.indexOf("\u0000") + 1));
      }
    }
    const key = locationKey(file.machine, file.path);
    if (needsText(file.path) && viewersRef.current.get(key)?.status !== "ready") loadFile(file.machine, file.path);
    jumpTo(file, openRequest.line, null);
    onOpenRequestHandled?.(openRequest.nonce);
  }, [isPristine, jumpTo, loadDirectory, loadFile, onOpenRequestHandled, openRequest, pageFor]);

  // The jump waits for the file it is a place in: the rows do not exist until the
  // read lands, and scrolling before then would settle on the wrong offset.
  useEffect(() => {
    if (pendingJump === null || activeFileKey !== pendingJump.key || !activeFile) return;
    if (needsText(activeFile.path) && viewers.get(pendingJump.key)?.status !== "ready") return;
    if (pendingJump.line !== null) {
      const row = viewerRef.current?.querySelector(`[data-line="${pendingJump.line}"]`);
      if (row) scrollIntoContainer(row, "center");
      setLitLine(row ? { key: pendingJump.key, line: pendingJump.line } : null);
    } else if (pendingJump.fragment !== null) {
      scrollToFragment(viewerRef.current, pendingJump.fragment);
    }
    setPendingJump(null);
  }, [activeFile, activeFileKey, pendingJump, viewers]);

  useEffect(() => {
    if (litLine === null) return;
    const timer = window.setTimeout(() => setLitLine(null), REVEAL_FLASH_MS);
    return () => window.clearTimeout(timer);
  }, [litLine]);

  // ---- the open file ------------------------------------------------------

  /**
   * What a document's references are written against: its workspace when it is
   * in one — a README's `/docs/x.png` means the repository's — and the top of
   * its filesystem otherwise. The viewers work on paths relative to it.
   */
  const documentRoot = useCallback((file: Location): string => (
    containingWorkspace(file)?.root ?? filesystemRoot(file.path)
  ), [containingWorkspace]);

  const activeRoot = activeFile ? documentRoot(activeFile) : null;
  const activeRelative = activeFile && activeRoot !== null
    ? relativeBrowsePath(activeRoot, activeFile.path) ?? activeFile.path
    : null;
  const toAbsolute = useCallback((relative: string): string => (
    activeRoot === null ? relative : joinBrowsePath(activeRoot, relative)
  ), [activeRoot]);

  const activeViewer = activeFileKey === null ? null : viewers.get(activeFileKey) ?? null;
  const activeKind = activeFile === null ? null : fileViewerKind(activeFile.path);
  const activeSource = activeFileKey !== null && sourceKeys.has(activeFileKey);
  // Only a file with two readings offers the switch, and only once it is known
  // to have a text form — a PNG's "source" is the binary notice.
  const canReadSource = activeFile !== null
    && hasSourceForm(activeFile.path)
    && activeViewer?.status === "ready"
    && !activeViewer.binary;

  /**
   * The bytes the open files need: a picture, PDF, recording or font page needs
   * its own, and a rendered document — Markdown, or a notebook's text cells —
   * needs every picture it points at.
   *
   * Derived rather than accumulated so the set shrinks when a page closes; the
   * bytes are held as `data:` URLs, and the host will hand over eight megabytes
   * of one before it refuses.
   */
  const neededMedia = useMemo(() => {
    const needed = new Map<string, Location>();
    for (const page of pages) {
      const file = page.file;
      if (!file) continue;
      const key = locationKey(file.machine, file.path);
      if (sourceKeys.has(key)) continue;
      if (readsBytes(file.path)) {
        needed.set(key, file);
        continue;
      }
      const kind = fileViewerKind(file.path);
      if (kind !== "markdown" && kind !== "notebook") continue;
      const viewer = viewers.get(key);
      if (viewer?.status !== "ready" || viewer.binary) continue;
      const root = documentRoot(file);
      const relative = relativeBrowsePath(root, file.path);
      if (relative === null) continue;
      const markdown = kind === "notebook" ? notebookMarkdown(viewer.content) : viewer.content;
      for (const reference of documentImageReferences(markdown)) {
        const resolved = resolveDocumentReference(relative, reference.split("#")[0]!);
        if (resolved === null || imageMediaType(resolved) === null) continue;
        const absolute = joinBrowsePath(root, resolved);
        needed.set(locationKey(file.machine, absolute), { machine: file.machine, path: absolute });
      }
    }
    return needed;
  }, [documentRoot, pages, sourceKeys, viewers]);

  useEffect(() => {
    if (!active) return;
    for (const [key, location] of neededMedia) {
      if (!media.has(key)) loadMedia(location.machine, location.path);
    }
    if (![...media.keys()].some((key) => !neededMedia.has(key))) return;
    setMedia((current) => {
      const next = new Map([...current].filter(([key]) => neededMedia.has(key)));
      return next.size === current.size ? current : next;
    });
  }, [active, media, loadMedia, neededMedia]);

  /**
   * Turns a document's own image reference into bytes already read.
   *
   * Synchronous because it runs while the Markdown renders; anything not read
   * yet answers null and is drawn as its alternative text until the read that
   * `neededMedia` started lands and the document renders again.
   */
  const resolveImageSrc = useCallback((source: string) => {
    if (!activeFile || activeRelative === null) return source;
    if (/^[a-z][a-z0-9+.-]*:/i.test(source) || source.startsWith("//")) return source;
    const resolved = resolveDocumentReference(activeRelative, source.split("#")[0]!);
    if (resolved === null) return null;
    const picture = media.get(locationKey(activeFile.machine, toAbsolute(resolved)));
    return picture?.status === "ready" ? picture.source : null;
  }, [activeFile, activeRelative, media, toAbsolute]);

  /** Whether `path` is a directory as far as the listings already read can tell. */
  const isKnownDirectory = useCallback((machine: BrowseMachine, path: string): boolean => {
    const parent = parentBrowsePath(path);
    if (parent === null) return true;
    const listing = listingsRef.current.get(locationKey(machine, parent));
    if (listing?.status !== "ready") return false;
    return listing.listing.entries.some((entry) => entry.path === path && entry.kind === "directory");
  }, []);

  /** Opens `file` in the active page and lands where a link pointed. */
  const followTo = useCallback((file: Location, line: number | null, fragment: string | null) => {
    const id = activePageIdRef.current;
    if (id === null) return;
    showFile(id, file, false);
    jumpTo(file, line, fragment);
  }, [jumpTo, showFile]);

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
    if (!activeFile || activeRelative === null) return;
    const resolved = resolveDocumentReference(activeRelative, target.path);
    if (resolved === null) return;
    const location: Location = { machine: activeFile.machine, path: toAbsolute(resolved) };
    if (target.path.endsWith("/") || isKnownDirectory(location.machine, location.path)) {
      revealInTree(location);
      const id = activePageIdRef.current;
      const page = pagesRef.current.find((candidate) => candidate.id === id);
      if (!page?.expanded.has(locationKey(location.machine, location.path))) toggleDirectory(location.machine, location.path);
      return;
    }
    followTo(location, target.line, target.fragment);
  }, [activeFile, activeRelative, followTo, isKnownDirectory, revealInTree, toAbsolute, toggleDirectory]);

  /** What a page in an HTML preview reads: its stylesheets and its pictures, on the file's machine. */
  const htmlResources = useMemo<HtmlPreviewResources>(() => ({
    readText: async (path) => {
      if (!activeFile) return null;
      try {
        const file = await browseReadFile(activeFile.machine, toAbsolute(path));
        return file.binary ? null : file.content;
      } catch {
        return null;
      }
    },
    readImage: async (path) => {
      const mediaType = imageMediaType(path);
      if (mediaType === null || !activeFile) return null;
      try {
        const file = await browseReadFileBytes(activeFile.machine, toAbsolute(path));
        return file.tooLarge ? null : `data:${mediaType};base64,${file.data}`;
      } catch {
        return null;
      }
    }
  }), [activeFile, toAbsolute]);

  const onOpenFileFromPage = useCallback((path: string, line: number | null) => {
    if (!activeFile) return;
    followTo({ machine: activeFile.machine, path: toAbsolute(path) }, line, null);
  }, [activeFile, followTo, toAbsolute]);

  const revealActiveFile = useMemo(() => {
    if (!activeFile || activeFile.machine || !hasBackendRuntime()) return undefined;
    const path = nativeSpelling(activeFile.path);
    return () => {
      void browseOpenInFileManager(path).catch(() => undefined);
    };
  }, [activeFile]);

  // ---- file operations ----------------------------------------------------

  /** Re-reads the directory `path` is in, wherever it is cached. */
  const reloadParent = useCallback((machine: BrowseMachine, path: string) => {
    const parent = parentBrowsePath(path);
    if (parent !== null) loadDirectory(machine, parent);
  }, [loadDirectory]);

  const commitRename = useCallback(() => {
    const current = renaming;
    if (!current || current.busy) return;
    const name = current.draft.trim();
    if (!name || name === browseName(current.path)) {
      setRenaming(null);
      return;
    }
    setRenaming({ ...current, busy: true });
    browseRenamePath(current.machine, current.path, name).then((renamed) => {
      setRenaming(null);
      setActionError(null);
      const shift = (location: Location | null): Location | null => {
        if (!location || !sameBrowseMachine(location.machine, current.machine)) return location;
        const next = moved(location.path, current.path, renamed);
        return next === null ? location : { machine: location.machine, path: next };
      };
      setPages((pages) => pages.map((page) => ({
        ...page,
        directory: shift(page.directory),
        file: shift(page.file),
        expanded: new Set([...page.expanded].map((key) => {
          const separator = key.indexOf("\u0000");
          if (key.slice(0, separator) !== machineKey(current.machine)) return key;
          const next = moved(key.slice(separator + 1), current.path, renamed);
          return next === null ? key : locationKey(current.machine, next);
        }))
      })));
      reloadParent(current.machine, current.path);
      const key = locationKey(current.machine, renamed);
      setFocusedPath(key);
      setRevealedKey(key);
      setRevealNonce((nonce) => nonce + 1);
    }).catch((error: unknown) => {
      setRenaming((latest) => (latest ? { ...latest, busy: false } : latest));
      setActionError(describeError(error));
    });
  }, [reloadParent, renaming]);

  const commitCreate = useCallback(() => {
    const current = creating;
    if (!current || current.busy) return;
    const name = current.draft.trim();
    if (!name) {
      setCreating(null);
      return;
    }
    setCreating({ ...current, busy: true });
    browseCreateDirectory(current.machine, current.parent, name).then((made) => {
      setCreating(null);
      setActionError(null);
      loadDirectory(current.machine, current.parent);
      const key = locationKey(current.machine, made);
      setFocusedPath(key);
      setRevealedKey(key);
      setRevealNonce((nonce) => nonce + 1);
    }).catch((error: unknown) => {
      setCreating((latest) => (latest ? { ...latest, busy: false } : latest));
      setActionError(describeError(error));
    });
  }, [creating, loadDirectory]);

  const commitDelete = useCallback(() => {
    const current = confirmDelete;
    if (!current || current.busy) return;
    setConfirmDelete({ ...current, busy: true });
    browseDeletePath(current.machine, current.path).then(() => {
      setConfirmDelete(null);
      setActionError(null);
      const gone = (location: Location | null) => Boolean(
        location && sameBrowseMachine(location.machine, current.machine) && relativeBrowsePath(current.path, location.path) !== null
      );
      setPages((pages) => pages.map((page) => ({
        ...page,
        file: gone(page.file) ? null : page.file,
        directory: gone(page.directory)
          ? { machine: current.machine, path: parentBrowsePath(current.path) ?? current.path }
          : page.directory
      })));
      reloadParent(current.machine, current.path);
    }).catch((error: unknown) => {
      setConfirmDelete(null);
      setActionError(describeError(error));
    });
  }, [confirmDelete, reloadParent]);

  /** `path` relative to its workspace, or to the page's directory when it is in none. */
  const relativeFor = useCallback((location: Location): string | null => {
    const workspace = containingWorkspace(location);
    const base = workspace?.root ?? (directory && sameBrowseMachine(directory.machine, location.machine) ? directory.path : null);
    if (base === null) return null;
    const relative = relativeBrowsePath(base, location.path);
    return relative ? relative : null;
  }, [containingWorkspace, directory]);

  const openWithSubmenu = useCallback((location: Location): ContextMenuItem => {
    const local = location.machine === null;
    return {
      id: "open-with",
      label: t("用…打开", "Open With"),
      icon: <SquareArrowOutUpRight size={13} aria-hidden="true" />,
      disabled: !local || !hasBackendRuntime(),
      title: local ? undefined : t("文件在另一台机器上，本机的程序打不开它", "The file is on another machine, which this computer's programs cannot open"),
      loadChildren: async () => {
        const path = nativeSpelling(location.path);
        const choices = await browseOpenWithChoices(path);
        const sections: ContextMenuSection[] = [{
          id: "apps",
          items: choices.apps.map((app) => ({
            id: `app:${app.id}`,
            label: app.name,
            hint: app.default ? t("默认", "Default") : undefined,
            icon: app.icon
              ? <img className="context-menu__app-icon" src={app.icon} alt="" />
              : <SquareArrowOutUpRight size={13} aria-hidden="true" />,
            onSelect: () => {
              void browseOpenWith(path, app.id).catch((error: unknown) => setActionError(describeError(error)));
            }
          }))
        }];
        if (choices.chooser) {
          sections.push({
            id: "chooser",
            items: [{
              id: "chooser",
              label: t("选择其他应用…", "Choose Another App…"),
              onSelect: () => {
                void browseOpenWithChooser(path).catch((error: unknown) => setActionError(describeError(error)));
              }
            }]
          });
        }
        return sections;
      }
    };
  }, [t]);

  const revealItem = useCallback((location: Location): ContextMenuItem => ({
    id: "reveal",
    label: t("在文件管理器中打开", "Open in File Manager"),
    icon: <FolderInput size={13} aria-hidden="true" />,
    disabled: location.machine !== null || !hasBackendRuntime(),
    title: location.machine !== null
      ? t("文件管理器只能打开本机的文件", "The file manager only reaches this computer's files")
      : undefined,
    onSelect: () => {
      void browseOpenInFileManager(nativeSpelling(location.path)).catch((error: unknown) => setActionError(describeError(error)));
    }
  }), [t]);

  const copyItems = useCallback((location: Location): ContextMenuItem[] => {
    const relative = relativeFor(location);
    return [
      {
        id: "copy-absolute",
        label: t("复制绝对路径", "Copy Absolute Path"),
        icon: <Copy size={13} aria-hidden="true" />,
        onSelect: () => copyText(nativeSpelling(location.path))
      },
      {
        id: "copy-relative",
        label: t("复制相对路径", "Copy Relative Path"),
        icon: <Copy size={13} aria-hidden="true" />,
        disabled: relative === null,
        onSelect: () => {
          if (relative !== null) copyText(isWindowsPath(location.path) ? relative.replace(/\//g, "\\") : relative);
        }
      }
    ];
  }, [relativeFor, t]);

  /** The menu of one row of the tree, or of a search result. */
  const entryMenu = useCallback((location: Location, kind: BrowseEntryKind): ContextMenuSection[] => {
    const directoryRow = kind === "directory";
    return [
      {
        id: "open",
        items: [
          revealItem(location),
          {
            id: "open",
            label: directoryRow ? t("打开文件夹", "Open Folder") : t("打开文件", "Open File"),
            icon: directoryRow ? <FolderOpen size={13} aria-hidden="true" /> : <File size={13} aria-hidden="true" />,
            onSelect: () => {
              if (directoryRow) navigate(location);
              else {
                const id = activePageIdRef.current;
                if (id === null) openInNewPage(location, kind);
                else showFile(id, location, true);
              }
            }
          },
          openWithSubmenu(location),
          {
            id: "new-page",
            label: t("在新页面打开", "Open in New Page"),
            icon: <FilePlus2 size={13} aria-hidden="true" />,
            onSelect: () => openInNewPage(location, kind)
          }
        ]
      },
      {
        id: "edit",
        items: [
          {
            id: "rename",
            label: t("重命名…", "Rename…"),
            icon: <PenLine size={13} aria-hidden="true" />,
            onSelect: () => {
              setActionError(null);
              setFilter("");
              setRenaming({ machine: location.machine, path: location.path, draft: browseName(location.path), busy: false });
            }
          },
          {
            id: "delete",
            label: t("删除…", "Delete…"),
            icon: <Trash2 size={13} aria-hidden="true" />,
            danger: true,
            onSelect: () => {
              setActionError(null);
              setConfirmDelete({ machine: location.machine, path: location.path, busy: false });
            }
          }
        ]
      },
      { id: "copy", items: copyItems(location) }
    ];
  }, [copyItems, navigate, openInNewPage, openWithSubmenu, revealItem, showFile, t]);

  /** The menu of the tree's empty space: the directory the tree shows. */
  const directoryMenu = useCallback((location: Location): ContextMenuSection[] => [
    {
      id: "open",
      items: [
        revealItem(location),
        {
          id: "new-page",
          label: t("在新页面打开", "Open in New Page"),
          icon: <FilePlus2 size={13} aria-hidden="true" />,
          onSelect: () => openInNewPage(location, "directory")
        },
        {
          id: "new-folder",
          label: t("新建目录", "New Folder"),
          icon: <FolderPlus size={13} aria-hidden="true" />,
          onSelect: () => {
            setActionError(null);
            setFilter("");
            setCreating({ machine: location.machine, parent: location.path, draft: "", busy: false });
          }
        }
      ]
    },
    { id: "copy", items: copyItems(location) }
  ], [copyItems, openInNewPage, revealItem, t]);

  const machineMenu = useCallback((machine: BrowseMachine): ContextMenuSection[] => [{
    id: "open",
    items: [
      {
        id: "open",
        label: t("打开", "Open"),
        icon: <FolderOpen size={13} aria-hidden="true" />,
        onSelect: () => navigate({ machine, path: "~" })
      },
      {
        id: "new-page",
        label: t("在新页面打开", "Open in New Page"),
        icon: <FilePlus2 size={13} aria-hidden="true" />,
        onSelect: () => openInNewPage({ machine, path: "~" }, "machine")
      }
    ]
  }], [navigate, openInNewPage, t]);

  const openMenu = useCallback((anchor: ContextMenuAnchor, sections: ContextMenuSection[]) => {
    setMenu({ anchor, sections });
  }, []);

  // ---- address bar --------------------------------------------------------

  const addressText = directory ? formatAddress(directory.machine, directory.path, addressContext) : "";

  const startEditingAddress = () => {
    setAddress({ text: addressText, error: null, busy: false });
  };

  useEffect(() => {
    if (address === null || address.busy) return;
    const input = addressRef.current;
    if (input && document.activeElement !== input) {
      input.focus();
      input.select();
    }
  }, [address]);

  const submitAddress = useCallback(() => {
    const current = address;
    if (!current || current.busy) return;
    const parsed = parseAddress(current.text, addressContext, directory);
    if (parsed.kind === "machines") {
      navigate(null);
      return;
    }
    if (parsed.kind === "error") {
      setAddress({ ...current, error: t("没有登记名为 {name} 的 SSH 机器", "No SSH machine named {name} is registered", { name: parsed.name }) });
      return;
    }
    setAddress({ ...current, busy: true, error: null });
    // Patient: the first look at a machine may be what brings its link up.
    browseProbePaths([{ machine: parsed.machine, path: parsed.path }], { patient: true }).then(([result]) => {
      if (!result?.reached) {
        setAddress({ ...current, busy: false, error: t("这台机器现在连不上", "That machine cannot be reached right now") });
        return;
      }
      const location: Location = { machine: parsed.machine, path: result.path };
      if (result.kind === "directory") {
        navigate(location);
        return;
      }
      if (result.kind === "file") {
        setAddress(null);
        const id = activePageIdRef.current;
        const page = pagesRef.current.find((candidate) => candidate.id === id);
        if (!page || id === null) {
          openPage(pageFor(location, false));
          return;
        }
        if (!page.directory || !within(page.directory, location)) {
          navigate({ machine: location.machine, path: parentBrowsePath(location.path) ?? location.path });
        }
        showFile(id, location, true);
        return;
      }
      setAddress({ ...current, busy: false, error: t("找不到这个位置", "Nothing is there") });
    }).catch((error: unknown) => {
      setAddress({ ...current, busy: false, error: describeError(error) });
    });
  }, [address, addressContext, directory, navigate, openPage, pageFor, showFile, t]);

  // ---- workspace menus ----------------------------------------------------

  const workspaceSections = useCallback((pick: (location: Location) => void): PopoverMenuSection[] => {
    const groups: { key: string; machine: BrowseMachine; items: typeof workspaceRoots }[] = [];
    for (const workspace of workspaceRoots) {
      const key = machineKey(workspace.machine);
      const group = groups.find((candidate) => candidate.key === key);
      if (group) group.items.push(workspace);
      else groups.push({ key, machine: workspace.machine, items: [workspace] });
    }
    if (!groups.length) {
      return [{
        id: "home",
        items: [{
          id: "home",
          label: t("主目录", "Home"),
          icon: <Monitor size={13} aria-hidden="true" />,
          onSelect: () => pick({ machine: null, path: "~" })
        }]
      }];
    }
    return groups.map((group) => ({
      id: group.key,
      label: machineLabel(group.machine),
      items: group.items.map((workspace) => ({
        id: `workspace-${workspace.number}`,
        label: workspace.label,
        description: formatAddress(workspace.machine, workspace.root, addressContext),
        descriptionIsPath: true,
        hint: `${workspace.number}`,
        icon: <FolderOpen size={13} aria-hidden="true" />,
        onSelect: () => pick({ machine: workspace.machine, path: workspace.root })
      }))
    }));
  }, [addressContext, machineLabel, t, workspaceRoots]);

  // ---- layout -------------------------------------------------------------

  // The tree folds away on request; with a file open it also steps aside when
  // the pane is too narrow for the two to share it.
  const treeVisible = activePage !== null && (activeFile === null ? showTree : canFitTree && showTree);
  const treeIsColumn = treeVisible && canFitTree && activeFile !== null;
  const viewerVisible = activeFile !== null;
  const emptyStateVisible = !viewerVisible && (!treeVisible || canFitTree);
  const treeToggleDisabled = activePage === null || (activeFile !== null && !canFitTree);

  const paneMenuSections = useMemo<PopoverMenuSection[]>(() => [
    {
      id: "files-tree",
      items: [{
        id: "show-tree",
        label: t("显示文件树", "Show file tree"),
        checked: treeVisible,
        checkedRole: "checkbox",
        disabled: treeToggleDisabled,
        description: treeToggleDisabled && activePage ? t("面板太窄，放不下文件树", "The pane is too narrow for the tree") : undefined,
        onSelect: () => setShowTreePreference(!showTree)
      }]
    },
    {
      id: "files-refresh",
      items: [{ id: "refresh", label: t("刷新", "Refresh"), onSelect: refresh }]
    }
  ], [activePage, refresh, setShowTreePreference, showTree, t, treeToggleDisabled, treeVisible]);

  const treeToggleLabel = treeVisible
    ? t("收起文件目录", "Collapse file tree")
    : t("展开文件目录", "Expand file tree");

  const newPageLabel = t("新建页面", "New page");
  const addPageControl = workspaces.length > 1
    ? (
      <PopoverMenu
        triggerClassName="icon-button files-pane__add-page"
        trigger={<Plus size={14} aria-hidden="true" />}
        triggerLabel={newPageLabel}
        menuLabel={newPageLabel}
        align="end"
        dense
        sections={workspaceSections((location) => openPage(newPage(location)))}
      />
    )
    : (
      <IconButton
        className="files-pane__add-page"
        label={newPageLabel}
        onClick={() => openPage(newPage(homeLocation()))}
      >
        <Plus size={14} aria-hidden="true" />
      </IconButton>
    );

  const pageLabel = (page: FilePage): string => {
    if (page.file) return browseName(page.file.path);
    if (!page.directory) return t("所有机器", "All machines");
    if (page.directory.path === "/" || page.directory.path === "~") return machineLabel(page.directory.machine);
    return browseName(page.directory.path);
  };
  const pageTitle = (page: FilePage): string => {
    const shown = page.file ?? page.directory;
    return shown ? formatAddress(shown.machine, shown.path, addressContext) : t("所有机器", "All machines");
  };

  /**
   * A tab's right-click menu: the page's own actions, then those of the file or folder it shows —
   * its row's menu, less opening it, which the tab already is — then closing pages. Renaming
   * happens in the tab; a delete asks in the page, shown with its tree.
   */
  const tabMenu = (id: string): ContextMenuSection[] => {
    const page = pages.find((candidate) => candidate.id === id);
    if (!page) return [];
    const sections: ContextMenuSection[] = [];
    const pageItems: ContextMenuItem[] = [];
    if (page.preview) {
      pageItems.push({
        id: "keep-open",
        label: t("固定打开", "Keep open"),
        icon: <Pin size={13} aria-hidden="true" />,
        onSelect: () => keepPage(id)
      });
    }
    if (page.file) {
      const file = page.file;
      pageItems.push({
        id: "reveal-in-tree",
        label: t("在文件树中显示", "Reveal in file tree"),
        icon: <FolderTree size={13} aria-hidden="true" />,
        onSelect: () => {
          setActivePageId(id);
          window.setTimeout(() => revealInTree(file), 0);
        }
      });
    }
    if (pageItems.length) sections.push({ id: "page", items: pageItems });

    const target = page.file ?? page.directory;
    if (target) {
      const kind: BrowseEntryKind = page.file ? "file" : "directory";
      const openItems: ContextMenuItem[] = [
        revealItem(target),
        openWithSubmenu(target),
        {
          id: "new-page",
          label: t("在新页面打开", "Open in New Page"),
          icon: <FilePlus2 size={13} aria-hidden="true" />,
          onSelect: () => openInNewPage(target, kind)
        }
      ];
      if (kind === "directory") {
        openItems.push({
          id: "new-folder",
          label: t("新建目录", "New Folder"),
          icon: <FolderPlus size={13} aria-hidden="true" />,
          onSelect: () => {
            setActivePageId(id);
            setShowTreePreference(true);
            setActionError(null);
            setFilter("");
            setCreating({ machine: target.machine, parent: target.path, draft: "", busy: false });
          }
        });
      }
      sections.push({ id: "open", items: openItems });
      sections.push({
        id: "edit",
        items: [
          {
            id: "rename",
            label: t("重命名…", "Rename…"),
            icon: <PenLine size={13} aria-hidden="true" />,
            onSelect: () => {
              setActionError(null);
              setRenaming({ machine: target.machine, path: target.path, draft: browseName(target.path), busy: false, tab: id });
            }
          },
          {
            id: "delete",
            label: t("删除…", "Delete…"),
            icon: <Trash2 size={13} aria-hidden="true" />,
            danger: true,
            onSelect: () => {
              setActivePageId(id);
              setShowTreePreference(true);
              setActionError(null);
              setConfirmDelete({ machine: target.machine, path: target.path, busy: false });
            }
          }
        ]
      });
      sections.push({ id: "copy", items: copyItems(target) });
    }

    sections.push({
      id: "close",
      items: [
        {
          id: "close",
          label: t("关闭页面", "Close page"),
          icon: <X size={13} aria-hidden="true" />,
          onSelect: () => closePages((candidate) => candidate.id === id)
        },
        {
          id: "close-others",
          label: t("关闭其他页面", "Close other pages"),
          icon: <ListX size={13} aria-hidden="true" />,
          disabled: pages.length < 2,
          onSelect: () => closePages((candidate) => candidate.id !== id)
        },
        {
          id: "close-all",
          label: t("关闭所有页面", "Close all pages"),
          icon: <SquareX size={13} aria-hidden="true" />,
          onSelect: () => closePages(() => true)
        }
      ]
    });
    return sections;
  };

  // The title stays put with the drawer handle right after it, whatever the tab
  // strip beside them is doing, so folding the tree away is always one reach.
  const header = (
    <div className="files-pane__header">
      <span className="files-pane__pane-title" title={activePage ? pageTitle(activePage) : undefined}>{t("文件", "Files")}</span>
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
      <PageTabs
        tabs={pages.map((page) => {
          const label = pageLabel(page);
          return {
            id: page.id,
            label,
            content: page.preview ? <>{label}<span className="sr-only">{t("预览", "preview")}</span></> : undefined,
            title: pageTitle(page),
            icon: page.file
              ? <FileKindIcon path={page.file.path} />
              : page.directory
                ? <FolderOpen size={14} aria-hidden="true" />
                : <Monitor size={14} aria-hidden="true" />,
            hint: page.file && page.directory ? browseName(page.directory.path) : undefined,
            className: page.preview ? "files-pane__tab--preview" : undefined
          };
        })}
        activeId={activePageId}
        ariaLabel={t("文件页面", "File pages")}
        moreLabel={t("更多页面", "More pages")}
        onSelect={setActivePageId}
        onClose={(id) => closePages((page) => page.id === id)}
        closeLabel={(tab) => t("关闭 {name}", "Close {name}", { name: tab.label })}
        closeOnDeleteKey
        onReorder={reorderPages}
        onTabDoubleClick={keepPage}
        onTabContextMenu={(id, point) => openMenu(point, tabMenu(id))}
        renderEditor={(tab) => (renaming?.tab === tab.id ? renameInput("page-tab__editor") : null)}
        trailing={addPageControl}
      />
    </div>
  );

  // ---- tree rows ----------------------------------------------------------

  const onRowKeyDown = (
    event: KeyboardEvent<HTMLLIElement>,
    key: string,
    activate: (keep: boolean) => void,
    row: EntryRow | null
  ) => {
    const index = navigableKeys.indexOf(key);
    switch (event.key) {
      case "ArrowDown":
        focusRow(navigableKeys[index + 1]);
        break;
      case "ArrowUp":
        focusRow(navigableKeys[index - 1]);
        break;
      case "ArrowRight": {
        if (searching || !row || row.kind !== "directory" || !directory) return;
        if (!row.expanded) {
          toggleDirectory(directory.machine, row.path);
          break;
        }
        const next = rows[rows.findIndex((candidate) => candidate.type === "entry" && candidate.key === key) + 1];
        if (next?.type === "entry" && next.depth > row.depth) focusRow(next.key);
        break;
      }
      case "ArrowLeft": {
        if (searching || !row || !directory) return;
        if (row.kind === "directory" && row.expanded) toggleDirectory(directory.machine, row.path);
        else {
          const parent = parentBrowsePath(row.path);
          if (parent && parent !== directory.path) focusRow(locationKey(directory.machine, parent));
        }
        break;
      }
      case "Enter":
        activate(true);
        break;
      case "Backspace":
        if (searching) return;
        goUp();
        break;
      default:
        return;
    }
    event.preventDefault();
  };

  const rowMenuButton = (sections: () => ContextMenuSection[], label: string) => (
    <span className="files-pane__row-menu">
      <button
        type="button"
        className="icon-button files-pane__row-menu-trigger"
        aria-label={label}
        title={label}
        aria-haspopup="menu"
        onClick={(event) => {
          event.stopPropagation();
          const box = event.currentTarget.getBoundingClientRect();
          openMenu({ rect: { left: box.left, top: box.top, right: box.right, bottom: box.bottom }, align: "end" }, sections());
        }}
        onDoubleClick={(event) => event.stopPropagation()}
      >
        <MoreVertical size={13} aria-hidden="true" />
      </button>
    </span>
  );

  /** The field a rename is typed in: in a row of the tree, or in a tab. */
  const renameInput = (className: string) => renaming && (
    <input
      className={className}
      aria-label={t("新名称", "New name")}
      value={renaming.draft}
      disabled={renaming.busy}
      spellCheck={false}
      // biome-ignore lint/a11y/noAutofocus: the field exists only because Rename was just chosen.
      autoFocus
      onFocus={(event) => {
        // The name without its extension is what a rename usually changes.
        const dot = event.currentTarget.value.lastIndexOf(".");
        event.currentTarget.setSelectionRange(0, dot > 0 ? dot : event.currentTarget.value.length);
      }}
      onClick={(event) => event.stopPropagation()}
      onDoubleClick={(event) => event.stopPropagation()}
      onChange={(event) => setRenaming({ ...renaming, draft: event.target.value })}
      onBlur={commitRename}
      onKeyDown={(event) => {
        event.stopPropagation();
        if (event.key === "Enter") {
          event.preventDefault();
          commitRename();
        } else if (event.key === "Escape") {
          event.preventDefault();
          setRenaming(null);
        }
      }}
    />
  );
  const renameField = (path: string) => (renaming && !renaming.tab && renaming.path === path
    ? renameInput("files-pane__rename")
    : null);

  /**
   * A click on a folder. Two within `DOUBLE_CLICK_MS` go into it; one alone opens or closes it once
   * that window has passed. A click on another row meanwhile settles the first as a single click.
   */
  const clickDirectory = (key: string, location: Location) => {
    setFocusedPath(key);
    const pending = pendingClickRef.current;
    pendingClickRef.current = null;
    if (pending) window.clearTimeout(pending.timer);
    if (pending?.key === key) {
      navigate(location);
      return;
    }
    pending?.settle();
    const id = activePageIdRef.current;
    if (id === null) return;
    prefetchDirectory(location.machine, location.path);
    const settle = () => settleDirectoryClick(id, location);
    const timer = window.setTimeout(() => {
      pendingClickRef.current = null;
      settle();
    }, DOUBLE_CLICK_MS);
    pendingClickRef.current = { key, timer, settle };
  };

  const stopHoverPrefetch = () => {
    if (hoverPrefetchRef.current !== null) window.clearTimeout(hoverPrefetchRef.current);
    hoverPrefetchRef.current = null;
  };

  /** The pointer resting on a folder reads it ahead, as a link to a path is probed ahead. */
  const hoverDirectory = (location: Location) => {
    stopHoverPrefetch();
    hoverPrefetchRef.current = window.setTimeout(() => {
      hoverPrefetchRef.current = null;
      prefetchDirectory(location.machine, location.path);
    }, PREFETCH_HOVER_MS);
  };

  const renderEntryRow = (row: EntryRow, rowFocus: string | null) => {
    if (!directory) return null;
    const location: Location = { machine: directory.machine, path: row.path };
    const directoryRow = row.kind === "directory";
    const current = activeFileKey === row.key;
    const activate = (keep: boolean) => {
      setFocusedPath(row.key);
      if (directoryRow) {
        toggleDirectory(directory.machine, row.path);
        return;
      }
      if (row.kind === "other") return;
      const id = activePageIdRef.current;
      if (id !== null) showFile(id, location, keep);
    };
    const editor = renameField(row.path);
    return (
      <li
        key={row.key}
        ref={registerRow(row.key)}
        role="treeitem"
        aria-level={row.depth + 1}
        aria-expanded={directoryRow ? row.expanded : undefined}
        aria-current={current || undefined}
        tabIndex={row.key === rowFocus ? 0 : -1}
        className={`files-pane__row${current ? " files-pane__row--current" : ""}${row.key === revealedKey ? " files-pane__row--revealed" : ""}${row.kind === "other" ? " files-pane__row--other" : ""}`}
        style={{ paddingLeft: ROOT_INDENT + row.depth * INDENT_PER_LEVEL }}
        onClick={(event) => {
          if (editor) return;
          if (directoryRow) {
            clickDirectory(row.key, location);
            return;
          }
          activate(event.detail !== 1);
        }}
        onMouseDown={directoryRow
          ? (event) => {
            if (event.button === 0 && !editor) prefetchDirectory(location.machine, location.path);
          }
          : undefined}
        onMouseEnter={directoryRow ? () => hoverDirectory(location) : undefined}
        onMouseLeave={directoryRow ? stopHoverPrefetch : undefined}
        onContextMenu={(event) => {
          event.preventDefault();
          event.stopPropagation();
          setFocusedPath(row.key);
          openMenu({ x: event.clientX, y: event.clientY }, entryMenu(location, row.kind));
        }}
        onFocus={() => setFocusedPath(row.key)}
        onKeyDown={(event) => onRowKeyDown(event, row.key, activate, row)}
        title={formatAddress(directory.machine, row.path, addressContext)}
      >
        {directoryRow
          ? (
            <ChevronRight
              size={14}
              aria-hidden="true"
              className={`files-pane__chevron${row.expanded ? " files-pane__chevron--open" : ""}`}
            />
          )
          : <FileKindIcon path={row.path} className="files-pane__icon" />}
        {editor ?? <span className="files-pane__name">{row.name}</span>}
        {row.link && !editor && <Link2 size={11} aria-label={t("链接", "link")} className="files-pane__link" />}
        {!editor && rowMenuButton(() => entryMenu(location, row.kind), t("文件操作", "File actions"))}
      </li>
    );
  };

  const renderMachineRow = (row: Extract<TreeRow, { type: "machine" }>, rowFocus: string | null) => {
    const enter = () => navigate({ machine: row.machine, path: "~" });
    return (
      <li
        key={row.key}
        ref={registerRow(row.key)}
        role="treeitem"
        aria-level={1}
        tabIndex={row.key === rowFocus ? 0 : -1}
        className={`files-pane__row${row.key === revealedKey ? " files-pane__row--revealed" : ""}`}
        style={{ paddingLeft: ROOT_INDENT }}
        onClick={enter}
        onContextMenu={(event) => {
          event.preventDefault();
          event.stopPropagation();
          openMenu({ x: event.clientX, y: event.clientY }, machineMenu(row.machine));
        }}
        onFocus={() => setFocusedPath(row.key)}
        onKeyDown={(event) => onRowKeyDown(event, row.key, enter, null)}
        title={row.detail ?? row.name}
      >
        <MachineIcon machine={row.machine} className="files-pane__icon" />
        <span className="files-pane__name">{row.name}</span>
        {row.detail && <span className="files-pane__detail">{row.detail}</span>}
      </li>
    );
  };

  const rowFocus = focusedPath !== null && treeRows.some((row) => row.key === focusedPath)
    ? focusedPath
    : treeRows[0]?.key ?? null;
  const rootState = directory ? listings.get(locationKey(directory.machine, directory.path)) : undefined;

  const creatingRow = creating && directory && sameBrowseMachine(creating.machine, directory.machine) && creating.parent === directory.path
    ? (
      <li className="files-pane__row files-pane__row--editing" role="none" style={{ paddingLeft: ROOT_INDENT }}>
        <FolderPlus size={14} aria-hidden="true" className="files-pane__icon" />
        <input
          className="files-pane__rename"
          aria-label={t("新目录的名称", "Name of the new folder")}
          placeholder={t("新建目录", "New folder")}
          value={creating.draft}
          disabled={creating.busy}
          spellCheck={false}
          // biome-ignore lint/a11y/noAutofocus: the field exists only because New Folder was just chosen.
          autoFocus
          onChange={(event) => setCreating({ ...creating, draft: event.target.value })}
          onBlur={() => {
            if (!creating.draft.trim()) setCreating(null);
            else commitCreate();
          }}
          onKeyDown={(event) => {
            if (event.key === "Enter") {
              event.preventDefault();
              commitCreate();
            } else if (event.key === "Escape") {
              event.preventDefault();
              setCreating(null);
            }
          }}
        />
      </li>
    )
    : null;

  const tree = !activePage ? null : searching
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
        {directory && searchRows.map((match) => {
          const separator = match.path.lastIndexOf("/");
          const parentPath = separator > 0 ? match.path.slice(0, separator) : "";
          const location: Location = { machine: directory.machine, path: joinBrowsePath(searchRoot, match.path) };
          const key = locationKey(location.machine, location.path);
          const selected = key === activeFileKey;
          const activate = (keep: boolean) => {
            setFocusedPath(key);
            if (match.kind === "directory") {
              revealInTree(location);
              toggleDirectory(location.machine, location.path);
              return;
            }
            const id = activePageIdRef.current;
            if (id !== null) showFile(id, location, keep);
          };
          return (
            <li
              key={key}
              ref={registerRow(key)}
              role="option"
              aria-selected={selected}
              tabIndex={key === (focusedPath ?? navigableKeys[0]) ? 0 : -1}
              className={`files-pane__row${selected ? " files-pane__row--current" : ""}`}
              style={{ paddingLeft: ROOT_INDENT }}
              onClick={(event) => activate(event.detail !== 1)}
              onContextMenu={(event) => {
                event.preventDefault();
                event.stopPropagation();
                openMenu({ x: event.clientX, y: event.clientY }, entryMenu(location, match.kind));
              }}
              onFocus={() => setFocusedPath(key)}
              onKeyDown={(event) => onRowKeyDown(event, key, activate, null)}
              title={match.path}
            >
              {match.kind === "directory"
                ? <FolderOpen size={14} aria-hidden="true" className="files-pane__icon" />
                : <FileKindIcon path={match.path} className="files-pane__icon" />}
              <span className="files-pane__name">{highlight(match.name, match.positions, separator + 1)}</span>
              {parentPath && <span className="files-pane__detail">{highlight(parentPath, match.positions, 0)}</span>}
              {rowMenuButton(() => entryMenu(location, match.kind), t("文件操作", "File actions"))}
            </li>
          );
        })}
        {search.status === "ready" && search.truncated && (
          <li className="files-pane__row files-pane__row--message" role="none">
            <span className="files-pane__name">{t("结果过多，只显示了一部分", "Too many results; only some are shown")}</span>
          </li>
        )}
      </ul>
    )
    : rootState?.status === "error"
      ? <p className="files-pane__error" role="alert">{rootState.message}</p>
      : (
        <ul
          className="files-pane__tree"
          role="tree"
          aria-label={directory ? t("文件", "Files") : t("机器", "Machines")}
        >
          {creatingRow}
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
            return row.type === "machine" ? renderMachineRow(row, rowFocus) : renderEntryRow(row, rowFocus);
          })}
          {rootState?.status === "ready" && rootState.listing.truncated && (
            <li className="files-pane__row files-pane__row--message" role="none">
              <span className="files-pane__name">{t("目录里的项目过多，只列出了一部分", "This folder holds too many items; only some are listed")}</span>
            </li>
          )}
        </ul>
      );

  const confirmName = confirmDelete ? browseName(confirmDelete.path) : "";

  /**
   * What the last action said, and a delete asking first: at the foot of the tree, the column they
   * usually come from, or of whatever the page shows while its tree is put away — an action from a
   * tab's menu can come from a page whose tree is not on screen.
   */
  const notices = (
    <>
      {actionError && (
        <div className="files-pane__action-error" role="alert">
          <span>{actionError}</span>
          <IconButton label={t("关闭", "Dismiss")} onClick={() => setActionError(null)}>
            <X size={12} aria-hidden="true" />
          </IconButton>
        </div>
      )}

      {confirmDelete && (
        <div className="files-pane__confirm" role="alertdialog" aria-label={t("确认删除", "Confirm delete")}>
          <p>
            {confirmDelete.machine
              ? t(
                "永久删除“{name}”？那台机器没有回收站，删除后无法恢复。",
                "Delete “{name}” for good? That machine has no Trash to restore it from.",
                { name: confirmName }
              )
              : t("把“{name}”移到废纸篓？", "Move “{name}” to the Trash?", { name: confirmName })}
          </p>
          <div className="files-pane__confirm-actions">
            <button type="button" className="files-pane__confirm-cancel" disabled={confirmDelete.busy} onClick={() => setConfirmDelete(null)}>
              {t("取消", "Cancel")}
            </button>
            <button
              type="button"
              className="files-pane__confirm-delete"
              disabled={confirmDelete.busy}
              // biome-ignore lint/a11y/noAutofocus: the reader has just asked to delete; the answer is one key away.
              autoFocus
              onClick={commitDelete}
            >
              {confirmDelete.machine ? t("永久删除", "Delete") : t("移到废纸篓", "Move to Trash")}
            </button>
          </div>
        </div>
      )}
    </>
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
          <div className="files-pane__address">
            <IconButton
              className="files-pane__address-up"
              label={t("上一级", "Up one level")}
              disabled={!directory}
              onClick={goUp}
            >
              <ChevronLeft size={14} aria-hidden="true" />
            </IconButton>
            {address
              ? (
                <input
                  ref={addressRef}
                  className={`files-pane__address-input${address.error ? " files-pane__address-input--invalid" : ""}`}
                  aria-label={t("位置", "Location")}
                  aria-invalid={address.error ? true : undefined}
                  placeholder={t("路径，或 机器:路径", "A path, or machine:path")}
                  title={address.error ?? undefined}
                  value={address.text}
                  readOnly={address.busy}
                  spellCheck={false}
                  autoComplete="off"
                  onChange={(event) => setAddress({ text: event.target.value, error: null, busy: false })}
                  onBlur={() => {
                    if (!address.busy) setAddress(null);
                  }}
                  onKeyDown={(event) => {
                    if (event.key === "Enter") {
                      event.preventDefault();
                      submitAddress();
                    } else if (event.key === "Escape") {
                      event.preventDefault();
                      setAddress(null);
                    }
                  }}
                />
              )
              : (
                <button
                  type="button"
                  className="files-pane__address-display"
                  title={directory ? addressText : t("所有机器", "All machines")}
                  aria-label={t("位置：{address}，点按编辑", "Location: {address}; click to edit", {
                    address: directory ? addressText : t("所有机器", "All machines")
                  })}
                  onClick={startEditingAddress}
                >
                  {directory
                    ? <PathText className="files-pane__address-text" path={addressText} title={null} />
                    : <span className="files-pane__address-placeholder">{t("所有机器", "All machines")}</span>}
                </button>
              )}
            {address?.busy && <LoaderCircle size={13} className="spin files-pane__address-busy" aria-hidden="true" />}
          </div>
          {address?.error && <p className="files-pane__address-error" role="alert">{address.error}</p>}

          {/* biome-ignore lint/a11y/noStaticElementInteractions: the space between the rows is the directory's own menu; the keyboard reaches the same actions through the rows and the address bar. */}
          {/* biome-ignore lint/a11y/noNoninteractiveElementInteractions: as above. */}
          <div
            className="files-pane__tree-scroll"
            onContextMenu={(event) => {
              if (!directory) return;
              event.preventDefault();
              openMenu({ x: event.clientX, y: event.clientY }, directoryMenu(directory));
            }}
          >
            {tree}
          </div>

          {treeVisible && notices}

          <div className="files-pane__footer">
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
                disabled={!directory}
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
            <PopoverMenu
              triggerClassName="icon-button files-pane__workspaces"
              trigger={<MoreHorizontal size={14} aria-hidden="true" />}
              triggerLabel={t("工作区", "Workspaces")}
              menuLabel={t("工作区", "Workspaces")}
              align="end"
              placement="above"
              dense
              sections={workspaceSections((location) => navigate(location))}
            />
          </div>
        </div>

        {viewerVisible && activeFile !== null && activeRelative !== null && (
          <div
            className="files-pane__viewer"
            ref={viewerRef}
            // A path a document writes opens on the document's machine.
            data-mework-path-machine={machineKey(activeFile.machine)}
          >
            {/* The reference shell gives the open file its own breadcrumb row with the
                actions that belong to the file rather than to the pane. */}
            <div className="files-pane__viewer-bar">
              <span className="files-pane__viewer-path" title={formatAddress(activeFile.machine, activeFile.path, addressContext)}>
                <span className="files-pane__viewer-directory">
                  {activeRelative.slice(0, activeRelative.lastIndexOf("/") + 1)}
                </span>
                <span className="files-pane__viewer-name">{browseName(activeFile.path)}</span>
              </span>
              {canReadSource && activeFileKey !== null && (
                <IconButton
                  className="files-pane__viewer-button"
                  label={activeSource ? t("显示渲染结果", "Show rendered") : t("显示源码", "Show source")}
                  aria-pressed={activeSource}
                  onClick={() => setSourceKeys((current) => {
                    const next = new Set(current);
                    if (!next.delete(activeFileKey)) next.add(activeFileKey);
                    return next;
                  })}
                >
                  {activeSource ? <Eye size={13} aria-hidden="true" /> : <Code2 size={13} aria-hidden="true" />}
                </IconButton>
              )}
              <IconButton
                className="files-pane__viewer-button"
                label={t("重新读取文件", "Reload file")}
                onClick={() => {
                  if (needsText(activeFile.path)) loadFile(activeFile.machine, activeFile.path);
                  // The bytes behind a picture are cached separately, and a
                  // reload that left them alone would keep showing the old one.
                  setMedia((current) => {
                    if (!current.size) return current;
                    const next = new Map(current);
                    if (activeFileKey !== null) next.delete(activeFileKey);
                    for (const key of neededMedia.keys()) next.delete(key);
                    return next;
                  });
                }}
              >
                <RotateCw size={13} aria-hidden="true" />
              </IconButton>
              <IconButton
                className="files-pane__viewer-button"
                label={t("在文件树中显示", "Reveal in file tree")}
                onClick={() => revealInTree(activeFile)}
              >
                <FolderTree size={13} aria-hidden="true" />
              </IconButton>
              <IconButton
                className="files-pane__viewer-button"
                label={t("复制文件内容", "Copy file contents")}
                disabled={activeViewer?.status !== "ready" || activeViewer.binary}
                onClick={() => {
                  if (activeViewer?.status !== "ready") return;
                  copyText(activeViewer.content);
                }}
              >
                <Copy size={13} aria-hidden="true" />
              </IconButton>
              <IconButton
                className="files-pane__viewer-button"
                label={t("关闭文件", "Close file")}
                onClick={() => {
                  const id = activePageIdRef.current;
                  if (id !== null) updatePage(id, (page) => ({ ...page, file: null }));
                }}
              >
                <X size={13} aria-hidden="true" />
              </IconButton>
            </div>
            {activeViewer?.status === "ready" && activeViewer.truncated && needsText(activeFile.path) && (
              <p className="files-pane__notice">
                {t("文件过大，只显示了开头部分", "This file is too large; only its beginning is shown")}
              </p>
            )}
            <FileViewerBody
              path={activeRelative}
              kind={activeKind ?? "text"}
              source={activeSource}
              viewer={activeViewer}
              picture={activeFileKey === null ? null : media.get(activeFileKey) ?? null}
              litLine={litLine !== null && litLine.key === activeFileKey ? litLine.line : null}
              pathBaseDir={activeRoot}
              onDocumentClick={onDocumentClick}
              resolveImageSrc={resolveImageSrc}
              htmlResources={htmlResources}
              onOpenFile={onOpenFileFromPage}
              onReveal={revealActiveFile}
            />
            {!treeVisible && notices}
          </div>
        )}

        {emptyStateVisible && (
          <div className="files-pane__placeholder">
            <div className="files-pane__placeholder-icon" aria-hidden="true">
              <FolderOpen size={20} />
            </div>
            <h3>
              {activePage
                ? t("未选择文件", "No file selected")
                : t("没有打开的页面", "No pages open")}
            </h3>
            <p>
              {!activePage
                ? t("用标签栏的 + 打开一个工作区。", "Open a workspace with the + in the tab bar.")
                : treeVisible
                  ? t("在文件树中选择一个文件。", "Pick a file in the tree.")
                  : t("打开文件树来浏览。", "Show the file tree to browse.")}
            </p>
            {!treeVisible && !viewerVisible && (actionError || confirmDelete) && (
              <div className="files-pane__placeholder-notices">{notices}</div>
            )}
          </div>
        )}
      </div>
      {menu && (
        <ContextMenu
          anchor={menu.anchor}
          sections={menu.sections}
          label={t("文件操作", "File actions")}
          onClose={() => setMenu(null)}
        />
      )}
    </SidePane>
  );
}

interface FileViewerBodyProps {
  /** Relative to `pathBaseDir`: the viewers resolve a document's references against it. */
  path: string;
  kind: FileViewerKind;
  /** True while the reader has asked for the source of a file that has another form. */
  source: boolean;
  viewer: ViewerState | null;
  /** The file's bytes, for the kinds read that way. */
  picture: MediaState | null;
  /** The line to light, already narrowed to this file. */
  litLine: number | null;
  /** What the document's own paths are written against: its workspace, or its filesystem's top. */
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
  const name = browseName(path);

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
