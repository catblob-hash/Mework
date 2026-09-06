//! In-app updates sourced from GitHub Releases.
//!
//! Mework has no update server. A release is a GitHub Release tagged `v<version>` that carries
//! the NSIS installer (`*-setup.exe`), the portable archive (`*_portable.zip`) and, optionally,
//! a `SHA256SUMS` file (written by `scripts/release-assets.mjs`). This module is the client
//! half: check, download, verify, hand off. Every decision that leads to a file write or a
//! process launch is a pure function here so it can be tested; the Tauri commands in `lib.rs`
//! only wire them to the app handle.
//!
//! Trust model: TLS to GitHub is the integrity anchor, exactly as when a user downloads from
//! the release page by hand. `SHA256SUMS` comes from the same origin, so it catches a corrupt
//! or truncated transfer, not a compromised account. There is no signature scheme.
//!
//! Installer hand-off mirrors tauri-plugin-updater: the downloaded NSIS installer is started
//! through `ShellExecuteW` with `/P /UPDATE /R` (passive UI, keep user data and shortcuts,
//! relaunch afterwards) and the running app exits so the installer can replace it.

use std::{
    fmt, fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use reqwest::{
    blocking::{Client, Response},
    header::{ACCEPT, CONTENT_LENGTH, USER_AGENT},
    redirect::Policy,
    StatusCode, Url,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Source repository as declared in Cargo.toml. Releases are read from its GitHub API.
pub const REPOSITORY_URL: &str = env!("CARGO_PKG_REPOSITORY");

/// GitHub answers unauthenticated API calls quickly; a slow answer means a proxy problem the
/// user should see rather than wait through.
const API_TIMEOUT: Duration = Duration::from_secs(30);
/// Per-operation limit for the asset download (connect, headers, and each body read), not a
/// total. A slow link keeps a 50 MiB transfer alive as long as bytes keep arriving.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(60);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_REDIRECTS: usize = 5;
/// A release JSON body with long notes is a few hundred KiB.
const MAX_RELEASE_BODY: usize = 4 * 1024 * 1024;
/// A checksum list is a handful of lines.
const MAX_CHECKSUMS_BODY: usize = 64 * 1024;
/// Anything larger than this is not a Mework release asset.
const MAX_ASSET_SIZE: u64 = 2 * 1024 * 1024 * 1024;
const MAX_ASSET_NAME_LENGTH: usize = 128;
const DOWNLOAD_CHUNK: usize = 64 * 1024;
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

/// How this copy of Mework got onto the machine. Decides which release asset applies and what
/// "install" means afterwards.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallFlavor {
    /// NSIS per-machine install; `uninstall.exe` sits next to the executable.
    Installer,
    /// Unzipped anywhere; the user replaces the files themselves.
    Portable,
}

/// The NSIS installer always writes its uninstaller beside the main binary, and nothing else
/// puts an `uninstall.exe` there, so its presence is the flavor.
pub fn detect_flavor(executable_dir: &Path) -> InstallFlavor {
    if executable_dir.join("uninstall.exe").is_file() {
        InstallFlavor::Installer
    } else {
        InstallFlavor::Portable
    }
}

/// Architecture token used in release asset names (`Mework_1.0.0_x64-setup.exe`).
pub fn arch_token() -> &'static str {
    arch_token_for(std::env::consts::ARCH)
}

fn arch_token_for(arch: &str) -> &'static str {
    match arch {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        "x86" => "x86",
        _ => "unknown",
    }
}

const KNOWN_ARCH_TOKENS: &[&str] = &["x64", "x86_64", "amd64", "arm64", "aarch64", "x86", "ia32"];

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppVersionInfo {
    pub version: String,
    pub flavor: InstallFlavor,
    /// `cargo build` without `--release`; the binary was never shipped as a release asset.
    pub development_build: bool,
    pub arch: String,
    pub os: String,
    pub repository_url: String,
    pub releases_url: String,
    pub executable_dir: String,
}

pub fn version_info(version: &str, executable_dir: &Path) -> AppVersionInfo {
    AppVersionInfo {
        version: version.to_owned(),
        flavor: detect_flavor(executable_dir),
        development_build: cfg!(debug_assertions),
        arch: std::env::consts::ARCH.to_owned(),
        os: std::env::consts::OS.to_owned(),
        repository_url: REPOSITORY_URL.to_owned(),
        releases_url: format!("{}/releases", REPOSITORY_URL.trim_end_matches('/')),
        executable_dir: executable_dir.display().to_string(),
    }
}

/// `owner/repo` from the Cargo `repository` URL. Only GitHub is supported; the release API
/// shape below is GitHub's.
pub fn repository_slug(repository_url: &str) -> Result<(String, String), String> {
    let url = Url::parse(repository_url.trim())
        .map_err(|error| format!("Cargo.toml 的 repository 不是合法 URL: {error}"))?;
    if url.host_str() != Some("github.com") {
        return Err("只支持托管在 github.com 的仓库".to_owned());
    }
    let mut segments = url
        .path_segments()
        .ok_or_else(|| "repository URL 缺少路径".to_owned())?
        .filter(|segment| !segment.is_empty());
    let owner = segments
        .next()
        .ok_or_else(|| "repository URL 缺少仓库所有者".to_owned())?;
    let repo = segments
        .next()
        .ok_or_else(|| "repository URL 缺少仓库名".to_owned())?
        .trim_end_matches(".git");
    if owner.is_empty() || repo.is_empty() {
        return Err("repository URL 缺少仓库所有者或仓库名".to_owned());
    }
    Ok((owner.to_owned(), repo.to_owned()))
}

// ---------------------------------------------------------------------------------------------
// Versions

/// The subset of semver a release tag carries: `v1.2.3`, `1.2.3-beta.1`, build metadata after
/// `+` ignored. A prerelease orders before its release; identifiers compare the semver way.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Version {
    major: u64,
    minor: u64,
    patch: u64,
    pre: Vec<String>,
}

impl Version {
    pub fn parse(text: &str) -> Option<Version> {
        let text = text.trim();
        let text = text
            .strip_prefix('v')
            .or_else(|| text.strip_prefix('V'))
            .unwrap_or(text);
        let text = text.split('+').next().unwrap_or(text);
        let (core, pre) = match text.split_once('-') {
            Some((core, pre)) => (core, Some(pre)),
            None => (text, None),
        };
        let mut numbers = core.split('.');
        let major = numbers.next()?.parse::<u64>().ok()?;
        let minor = numbers.next()?.parse::<u64>().ok()?;
        let patch = numbers.next()?.parse::<u64>().ok()?;
        if numbers.next().is_some() {
            return None;
        }
        let pre = match pre {
            Some(pre) => {
                if pre.is_empty() {
                    return None;
                }
                let identifiers = pre.split('.').map(str::to_owned).collect::<Vec<_>>();
                if identifiers.iter().any(|identifier| {
                    identifier.is_empty()
                        || !identifier
                            .chars()
                            .all(|character| character.is_ascii_alphanumeric() || character == '-')
                }) {
                    return None;
                }
                identifiers
            }
            None => Vec::new(),
        };
        Some(Version {
            major,
            minor,
            patch,
            pre,
        })
    }

