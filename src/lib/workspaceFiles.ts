/**
 * Where a path a model wrote lives inside a checkout.
 *
 * The review pane addresses a file by its path inside the checkout it shows;
 * this turns a path from the transcript — absolute, or written against the
 * conversation's working directory — into that one, or says it is elsewhere.
 * The file pane itself browses absolute paths on any machine (`fileBrowser.ts`).
 */

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
