//! Online skill registry search and retrieval.
//!
//! Production CSP permits only `connect-src ipc:`, and installation clones repositories or extracts
//! untrusted archives, so all network retrieval runs in the host.
//!
//! This module fetches remote content only. [`crate::skills`] performs installation and applies the
//! shared constraints: no symlinks, at most 100 MiB and 2000 entries per skill, and
//! case-insensitively unique directory names.
//!
//! Installation handles:
//!
//! - `skills.sh:{owner}/{repo}/{skill}`
//! - `claude-plugins:{owner}/{repo}/{directoryPath}`
//! - `clawhub:{ownerHandle}/{slug}`
//! - `github:{https URL to SKILL.md}`

use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

use serde::Serialize;
use serde_json::Value;

use crate::environment_tools::resolve_on_path;

/// Total timeout for one search. All three sources run concurrently, so this is the wall-clock limit.
const SEARCH_TIMEOUT: Duration = Duration::from_secs(15);
/// Maximum duration of one git subcommand. Every operation on an unreviewed repository must be bounded.
const GIT_TIMEOUT: Duration = Duration::from_secs(120);
/// Maximum downloaded skill archive size, matching the per-skill limit in `skills.rs`.
const MAX_ARCHIVE_BYTES: u64 = 100 * 1024 * 1024;
/// Maximum search response body size.
const MAX_SEARCH_BODY_BYTES: u64 = 4 * 1024 * 1024;
/// Maximum results returned by one source.
const MAX_RESULTS_PER_SOURCE: usize = 200;
/// Maximum depth when searching a clone for a skill directory by name.
const MAX_SKILL_SEARCH_DEPTH: usize = 6;

/// An online skill source. Values exactly match the renderer's `SkillSearchSource`.
/// `Github` is not searchable; the renderer constructs it from a direct URL, but `fetch` consumes
/// its `github:` handle and therefore shares this vocabulary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum SkillSearchSource {
    #[serde(rename = "skills.sh")]
    SkillsSh,
    #[serde(rename = "claude-plugins.dev")]
    ClaudePlugins,
    #[serde(rename = "clawhub.ai")]
    Clawhub,
    #[serde(rename = "github")]
    #[allow(dead_code)]
    Github,
}

/// A search result. `install_source` is an opaque handle passed unchanged by the renderer to [`fetch`].
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillSearchResult {
    pub slug: String,
    pub name: String,
    pub description: String,
    pub author: String,
    pub stars: u64,
    pub downloads: u64,
    pub source_registry: SkillSearchSource,
    pub source_url: String,
    pub install_source: String,
}

/// A cross-registry search report. Failure of one independent source does not hide results from
/// the others; an empty result list with every source failed distinguishes failure from no matches.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillSearchReport {
    pub results: Vec<SkillSearchResult>,
    pub failed_sources: Vec<SkillSearchSource>,
}

/// A fetched skill whose `skill_dir` lies within `temp_dir`. The caller removes `temp_dir` after installation.
pub struct FetchedSkill {
    pub temp_dir: PathBuf,
    pub skill_dir: PathBuf,
    /// Registry page URL for this skill, stored in `SkillRecord.source_url`.
    pub source_url: String,
}

impl FetchedSkill {
    /// Removes the temporary workspace. Cleanup failure is non-fatal because the caller already
    /// obtained its result and the directory is under the system temporary path.
    pub fn cleanup(&self) {
        remove_directory_forcefully(&self.temp_dir);
    }
}

// ===========================================================================
// Search
// ===========================================================================

fn client() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .timeout(SEARCH_TIMEOUT)
        .connect_timeout(Duration::from_secs(8))
        .user_agent("Mework/0.1")
        .build()
        .map_err(|error| format!("无法初始化技能注册表客户端: {error}"))
}

fn fetch_json(client: &reqwest::blocking::Client, url: &str) -> Result<Value, String> {
    let response = client
        .get(url)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .map_err(|error| format!("请求失败: {error}"))?;
    if !response.status().is_success() {
        return Err(format!("HTTP {}", response.status().as_u16()));
    }
    let mut body = Vec::new();
    response
        .take(MAX_SEARCH_BODY_BYTES)
        .read_to_end(&mut body)
        .map_err(|error| format!("读取响应失败: {error}"))?;
    serde_json::from_slice(&body).map_err(|error| format!("响应不是有效的 JSON: {error}"))
}