    /// `1.2.3` or `1.2.3-beta.1`: the tag without its `v`, for display and asset matching.
    pub fn to_plain_string(&self) -> String {
        let core = format!("{}.{}.{}", self.major, self.minor, self.patch);
        if self.pre.is_empty() {
            core
        } else {
            format!("{core}-{}", self.pre.join("."))
        }
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        let core = (self.major, self.minor, self.patch).cmp(&(other.major, other.minor, other.patch));
        if core != Ordering::Equal {
            return core;
        }
        match (self.pre.is_empty(), other.pre.is_empty()) {
            (true, true) => Ordering::Equal,
            // A release outranks any prerelease of the same core.
            (true, false) => Ordering::Greater,
            (false, true) => Ordering::Less,
            (false, false) => {
                for (left, right) in self.pre.iter().zip(other.pre.iter()) {
                    let ordering = match (left.parse::<u64>(), right.parse::<u64>()) {
                        (Ok(left), Ok(right)) => left.cmp(&right),
                        // Numeric identifiers always order before alphanumeric ones.
                        (Ok(_), Err(_)) => Ordering::Less,
                        (Err(_), Ok(_)) => Ordering::Greater,
                        (Err(_), Err(_)) => left.cmp(right),
                    };
                    if ordering != Ordering::Equal {
                        return ordering;
                    }
                }
                self.pre.len().cmp(&other.pre.len())
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Release lookup

/// One downloadable file on a release, as the renderer sees it and hands it back for download.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseAsset {
    pub name: String,
    pub download_url: String,
    pub size: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseSummary {
    pub tag: String,
    pub name: String,
    pub html_url: String,
    /// Release body as GitHub stores it: Markdown.
    pub notes: String,
    pub published_at: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateCheck {
    pub current_version: String,
    pub latest_version: String,
    pub update_available: bool,
    pub release: ReleaseSummary,
    /// The asset for this machine's flavor and architecture, when the release has one.
    pub asset: Option<ReleaseAsset>,
    /// `SHA256SUMS` when the release publishes one.
    pub checksums_asset: Option<ReleaseAsset>,
    pub checked_at: String,
}

/// The fields of GitHub's release object this module reads. Unknown fields are ignored.
#[derive(Clone, Debug, Deserialize)]
pub struct GithubRelease {
    pub tag_name: String,
    #[serde(default)]
    pub name: Option<String>,
    pub html_url: String,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub published_at: Option<String>,
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub assets: Vec<GithubReleaseAsset>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct GithubReleaseAsset {
    pub name: String,
    pub browser_download_url: String,
    pub size: u64,
}

impl From<GithubReleaseAsset> for ReleaseAsset {
    fn from(asset: GithubReleaseAsset) -> Self {
        ReleaseAsset {
            name: asset.name,
            download_url: asset.browser_download_url,
            size: asset.size,
        }
    }
}

fn name_tokens(name: &str) -> Vec<String> {
    name.to_ascii_lowercase()
        .split(['_', '-', '.', ' '])
        .filter(|token| !token.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Picks the release file for `flavor` on `arch_token`.
///
/// Installer assets end in `-setup.exe`; portable ones are `.zip` files named `portable`.
/// Among candidates the one carrying this machine's architecture token wins. A release that
/// names no architecture at all with a single candidate is accepted too, so a future rename
/// does not silently disable updates; two unlabelled candidates are ambiguous and match none.
pub fn select_asset(
    assets: &[ReleaseAsset],
    flavor: InstallFlavor,
    arch_token: &str,
) -> Option<ReleaseAsset> {
    let arch_token = arch_token.to_ascii_lowercase();
    let candidates = assets
        .iter()
        .filter(|asset| {
            let lower = asset.name.to_ascii_lowercase();
            match flavor {
                InstallFlavor::Installer => {
                    lower.ends_with("-setup.exe") || lower.ends_with("_setup.exe")
                }
                InstallFlavor::Portable => lower.ends_with(".zip") && lower.contains("portable"),
            }
        })
        .collect::<Vec<_>>();
    if let Some(matching) = candidates
        .iter()
        .find(|asset| name_tokens(&asset.name).iter().any(|token| *token == arch_token))
    {
        return Some((*matching).clone());
    }
    let unlabelled = candidates
        .iter()
        .filter(|asset| {
            !name_tokens(&asset.name)
                .iter()
                .any(|token| KNOWN_ARCH_TOKENS.contains(&token.as_str()))
        })
        .collect::<Vec<_>>();
    match unlabelled.as_slice() {
        [only] => Some((**only).clone()),
        _ => None,
    }
}

pub fn select_checksums_asset(assets: &[ReleaseAsset]) -> Option<ReleaseAsset> {
    assets
        .iter()
        .find(|asset| {
            let lower = asset.name.to_ascii_lowercase();
            lower == "sha256sums" || lower == "sha256sums.txt"
        })
        .cloned()
}

/// Pure half of the update check: everything after the HTTP response is parsed.
pub fn build_update_check(
    current_version: &str,
    flavor: InstallFlavor,
    arch_token: &str,
    release: GithubRelease,
    checked_at: String,
) -> Result<UpdateCheck, String> {
    if release.draft {
        return Err("GitHub 返回的最新发布还是草稿".to_owned());
    }
    let current = Version::parse(current_version)
        .ok_or_else(|| format!("当前版本号 {current_version} 不是合法的语义化版本"))?;
    let latest = Version::parse(&release.tag_name).ok_or_else(|| {
        format!(
            "最新发布的标签 {} 不是 v<主>.<次>.<修订> 形式，无法比较版本",
            release.tag_name
        )
    })?;
    let assets = release
        .assets
        .into_iter()
        .map(ReleaseAsset::from)
        .collect::<Vec<_>>();
    let asset = select_asset(&assets, flavor, arch_token);
    let checksums_asset = select_checksums_asset(&assets);
    Ok(UpdateCheck {
        current_version: current.to_plain_string(),
        latest_version: latest.to_plain_string(),
        update_available: latest > current,
        release: ReleaseSummary {
            tag: release.tag_name.clone(),
            name: release
                .name
                .filter(|name| !name.trim().is_empty())
                .unwrap_or(release.tag_name),
            html_url: release.html_url,
            notes: release.body.unwrap_or_default(),
            published_at: release.published_at.unwrap_or_default(),
        },
        asset,
        checksums_asset,
        checked_at,
    })
}

fn user_agent(current_version: &str) -> String {
    format!("Mework/{current_version} (+{REPOSITORY_URL})")
}

/// Only GitHub and its asset CDN may be contacted, on HTTPS. Release JSON is fetched from the
/// API host; `browser_download_url` lives on `github.com` and redirects to
/// `objects.githubusercontent.com` (or `release-assets.githubusercontent.com`).
pub fn is_allowed_download_url(url: &Url) -> bool {
    if url.scheme() != "https" {
        return false;
    }
    match url.host_str() {
        Some(host) => {
            let host = host.to_ascii_lowercase();
            host == "github.com"
                || host == "api.github.com"
                || host.ends_with(".githubusercontent.com")
        }
        None => false,
    }
}

fn github_redirect_policy() -> Policy {
    Policy::custom(|attempt| {
        if attempt.previous().len() >= MAX_REDIRECTS {
            return attempt.error("重定向次数过多");
        }
        if is_allowed_download_url(attempt.url()) {
            attempt.follow()
        } else {
            attempt.stop()
        }
    })
}

fn client(timeout: Duration, current_version: &str) -> Result<Client, String> {
    Client::builder()
        .timeout(timeout)
        .connect_timeout(CONNECT_TIMEOUT)
        .redirect(github_redirect_policy())
        .user_agent(user_agent(current_version))
        .build()
        .map_err(|error| format!("无法初始化更新客户端: {error}"))
}

fn read_limited(mut response: Response, limit: usize) -> Result<Vec<u8>, String> {
    let mut body = Vec::new();
    response
        .by_ref()
        .take((limit + 1) as u64)
        .read_to_end(&mut body)
        .map_err(|error| format!("读取 GitHub 响应失败: {error}"))?;
    if body.len() > limit {
        return Err(format!("GitHub 响应超过 {limit} 字节上限"));
    }
    Ok(body)
}

fn rate_limit_message(response: &Response) -> Option<String> {
    let remaining = response
        .headers()
        .get("x-ratelimit-remaining")?
        .to_str()
        .ok()?;
    if remaining.trim() != "0" {
        return None;
    }
    let reset = response
        .headers()
        .get("x-ratelimit-reset")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<i64>().ok())
        .and_then(|seconds| chrono::DateTime::<chrono::Utc>::from_timestamp(seconds, 0))
        .map(|instant| {
            instant
                .with_timezone(&chrono::Local)
                .format("%H:%M")
                .to_string()
        });
    Some(match reset {
        Some(reset) => format!("GitHub API 的匿名请求配额已用完，{reset} 后恢复"),
        None => "GitHub API 的匿名请求配额已用完，请稍后再试".to_owned(),
    })
}

/// Asks GitHub for the latest non-prerelease, non-draft release and compares it with
/// `current_version`.
///
/// The REST API is the primary source because it names the assets and carries the release
/// notes. Its anonymous quota is 60 calls an hour per address, and a developer machine or a
/// shared office egress can exhaust that with unrelated traffic; when it is exhausted the
/// redirect-based fallback below still answers the one question that matters — is there a
/// newer version, and where is its file — without the notes.
pub fn check_for_update(current_version: &str, flavor: InstallFlavor) -> Result<UpdateCheck, String> {
    let (owner, repo) = repository_slug(REPOSITORY_URL)?;
    let url = format!("https://api.github.com/repos/{owner}/{repo}/releases/latest");
    let client = client(API_TIMEOUT, current_version)?;
    let response = client
        .get(&url)
        .header(ACCEPT, "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .map_err(|error| format!("无法连接 GitHub 检查更新: {error}"))?;
    let status = response.status();
    if status == StatusCode::NOT_FOUND {
        return Err(format!("{owner}/{repo} 还没有正式发布的版本"));
    }
    if status == StatusCode::FORBIDDEN || status == StatusCode::TOO_MANY_REQUESTS {
        if let Some(message) = rate_limit_message(&response) {
            return check_via_release_redirects(&client, &owner, &repo, current_version, flavor)
                .map_err(|fallback| format!("{message}；改走发布页也失败了：{fallback}"));
        }
    }
    if !status.is_success() {
        let body = read_limited(response, MAX_RELEASE_BODY).unwrap_or_default();
        return Err(crate::http_util::api_error_message(status, &body));
    }
    let body = read_limited(response, MAX_RELEASE_BODY)?;
    let release = serde_json::from_slice::<GithubRelease>(&body)
        .map_err(|error| format!("GitHub 发布信息无法解析: {error}"))?;
    build_update_check(
        current_version,
        flavor,
        arch_token(),
        release,
        chrono::Utc::now().to_rfc3339(),
    )
}

/// The tag a `/releases/latest` redirect landed on: `/owner/repo/releases/tag/<tag>`.
pub fn tag_from_release_url(url: &Url) -> Option<String> {
    let segments = url.path_segments()?.collect::<Vec<_>>();
    match segments.as_slice() {
        [_, _, "releases", "tag", tag] if !tag.is_empty() => {
            percent_decode(tag).filter(|decoded| !decoded.is_empty())
        }
        _ => None,
    }
}

fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = bytes.get(index + 1..index + 3)?;
            let value = u8::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok()?;
            decoded.push(value);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

/// File names a release built by `scripts/release-assets.mjs` carries for `version`.
pub fn conventional_asset_names(version: &str, arch_token: &str) -> (String, String, &'static str) {
    (
        format!("Mework_{version}_{arch_token}-setup.exe"),
        format!("Mework_{version}_{arch_token}_portable.zip"),
        "SHA256SUMS",
    )
}

/// `HEAD` on a `releases/download/...` URL: GitHub answers 302 to its CDN, which reports the
/// size. A 404 means the release has no such file.
fn probe_asset(client: &Client, download_url: &str, name: &str) -> Option<ReleaseAsset> {
    let response = client.head(download_url).send().ok()?;
    if !response.status().is_success() {
        return None;
    }
    let size = response
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())?;
    Some(ReleaseAsset {
        name: name.to_owned(),
        download_url: download_url.to_owned(),
        size,
    })
}

/// Update check without the REST API: `/releases/latest` redirects to the latest release's
/// tag page, and the assets are found by their conventional names. No notes are available
/// this way; the page link stands in for them.
fn check_via_release_redirects(
    client: &Client,
    owner: &str,
    repo: &str,
    current_version: &str,
    flavor: InstallFlavor,
) -> Result<UpdateCheck, String> {
    let latest_url = format!("https://github.com/{owner}/{repo}/releases/latest");
    let response = client
        .head(&latest_url)
        .send()
        .map_err(|error| format!("无法访问发布页: {error}"))?;
    if response.status() == StatusCode::NOT_FOUND {
        return Err(format!("{owner}/{repo} 还没有正式发布的版本"));
    }
    if !response.status().is_success() {
        return Err(format!("发布页返回 HTTP {}", response.status().as_u16()));
    }
    let tag = tag_from_release_url(response.url())
        .ok_or_else(|| format!("发布页没有跳转到某个标签页（停在 {}）", response.url()))?;
    let latest = Version::parse(&tag)
        .ok_or_else(|| format!("最新发布的标签 {tag} 不是 v<主>.<次>.<修订> 形式，无法比较版本"))?;
    let (installer, portable, checksums) =
        conventional_asset_names(&latest.to_plain_string(), arch_token());
    let download_base = format!("https://github.com/{owner}/{repo}/releases/download/{tag}");
    let wanted = match flavor {
        InstallFlavor::Installer => installer,
        InstallFlavor::Portable => portable,
    };
    let asset = probe_asset(client, &format!("{download_base}/{wanted}"), &wanted);
    let checksums_asset = probe_asset(client, &format!("{download_base}/{checksums}"), checksums);
    let release = GithubRelease {
        tag_name: tag.clone(),
        name: None,
        html_url: response.url().to_string(),
        body: None,
        published_at: None,
        draft: false,
        assets: asset
            .into_iter()
            .chain(checksums_asset)
            .map(|asset| GithubReleaseAsset {
                name: asset.name,
                browser_download_url: asset.download_url,
                size: asset.size,
            })
            .collect(),
    };
    build_update_check(
        current_version,
        flavor,
        arch_token(),
        release,
        chrono::Utc::now().to_rfc3339(),
    )
}

// ---------------------------------------------------------------------------------------------
// Download

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DownloadEvent {
    #[serde(rename_all = "camelCase")]
    Progress {
        received_bytes: u64,
        total_bytes: u64,
    },
    Verifying,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verification {
    /// The file's SHA-256 matched the release's `SHA256SUMS` entry.
    Verified,
    /// The release publishes no `SHA256SUMS` at all; TLS is the only integrity check. A list
    /// that exists but omits the file is a download error, not this state.
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadedUpdate {
    pub path: String,
    pub file_name: String,
    pub size_bytes: u64,
    pub sha256: String,
    pub verification: Verification,
    pub flavor: InstallFlavor,
}

pub struct DownloadRequest {
    pub asset: ReleaseAsset,
    pub checksums_asset: Option<ReleaseAsset>,
    pub flavor: InstallFlavor,
    pub destination_dir: PathBuf,
    pub current_version: String,
}

/// The message a cancelled download fails with. The renderer initiated the cancel and matches
/// on this to stay quiet about it.
pub const CANCELLED_MESSAGE: &str = "下载已取消";

/// Release asset names are used as file names on disk, so they must be plain: no separators,
/// no leading dot, ASCII letters, digits, `.`, `_`, `-` only.
pub fn validate_asset_file_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > MAX_ASSET_NAME_LENGTH {
        return Err("发布资产的文件名为空或过长".to_owned());
    }
    if name.starts_with('.') {
        return Err(format!("发布资产的文件名 {name} 不能以点开头"));
    }
    if !name
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-'))
    {
        return Err(format!("发布资产的文件名 {name} 含有不允许的字符"));
    }
    Ok(())
}

/// A release file is accepted only at its canonical address in this repository:
/// `https://github.com/{owner}/{repo}/releases/download/{tag}/{name}`, with `{name}` equal to
/// the asset's own name and `{tag}` a version tag. This is what GitHub's API reports as
/// `browser_download_url` and what the redirect fallback constructs, so nothing legitimate is
/// lost — and the renderer cannot point the host at another repository's executable, which
/// the host would otherwise download, record, and launch on the user's behalf.
///
/// Returns the tag segment so a checksum file can be required to come from the same release.
pub fn validate_release_download_url(url: &Url, expected_name: &str) -> Result<String, String> {
    let (owner, repo) = repository_slug(REPOSITORY_URL)?;
    if url.scheme() != "https" {
        return Err("发布资产只能通过 HTTPS 下载".to_owned());
    }
    if !url.host_str().is_some_and(|host| host.eq_ignore_ascii_case("github.com")) {
        return Err(format!(
            "只从 GitHub 下载更新，拒绝 {}",
            url.host_str().unwrap_or("未知主机")
        ));
    }
    if url.port().is_some() || !url.username().is_empty() || url.password().is_some() {
        return Err("发布资产的下载地址不能带端口或凭据".to_owned());
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err("发布资产的下载地址不能带查询参数或片段".to_owned());
    }
    let segments = url
        .path_segments()
        .map(|segments| segments.collect::<Vec<_>>())
        .unwrap_or_default();
    let tag = match segments.as_slice() {
        [url_owner, url_repo, "releases", "download", tag, name]
            if url_owner.eq_ignore_ascii_case(&owner)
                && url_repo.eq_ignore_ascii_case(&repo)
                && *name == expected_name
                && Version::parse(tag).is_some() =>
        {
            (*tag).to_owned()
        }
        _ => {
            return Err(format!(
                "{} 不是 {owner}/{repo} 发布页上 {expected_name} 的下载地址",
                url.as_str()
            ))
        }
    };
    Ok(tag)
}

pub fn validate_asset(asset: &ReleaseAsset, flavor: InstallFlavor) -> Result<Url, String> {
    validate_asset_file_name(&asset.name)?;
    let lower = asset.name.to_ascii_lowercase();
    let expected = match flavor {
        InstallFlavor::Installer => ".exe",
        InstallFlavor::Portable => ".zip",
    };
    if !lower.ends_with(expected) {
        return Err(format!(
            "{} 不是当前安装形态需要的 {expected} 文件",
            asset.name
        ));
    }
    if asset.size == 0 || asset.size > MAX_ASSET_SIZE {
        return Err(format!("发布资产 {} 的大小 {} 不合理", asset.name, asset.size));
    }
    let url = Url::parse(&asset.download_url)
        .map_err(|error| format!("发布资产的下载地址无法解析: {error}"))?;
    validate_release_download_url(&url, &asset.name)?;
    Ok(url)
}

/// Parses GNU `sha256sum` output: `<hex>  <name>` or `<hex> *<name>`, LF or CRLF, blank lines
/// and `#` comments ignored. Returns `(name, lowercase hex)` pairs.
pub fn parse_sha256sums(text: &str) -> Result<Vec<(String, String)>, String> {
    let mut entries = Vec::new();
    for (index, raw_line) in text.lines().enumerate() {
        let line = raw_line.trim_end_matches('\r').trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (hex, rest) = line
            .split_once(char::is_whitespace)
            .ok_or_else(|| format!("SHA256SUMS 第 {} 行缺少文件名", index + 1))?;
        if hex.len() != 64 || !hex.chars().all(|character| character.is_ascii_hexdigit()) {
            return Err(format!("SHA256SUMS 第 {} 行的摘要不是 64 位十六进制", index + 1));
        }
        let name = rest.trim_start().trim_start_matches('*').trim();
        if name.is_empty() {
            return Err(format!("SHA256SUMS 第 {} 行缺少文件名", index + 1));
        }
        entries.push((name.to_owned(), hex.to_ascii_lowercase()));
    }
    Ok(entries)
}

/// Looks `file_name` up in a parsed checksum list.
pub fn expected_sha256<'a>(entries: &'a [(String, String)], file_name: &str) -> Option<&'a str> {
    entries
        .iter()
        .find(|(name, _)| name == file_name)
        .map(|(_, hex)| hex.as_str())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn remove_if_exists(path: &Path) {
    if path.exists() {
        let _ = fs::remove_file(path);
    }
}

/// Deletes earlier installers left in the app-private `updates` directory so a user who
/// updates several times does not accumulate a 35 MiB file per version. Only files are
/// touched, only in this directory, and never the one about to be written. The portable
/// flavor downloads to the user's own Downloads folder, where nothing is ever pruned.
pub fn prune_stale_downloads(directory: &Path, keep_name: &str) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let stale = name != keep_name
            && (name.to_ascii_lowercase().ends_with(".exe") || name.ends_with(".part"));
        if stale && entry.file_type().is_ok_and(|kind| kind.is_file()) {
            let _ = fs::remove_file(entry.path());
        }
    }
}

fn fetch_checksums(
    client: &Client,
    asset: &ReleaseAsset,
    release_tag: &str,
) -> Result<Vec<(String, String)>, String> {
    validate_asset_file_name(&asset.name)?;
    let url = Url::parse(&asset.download_url)
        .map_err(|error| format!("校验和文件的下载地址无法解析: {error}"))?;
    // The checksum list must come from the very release the file came from; a list from
    // another release would either fail to match or, worse, vouch for the wrong bytes.
    let tag = validate_release_download_url(&url, &asset.name)?;
    if tag != release_tag {
        return Err(format!(
            "校验和文件来自发布 {tag}，而下载的文件来自 {release_tag}"
        ));
    }
    let response = client
        .get(url)
        .send()
        .map_err(|error| format!("下载校验和文件失败: {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "下载校验和文件失败（HTTP {}）",
            response.status().as_u16()
        ));
    }
    let body = read_limited(response, MAX_CHECKSUMS_BODY)?;
    parse_sha256sums(&String::from_utf8_lossy(&body))
}

/// Downloads `request.asset` into `request.destination_dir`, streaming progress and honouring
/// `cancel`. The file is written as `<name>.part` and renamed only after the size matches and,
/// when the release has a `SHA256SUMS`, the digest matches too.
pub fn download_update(
    request: DownloadRequest,
    cancel: &AtomicBool,
    mut progress: impl FnMut(DownloadEvent),
) -> Result<DownloadedUpdate, String> {
    let url = validate_asset(&request.asset, request.flavor)?;
    let release_tag = validate_release_download_url(&url, &request.asset.name)?;
    fs::create_dir_all(&request.destination_dir)
        .map_err(|error| format!("无法创建下载目录 {}: {error}", request.destination_dir.display()))?;
    if request.flavor == InstallFlavor::Installer {
        prune_stale_downloads(&request.destination_dir, &request.asset.name);
    }
    let final_path = request.destination_dir.join(&request.asset.name);
    let part_path = request
        .destination_dir
        .join(format!("{}.part", request.asset.name));
    remove_if_exists(&part_path);

    let client = client(DOWNLOAD_TIMEOUT, &request.current_version)?;
    let mut response = client
        .get(url)
        .header(ACCEPT, "application/octet-stream")
        .header(USER_AGENT, user_agent(&request.current_version))
        .send()
        .map_err(|error| format!("下载更新失败: {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "下载更新失败（HTTP {}）",
            response.status().as_u16()
        ));
    }
    if let Some(length) = response
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
    {
        if length != request.asset.size {
            return Err(format!(
                "GitHub 返回的文件大小 {length} 与发布信息中的 {} 不一致",
                request.asset.size
            ));
        }
    }

    let total = request.asset.size;
    let mut file = fs::File::create(&part_path)
        .map_err(|error| format!("无法写入 {}: {error}", part_path.display()))?;
    let mut hasher = Sha256::new();
    let mut received: u64 = 0;
    let mut buffer = vec![0u8; DOWNLOAD_CHUNK];
    let mut last_report = Instant::now();
    progress(DownloadEvent::Progress {
        received_bytes: 0,
        total_bytes: total,
    });
    let outcome: Result<(), String> = loop {
        if cancel.load(Ordering::Acquire) {
            break Err(CANCELLED_MESSAGE.to_owned());
        }
        let read = match response.read(&mut buffer) {
            Ok(0) => break Ok(()),
            Ok(read) => read,
            Err(error) => break Err(format!("下载更新中断: {error}")),
        };
        received += read as u64;
        if received > total {
            break Err(format!("下载的数据超过发布信息中的 {total} 字节"));
        }
        if let Err(error) = file.write_all(&buffer[..read]) {
            break Err(format!("写入 {} 失败: {error}", part_path.display()));
        }
        hasher.update(&buffer[..read]);
        if last_report.elapsed() >= PROGRESS_INTERVAL {
            last_report = Instant::now();
            progress(DownloadEvent::Progress {
                received_bytes: received,
                total_bytes: total,
            });
        }
    };
    if let Err(message) = outcome {
        drop(file);
        remove_if_exists(&part_path);
        return Err(message);
    }
    if let Err(error) = file.flush().and_then(|()| file.sync_all()) {
        drop(file);
        remove_if_exists(&part_path);
        return Err(format!("落盘 {} 失败: {error}", part_path.display()));
    }
    drop(file);
    if received != total {
        remove_if_exists(&part_path);
        return Err(format!(
            "下载不完整：收到 {received} 字节，发布信息中是 {total} 字节"
        ));
    }
    progress(DownloadEvent::Progress {
        received_bytes: received,
        total_bytes: total,
    });
    let digest = hex(&hasher.finalize());

    let verification = match &request.checksums_asset {
        Some(checksums_asset) => {
            progress(DownloadEvent::Verifying);
            let entries = match fetch_checksums(&client, checksums_asset, &release_tag) {
                Ok(entries) => entries,
                Err(message) => {
                    remove_if_exists(&part_path);
                    return Err(message);
                }
            };
            match expected_sha256(&entries, &request.asset.name) {
                Some(expected) if expected == digest => Verification::Verified,
                Some(_) => {
                    remove_if_exists(&part_path);
                    return Err(format!(
                        "{} 的 SHA-256 与发布的 SHA256SUMS 不一致，已删除下载的文件",
                        request.asset.name
                    ));
                }
                // A published list that skips this file is not "no checksum": either the
                // release was assembled by hand and is incomplete, or the file was swapped after
                // the list was written. Neither is a file to install.
                None => {
                    remove_if_exists(&part_path);
                    return Err(format!(
                        "发布的 SHA256SUMS 里没有 {} 的条目，已删除下载的文件",
                        request.asset.name
                    ));
                }
            }
        }
        None => Verification::Unavailable,
    };

    // A same-named file from an earlier download is not worth keeping: `begin_download` already
    // made it uninstallable, so losing it if the rename below fails costs nothing.
    remove_if_exists(&final_path);
    fs::rename(&part_path, &final_path).map_err(|error| {
        remove_if_exists(&part_path);
        format!("无法重命名 {}: {error}", part_path.display())
    })?;
    Ok(DownloadedUpdate {
        path: final_path.display().to_string(),
        file_name: request.asset.name,
        size_bytes: received,
        sha256: digest,
        verification,
        flavor: request.flavor,
    })
}

// ---------------------------------------------------------------------------------------------
// Session: one download at a time, install only what was downloaded

/// Process-local update state. One download may be in flight; the last successful download is
/// the only file `install` will ever launch or reveal.
#[derive(Default)]
pub struct UpdateSession {
    inner: Mutex<SessionInner>,
}

#[derive(Default)]
struct SessionInner {
    in_flight: Option<Arc<AtomicBool>>,
    downloaded: Option<DownloadedUpdate>,
}

impl UpdateSession {
    fn lock(&self) -> std::sync::MutexGuard<'_, SessionInner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Claims the download slot. Starting a download also forgets the previous result: the
    /// renderer is about to replace it, and a stale "install" must not launch the old file.
    pub fn begin_download(&self) -> Result<Arc<AtomicBool>, String> {
        let mut inner = self.lock();
        if inner.in_flight.is_some() {
            return Err("已有一个更新正在下载".to_owned());
        }
        let cancel = Arc::new(AtomicBool::new(false));
        inner.in_flight = Some(cancel.clone());
        inner.downloaded = None;
        Ok(cancel)
    }

    pub fn finish_download(&self, result: Option<DownloadedUpdate>) {
        let mut inner = self.lock();
        inner.in_flight = None;
        inner.downloaded = result;
    }

    /// Requests cancellation of the in-flight download, if any. Returns whether one was running.
    pub fn cancel_download(&self) -> bool {
        match &self.lock().in_flight {
            Some(cancel) => {
                cancel.store(true, Ordering::Release);
                true
            }
            None => false,
        }
    }

    /// The renderer may only install the file this session downloaded. A path that differs,
    /// a file that has since disappeared, or one whose bytes no longer hash to what was
    /// downloaded is refused — the last case covers another Mework instance (or anything else
    /// with the user's rights) having replaced the file in the meantime.
    ///
    /// The digest is taken through a handle that stays open in the returned authorization, so
    /// what gets launched is the object that was verified rather than whatever the path resolves
    /// to a moment later.
    pub fn authorize_install(&self, requested_path: &str) -> Result<AuthorizedInstall, String> {
        let downloaded = {
            let inner = self.lock();
            let downloaded = inner
                .downloaded
                .as_ref()
                .ok_or_else(|| "还没有下载好的更新，请先下载".to_owned())?;
            if downloaded.path != requested_path {
                return Err("请求安装的文件不是这次下载的更新".to_owned());
            }
            downloaded.clone()
        };
        let path = Path::new(&downloaded.path);
        if !path.is_file() {
            return Err("下载好的更新文件已不存在，请重新下载".to_owned());
        }
        let mut guard = open_guarded(path)?;
        let (size, digest) = sha256_reader(&mut guard, path)?;
        if size != downloaded.size_bytes || digest != downloaded.sha256 {
            return Err(
                "下载好的更新文件在下载后被改动过，拒绝安装；请重新下载".to_owned(),
            );
        }
        Ok(AuthorizedInstall { downloaded, guard })
    }
}

/// A verified update file, held open so it cannot be swapped before it is launched.
///
/// `authorize_install` used to hash the path and hand back only the download metadata; the
/// launcher then reopened the same path. Anything running with the user's rights could replace
/// the file in between, and `path.is_file()` cannot tell one object from another. On Windows the
/// handle below is opened sharing reads only: while it lives, writing, truncating, deleting and
/// renaming that file all fail with a sharing violation, while reading — including the image load
/// that starts the installer — still succeeds. Dropping this value ends the protection, so the
/// launcher takes it by value and keeps it until the installer process exists.
pub struct AuthorizedInstall {
    pub downloaded: DownloadedUpdate,
    guard: fs::File,
}

impl fmt::Debug for AuthorizedInstall {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The handle is deliberately opaque; only the file it protects is interesting.
        formatter
            .debug_struct("AuthorizedInstall")
            .field("downloaded", &self.downloaded)
            .finish_non_exhaustive()
    }
}

/// Opens a file for reading and, on Windows, denies every other opener write, delete and rename
/// access for as long as the handle lives.
fn open_guarded(path: &Path) -> Result<fs::File, String> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        // Rust's default share mode is read|write|delete, which protects nothing. Sharing reads
        // alone still admits the read and execute access a process launch needs.
        options.share_mode(windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ);
    }
    options
        .open(path)
        .map_err(|error| format!("无法读取 {}: {error}", path.display()))
}

