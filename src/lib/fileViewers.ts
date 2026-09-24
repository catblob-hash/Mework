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

export type FileViewerKind =
  | "markdown"
  | "html"
  | "image"
  | "pdf"
  | "audio"
  | "video"
  | "font"
  | "csv"
  | "notebook"
  | "code"
  | "text";

const MARKDOWN_EXTENSIONS = new Set(["md", "markdown", "mdown", "mkd", "mkdn", "mdx", "rmd", "qmd"]);

const HTML_EXTENSIONS = new Set(["html", "htm", "xhtml"]);

const CSV_EXTENSIONS = new Set(["csv", "tsv", "tab", "psv"]);

/**
 * Image types a WebView renders from a `data:` URL.
 *
 * TIFF and HEIC decode in WebKit — the macOS window — and nowhere else; the
 * viewer tries them and says so when the engine it runs in cannot. SVG is
 * included because it is shown through `<img>`, where scripts and external
 * references never run.
 */
const IMAGE_MEDIA_TYPES: Record<string, string> = {
  png: "image/png",
  apng: "image/apng",
  jpg: "image/jpeg",
  jpeg: "image/jpeg",
  jpe: "image/jpeg",
  jfif: "image/jpeg",
  pjpeg: "image/jpeg",
  pjp: "image/jpeg",
  gif: "image/gif",
  webp: "image/webp",
  avif: "image/avif",
  bmp: "image/bmp",
  dib: "image/bmp",
  ico: "image/x-icon",
  cur: "image/x-icon",
  tif: "image/tiff",
  tiff: "image/tiff",
  heic: "image/heic",
  heif: "image/heif",
  svg: "image/svg+xml"
};

/** Decoded by the Web Audio API, which — unlike `<audio>` — the renderer's CSP lets run. */
const AUDIO_MEDIA_TYPES: Record<string, string> = {
  mp3: "audio/mpeg",
  wav: "audio/wav",
  wave: "audio/wav",
  ogg: "audio/ogg",
  oga: "audio/ogg",
  opus: "audio/ogg",
  m4a: "audio/mp4",
  aac: "audio/aac",
  flac: "audio/flac",
  weba: "audio/webm",
  aif: "audio/aiff",
  aiff: "audio/aiff",
  caf: "audio/x-caf"
};

const VIDEO_EXTENSIONS = new Set(["mp4", "m4v", "mov", "webm", "mkv", "avi", "ogv", "wmv", "flv", "mpg", "mpeg", "3gp"]);

const FONT_MEDIA_TYPES: Record<string, string> = {
  ttf: "font/ttf",
  otf: "font/otf",
  woff: "font/woff",
  woff2: "font/woff2",
  ttc: "font/collection"
};

/**
 * Extension to grammar. The value is the grammar id `./codeHighlight` knows, so
 * a language absent from that table must be absent here too — a file whose
 * grammar cannot be found falls back to `text` and is shown uncoloured.
 */
