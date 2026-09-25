//! One status read per checkout at a time, shared by everyone who asks.
//!
//! The Git pane polls every workspace of the open conversation every few
//! seconds, the composer's Git chip polls the one it shows, and every
//! conversation of a project polls the same checkouts. Each poll is a
//! `git status` with line counts and diff digests — and for a checkout on
//! another machine, a round trip to it. Reading once per checkout and handing
//! the answer to every caller that asked meanwhile keeps the cost proportional
//! to the checkouts rather than to the callers, and a slow machine is asked
//! one question at a time rather than a queue of identical ones.
//!
//! A finished read keeps answering for a moment, to merge a burst of polls
//! that arrive just after it. Writes forget the checkout's read ([`forget`]),
//! so nobody who asks after a write is answered with the state from before it.

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use crate::git::{GitWorkspaceSummary, GitWorkspaceSummaryResult};

/// How long a finished read keeps answering. Far below the pane's poll
/// interval: this merges concurrent askers, it is not a cache of old state.
const FRESHNESS: Duration = Duration::from_millis(750);

/// What one read found: the summary, or `None` when the directory is not a
/// repository root.
pub(crate) type Outcome = Result<Option<GitWorkspaceSummary>, String>;

#[derive(Default)]
struct Flight {
    /// The outcome and when it arrived, once the read has finished.
    outcome: Mutex<Option<(Outcome, Instant)>>,
    ready: Condvar,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn flights() -> &'static Mutex<HashMap<String, Arc<Flight>>> {
    static FLIGHTS: OnceLock<Mutex<HashMap<String, Arc<Flight>>>> = OnceLock::new();
    FLIGHTS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Settles a flight whose reader never got to, so nobody waits on it forever.
struct Settle<'a> {
    flight: &'a Flight,
}

impl Drop for Settle<'_> {
    fn drop(&mut self) {
        let mut outcome = lock(&self.flight.outcome);
        if outcome.is_none() {
            *outcome = Some((
                Err("读取 Git 状态的任务意外中断；请重试".into()),
                Instant::now(),
            ));
            self.flight.ready.notify_all();
        }
    }
}