/// Size and lowercase hex SHA-256 of a file on disk. Production code hashes through the guarded
/// handle it is about to launch instead; this exists so tests can state what a file contains.
#[cfg(test)]
fn sha256_file(path: &Path) -> Result<(u64, String), String> {
    let mut file = fs::File::open(path)
        .map_err(|error| format!("无法读取 {}: {error}", path.display()))?;
    sha256_reader(&mut file, path)
}

/// Size and lowercase hex SHA-256 of everything left in `reader`.
fn sha256_reader(reader: &mut impl Read, path: &Path) -> Result<(u64, String), String> {
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; DOWNLOAD_CHUNK];
    let mut size: u64 = 0;
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|error| format!("读取 {} 失败: {error}", path.display()))?;
        if read == 0 {
            break;
        }
        size += read as u64;
        hasher.update(&buffer[..read]);
    }
    Ok((size, hex(&hasher.finalize())))
}

// ---------------------------------------------------------------------------------------------
// Install hand-off

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallAction {
    /// The NSIS installer was started; the app is exiting so it can be replaced.
    InstallerLaunched,
    /// The portable archive was shown in the file manager for the user to unpack.
    Revealed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallOutcome {
    pub action: InstallAction,
}

/// Arguments Tauri's own updater passes to its NSIS installer: passive progress UI, update
/// mode (keep user data, shortcuts and install directory), relaunch when done.
pub const NSIS_UPDATE_ARGUMENTS: &str = "/P /UPDATE /R";

/// Starts the downloaded installer. The installer requests elevation itself (per-machine
/// install), so `ShellExecuteW` — which routes through the UAC prompt — is the only launcher
/// that works; `CreateProcess` would fail with `ERROR_ELEVATION_REQUIRED`.
///
/// Takes the authorization by value and holds its guard across the launch, so the bytes Windows
/// loads are the bytes `authorize_install` hashed. `ShellExecuteW` returns once the process has
/// been created (after the UAC prompt is answered), which is where the protection may end.
#[cfg(windows)]
pub fn launch_installer(authorized: AuthorizedInstall) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt as _;
    use windows_sys::Win32::System::Com::{
        CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE,
    };
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    fn wide(value: &std::ffi::OsStr) -> Vec<u16> {
        value.encode_wide().chain(std::iter::once(0)).collect()
    }

    let AuthorizedInstall { downloaded, guard } = authorized;
    let path = Path::new(&downloaded.path);
    if !path.is_file() {
        return Err(format!("安装程序 {} 不存在", path.display()));
    }
    let verb = wide(std::ffi::OsStr::new("open"));
    let file = wide(path.as_os_str());
    let parameters = wide(std::ffi::OsStr::new(NSIS_UPDATE_ARGUMENTS));
    let handle = std::thread::Builder::new()
        .name("mework-launch-installer".to_owned())
        .spawn(move || {
            // SAFETY: every wide string is NUL-terminated and outlives the call; null pointers
            // request the documented defaults.
            unsafe {
                let com = CoInitializeEx(
                    std::ptr::null(),
                    (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32,
                );
                let result = ShellExecuteW(
                    std::ptr::null_mut(),
                    verb.as_ptr(),
                    file.as_ptr(),
                    parameters.as_ptr(),
                    std::ptr::null(),
                    SW_SHOWNORMAL,
                );
                if com >= 0 {
                    CoUninitialize();
                }
                result as isize
            }
        })
        .map_err(|error| format!("无法启动安装程序的线程: {error}"))?;
    let result = handle
        .join()
        .map_err(|_| "启动安装程序的线程异常退出".to_owned())?;
    // Released only now: everything above ran with the verified file still un-replaceable.
    drop(guard);
    // ShellExecuteW reports success only for values above 32. 5 is access denied, which is
    // what a declined UAC prompt comes back as.
    if result > 32 {
        Ok(())
    } else if result == 5 {
        Err("安装程序没有获得管理员权限（UAC 已取消）".to_owned())
    } else {
        Err(format!("系统拒绝启动安装程序（ShellExecute 返回 {result}）"))
    }
}

#[cfg(not(windows))]
pub fn launch_installer(authorized: AuthorizedInstall) -> Result<(), String> {
    drop(authorized);
    Err("只有 Windows 安装版支持应用内安装更新".to_owned())
}

/// Shows the downloaded file selected in the file manager.
#[cfg(windows)]
pub fn reveal_file(path: &Path) -> Result<(), String> {
    use std::os::windows::process::CommandExt as _;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    if !path.is_file() {
        return Err(format!("文件 {} 不存在", path.display()));
    }
    // `/select,` takes the path in the same argument; Explorer accepts quotes around it, which
    // is what protects a path containing spaces.
    std::process::Command::new("explorer.exe")
        .raw_arg(format!("/select,\"{}\"", path.display()))
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("无法打开文件所在目录: {error}"))
}