const CODE_LANGUAGES: Record<string, string> = {
  ts: "typescript", mts: "typescript", cts: "typescript",
  tsx: "tsx",
  js: "javascript", mjs: "javascript", cjs: "javascript",
  jsx: "jsx",
  rs: "rust",
  go: "go",
  py: "python", pyi: "python", pyw: "python", pyx: "python", pxd: "python", mojo: "python",
  gyp: "python", bzl: "python", star: "python",
  rb: "ruby", rake: "ruby", gemspec: "ruby", ru: "ruby", podspec: "ruby", cr: "ruby",
  java: "java",
  kt: "kotlin", kts: "kotlin",
  scala: "scala", sc: "scala", sbt: "scala",
  groovy: "groovy", gradle: "groovy", gvy: "groovy",
  dart: "dart",
  cs: "csharp", csx: "csharp",
  swift: "swift",
  c: "c", h: "c",
  cc: "cpp", cpp: "cpp", cxx: "cpp", "c++": "cpp", hpp: "cpp", hh: "cpp", hxx: "cpp", "h++": "cpp",
  ipp: "cpp", tpp: "cpp", inl: "cpp", ino: "cpp",
  m: "objectivec", mm: "objectivec",
  glsl: "shader", vert: "shader", frag: "shader", geom: "shader", comp: "shader", tesc: "shader",
  tese: "shader", hlsl: "shader", fx: "shader", wgsl: "shader", metal: "shader", cu: "shader", cuh: "shader",
  sol: "solidity",
  v: "verilog", sv: "verilog", svh: "verilog", vh: "verilog",
  vhd: "vhdl", vhdl: "vhdl",
  zig: "zig", zon: "zig",
  nim: "nim", nims: "nim", nimble: "nim",
  php: "php", phtml: "php",
  pl: "perl", pm: "perl", pod: "perl",
  lua: "lua", luau: "lua",
  r: "r",
  jl: "julia",
  hs: "haskell", lhs: "haskell", purs: "haskell",
  elm: "elm",
  ml: "ocaml", mli: "ocaml", fs: "ocaml", fsi: "ocaml", fsx: "ocaml", re: "ocaml",
  ex: "elixir", exs: "elixir", heex: "elixir",
  erl: "erlang", hrl: "erlang",
  clj: "lisp", cljs: "lisp", cljc: "lisp", edn: "lisp", scm: "lisp", ss: "lisp", rkt: "lisp",
  lisp: "lisp", lsp: "lisp", el: "lisp", fnl: "lisp",
  gleam: "gleam",
  tcl: "tcl",
  sh: "shell", bash: "shell", zsh: "shell", fish: "shell", ksh: "shell", command: "shell", bats: "shell",
  awk: "shell", env: "shell",
  ps1: "powershell", psm1: "powershell", psd1: "powershell",
  bat: "batch", cmd: "batch",
  vb: "vb", vbs: "vb", bas: "vb", vba: "vb",
  f: "fortran", for: "fortran", f77: "fortran", f90: "fortran", f95: "fortran", f03: "fortran", f08: "fortran",
  sql: "sql", psql: "sql", pgsql: "sql", mysql: "sql", ddl: "sql", dml: "sql",
  css: "css",
  scss: "scss", sass: "scss", less: "scss", styl: "scss", pcss: "scss",
  json: "json", json5: "json", jsonc: "json", jsonl: "json", ndjson: "json", webmanifest: "json",
  har: "json", geojson: "json", topojson: "json", ipynb: "json",
  yaml: "yaml", yml: "yaml",
  toml: "toml",
  ini: "ini", cfg: "ini", conf: "ini", properties: "ini", prefs: "ini", desktop: "ini", service: "ini",
  editorconfig: "ini", gitconfig: "ini", reg: "ini",
  html: "xml", htm: "xml", xhtml: "xml", xml: "xml", svg: "xml", xsd: "xml", xsl: "xml", xslt: "xml",
  wsdl: "xml", rss: "xml", atom: "xml", plist: "xml", xaml: "xml", csproj: "xml", fsproj: "xml",
  vbproj: "xml", props: "xml", targets: "xml", resx: "xml", storyboard: "xml", xib: "xml", kml: "xml",
  gpx: "xml", vue: "xml", svelte: "xml", astro: "xml", jsp: "xml", asp: "xml", aspx: "xml",
  cshtml: "xml", razor: "xml", erb: "xml", ejs: "xml", hbs: "xml", handlebars: "xml", mustache: "xml",
  njk: "xml", jinja: "xml", jinja2: "xml", j2: "xml", liquid: "xml", twig: "xml",
  graphql: "graphql", gql: "graphql", proto: "graphql", thrift: "graphql", prisma: "graphql",
  tf: "hcl", tfvars: "hcl", hcl: "hcl", nomad: "hcl",
  nix: "nix",
  tex: "latex", sty: "latex", cls: "latex", ltx: "latex", bib: "latex",
  asm: "assembly", s: "assembly", nasm: "assembly",
  cmake: "cmake",
  mk: "makefile", mak: "makefile",
  dockerfile: "dockerfile",
  diff: "diff", patch: "diff", rej: "diff",
  log: "log"
};

