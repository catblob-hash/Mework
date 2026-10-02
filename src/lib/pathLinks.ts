import { hasBackendRuntime, invoke } from "./backend";
import type { RunTarget } from "../types";

/**
 * The single entry point for a click on a file path the app detected.
 *
 * Paths are detected in model replies by `./remarkPathLinks`, which renders
 * them as `<button data-mework-path>`. Those buttons are generated at runtime
 * in unbounded numbers inside a component memoized to avoid re-parsing during
 * streaming, so activation is handled by one document-level capture listener
 * rather than a handler per node — the same reasoning as `./externalLinks`.
 *
 * A click is offered to the app first, through the handler registered by
 * `setPathOpenHandler`, so a file in the open workspace is shown in the file
 * pane. Only what the pane cannot reach is revealed in the system file manager,
 * which is the fallback this module started as.
 *
 * A `<button>` rather than an anchor keeps the two interceptors independent:
 * the external-link one inspects only `HTMLAnchorElement`, so it never sees
 * these nodes and listener order does not matter.
 */

/** Attribute carrying the path to reveal. */
const PATH_ATTRIBUTE = "data-mework-path";

/** Attribute carrying the `:line` the displayed reference named, when it had one. */
const LINE_ATTRIBUTE = "data-mework-path-line";

/** Attribute carrying the directory relative paths resolve against. */
const BASE_ATTRIBUTE = "data-mework-path-base";

/**
 * Attribute carrying the machine a surface's paths are on, as its key (`local`,
 * `wsl:<distro>`, `ssh:<id>`): set by a document the file pane shows, whose
 * paths are on the document's machine whatever the conversation's workspaces are.
 */
const MACHINE_ATTRIBUTE = "data-mework-path-machine";

/** The machine a key names, or undefined when it names none. */
export function machineFromKey(key: string | null | undefined): RunTarget | null | undefined {
  if (key === "local") return null;
  if (key?.startsWith("wsl:") && key.length > 4) return { kind: "wsl", distro: key.slice(4) };
  if (key?.startsWith("ssh:") && key.length > 4) return { kind: "ssh", machineId: key.slice(4) };
  return undefined;
}

/** A click on a detected path, before anything has decided what to do with it. */
export interface PathOpenRequest {
  /** Exactly what the link carries: absolute, or relative to `baseDir`. */
  path: string;
  /** The directory relative paths resolve against, or null when the surface knew none. */
  baseDir: string | null;
  /** The line the reference named, or null when it named only a file. */
  line: number | null;
  /**
   * The workspace number a tool call named, when the path was written against a
   * workspace other than the surface's own; `baseDir` does not apply to it then.
   */
  workspace?: number | null;
  /**
   * Set by a turn's list of changed files: a file whose change Git tracks opens
   * in the review pane on its diff, and only anything else in the file pane.
   */
  review?: boolean;
  /**
   * Set by a tool row's file name: the file opens in a page of its own in the
   * file pane rather than in the preview page a passing click reuses.
   */
  newPage?: boolean;
  /**
   * The machine the path is on, when the surface that showed it says so — a
   * document in the file pane. Absent, the path is somewhere among the
   * conversation's workspaces, and the handler works out where.
   */
  machine?: RunTarget | null;
  /** The link's box, for a menu that opens where the link is. */
  anchor?: { left: number; top: number; right: number; bottom: number };
}

/**
 * Decides a click in the app instead of in the file manager.
 *
 * The app registers one of these to open the clicked file in its own file pane,
 * which is what a reader almost always means; revealing the file on disk stays
 * as the answer for everything the pane cannot show — a path outside the
 * workspace, or a click while no conversation owns a workspace at all. Returning
 * false is how the handler says so, and the reveal below runs unchanged.
 */
export type PathOpenHandler = (request: PathOpenRequest) => boolean;

let openHandler: PathOpenHandler | null = null;

/** Installs the in-app handler and returns a function that removes exactly it. */
export function setPathOpenHandler(handler: PathOpenHandler): () => void {
  openHandler = handler;
  return () => {
    if (openHandler === handler) openHandler = null;
  };
}

/**
 * Told when the pointer reaches a path, before anything is clicked: a path that
 * could be in several places can be looked up on every machine while the reader
 * is still deciding, so the click finds the answer waiting.
 */
export type PathPrefetchHandler = (request: PathOpenRequest) => void;

let prefetchHandler: PathPrefetchHandler | null = null;

/** Installs the hover handler and returns a function that removes exactly it. */
export function setPathPrefetchHandler(handler: PathPrefetchHandler): () => void {
  prefetchHandler = handler;
  return () => {
    if (prefetchHandler === handler) prefetchHandler = null;
  };
}

/**
 * Tells the app the pointer has rested on a path a surface opens through
 * `openPath` itself rather than through a `data-mework-path` element.
 */
export function prefetchPath(request: PathOpenRequest): void {
  prefetchHandler?.(request);
}