#[cfg(not(windows))]
pub fn reveal_file(path: &Path) -> Result<(), String> {
    let directory = path
        .parent()
        .ok_or_else(|| "文件没有所在目录".to_owned())?;
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    std::process::Command::new(opener)
        .arg(directory)
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("无法打开文件所在目录: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asset(name: &str) -> ReleaseAsset {
        ReleaseAsset {
            name: name.to_owned(),
            download_url: format!(
                "https://github.com/catblob-hash/Mework/releases/download/v1.1.0/{name}"
            ),
            size: 1024,
        }
    }

    fn release(tag: &str, assets: &[&str]) -> GithubRelease {
        GithubRelease {
            tag_name: tag.to_owned(),
            name: Some(format!("Mework {tag}")),
            html_url: format!("https://github.com/catblob-hash/Mework/releases/tag/{tag}"),
            body: Some("## Notes".to_owned()),
            published_at: Some("2026-09-05T00:00:00Z".to_owned()),
            draft: false,
            assets: assets
                .iter()
                .map(|name| GithubReleaseAsset {
                    name: (*name).to_owned(),
                    browser_download_url: format!(
                        "https://github.com/catblob-hash/Mework/releases/download/{tag}/{name}"
                    ),
                    size: 4096,
                })
                .collect(),
        }
    }

    #[test]
    fn repository_slug_comes_from_cargo_metadata() {
        assert_eq!(
            repository_slug("https://github.com/catblob-hash/Mework").unwrap(),
            ("catblob-hash".to_owned(), "Mework".to_owned())
        );
        assert_eq!(
            repository_slug("https://github.com/catblob-hash/Mework.git/").unwrap(),
            ("catblob-hash".to_owned(), "Mework".to_owned())
        );
        assert!(repository_slug("https://gitlab.com/a/b").is_err());
        assert!(repository_slug("https://github.com/only-owner").is_err());
        assert!(repository_slug("").is_err());
        // The compiled-in value must itself be usable, or every check fails at runtime.
        repository_slug(REPOSITORY_URL).expect("Cargo.toml repository points at GitHub");
    }

    #[test]
    fn versions_parse_with_optional_v_and_prerelease() {
        let parsed = Version::parse("v1.2.3").unwrap();
        assert_eq!(parsed.to_plain_string(), "1.2.3");
        assert_eq!(Version::parse("1.2.3-beta.1+build.7").unwrap().to_plain_string(), "1.2.3-beta.1");
        assert_eq!(Version::parse(" V10.0.1 ").unwrap().to_plain_string(), "10.0.1");
        for invalid in ["", "1.2", "1.2.3.4", "1.2.x", "v1.2.3-", "1.2.3-a..b", "release-1", "1.2.3-b@1"] {
            assert!(Version::parse(invalid).is_none(), "{invalid:?} must not parse");
        }
    }

    #[test]
    fn versions_order_like_semver() {
        let order = [
            "1.0.0-alpha",
            "1.0.0-alpha.1",
            "1.0.0-alpha.beta",
            "1.0.0-beta",
            "1.0.0-beta.2",
            "1.0.0-beta.11",
            "1.0.0-rc.1",
            "1.0.0",
            "1.0.1",
            "1.1.0",
            "2.0.0",
            "10.0.0",
        ];
        for pair in order.windows(2) {
            let lower = Version::parse(pair[0]).unwrap();
            let higher = Version::parse(pair[1]).unwrap();
            assert!(lower < higher, "{} should be below {}", pair[0], pair[1]);
        }
        assert_eq!(Version::parse("v1.0.0").unwrap(), Version::parse("1.0.0").unwrap());
    }

    #[test]
    fn flavor_follows_the_nsis_uninstaller() {
        let directory = tempfile::tempdir().unwrap();
        assert_eq!(detect_flavor(directory.path()), InstallFlavor::Portable);
        fs::write(directory.path().join("uninstall.exe"), b"stub").unwrap();
        assert_eq!(detect_flavor(directory.path()), InstallFlavor::Installer);
        // A directory named uninstall.exe is not the uninstaller.
        let other = tempfile::tempdir().unwrap();
        fs::create_dir(other.path().join("uninstall.exe")).unwrap();
        assert_eq!(detect_flavor(other.path()), InstallFlavor::Portable);
    }

    #[test]
    fn arch_tokens_match_release_naming() {
        assert_eq!(arch_token_for("x86_64"), "x64");
        assert_eq!(arch_token_for("aarch64"), "arm64");
        assert_eq!(arch_token_for("x86"), "x86");
        assert_eq!(arch_token_for("riscv64"), "unknown");
    }

    #[test]
    fn selects_the_asset_for_flavor_and_architecture() {
        let assets = vec![
            asset("Mework_1.1.0_x64-setup.exe"),
            asset("Mework_1.1.0_arm64-setup.exe"),
            asset("Mework_1.1.0_x64_portable.zip"),
            asset("Mework_1.1.0_arm64_portable.zip"),
            asset("SHA256SUMS"),
            asset("Source code.zip"),
        ];
        assert_eq!(
            select_asset(&assets, InstallFlavor::Installer, "x64").unwrap().name,
            "Mework_1.1.0_x64-setup.exe"
        );
        assert_eq!(
            select_asset(&assets, InstallFlavor::Installer, "arm64").unwrap().name,
            "Mework_1.1.0_arm64-setup.exe"
        );
        assert_eq!(
            select_asset(&assets, InstallFlavor::Portable, "x64").unwrap().name,
            "Mework_1.1.0_x64_portable.zip"
        );
        // No asset for this architecture, and the labelled ones must not be misused.
        assert!(select_asset(&assets, InstallFlavor::Installer, "x86").is_none());
        assert_eq!(select_checksums_asset(&assets).unwrap().name, "SHA256SUMS");
    }

    #[test]
    fn a_single_unlabelled_asset_is_accepted_but_two_are_ambiguous() {
        let single = vec![asset("Mework-setup.exe"), asset("Mework_portable.zip")];
        assert_eq!(
            select_asset(&single, InstallFlavor::Installer, "x64").unwrap().name,
            "Mework-setup.exe"
        );
        assert_eq!(
            select_asset(&single, InstallFlavor::Portable, "x64").unwrap().name,
            "Mework_portable.zip"
        );
        let ambiguous = vec![asset("Mework-a-setup.exe"), asset("Mework-b-setup.exe")];
        assert!(select_asset(&ambiguous, InstallFlavor::Installer, "x64").is_none());
        // The architecture label wins even when an unlabelled sibling exists.
        let mixed = vec![asset("Mework-setup.exe"), asset("Mework_x64-setup.exe")];
        assert_eq!(
            select_asset(&mixed, InstallFlavor::Installer, "x64").unwrap().name,
            "Mework_x64-setup.exe"
        );
        assert!(select_checksums_asset(&single).is_none());
        // `x64` inside a longer token is not the architecture token: the exact label wins over
        // an earlier candidate that merely contains the letters.
        let embedded = vec![
            asset("Mework_0x64beef-setup.exe"),
            asset("Mework_x64-setup.exe"),
        ];
        assert_eq!(
            select_asset(&embedded, InstallFlavor::Installer, "x64").unwrap().name,
            "Mework_x64-setup.exe"
        );
    }

    #[test]
    fn update_check_compares_versions_and_picks_assets() {
        let check = build_update_check(
            "1.0.0",
            InstallFlavor::Installer,
            "x64",
            release(
                "v1.1.0",
                &[
                    "Mework_1.1.0_x64-setup.exe",
                    "Mework_1.1.0_x64_portable.zip",
                    "SHA256SUMS",
                ],
            ),
            "2026-09-05T01:00:00Z".to_owned(),
        )
        .unwrap();
        assert!(check.update_available);
        assert_eq!(check.current_version, "1.0.0");
        assert_eq!(check.latest_version, "1.1.0");
        assert_eq!(check.release.tag, "v1.1.0");
        assert_eq!(check.release.name, "Mework v1.1.0");
        assert_eq!(check.release.notes, "## Notes");
        assert_eq!(check.asset.unwrap().name, "Mework_1.1.0_x64-setup.exe");
        assert_eq!(check.checksums_asset.unwrap().name, "SHA256SUMS");

        let same = build_update_check(
            "1.1.0",
            InstallFlavor::Portable,
            "x64",
            release("v1.1.0", &["Mework_1.1.0_x64_portable.zip"]),
            String::new(),
        )
        .unwrap();
        assert!(!same.update_available);
        assert_eq!(same.asset.unwrap().name, "Mework_1.1.0_x64_portable.zip");
        assert!(same.checksums_asset.is_none());

        // Running a newer build than the latest release is not an update either.
        let ahead = build_update_check(
            "1.2.0",
            InstallFlavor::Portable,
            "x64",
            release("v1.1.0", &[]),
            String::new(),
        )
        .unwrap();
        assert!(!ahead.update_available);
        assert!(ahead.asset.is_none());
    }

    #[test]
    fn update_check_rejects_unparseable_tags_and_drafts() {
        let error = build_update_check(
            "1.0.0",
            InstallFlavor::Installer,
            "x64",
            release("nightly", &[]),
            String::new(),
        )
        .unwrap_err();
        assert!(error.contains("nightly"), "{error}");
        let mut draft = release("v9.9.9", &[]);
        draft.draft = true;
        assert!(build_update_check("1.0.0", InstallFlavor::Installer, "x64", draft, String::new()).is_err());
        assert!(build_update_check("dev", InstallFlavor::Installer, "x64", release("v1.0.0", &[]), String::new()).is_err());
    }

    #[test]
    fn release_json_from_github_parses() {
        let json = serde_json::json!({
            "url": "https://api.github.com/repos/catblob-hash/Mework/releases/1",
            "tag_name": "v1.0.0",
            "name": null,
            "draft": false,
            "prerelease": false,
            "html_url": "https://github.com/catblob-hash/Mework/releases/tag/v1.0.0",
            "body": null,
            "published_at": "2026-09-02T00:00:00Z",
            "author": { "login": "catblob-hash" },
            "assets": [
                {
                    "name": "Mework_1.0.0_x64-setup.exe",
                    "browser_download_url": "https://github.com/catblob-hash/Mework/releases/download/v1.0.0/Mework_1.0.0_x64-setup.exe",
                    "size": 36175872,
                    "content_type": "application/x-msdownload",
                    "download_count": 3
                }
            ]
        });
        let release: GithubRelease = serde_json::from_value(json).unwrap();
        let check = build_update_check("1.0.0", InstallFlavor::Installer, "x64", release, String::new()).unwrap();
        // A null name falls back to the tag; null notes become empty.
        assert_eq!(check.release.name, "v1.0.0");
        assert_eq!(check.release.notes, "");
        assert_eq!(check.asset.unwrap().size, 36175872);
    }

    #[test]
    fn download_hosts_are_github_only_over_https() {
        for allowed in [
            "https://github.com/catblob-hash/Mework/releases/download/v1.0.0/a.exe",
            "https://objects.githubusercontent.com/github-production-release-asset/abc",
            "https://release-assets.githubusercontent.com/x",
            "https://api.github.com/repos/catblob-hash/Mework/releases/latest",
        ] {
            assert!(is_allowed_download_url(&Url::parse(allowed).unwrap()), "{allowed}");
        }
        for refused in [
            "http://github.com/catblob-hash/Mework/releases/download/v1.0.0/a.exe",
            "https://github.com.evil.example/a.exe",
            "https://evilgithubusercontent.com/a.exe",
            "https://example.com/a.exe",
            "file:///C:/a.exe",
        ] {
            assert!(!is_allowed_download_url(&Url::parse(refused).unwrap()), "{refused}");
        }
    }

    #[test]
    fn asset_validation_guards_file_names_sizes_and_hosts() {
        let good = asset("Mework_1.1.0_x64-setup.exe");
        validate_asset(&good, InstallFlavor::Installer).unwrap();
        assert!(validate_asset(&good, InstallFlavor::Portable).is_err());
        for bad_name in [
            "",
            ".hidden.exe",
            "..\\evil.exe",
            "dir/evil.exe",
            "空格 name.exe",
            "with space.exe",
            "semi;colon.exe",
        ] {
            let mut bad = good.clone();
            bad.name = bad_name.to_owned();
            assert!(validate_asset(&bad, InstallFlavor::Installer).is_err(), "{bad_name:?}");
        }
        let mut long = good.clone();
        long.name = format!("{}.exe", "a".repeat(MAX_ASSET_NAME_LENGTH));
        assert!(validate_asset(&long, InstallFlavor::Installer).is_err());
        let mut empty = good.clone();
        empty.size = 0;
        assert!(validate_asset(&empty, InstallFlavor::Installer).is_err());
        let mut huge = good.clone();
        huge.size = MAX_ASSET_SIZE + 1;
        assert!(validate_asset(&huge, InstallFlavor::Installer).is_err());
        let mut elsewhere = good.clone();
        elsewhere.download_url = "https://example.com/Mework_1.1.0_x64-setup.exe".to_owned();
        assert!(validate_asset(&elsewhere, InstallFlavor::Installer).is_err());
        let mut plaintext = good;
        plaintext.download_url = "http://github.com/x/Mework_1.1.0_x64-setup.exe".to_owned();
        assert!(validate_asset(&plaintext, InstallFlavor::Installer).is_err());
    }

    #[test]
    fn sha256sums_parses_gnu_formats() {
        let text = "# release checksums\r\n\
            0123456789ABCDEF0123456789abcdef0123456789abcdef0123456789abcdef  Mework_1.1.0_x64-setup.exe\r\n\
            \r\n\
            fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210 *Mework_1.1.0_x64_portable.zip\n";
        let entries = parse_sha256sums(text).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(
            expected_sha256(&entries, "Mework_1.1.0_x64-setup.exe"),
            Some("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")
        );
        assert_eq!(
            expected_sha256(&entries, "Mework_1.1.0_x64_portable.zip"),
            Some("fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210")
        );
        assert_eq!(expected_sha256(&entries, "other.zip"), None);
        assert!(parse_sha256sums("deadbeef  short.exe").is_err());
        assert!(parse_sha256sums(&"0".repeat(64)).is_err());
        assert!(parse_sha256sums(&format!("{}  ", "0".repeat(64))).is_err());
        assert!(parse_sha256sums(&format!("{}g  x.exe", "0".repeat(63))).is_err());
        assert_eq!(parse_sha256sums("\n\n").unwrap(), Vec::<(String, String)>::new());
    }

    #[test]
    fn session_allows_one_download_and_installs_only_that_file() {
        let session = UpdateSession::default();
        assert!(session.authorize_install("C:\\anything.exe").is_err());
        assert!(!session.cancel_download());

        let cancel = session.begin_download().unwrap();
        assert!(session.begin_download().is_err(), "second download must wait");
        assert!(session.cancel_download());
        assert!(cancel.load(Ordering::Acquire));
        session.finish_download(None);
        assert!(session.authorize_install("C:\\anything.exe").is_err());

        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("Mework_1.1.0_x64-setup.exe");
        fs::write(&file, b"installer").unwrap();
        let (size, digest) = sha256_file(&file).unwrap();
        assert_eq!(size, 9);
        // SHA-256 of the ASCII bytes "installer".
        assert_eq!(digest.len(), 64);
        let downloaded = DownloadedUpdate {
            path: file.display().to_string(),
            file_name: "Mework_1.1.0_x64-setup.exe".to_owned(),
            size_bytes: size,
            sha256: digest,
            verification: Verification::Unavailable,
            flavor: InstallFlavor::Installer,
        };
        session.begin_download().unwrap();
        session.finish_download(Some(downloaded.clone()));
        assert_eq!(
            session.authorize_install(&downloaded.path).unwrap().downloaded,
            downloaded
        );
        let sibling = directory.path().join("other.exe");
        fs::write(&sibling, b"x").unwrap();
        assert!(session.authorize_install(&sibling.display().to_string()).is_err());

        // The bytes on disk must still be the bytes that were downloaded.
        fs::write(&file, b"installer!").unwrap();
        let tampered = session.authorize_install(&downloaded.path).unwrap_err();
        assert!(tampered.contains("改动"), "{tampered}");
        fs::write(&file, b"INSTALLER").unwrap();
        assert!(session.authorize_install(&downloaded.path).is_err(), "same size, other bytes");
        fs::write(&file, b"installer").unwrap();
        assert!(session.authorize_install(&downloaded.path).is_ok());

        // A new download forgets the old file even before it finishes.
        let _cancel = session.begin_download().unwrap();
        assert!(session.authorize_install(&downloaded.path).is_err());
        session.finish_download(None);

        // A vanished file is refused too.
        session.begin_download().unwrap();
        session.finish_download(Some(downloaded.clone()));
        fs::remove_file(&file).unwrap();
        assert!(session.authorize_install(&downloaded.path).is_err());
    }

    #[cfg(windows)]
    fn authorized_session(directory: &Path) -> (UpdateSession, DownloadedUpdate) {
        let file = directory.join("Mework_1.1.0_x64-setup.exe");
        fs::write(&file, b"installer").unwrap();
        let (size_bytes, sha256) = sha256_file(&file).unwrap();
        let downloaded = DownloadedUpdate {
            path: file.display().to_string(),
            file_name: "Mework_1.1.0_x64-setup.exe".to_owned(),
            size_bytes,
            sha256,
            verification: Verification::Unavailable,
            flavor: InstallFlavor::Installer,
        };
        let session = UpdateSession::default();
        session.begin_download().unwrap();
        session.finish_download(Some(downloaded.clone()));
        (session, downloaded)
    }

    /// The old authorization returned a past digest and the launcher reopened a bare path, so
    /// anything with the user's rights could swap the file in between. The authorization now
    /// holds the verified object open until the launcher is done with it.
    #[cfg(windows)]
    #[test]
    fn an_authorized_install_cannot_be_swapped_before_it_launches() {
        let directory = tempfile::tempdir().unwrap();
        let (session, downloaded) = authorized_session(directory.path());
        let file = PathBuf::from(&downloaded.path);
        let moved = directory.path().join("swapped.exe");

        let authorized = session.authorize_install(&downloaded.path).unwrap();
        assert!(fs::write(&file, b"attacker!").is_err(), "覆盖写入应被共享模式拒绝");
        assert!(
            fs::OpenOptions::new().write(true).open(&file).is_err(),
            "以写入方式打开应被拒绝"
        );
        assert!(fs::remove_file(&file).is_err(), "删除应被拒绝");
        assert!(fs::rename(&file, &moved).is_err(), "改名替换应被拒绝");
        assert_eq!(fs::read(&file).unwrap(), b"installer", "被校验的字节仍在原处");

        drop(authorized);
        // Without these three the assertions above could be passing for any unrelated reason —
        // a read-only file, a locked directory — rather than measuring the guard.
        fs::write(&file, b"attacker!").expect("授权释放后覆盖写入应当恢复");
        fs::rename(&file, &moved).expect("授权释放后改名应当恢复");
        fs::remove_file(&moved).expect("授权释放后删除应当恢复");
    }

    /// Sharing reads only must still admit the read and execute access a process launch needs;
    /// a guard that blocked the launch would break the update instead of protecting it.
    #[cfg(windows)]
    #[test]
    fn the_guard_still_lets_windows_start_the_protected_file() {
        let directory = tempfile::tempdir().unwrap();
        let system_root = std::env::var_os("SystemRoot").expect("Windows 总有 SystemRoot");
        let source = Path::new(&system_root).join("System32").join("cmd.exe");
        let copy = directory.path().join("guarded.exe");
        fs::copy(&source, &copy).expect("复制一个无害的可执行文件");

        let guard = open_guarded(&copy).unwrap();
        let status = std::process::Command::new(&copy)
            .args(["/c", "exit 7"])
            .status()
            .expect("受保护的可执行文件仍应能启动");
        assert_eq!(status.code(), Some(7), "启动的确实是被保护的那个文件");
        assert!(fs::write(&copy, b"x").is_err(), "启动期间它仍不可被替换");

        drop(guard);
        fs::write(&copy, b"x").expect("授权释放后写入应当恢复");
    }

    #[test]
    fn downloads_are_bound_to_this_repository_release_path() {
        let name = "Mework_1.1.0_x64-setup.exe";
        let ok = Url::parse(&format!(
            "https://github.com/catblob-hash/Mework/releases/download/v1.1.0/{name}"
        ))
        .unwrap();
        assert_eq!(validate_release_download_url(&ok, name).unwrap(), "v1.1.0");
        // GitHub treats owner and repository names case-insensitively.
        let cased = Url::parse(&format!(
            "https://GitHub.com/CatBlob-Hash/mework/releases/download/v1.1.0/{name}"
        ))
        .unwrap();
        assert_eq!(validate_release_download_url(&cased, name).unwrap(), "v1.1.0");
        for refused in [
            // Another repository, even on GitHub.
            "https://github.com/attacker/repo/releases/download/v1.1.0/Mework_1.1.0_x64-setup.exe",
            "https://github.com/catblob-hash/Mework-fork/releases/download/v1.1.0/Mework_1.1.0_x64-setup.exe",
            // The CDN directly: only the canonical address is accepted as input.
            "https://objects.githubusercontent.com/github-production-release-asset/1/Mework_1.1.0_x64-setup.exe",
            // Right repository, wrong kind of path.
            "https://github.com/catblob-hash/Mework/raw/main/Mework_1.1.0_x64-setup.exe",
            "https://github.com/catblob-hash/Mework/releases/download/Mework_1.1.0_x64-setup.exe",
            "https://github.com/catblob-hash/Mework/releases/download/nightly/Mework_1.1.0_x64-setup.exe",
            "https://github.com/catblob-hash/Mework/releases/download/v1.1.0/other.exe",
            "https://github.com/catblob-hash/Mework/releases/download/v1.1.0/Mework_1.1.0_x64-setup.exe/extra",
            // Decorations that a redirector or a proxy could abuse.
            "http://github.com/catblob-hash/Mework/releases/download/v1.1.0/Mework_1.1.0_x64-setup.exe",
            "https://github.com:8443/catblob-hash/Mework/releases/download/v1.1.0/Mework_1.1.0_x64-setup.exe",
            "https://user@github.com/catblob-hash/Mework/releases/download/v1.1.0/Mework_1.1.0_x64-setup.exe",
            "https://github.com/catblob-hash/Mework/releases/download/v1.1.0/Mework_1.1.0_x64-setup.exe?x=1",
            "https://github.com/catblob-hash/Mework/releases/download/v1.1.0/Mework_1.1.0_x64-setup.exe#f",
            "https://github.com.evil.example/catblob-hash/Mework/releases/download/v1.1.0/Mework_1.1.0_x64-setup.exe",
        ] {
            let url = Url::parse(refused).unwrap();
            assert!(validate_release_download_url(&url, name).is_err(), "{refused}");
        }
        // validate_asset routes through the same rule.
        let mut elsewhere = asset(name);
        elsewhere.download_url =
            "https://github.com/attacker/repo/releases/download/v1.1.0/Mework_1.1.0_x64-setup.exe".to_owned();
        assert!(validate_asset(&elsewhere, InstallFlavor::Installer).is_err());
        // What the API reports and what the fallback builds both pass.
        let (installer, portable, checksums) = conventional_asset_names("1.1.0", "x64");
        for built in [installer, portable, checksums.to_owned()] {
            let url = Url::parse(&format!(
                "https://github.com/catblob-hash/Mework/releases/download/v1.1.0/{built}"
            ))
            .unwrap();
            assert_eq!(validate_release_download_url(&url, &built).unwrap(), "v1.1.0");
        }
    }

    #[test]
    fn download_events_and_outcomes_serialize_for_the_renderer() {
        let progress = serde_json::to_value(DownloadEvent::Progress {
            received_bytes: 5,
            total_bytes: 10,
        })
        .unwrap();
        assert_eq!(
            progress,
            serde_json::json!({ "type": "progress", "receivedBytes": 5, "totalBytes": 10 })
        );
        assert_eq!(
            serde_json::to_value(DownloadEvent::Verifying).unwrap(),
            serde_json::json!({ "type": "verifying" })
        );
        let outcome = serde_json::to_value(InstallOutcome {
            action: InstallAction::InstallerLaunched,
        })
        .unwrap();
        assert_eq!(outcome, serde_json::json!({ "action": "installer_launched" }));
        let info = version_info("1.0.0", Path::new("C:\\Mework"));
        let json = serde_json::to_value(&info).unwrap();
        assert_eq!(json["flavor"], "portable");
        assert_eq!(json["releasesUrl"], format!("{REPOSITORY_URL}/releases"));
        assert_eq!(json["version"], "1.0.0");
        let downloaded = serde_json::to_value(DownloadedUpdate {
            path: "p".to_owned(),
            file_name: "f".to_owned(),
            size_bytes: 1,
            sha256: "s".to_owned(),
            verification: Verification::Verified,
            flavor: InstallFlavor::Installer,
        })
        .unwrap();
        assert_eq!(downloaded["verification"], "verified");
        assert_eq!(downloaded["flavor"], "installer");
        assert_eq!(downloaded["sizeBytes"], 1);
    }

    #[test]
    fn nsis_arguments_match_tauri_updater() {
        assert_eq!(NSIS_UPDATE_ARGUMENTS, "/P /UPDATE /R");
    }

    #[test]
    fn stale_installers_are_pruned_but_the_target_and_other_files_survive() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        for name in [
            "Mework_1.0.0_x64-setup.exe",
            "Mework_1.1.0_x64-setup.exe",
            "Mework_1.1.0_x64-setup.exe.part",
            "notes.txt",
        ] {
            fs::write(root.join(name), b"x").unwrap();
        }
        fs::create_dir(root.join("folder.exe")).unwrap();
        prune_stale_downloads(root, "Mework_1.1.0_x64-setup.exe");
        assert!(!root.join("Mework_1.0.0_x64-setup.exe").exists());
        assert!(!root.join("Mework_1.1.0_x64-setup.exe.part").exists());
        assert!(root.join("Mework_1.1.0_x64-setup.exe").exists());
        assert!(root.join("notes.txt").exists());
        assert!(root.join("folder.exe").is_dir());
        // A missing directory is not an error.
        prune_stale_downloads(&root.join("absent"), "x.exe");
    }

    #[test]
    fn release_redirect_fallback_reads_the_tag_and_conventional_names() {
        let landed = Url::parse("https://github.com/catblob-hash/Mework/releases/tag/v1.1.0").unwrap();
        assert_eq!(tag_from_release_url(&landed).as_deref(), Some("v1.1.0"));
        let encoded = Url::parse("https://github.com/catblob-hash/Mework/releases/tag/v1.1.0%2Bbuild").unwrap();
        assert_eq!(tag_from_release_url(&encoded).as_deref(), Some("v1.1.0+build"));
        for elsewhere in [
            "https://github.com/catblob-hash/Mework/releases",
            "https://github.com/catblob-hash/Mework/releases/latest",
            "https://github.com/catblob-hash/Mework",
            "https://github.com/catblob-hash/Mework/releases/tag/",
            "https://github.com/catblob-hash/Mework/releases/tag/v1/extra",
        ] {
            assert!(tag_from_release_url(&Url::parse(elsewhere).unwrap()).is_none(), "{elsewhere}");
        }
        let (installer, portable, checksums) = conventional_asset_names("1.1.0", "x64");
        assert_eq!(installer, "Mework_1.1.0_x64-setup.exe");
        assert_eq!(portable, "Mework_1.1.0_x64_portable.zip");
        assert_eq!(checksums, "SHA256SUMS");
        // The conventional names must be what the selector picks, or the fallback would find
        // the file and then refuse it.
        let assets = vec![asset(&installer), asset(&portable), asset(checksums)];
        assert_eq!(select_asset(&assets, InstallFlavor::Installer, "x64").unwrap().name, installer);
        assert_eq!(select_asset(&assets, InstallFlavor::Portable, "x64").unwrap().name, portable);
        assert_eq!(select_checksums_asset(&assets).unwrap().name, checksums);
    }
}
