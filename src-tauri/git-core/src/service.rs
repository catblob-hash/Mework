//! One Git operation as a message, for a checkout on another machine.
//!
//! The host reads and writes a checkout on its own filesystem by calling this
//! crate directly. A checkout on an SSH or WSL machine is served by the same
//! calls made there: the host starts `mework-remote git` through that
//! machine's agent, writes one [`GitServiceRequest`] to its standard input as
//! JSON, and reads one [`GitServiceReply`] back from its standard output.
//!
//! One process per operation, rather than a request type in the agent's own
//! protocol: the helper is a leaf the daemon already knows how to start,
//! bound, cancel and reap, a Git that hangs or crashes takes nothing else down
//! with it, and the protocol stays the same size. What it costs — starting one
//! small process — is paid on the machine, next to the repository, where every
//! Git invocation the operation makes then runs without a round trip of its
//! own.
//!
//! Nothing here widens what the host could already do: the root is one the
//! host recorded and resolved, never a path the model or the renderer chose,
//! and every operation applies the same validation it applies on the host.

use std::io::{Read, Write};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{GitAction, GitChangePageRequest, GitDiffRequest, IsolatedWorktree};

/// Largest request the helper reads. Requests carry paths and revisions, never
/// file contents; a larger one is a mistake, not a big request.
const MAX_REQUEST_BYTES: u64 = 4 * 1024 * 1024;

/// One operation on the checkout at `root`, on the machine `machine` names.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitServiceRequest {
    /// The machine's identity (the host's `run_environment::env_key`), which
    /// repository and worktree ids are keyed by.
    pub machine: String,
    /// The checkout's root as the host records it on that machine. May begin
    /// with `~`, which is the helper's home directory.
    pub root: String,
    pub op: GitServiceOp,
    /// The host's UI language — English when set, Simplified Chinese otherwise
    /// — so the helper words failures as the host would.
    #[serde(default)]
    pub english: bool,
}

/// What to do. Each arm is the crate function of the same name, with the
/// arguments it takes besides the checkout.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum GitServiceOp {
    Summary {
        #[serde(default)]
        known_revision: Option<String>,
    },
    ChangePage {
        request: GitChangePageRequest,
    },
    Diff {
        request: GitDiffRequest,
    },
    Branches,
    PrepareDiscard {
        paths: Vec<String>,
        #[serde(default)]
        include_untracked: bool,
    },
    Action {
        action: GitAction,
    },
    CreateConversationWorktree {
        name: String,
        #[serde(default)]
        from_branch: Option<String>,
    },
    ReleaseWorktree {
        path: String,
        branch: String,
        base_oid: String,
    },
}

/// The answer: the operation's own result as JSON, or the message it failed
/// with — worded the way the host words the same failure for a local checkout.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum GitServiceReply {
    Ok(serde_json::Value),
    Err(String),
}

/// Runs one request in this process.
pub fn handle(request: GitServiceRequest) -> GitServiceReply {
    crate::set_identity_namespace(&request.machine);
    crate::set_english(request.english);
    match run(request) {
        Ok(value) => GitServiceReply::Ok(value),
        Err(message) => GitServiceReply::Err(message),
    }
}

fn run(request: GitServiceRequest) -> Result<serde_json::Value, String> {
    let root = expand_home(&request.root)?;
    match request.op {
        GitServiceOp::Summary { known_revision } => {
            json(crate::workspace_summary(&root, known_revision))
        }
        GitServiceOp::ChangePage { request } => json(crate::change_page(&root, request)),
        GitServiceOp::Diff { request } => json(crate::diff(&root, request)),
        GitServiceOp::Branches => json(crate::branches(&root)),
        GitServiceOp::PrepareDiscard {
            paths,
            include_untracked,
        } => json(crate::prepare_discard(&root, &paths, include_untracked)),
        GitServiceOp::Action { action } => json(crate::execute_action(&root, action)),
        GitServiceOp::CreateConversationWorktree { name, from_branch } => json(
            crate::create_conversation_worktree(&root, &name, from_branch.as_deref()),
        ),
        GitServiceOp::ReleaseWorktree {
            path,
            branch,
            base_oid,
        } => json(crate::release_isolated_worktree(
            &root,
            &IsolatedWorktree {
                path: expand_home(&path)?,
                branch,
                base_oid,
            },
        )),
    }
}

fn json<T: Serialize>(value: Result<T, String>) -> Result<serde_json::Value, String> {
    value.and_then(|value| {
        serde_json::to_value(value).map_err(|error| {
            text!(
                "无法编码 Git 结果：{error}",
                "Could not encode the Git result: {error}"
            )
        })
    })
}