/// Searches the three registries. GitHub is excluded because it is installed from a direct URL.
pub fn search(query: &str) -> SkillSearchReport {
    let query = query.trim();
    if query.is_empty() {
        return SkillSearchReport::default();
    }
    let Ok(client) = client() else {
        return SkillSearchReport {
            results: Vec::new(),
            failed_sources: vec![
                SkillSearchSource::SkillsSh,
                SkillSearchSource::ClaudePlugins,
                SkillSearchSource::Clawhub,
            ],
        };
    };

    let encoded = percent_encode_query(query);
    let sources: [(SkillSearchSource, String, fn(&Value) -> Option<Vec<SkillSearchResult>>); 3] = [
        (
            SkillSearchSource::SkillsSh,
            format!("https://skills.sh/api/search?q={encoded}"),
            normalize_skills_sh,
        ),
        (
            SkillSearchSource::ClaudePlugins,
            format!("https://claude-plugins.dev/api/skills?q={encoded}&limit=20"),
            normalize_claude_plugins,
        ),
        (
            SkillSearchSource::Clawhub,
            format!("https://clawhub.ai/api/v1/search?q={encoded}"),
            normalize_clawhub,
        ),
    ];

    // Sources are independent, so search concurrently rather than accumulating their timeouts.
    let outcomes = std::thread::scope(|scope| {
        let handles: Vec<_> = sources
            .iter()
            .map(|(source, url, normalize)| {
                let client = client.clone();
                (
                    *source,
                    scope.spawn(move || {
                        fetch_json(&client, url).ok().and_then(|body| normalize(&body))
                    }),
                )
            })
            .collect();
        handles
            .into_iter()
            // A thread panic and request failure both mean this source yielded no results.
            .map(|(source, handle)| (source, handle.join().unwrap_or(None)))
            .collect::<Vec<_>>()
    });

    let mut report = SkillSearchReport::default();
    // Deduplicate same-named skills across registries, preserving `sources` order.
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (source, parsed) in outcomes {
        match parsed {
            Some(results) => {
                for result in results.into_iter().take(MAX_RESULTS_PER_SOURCE) {
                    if seen.insert(result.name.to_lowercase()) {
                        report.results.push(result);
                    }
                }
            }
            None => report.failed_sources.push(source),
        }
    }
    report
}

