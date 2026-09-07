import { hasBackendRuntime, invoke } from "./backend";

/**
 * The single exit path for revealing a file path in the system file manager.
 *
 * Paths are detected in model replies by `./remarkPathLinks`, which renders
 * them as `<button data-mework-path>`. Those buttons are generated at runtime
 * in unbounded numbers inside a component memoized to avoid re-parsing during
 * streaming, so activation is handled by one document-level capture listener
 * rather than a handler per node — the same reasoning as `./externalLinks`.
 *
 * A `<button>` rather than an anchor keeps the two interceptors independent:
 * the external-link one inspects only `HTMLAnchorElement`, so it never sees
 * these nodes and listener order does not matter.
 */

/** Attribute carrying the path to reveal. */
const PATH_ATTRIBUTE = "data-mework-path";

/** Attribute carrying the directory relative paths resolve against. */
const BASE_ATTRIBUTE = "data-mework-path-base";

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
