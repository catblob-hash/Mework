//! The file browser's reads and writes, on the machine the files are on.
//!
//! The host's file pane browses this computer, a WSL distribution or an SSH
//! machine with the same requests. For this computer the host calls [`handle`]
//! itself; on another machine the agent's `files` helper (`mework-remote
//! files`) does, the request on standard input and the reply on standard
//! output — one round trip per request, the shape of the `git` helper. One
//! implementation means a listing sorts, a binary file is recognized and a path
//! is spelled the same way wherever the file is.
//!
//! Paths are the machine's own and absolute; a leading `~` is the account's
//! home and the empty path is the home itself. A Windows machine's paths are
//! spelled with forward slashes and an upper-case drive (`C:/Users/dev`), which
//! the host and the renderer compare without caring about the separator, and
//! `/` there is the list of the machine's drives, so going up from `C:/` leads
//! somewhere. Paths are normalized lexically — `.` and `..` applied, repeated
//! separators collapsed — and never canonicalized: the pane shows the path the
//! reader walked, and a workspace registered under a symlink stays the path it
//! was registered as.
//!
//! This is the reader's own file manager, not a tool the model calls, so
//! nothing here is confined to a workspace: what the renderer asks for is what
//! the user is looking at. What *is* bounded is what one request may cost — a
//! text read stops at [`MAX_TEXT_BYTES`], a byte read refuses past
//! [`MAX_PREVIEW_BYTES`], a listing at [`MAX_LIST_ENTRIES`] and a search at its
//! budgets — because an answer has to cross a link and land in a renderer.

use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// A text read stops here and says so.
pub const MAX_TEXT_BYTES: u64 = 1_048_576;
/// A byte read crosses as base64, a third larger again, so it is refused whole
/// past this rather than sent in part: half a picture decodes to nothing.
pub const MAX_PREVIEW_BYTES: u64 = 8 * 1_048_576;
/// How many paths one stat request may ask about.
pub const MAX_STAT_PATHS: usize = 64;
/// A directory with more entries than this is listed in part, and says so.
pub const MAX_LIST_ENTRIES: usize = 20_000;
/// The most matches a search hands back.
pub const MAX_SEARCH_LIMIT: usize = 200;

const BINARY_SNIFF_BYTES: usize = 8 * 1024;
const MAX_PATH_CHARS: usize = 4096;
const MAX_NAME_BYTES: usize = 255;
const MAX_REQUEST_BYTES: u64 = 1 << 20;

// The search is a convenience for finding a file, not a promise to see every
// one: past a budget it stops and says it did.
const MAX_SEARCH_ENTRIES: usize = 20_000;
const MAX_SEARCH_DEPTH: usize = 12;
const MAX_SEARCH_MILLIS: u64 = 2_000;
const SEARCH_GIT_TIMEOUT: Duration = Duration::from_secs(5);
const SEARCH_GIT_MAX_OUTPUT: usize = 8 * 1024 * 1024;

/// Version-control internals, dependencies and build output: enough of them to
/// eat the walk's whole budget, and never what a reader is looking for.
const SEARCH_SKIPPED_DIRECTORIES: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "dist",
    "build",
    ".next",
    ".venv",
    "venv",
    "__pycache__",
    ".cache",
    ".turbo",
    ".gradle",
    ".idea",
    "vendor",
];

/// What a path names, with links followed. A link whose target is gone is
/// [`EntryKind::Other`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EntryKind {
    Directory,
    File,
    Other,
}

/// One name in a directory.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub name: String,
    /// The entry's own absolute path, spelled the way every reply spells one.
    pub path: String,
    pub kind: EntryKind,
    /// Whether the name is a symbolic link (or a junction); `kind` is then what
    /// it leads to.
    pub link: bool,
    /// Byte size of a file; `None` for anything else.
    pub size: Option<u64>,
}

/// One directory.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Listing {
    /// The directory, normalized: `~` expanded, `..` applied. `/` on a Windows
    /// machine is its list of drives.
    pub path: String,
    /// Where "up" leads, or `None` at the top of the machine.
    pub parent: Option<String>,
    /// Whether the machine spells paths the Windows way.
    pub windows: bool,
    /// Directories first, then by name without case — the host's order.
    pub entries: Vec<Entry>,
    /// True when the directory held more than [`MAX_LIST_ENTRIES`].
    pub truncated: bool,
}

