use std::{
    collections::HashSet,
    fs::{self, File},
    io::Read,
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use serde::Serialize;
use wait_timeout::ChildExt;

use crate::{host_platform::host_platform, path_guard};

const MAX_WORKSPACE_FILE_BYTES: u64 = 1_048_576;
/// 图片预览整份过 IPC，base64 还要再涨 4/3，所以这道闸比文本那道低得多也严得多：
/// 超了就不给数据，而不是给半份。
const MAX_WORKSPACE_PREVIEW_BYTES: u64 = 8 * 1_048_576;
const BINARY_SNIFF_BYTES: usize = 8 * 1024;

const ENTRY_KIND_DIRECTORY: &str = "directory";
const ENTRY_KIND_FILE: &str = "file";
const ENTRY_KIND_SYMLINK: &str = "symlink";
const ENTRY_KIND_OTHER: &str = "other";

// 搜索是为文件面板准备的便利功能，不是授权面：预算触顶就停并报告截断，
// 绝不为完整性放宽上述任一限制。
const MAX_SEARCH_ENTRIES: usize = 20_000;
const MAX_SEARCH_DEPTH: usize = 12;
const MAX_SEARCH_MILLIS: u64 = 2_000;
const MAX_SEARCH_LIMIT: usize = 200;
// `git ls-files` 是一条只读快查询；超时或失败一律回退到目录遍历。
const SEARCH_GIT_TIMEOUT: Duration = Duration::from_secs(5);
const SEARCH_GIT_MAX_OUTPUT: usize = 8 * 1024 * 1024;

/// 与 `list_directory` 不同，这里不递归展示全部内容：这些目录要么是版本控制
/// 内部数据，要么是依赖与构建产物，数量足以吃光预算且从未是搜索的目标。
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

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceEntry {
    pub name: String,
    pub kind: String,
    pub size: Option<u64>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceDirectoryListing {
    pub path: String,
    pub entries: Vec<WorkspaceEntry>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceFileContent {
    pub path: String,
    pub content: String,
    pub truncated: bool,
    pub size: u64,
    pub binary: bool,
}

/// 一个文件的原始字节，供文件面板预览图片之用。图片没有「只看开头」这一说，
/// 所以超过预览上限时不返回半截数据：`data` 留空、`too_large` 置位，由界面
/// 说明为什么这张图不显示。
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceFileBytes {
    pub path: String,
    /// 标准 base64；`too_large` 为真时为空串。
    pub data: String,
    pub size: u64,
    pub too_large: bool,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceSearchMatch {
    pub name: String,
    pub path: String,
    pub kind: String,
    pub positions: Vec<u32>,
    pub score: i32,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceSearchResults {
    pub query: String,
    pub matches: Vec<WorkspaceSearchMatch>,
    pub truncated: bool,
}

pub fn list_directory(
    workspace: &Path,
    relative_path: &str,
) -> Result<WorkspaceDirectoryListing, String> {
    let root = workspace_root(workspace)?;
    let resolved = resolve(&root, relative_path)?;
    let path = workspace_relative(&root, &resolved)?;

    // The kind check runs on the path so the refusal names the request; the
    // listing itself must not trust it, so the handle below re-checks it.
    let metadata = fs::metadata(&resolved)
        .map_err(|error| format!("文件浏览无法读取目录信息 {path}: {error}"))?;
    if !metadata.is_dir() {
        return Err(format!("文件浏览无法列出非目录路径: {path}"));
    }

    // A canonical path is not an authorization: between the check above and
    // `read_dir` another process can rename the directory away and leave a
    // symlink or junction to an outside directory under the same name. The
    // verified handle stays open across the whole listing so the entries come
    // from the object that passed the check.
    let handle = open_verified_directory(&resolved, &path)?;

    let mut entries = Vec::new();
    let directory =
        fs::read_dir(&resolved).map_err(|error| format!("文件浏览无法列出目录 {path}: {error}"))?;
    for entry in directory {
        let entry = entry.map_err(|error| format!("文件浏览无法读取目录项 {path}: {error}"))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        // `file_type` and `DirEntry::metadata` keep a link visible as a link
        // instead of silently reporting the kind and size of its target.
        let (kind, size) = match entry.file_type() {
            Ok(file_type) if file_type.is_symlink() => (ENTRY_KIND_SYMLINK, None),
            Ok(file_type) if file_type.is_dir() => (ENTRY_KIND_DIRECTORY, None),
            Ok(file_type) if file_type.is_file() => (
                ENTRY_KIND_FILE,
                entry.metadata().ok().map(|metadata| metadata.len()),
            ),
            Ok(_) => (ENTRY_KIND_OTHER, None),
            Err(_) => (ENTRY_KIND_OTHER, None),
        };
        entries.push(WorkspaceEntry {
            name,
            kind: kind.to_owned(),
            size,
        });
    }
    verify_directory_unchanged(&handle, &resolved, &path)?;

    entries.sort_by(|left, right| {
        let left_is_directory = left.kind == ENTRY_KIND_DIRECTORY;
        let right_is_directory = right.kind == ENTRY_KIND_DIRECTORY;
        right_is_directory
            .cmp(&left_is_directory)
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
    });

    Ok(WorkspaceDirectoryListing { path, entries })
}

pub fn read_file(workspace: &Path, relative_path: &str) -> Result<WorkspaceFileContent, String> {
    let (file, path, size) = open_readable_file(workspace, relative_path)?;

    let mut bytes = Vec::new();
    file.take(MAX_WORKSPACE_FILE_BYTES)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("文件浏览无法读取文件 {path}: {error}"))?;

    let binary = bytes.iter().take(BINARY_SNIFF_BYTES).any(|byte| *byte == 0);
    let content = if binary {
        String::new()
    } else {
        String::from_utf8_lossy(&bytes).into_owned()
    };

    Ok(WorkspaceFileContent {
        path,
        content,
        truncated: size > MAX_WORKSPACE_FILE_BYTES,
        size,
        binary,
    })
}

/// 读取整个文件并以 base64 交回。只有图片预览用得上：文本走 `read_file`，
/// 它按 1 MiB 截断并在末尾说明，而一张截断的图片解不出来。
pub fn read_file_bytes(workspace: &Path, relative_path: &str) -> Result<WorkspaceFileBytes, String> {
    let (file, path, size) = open_readable_file(workspace, relative_path)?;
    if size > MAX_WORKSPACE_PREVIEW_BYTES {
        return Ok(WorkspaceFileBytes {
            path,
            data: String::new(),
            size,
            too_large: true,
        });
    }

    let mut bytes = Vec::new();
    // 上限之外多留一字节：文件在 metadata 与读取之间长大时读到的会比上限多，
    // 那说明这份数据已经不是刚才那份，按超限处理而不是交回半截。
    file.take(MAX_WORKSPACE_PREVIEW_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("文件浏览无法读取文件 {path}: {error}"))?;
    if bytes.len() as u64 > MAX_WORKSPACE_PREVIEW_BYTES {
        return Ok(WorkspaceFileBytes {
            path,
            data: String::new(),
            size: bytes.len() as u64,
            too_large: true,
        });
    }

    Ok(WorkspaceFileBytes {
        data: base64::engine::Engine::encode(&base64::engine::general_purpose::STANDARD, &bytes),
        size: bytes.len() as u64,
        too_large: false,
        path,
    })
}

/// 打开一个通过范围校验的普通文件，交回句柄、工作区相对路径与当前大小。
fn open_readable_file(workspace: &Path, relative_path: &str) -> Result<(File, String, u64), String> {
    let root = workspace_root(workspace)?;
    let resolved = resolve(&root, relative_path)?;
    let path = workspace_relative(&root, &resolved)?;

    // The kind checks run on the path so the refusal names the request; the
    // guarded open below is what the content is actually read from.
    let metadata = fs::metadata(&resolved)
        .map_err(|error| format!("文件浏览无法读取文件信息 {path}: {error}"))?;
    if metadata.is_dir() {
        return Err(format!("文件浏览无法读取目录: {path}"));
    }
    if !metadata.is_file() {
        return Err(format!("文件浏览只能读取普通文件: {path}"));
    }

    // Binds the read to the authorized filesystem object: the helper verifies
    // the opened handle's own final path against the canonical path that passed
    // the scope check, so replacing a directory on the way with a symlink or
    // junction after the check fails closed instead of returning outside
    // content under an inside path.
    let (file, _) = path_guard::secure_open_existing_file_with_scope(
        &root,
        &guarded_request(&root, relative_path)?,
        &path_guard::ExecutionScope::workspace_only(&root),
    )
    .map_err(|error| format!("文件浏览无法打开文件 {path}: {error}"))?;
    let size = file
        .metadata()
        .map_err(|error| format!("文件浏览无法读取文件信息 {path}: {error}"))?
        .len();
    Ok((file, path, size))
}

/// 对工作区做一次预算受限的文件名搜索：候选是文件与目录，按查询做大小写
/// 不敏感的子序列匹配。首选 `git ls-files` 给出的文件视图（tracked 加未被
/// 忽略的 untracked），目录候选从文件路径前缀推导；Git 不可用或命令失败时
/// 回退到递归目录遍历。遍历沿用 `list_directory` 的验证句柄方案，单个目录
/// 失败只跳过该目录；只有根解析失败才是硬错误。符号链接既不进结果也不
/// 下钻，与 `read_file` 拒绝链接一致。
pub fn search_files(
    workspace: &Path,
    query: &str,
    limit: usize,
) -> Result<WorkspaceSearchResults, String> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return Ok(WorkspaceSearchResults {
            query: query.to_owned(),
            matches: Vec::new(),
            truncated: false,
        });
    }
    let limit = limit.clamp(1, MAX_SEARCH_LIMIT);

    let root = workspace_root(workspace)?;
    let query_chars = trimmed.chars().map(lower_char).collect::<Vec<_>>();
    let deadline = Instant::now() + Duration::from_millis(MAX_SEARCH_MILLIS);

    // 首选 Git 自己的文件视图：tracked + 未被忽略的 untracked，一条命令给出
    // 全部文件候选，目录候选从路径前缀推导。任何失败（Git 缺失、不是仓库、
    // 超时、输出异常）都回退到目录遍历而不是报错。
    if let Some(candidates) = collect_git_search_candidates(&root) {
        let matches = matches_from_git_candidates(&candidates, &query_chars);
        return Ok(finish_search(query, matches, candidates.truncated, limit));
    }

    // 回退：预算受限的递归遍历。
    let mut matches = Vec::new();
    let mut truncated = false;
    let mut visited = 0usize;
    // 待读目录队列：(已解析路径, 工作区相对前缀, 目录深度)。根深度为 0。
    let mut pending = vec![(root, String::new(), 0usize)];

    while let Some((resolved, prefix, depth)) = pending.pop() {
        // 深度预算针对候选路径：已到上限的目录不再读取，其子路径必然超限。
        if depth >= MAX_SEARCH_DEPTH {
            truncated = true;
            continue;
        }
        if visited >= MAX_SEARCH_ENTRIES || Instant::now() >= deadline {
            truncated = true;
            break;
        }
        let display = if prefix.is_empty() {
            "."
        } else {
            prefix.as_str()
        };
        let Ok(handle) = open_verified_directory(&resolved, display) else {
            continue;
        };
        let Ok(directory) = fs::read_dir(&resolved) else {
            continue;
        };

        // 条目先收齐、验证句柄之后再处理：目录在读取期间被替换，就丢弃
        // 它的全部条目而不是让外来对象的内容混进结果。
        let mut entries = Vec::new();
        let mut budget_exhausted = false;
        for entry in directory {
            visited += 1;
            if visited > MAX_SEARCH_ENTRIES || Instant::now() >= deadline {
                budget_exhausted = true;
                break;
            }
            let Ok(entry) = entry else { continue };
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            entries.push((file_type, entry.file_name().to_string_lossy().into_owned()));
        }
        if verify_directory_unchanged(&handle, &resolved, display).is_err() {
            continue;
        }
        if budget_exhausted {
            truncated = true;
            break;
        }

        for (file_type, name) in entries {
            // 符号链接既不作为候选也不作为下钻入口。
            if file_type.is_symlink() {
                continue;
            }
            let relative = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            if file_type.is_dir() {
                if SEARCH_SKIPPED_DIRECTORIES.contains(&name.as_str())
                    // Cargo 的 target 目录常带后缀（target-cache 等），按
                    // 前缀一并跳过。
                    || name.starts_with("target-")
                {
                    continue;
                }
                push_candidate(&mut matches, &query_chars, &relative, false);
                pending.push((resolved.join(&name), relative, depth + 1));
            } else if file_type.is_file() {
                push_candidate(&mut matches, &query_chars, &relative, true);
            }
        }
    }

    matches.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| left.path.to_lowercase().cmp(&right.path.to_lowercase()))
    });
    matches.truncate(limit);

    Ok(WorkspaceSearchResults {
        query: query.to_owned(),
        matches,
        truncated,
    })
}