/**
 * Files whose whole name is their type. `fileExtension` reports nothing for
 * these, and falling through to `text` would leave a Dockerfile uncoloured next
 * to the `.dockerfile` beside it.
 */
const CODE_FILENAMES: Record<string, string> = {
  dockerfile: "dockerfile",
  containerfile: "dockerfile",
  makefile: "makefile",
  gnumakefile: "makefile",
  bsdmakefile: "makefile",
  justfile: "makefile",
  "cmakelists.txt": "cmake",
  rakefile: "ruby",
  gemfile: "ruby",
  brewfile: "ruby",
  vagrantfile: "ruby",
  podfile: "ruby",
  fastfile: "ruby",
  guardfile: "ruby",
  capfile: "ruby",
  jenkinsfile: "groovy",
  "cargo.lock": "toml",
  "poetry.lock": "toml",
  pipfile: "toml",
  "yarn.lock": "yaml",
  procfile: "yaml",
  ".clang-format": "yaml",
  "go.mod": "go",
  "go.sum": "text",
  ".bashrc": "shell",
  ".bash_profile": "shell",
  ".bash_logout": "shell",
  ".zshrc": "shell",
  ".zshenv": "shell",
  ".zprofile": "shell",
  ".profile": "shell",
  ".gitignore": "shell",
  ".gitattributes": "shell",
  ".gitmodules": "ini",
  ".dockerignore": "shell",
  ".npmignore": "shell",
  ".prettierignore": "shell",
  ".eslintignore": "shell",
  ".htaccess": "shell",
  codeowners: "shell",
  ".npmrc": "ini",
  ".yarnrc": "ini",
  ".editorconfig": "ini",
  ".gitconfig": "ini",
  ".babelrc": "json",
  ".eslintrc": "json",
  ".prettierrc": "json",
  ".env": "shell"
};

/**
 * Names a fenced code block may carry that are not also file extensions.
 *
 * Everything else a fence says is looked up as an extension, which is how most
 * of them are written anyway: ```ts, ```py, ```rs.
 */
const FENCE_ALIASES: Record<string, string> = {
  typescript: "typescript",
  javascript: "javascript",
  node: "javascript",
  python: "python",
  python3: "python",
  rust: "rust",
  golang: "go",
  ruby: "ruby",
  kotlin: "kotlin",
  csharp: "csharp",
  "c#": "csharp",
  "objective-c": "objectivec",
  objc: "objectivec",
  "f#": "ocaml",
  fsharp: "ocaml",
  ocaml: "ocaml",
  shell: "shell",
  console: "shell",
  terminal: "shell",
  sh: "shell",
  shellsession: "shell",
  powershell: "powershell",
  pwsh: "powershell",
  batch: "batch",
  dos: "batch",
  yml: "yaml",
  html: "xml",
  xml: "xml",
  vue: "xml",
  svelte: "xml",
  markdown: "markdown",
  md: "markdown",
  latex: "latex",
  tex: "latex",
  haskell: "haskell",
  elixir: "elixir",
  erlang: "erlang",
  clojure: "lisp",
  scheme: "lisp",
  racket: "lisp",
  elisp: "lisp",
  emacs: "lisp",
  julia: "julia",
  perl: "perl",
  fortran: "fortran",
  vbnet: "vb",
  vba: "vb",
  terraform: "hcl",
  docker: "dockerfile",
  dockerfile: "dockerfile",
  make: "makefile",
  makefile: "makefile",
  cmake: "cmake",
  protobuf: "graphql",
  solidity: "solidity",
  verilog: "verilog",
  systemverilog: "verilog",
  glsl: "shader",
  hlsl: "shader",
  wgsl: "shader",
  cuda: "shader",
  assembly: "assembly",
  nasm: "assembly",
  diff: "diff",
  patch: "diff",
  udiff: "diff",
  jsonc: "json",
  json5: "json",
  ini: "ini",
  toml: "toml",
  env: "shell",
  dotenv: "shell",
  log: "log"
};

