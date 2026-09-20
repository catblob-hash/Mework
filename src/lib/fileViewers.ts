/**
 * Which viewer an open workspace file gets.
 *
 * The file pane reads one blob and has to decide what it is looking at. The
 * decision is made from the name alone — the host answers with text or with a
 * `binary` flag, neither of which tells a PNG apart from a ZIP — so the mapping
 * here is the only thing that stands between a screenshot and the "binary file
 * cannot be shown" notice.
 *
 * `code` and `text` render the same way; they are kept apart because only `code`
 * has a grammar to colour, and a viewer that claims a language it cannot name is
 * worse than one that shows the bytes plainly.
 */

import { fileExtension } from "./fileIcons";

export type FileViewerKind = "markdown" | "image" | "code" | "text";

const MARKDOWN_EXTENSIONS = new Set(["md", "markdown", "mdown", "mkd", "mkdn", "mdx"]);

/**
 * Image types a WebView renders from a `data:` URL.
 *
 * `tif`, `icns`, and the raw camera formats are deliberately absent: the tree
 * gives them an image icon, but no browser decodes them, and a broken `<img>`
 * explains less than the binary notice does. SVG is included because it is shown
 * through `<img>`, where scripts and external references never run.
 */
const IMAGE_MEDIA_TYPES: Record<string, string> = {
  png: "image/png",
  jpg: "image/jpeg",
  jpeg: "image/jpeg",
  gif: "image/gif",
  webp: "image/webp",
  avif: "image/avif",
  bmp: "image/bmp",
  ico: "image/x-icon",
  svg: "image/svg+xml"
};

/**
 * Extension to grammar. The value is the grammar id `./codeHighlight` knows, so
 * a language absent from that table must be absent here too — a file whose
 * grammar cannot be found falls back to `text` and is shown uncoloured.
 */
const CODE_LANGUAGES: Record<string, string> = {
  ts: "typescript", tsx: "typescript", mts: "typescript", cts: "typescript",
  js: "javascript", jsx: "javascript", mjs: "javascript", cjs: "javascript",
  rs: "rust",
  go: "go",
  py: "python", pyi: "python", pyw: "python",
  rb: "ruby", rake: "ruby", gemspec: "ruby",
  java: "java",
  kt: "kotlin", kts: "kotlin",
  cs: "csharp",
  swift: "swift",
  c: "c", h: "c",
  cc: "cpp", cpp: "cpp", cxx: "cpp", hpp: "cpp", hh: "cpp", hxx: "cpp",
  m: "c", mm: "cpp",
  php: "php",
  lua: "lua",
  sh: "shell", bash: "shell", zsh: "shell", fish: "shell", ksh: "shell",
  ps1: "shell", psm1: "shell",
  sql: "sql",
  css: "css", scss: "css", sass: "css", less: "css", styl: "css",
  json: "json", json5: "json", jsonc: "json", jsonl: "json", ndjson: "json",
  ipynb: "json",
  yaml: "yaml", yml: "yaml",
  toml: "toml", ini: "toml", cfg: "toml", conf: "toml", properties: "toml",
  html: "xml", htm: "xml", xhtml: "xml", xml: "xml", svg: "xml",
  vue: "xml", svelte: "xml", astro: "xml", plist: "xml", xaml: "xml",
  graphql: "graphql", gql: "graphql", proto: "graphql"
};

/**
 * Files whose whole name is their type. `fileExtension` reports nothing for
 * these, and falling through to `text` would leave a Dockerfile uncoloured next
 * to the `.dockerfile` beside it.
 */
const CODE_FILENAMES: Record<string, string> = {
  dockerfile: "shell",
  containerfile: "shell",
  makefile: "shell",
  gnumakefile: "shell",
  justfile: "shell",
  rakefile: "ruby",
  gemfile: "ruby",
  brewfile: "ruby",
  vagrantfile: "ruby",
  ".bashrc": "shell",
  ".bash_profile": "shell",
  ".zshrc": "shell",
  ".profile": "shell",
  ".gitignore": "shell",
  ".gitattributes": "shell",
  ".dockerignore": "shell",
  ".npmrc": "toml",
  ".editorconfig": "toml",
  ".env": "shell"
};

function fileName(path: string): string {
  return path.slice(path.lastIndexOf("/") + 1);
}

/** The grammar id for `path`, or null when nothing here claims it. */
export function codeLanguage(path: string): string | null {
  const name = fileName(path);
  const extension = fileExtension(name);
  if (extension) return CODE_LANGUAGES[extension] ?? null;
  return CODE_FILENAMES[name.toLowerCase()] ?? null;
}

/** The media type an image of this name is served as, or null when it is not one. */
export function imageMediaType(path: string): string | null {
  return IMAGE_MEDIA_TYPES[fileExtension(fileName(path))] ?? null;
}

export function fileViewerKind(path: string): FileViewerKind {
  const name = fileName(path);
  const extension = fileExtension(name);
  if (MARKDOWN_EXTENSIONS.has(extension)) return "markdown";
  // SVG is both a picture and a document. It is shown as a picture, and the
  // source toggle is what puts its markup on screen.
  if (IMAGE_MEDIA_TYPES[extension]) return "image";
  return codeLanguage(path) === null ? "text" : "code";
}

/**
 * Resolves a reference written inside `documentPath` against the file it was
 * written in, or returns null when it climbs out of the workspace.
 *
 * Markdown links and image references are relative to their own document, not
 * to the workspace root, so `../assets/a.png` in `docs/guide.md` is
 * `assets/a.png`. A rooted reference — `/docs/guide.md` — is taken as
 * workspace-rooted, which is how a repository's own docs are usually written.
 */
export function resolveDocumentReference(documentPath: string, reference: string): string | null {
  const raw = reference.trim();
  if (!raw) return null;
  const rooted = raw.startsWith("/");
  const directory = rooted ? "" : documentPath.slice(0, documentPath.lastIndexOf("/") + 1);
  const segments: string[] = [];
  for (const segment of `${directory}${rooted ? raw.slice(1) : raw}`.split("/")) {
    if (segment === "" || segment === ".") continue;
    if (segment !== "..") {
      segments.push(segment);
      continue;
    }
    if (!segments.length) return null;
    segments.pop();
  }
  return segments.length ? segments.join("/") : null;
}