/// Minimal escaping for `?q=`. Ampersands, hashes, and spaces would otherwise change query semantics.
fn percent_encode_query(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(*byte as char);
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

fn text(value: Option<&Value>) -> String {
    value.and_then(Value::as_str).unwrap_or_default().trim().to_owned()
}

fn count(value: Option<&Value>) -> u64 {
    value.and_then(Value::as_u64).unwrap_or(0)
}

fn normalize_skills_sh(body: &Value) -> Option<Vec<SkillSearchResult>> {
    let entries = body.get("skills")?.as_array()?;
    Some(
        entries
            .iter()
            .filter_map(|entry| {
                let id = text(entry.get("id"));
                let name = text(entry.get("name"));
                let source = text(entry.get("source"));
                // Installation parses `id` as owner/repo/skill, so every segment is required.
                if id.split('/').count() != 3 || id.split('/').any(str::is_empty) || name.is_empty()
                {
                    return None;
                }
                Some(SkillSearchResult {
                    author: source.split('/').next().unwrap_or_default().to_owned(),
                    slug: id.clone(),
                    name,
                    description: String::new(),
                    stars: 0,
                    downloads: count(entry.get("installs")),
                    source_registry: SkillSearchSource::SkillsSh,
                    source_url: format!("https://skills.sh/{id}"),
                    install_source: format!("skills.sh:{id}"),
                })
            })
            .collect(),
    )
}

fn normalize_claude_plugins(body: &Value) -> Option<Vec<SkillSearchResult>> {
    let entries = body.get("skills")?.as_array()?;
    Some(
        entries
            .iter()
            .filter_map(|entry| {
                let metadata = entry.get("metadata");
                let repo_owner = text(metadata.and_then(|value| value.get("repoOwner")));
                let repo_name = text(metadata.and_then(|value| value.get("repoName")));
                let source_url = text(entry.get("sourceUrl"));
                let directory_path =
                    normalize_directory_path(&text(metadata.and_then(|v| v.get("directoryPath"))))
                        .or_else(|| {
                            directory_path_from_tree_url(&source_url, &repo_owner, &repo_name)
                        })?;
                // Drop entries with an unresolvable installation path rather than guessing from the
                // display name and potentially installing a different skill.
                if repo_owner.is_empty() || repo_name.is_empty() {
                    return None;
                }
                let name = text(entry.get("name"));
                if name.is_empty() {
                    return None;
                }
                let author = {
                    let explicit = text(entry.get("author"));
                    if explicit.is_empty() {
                        text(entry.get("namespace"))
                    } else {
                        explicit
                    }
                };
                Some(SkillSearchResult {
                    slug: text(entry.get("id")),
                    name,
                    description: text(entry.get("description")),
                    author,
                    stars: count(entry.get("stars")),
                    downloads: count(entry.get("installs")),
                    source_registry: SkillSearchSource::ClaudePlugins,
                    source_url: if source_url.is_empty() {
                        format!("https://github.com/{repo_owner}/{repo_name}/tree/main/{directory_path}")
                    } else {
                        source_url
                    },
                    install_source: format!(
                        "claude-plugins:{repo_owner}/{repo_name}/{directory_path}"
                    ),
                })
            })
            .collect(),
    )
}

fn normalize_clawhub(body: &Value) -> Option<Vec<SkillSearchResult>> {
    let entries = body.get("results")?.as_array()?;
    Some(
        entries
            .iter()
            .filter_map(|entry| {
                let owner = text(entry.get("ownerHandle"));
                let slug = text(entry.get("slug"));
                if owner.is_empty() || slug.is_empty() {
                    return None;
                }
                let name = text(entry.get("displayName"));
                Some(SkillSearchResult {
                    name: if name.is_empty() { slug.clone() } else { name },
                    description: text(entry.get("summary")),
                    author: owner.clone(),
                    stars: 0,
                    downloads: 0,
                    source_registry: SkillSearchSource::Clawhub,
                    source_url: format!("https://clawhub.ai/{owner}/skills/{slug}"),
                    install_source: format!("clawhub:{owner}/{slug}"),
                    slug,
                })
            })
            .collect(),
    )
}

fn normalize_directory_path(raw: &str) -> Option<String> {
    let normalized = raw
        .split('/')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("/");
    (!normalized.is_empty()).then_some(normalized)
}

/// Extracts `{dir}` from `https://github.com/{owner}/{repo}/tree/main/{dir}`.
/// Reject a mismatched repository or a branch other than main/master because the URL does not refer
/// to its claimed repository.
fn directory_path_from_tree_url(source_url: &str, owner: &str, repo: &str) -> Option<String> {
    if source_url.is_empty() || owner.is_empty() || repo.is_empty() {
        return None;
    }
    let url = url::Url::parse(source_url).ok()?;
    if url.host_str()? != "github.com" {
        return None;
    }
    let segments = url
        .path_segments()?
        .filter(|segment| !segment.is_empty())
        .map(percent_decode)
        .collect::<Option<Vec<_>>>()?;
    let [url_owner, url_repo, kind, branch, rest @ ..] = segments.as_slice() else {
        return None;
    };
    if !url_owner.eq_ignore_ascii_case(owner)
        || !url_repo.eq_ignore_ascii_case(repo)
        || kind != "tree"
        || !matches!(branch.as_str(), "main" | "master")
    {
        return None;
    }
    normalize_directory_path(&rest.join("/"))
}

// ===========================================================================
// GitHub direct URLs
// ===========================================================================

/// Location parsed from a GitHub URL to `SKILL.md`.
#[derive(Clone, Debug, PartialEq)]
pub struct GithubSkillLocation {
    pub owner: String,
    pub repo: String,
    /// `refs/heads` or `refs/tags` prefix, present only in raw URLs.
    pub ref_namespace: Option<&'static str>,
    /// Decoded ref-and-directory segments. Their boundary requires a remote lookup.
    pub ref_and_path: Vec<String>,
    pub descriptor_file_name: &'static str,
}

fn invalid_path_part(part: &str) -> bool {
    // Decoded `/` changes path depth, `\` does so on Windows, and NUL cannot name a file.
    // None can occur in a real GitHub entry name.
    part.is_empty()
        || part != part.trim()
        || part == "."
        || part == ".."
        || part.contains('\\')
        || part.contains('/')
        || part.contains('\0')
}

fn repo_part_is_valid(part: &str) -> bool {
    !part.is_empty()
        && part
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '.' | '-'))
}

