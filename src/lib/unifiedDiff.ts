/**
 * A parser for `git diff` text output.
 *
 * The host hands back the raw output of `git diff --unified=3 …` — one string for
 * the whole scope — and the viewer needs it as files, hunks and numbered lines.
 * Nothing here throws: a patch that cannot be understood still names a file, and
 * says so with `incomplete`, because a diff the reader cannot see is a worse
 * failure than a diff the reader can see is partial.
 */

export type DiffFileStatus = "added" | "deleted" | "modified" | "renamed" | "copied" | "typeChanged";

export type DiffLineKind = "context" | "addition" | "deletion";

export interface DiffLine {
  kind: DiffLineKind;
  /** 1-based line number in the old file; null on an addition. */
  oldLine: number | null;
  /** 1-based line number in the new file; null on a deletion. */
  newLine: number | null;
  /** The line's content with the leading `+`/`-`/space marker removed. */
  text: string;
  /** This line was the last in its file and carried no trailing newline. */
  noNewline: boolean;
}

export interface DiffHunk {
  oldStart: number;
  oldLines: number;
  newStart: number;
  newLines: number;
  /** Whatever followed the closing `@@` — usually the enclosing function. */
  heading: string;
  lines: DiffLine[];
}

export interface DiffFile {
  /** Repository-relative path of the new side; for a deletion, of the old side. */
  path: string;
  /** The old path, set only when it differs from `path` (rename or copy). */
  oldPath: string | null;
  status: DiffFileStatus;
  binary: boolean;
  /** True when only the file mode changed and there is no content hunk. */
  modeChangeOnly: boolean;
  oldMode: string | null;
  newMode: string | null;
  hunks: DiffHunk[];
  additions: number;
  deletions: number;
  /** True when the patch text ran out mid-file, e.g. the host hit its output cap. */
  incomplete: boolean;
}

export interface ParseUnifiedDiffOptions {
  /**
   * Path to trust over the one in the `diff --git` header.
   *
   * `git diff --no-index` names an untracked file by its absolute host path, which
   * is not what anything else in the app calls that file. A single-file request
   * already knows the repository-relative path it asked for, so it can say so.
   */
  fallbackPath?: string;
}

const HUNK_HEADER = /^@@+ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@+ ?(.*)$/;
const COMBINED_HUNK_HEADER = /^@@@/;

/** Whether a mode pair changes the kind of object rather than its permissions. */
function isTypeChange(oldMode: string | null, newMode: string | null): boolean {
  if (!oldMode || !newMode) return false;
  // The high four octal digits are the object type: 1000 regular, 1200 symlink,
  // 0400 directory, 1600 gitlink. Permission bits below them are not a type change.
  return oldMode.slice(0, oldMode.length - 3) !== newMode.slice(0, newMode.length - 3);
}

/**
 * Unquotes a path the way `git` writes one in a `diff --git` header.
 *
 * Git wraps a path containing unusual bytes in double quotes with C escapes, and
 * writes non-ASCII as octal escapes of the individual UTF-8 bytes. Decoding the
 * escapes one character at a time would therefore produce mojibake, so octal runs
 * are collected as bytes and decoded together.
 */
export function unquoteGitPath(raw: string): string {
  const trimmed = raw.trim();
  if (!trimmed.startsWith("\"") || !trimmed.endsWith("\"") || trimmed.length < 2) return trimmed;
  const body = trimmed.slice(1, -1);
  const bytes: number[] = [];
  const encoder = new TextEncoder();
  let index = 0;
  while (index < body.length) {
    const character = body[index];
    if (character !== "\\") {
      for (const byte of encoder.encode(character)) bytes.push(byte);
      index += 1;
      continue;
    }
    const escaped = body[index + 1];
    index += 2;
    switch (escaped) {
      case "\\": bytes.push(0x5c); break;
      case "\"": bytes.push(0x22); break;
      case "a": bytes.push(0x07); break;
      case "b": bytes.push(0x08); break;
      case "f": bytes.push(0x0c); break;
      case "n": bytes.push(0x0a); break;
      case "r": bytes.push(0x0d); break;
      case "t": bytes.push(0x09); break;
      case "v": bytes.push(0x0b); break;
      default: {
        if (escaped !== undefined && escaped >= "0" && escaped <= "7") {
          let octal = escaped;
          while (octal.length < 3 && body[index] >= "0" && body[index] <= "7") {
            octal += body[index];
            index += 1;
          }
          bytes.push(Number.parseInt(octal, 8) & 0xff);
          break;
        }
        if (escaped !== undefined) for (const byte of encoder.encode(escaped)) bytes.push(byte);
      }
    }
  }
  try {
    return new TextDecoder("utf-8").decode(new Uint8Array(bytes));
  } catch {
    return body;
  }
}