/// 结果收尾：排序、截断到 limit。排序规则在 Git 候选与遍历回退两条路径上
/// 保持一致，评分本身不变。
fn finish_search(
    query: &str,
    mut matches: Vec<WorkspaceSearchMatch>,
    truncated: bool,
    limit: usize,
) -> WorkspaceSearchResults {
    matches.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| left.path.to_lowercase().cmp(&right.path.to_lowercase()))
    });
    matches.truncate(limit);
    WorkspaceSearchResults {
        query: query.to_owned(),
        matches,
        truncated,
    }
}

/// Git 候选收集的结果：文件条目是从 `ls-files` 直接得到的工作区相对路径，
/// 目录条目是从文件路径前缀推导出的可匹配目录。任何一条都还没做存在性
/// 校验之外的安全归一化。
struct GitCandidates {
    files: Vec<String>,
    directories: HashSet<String>,
    truncated: bool,
}

/// 用 `git ls-files --cached --others --exclude-standard -z` 列出仓库内
/// tracked 与未忽略 untracked 的文件；不是仓库或命令失败则返回 `None`，
/// 让调用方回退到目录遍历。命令直接 spawn 且只读，不取 git.rs 的
/// repository 锁；超时、输出超帽或退出失败一律按失败回退。
fn collect_git_search_candidates(root: &Path) -> Option<GitCandidates> {
    // Resolved here instead of by the OS's own PATH lookup: on a Mac without
    // the command line tools the first `git` on PATH can be the `/usr/bin`
    // stand-in, which opens the install dialog on every search. Skipping it
    // still finds a real git further down PATH, and with none the search walks
    // the directory as it does wherever git is missing. Windows keeps the
    // lookup it always had.
    let git = if host_platform().is_windows() {
        PathBuf::from("git")
    } else {
        crate::environment_tools::resolve_on_path("git")?
    };
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
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .spawn()
        .ok()?;

    let mut stdout = child.stdout.take()?;
    let mut buffer = Vec::new();
    let deadline = Instant::now() + SEARCH_GIT_TIMEOUT;
    let mut chunk = vec![0u8; 64 * 1024];
    loop {
        match stdout.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => {
                buffer.extend_from_slice(&chunk[..read]);
                if buffer.len() > SEARCH_GIT_MAX_OUTPUT {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
    }
    // 输出读尽后仍要等进程退出；迟迟不退出按超时失败处理。
    let remaining = deadline.saturating_duration_since(Instant::now());
    match child.wait_timeout(remaining.max(Duration::from_millis(100))) {
        Ok(Some(status)) if status.success() => {}
        _ => {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
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
        // `-z` 输出以 NUL 分隔，空格与非 UTF-8 文件名都原样保留。
        let path = String::from_utf8_lossy(raw);
        let path = path.trim_start_matches("./");
        // 目录候选从文件路径的每一段前缀推导并去重，目录本身才能被匹配。
        // 先记下前缀（含安全检查），文件本身最后入列。
        let Some(relative) = safe_workspace_relative(root, path) else {
            continue;
        };
        let mut prefix = String::new();
        let parts: Vec<&str> = relative.split('/').collect();
        for part in &parts[..parts.len() - 1] {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(part);
            directories.insert(prefix.clone());
        }
        if seen.insert(relative.clone()) {
            files.push(relative);
        }
    }
    Some(GitCandidates {
        files,
        directories,
        truncated,
    })
}

/// 校验一条来自 `git ls-files` 的仓库相对路径（不可信输入）：拒绝绝对路径
/// 与含 `..` 组件的路径，再经文件系统归一化确认仍落在工作区内。通过后返回
/// 以 `/` 分隔的工作区相对形式。注意检查作用于原始字符串：`root.join` 之后
/// 绝对根自身的 Prefix/RootDir 组件会污染判断。
fn safe_workspace_relative(root: &Path, raw: &str) -> Option<String> {
    if raw.is_empty()
        || Path::new(raw).components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return None;
    }
    // 归一化后必须仍以工作区根开头；这同时剔除了已删除或不存在的条目。
    let canonical = fs::canonicalize(root.join(raw)).ok()?;
    let stripped = canonical.strip_prefix(root).ok()?;
    let relative = stripped
        .components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/");
    if relative.is_empty() {
        None
    } else {
        Some(relative)
    }
}

/// 把 Git 候选过一遍评分。评分规则与遍历回退完全一致，全部委托给
/// `push_candidate`。
fn matches_from_git_candidates(
    candidates: &GitCandidates,
    query_chars: &[char],
) -> Vec<WorkspaceSearchMatch> {
    let mut matches = Vec::new();
    for relative in &candidates.files {
        push_candidate(&mut matches, query_chars, relative, true);
    }
    let mut directories = candidates.directories.iter().collect::<Vec<_>>();
    directories.sort();
    for relative in directories {
        push_candidate(&mut matches, query_chars, relative, false);
    }
    matches
}

fn push_candidate(
    matches: &mut Vec<WorkspaceSearchMatch>,
    query_chars: &[char],
    relative: &str,
    is_file: bool,
) {
    // 匹配在逐字符取小写、字符数不变的副本上做：查询已经折过大小写，
    // 路径不折的话 `README.md` 这类含大写的路径永远匹配不到。位置下标
    // 因为字符数不变而仍然对齐原路径。
    let path_chars = relative.chars().map(lower_char).collect::<Vec<_>>();
    let Some((positions, score)) = score_candidate(query_chars, &path_chars, is_file) else {
        return;
    };
    let name = relative.rsplit('/').next().unwrap_or(relative).to_owned();
    matches.push(WorkspaceSearchMatch {
        name,
        path: relative.to_owned(),
        kind: if is_file {
            ENTRY_KIND_FILE
        } else {
            ENTRY_KIND_DIRECTORY
        }
        .to_owned(),
        positions,
        score,
    });
}

/// 计分规则是确定性的（排序与测试都依赖它）：从 0 起，基名内的每个匹配
/// 字符 +8、基名外 +2；查询整体作为连续大小写不敏感子串出现在基名 +12、
/// 基名以查询开头再加 +20；相邻匹配之间的未匹配字符每个 −1；路径每深一层
/// −1 作浅层偏好；同等条件下文件比目录 +3。全程饱和运算，下限 0。匹配
/// 位置取一次从左到右的贪婪子序列扫描，不做最优对齐；位置是相对路径的
/// 字符下标，供前端加粗。
fn score_candidate(query: &[char], path: &[char], is_file: bool) -> Option<(Vec<u32>, i32)> {
    let mut positions = Vec::with_capacity(query.len());
    let mut from = 0usize;
    for &wanted in query {
        let found = path[from..].iter().position(|&c| c == wanted)? + from;
        positions.push(found as u32);
        from = found + 1;
    }

    let basename_start = path
        .iter()
        .rposition(|&c| c == '/')
        .map_or(0, |index| index + 1);
    let basename = &path[basename_start..];

    let mut score = 0i32;
    for &index in &positions {
        let bonus = if index as usize >= basename_start {
            8
        } else {
            2
        };
        score = score.saturating_add(bonus);
    }
    for pair in positions.windows(2) {
        let gap = pair[1] - pair[0] - 1;
        score = score.saturating_sub(gap as i32);
    }
    if contains_subslice(basename, query) {
        score = score.saturating_add(12);
    }
    if basename.starts_with(query) {
        score = score.saturating_add(20);
    }
    let depth = path.iter().filter(|&&c| c == '/').count() + 1;
    score = score.saturating_sub(depth as i32);
    if is_file {
        score = score.saturating_add(3);
    }
    Some((positions, score.max(0)))
}

/// 逐字符取小写首形，保持字符数不变，字符下标才能与原路径对齐。
fn lower_char(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

fn contains_subslice(haystack: &[char], needle: &[char]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn workspace_root(workspace: &Path) -> Result<PathBuf, String> {
    path_guard::canonical_workspace(workspace)
        .map_err(|error| format!("文件浏览无法访问工作区: {error}"))
}

/// The empty request and `.` address the workspace itself, which the path guard
/// rejects as an empty path, so they resolve to the already-canonical root.
fn resolve(root: &Path, relative_path: &str) -> Result<PathBuf, String> {
    if is_root_request(relative_path) {
        return Ok(root.to_path_buf());
    }
    path_guard::resolve_existing(root, &guarded_request(root, relative_path)?)
        .map_err(|error| format!("文件浏览无法访问路径 {relative_path}: {error}"))
}

fn is_root_request(relative_path: &str) -> bool {
    relative_path.is_empty() || relative_path == "."
}

/// Spells the request as an absolute path because `path_guard` trims its
/// argument: a listed name whose first or last character is a space would
/// otherwise address a neighbouring file, and leading spaces are legal names on
/// every supported platform. An absolute path never starts with whitespace, and
/// a trailing `.` component — dropped again while the guard normalises the
/// path — keeps the string from ending in the name's own whitespace.
fn guarded_request(root: &Path, relative_path: &str) -> Result<String, String> {
    let joined = root.join(relative_path);
    let mut request = joined
        .to_str()
        .ok_or_else(|| format!("文件浏览无法处理该路径: {relative_path}"))?
        .to_owned();
    if request.ends_with(char::is_whitespace) {
        request.push(std::path::MAIN_SEPARATOR);
        request.push('.');
    }
    Ok(request)
}

#[cfg(windows)]
fn open_verified_directory(resolved: &Path, path: &str) -> Result<File, String> {
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_SHARE_READ, FILE_SHARE_WRITE,
    };

    let handle = fs::OpenOptions::new()
        .read(true)
        // Withholding delete sharing pins the checked name for as long as this
        // handle lives, so it cannot be renamed away and replaced mid-listing.
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        // Open a final reparse point itself instead of following it.
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(resolved)
        .map_err(|error| format!("文件浏览无法打开目录 {path}: {error}"))?;
    let metadata = handle
        .metadata()
        .map_err(|error| format!("文件浏览无法读取目录信息 {path}: {error}"))?;
    if !metadata.is_dir() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(format!("文件浏览无法列出非目录路径: {path}"));
    }
    Ok(handle)
}

#[cfg(unix)]
fn open_verified_directory(resolved: &Path, path: &str) -> Result<File, String> {
    use std::os::unix::fs::OpenOptionsExt;

    let handle = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(resolved)
        .map_err(|error| format!("文件浏览无法打开目录 {path}: {error}"))?;
    if !handle
        .metadata()
        .map_err(|error| format!("文件浏览无法读取目录信息 {path}: {error}"))?
        .is_dir()
    {
        return Err(format!("文件浏览无法列出非目录路径: {path}"));
    }
    Ok(handle)
}

#[cfg(not(any(windows, unix)))]
fn open_verified_directory(resolved: &Path, path: &str) -> Result<File, String> {
    let handle =
        File::open(resolved).map_err(|error| format!("文件浏览无法打开目录 {path}: {error}"))?;
    if !handle
        .metadata()
        .map_err(|error| format!("文件浏览无法读取目录信息 {path}: {error}"))?
        .is_dir()
    {
        return Err(format!("文件浏览无法列出非目录路径: {path}"));
    }
    Ok(handle)
}

/// An open descriptor does not pin a name on Unix, so the object the entries
/// came from is compared against the one the checked name resolves to now.
#[cfg(unix)]
fn verify_directory_unchanged(handle: &File, resolved: &Path, path: &str) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;

    let opened = handle
        .metadata()
        .map_err(|error| format!("文件浏览无法读取目录信息 {path}: {error}"))?;
    let current = fs::symlink_metadata(resolved)
        .map_err(|error| format!("文件浏览无法读取目录信息 {path}: {error}"))?;
    if opened.dev() != current.dev() || opened.ino() != current.ino() {
        return Err(format!("文件浏览在读取目录时发现路径被替换: {path}"));
    }
    Ok(())
}

/// Windows pins the name through the verified handle's share mode, so the
/// listing cannot have come from a replacement.
#[cfg(not(unix))]
fn verify_directory_unchanged(_handle: &File, _resolved: &Path, _path: &str) -> Result<(), String> {
    Ok(())
}

fn workspace_relative(root: &Path, path: &Path) -> Result<String, String> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| "文件浏览的路径不在工作区内".to_owned())?;
    Ok(relative
        .components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("src")).unwrap();
        fs::create_dir(root.path().join("Assets")).unwrap();
        fs::write(root.path().join("readme.md"), "hello").unwrap();
        fs::write(root.path().join("Cargo.toml"), "[package]").unwrap();
        fs::write(root.path().join("src").join("main.rs"), "fn main() {}").unwrap();
        root
    }

    #[test]
    fn root_listing_puts_directories_first_and_sorts_case_insensitively() {
        let root = workspace();

        let listing = list_directory(root.path(), "").unwrap();

        assert_eq!(listing.path, "");
        let names = listing
            .entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["Assets", "src", "Cargo.toml", "readme.md"]);
        let kinds = listing
            .entries
            .iter()
            .map(|entry| entry.kind.as_str())
            .collect::<Vec<_>>();
        assert_eq!(kinds, vec!["directory", "directory", "file", "file"]);
        assert_eq!(listing.entries[3].size, Some(5));
        assert_eq!(listing.entries[0].size, None);
        assert_eq!(list_directory(root.path(), ".").unwrap(), listing);
    }

    #[test]
    fn nested_path_is_listed_with_slash_separated_relative_path() {
        let root = workspace();
        fs::create_dir(root.path().join("src").join("lib")).unwrap();
        fs::write(root.path().join("src").join("lib").join("mod.rs"), "").unwrap();

        let listing = list_directory(root.path(), "src/lib").unwrap();

        assert_eq!(listing.path, "src/lib");
        assert_eq!(listing.entries.len(), 1);
        assert_eq!(listing.entries[0].name, "mod.rs");
    }

    #[test]
    fn a_path_outside_the_workspace_is_rejected() {
        let outer = tempfile::tempdir().unwrap();
        let inner = outer.path().join("workspace");
        fs::create_dir(&inner).unwrap();
        let secret = outer.path().join("secret.txt");
        fs::write(&secret, "secret").unwrap();

        let error = list_directory(&inner, outer.path().to_str().unwrap()).unwrap_err();
        assert!(error.starts_with("文件浏览无法访问路径"), "{error}");
        assert!(read_file(&inner, secret.to_str().unwrap()).is_err());
    }

    #[test]
    fn parent_traversal_is_rejected() {
        let outer = tempfile::tempdir().unwrap();
        let inner = outer.path().join("workspace");
        fs::create_dir(&inner).unwrap();
        fs::write(outer.path().join("secret.txt"), "secret").unwrap();

        assert!(list_directory(&inner, "..").is_err());
        assert!(read_file(&inner, "../secret.txt").is_err());
    }

    #[test]
    fn a_text_file_is_read_in_full() {
        let root = workspace();

        let file = read_file(root.path(), "src/main.rs").unwrap();

        assert_eq!(file.path, "src/main.rs");
        assert_eq!(file.content, "fn main() {}");
        assert_eq!(file.size, 12);
        assert!(!file.truncated);
        assert!(!file.binary);
    }

    #[test]
    fn a_file_larger_than_the_cap_is_truncated() {
        let root = workspace();
        let size = MAX_WORKSPACE_FILE_BYTES as usize + 64;
        fs::write(root.path().join("big.txt"), vec![b'a'; size]).unwrap();

        let file = read_file(root.path(), "big.txt").unwrap();

        assert!(file.truncated);
        assert_eq!(file.size, size as u64);
        assert_eq!(file.content.len(), MAX_WORKSPACE_FILE_BYTES as usize);
        assert!(!file.binary);
    }

    #[test]
    fn a_nul_byte_in_the_sniffed_prefix_marks_the_file_binary() {
        let root = workspace();
        let mut bytes = vec![b'a'; 32];
        bytes.push(0);
        bytes.extend(vec![b'b'; 32]);
        fs::write(root.path().join("image.png"), &bytes).unwrap();
        let mut late = vec![b'a'; BINARY_SNIFF_BYTES];
        late.push(0);
        fs::write(root.path().join("late.txt"), &late).unwrap();

        let binary = read_file(root.path(), "image.png").unwrap();
        assert!(binary.binary);
        assert_eq!(binary.content, "");
        assert_eq!(binary.size, 65);

        let text = read_file(root.path(), "late.txt").unwrap();
        assert!(!text.binary);
        assert_eq!(text.content.len(), BINARY_SNIFF_BYTES + 1);
    }

    #[test]
    fn reading_a_directory_is_refused() {
        let root = workspace();

        let error = read_file(root.path(), "src").unwrap_err();

        assert_eq!(error, "文件浏览无法读取目录: src");
        assert!(read_file(root.path(), "").unwrap_err().contains("目录"));
    }

    /// 图片预览要的是整份字节：`read_file` 那条路对二进制只回一个标志位。
    #[test]
    fn a_binary_file_is_returned_whole_as_base64() {
        let root = workspace();
        let bytes: Vec<u8> = vec![0x89, b'P', b'N', b'G', 0x00, 0xFF];
        fs::write(root.path().join("logo.png"), &bytes).unwrap();

        let file = read_file_bytes(root.path(), "logo.png").unwrap();

        assert_eq!(file.path, "logo.png");
        assert_eq!(file.size, bytes.len() as u64);
        assert!(!file.too_large);
        assert_eq!(
            base64::engine::Engine::decode(
                &base64::engine::general_purpose::STANDARD,
                &file.data
            )
            .unwrap(),
            bytes
        );
    }

    /// 超限不给半份：半张图解不出来，只会在界面上变成一个坏掉的框。
    #[test]
    fn a_file_past_the_preview_cap_comes_back_empty_and_flagged() {
        let root = workspace();
        let size = MAX_WORKSPACE_PREVIEW_BYTES as usize + 1;
        fs::write(root.path().join("huge.png"), vec![b'a'; size]).unwrap();

        let file = read_file_bytes(root.path(), "huge.png").unwrap();

        assert!(file.too_large);
        assert_eq!(file.data, "");
        assert_eq!(file.size, size as u64);
    }

    /// 字节这条路与文本那条路共用同一套范围校验，不能各判各的。
    #[test]
    fn reading_bytes_obeys_the_same_scope_rules() {
        let outer = tempfile::tempdir().unwrap();
        let inner = outer.path().join("workspace");
        fs::create_dir(&inner).unwrap();
        fs::write(outer.path().join("secret.png"), b"secret").unwrap();

        assert!(read_file_bytes(&inner, "../secret.png").is_err());
        let root = workspace();
        assert_eq!(
            read_file_bytes(root.path(), "src").unwrap_err(),
            "文件浏览无法读取目录: src"
        );
    }

    #[test]
    fn listing_a_file_is_refused() {
        let root = workspace();

        let error = list_directory(root.path(), "readme.md").unwrap_err();

        assert_eq!(error, "文件浏览无法列出非目录路径: readme.md");
    }

    #[test]
    fn hidden_entries_are_listed() {
        let root = workspace();
        fs::create_dir(root.path().join(".git")).unwrap();
        fs::write(root.path().join(".env"), "SECRET=1").unwrap();

        let listing = list_directory(root.path(), "").unwrap();

        let names = listing
            .entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            vec![".git", "Assets", "src", ".env", "Cargo.toml", "readme.md"]
        );
        assert_eq!(listing.entries[0].kind, "directory");
        assert_eq!(listing.entries[3].kind, "file");
    }

    #[test]
    fn a_leading_space_in_a_name_addresses_the_listed_file() {
        let root = workspace();
        fs::write(root.path().join(" notes.txt"), "leading").unwrap();
        fs::write(root.path().join("notes.txt"), "plain").unwrap();

        let listing = list_directory(root.path(), "").unwrap();
        let listed = listing
            .entries
            .iter()
            .find(|entry| entry.name == " notes.txt")
            .expect("the leading-space name is listed unchanged");

        let file = read_file(root.path(), &listed.name).unwrap();

        assert_eq!(file.path, " notes.txt");
        assert_eq!(file.content, "leading");
        assert_eq!(
            read_file(root.path(), "notes.txt").unwrap().content,
            "plain"
        );
    }

    #[test]
    fn a_directory_named_with_a_leading_space_is_listed() {
        let root = workspace();
        fs::create_dir(root.path().join(" drafts")).unwrap();
        fs::write(root.path().join(" drafts").join("one.md"), "one").unwrap();

        let listing = list_directory(root.path(), " drafts").unwrap();

        assert_eq!(listing.path, " drafts");
        assert_eq!(listing.entries.len(), 1);
        assert_eq!(listing.entries[0].name, "one.md");
    }

    #[test]
    fn invalid_utf8_is_decoded_lossily() {
        let root = workspace();
        fs::write(root.path().join("latin1.txt"), b"caf\xE9 au lait").unwrap();

        let file = read_file(root.path(), "latin1.txt").unwrap();

        assert!(!file.binary);
        assert_eq!(file.content, "caf\u{FFFD} au lait");
        assert_eq!(file.size, 12);
    }

    #[test]
    fn a_file_of_exactly_the_cap_is_not_truncated() {
        let root = workspace();
        let size = MAX_WORKSPACE_FILE_BYTES as usize;
        fs::write(root.path().join("exact.txt"), vec![b'a'; size]).unwrap();

        let file = read_file(root.path(), "exact.txt").unwrap();

        assert!(!file.truncated);
        assert_eq!(file.size, MAX_WORKSPACE_FILE_BYTES);
        assert_eq!(file.content.len(), size);
    }

    #[test]
    fn a_symlink_entry_is_reported_as_a_link() {
        let root = workspace();
        if symlink_file(&root.path().join("readme.md"), &root.path().join("link.md")).is_err() {
            // Windows only creates symlinks for privileged or developer-mode
            // processes; there is nothing to assert without one.
            return;
        }

        let listing = list_directory(root.path(), "").unwrap();
        let entry = listing
            .entries
            .iter()
            .find(|entry| entry.name == "link.md")
            .expect("the link is listed");

        assert_eq!(entry.kind, "symlink");
        assert_eq!(entry.size, None);
    }

    #[test]
    fn a_link_leaving_the_workspace_is_refused() {
        let outer = tempfile::tempdir().unwrap();
        let inner = outer.path().join("workspace");
        fs::create_dir(&inner).unwrap();
        let secrets = outer.path().join("secrets");
        fs::create_dir(&secrets).unwrap();
        fs::write(secrets.join("file.txt"), "secret").unwrap();
        if symlink_dir(&secrets, &inner.join("sub")).is_err() {
            return;
        }

        assert!(read_file(&inner, "sub/file.txt").is_err());
        assert!(list_directory(&inner, "sub").is_err());
    }

    #[test]
    fn an_empty_query_returns_no_matches_without_walking() {
        let root = workspace();

        for query in ["", "   "] {
            let results = search_files(root.path(), query, 50).unwrap();
            assert_eq!(results.query, query);
            assert!(results.matches.is_empty());
            assert!(!results.truncated);
        }
    }

    #[test]
    fn an_uppercase_path_matches_a_lowercase_query_and_the_reverse() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("src")).unwrap();
        fs::write(root.path().join("README.md"), "readme").unwrap();
        fs::write(root.path().join("src").join("DiffViewer.tsx"), "tsx").unwrap();

        for query in ["readme", "README", "ReAdMe"] {
            let results = search_files(root.path(), query, 10).unwrap();
            assert!(
                results.matches.iter().any(|hit| hit.path == "README.md"),
                "query {query} should reach README.md"
            );
        }
        let results = search_files(root.path(), "diffviewer", 10).unwrap();
        let hit = results
            .matches
            .iter()
            .find(|hit| hit.path == "src/DiffViewer.tsx")
            .expect("DiffViewer.tsx");
        // 位置仍然是原路径的下标，折大小写没有改变字符数。
        let chars = hit.path.chars().collect::<Vec<_>>();
        for &index in &hit.positions {
            assert!((index as usize) < chars.len());
        }
    }

    #[test]
    fn a_subsequence_of_the_path_is_found_with_ascending_char_positions() {
        let root = workspace();

        let results = search_files(root.path(), "smn", 50).unwrap();

        let hit = results
            .matches
            .iter()
            .find(|match_| match_.path == "src/main.rs")
            .expect("src/main.rs is a subsequence match");
        assert_eq!(hit.kind, "file");
        assert_eq!(hit.name, "main.rs");
        // 贪婪扫描：s 在 src 开头，m 与 n 落在 main.rs 内。
        assert_eq!(hit.positions, vec![0, 4, 7]);
        assert_eq!(hit.path.chars().nth(0), Some('s'));
        assert_eq!(hit.path.chars().nth(4), Some('m'));
        assert_eq!(hit.path.chars().nth(7), Some('n'));
    }

    #[test]
    fn a_basename_prefix_match_outranks_a_deep_incidental_path_match() {
        let root = workspace();
        fs::create_dir_all(root.path().join("media/archive/index")).unwrap();
        fs::write(root.path().join("media/archive/index/notepad.txt"), "").unwrap();

        let results = search_files(root.path(), "main", 50).unwrap();

        assert_eq!(results.matches[0].path, "src/main.rs");
        // 深处凑巧含子序列的路径仍然命中，只是排在后面。
        assert!(results
            .matches
            .iter()
            .any(|match_| match_.path == "media/archive/index/notepad.txt"));
        assert!(results.matches.len() >= 2);
    }

    #[test]
    fn skipped_directories_are_never_returned_or_descended() {
        let root = workspace();
        fs::create_dir_all(root.path().join(".git").join("refs")).unwrap();
        fs::write(root.path().join(".git").join("config"), "").unwrap();
        fs::write(root.path().join(".git").join("refs").join("head"), "").unwrap();
        fs::create_dir_all(root.path().join("node_modules").join("pkg")).unwrap();
        fs::write(
            root.path()
                .join("node_modules")
                .join("pkg")
                .join("inner.js"),
            "",
        )
        .unwrap();

        let results = search_files(root.path(), "config", 200).unwrap();
        assert!(results.matches.is_empty(), "{:?}", results.matches);

        let results = search_files(root.path(), "innerjs", 200).unwrap();
        assert!(results.matches.is_empty(), "{:?}", results.matches);

        // 目录本身也是候选，但被跳过的名字绝不出现。
        let results = search_files(root.path(), "git", 200).unwrap();
        assert!(results
            .matches
            .iter()
            .all(|match_| !match_.path.contains(".git")));
    }

    #[test]
    fn a_symlink_is_not_returned() {
        let root = workspace();
        if symlink_file(&root.path().join("readme.md"), &root.path().join("link.md")).is_err() {
            // Windows only creates symlinks for privileged or developer-mode
            // processes; there is nothing to assert without one.
            return;
        }

        let results = search_files(root.path(), "link", 50).unwrap();

        assert!(results
            .matches
            .iter()
            .all(|match_| match_.path != "link.md"));
    }

    #[test]
    fn the_limit_is_honoured_and_clamped() {
        let root = workspace();
        for index in 1..=5 {
            fs::write(root.path().join(format!("target{index}.txt")), "").unwrap();
        }

        let results = search_files(root.path(), "target", 3).unwrap();
        assert_eq!(results.matches.len(), 3);

        // limit=0 收敛到 1；超过上限收敛到 200。
        let results = search_files(root.path(), "target", 0).unwrap();
        assert_eq!(results.matches.len(), 1);
        let results = search_files(root.path(), "target", 10_000).unwrap();
        assert_eq!(results.matches.len(), 5);
    }

    #[test]
    fn identical_calls_return_identical_results() {
        let root = workspace();

        let first = search_files(root.path(), "sr", 50).unwrap();
        let second = search_files(root.path(), "sr", 50).unwrap();

        assert_eq!(first, second);
    }

    #[cfg(unix)]
    fn symlink_file(target: &Path, link: &Path) -> std::io::Result<()> {
        std::os::unix::fs::symlink(target, link)
    }

    #[cfg(unix)]
    fn symlink_dir(target: &Path, link: &Path) -> std::io::Result<()> {
        std::os::unix::fs::symlink(target, link)
    }

    #[cfg(windows)]
    fn symlink_file(target: &Path, link: &Path) -> std::io::Result<()> {
        std::os::windows::fs::symlink_file(target, link)
    }

    #[cfg(windows)]
    fn symlink_dir(target: &Path, link: &Path) -> std::io::Result<()> {
        std::os::windows::fs::symlink_dir(target, link)
    }

    fn git_available() -> bool {
        Command::new("git")
            .arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    fn run_test_git(root: &Path, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .expect("git binary is available");
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn commit_all(root: &Path) {
        if !root.join(".git").exists() {
            run_test_git(root, &["init", "--quiet"]);
        }
        run_test_git(root, &["add", "-A"]);
        run_test_git(
            root,
            &[
                "-c",
                "user.name=mework-test",
                "-c",
                "user.email=mework-test@example.com",
                "commit",
                "-m",
                "test",
            ],
        );
    }

    fn not_inside_any_repository(root: &Path) -> bool {
        !Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["rev-parse", "--is-inside-work-tree"])
            .stdin(Stdio::null())
            .output()
            .is_ok_and(|output| output.status.success() && output.stdout.starts_with(b"true"))
    }

    #[test]
    fn git_candidates_cover_tracked_and_untracked_but_never_ignored_files() {
        if !git_available() {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("src")).unwrap();
        fs::write(root.path().join("src").join("tracked.rs"), "fn main() {}").unwrap();
        fs::write(root.path().join(".gitignore"), "scratchpad/\n").unwrap();
        commit_all(root.path());
        fs::create_dir(root.path().join("scratchpad")).unwrap();
        fs::write(root.path().join("scratchpad").join("ignored.log"), "x").unwrap();
        fs::create_dir(root.path().join("notes")).unwrap();
        fs::write(root.path().join("notes").join("untracked.md"), "x").unwrap();

        let results = search_files(root.path(), "tracked.rs", 50).unwrap();
        assert_eq!(results.matches.len(), 1, "{:?}", results.matches);
        assert_eq!(results.matches[0].path, "src/tracked.rs");
        assert_eq!(results.matches[0].kind, "file");

        let results = search_files(root.path(), "notes/", 50).unwrap();
        assert_eq!(results.matches.len(), 1, "{:?}", results.matches);
        assert_eq!(results.matches[0].path, "notes/untracked.md");

        let results = search_files(root.path(), "ignored", 50).unwrap();
        assert!(results.matches.is_empty(), "{:?}", results.matches);
        let results = search_files(root.path(), "scratchpad", 50).unwrap();
        assert!(results.matches.is_empty(), "{:?}", results.matches);
    }

    #[test]
    fn a_directory_candidate_is_derived_from_file_path_prefixes() {
        if !git_available() {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("src").join("lib")).unwrap();
        fs::write(root.path().join("src").join("lib").join("mod.rs"), "").unwrap();
        commit_all(root.path());

        let results = search_files(root.path(), "srlb", 50).unwrap();

        let hit = results
            .matches
            .iter()
            .find(|match_| match_.path == "src/lib")
            .expect("the derived directory src/lib is a candidate");
        assert_eq!(hit.kind, "directory");
        assert_eq!(hit.name, "lib");
    }

    #[test]
    fn a_non_git_workspace_still_searches_through_the_fallback_walk() {
        let root = tempfile::tempdir().unwrap();
        assert!(
            not_inside_any_repository(root.path()),
            "the temp directory must not sit inside a repository for this test"
        );
        fs::create_dir_all(root.path().join("deep")).unwrap();
        fs::write(root.path().join("deep").join("needle.txt"), "").unwrap();

        let results = search_files(root.path(), "needle", 50).unwrap();

        assert_eq!(results.matches.len(), 1, "{:?}", results.matches);
        assert_eq!(results.matches[0].path, "deep/needle.txt");
    }

    #[test]
    fn the_fallback_walk_skips_suffixed_cargo_target_directories() {
        let root = tempfile::tempdir().unwrap();
        assert!(
            not_inside_any_repository(root.path()),
            "the temp directory must not sit inside a repository for this test"
        );
        fs::create_dir_all(root.path().join("target-cache")).unwrap();
        fs::write(
            root.path()
                .join("target-cache")
                .join("libwindows_threading.rmeta"),
            "",
        )
        .unwrap();

        let results = search_files(root.path(), "rmeta", 200).unwrap();
        assert!(results.matches.is_empty(), "{:?}", results.matches);
        let results = search_files(root.path(), "target-cache", 200).unwrap();
        assert!(results.matches.is_empty(), "{:?}", results.matches);
    }

    #[test]
    fn a_name_with_a_space_and_non_ascii_characters_survives_the_z_parse() {
        if !git_available() {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("doc notes")).unwrap();
        fs::write(
            root.path().join("doc notes").join("设计 草图.md"),
            "drawing",
        )
        .unwrap();
        commit_all(root.path());

        let results = search_files(root.path(), "设计 草图", 50).unwrap();

        assert_eq!(results.matches.len(), 1, "{:?}", results.matches);
        assert_eq!(results.matches[0].path, "doc notes/设计 草图.md");
        assert_eq!(results.matches[0].name, "设计 草图.md");

        let results = search_files(root.path(), "doc notes", 50).unwrap();
        assert!(results
            .matches
            .iter()
            .any(|match_| match_.path == "doc notes/设计 草图.md"));
    }

    #[test]
    fn ls_files_paths_are_reported_relative_to_the_workspace_root() {
        if !git_available() {
            return;
        }
        let repository = tempfile::tempdir().unwrap();
        fs::write(repository.path().join("outer.txt"), "").unwrap();
        fs::create_dir(repository.path().join("sub")).unwrap();
        fs::write(repository.path().join("sub").join("inner.txt"), "").unwrap();
        commit_all(repository.path());

        // 工作区是仓库的子目录：路径必须相对工作区而不是仓库根。
        let workspace = repository.path().join("sub");
        let results = search_files(&workspace, "inner", 50).unwrap();

        assert_eq!(results.matches.len(), 1, "{:?}", results.matches);
        assert_eq!(results.matches[0].path, "inner.txt");

        let results = search_files(&workspace, "outer", 50).unwrap();
        assert!(results.matches.is_empty(), "{:?}", results.matches);
    }

    #[test]
    fn identical_calls_are_deterministic_in_a_git_workspace() {
        if !git_available() {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("src").join("lib")).unwrap();
        fs::write(root.path().join("src").join("main.rs"), "").unwrap();
        fs::write(root.path().join("src").join("lib").join("mod.rs"), "").unwrap();
        commit_all(root.path());

        let first = search_files(root.path(), "sr", 50).unwrap();
        let second = search_files(root.path(), "sr", 50).unwrap();

        assert_eq!(first, second);
        assert!(first
            .matches
            .iter()
            .any(|match_| match_.path == "src/main.rs"));
    }
}
