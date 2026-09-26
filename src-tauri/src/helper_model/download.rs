//! Downloads a model's files, each checked against the SHA-256 the app pins
//! for it.
//!
//! Every file comes from one host, picked before the download starts: Hugging
//! Face, or its mirror in mainland China (hf-mirror.com, which serves every
//! public repository under the same paths) when the user asks for it; the
//! renderer offers that on systems set to Chinese for mainland China. A
//! failure never switches hosts. Transfers resume instead: a partial file stays as `.part` and
//! continues with a range request, on the automatic retries after a dropped
//! or stalled connection as well as on the next install after a failure, a
//! cancel or a restart. Files already verified are recorded in
//! `verified.json`, so a model update only fetches what changed.
//!
//! `MEWORK_LOCAL_MODEL_MIRROR` (a base URL serving every file at its `remote`
//! path) replaces every host; development and tests serve files from it.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde::Deserialize;
use sha2::{Digest, Sha256};

pub const CANCELLED: &str = "下载已取消";
const VERIFIED: &str = "verified.json";
const MIRROR_ENV: &str = "MEWORK_LOCAL_MODEL_MIRROR";
const HF_MIRROR: &str = "https://hf-mirror.com";
const CHUNK: usize = 256 * 1024;
const PROGRESS_INTERVAL: Duration = Duration::from_millis(200);
/// Failed attempts in a row, with no byte gained, before a file gives up.
const ATTEMPTS: u32 = 5;
/// The first wait before a retry; it doubles each time.
const RETRY_DELAY: Duration = if cfg!(test) { Duration::from_millis(10) } else { Duration::from_secs(2) };
/// The blocking client applies its timeout to each read, so this is how long
/// a connection may go without a byte before it counts as dropped.
const STALL: Duration = Duration::from_secs(60);

/// A Hugging Face repository at a pinned revision.
#[derive(Clone, Copy, Debug)]
pub struct Repo {
    pub id: &'static str,
    pub revision: &'static str,
}

#[derive(Clone, Debug, Deserialize)]
pub struct RemoteFile {
    /// Path in the repository.
    pub remote: String,
    /// Path under the model's directory.
    pub local: String,
    pub size: u64,
    pub sha256: String,
}

/// A host files are fetched from, by their `remote` path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    /// For the renderer: `huggingFace`, `hfMirror` or `mirror`.
    pub label: &'static str,
    base: String,
}

impl Source {
    /// Where `repo`'s files come from; `china_mirror` picks the mirror in
    /// mainland China.
    pub fn of(repo: &Repo, china_mirror: bool) -> Self {
        if let Ok(base) = std::env::var(MIRROR_ENV) {
            if !base.trim().is_empty() {
                return Self { label: "mirror", base };
            }
        }
        let Repo { id, revision } = repo;
        if china_mirror {
            Self { label: "hfMirror", base: format!("{HF_MIRROR}/{id}/resolve/{revision}") }
        } else {
            Self { label: "huggingFace", base: format!("https://huggingface.co/{id}/resolve/{revision}") }
        }
    }

    fn url(&self, remote: &str) -> String {
        format!("{}/{remote}", self.base.trim_end_matches('/'))
    }
}

fn verified(dir: &Path) -> BTreeMap<String, String> {
    fs::read_to_string(dir.join(VERIFIED)).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default()
}

fn is_verified(dir: &Path, record: &BTreeMap<String, String>, file: &RemoteFile) -> bool {
    record.get(&file.local) == Some(&file.sha256)
        && fs::metadata(dir.join(&file.local)).map(|m| m.len() == file.size).unwrap_or(false)
}

pub struct Progress {
    pub received: u64,
    pub total: u64,
    pub source: Source,
}

fn client() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(20))
        // Per read, not for the whole 1.7 GB transfer (see `STALL`).
        .timeout(STALL)
        .user_agent(concat!("Mework/", env!("CARGO_PKG_VERSION")))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() > 10 || attempt.url().scheme() != "https" {
                attempt.stop()
            } else {
                attempt.follow()
            }
        }))
        .build()
        .map_err(|error| format!("无法创建下载客户端: {error}"))
}

pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 4 << 20];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    Ok(hasher.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Downloads every file of `files` not yet verified in `dir` from `source`.
/// `progress` is throttled.
pub fn download(
    dir: &Path,
    files: &[RemoteFile],
    source: &Source,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(Progress),
) -> Result<(), String> {
    fs::create_dir_all(dir).map_err(|error| format!("无法创建模型目录: {error}"))?;
    let client = client()?;
    let total: u64 = files.iter().map(|file| file.size).sum();
    let mut done_bytes: u64 = 0;
    let mut record = verified(dir);
    for file in files {
        if is_verified(dir, &record, file) {
            done_bytes += file.size;
            continue;
        }
        let target = dir.join(&file.local);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|error| format!("无法创建模型目录: {error}"))?;
        }
        let part = target.with_file_name(format!(
            "{}.part",
            target.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default()
        ));
        let url = source.url(&file.remote);
        let mut failures = 0;
        loop {
            let before = part_len(&part);
            let result = fetch(&client, &url, file, &part, cancel, &mut |received| {
                progress(Progress { received: done_bytes + received, total, source: source.clone() })
            });
            match result {
                Ok(()) => break,
                Err(Failure::Retry(error)) => {
                    failures = if part_len(&part) > before { 1 } else { failures + 1 };
                    if failures >= ATTEMPTS {
                        return Err(format!("{error}（已下载的部分会保留，再次下载时从断点继续）"));
                    }
                    eprintln!("[local-model] {url}: {error}; retrying");
                    wait(RETRY_DELAY * 2u32.pow(failures - 1), cancel)?;
                }
                Err(Failure::Fatal(error)) => return Err(error),
            }
        }
        let digest = sha256_file(&part).map_err(|error| format!("无法校验 {}: {error}", file.local))?;
        if digest != file.sha256 {
            let _ = fs::remove_file(&part);
            return Err(format!("{} 校验失败（sha256 不符），请重试", file.local));
        }
        fs::rename(&part, &target).map_err(|error| format!("无法放置 {}: {error}", file.local))?;
        record.insert(file.local.clone(), file.sha256.clone());
        let text = serde_json::to_string_pretty(&record).expect("record serializes");
        fs::write(dir.join(VERIFIED), text).map_err(|error| format!("无法写入校验记录: {error}"))?;
        done_bytes += file.size;
        progress(Progress { received: done_bytes, total, source: source.clone() });
    }
    Ok(())
}

fn part_len(part: &Path) -> u64 {
    fs::metadata(part).map(|m| m.len()).unwrap_or(0)
}

/// Sleeps `delay`, or until cancelled.
fn wait(delay: Duration, cancel: &AtomicBool) -> Result<(), String> {
    let until = Instant::now() + delay;
    while Instant::now() < until {
        if cancel.load(Ordering::Acquire) {
            return Err(CANCELLED.into());
        }
        std::thread::sleep(Duration::from_millis(100).min(until.saturating_duration_since(Instant::now())));
    }
    Ok(())
}

enum Failure {
    /// Worth another attempt at the same host: the connection failed,
    /// dropped or stalled, or the server had a passing error.
    Retry(String),
    Fatal(String),
}

/// `start` of a `Content-Range: bytes start-end/size` header.
fn content_range_start(value: &str) -> Option<u64> {
    value.strip_prefix("bytes ")?.split('-').next()?.trim().parse().ok()
}