/** Strips one `a/` or `b/` prefix from an already-unquoted header path. */
function stripSidePrefix(path: string): string {
  if (path === "/dev/null") return path;
  if (path.startsWith("a/") || path.startsWith("b/")) return path.slice(2);
  return path;
}

/**
 * Splits `diff --git <old> <new>` into its two paths.
 *
 * The separator is a space and both paths may contain spaces, so the split is
 * only unambiguous when at least one side is quoted. When neither is, git's own
 * convention is relied on — the two sides differ by their `a/`…`b/` prefixes —
 * and the `---`/`+++` lines below correct any remaining guess.
 */
function splitHeaderPaths(rest: string): [string, string] {
  if (rest.startsWith("\"")) {
    const end = findClosingQuote(rest, 0);
    if (end > 0) {
      return [rest.slice(0, end + 1), rest.slice(end + 1).trim()];
    }
  }
  const marker = rest.indexOf(" b/");
  if (marker >= 0) return [rest.slice(0, marker), rest.slice(marker + 1)];
  const quoted = rest.indexOf(" \"b/");
  if (quoted >= 0) return [rest.slice(0, quoted), rest.slice(quoted + 1)];
  const half = Math.floor(rest.length / 2);
  return [rest.slice(0, half).trim(), rest.slice(half).trim()];
}

function findClosingQuote(text: string, from: number): number {
  let index = from + 1;
  while (index < text.length) {
    if (text[index] === "\\") {
      index += 2;
      continue;
    }
    if (text[index] === "\"") return index;
    index += 1;
  }
  return -1;
}

function headerPath(raw: string): string {
  return stripSidePrefix(unquoteGitPath(raw));
}

/** The path on a `---`/`+++` line, with git's trailing timestamp column removed. */
function markerPath(raw: string): string {
  const value = raw.startsWith("\"")
    ? raw.slice(0, findClosingQuote(raw, 0) + 1 || raw.length)
    : raw.split("\t")[0];
  return headerPath(value);
}

interface FileDraft {
  headerOld: string | null;
  headerNew: string | null;
  markerOld: string | null;
  markerNew: string | null;
  renameFrom: string | null;
  renameTo: string | null;
  copyFrom: string | null;
  copyTo: string | null;
  newFileMode: string | null;
  deletedFileMode: string | null;
  oldMode: string | null;
  newMode: string | null;
  binary: boolean;
  combined: boolean;
  hunks: DiffHunk[];
  additions: number;
  deletions: number;
  incomplete: boolean;
}

function emptyDraft(): FileDraft {
  return {
    headerOld: null,
    headerNew: null,
    markerOld: null,
    markerNew: null,
    renameFrom: null,
    renameTo: null,
    copyFrom: null,
    copyTo: null,
    newFileMode: null,
    deletedFileMode: null,
    oldMode: null,
    newMode: null,
    binary: false,
    combined: false,
    hunks: [],
    additions: 0,
    deletions: 0,
    incomplete: false
  };
}

