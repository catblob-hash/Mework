import { hasBackendRuntime, invoke } from "./backend";

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

/** A click on a detected path, before anything has decided what to do with it. */
export interface PathOpenRequest {
  /** Exactly what the link carries: absolute, or relative to `baseDir`. */
  path: string;
  /** The directory relative paths resolve against, or null when the surface knew none. */
  baseDir: string | null;
  /** The line the reference named, or null when it named only a file. */
  line: number | null;
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

/** Installs the document interceptor and returns its cleanup function. */
export function installPathLinkInterceptor(
  documentRef: Document = document,
  reveal: (path: string, baseDir: string | null) => Promise<void> = revealPath
): () => void {
  const handle = (event: MouseEvent) => {
    if (!activationClick(event)) return;
    const owner = pathTarget(event);
    if (!owner) return;
    const path = owner.getAttribute(PATH_ATTRIBUTE);
    if (!path) return;
    const baseDir = owner.closest<HTMLElement>(`[${BASE_ATTRIBUTE}]`)?.getAttribute(BASE_ATTRIBUTE) ?? null;
    const declaredLine = Number.parseInt(owner.getAttribute(LINE_ATTRIBUTE) ?? "", 10);
    const line = Number.isSafeInteger(declaredLine) && declaredLine > 0 ? declaredLine : null;
    // The app gets first refusal: a file it can show belongs in the file pane,
    // and only what the pane cannot reach falls through to the file manager.
    if (openHandler?.({ path, baseDir, line })) {
      event.preventDefault();
      event.stopPropagation();
      return;
    }
    // A relative path without a working directory cannot be resolved by the
    // host either, so do not spend an IPC round trip on it.
    if (!baseDir && !isAbsolutePath(path)) return;
    event.preventDefault();
    event.stopPropagation();
    void reveal(path, baseDir).catch((error: unknown) => {
      console.error("打开文件位置失败", error);
    });
  };
  documentRef.addEventListener("click", handle, true);
  documentRef.addEventListener("auxclick", handle, true);
  return () => {
    documentRef.removeEventListener("click", handle, true);
    documentRef.removeEventListener("auxclick", handle, true);
  };
}
