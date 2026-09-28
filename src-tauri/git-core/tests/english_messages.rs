//! Messages in English once the process is told the UI is English.
//!
//! The language is one switch for the whole process, so this lives in a test
//! binary of its own, with a single test: the unit tests, which run in parallel
//! threads of one process, assert the Simplified Chinese wording.

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use git_core::service::{handle, GitServiceOp, GitServiceReply, GitServiceRequest};
use git_core::{GitChangePageRequest, RemoteGitOutput, RemoteGitProbe};

fn has_han(text: &str) -> bool {
    text.chars()
        .any(|ch| ('\u{4e00}'..='\u{9fff}').contains(&ch))
}

fn page_request(expected_revision: &str) -> GitChangePageRequest {
    GitChangePageRequest {
        expected_revision: expected_revision.to_owned(),
        cursor: None,
        query: None,
        limit: 50,
        selected_path: None,
        base: None,
    }
}

fn git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

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
fn failures_are_worded_in_english_when_the_ui_is() {
    git_core::set_english(true);
    assert!(git_core::english());

    // A label composed into a message.
    let error = git_core::create_isolated_worktree(Path::new("."), "bad id", "slot").unwrap_err();
    assert_eq!(
        error,
        "The run id may only contain letters, digits, underscores and hyphens"
    );

    // A failed Git invocation, as a remote status probe reports it.
    let probe = RemoteGitProbe {
        machine_key: "ssh:test".into(),
        sections: HashMap::from([(
            "rev-parse".to_owned(),
            RemoteGitOutput {
                status: 128,
                stdout: Vec::new(),
                stderr: b"fatal: unsafe repository".to_vec(),
            },
        )]),
    };
    let error = git_core::remote_workspace_snapshot(&probe).unwrap_err();
    assert_eq!(
        error,
        "Could not detect the Git repository: fatal: unsafe repository"
    );

    // The remote agent's helper answers in the language the request names.
    let reply = handle(GitServiceRequest {
        machine: "ssh:test".into(),
        root: "~root/app".into(),
        op: GitServiceOp::Branches,
        english: true,
    });
    assert_eq!(
        reply,
        GitServiceReply::Err("Unsupported path form: ~root/app".into())
    );

    if git_available() {
        let plain = tempfile::tempdir().unwrap();
        let error = git_core::change_page(plain.path(), page_request(&"0".repeat(64))).unwrap_err();
        assert!(!has_han(&error), "{error}");
        assert_eq!(
            error,
            "The working directory is not the root of a Git repository of its own"
        );

        let repository = tempfile::tempdir().unwrap();
        git(repository.path(), &["init", "-q", "-b", "main"]);
        std::fs::write(repository.path().join("a.txt"), "one\n").unwrap();
        git(repository.path(), &["add", "a.txt"]);
        git(repository.path(), &["commit", "-q", "-m", "first"]);
        let error = git_core::change_page(repository.path(), page_request("stale")).unwrap_err();
        assert_eq!(
            error,
            "The Git change page revision is invalid; refresh the repository status and try again"
        );

        let reply = handle(GitServiceRequest {
            machine: "ssh:test".into(),
            root: plain.path().to_string_lossy().into_owned(),
            op: GitServiceOp::ChangePage {
                request: page_request(&"0".repeat(64)),
            },
            english: true,
        });
        let GitServiceReply::Err(message) = reply else {
            panic!("a checkout that is not a repository has no change page");
        };
        assert!(!has_han(&message), "{message}");
        assert!(
            message.starts_with("The working directory is not"),
            "{message}"
        );
    }

    // And back: a request from a host in Chinese is answered in Chinese.
    let reply = handle(GitServiceRequest {
        machine: "ssh:test".into(),
        root: "~root/app".into(),
        op: GitServiceOp::Branches,
        english: false,
    });
    assert_eq!(
        reply,
        GitServiceReply::Err("不支持的路径写法：~root/app".into())
    );
    assert!(!git_core::english());
}
