/**
 * Which icon stands for a file.
 *
 * The reference shell sorts every path into one of eight buckets and draws one
 * glyph per bucket rather than one per language, so a tree of mixed sources reads
 * as shape rather than as a colour field. The buckets are reproduced here; the
 * glyphs are this app's icon set, which is not the reference's, so only the
 * grouping is shared.
 */
export type FileIconKind =
  | "doc"
  | "code"
  | "data"
  | "sheet"
  | "preso"
  | "image"
  | "archive"
  | "skill";

const CODE_EXTENSIONS = new Set([
  "ts", "tsx", "js", "jsx", "mjs", "cjs", "mts", "cts",
  "rs", "go", "py", "pyi", "rb", "java", "kt", "kts", "scala", "swift",
  "c", "h", "cc", "cpp", "cxx", "hpp", "hh", "m", "mm",
  "cs", "fs", "fsx", "vb", "php", "pl", "pm", "lua", "r", "jl", "dart", "ex", "exs",
  "erl", "hrl", "hs", "elm", "clj", "cljs", "nim", "zig", "v", "sol",
  "sh", "bash", "zsh", "fish", "ps1", "psm1", "bat", "cmd",
  "html", "htm", "xhtml", "vue", "svelte", "astro",
  "css", "scss", "sass", "less", "styl",
  "sql", "graphql", "gql", "proto", "tla", "cfg", "csp",
  "json", "json5", "jsonc", "yaml", "yml", "toml", "ini", "xml", "plist",
  "gradle", "cmake", "mk", "dockerfile", "tf", "tfvars", "hcl"
]);

const DATA_EXTENSIONS = new Set(["jsonl", "ndjson", "parquet", "db", "sqlite", "sqlite3", "avro", "orc"]);
const SHEET_EXTENSIONS = new Set(["csv", "tsv", "xls", "xlsx", "ods", "numbers"]);
const PRESO_EXTENSIONS = new Set(["ppt", "pptx", "odp", "key"]);
const IMAGE_EXTENSIONS = new Set([
  "png", "jpg", "jpeg", "gif", "webp", "avif", "bmp", "tif", "tiff", "ico", "icns", "svg"
]);
const ARCHIVE_EXTENSIONS = new Set([
  "zip", "tar", "gz", "tgz", "bz2", "xz", "zst", "7z", "rar", "jar", "war", "asar", "dmg", "iso"
]);
const DOC_EXTENSIONS = new Set([
  "pdf", "md", "markdown", "mdx", "txt", "log", "rtf", "doc", "docx", "odt", "pages",
  "rst", "tex", "epub", "adoc",
  "mp4", "mov", "webm", "mkv", "avi", "m4v", "ogv", "3gp",
  "mp3", "wav", "ogg", "flac", "aac", "m4a", "wma", "aiff"
]);

/**
 * Extension of a file name, lowercased.
 *
 * A dotfile with no second dot — `.gitignore`, `.env` — is all name and no
 * extension; treating its name as one would file every dotfile under whatever
 * bucket its name happened to land in.
 */
export function fileExtension(name: string): string {
  const separator = name.lastIndexOf(".");
  if (separator <= 0) return "";
  return name.slice(separator + 1).toLowerCase();
}

export function fileIconKind(path: string): FileIconKind {
  const name = path.slice(path.lastIndexOf("/") + 1);
  if (name === "SKILL.md") return "skill";
  const extension = fileExtension(name);
  if (!extension) return "doc";
  if (ARCHIVE_EXTENSIONS.has(extension)) return "archive";
  if (PRESO_EXTENSIONS.has(extension)) return "preso";
  if (DATA_EXTENSIONS.has(extension)) return "data";
  if (SHEET_EXTENSIONS.has(extension)) return "sheet";
  if (IMAGE_EXTENSIONS.has(extension)) return "image";
  if (CODE_EXTENSIONS.has(extension)) return "code";
  if (DOC_EXTENSIONS.has(extension)) return "doc";
  return "doc";
}