/// `~` and `~/…` (or `~\…`) against the home directory of this process.
fn expand_home(path: &str) -> Result<PathBuf, String> {
    let rest = match path.strip_prefix('~') {
        None => return Ok(PathBuf::from(path)),
        Some(rest) if rest.is_empty() || rest.starts_with('/') || rest.starts_with('\\') => rest,
        // `~user` names someone else's home, which this helper has no reason to reach.
        Some(_) => {
            return Err(text!(
                "不支持的路径写法：{path}",
                "Unsupported path form: {path}"
            ))
        }
    };
    let home = std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .or_else(|| std::env::var_os("USERPROFILE").filter(|home| !home.is_empty()))
        .ok_or_else(|| {
            text!(
                "无法展开 {path}：这台机器没有主目录",
                "Could not expand {path}: this machine has no home directory"
            )
        })?;
    let rest = rest.trim_start_matches(['/', '\\']);
    Ok(if rest.is_empty() {
        PathBuf::from(home)
    } else {
        PathBuf::from(home).join(rest)
    })
}

/// `mework-remote git`: one request on standard input, one reply on standard
/// output. A request that cannot be read is answered like any other failure,
/// so the host always has a reply to parse.
pub fn run_stdio() -> Result<(), String> {
    let mut input = Vec::new();
    std::io::stdin()
        .take(MAX_REQUEST_BYTES + 1)
        .read_to_end(&mut input)
        .map_err(|error| {
            text!(
                "无法读取 Git 请求：{error}",
                "Could not read the Git request: {error}"
            )
        })?;
    let reply = if input.len() as u64 > MAX_REQUEST_BYTES {
        GitServiceReply::Err(text!(
            "Git 请求超过大小上限",
            "The Git request exceeds the size limit"
        ))
    } else {
        match serde_json::from_slice::<GitServiceRequest>(&input) {
            Ok(request) => handle(request),
            Err(error) => {
                // Answered in the request's language when it names one.
                crate::set_english(
                    serde_json::from_slice::<serde_json::Value>(&input)
                        .ok()
                        .and_then(|request| request.get("english")?.as_bool())
                        .unwrap_or(false),
                );
                GitServiceReply::Err(text!(
                    "无法解析 Git 请求：{error}",
                    "Could not parse the Git request: {error}"
                ))
            }
        }
    };
    let bytes = serde_json::to_vec(&reply).map_err(|error| {
        text!(
            "无法编码 Git 回答：{error}",
            "Could not encode the Git reply: {error}"
        )
    })?;
    let mut stdout = std::io::stdout().lock();
    stdout
        .write_all(&bytes)
        .and_then(|()| stdout.flush())
        .map_err(|error| {
            text!(
                "无法写出 Git 回答：{error}",
                "Could not write the Git reply: {error}"
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .status()
            .expect("git runs");
        assert!(status.success(), "git {args:?}");
    }

    #[test]
    fn requests_travel_as_tagged_json() {
        let request = GitServiceRequest {
            machine: "ssh:m1".into(),
            root: "~/app".into(),
            op: GitServiceOp::PrepareDiscard {
                paths: vec!["a.txt".into()],
                include_untracked: true,
            },
            english: false,
        };
        let text = serde_json::to_string(&request).unwrap();
        assert!(text.contains(r#""kind":"prepareDiscard""#), "{text}");
        assert!(text.contains(r#""includeUntracked":true"#), "{text}");
        let back: GitServiceRequest = serde_json::from_str(&text).unwrap();
        assert!(matches!(
            back.op,
            GitServiceOp::PrepareDiscard { include_untracked: true, .. }
        ));
        let branches: GitServiceRequest =
            serde_json::from_str(r#"{"machine":"wsl:u","root":"/srv","op":{"kind":"branches"}}"#)
                .unwrap();
        assert!(matches!(branches.op, GitServiceOp::Branches));
        let reply = serde_json::to_string(&GitServiceReply::Err("no".into())).unwrap();
        assert_eq!(reply, r#"{"err":"no"}"#);
    }

    #[test]
    fn home_is_expanded_and_other_users_homes_are_refused() {
        assert_eq!(expand_home("/srv/app").unwrap(), PathBuf::from("/srv/app"));
        assert!(expand_home("~root/app").is_err());
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from);
        if let Some(home) = home {
            assert_eq!(expand_home("~").unwrap(), home);
            assert_eq!(expand_home("~/app").unwrap(), home.join("app"));
        }
    }

    #[test]
    fn a_summary_request_reads_the_checkout_it_names() {
        if crate::find_program("git").is_none() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-q", "-b", "main"]);
        std::fs::write(dir.path().join("a.txt"), "one\n").unwrap();
        git(dir.path(), &["add", "a.txt"]);
        git(dir.path(), &["commit", "-q", "-m", "first"]);
        std::fs::write(dir.path().join("a.txt"), "one\ntwo\n").unwrap();
        let reply = run(GitServiceRequest {
            machine: "ssh:test".into(),
            root: dir.path().to_string_lossy().into_owned(),
            op: GitServiceOp::Summary {
                known_revision: None,
            },
            english: false,
        })
        .unwrap();
        assert_eq!(reply["kind"], "snapshot");
        assert_eq!(reply["summary"]["branch"], "main");
        assert_eq!(reply["summary"]["additions"], 1);
    }
}
