/**
 * What a link in rendered prose points at when it does not point at the web.
 *
 * Model replies and repository docs both link files the way their own tools
 * write them: `src/App.tsx:42`, `docs/guide.md#L10-L20`, `file:///home/me/a.rs`,
 * `../README.md#install`. Each of those names a file, possibly a line in it, and
 * possibly a heading; this module is the one place that takes the forms apart, so
 * the transcript and the file pane agree on what a link means.
 */

export interface LocalLinkTarget {
  /** The file, exactly as written but decoded; empty for a link to a heading in the same document. */
  path: string;
  /** The line named by `:12` or `#L12`, or null when the link names none. */
  line: number | null;
  /** A fragment that is not a line reference: the heading a link jumps to. */
  fragment: string | null;
}

/** Any scheme but a Windows drive letter, which is one character before its colon. */
const SCHEME = /^[a-z][a-z0-9+.-]+:/i;

/** `#L12`, `#L12-L20`, `#L12C3`, and the `#12` some tools write. */
const LINE_FRAGMENT = /^L?(\d+)(?:C\d+)?(?:-L?\d+(?:C\d+)?)?$/i;

/** `:12` or `:12:5` at the end of a path. */
const LINE_SUFFIX = /:(\d+)(?::\d+)?$/;

function decode(value: string): string {
  try {
    return decodeURIComponent(value);
  } catch {
    // A stray `%` is part of the name, not the start of an escape.
    return value;
  }
}

function positiveLine(value: string): number | null {
  const line = Number.parseInt(value, 10);
  return Number.isSafeInteger(line) && line > 0 ? line : null;
}

/**
 * Takes a link's `href` apart, or returns null when it is somebody else's to
 * follow — the web, mail, a data URL — or when it names nothing at all.
 */
export function localLinkTarget(href: string | null | undefined): LocalLinkTarget | null {
  if (typeof href !== "string") return null;
  let raw = href.trim();
  if (!raw) return null;
  if (/^file:/i.test(raw)) {
    raw = raw.replace(/^file:(?:\/\/(?:localhost)?)?/i, "");
    // `file:///C:/x` carries the drive after the root slash.
    if (/^\/[A-Za-z]:[\\/]/.test(raw)) raw = raw.slice(1);
  } else if (SCHEME.test(raw) || raw.startsWith("//")) {
    return null;
  }

  const hash = raw.indexOf("#");
  const beforeHash = hash < 0 ? raw : raw.slice(0, hash);
  const fragment = hash < 0 ? null : decode(raw.slice(hash + 1));
  const query = beforeHash.indexOf("?");
  let path = decode(query < 0 ? beforeHash : beforeHash.slice(0, query));

  let line: number | null = null;
  const suffix = path.match(LINE_SUFFIX);
  // `C:` alone is a drive, not a line; a suffix needs a path in front of it.
  if (suffix && suffix.index !== undefined && suffix.index > 0 && !/^[A-Za-z]$/.test(path.slice(0, suffix.index))) {
    line = positiveLine(suffix[1]);
    path = path.slice(0, suffix.index);
  }

  let heading: string | null = null;
  if (fragment !== null && fragment !== "") {
    const lineMatch = path ? fragment.match(LINE_FRAGMENT) : null;
    if (lineMatch) line = positiveLine(lineMatch[1]);
    else heading = fragment;
  }

  if (!path && heading === null) return null;
  return { path, line, fragment: heading };
}

/**
 * The anchor a heading answers to, in the form the ecosystem settled on:
 * lowercased, punctuation dropped, spaces hyphenated.
 *
 * Computed from the headings on screen at click time rather than written into
 * them at render time, so a document's own table of contents works without the
 * renderer having to mint ids for every heading it draws.
 */
export function headingSlug(text: string): string {
  return text.trim().toLowerCase()
    .replace(/[^\p{L}\p{N}\s_-]/gu, "")
    .replace(/\s/g, "-");
}

/** Hand-written tables of contents often fold the runs GitHub's slugs keep: `a--b` as `a-b`. */
function foldedSlug(slug: string): string {
  return slug.replace(/-{2,}/g, "-");
}

function scrollableParent(element: Element): HTMLElement | null {
  let node: Node | null = element.parentNode;
  while (node) {
    if (node instanceof HTMLElement) {
      const { overflowY } = getComputedStyle(node);
      if ((overflowY === "auto" || overflowY === "scroll") && node.scrollHeight > node.clientHeight) return node;
      node = node.parentNode;
    } else if (node instanceof ShadowRoot) {
      node = node.host;
    } else {
      node = node.parentNode;
    }
  }
  return null;
}

/**
 * Brings `element` into view by scrolling only the one box that scrolls it.
 *
 * `scrollIntoView` scrolls every ancestor that can move, and `overflow: hidden`
 * boxes can: a jump inside the file pane would also slide the pane's own chrome
 * out from under its title bar.
 *
 * `nearest` moves the box only as far as the element needs to be wholly in it,
 * and not at all when it already is, or when nothing scrolls it.
 */
export function scrollIntoContainer(element: Element, block: "start" | "center" | "nearest" = "start"): void {
  const container = scrollableParent(element);
  if (!container) {
    if (block !== "nearest") element.scrollIntoView({ block });
    return;
  }
  const target = element.getBoundingClientRect();
  const frame = container.getBoundingClientRect();
  if (block === "nearest") {
    if (target.top < frame.top) container.scrollTop += target.top - frame.top;
    else if (target.bottom > frame.bottom) container.scrollTop += Math.min(target.bottom - frame.bottom, target.top - frame.top);
    return;
  }
  const offset = block === "center"
    ? target.top - frame.top - (frame.height - target.height) / 2
    : target.top - frame.top - 8;
  container.scrollTop += offset;
}

/**
 * Scrolls to what `fragment` names inside `container`, and reports whether
 * anything answered.
 *
 * An element with that id comes first — footnotes and HTML anchors carry one,
 * possibly behind the prefix the sanitizer gives every id — then a heading whose
 * slug matches. Only `container` is searched: two replies on screen can each have
 * a footnote 1, and the link means the one beside it.
 */
export function scrollToFragment(container: ParentNode | null, fragment: string): boolean {
  if (!container || !fragment) return false;
  let decoded = fragment;
  try {
    decoded = decodeURIComponent(fragment);
  } catch {
    // A fragment that is not valid percent-encoding is still a fragment.
  }
  for (const id of [`user-content-${decoded}`, decoded]) {
    const escaped = typeof CSS !== "undefined" && typeof CSS.escape === "function" ? CSS.escape(id) : id.replace(/["\\]/g, "\\$&");
    const target = container.querySelector(`[id="${escaped}"], [name="${escaped}"]`);
    if (target) {
      scrollIntoContainer(target);
      return true;
    }
  }
  const wanted = headingSlug(decoded);
  if (!wanted) return false;
  for (const heading of container.querySelectorAll("h1, h2, h3, h4, h5, h6")) {
    const slug = headingSlug(heading.textContent ?? "");
    if (slug !== wanted && foldedSlug(slug) !== foldedSlug(wanted)) continue;
    scrollIntoContainer(heading);
    return true;
  }
  return false;
}