/// What one path of a stat request turned out to be.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Stat {
    /// The path normalized, or the request's own spelling when it could not be.
    pub path: String,
    /// `None` when nothing is there, or it cannot be reached.
    pub kind: Option<EntryKind>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Stats {
    pub windows: bool,
    pub stats: Vec<Stat>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextFile {
    pub path: String,
    /// Lossily decoded text; empty for a binary file.
    pub content: String,
    /// True when the read stopped at [`MAX_TEXT_BYTES`].
    pub truncated: bool,
    pub size: u64,
    pub binary: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileBytes {
    pub path: String,
    /// Standard base64 of the whole file; empty when `too_large`.
    pub data: String,
    pub size: u64,
    pub too_large: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchMatch {
    pub name: String,
    /// Relative to the searched directory, `/` separated.
    pub path: String,
    pub kind: EntryKind,
    /// Character offsets into `path` that the query matched, ascending.
    pub positions: Vec<u32>,
    pub score: i32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResults {
    pub query: String,
    /// The searched directory, normalized.
    pub root: String,
    pub matches: Vec<SearchMatch>,
    /// True when the search stopped at a budget rather than seeing everything.
    pub truncated: bool,
}

/// The path a rename or a new directory ended up at.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Changed {
    pub path: String,
}

/// One request. The tag is `op`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum FilesOp {
    List {
        path: String,
    },
    Stat {
        paths: Vec<String>,
    },
    ReadText {
        path: String,
    },
    ReadBytes {
        path: String,
    },
    Search {
        root: String,
        query: String,
        limit: usize,
    },
    Rename {
        path: String,
        name: String,
    },
    /// Removes a file, a link or a whole directory for good. The host sends a
    /// file on its own computer to the Trash instead and never asks this for it.
    Remove {
        path: String,
    },
    CreateDirectory {
        parent: String,
        name: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilesRequest {
    pub op: FilesOp,
    /// Whether failures are worded in English rather than Chinese.
    #[serde(default)]
    pub english: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilesReply {
    Ok(serde_json::Value),
    Err(String),
}

macro_rules! say {
    ($english:expr, $zh:literal, $en:literal $(, $($arg:tt)*)?) => {
        if $english {
            ::std::format!($en $(, $($arg)*)?)
        } else {
            ::std::format!($zh $(, $($arg)*)?)
        }
    };
}

/// Answers one request on this machine.
pub fn handle(request: FilesRequest) -> FilesReply {
    let english = request.english;
    let machine = Machine::this(english);
    let value = match request.op {
        FilesOp::List { path } => machine
            .list(&path)
            .and_then(|value| to_value(value, english)),
        FilesOp::Stat { paths } => machine
            .stat(&paths)
            .and_then(|value| to_value(value, english)),
        FilesOp::ReadText { path } => machine
            .read_text(&path)
            .and_then(|value| to_value(value, english)),
        FilesOp::ReadBytes { path } => machine
            .read_bytes(&path)
            .and_then(|value| to_value(value, english)),
        FilesOp::Search { root, query, limit } => machine
            .search(&root, &query, limit)
            .and_then(|value| to_value(value, english)),
        FilesOp::Rename { path, name } => machine
            .rename(&path, &name)
            .and_then(|value| to_value(value, english)),
        FilesOp::Remove { path } => machine
            .remove(&path)
            .and_then(|()| to_value(serde_json::Value::Null, english)),
        FilesOp::CreateDirectory { parent, name } => machine
            .create_directory(&parent, &name)
            .and_then(|value| to_value(value, english)),
    };
    match value {
        Ok(value) => FilesReply::Ok(value),
        Err(message) => FilesReply::Err(message),
    }
}

fn to_value<T: Serialize>(value: T, english: bool) -> Result<serde_json::Value, String> {
    serde_json::to_value(value).map_err(|error| {
        say!(
            english,
            "无法编码文件回答：{error}",
            "Could not encode the file reply: {error}"
        )
    })
}

/// `mework-remote files`: one request on standard input, one reply on standard
/// output. A request that cannot be read is answered like any other failure,
/// so the host always has a reply to parse.
pub fn run_stdio() -> Result<(), String> {
    let mut input = Vec::new();
    std::io::stdin()
        .take(MAX_REQUEST_BYTES + 1)
        .read_to_end(&mut input)
        .map_err(|error| format!("Could not read the file request: {error}"))?;
    let reply = if input.len() as u64 > MAX_REQUEST_BYTES {
        FilesReply::Err("The file request exceeds the size limit".into())
    } else {
        match serde_json::from_slice::<FilesRequest>(&input) {
            Ok(request) => handle(request),
            Err(error) => {
                let english = serde_json::from_slice::<serde_json::Value>(&input)
                    .ok()
                    .and_then(|request| request.get("english")?.as_bool())
                    .unwrap_or(false);
                FilesReply::Err(say!(
                    english,
                    "无法解析文件请求：{error}",
                    "Could not parse the file request: {error}"
                ))
            }
        }
    };
    let bytes = serde_json::to_vec(&reply)
        .map_err(|error| format!("Could not encode the file reply: {error}"))?;
    let mut stdout = std::io::stdout().lock();
    stdout
        .write_all(&bytes)
        .and_then(|()| stdout.flush())
        .map_err(|error| format!("Could not write the file reply: {error}"))
}

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

/// Where a request points once `~` and the lexical rules have been applied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Place {
    /// A Windows machine's drives.
    Drives,
    /// An absolute path, spelled the way replies spell it.
    Path(String),
}

/// Normalizes `raw` the way a machine of the given kind spells paths.
///
/// `home` is the account's home, already in that spelling; without one a `~`
/// path is refused. A relative path is refused rather than read against
/// wherever the helper happens to run, and so is a Windows network path:
/// opening `//host/share` makes Windows authenticate against `host`.
pub fn normalize(
    raw: &str,
    windows: bool,
    home: Option<&str>,
    english: bool,
) -> Result<Place, String> {
    if raw.chars().count() > MAX_PATH_CHARS {
        return Err(say!(english, "路径过长", "The path is too long"));
    }
    if raw.chars().any(char::is_control) {
        return Err(say!(
            english,
            "路径包含控制字符",
            "The path contains control characters"
        ));
    }
    let mut text = if windows {
        raw.replace('\\', "/")
    } else {
        raw.to_owned()
    };
    if text.is_empty() || text == "~" || text.starts_with("~/") {
        let home = home.ok_or_else(|| {
            say!(
                english,
                "这台机器没有主目录",
                "This machine has no home directory"
            )
        })?;
        let home = if windows {
            home.replace('\\', "/")
        } else {
            home.to_owned()
        };
        let rest = text.strip_prefix('~').unwrap_or("");
        text = format!(
            "{}/{}",
            home.trim_end_matches('/'),
            rest.trim_start_matches('/')
        );
    } else if text.starts_with('~') {
        // `~user` is someone else's home, which this has no reason to reach.
        return Err(say!(
            english,
            "不支持的路径写法：{raw}",
            "Unsupported path form: {raw}"
        ));
    }

    if windows {
        if text.chars().all(|character| character == '/') {
            return Ok(Place::Drives);
        }
        if text.starts_with("//") {
            return Err(say!(
                english,
                "不打开网络路径：{raw}",
                "Network paths are not opened: {raw}"
            ));
        }
        let mut characters = text.chars();
        let (Some(letter), Some(':')) = (characters.next(), characters.next()) else {
            return Err(if text.starts_with('/') {
                say!(
                    english,
                    "路径缺少盘符：{raw}",
                    "The path names no drive: {raw}"
                )
            } else {
                say!(
                    english,
                    "需要绝对路径：{raw}",
                    "An absolute path is needed: {raw}"
                )
            });
        };
        if !letter.is_ascii_alphabetic() {
            return Err(say!(
                english,
                "需要绝对路径：{raw}",
                "An absolute path is needed: {raw}"
            ));
        }
        let rest = &text[2..];
        if !rest.is_empty() && !rest.starts_with('/') {
            // `C:foo` is relative to the drive's current directory.
            return Err(say!(
                english,
                "需要绝对路径：{raw}",
                "An absolute path is needed: {raw}"
            ));
        }
        let segments = collapse(rest);
        let drive = letter.to_ascii_uppercase();
        return Ok(Place::Path(format!("{drive}:/{}", segments.join("/"))));
    }

    if !text.starts_with('/') {
        return Err(say!(
            english,
            "需要绝对路径：{raw}",
            "An absolute path is needed: {raw}"
        ));
    }
    Ok(Place::Path(format!("/{}", collapse(&text).join("/"))))
}

/// Applies `.` and `..` and drops empty segments. `..` at the top stays at the
/// top, the way a shell's `cd /..` does.
fn collapse(path: &str) -> Vec<&str> {
    let mut segments = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            other => segments.push(other),
        }
    }
    segments
}

/// Where "up" leads from `place`, or `None` at the top.
pub fn parent_of(place: &Place, windows: bool) -> Option<String> {
    let Place::Path(path) = place else {
        return None;
    };
    let trimmed = path.trim_end_matches('/');
    if windows {
        // `C:/` is the top of its drive; above it is the list of drives.
        if trimmed.len() <= 2 {
            return Some("/".to_owned());
        }
        let cut = trimmed.rfind('/')?;
        return Some(if cut <= 2 {
            format!("{}/", &trimmed[..2])
        } else {
            trimmed[..cut].to_owned()
        });
    }
    if trimmed.is_empty() {
        return None;
    }
    let cut = trimmed.rfind('/')?;
    Some(if cut == 0 {
        "/".to_owned()
    } else {
        trimmed[..cut].to_owned()
    })
}

/// `dir` and `name` joined with one separator.
pub fn join(dir: &str, name: &str) -> String {
    if dir.ends_with('/') {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    }
}

/// Whether `name` can be given to a file as it is: one segment, nothing the
/// machine would read as a separator or a reference to a directory.
fn check_name(name: &str, windows: bool, english: bool) -> Result<(), String> {
    let invalid = name.is_empty()
        || name == "."
        || name == ".."
        || name.len() > MAX_NAME_BYTES
        || name.contains('/')
        || name.chars().any(char::is_control)
        || (windows
            && (name.contains(['\\', ':', '*', '?', '"', '<', '>', '|'])
                || name.ends_with([' ', '.'])));
    if invalid {
        return Err(say!(english, "名称无效：{name}", "Invalid name: {name}"));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Operations
// ---------------------------------------------------------------------------

/// This machine, as the requests see it.
struct Machine {
    windows: bool,
    home: Option<String>,
    english: bool,
}

impl Machine {
    fn this(english: bool) -> Self {
        let windows = cfg!(windows);
        let home = std::env::var_os(if windows { "USERPROFILE" } else { "HOME" })
            .or_else(|| std::env::var_os(if windows { "HOME" } else { "USERPROFILE" }))
            .filter(|home| !home.is_empty())
            .map(|home| {
                let home = home.to_string_lossy().into_owned();
                if windows {
                    home.replace('\\', "/")
                } else {
                    home
                }
            });
        Machine {
            windows,
            home,
            english,
        }
    }

    fn place(&self, raw: &str) -> Result<Place, String> {
        normalize(raw, self.windows, self.home.as_deref(), self.english)
    }

    fn io_error(&self, path: &str, error: &std::io::Error) -> String {
        match error.kind() {
            ErrorKind::NotFound => say!(self.english, "找不到 {path}", "{path} does not exist"),
            ErrorKind::PermissionDenied => say!(
                self.english,
                "没有权限访问 {path}",
                "Permission denied: {path}"
            ),
            ErrorKind::AlreadyExists => {
                say!(self.english, "{path} 已经存在", "{path} already exists")
            }
            _ => say!(
                self.english,
                "无法访问 {path}：{error}",
                "Could not access {path}: {error}"
            ),
        }
    }

    fn list(&self, raw: &str) -> Result<Listing, String> {
        let place = self.place(raw)?;
        let parent = parent_of(&place, self.windows);
        let Place::Path(path) = &place else {
            return Ok(Listing {
                path: "/".to_owned(),
                parent,
                windows: true,
                entries: drives(),
                truncated: false,
            });
        };
        let metadata = fs::metadata(path).map_err(|error| self.io_error(path, &error))?;
        if !metadata.is_dir() {
            return Err(say!(
                self.english,
                "{path} 不是目录",
                "{path} is not a directory"
            ));
        }
        let mut entries = Vec::new();
        let mut truncated = false;
        for entry in fs::read_dir(path).map_err(|error| self.io_error(path, &error))? {
            let Ok(entry) = entry else { continue };
            if entries.len() >= MAX_LIST_ENTRIES {
                truncated = true;
                break;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let own = entry.file_type().ok();
            let link = own.is_some_and(|kind| kind.is_symlink());
            let (kind, size) = if link {
                // A link is shown as what it leads to, so a linked directory
                // opens like one; one that leads nowhere is just a name.
                match fs::metadata(entry.path()) {
                    Ok(target) if target.is_dir() => (EntryKind::Directory, None),
                    Ok(target) if target.is_file() => (EntryKind::File, Some(target.len())),
                    _ => (EntryKind::Other, None),
                }
            } else {
                match own {
                    Some(kind) if kind.is_dir() => (EntryKind::Directory, None),
                    Some(kind) if kind.is_file() => (
                        EntryKind::File,
                        entry.metadata().ok().map(|metadata| metadata.len()),
                    ),
                    _ => (EntryKind::Other, None),
                }
            };
            entries.push(Entry {
                path: join(path, &name),
                name,
                kind,
                link,
                size,
            });
        }
        sort_entries(&mut entries);
        Ok(Listing {
            path: path.clone(),
            parent,
            windows: self.windows,
            entries,
            truncated,
        })
    }

    fn stat(&self, paths: &[String]) -> Result<Stats, String> {
        if paths.len() > MAX_STAT_PATHS {
            return Err(say!(
                self.english,
                "一次最多查询 {MAX_STAT_PATHS} 个路径",
                "At most {MAX_STAT_PATHS} paths per request"
            ));
        }
        let stats = paths
            .iter()
            .map(|raw| match self.place(raw) {
                Ok(Place::Drives) => Stat {
                    path: "/".to_owned(),
                    kind: Some(EntryKind::Directory),
                },
                Ok(Place::Path(path)) => Stat {
                    kind: fs::metadata(&path).ok().map(|metadata| kind_of(&metadata)),
                    path,
                },
                Err(_) => Stat {
                    path: raw.clone(),
                    kind: None,
                },
            })
            .collect();
        Ok(Stats {
            windows: self.windows,
            stats,
        })
    }

    /// A regular file, opened, with its normalized path and size.
    fn open_file(&self, raw: &str) -> Result<(File, String, u64), String> {
        let path = match self.place(raw)? {
            Place::Path(path) => path,
            Place::Drives => {
                return Err(say!(
                    self.english,
                    "无法读取目录：/",
                    "Cannot read a directory: /"
                ))
            }
        };
        // Checked before opening: opening a FIFO for reading waits for a writer.
        let metadata = fs::metadata(&path).map_err(|error| self.io_error(&path, &error))?;
        if metadata.is_dir() {
            return Err(say!(
                self.english,
                "无法读取目录：{path}",
                "Cannot read a directory: {path}"
            ));
        }
        if !metadata.is_file() {
            return Err(say!(
                self.english,
                "只能读取普通文件：{path}",
                "Only regular files can be read: {path}"
            ));
        }
        let file = File::open(&path).map_err(|error| self.io_error(&path, &error))?;
        let size = file
            .metadata()
            .map_err(|error| self.io_error(&path, &error))?
            .len();
        Ok((file, path, size))
    }

    fn read_text(&self, raw: &str) -> Result<TextFile, String> {
        let (file, path, size) = self.open_file(raw)?;
        let mut bytes = Vec::new();
        file.take(MAX_TEXT_BYTES)
            .read_to_end(&mut bytes)
            .map_err(|error| self.io_error(&path, &error))?;
        let binary = bytes.iter().take(BINARY_SNIFF_BYTES).any(|byte| *byte == 0);
        Ok(TextFile {
            content: if binary {
                String::new()
            } else {
                String::from_utf8_lossy(&bytes).into_owned()
            },
            truncated: size > MAX_TEXT_BYTES,
            size,
            binary,
            path,
        })
    }

    fn read_bytes(&self, raw: &str) -> Result<FileBytes, String> {
        let (file, path, size) = self.open_file(raw)?;
        if size > MAX_PREVIEW_BYTES {
            return Ok(FileBytes {
                path,
                data: String::new(),
                size,
                too_large: true,
            });
        }
        let mut bytes = Vec::new();
        // One byte past the cap: a file that grew since it was measured is no
        // longer the file that was, and is refused rather than cut.
        file.take(MAX_PREVIEW_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| self.io_error(&path, &error))?;
        if bytes.len() as u64 > MAX_PREVIEW_BYTES {
            return Ok(FileBytes {
                path,
                data: String::new(),
                size: bytes.len() as u64,
                too_large: true,
            });
        }
        Ok(FileBytes {
            data: base64(&bytes),
            size: bytes.len() as u64,
            too_large: false,
            path,
        })
    }

    fn search(&self, raw_root: &str, query: &str, limit: usize) -> Result<SearchResults, String> {
        let root = match self.place(raw_root)? {
            Place::Path(path) => path,
            Place::Drives => {
                return Err(say!(
                    self.english,
                    "不能在驱动器列表里搜索",
                    "The list of drives cannot be searched"
                ))
            }
        };
        let metadata = fs::metadata(&root).map_err(|error| self.io_error(&root, &error))?;
        if !metadata.is_dir() {
            return Err(say!(
                self.english,
                "{root} 不是目录",
                "{root} is not a directory"
            ));
        }
        Ok(search_directory(Path::new(&root), &root, query, limit))
    }

    fn rename(&self, raw: &str, name: &str) -> Result<Changed, String> {
        check_name(name, self.windows, self.english)?;
        let place = self.place(raw)?;
        let (Place::Path(path), Some(parent)) = (&place, parent_of(&place, self.windows)) else {
            return Err(say!(
                self.english,
                "不能重命名根目录",
                "The root cannot be renamed"
            ));
        };
        if self.windows && parent == "/" {
            return Err(say!(
                self.english,
                "不能重命名驱动器",
                "A drive cannot be renamed"
            ));
        }
        fs::symlink_metadata(path).map_err(|error| self.io_error(path, &error))?;
        let target = join(&parent, name);
        if &target == path {
            return Ok(Changed { path: target });
        }
        // A case-only rename on a filesystem that ignores case finds the file
        // itself under the new name; anything else there is a different file.
        let case_only = target.to_lowercase() == path.to_lowercase();
        if !case_only && fs::symlink_metadata(&target).is_ok() {
            return Err(say!(
                self.english,
                "{target} 已经存在",
                "{target} already exists"
            ));
        }
        fs::rename(path, &target).map_err(|error| self.io_error(path, &error))?;
        Ok(Changed { path: target })
    }

    fn remove(&self, raw: &str) -> Result<(), String> {
        let place = self.place(raw)?;
        let (Place::Path(path), Some(parent)) = (&place, parent_of(&place, self.windows)) else {
            return Err(say!(
                self.english,
                "不能删除根目录",
                "The root cannot be deleted"
            ));
        };
        if (self.windows && parent == "/") || self.home.as_deref() == Some(path.as_str()) {
            return Err(say!(
                self.english,
                "不能删除 {path}",
                "{path} cannot be deleted"
            ));
        }
        let metadata = fs::symlink_metadata(path).map_err(|error| self.io_error(path, &error))?;
        let result = if metadata.file_type().is_symlink() {
            // A link to a directory is a directory entry on Windows.
            fs::remove_file(path).or_else(|_| fs::remove_dir(path))
        } else if metadata.is_dir() {
            fs::remove_dir_all(path)
        } else {
            fs::remove_file(path)
        };
        result.map_err(|error| self.io_error(path, &error))
    }

    fn create_directory(&self, raw_parent: &str, name: &str) -> Result<Changed, String> {
        check_name(name, self.windows, self.english)?;
        let parent = match self.place(raw_parent)? {
            Place::Path(path) => path,
            Place::Drives => {
                return Err(say!(
                    self.english,
                    "不能在驱动器列表里新建目录",
                    "No directory can be made in the list of drives"
                ))
            }
        };
        let target = join(&parent, name);
        fs::create_dir(&target).map_err(|error| self.io_error(&target, &error))?;
        Ok(Changed { path: target })
    }
}

fn kind_of(metadata: &fs::Metadata) -> EntryKind {
    if metadata.is_dir() {
        EntryKind::Directory
    } else if metadata.is_file() {
        EntryKind::File
    } else {
        EntryKind::Other
    }
}

/// Directories first, then by name without case. Stable, so names equal
/// without case keep the order the directory gave them.
pub fn sort_entries(entries: &mut [Entry]) {
    entries.sort_by(|left, right| {
        let left_directory = left.kind == EntryKind::Directory;
        let right_directory = right.kind == EntryKind::Directory;
        right_directory
            .cmp(&left_directory)
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
    });
}

/// The drives a Windows machine has, as directory entries.
#[cfg(windows)]
fn drives() -> Vec<Entry> {
    // A and B are floppy letters; asking about an empty one can stall.
    ('C'..='Z')
        .filter(|letter| fs::metadata(format!("{letter}:\\")).is_ok())
        .map(|letter| Entry {
            name: format!("{letter}:"),
            path: format!("{letter}:/"),
            kind: EntryKind::Directory,
            link: false,
            size: None,
        })
        .collect()
}

#[cfg(not(windows))]
fn drives() -> Vec<Entry> {
    Vec::new()
}

/// Standard base64 with padding.
pub fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut text = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let triple = (u32::from(chunk[0]) << 16)
            | (u32::from(chunk.get(1).copied().unwrap_or(0)) << 8)
            | u32::from(chunk.get(2).copied().unwrap_or(0));
        text.push(ALPHABET[(triple >> 18) as usize & 63] as char);
        text.push(ALPHABET[(triple >> 12) as usize & 63] as char);
        text.push(if chunk.len() > 1 {
            ALPHABET[(triple >> 6) as usize & 63] as char
        } else {
            '='
        });
        text.push(if chunk.len() > 2 {
            ALPHABET[triple as usize & 63] as char
        } else {
            '='
        });
    }
    text
}

// ---------------------------------------------------------------------------
// Search
// ---------------------------------------------------------------------------

/// A budgeted fuzzy search for names under `root`.
///
/// The candidates are Git's own view of the checkout when `root` is in one —
/// tracked files and untracked ones Git does not ignore, directories derived
/// from their paths — and otherwise a walk of the directory that skips
/// [`SEARCH_SKIPPED_DIRECTORIES`] and never follows a link. Each candidate is
/// scored by [`score_candidate`].
pub fn search_directory(
    root: &Path,
    spelled_root: &str,
    query: &str,
    limit: usize,
) -> SearchResults {
    let trimmed = query.trim();
    let mut results = SearchResults {
        query: query.to_owned(),
        root: spelled_root.to_owned(),
        matches: Vec::new(),
        truncated: false,
    };
    if trimmed.is_empty() {
        return results;
    }
    let limit = limit.clamp(1, MAX_SEARCH_LIMIT);
    let query_chars = trimmed.chars().map(lower_char).collect::<Vec<_>>();

    let (mut matches, truncated) = match git_candidates(root) {
        Some((files, directories, truncated)) => {
            let mut matches = Vec::new();
            for relative in &files {
                push_candidate(&mut matches, &query_chars, relative, true);
            }
            let mut directories = directories.into_iter().collect::<Vec<_>>();
            directories.sort();
            for relative in &directories {
                push_candidate(&mut matches, &query_chars, relative, false);
            }
            (matches, truncated)
        }
        None => walk_candidates(root, &query_chars),
    };
    matches.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| left.path.to_lowercase().cmp(&right.path.to_lowercase()))
    });
    matches.truncate(limit);
    results.matches = matches;
    results.truncated = truncated;
    results
}