/** Matches the host's own rule so a relative path is recognized identically. */
function isAbsolutePath(value: string): boolean {
  return value.startsWith("/") || /^[A-Za-z]:[\\/]/.test(value);
}

/**
 * Shows a path in the system file manager.
 *
 * The host validates the path and resolves it against `baseDir`; browser
 * preview and jsdom have no file manager to reach.
 */
export async function revealPath(path: string, baseDir: string | null): Promise<void> {
  if (!hasBackendRuntime()) throw new Error("浏览器预览无法打开文件位置");
  await invoke<void>("reveal_path_in_file_manager", { path, baseDir });
}

/**
 * Returns whether this click should be handled here.
 *
 * Modifier clicks and right-clicks keep platform semantics instead of being
 * converted into a reveal.
 */
function activationClick(event: MouseEvent): boolean {
  if (event.defaultPrevented) return false;
  if (event.button !== 0 && event.button !== 1) return false;
  return !(event.ctrlKey || event.metaKey || event.shiftKey || event.altKey);
}

function pathTarget(event: Event): HTMLElement | null {
  const composed = typeof event.composedPath === "function" ? event.composedPath() : [];
  const nodes = composed.length ? composed : [event.target];
  for (const node of nodes) {
    if (!(node instanceof HTMLElement)) continue;
    const owner = node.closest<HTMLElement>(`[${PATH_ATTRIBUTE}]`);
    if (owner) return owner;
  }
  return null;
}

/**
 * Opens a path the way a click on it does, and says whether anything took it.
 *
 * The app gets first refusal: a file it can show belongs in one of its panes,
 * and only what the panes cannot reach falls through to the file manager.
 */
export function openPath(
  request: PathOpenRequest,
  reveal: (path: string, baseDir: string | null) => Promise<void> = revealPath
): boolean {
  if (openHandler?.(request)) return true;
  // A relative path without a working directory cannot be resolved by the
  // host either, so do not spend an IPC round trip on it.
  if (!request.baseDir && !isAbsolutePath(request.path)) return false;
  void reveal(request.path, request.baseDir).catch((error: unknown) => {
    console.error("打开文件位置失败", error);
  });
  return true;
}

/** What a detected path's button says about itself and where it sits. */
function requestFor(owner: HTMLElement): PathOpenRequest | null {
  const path = owner.getAttribute(PATH_ATTRIBUTE);
  if (!path) return null;
  const baseDir = owner.closest<HTMLElement>(`[${BASE_ATTRIBUTE}]`)?.getAttribute(BASE_ATTRIBUTE) ?? null;
  const declaredLine = Number.parseInt(owner.getAttribute(LINE_ATTRIBUTE) ?? "", 10);
  const line = Number.isSafeInteger(declaredLine) && declaredLine > 0 ? declaredLine : null;
  const machine = machineFromKey(owner.closest<HTMLElement>(`[${MACHINE_ATTRIBUTE}]`)?.getAttribute(MACHINE_ATTRIBUTE));
  const box = owner.getBoundingClientRect();
  return {
    path,
    baseDir,
    line,
    ...(machine !== undefined ? { machine } : {}),
    anchor: { left: box.left, top: box.top, right: box.right, bottom: box.bottom }
  };
}

/**
 * How long the pointer rests on a path before it is looked up: long enough to
 * pass over links on the way somewhere else, a fraction of how long the reader
 * takes to click.
 */
const PREFETCH_DWELL_MS = 60;

/** Installs the document interceptor and returns its cleanup function. */
export function installPathLinkInterceptor(
  documentRef: Document = document,
  reveal: (path: string, baseDir: string | null) => Promise<void> = revealPath
): () => void {
  const handle = (event: MouseEvent) => {
    if (!activationClick(event)) return;
    const owner = pathTarget(event);
    if (!owner) return;
    const request = requestFor(owner);
    if (!request || !openPath(request, reveal)) return;
    event.preventDefault();
    event.stopPropagation();
  };
  // Once per link the pointer rests on, not per movement inside it, and not for
  // every link a pointer sweeping across a reply passes over.
  let hovered: HTMLElement | null = null;
  let dwell: ReturnType<typeof setTimeout> | null = null;
  const hover = (event: MouseEvent) => {
    const owner = pathTarget(event);
    if (owner === hovered) return;
    hovered = owner;
    if (dwell !== null) clearTimeout(dwell);
    dwell = null;
    if (!owner || !prefetchHandler) return;
    dwell = setTimeout(() => {
      dwell = null;
      if (hovered !== owner || !prefetchHandler) return;
      const request = requestFor(owner);
      if (request) prefetchHandler(request);
    }, PREFETCH_DWELL_MS);
  };
  documentRef.addEventListener("click", handle, true);
  documentRef.addEventListener("auxclick", handle, true);
  documentRef.addEventListener("mouseover", hover, true);
  return () => {
    if (dwell !== null) clearTimeout(dwell);
    documentRef.removeEventListener("click", handle, true);
    documentRef.removeEventListener("auxclick", handle, true);
    documentRef.removeEventListener("mouseover", hover, true);
  };
}
