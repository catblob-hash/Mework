import { hasBackendRuntime, invoke } from "./backend";
import type { GitTarget } from "./git";

/** Mirror of the host's `WorkspaceEntryKind`. A symlink is reported as itself, never resolved. */
export type WorkspaceEntryKind = "directory" | "file" | "symlink" | "other";

export interface WorkspaceEntry {
  name: string;
  kind: WorkspaceEntryKind;
  /** Byte size for regular files; null when the platform did not report one. */
  size: number | null;
}

export interface WorkspaceDirectoryListing {
  /** Workspace-relative path of the listed directory, `/` separated; `""` is the root. */
  path: string;
  entries: WorkspaceEntry[];
}

export interface WorkspaceFileContent {
  path: string;
  /** Lossily decoded text, empty for a binary file. */
  content: string;
  /** True when the host stopped at its read cap and the tail is missing. */
  truncated: boolean;
  size: number;
  binary: boolean;
}

export interface WorkspaceFileBytes {
  path: string;
  /** Standard base64 of the whole file; empty when `tooLarge`. */
  data: string;
  size: number;
  /** True when the file is past the preview cap, so no partial bytes were sent. */
  tooLarge: boolean;
}

export interface WorkspaceSearchMatch {
  name: string;
  /** Workspace-relative path, `/` separated. */
  path: string;
  kind: "directory" | "file";
  /** Character offsets into `path` that the query matched, ascending. */
  positions: number[];
  score: number;
}

export interface WorkspaceSearchResults {
  query: string;
  matches: WorkspaceSearchMatch[];
  /** True when the host stopped walking at a budget rather than seeing the whole tree. */
  truncated: boolean;
}

/** Host-side cap; asking for more is clamped there, so the UI asks for exactly this. */
const WORKSPACE_SEARCH_LIMIT = 200;

function requireWorkspaceRuntime(): void {
  if (!hasBackendRuntime()) throw new Error("文件浏览仅可在连接 Rust 后端时使用");
}

/**
 * Path of a child inside `parent`.
 *
 * Workspace paths are always `/` separated and the root is the empty string, so
 * a child of the root must not gain a leading separator.
 */
export function joinRelativePath(parent: string, name: string): string {
  const base = parent.replace(/\/+$/, "");
  const child = name.replace(/^\/+/, "");
  if (!base) return child;
  if (!child) return base;
  return `${base}/${child}`;
}

/** The containing directory, or null when the path already is the workspace root. */
export function parentRelativePath(path: string): string | null {
  const trimmed = path.replace(/\/+$/, "");
  if (!trimmed) return null;
  const separator = trimmed.lastIndexOf("/");
  return separator < 0 ? "" : trimmed.slice(0, separator);
}

/** Last segment of a workspace path; the root has no name of its own. */
export function basename(path: string): string {
  const trimmed = path.replace(/\/+$/, "");
  if (!trimmed) return "";
  const separator = trimmed.lastIndexOf("/");
  return separator < 0 ? trimmed : trimmed.slice(separator + 1);
}

/**
 * A path in the one shape the comparison below can work with: `/` separated, no
 * repeated or trailing separator, and no Windows extended-length prefix.
 *
 * The prefix matters more than it looks. The host answers with the workspace
 * directory as `\\?\C:\…`, because that is what it canonicalizes to, while a
 * path a model writes never carries one. Comparing the two as they arrive makes
 * every absolute path look like it belongs to a different disk.
 *
 * A network location is refused rather than normalized: the host refuses it too,
 * and `//server/share` collapsed into a rooted path would name something else.
 */
function normalizePath(value: string): string | null {
  const trimmed = value.trim();
  if (!trimmed) return null;
  const extended = /^[\\/]{2}\?[\\/]/.test(trimmed);
  if (extended && /^[\\/]{2}\?[\\/]UNC[\\/]/i.test(trimmed)) return null;
  if (!extended && /^[\\/]{2}/.test(trimmed)) return null;
  const separated = (extended ? trimmed.slice(4) : trimmed)
    .replace(/\\/g, "/")
    .replace(/\/{2,}/g, "/");
  return separated.replace(/(.)\/+$/, "$1") || null;
}

function isAbsolute(path: string): boolean {
  return path.startsWith("/") || /^[A-Za-z]:\//.test(path);
}

/**
 * Applies `.` and `..` segments, or returns null when the path climbs past its
 * own start. A path that escapes the workspace is not a workspace path, and
 * silently clamping it at the root would address the wrong file.
 */
function collapse(segments: readonly string[]): string[] | null {
  const result: string[] = [];
  for (const segment of segments) {
    if (segment === "" || segment === ".") continue;
    if (segment !== "..") {
      result.push(segment);
      continue;
    }
    if (!result.length) return null;
    result.pop();
  }
  return result;
}