fn walk_candidates(root: &Path, query_chars: &[char]) -> (Vec<SearchMatch>, bool) {
    let deadline = Instant::now() + Duration::from_millis(MAX_SEARCH_MILLIS);
    let mut matches = Vec::new();
    let mut truncated = false;
    let mut visited = 0usize;
    let mut pending = vec![(root.to_path_buf(), String::new(), 0usize)];
    while let Some((directory, prefix, depth)) = pending.pop() {
        if depth >= MAX_SEARCH_DEPTH {
            truncated = true;
            continue;
        }
        if visited >= MAX_SEARCH_ENTRIES || Instant::now() >= deadline {
            truncated = true;
            break;
        }
        let Ok(entries) = fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries {
            visited += 1;
            if visited > MAX_SEARCH_ENTRIES || Instant::now() >= deadline {
                truncated = true;
                break;
            }
            let Ok(entry) = entry else { continue };
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_symlink() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let relative = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            if file_type.is_dir() {
                if SEARCH_SKIPPED_DIRECTORIES.contains(&name.as_str())
                    || name.starts_with("target-")
                {
                    continue;
                }
                push_candidate(&mut matches, query_chars, &relative, false);
                pending.push((directory.join(&name), relative, depth + 1));
            } else if file_type.is_file() {
                push_candidate(&mut matches, query_chars, &relative, true);
            }
        }
    }
    (matches, truncated)
}