fn fetch(
    client: &reqwest::blocking::Client,
    url: &str,
    file: &RemoteFile,
    part: &Path,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64),
) -> Result<(), Failure> {
    use reqwest::StatusCode;
    let mut have = part_len(part);
    if have > file.size {
        let _ = fs::remove_file(part);
        have = 0;
    }
    if have == file.size {
        return Ok(());
    }
    let mut request = client.get(url);
    if have > 0 {
        request = request.header(reqwest::header::RANGE, format!("bytes={have}-"));
    }
    let mut response = request.send().map_err(|error| {
        let host = reqwest::Url::parse(url).ok().and_then(|url| url.host_str().map(str::to_owned)).unwrap_or_default();
        Failure::Retry(format!("无法连接 {host}: {error}"))
    })?;
    let status = response.status();
    match status {
        // The whole file, whether or not a range was asked for.
        StatusCode::OK => have = 0,
        StatusCode::PARTIAL_CONTENT => {
            let start = response
                .headers()
                .get(reqwest::header::CONTENT_RANGE)
                .and_then(|value| value.to_str().ok())
                .and_then(content_range_start);
            if start != Some(have) {
                let _ = fs::remove_file(part);
                return Err(Failure::Retry(format!("{} 续传位置不符，重新下载", file.local)));
            }
        }
        StatusCode::RANGE_NOT_SATISFIABLE => {
            let _ = fs::remove_file(part);
            return Err(Failure::Retry(format!("{} 无法续传，重新下载", file.local)));
        }
        _ if status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS || status == StatusCode::REQUEST_TIMEOUT => {
            return Err(Failure::Retry(format!("下载 {} 失败：HTTP {status}", file.remote)));
        }
        _ => return Err(Failure::Fatal(format!("下载 {} 失败：HTTP {status}", file.remote))),
    }
    let mut out = OpenOptions::new()
        .create(true)
        .write(true)
        .append(have > 0)
        .truncate(have == 0)
        .open(part)
        .map_err(|error| Failure::Fatal(format!("无法写入 {}: {error}", file.local)))?;
    progress(have);
    let mut buffer = vec![0u8; CHUNK];
    let mut last = Instant::now();
    let mut received = have;
    loop {
        if cancel.load(Ordering::Acquire) {
            return Err(Failure::Fatal(CANCELLED.into()));
        }
        let n = match response.read(&mut buffer) {
            Ok(n) => n,
            // What arrived stays in the part; the retry continues from there.
            Err(error) => return Err(Failure::Retry(format!("下载中断: {error}"))),
        };
        if n == 0 {
            break;
        }
        out.write_all(&buffer[..n]).map_err(|error| Failure::Fatal(format!("无法写入 {}: {error}", file.local)))?;
        received += n as u64;
        if received > file.size {
            drop(out);
            let _ = fs::remove_file(part);
            return Err(Failure::Fatal(format!("{} 大小不符", file.local)));
        }
        if last.elapsed() >= PROGRESS_INTERVAL {
            progress(received);
            last = Instant::now();
        }
    }
    out.sync_all().map_err(|error| Failure::Fatal(format!("无法写入 {}: {error}", file.local)))?;
    progress(received);
    if received != file.size {
        return Err(Failure::Retry(format!("{} 的连接提前结束", file.local)));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::BufRead;
    use std::net::TcpListener;
    use std::sync::mpsc;

    use super::*;

    const REPO: Repo = Repo { id: "owner/model", revision: "abc123" };

    #[test]
    fn builds_source_urls() {
        let url = |china| Source::of(&REPO, china).url("a/b.bin");
        assert_eq!(url(false), "https://huggingface.co/owner/model/resolve/abc123/a/b.bin");
        assert_eq!(url(true), "https://hf-mirror.com/owner/model/resolve/abc123/a/b.bin");
        let mirror = Source { label: "mirror", base: "http://127.0.0.1:9/x/".into() };
        assert_eq!(mirror.url("a/b.bin"), "http://127.0.0.1:9/x/a/b.bin");
    }

    #[test]
    fn reads_the_start_of_a_content_range() {
        assert_eq!(content_range_start("bytes 100-199/1746942600"), Some(100));
        assert_eq!(content_range_start("bytes */100"), None);
        assert_eq!(content_range_start("items 1-2/3"), None);
    }

    #[test]
    fn a_file_counts_once_verified_with_its_size() {
        let dir = tempfile::tempdir().unwrap();
        let file = RemoteFile {
            remote: "x".into(),
            local: "sub/x.bin".into(),
            size: 3,
            sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad".into(),
        };
        fs::create_dir_all(dir.path().join("sub")).unwrap();
        fs::write(dir.path().join("sub/x.bin"), b"abc").unwrap();
        assert!(!is_verified(dir.path(), &verified(dir.path()), &file), "right size but never verified");
        let record: BTreeMap<_, _> = [(file.local.clone(), file.sha256.clone())].into();
        fs::write(dir.path().join(VERIFIED), serde_json::to_string(&record).unwrap()).unwrap();
        assert!(is_verified(dir.path(), &verified(dir.path()), &file));
        fs::write(dir.path().join("sub/x.bin"), b"abcd").unwrap();
        assert!(!is_verified(dir.path(), &verified(dir.path()), &file), "size changed");
    }

    enum Reply {
        /// The requested range, cut after this many bytes when set.
        Body(Option<usize>),
        Status(&'static str),
    }

    /// Answers one request per entry of `replies` over plain HTTP, from
    /// `body`, and reports each request's `Range` start.
    fn serve(body: Vec<u8>, replies: Vec<Reply>) -> (Source, mpsc::Receiver<Option<usize>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let source = Source { label: "mirror", base: format!("http://{}", listener.local_addr().unwrap()) };
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for reply in replies {
                let (mut stream, _) = listener.accept().unwrap();
                let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
                let mut start = None;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line.trim().is_empty() {
                        break;
                    }
                    if let Some(value) = line.to_ascii_lowercase().strip_prefix("range: bytes=") {
                        start = value.trim().trim_end_matches('-').parse::<usize>().ok();
                    }
                }
                tx.send(start).unwrap();
                let rest = &body[start.unwrap_or(0)..];
                match reply {
                    Reply::Status(status) => {
                        let head = format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                        stream.write_all(head.as_bytes()).unwrap();
                    }
                    Reply::Body(cut) => {
                        let head = match start {
                            Some(start) => format!(
                                "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\n\
                                 Content-Range: bytes {start}-{}/{}\r\nConnection: close\r\n\r\n",
                                rest.len(),
                                body.len() - 1,
                                body.len()
                            ),
                            None => format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", rest.len()),
                        };
                        stream.write_all(head.as_bytes()).unwrap();
                        let _ = stream.write_all(&rest[..cut.unwrap_or(rest.len())]);
                    }
                }
            }
        });
        (source, rx)
    }

    fn test_file(body: &[u8]) -> RemoteFile {
        RemoteFile {
            remote: "f.bin".into(),
            local: "f.bin".into(),
            size: body.len() as u64,
            sha256: Sha256::digest(body).iter().map(|b| format!("{b:02x}")).collect(),
        }
    }

    #[test]
    fn a_dropped_connection_resumes_where_it_stopped() {
        let body: Vec<u8> = (0..600_000u32).map(|i| (i % 251) as u8).collect();
        let replies = vec![Reply::Body(Some(250_000)), Reply::Status("503 Busy"), Reply::Body(None)];
        let (source, starts) = serve(body.clone(), replies);
        let dir = tempfile::tempdir().unwrap();
        download(dir.path(), &[test_file(&body)], &source, &AtomicBool::new(false), &mut |_| {}).unwrap();
        assert_eq!(fs::read(dir.path().join("f.bin")).unwrap(), body);
        assert_eq!(starts.try_iter().collect::<Vec<_>>(), [None, Some(250_000), Some(250_000)]);
    }

    #[test]
    fn a_missing_file_fails_at_once() {
        let body = b"0123456789".to_vec();
        let (source, starts) = serve(body.clone(), vec![Reply::Status("401 Unauthorized")]);
        let dir = tempfile::tempdir().unwrap();
        let error = download(dir.path(), &[test_file(&body)], &source, &AtomicBool::new(false), &mut |_| {})
            .unwrap_err();
        assert!(error.contains("401"), "{error}");
        assert_eq!(starts.try_iter().count(), 1, "asked once");
    }
}