function fileName(path: string): string {
  return path.slice(path.lastIndexOf("/") + 1);
}

/** The grammar id for `path`, or null when nothing here claims it. */
export function codeLanguage(path: string): string | null {
  const name = fileName(path);
  const lower = name.toLowerCase();
  const byName = CODE_FILENAMES[lower];
  if (byName !== undefined) return byName === "text" ? null : byName;
  // `.env.local`, `.env.production`: the name, not the suffix, says what it is.
  if (lower.startsWith(".env.")) return "shell";
  if (lower.startsWith("dockerfile.") || lower.endsWith(".dockerfile")) return "dockerfile";
  const extension = fileExtension(name);
  if (MARKDOWN_EXTENSIONS.has(extension)) return "markdown";
  if (CSV_EXTENSIONS.has(extension)) return null;
  return extension ? CODE_LANGUAGES[extension] ?? null : null;
}

/** The grammar a fenced code block names in its info string, or null when it names none known. */
export function fenceLanguage(info: string | null | undefined): string | null {
  if (!info) return null;
  const name = info.trim().split(/[\s{,]/)[0].toLowerCase().replace(/^\./, "");
  if (!name) return null;
  return FENCE_ALIASES[name] ?? CODE_LANGUAGES[name] ?? CODE_FILENAMES[name] ?? null;
}

/** The media type an image of this name is served as, or null when it is not one. */
export function imageMediaType(path: string): string | null {
  return IMAGE_MEDIA_TYPES[fileExtension(fileName(path))] ?? null;
}

/** The media type of a file the pane reads as bytes rather than as text, or null. */
export function binaryMediaType(path: string): string | null {
  const extension = fileExtension(fileName(path));
  if (extension === "pdf") return "application/pdf";
  return IMAGE_MEDIA_TYPES[extension]
    ?? AUDIO_MEDIA_TYPES[extension]
    ?? FONT_MEDIA_TYPES[extension]
    ?? null;
}

export function fileViewerKind(path: string): FileViewerKind {
  const name = fileName(path);
  const extension = fileExtension(name);
  if (MARKDOWN_EXTENSIONS.has(extension)) return "markdown";
  if (HTML_EXTENSIONS.has(extension)) return "html";
  // SVG is both a picture and a document. It is shown as a picture, and the
  // source toggle is what puts its markup on screen.
  if (IMAGE_MEDIA_TYPES[extension]) return "image";
  if (extension === "pdf") return "pdf";
  if (AUDIO_MEDIA_TYPES[extension]) return "audio";
  if (VIDEO_EXTENSIONS.has(extension)) return "video";
  if (FONT_MEDIA_TYPES[extension]) return "font";
  if (CSV_EXTENSIONS.has(extension)) return "csv";
  if (extension === "ipynb") return "notebook";
  return codeLanguage(path) === null ? "text" : "code";
}

/**
 * Whether the file has a rendered form and a source form, so the viewer offers
 * the switch between them. A picture's "source" is only readable when it is text,
 * which for pictures means SVG.
 */
export function hasSourceForm(path: string): boolean {
  const kind = fileViewerKind(path);
  if (kind === "image") return fileExtension(fileName(path)) === "svg";
  return kind === "markdown" || kind === "html" || kind === "csv" || kind === "notebook";
}

/** Whether the viewer needs the file's bytes rather than its text to show it at all. */
export function readsBytes(path: string): boolean {
  const kind = fileViewerKind(path);
  return kind === "image" || kind === "pdf" || kind === "audio" || kind === "font";
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