/// Parses a GitHub URL pointing to a skill's `SKILL.md`.
/// The URL must identify `SKILL.md`: a bare repository or tree URL would require installation to
/// guess among multiple skills. The renderer and installer share this parser.
pub fn parse_github_skill_url(raw_url: &str) -> Option<GithubSkillLocation> {
    let trimmed = raw_url.trim();
    let url = url::Url::parse(trimmed).ok()?;
    if url.scheme() != "https" && url.scheme() != "http" {
        return None;
    }
    let segments = url
        .path_segments()?
        .filter(|segment| !segment.is_empty())
        .map(percent_decode)
        .collect::<Option<Vec<_>>>()?;
    if segments.iter().any(|part| invalid_path_part(part)) {
        return None;
    }

    let host = url.host_str()?.to_ascii_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host).to_owned();
    let [owner, raw_repo, tail @ ..] = segments.as_slice() else {
        return None;
    };
    // `blob` and `raw` identify files. GitHub Raw redirects to the raw-content domain, but both
    // routes use the same syntax contract.
    let raw_ref_and_path: &[String] = if host == "github.com"
        && matches!(tail.first().map(String::as_str), Some("blob") | Some("raw"))
    {
        &tail[1..]
    } else if host == "raw.githubusercontent.com" {
        tail
    } else {
        return None;
    };

    let repo = raw_repo.strip_suffix(".git").unwrap_or(raw_repo).to_owned();
    let descriptor_file_name = match raw_ref_and_path.last().map(String::as_str) {
        Some("SKILL.md") => "SKILL.md",
        Some("skill.md") => "skill.md",
        _ => return None,
    };
    let mut ref_and_path = raw_ref_and_path[..raw_ref_and_path.len() - 1].to_vec();
    let mut ref_namespace = None;
    if ref_and_path.first().map(String::as_str) == Some("refs") {
        match ref_and_path.get(1).map(String::as_str) {
            Some("heads") => ref_namespace = Some("heads"),
            Some("tags") => ref_namespace = Some("tags"),
            _ => {}
        }
        if ref_namespace.is_some() {
            ref_and_path.drain(..2);
        }
    }

    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    if !repo_part_is_valid(owner) || !repo_part_is_valid(&repo) {
        return None;
    }
    // At least one segment is required; it may be a ref selecting a root-level skill.
    if ref_and_path.is_empty() {
        return None;
    }

    Some(GithubSkillLocation {
        owner: owner.clone(),
        repo,
        ref_namespace,
        ref_and_path,
        descriptor_file_name,
    })
}

/// Converts a validated direct GitHub URL into an installable search result.
/// The renderer has an equivalent parser for immediate input feedback. This implementation shares
/// [`parse_github_skill_url`] so accepted URLs remain installable.
#[allow(dead_code)]
pub fn github_result(raw_url: &str) -> Option<SkillSearchResult> {
    let location = parse_github_skill_url(raw_url)?;
    let path = location.ref_and_path.join("/");
    let namespace = location
        .ref_namespace
        .map(|namespace| format!("refs/{namespace}/"))
        .unwrap_or_default();
    let canonical = if location.ref_namespace.is_some() {
        format!(
            "https://raw.githubusercontent.com/{}/{}/{namespace}{path}/{}",
            location.owner, location.repo, location.descriptor_file_name
        )
    } else {
        format!(
            "https://github.com/{}/{}/blob/{path}/{}",
            location.owner, location.repo, location.descriptor_file_name
        )
    };
    Some(SkillSearchResult {
        slug: format!("{}/{}/{namespace}{path}", location.owner, location.repo),
        name: location.repo.clone(),
        description: String::new(),
        author: location.owner.clone(),
        stars: 0,
        downloads: 0,
        source_registry: SkillSearchSource::Github,
        source_url: canonical.clone(),
        install_source: format!("github:{canonical}"),
    })
}

/// Decodes only `%XX` sequences and requires valid UTF-8; invalid input rejects the URL.
fn percent_decode(value: &str) -> Option<String> {
    if !value.contains('%') {
        return Some(value.to_owned());
    }
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return None;
            }
            let high = (bytes[index + 1] as char).to_digit(16)?;
            let low = (bytes[index + 2] as char).to_digit(16)?;
            decoded.push((high * 16 + low) as u8);
            index += 3;
            continue;
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    String::from_utf8(decoded).ok()
}

// ===========================================================================
// Fetch
// ===========================================================================

/// Fetches a skill into a temporary directory from an installation handle.
pub fn fetch(install_source: &str) -> Result<FetchedSkill, String> {
    let (source, identifier) = install_source
        .split_once(':')
        .ok_or_else(|| format!("无法识别的安装来源：{install_source}"))?;
    match source {
        "skills.sh" => fetch_from_skills_sh(identifier),
        "claude-plugins" => fetch_from_claude_plugins(identifier),
        "clawhub" => fetch_from_clawhub(identifier),
        // A `github:` payload is a URL containing colons, so use only the `split_once` suffix intact.
        "github" => fetch_from_github(identifier),
        _ => Err(format!("无法识别的安装来源：{source}")),
    }
}

fn fetch_from_skills_sh(identifier: &str) -> Result<FetchedSkill, String> {
    let parts: Vec<&str> = identifier.split('/').collect();
    let [owner, repo, skill_name] = parts.as_slice() else {
        return Err(format!("skills.sh 安装句柄格式无效：{identifier}"));
    };
    if !repo_part_is_valid(owner) || !repo_part_is_valid(repo) || invalid_path_part(skill_name) {
        return Err(format!("skills.sh 安装句柄格式无效：{identifier}"));
    }
    let repo_url = format!("https://github.com/{owner}/{repo}");
    let temp_dir = create_temp_dir("skills-sh")?;
    let content = match checkout_repository(&repo_url, "HEAD", None, &temp_dir) {
        Ok(content) => content,
        Err(error) => {
            remove_directory_forcefully(&temp_dir);
            return Err(error);
        }
    };
    finish_fetch(temp_dir, content.clone(), find_skill_dir(&content, skill_name), repo_url)
}