function settle(draft: FileDraft): DiffFile {
  const added = draft.newFileMode !== null || draft.markerOld === "/dev/null";
  const deleted = draft.deletedFileMode !== null || draft.markerNew === "/dev/null";
  const renamed = draft.renameFrom !== null || draft.renameTo !== null;
  const copied = draft.copyFrom !== null || draft.copyTo !== null;

  const newSide = deleted
    ? null
    : (draft.markerNew !== "/dev/null" ? draft.markerNew : null)
      ?? draft.renameTo
      ?? draft.copyTo
      ?? draft.headerNew;
  const oldSide = added
    ? null
    : (draft.markerOld !== "/dev/null" ? draft.markerOld : null)
      ?? draft.renameFrom
      ?? draft.copyFrom
      ?? draft.headerOld;

  const path = newSide ?? oldSide ?? "";
  const previous = oldSide !== null && oldSide !== path ? oldSide : null;

  const status: DiffFileStatus = added
    ? "added"
    : deleted
      ? "deleted"
      : renamed
        ? "renamed"
        : copied
          ? "copied"
          : isTypeChange(draft.oldMode, draft.newMode)
            ? "typeChanged"
            : "modified";

  return {
    path,
    oldPath: previous,
    status,
    binary: draft.binary,
    modeChangeOnly: draft.hunks.length === 0
      && !draft.binary
      && draft.oldMode !== null
      && draft.newMode !== null
      && !added
      && !deleted,
    oldMode: draft.oldMode ?? draft.deletedFileMode,
    newMode: draft.newMode ?? draft.newFileMode,
    hunks: draft.hunks,
    additions: draft.additions,
    deletions: draft.deletions,
    incomplete: draft.incomplete || draft.combined
  };
}

/**
 * Reads a patch into files.
 *
 * The scan is line-oriented and single pass. Structural lines are matched after a
 * trailing `\r` is dropped, but a hunk body line keeps whatever it has: a change
 * from CRLF to LF is a real diff, and stripping it here would render the two
 * sides identical.
 */