/// The summary of the checkout `key` names, answered for a caller that last
/// saw `known_revision`. `read` runs only when no read of that checkout is in
/// flight or just finished; otherwise this waits for that one's answer.
pub(crate) fn summary(
    key: &str,
    known_revision: Option<&str>,
    read: impl FnOnce() -> Outcome,
) -> Result<GitWorkspaceSummaryResult, String> {
    let (flight, reader) = {
        let mut map = lock(flights());
        // Finished flights past their freshness are dropped as they are met, so
        // the map holds only checkouts someone asked about lately.
        map.retain(|_, flight| match &*lock(&flight.outcome) {
            None => true,
            Some((Ok(_), at)) => at.elapsed() < FRESHNESS,
            Some((Err(_), _)) => false,
        });
        match map.get(key) {
            Some(flight) => (flight.clone(), false),
            None => {
                let flight = Arc::new(Flight::default());
                map.insert(key.to_owned(), flight.clone());
                (flight, true)
            }
        }
    };
    let outcome = if reader {
        let settle = Settle { flight: &flight };
        let outcome = read();
        *lock(&flight.outcome) = Some((outcome.clone(), Instant::now()));
        flight.ready.notify_all();
        drop(settle);
        outcome
    } else {
        let mut guard = lock(&flight.outcome);
        while guard.is_none() {
            guard = flight
                .ready
                .wait(guard)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
        guard.as_ref().map(|(outcome, _)| outcome.clone()).unwrap_or_else(|| Ok(None))
    };
    answer(outcome, known_revision)
}

/// Drops what is known about `key`, after a write changed it.
pub(crate) fn forget(key: &str) {
    lock(flights()).remove(key);
}

/// An [`Outcome`] as the reply to a caller that holds `known_revision`.
pub(crate) fn answer(
    outcome: Outcome,
    known_revision: Option<&str>,
) -> Result<GitWorkspaceSummaryResult, String> {
    Ok(match outcome? {
        None => GitWorkspaceSummaryResult::NotRepository,
        Some(summary) if known_revision == Some(summary.summary_revision.as_str()) => {
            GitWorkspaceSummaryResult::Unchanged {
                revision: summary.summary_revision,
            }
        }
        Some(summary) => GitWorkspaceSummaryResult::Snapshot { summary },
    })
}

/// A summary read's result as an [`Outcome`]; an `Unchanged` answer cannot
/// arise from a read that was not given a revision.
pub(crate) fn outcome(result: Result<GitWorkspaceSummaryResult, String>) -> Outcome {
    match result? {
        GitWorkspaceSummaryResult::NotRepository => Ok(None),
        GitWorkspaceSummaryResult::Snapshot { summary } => Ok(Some(summary)),
        GitWorkspaceSummaryResult::Unchanged { .. } => {
            Err("Git 摘要读取意外地只返回了修订号；请重试".into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn summary_with(revision: &str) -> GitWorkspaceSummary {
        let mut value = serde_json::json!({
            "repositoryId": "r", "worktreeId": "w", "branch": "main", "head": null,
            "contentRevision": "c", "summaryRevision": revision, "upstream": null,
            "upstreamTarget": null, "ahead": 0, "behind": 0, "additions": 0, "deletions": 0,
            "staged": 0, "unstaged": 0, "untracked": 0, "conflicted": 0, "stash": 0,
            "changedFiles": 0, "stageable": 0, "unstageable": 0, "remote": null, "remotes": [],
            "gitVersion": "2", "repositoryRoot": "/r", "worktreeRoot": "/r", "detached": false,
            "unborn": false, "operation": null, "operationRevision": null, "isClean": true,
            "binaryFiles": 0, "warnings": []
        });
        value["summaryRevision"] = revision.into();
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn concurrent_askers_share_one_read_and_each_gets_its_own_answer() {
        let reads = Arc::new(AtomicUsize::new(0));
        let key = "test:concurrent";
        let (release, gate) = std::sync::mpsc::channel::<()>();
        let reader = {
            let reads = reads.clone();
            std::thread::spawn(move || {
                summary(key, None, || {
                    reads.fetch_add(1, Ordering::SeqCst);
                    gate.recv().unwrap();
                    Ok(Some(summary_with("rev-1")))
                })
            })
        };
        // Wait until the first read is in flight before asking again.
        while reads.load(Ordering::SeqCst) == 0 {
            std::thread::yield_now();
        }
        let waiter = {
            let reads = reads.clone();
            std::thread::spawn(move || {
                summary(key, Some("rev-1"), || {
                    reads.fetch_add(1, Ordering::SeqCst);
                    Ok(None)
                })
            })
        };
        std::thread::sleep(Duration::from_millis(20));
        release.send(()).unwrap();
        assert!(matches!(
            reader.join().unwrap(),
            Ok(GitWorkspaceSummaryResult::Snapshot { .. })
        ));
        assert!(matches!(
            waiter.join().unwrap(),
            Ok(GitWorkspaceSummaryResult::Unchanged { revision }) if revision == "rev-1"
        ));
        assert_eq!(reads.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_write_is_never_answered_with_the_read_from_before_it() {
        let key = "test:forget";
        assert!(matches!(
            summary(key, None, || Ok(Some(summary_with("before")))),
            Ok(GitWorkspaceSummaryResult::Snapshot { summary }) if summary.summary_revision == "before"
        ));
        forget(key);
        assert!(matches!(
            summary(key, None, || Ok(Some(summary_with("after")))),
            Ok(GitWorkspaceSummaryResult::Snapshot { summary }) if summary.summary_revision == "after"
        ));
    }

    #[test]
    fn a_failed_read_is_not_served_to_later_askers() {
        let key = "test:failure";
        assert!(summary(key, None, || Err("offline".into())).is_err());
        assert!(matches!(
            summary(key, None, || Ok(None)),
            Ok(GitWorkspaceSummaryResult::NotRepository)
        ));
    }
}