fn fetch_from_claude_plugins(identifier: &str) -> Result<FetchedSkill, String> {
    let mut parts = identifier.split('/');
    let owner = parts.next().unwrap_or_default();
    let repo = parts.next().unwrap_or_default();
    let directory_parts: Vec<&str> = parts.collect();
    if !repo_part_is_valid(owner)
        || !repo_part_is_valid(repo)
        || directory_parts.is_empty()
        || directory_parts.iter().any(|part| invalid_path_part(part))
    {
        return Err(format!("claude-plugins 安装句柄格式无效：{identifier}"));
    }
    let directory_path = directory_parts.join("/");
    let repo_url = format!("https://github.com/{owner}/{repo}");
    let temp_dir = create_temp_dir("claude-plugins")?;
    let content = match checkout_repository(&repo_url, "HEAD", Some(&directory_path), &temp_dir) {
        Ok(content) => content,
        Err(error) => {
            remove_directory_forcefully(&temp_dir);
            return Err(error);
        }
    };
    let skill_dir = content.join(directory_path.replace('/', std::path::MAIN_SEPARATOR_STR));
    finish_fetch(
        temp_dir,
        content,
        Some(skill_dir),
        format!("{repo_url}/tree/main/{directory_path}"),
    )
}

fn fetch_from_github(raw_url: &str) -> Result<FetchedSkill, String> {
    let location = parse_github_skill_url(raw_url)
        .ok_or_else(|| format!("GitHub 技能地址无效：{raw_url}"))?;
    let repo_url = format!("https://github.com/{}/{}", location.owner, location.repo);
    // Branch names can contain `/`, so only the remote can identify the ref/path boundary.
    let (reference, directory_path) = resolve_github_reference(&repo_url, &location)?;
    let temp_dir = create_temp_dir("github")?;
    let content = match checkout_repository(
        &repo_url,
        &reference,
        directory_path.as_deref(),
        &temp_dir,
    ) {
        Ok(content) => content,
        Err(error) => {
            remove_directory_forcefully(&temp_dir);
            return Err(error);
        }
    };
    let skill_dir = match &directory_path {
        Some(path) => content.join(path.replace('/', std::path::MAIN_SEPARATOR_STR)),
        None => content.clone(),
    };
    let source_url = match &directory_path {
        Some(path) => format!("{repo_url}/tree/{reference}/{path}"),
        None => format!("{repo_url}/tree/{reference}"),
    };
    finish_fetch(temp_dir, content, Some(skill_dir), source_url)
}

fn fetch_from_clawhub(identifier: &str) -> Result<FetchedSkill, String> {
    let parts: Vec<&str> = identifier.split('/').collect();
    let [owner, slug] = parts.as_slice() else {
        return Err(format!("clawhub 安装句柄格式无效：{identifier}"));
    };
    if !repo_part_is_valid(owner) || !repo_part_is_valid(slug) {
        return Err(format!("clawhub 安装句柄格式无效：{identifier}"));
    }
    let client = client()?;
    // Verify the detail response before downloading: the endpoint selects by slug and owner, and
    // accepting a mismatched package would install a different skill.
    let detail = fetch_json(
        &client,
        &format!(
            "https://clawhub.ai/api/v1/skills/{}?ownerHandle={}",
            percent_encode_query(slug),
            percent_encode_query(owner)
        ),
    )
    .map_err(|error| format!("clawhub 详情请求失败：{error}"))?;
    let detail_slug = text(detail.get("skill").and_then(|skill| skill.get("slug")));
    let detail_owner = text(detail.get("owner").and_then(|value| value.get("handle")));
    if detail_slug != *slug || !detail_owner.eq_ignore_ascii_case(owner) {
        return Err(format!("clawhub 详情与请求的技能不一致：{identifier}"));
    }

    let temp_dir = create_temp_dir("clawhub")?;
    let archive_path = temp_dir.join("skill.zip");
    let download = || -> Result<(), String> {
        let response = client
            .get(format!(
                "https://clawhub.ai/api/v1/download?slug={}&ownerHandle={}",
                percent_encode_query(slug),
                percent_encode_query(owner)
            ))
            .send()
            .map_err(|error| format!("clawhub 下载失败：{error}"))?;
        if !response.status().is_success() {
            return Err(format!("clawhub 下载失败：HTTP {}", response.status().as_u16()));
        }
        let mut bytes = Vec::new();
        response
            .take(MAX_ARCHIVE_BYTES)
            .read_to_end(&mut bytes)
            .map_err(|error| format!("clawhub 下载失败：{error}"))?;
        if bytes.len() as u64 >= MAX_ARCHIVE_BYTES {
            return Err("技能压缩包超过 100 MiB".into());
        }
        fs::write(&archive_path, &bytes).map_err(|error| format!("无法写入临时压缩包：{error}"))
    };
    if let Err(error) = download() {
        remove_directory_forcefully(&temp_dir);
        return Err(error);
    }
    Ok(FetchedSkill {
        skill_dir: archive_path,
        temp_dir,
        source_url: format!("https://clawhub.ai/{owner}/skills/{slug}"),
    })
}