/// `git ls-files --cached --others --exclude-standard -z` under `root`, or
/// `None` when Git is missing, `root` is not in a work tree, or Git fails, is
/// slow or says too much — every one of which falls back to the walk.
fn git_candidates(root: &Path) -> Option<(Vec<String>, HashSet<String>, bool)> {
    let git = find_git()?;
    let mut child = Command::new(git)
        .arg("--literal-pathspecs")
        .arg("-C")
        .arg(root)
        .args([
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
            "--",
            ".",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    // Read on a thread so a Git that stops talking cannot hold the deadline up.
    let reader = std::thread::spawn(move || {
        let mut buffer = Vec::new();
        let mut chunk = vec![0u8; 64 * 1024];
        loop {
            match stdout.read(&mut chunk) {
                Ok(0) => return Some(buffer),
                Ok(read) => {
                    buffer.extend_from_slice(&chunk[..read]);
                    if buffer.len() > SEARCH_GIT_MAX_OUTPUT {
                        return None;
                    }
                }
                Err(_) => return None,
            }
        }
    });
    // A reader that gave up on too much output stops draining the pipe, so Git
    // blocks on it and is killed at the deadline like a slow one.
    let deadline = Instant::now() + SEARCH_GIT_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    let buffer = reader.join().ok().flatten()?;
    if !status.success() {
        return None;
    }
    let mut files = Vec::new();
    let mut directories = HashSet::new();
    let mut seen = HashSet::new();
    let mut truncated = false;
    for raw in buffer.split(|&byte| byte == 0) {
        if raw.is_empty() {
            continue;
        }
        if files.len() >= MAX_SEARCH_ENTRIES {
            truncated = true;
            break;
        }
        let path = String::from_utf8_lossy(raw);
        let path = path.trim_start_matches("./");
        // `ls-files` speaks for the checkout: an absolute or climbing path is
        // not one of its own and is left out.
        if path.is_empty()
            || path.starts_with('/')
            || path.split('/').any(|part| part == ".." || part.is_empty())
        {
            continue;
        }
        let parts: Vec<&str> = path.split('/').collect();
        let mut prefix = String::new();
        for part in &parts[..parts.len() - 1] {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(part);
            directories.insert(prefix.clone());
        }
        if seen.insert(path.to_owned()) {
            files.push(path.to_owned());
        }
    }
    Some((files, directories, truncated))
}

/// The first `git` on `PATH`. On a Mac without the command line tools the one
/// in `/usr/bin` is a stand-in that asks to install them, so it is passed over
/// unless the tools are there.
fn find_git() -> Option<PathBuf> {
    let name = if cfg!(windows) { "git.exe" } else { "git" };
    let path = std::env::var_os("PATH")?;
    for directory in std::env::split_paths(&path) {
        let candidate = directory.join(name);
        if !candidate.is_file() {
            continue;
        }
        if cfg!(target_os = "macos")
            && candidate == Path::new("/usr/bin/git")
            && !Path::new("/Library/Developer/CommandLineTools/usr/bin/git").exists()
            && !Path::new("/Applications/Xcode.app").exists()
        {
            continue;
        }
        return Some(candidate);
    }
    None
}

fn push_candidate(
    matches: &mut Vec<SearchMatch>,
    query_chars: &[char],
    relative: &str,
    is_file: bool,
) {
    // Matched on a copy folded one character at a time, so the offsets still
    // index the original path.
    let path_chars = relative.chars().map(lower_char).collect::<Vec<_>>();
    let Some((positions, score)) = score_candidate(query_chars, &path_chars, is_file) else {
        return;
    };
    matches.push(SearchMatch {
        name: relative.rsplit('/').next().unwrap_or(relative).to_owned(),
        path: relative.to_owned(),
        kind: if is_file {
            EntryKind::File
        } else {
            EntryKind::Directory
        },
        positions,
        score,
    });
}

/// Scores one candidate, deterministically (the order and the tests depend on
/// it): from 0, each matched character in the base name is +8 and outside it
/// +2; the query as a contiguous case-folded substring of the base name +12,
/// and the base name starting with it +20 more; each unmatched character
/// between two matched ones −1; each level of depth −1, so shallower paths win
/// a tie; a file +3 over a directory. Saturating, never below 0. The positions
/// are one greedy left-to-right subsequence scan, not an optimal alignment.
pub fn score_candidate(query: &[char], path: &[char], is_file: bool) -> Option<(Vec<u32>, i32)> {
    let mut positions = Vec::with_capacity(query.len());
    let mut from = 0usize;
    for &wanted in query {
        let found = path[from..]
            .iter()
            .position(|&character| character == wanted)?
            + from;
        positions.push(found as u32);
        from = found + 1;
    }
    let basename_start = path
        .iter()
        .rposition(|&character| character == '/')
        .map_or(0, |index| index + 1);
    let basename = &path[basename_start..];
    let mut score = 0i32;
    for &index in &positions {
        score = score.saturating_add(if index as usize >= basename_start {
            8
        } else {
            2
        });
    }
    for pair in positions.windows(2) {
        score = score.saturating_sub((pair[1] - pair[0] - 1) as i32);
    }
    if basename.windows(query.len()).any(|window| window == query) {
        score = score.saturating_add(12);
    }
    if basename.starts_with(query) {
        score = score.saturating_add(20);
    }
    let depth = path.iter().filter(|&&character| character == '/').count() + 1;
    score = score.saturating_sub(depth as i32);
    if is_file {
        score = score.saturating_add(3);
    }
    Some((positions, score.max(0)))
}

/// The first character of `character`'s lower case, so folding keeps the
/// character count and offsets still line up with the original.
pub fn lower_char(character: char) -> char {
    character.to_lowercase().next().unwrap_or(character)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn posix(raw: &str) -> Result<Place, String> {
        normalize(raw, false, Some("/home/dev"), true)
    }

    fn windows(raw: &str) -> Result<Place, String> {
        normalize(raw, true, Some("C:\\Users\\dev"), true)
    }

    fn path(place: Result<Place, String>) -> String {
        match place.expect("normalizes") {
            Place::Path(path) => path,
            Place::Drives => "<drives>".to_owned(),
        }
    }

    #[test]
    fn posix_paths_are_absolute_and_lexical() {
        assert_eq!(path(posix("/srv//app/./src/../lib/")), "/srv/app/lib");
        assert_eq!(path(posix("/..")), "/");
        assert_eq!(path(posix("/")), "/");
        assert_eq!(path(posix("~")), "/home/dev");
        assert_eq!(path(posix("")), "/home/dev");
        assert_eq!(path(posix("~/notes/")), "/home/dev/notes");
        // A backslash is an ordinary character in a POSIX name.
        assert_eq!(path(posix("/srv/a\\b")), "/srv/a\\b");
        assert!(posix("relative/path").is_err());
        assert!(posix("~root/x").is_err());
        assert!(posix("/srv/\nx").is_err());
    }

    #[test]
    fn windows_paths_take_a_drive_and_forward_slashes() {
        assert_eq!(
            path(windows("c:\\Users\\dev\\..\\Public")),
            "C:/Users/Public"
        );
        assert_eq!(path(windows("C:")), "C:/");
        assert_eq!(path(windows("C:/")), "C:/");
        assert_eq!(path(windows("D:/..")), "D:/");
        assert_eq!(path(windows("~\\Desktop")), "C:/Users/dev/Desktop");
        assert_eq!(path(windows("/")), "<drives>");
        assert_eq!(path(windows("\\")), "<drives>");
        assert!(windows("\\\\server\\share").is_err());
        assert!(windows("//server/share").is_err());
        assert!(windows("/Users").is_err());
        assert!(windows("C:relative").is_err());
        assert!(windows("relative").is_err());
    }

    #[test]
    fn up_leads_to_the_machine_top_and_then_nowhere() {
        let up = |place: Result<Place, String>, windows: bool| {
            parent_of(&place.expect("normalizes"), windows)
        };
        assert_eq!(up(posix("/srv/app"), false).as_deref(), Some("/srv"));
        assert_eq!(up(posix("/srv"), false).as_deref(), Some("/"));
        assert_eq!(up(posix("/"), false), None);
        assert_eq!(
            up(windows("C:/Users/dev"), true).as_deref(),
            Some("C:/Users")
        );
        assert_eq!(up(windows("C:/Users"), true).as_deref(), Some("C:/"));
        assert_eq!(up(windows("C:/"), true).as_deref(), Some("/"));
        assert_eq!(up(windows("/"), true), None);
    }

    #[test]
    fn names_are_one_segment() {
        for name in ["", ".", "..", "a/b", "a\u{0}b"] {
            assert!(check_name(name, false, true).is_err(), "{name:?}");
        }
        assert!(check_name("a\\b", false, true).is_ok());
        for name in ["a\\b", "a:b", "trailing.", "trailing ", "what?"] {
            assert!(check_name(name, true, true).is_err(), "{name:?}");
        }
        assert!(check_name("新建文件夹", true, true).is_ok());
    }

    #[test]
    fn base64_matches_the_standard_alphabet() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64(&[0xff, 0xfe, 0x00]), "//4A");
    }

    fn request(op: FilesOp) -> serde_json::Value {
        match handle(FilesRequest { op, english: true }) {
            FilesReply::Ok(value) => value,
            FilesReply::Err(message) => panic!("{message}"),
        }
    }

    fn failure(op: FilesOp) -> String {
        match handle(FilesRequest { op, english: true }) {
            FilesReply::Ok(value) => panic!("expected a failure, got {value}"),
            FilesReply::Err(message) => message,
        }
    }

    fn spelled(path: &Path) -> String {
        let text = path.to_string_lossy().into_owned();
        if cfg!(windows) {
            text.replace('\\', "/")
        } else {
            text
        }
    }

    #[test]
    fn lists_directories_first_and_follows_links() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let root = spelled(directory.path());
        fs::write(directory.path().join("b.txt"), "b").expect("write");
        fs::write(directory.path().join("A.md"), "a").expect("write");
        fs::create_dir(directory.path().join("src")).expect("mkdir");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(
                directory.path().join("src"),
                directory.path().join("linked"),
            )
            .expect("link");
            std::os::unix::fs::symlink(
                directory.path().join("gone"),
                directory.path().join("broken"),
            )
            .expect("link");
        }
        let listing: Listing = serde_json::from_value(request(FilesOp::List {
            path: format!("{root}/./"),
        }))
        .expect("listing");
        assert_eq!(listing.path, root);
        let names: Vec<_> = listing
            .entries
            .iter()
            .map(|entry| (entry.name.as_str(), entry.kind, entry.link))
            .collect();
        #[cfg(unix)]
        assert_eq!(
            names,
            [
                ("linked", EntryKind::Directory, true),
                ("src", EntryKind::Directory, false),
                ("A.md", EntryKind::File, false),
                ("b.txt", EntryKind::File, false),
                ("broken", EntryKind::Other, true),
            ]
        );
        #[cfg(not(unix))]
        assert_eq!(
            names,
            [
                ("src", EntryKind::Directory, false),
                ("A.md", EntryKind::File, false),
                ("b.txt", EntryKind::File, false),
            ]
        );
        assert_eq!(
            listing
                .entries
                .iter()
                .find(|entry| entry.name == "A.md")
                .map(|entry| entry.path.clone()),
            Some(format!("{root}/A.md"))
        );
        assert_eq!(
            listing
                .entries
                .iter()
                .find(|entry| entry.name == "b.txt")
                .and_then(|entry| entry.size),
            Some(1)
        );
        assert!(failure(FilesOp::List {
            path: format!("{root}/b.txt")
        })
        .contains("is not a directory"));
        assert!(failure(FilesOp::List {
            path: format!("{root}/missing")
        })
        .contains("does not exist"));
    }

    #[test]
    fn stats_say_what_is_there() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let root = spelled(directory.path());
        fs::write(directory.path().join("file"), "x").expect("write");
        let stats: Stats = serde_json::from_value(request(FilesOp::Stat {
            paths: vec![
                format!("{root}/file"),
                root.clone(),
                format!("{root}/missing"),
                "relative".into(),
            ],
        }))
        .expect("stats");
        let kinds: Vec<_> = stats.stats.iter().map(|stat| stat.kind).collect();
        assert_eq!(
            kinds,
            [
                Some(EntryKind::File),
                Some(EntryKind::Directory),
                None,
                None
            ]
        );
        assert_eq!(stats.stats[0].path, format!("{root}/file"));
    }

    #[test]
    fn reads_text_up_to_the_cap_and_spots_binary() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let root = spelled(directory.path());
        fs::write(directory.path().join("text.txt"), "héllo\n").expect("write");
        fs::write(directory.path().join("blob.bin"), [0u8, 1, 2]).expect("write");
        let text: TextFile = serde_json::from_value(request(FilesOp::ReadText {
            path: format!("{root}/text.txt"),
        }))
        .expect("text");
        assert_eq!(
            (text.content.as_str(), text.binary, text.truncated),
            ("héllo\n", false, false)
        );
        let blob: TextFile = serde_json::from_value(request(FilesOp::ReadText {
            path: format!("{root}/blob.bin"),
        }))
        .expect("blob");
        assert!(blob.binary && blob.content.is_empty());
        let bytes: FileBytes = serde_json::from_value(request(FilesOp::ReadBytes {
            path: format!("{root}/blob.bin"),
        }))
        .expect("bytes");
        assert_eq!(
            (bytes.data.as_str(), bytes.size, bytes.too_large),
            ("AAEC", 3, false)
        );
        assert!(
            failure(FilesOp::ReadText { path: root.clone() }).contains("Cannot read a directory")
        );
    }

    #[test]
    fn renames_creates_and_removes() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let root = spelled(directory.path());
        fs::write(directory.path().join("old.txt"), "x").expect("write");
        fs::write(directory.path().join("taken.txt"), "y").expect("write");
        let renamed: Changed = serde_json::from_value(request(FilesOp::Rename {
            path: format!("{root}/old.txt"),
            name: "new.txt".into(),
        }))
        .expect("rename");
        assert_eq!(renamed.path, format!("{root}/new.txt"));
        assert!(directory.path().join("new.txt").exists());
        assert!(failure(FilesOp::Rename {
            path: format!("{root}/new.txt"),
            name: "taken.txt".into()
        })
        .contains("already exists"));
        assert!(failure(FilesOp::Rename {
            path: format!("{root}/new.txt"),
            name: "../escape".into()
        })
        .contains("Invalid name"));
        // A case-only rename is a rename, whatever the filesystem folds.
        let cased: Changed = serde_json::from_value(request(FilesOp::Rename {
            path: format!("{root}/new.txt"),
            name: "NEW.txt".into(),
        }))
        .expect("rename");
        assert_eq!(cased.path, format!("{root}/NEW.txt"));

        let made: Changed = serde_json::from_value(request(FilesOp::CreateDirectory {
            parent: root.clone(),
            name: "made".into(),
        }))
        .expect("mkdir");
        assert_eq!(made.path, format!("{root}/made"));
        fs::write(directory.path().join("made/inner.txt"), "z").expect("write");
        assert!(failure(FilesOp::CreateDirectory {
            parent: root.clone(),
            name: "made".into()
        })
        .contains("already exists"));
        request(FilesOp::Remove {
            path: format!("{root}/made"),
        });
        assert!(!directory.path().join("made").exists());
        request(FilesOp::Remove {
            path: format!("{root}/NEW.txt"),
        });
        assert!(!directory.path().join("NEW.txt").exists());
        assert!(failure(FilesOp::Remove { path: "/".into() }).contains("cannot be deleted"));
    }

    #[test]
    fn searches_by_name_and_skips_dependencies() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let root = spelled(directory.path());
        fs::create_dir_all(directory.path().join("src/components")).expect("mkdir");
        fs::create_dir_all(directory.path().join("node_modules/pane")).expect("mkdir");
        fs::write(directory.path().join("src/components/FilesPane.tsx"), "").expect("write");
        fs::write(directory.path().join("node_modules/pane/FilesPane.js"), "").expect("write");
        let results: SearchResults = serde_json::from_value(request(FilesOp::Search {
            root: root.clone(),
            query: "filespane".into(),
            limit: 10,
        }))
        .expect("search");
        let paths: Vec<_> = results
            .matches
            .iter()
            .map(|found| found.path.as_str())
            .collect();
        assert_eq!(paths, ["src/components/FilesPane.tsx"]);
        assert_eq!(results.matches[0].positions.len(), "filespane".len());
        assert_eq!(results.root, root);
    }

    #[test]
    fn a_request_round_trips_as_the_helper_reads_it() {
        let encoded = serde_json::to_string(&FilesRequest {
            op: FilesOp::Search {
                root: "~".into(),
                query: "x".into(),
                limit: 5,
            },
            english: true,
        })
        .expect("encode");
        assert_eq!(
            encoded,
            r#"{"op":{"op":"search","root":"~","query":"x","limit":5},"english":true}"#
        );
        let reply = serde_json::to_string(&FilesReply::Err("nope".into())).expect("encode");
        assert_eq!(reply, r#"{"err":"nope"}"#);
    }
}