/**
 * Windows compares paths without case; POSIX does not.
 *
 * The test is the shape of the root rather than the running platform: a
 * renderer under browser-dev on Windows still addresses a Windows checkout, and
 * a drive letter is the only thing that says so.
 */
function fold(path: string, windows: boolean): string {
  return windows ? path.toLowerCase() : path;
}

/**
 * Where a path a model wrote lives inside the open workspace, or null when it
 * lives somewhere the file pane cannot reach.
 *
 * `baseDir` is the working directory the surface that showed the path was
 * reading against; `workspaceRoot` is the checkout the file pane browses. The
 * two are usually the same directory, and the whole point of resolving through
 * both is the case where they are not: a path is only workspace-relative once
 * it has been made absolute against the directory it was written for.
 *
 * A relative path with no `baseDir` is taken to be workspace-relative already,
 * which is what a model writing `src/App.tsx` means, and is the only reading
 * available before a workspace is known.
 */
export function workspaceRelativePath(
  rawPath: string,
  baseDir: string | null,
  workspaceRoot: string | null
): string | null {
  const path = normalizePath(rawPath);
  if (path === null) return null;
  const root = workspaceRoot === null ? null : normalizePath(workspaceRoot);
  const windows = root !== null ? /^[A-Za-z]:\//.test(root) : /^[A-Za-z]:\//.test(path);

  if (!isAbsolute(path)) {
    const base = baseDir === null ? null : normalizePath(baseDir);
    // Without a working directory, or with one that already is the checkout,
    // the path needs no resolution — it is relative to the root either way.
    if (base === null || root === null || fold(base, windows) === fold(root, windows)) {
      const collapsed = collapse(path.split("/"));
      return collapsed?.length ? collapsed.join("/") : null;
    }
    return workspaceRelativePath(`${base}/${path}`, null, root);
  }

  if (root === null) return null;
  const rootSegments = collapse(root.split("/"));
  const pathSegments = collapse(path.split("/"));
  if (rootSegments === null || pathSegments === null) return null;
  if (pathSegments.length <= rootSegments.length) return null;
  for (let index = 0; index < rootSegments.length; index += 1) {
    if (fold(pathSegments[index], windows) !== fold(rootSegments[index], windows)) return null;
  }
  return pathSegments.slice(rootSegments.length).join("/");
}

/**
 * The host's order: directories first, then case-insensitive by name.
 *
 * The comparison must stop exactly where `workspace_files::list_directory`'s
 * does. Case folding alone is not a total order — `README` and `readme` can
 * coexist on a case-sensitive filesystem — and both sorts are stable, so a
 * case-folded tie keeps the order the host already gave; adding a tie-breaker
 * here would reorder rows the host had settled. Locale collation is deliberately
 * avoided for the same reason: this reproduces the host's order, not the user's.
 */
export function sortEntries(entries: readonly WorkspaceEntry[]): WorkspaceEntry[] {
  return [...entries].sort((left, right) => {
    const leftDirectory = left.kind === "directory";
    const rightDirectory = right.kind === "directory";
    if (leftDirectory !== rightDirectory) return leftDirectory ? -1 : 1;
    const leftFolded = left.name.toLowerCase();
    const rightFolded = right.name.toLowerCase();
    if (leftFolded === rightFolded) return 0;
    return leftFolded < rightFolded ? -1 : 1;
  });
}

export async function listWorkspaceDirectory(
  target: GitTarget,
  relativePath: string
): Promise<WorkspaceDirectoryListing> {
  requireWorkspaceRuntime();
  return invoke<WorkspaceDirectoryListing>("list_workspace_directory", { target, relativePath });
}

export async function readWorkspaceFile(
  target: GitTarget,
  relativePath: string
): Promise<WorkspaceFileContent> {
  requireWorkspaceRuntime();
  return invoke<WorkspaceFileContent>("read_workspace_file", { target, relativePath });
}

/**
 * The whole file, base64-encoded.
 *
 * Only the image viewer wants this: text goes through `readWorkspaceFile`, which
 * truncates at a cap and says so, while half an image decodes to nothing at all.
 */
export async function readWorkspaceFileBytes(
  target: GitTarget,
  relativePath: string
): Promise<WorkspaceFileBytes> {
  requireWorkspaceRuntime();
  return invoke<WorkspaceFileBytes>("read_workspace_file_bytes", { target, relativePath });
}

/**
 * Fuzzy file-name search over the whole checkout.
 *
 * The tree lists one directory per call, so a filter that only narrowed what was
 * already expanded would answer a different question than the one being asked.
 * The host walks under its own budget and says when it stopped early.
 */
export async function searchWorkspaceFiles(
  target: GitTarget,
  query: string,
  limit: number = WORKSPACE_SEARCH_LIMIT
): Promise<WorkspaceSearchResults> {
  requireWorkspaceRuntime();
  return invoke<WorkspaceSearchResults>("search_workspace_files", { target, query, limit });
}