/// Clawhub fetches an archive rather than a directory; the caller selects the installation path accordingly.
pub fn is_archive(fetched: &FetchedSkill) -> bool {
    fetched
        .skill_dir
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("zip"))
}

fn finish_fetch(
    temp_dir: PathBuf,
    content: PathBuf,
    skill_dir: Option<PathBuf>,
    source_url: String,
) -> Result<FetchedSkill, String> {
    let Some(skill_dir) = skill_dir else {
        remove_directory_forcefully(&temp_dir);
        return Err("在仓库里没有找到这个技能的目录".into());
    };
    // The fetched directory must remain inside the temporary workspace. A `..` segment or symlink
    // could otherwise redirect installation to a path outside it.
    let canonical_content = content.canonicalize().unwrap_or_else(|_| content.clone());
    let canonical_skill = match skill_dir.canonicalize() {
        Ok(path) => path,
        Err(_) => {
            remove_directory_forcefully(&temp_dir);
            return Err("在仓库里没有找到这个技能的目录".into());
        }
    };
    if !canonical_skill.starts_with(&canonical_content) {
        remove_directory_forcefully(&temp_dir);
        return Err("技能目录逃出了下载工作区".into());
    }
    if !manifest_present(&canonical_skill) {
        remove_directory_forcefully(&temp_dir);
        return Err("目标目录里没有 SKILL.md".into());
    }
    Ok(FetchedSkill {
        temp_dir,
        skill_dir: canonical_skill,
        source_url,
    })
}

fn manifest_present(directory: &Path) -> bool {
    ["SKILL.md", "skill.md"].iter().any(|candidate| {
        matches!(
            fs::symlink_metadata(directory.join(candidate)),
            Ok(metadata) if metadata.is_file()
        )
    })
}

/// Finds a skill directory by name in a clone. A skills.sh handle provides the skill name but no path.
fn find_skill_dir(root: &Path, skill_name: &str) -> Option<PathBuf> {
    if manifest_present(root)
        && root
            .file_name()
            .is_some_and(|name| name.eq_ignore_ascii_case(skill_name))
    {
        return Some(root.to_path_buf());
    }
    let mut queue: Vec<(PathBuf, usize)> = vec![(root.to_path_buf(), 0)];
    let mut fallback = None;
    while let Some((directory, depth)) = queue.pop() {
        if depth > MAX_SKILL_SEARCH_DEPTH {
            continue;
        }
        let Ok(entries) = fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(metadata) = fs::symlink_metadata(&path) else {
                continue;
            };
            if !metadata.is_dir() {
                continue;
            }
            let matches_name = entry
                .file_name()
                .to_string_lossy()
                .eq_ignore_ascii_case(skill_name);
            if matches_name && manifest_present(&path) {
                return Some(path);
            }
            if matches_name && fallback.is_none() {
                fallback = Some(path.clone());
            }
            queue.push((path, depth + 1));
        }
    }
    // A root-level manifest makes the repository itself a skill when no same-named child exists.
    fallback.or_else(|| manifest_present(root).then(|| root.to_path_buf()))
}

// ===========================================================================
// git
// ===========================================================================

fn git_command() -> Result<PathBuf, String> {
    resolve_on_path("git")
        .ok_or_else(|| "PATH 上没有 git；在线安装技能需要它（见「环境依赖」设置页）".to_owned())
}

/// Runs one noninteractive, bounded git subcommand and returns stderr details on failure.
fn run_git(git: &Path, args: &[&str], cwd: Option<&Path>) -> Result<String, String> {
    let mut command = Command::new(git);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // A private repository could open a credential prompt and wait indefinitely; these variables
        // convert it into a regular failure.
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "")
        .env("GCM_INTERACTIVE", "never")
        .env("GIT_LFS_SKIP_SMUDGE", "1");
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("无法启动 git：{error}"))?;
    let status = {
        use wait_timeout::ChildExt as _;
        match child
            .wait_timeout(GIT_TIMEOUT)
            .map_err(|error| format!("等待 git 失败：{error}"))?
        {
            Some(status) => status,
            None => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("git 操作超时".into());
            }
        }
    };
    let output = child
        .wait_with_output()
        .map_err(|error| format!("读取 git 输出失败：{error}"))?;
    if !status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail: String = stderr.lines().take(4).collect::<Vec<_>>().join("; ");
        return Err(format!("git 失败：{}", detail.trim()));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Shallow-fetches a ref and checks it out into a worktree separate from `.git`.