export function parseUnifiedDiff(patch: string, options?: ParseUnifiedDiffOptions): DiffFile[] {
  if (!patch) return [];
  const rawLines = patch.split("\n");
  const files: DiffFile[] = [];
  let draft: FileDraft | null = null;
  let hunk: DiffHunk | null = null;
  let oldCursor = 0;
  let newCursor = 0;
  let oldRemaining = 0;
  let newRemaining = 0;

  const closeHunk = () => {
    if (!draft || !hunk) return;
    if (oldRemaining > 0 || newRemaining > 0) draft.incomplete = true;
    draft.hunks.push(hunk);
    hunk = null;
  };

  const closeFile = () => {
    closeHunk();
    if (draft) files.push(settle(draft));
    draft = null;
  };

  for (let index = 0; index < rawLines.length; index += 1) {
    const raw = rawLines[index];
    // The final split element after a trailing newline is an artefact, not a line.
    if (raw === "" && index === rawLines.length - 1) continue;
    const line = raw.endsWith("\r") ? raw.slice(0, -1) : raw;

    if (line.startsWith("diff --git ")) {
      closeFile();
      draft = emptyDraft();
      const [left, right] = splitHeaderPaths(line.slice("diff --git ".length));
      draft.headerOld = headerPath(left);
      draft.headerNew = headerPath(right);
      continue;
    }
    if (!draft) continue;

    if (line.startsWith("\\")) {
      // "\ No newline at end of file" describes the line before it, which may be the
      // last line of a hunk this parser has already closed on its declared counts.
      const open = hunk?.lines;
      const closed = draft.hunks[draft.hunks.length - 1]?.lines;
      const target = open?.length ? open : closed;
      const previous = target?.[target.length - 1];
      if (previous) previous.noNewline = true;
      continue;
    }

    if (hunk !== null) {
      const marker = line[0];
      if (marker === "+" || marker === "-" || marker === " " || line === "") {
        // An empty body line is an empty context line; git omits the marker for it
        // when `diff.suppressBlankEmpty` is unset, which is the default.
        const kind: DiffLineKind = marker === "+"
          ? "addition"
          : marker === "-" ? "deletion" : "context";
        const text = line === "" ? "" : raw.slice(1);
        if (kind === "addition") {
          hunk.lines.push({ kind, oldLine: null, newLine: newCursor, text, noNewline: false });
          newCursor += 1;
          newRemaining -= 1;
          draft.additions += 1;
        } else if (kind === "deletion") {
          hunk.lines.push({ kind, oldLine: oldCursor, newLine: null, text, noNewline: false });
          oldCursor += 1;
          oldRemaining -= 1;
          draft.deletions += 1;
        } else {
          hunk.lines.push({ kind, oldLine: oldCursor, newLine: newCursor, text, noNewline: false });
          oldCursor += 1;
          newCursor += 1;
          oldRemaining -= 1;
          newRemaining -= 1;
        }
        if (oldRemaining <= 0 && newRemaining <= 0) closeHunk();
        continue;
      }
      closeHunk();
      // Fall through: whatever this line is, it belongs to the file, not the hunk.
    }

    if (COMBINED_HUNK_HEADER.test(line)) {
      // A combined diff has one column per parent; reading it as a two-sided patch
      // would number every line wrongly, so the file is reported as unreadable.
      draft.combined = true;
      continue;
    }
    const hunkMatch = HUNK_HEADER.exec(line);
    if (hunkMatch) {
      const oldStart = Number.parseInt(hunkMatch[1], 10);
      const oldLines = hunkMatch[2] === undefined ? 1 : Number.parseInt(hunkMatch[2], 10);
      const newStart = Number.parseInt(hunkMatch[3], 10);
      const newLines = hunkMatch[4] === undefined ? 1 : Number.parseInt(hunkMatch[4], 10);
      hunk = { oldStart, oldLines, newStart, newLines, heading: hunkMatch[5].trimEnd(), lines: [] };
      oldCursor = oldStart;
      newCursor = newStart;
      oldRemaining = oldLines;
      newRemaining = newLines;
      continue;
    }
    if (line.startsWith("--- ")) {
      draft.markerOld = markerPath(line.slice(4));
      continue;
    }
    if (line.startsWith("+++ ")) {
      draft.markerNew = markerPath(line.slice(4));
      continue;
    }
    if (line.startsWith("new file mode ")) {
      draft.newFileMode = line.slice("new file mode ".length).trim();
      continue;
    }
    if (line.startsWith("deleted file mode ")) {
      draft.deletedFileMode = line.slice("deleted file mode ".length).trim();
      continue;
    }
    if (line.startsWith("old mode ")) {
      draft.oldMode = line.slice("old mode ".length).trim();
      continue;
    }
    if (line.startsWith("new mode ")) {
      draft.newMode = line.slice("new mode ".length).trim();
      continue;
    }
    if (line.startsWith("rename from ")) {
      draft.renameFrom = unquoteGitPath(line.slice("rename from ".length));
      continue;
    }
    if (line.startsWith("rename to ")) {
      draft.renameTo = unquoteGitPath(line.slice("rename to ".length));
      continue;
    }
    if (line.startsWith("copy from ")) {
      draft.copyFrom = unquoteGitPath(line.slice("copy from ".length));
      continue;
    }
    if (line.startsWith("copy to ")) {
      draft.copyTo = unquoteGitPath(line.slice("copy to ".length));
      continue;
    }
    // `index`, `similarity index`, `dissimilarity index` and the binary payload all
    // fall through here; none of them carry anything the viewer draws.
    if (line.startsWith("Binary files ") || line.startsWith("GIT binary patch")) {
      draft.binary = true;
    }
  }
  closeFile();

  if (options?.fallbackPath !== undefined && files.length === 1) {
    const only = files[0];
    files[0] = {
      ...only,
      path: options.fallbackPath,
      oldPath: only.oldPath === only.path ? options.fallbackPath : only.oldPath
    };
  }
  return files;
}

/** Total added and removed lines across every file. */
export function diffTotals(files: readonly DiffFile[]): { additions: number; deletions: number } {
  return files.reduce(
    (sum, file) => ({
      additions: sum.additions + file.additions,
      deletions: sum.deletions + file.deletions
    }),
    { additions: 0, deletions: 0 }
  );
}