/// Separation prevents a root-level skill copy from installing the repository metadata too.
fn checkout_repository(
    repo_url: &str,
    reference: &str,
    directory_path: Option<&str>,
    temp_dir: &Path,
) -> Result<PathBuf, String> {
    let git = git_command()?;
    let git_dir = temp_dir.join("repo.git");
    let content = temp_dir.join("content");
    fs::create_dir_all(&content).map_err(|error| format!("无法创建下载目录：{error}"))?;

    let git_dir_arg = format!("--git-dir={}", git_dir.display());
    run_git(&git, &["init", "--bare", "--quiet", &git_dir.to_string_lossy()], None)?;
    run_git(
        &git,
        &[
            &git_dir_arg,
            "fetch",
            "--quiet",
            "--depth",
            "1",
            "--no-tags",
            "--",
            repo_url,
            reference,
        ],
        None,
    )?;
    let pathspec = match directory_path {
        Some(path) => format!(":(top,literal){path}"),
        None => ".".to_owned(),
    };
    let work_tree_arg = format!("--work-tree={}", content.display());
    run_git(
        &git,
        &[
            &git_dir_arg,
            &work_tree_arg,
            "checkout",
            "--quiet",
            "FETCH_HEAD",
            "--",
            &pathspec,
        ],
        Some(&content),
    )?;
    Ok(content)
}

/// Asks the remote where the ref in a URL ends. A 40-digit hexadecimal value is a commit ID.
fn resolve_github_reference(
    repo_url: &str,
    location: &GithubSkillLocation,
) -> Result<(String, Option<String>), String> {
    let git = git_command()?;
    let listing = run_git(
        &git,
        &["ls-remote", "--heads", "--tags", "--", repo_url],
        None,
    )?;
    let mut names: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for line in listing.lines() {
        let Some((_, full_name)) = line.split_once('\t') else {
            continue;
        };
        let full_name = full_name.trim();
        if full_name.ends_with("^{}") {
            continue;
        }
        let Some(rest) = full_name.strip_prefix("refs/") else {
            continue;
        };
        let (namespace, name) = match rest.split_once('/') {
            Some(parts) => parts,
            None => continue,
        };
        if let Some(expected) = location.ref_namespace {
            if namespace != expected {
                continue;
            }
        } else if namespace != "heads" && namespace != "tags" {
            continue;
        }
        *names.entry(name.to_owned()).or_insert(0) += 1;
    }

    // Try longest prefixes first because branch names may contain `/`; this prevents interpreting
    // `feature/foo` as branch `feature` plus directory `foo`.
    for length in (1..=location.ref_and_path.len()).rev() {
        let name = location.ref_and_path[..length].join("/");
        match names.get(&name) {
            Some(1) => {
                let path = (length < location.ref_and_path.len())
                    .then(|| location.ref_and_path[length..].join("/"));
                return Ok((name, path));
            }
            Some(_) => {
                return Err(format!(
                    "{repo_url} 同时有名为「{name}」的分支和标签，这条地址说不清是哪一个"
                ))
            }
            None => {}
        }
    }

    let head = &location.ref_and_path[0];
    if location.ref_namespace.is_none()
        && head.len() == 40
        && head.chars().all(|character| character.is_ascii_hexdigit())
    {
        let path = (location.ref_and_path.len() > 1)
            .then(|| location.ref_and_path[1..].join("/"));
        return Ok((head.to_lowercase(), path));
    }
    Err(format!(
        "{repo_url} 里没有与「{}」匹配的分支或标签",
        location.ref_and_path.join("/")
    ))
}

// ===========================================================================
// Temporary directories
// ===========================================================================

fn create_temp_dir(prefix: &str) -> Result<PathBuf, String> {
    let mut base = std::env::temp_dir();
    let unique = format!(
        "mework-skill-{prefix}-{}-{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    );
    base.push(unique);
    fs::create_dir_all(&base).map_err(|error| format!("无法创建临时目录：{error}"))?;
    Ok(base)
}

/// Removes a directory tree. Git can mark package files read-only on Windows, causing
/// `remove_dir_all` to fail, so clear the read-only bit first.
fn remove_directory_forcefully(path: &Path) {
    fn clear_readonly(path: &Path) {
        let Ok(metadata) = fs::symlink_metadata(path) else {
            return;
        };
        if metadata.file_type().is_symlink() {
            return;
        }
        let mut permissions = metadata.permissions();
        if permissions.readonly() {
            #[allow(clippy::permissions_set_readonly_false)]
            permissions.set_readonly(false);
            let _ = fs::set_permissions(path, permissions);
        }
        if !metadata.is_dir() {
            return;
        }
        let Ok(entries) = fs::read_dir(path) else {
            return;
        };
        for entry in entries.flatten() {
            clear_readonly(&entry.path());
        }
    }

    if fs::remove_dir_all(path).is_ok() {
        return;
    }
    clear_readonly(path);
    let _ = fs::remove_dir_all(path);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_url_must_point_at_a_skill_manifest() {
        assert!(parse_github_skill_url("https://github.com/owner/repo").is_none());
        assert!(parse_github_skill_url("https://github.com/owner/repo/tree/main/skills/demo").is_none());
        let location =
            parse_github_skill_url("https://github.com/owner/repo/blob/main/skills/demo/SKILL.md")
                .expect("a blob URL ending in SKILL.md is installable");
        assert_eq!(location.owner, "owner");
        assert_eq!(location.repo, "repo");
        assert_eq!(location.ref_namespace, None);
        assert_eq!(location.ref_and_path, vec!["main", "skills", "demo"]);
        assert_eq!(location.descriptor_file_name, "SKILL.md");
    }

    #[test]
    fn raw_urls_keep_their_ref_namespace() {
        let location = parse_github_skill_url(
            "https://raw.githubusercontent.com/owner/repo/refs/heads/main/demo/SKILL.md",
        )
        .expect("the raw content route is the same syntax contract");
        assert_eq!(location.ref_namespace, Some("heads"));
        assert_eq!(location.ref_and_path, vec!["main", "demo"]);
    }

    #[test]
    fn traversal_segments_are_rejected_before_any_network_call() {
        assert!(parse_github_skill_url("https://github.com/owner/repo/blob/main/../SKILL.md").is_none());
        assert!(parse_github_skill_url(
            "https://github.com/owner/repo/blob/main/%2e%2e/SKILL.md"
        )
        .is_none());
        assert!(parse_github_skill_url("https://example.com/owner/repo/blob/main/SKILL.md").is_none());
    }

    #[test]
    fn github_results_round_trip_back_through_the_parser() {
        let result = github_result("https://github.com/owner/repo/blob/main/skills/demo/SKILL.md")
            .expect("a valid URL yields an installable result");
        assert_eq!(result.install_source.strip_prefix("github:").map(parse_github_skill_url).flatten().is_some(), true);
        assert_eq!(result.author, "owner");
        assert_eq!(result.name, "repo");
    }

    #[test]
    fn claude_plugins_entries_without_a_resolvable_directory_are_dropped() {
        let body = serde_json::json!({
            "skills": [
                { "id": "a", "name": "No metadata" },
                {
                    "id": "b",
                    "name": "Has metadata",
                    "metadata": { "repoOwner": "o", "repoName": "r", "directoryPath": "skills/b" }
                }
            ]
        });
        let results = normalize_claude_plugins(&body).expect("the response shape is valid");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].install_source, "claude-plugins:o/r/skills/b");
    }

    #[test]
    fn claude_plugins_falls_back_to_the_tree_url_for_a_directory() {
        let body = serde_json::json!({
            "skills": [{
                "id": "c",
                "name": "From tree URL",
                "sourceUrl": "https://github.com/o/r/tree/main/skills/c",
                "metadata": { "repoOwner": "o", "repoName": "r" }
            }]
        });
        let results = normalize_claude_plugins(&body).expect("the response shape is valid");
        assert_eq!(results[0].install_source, "claude-plugins:o/r/skills/c");
    }

    #[test]
    fn a_missing_top_level_array_fails_the_whole_source() {
        assert!(normalize_skills_sh(&serde_json::json!({ "oops": [] })).is_none());
        assert!(normalize_clawhub(&serde_json::json!({})).is_none());
    }

    #[test]
    fn skills_sh_requires_a_three_segment_identifier() {
        let body = serde_json::json!({
            "skills": [
                { "id": "owner/repo", "name": "Too short", "source": "owner/repo", "installs": 1 },
                { "id": "owner/repo/skill", "name": "Good", "source": "owner/repo", "installs": 7 }
            ]
        });
        let results = normalize_skills_sh(&body).expect("the response shape is valid");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].install_source, "skills.sh:owner/repo/skill");
        assert_eq!(results[0].downloads, 7);
        assert_eq!(results[0].author, "owner");
    }

    #[test]
    fn install_handles_are_rejected_before_git_runs() {
        assert!(fetch("skills.sh:owner/repo").is_err());
        assert!(fetch("clawhub:owner/slug/extra").is_err());
        assert!(fetch("claude-plugins:owner/repo").is_err());
        assert!(fetch("unknown:whatever").is_err());
        assert!(fetch("no-colon").is_err());
    }
}
