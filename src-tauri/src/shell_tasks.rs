//! Registry of `bash` / `powershell` tool calls, running and finished.
//!
//! A shell command's runtime is unpredictable: `npm install`, a test suite, and `ls` all arrive
//! through the same tool. Before this registry existed the model round simply blocked inside
//! `wait_timeout` with nothing visible anywhere — the user could see neither that a command was
//! running nor what it was, and the only way to stop it was to cancel the whole model run.
//!
//! So every spawned shell registers here. Registration is what makes it a *task*: it appears in the
//! task sidebar, it carries the command text so the row says something useful, and while it runs it
//! can be killed on its own without touching the run.
//!
//! A command that ends is **retained**, not dropped: its entry keeps its start, gains an end and an
//! outcome, and the row moves into the sidebar's collapsed "finished" section. That is the whole
//! point of a task list — the user's question after a build is "did it pass", and a row that
//! deletes itself the instant it could answer that never gets to. Retention is bounded per
//! conversation so a long session cannot grow the list without limit.
//!
//! The registry owns no threads. The tool call still blocks its own worker thread — that is what
//! the model's turn means — but it polls for a stop request instead of sleeping on the child, and
//! the process tree dies the moment one arrives.

use std::{
    collections::{HashMap, VecDeque},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use tauri::ipc::Channel;

use crate::{
    console_text::ConsoleTextDecoder,
    push_events::{AppEventHub, AppPushEvent},
};

/// Longest command text retained for display. The command itself may be up to 64 KiB; a task row
/// is a label, not a transcript.
const MAX_DISPLAY_COMMAND: usize = 512;

/// Live output retained per command, in bytes.
///
/// This buffer keeps the **tail**, which is the opposite of what the tool result keeps: the
/// executor caps its captured bytes at `MAX_TOOL_OUTPUT` from the *start*, because the model is
/// reading a result and the first thing a command says is usually the thing that explains it. A
/// person watching a build wants the other end — the error it died on. Both are right for their
/// own reader, so the two truncations deliberately disagree, and the page says so by reporting
/// `dropped_head_bytes` rather than quietly presenting a tail as the whole output.
const MAX_LIVE_OUTPUT: usize = 256 * 1024;

/// Which pipe a chunk came from. The two pipes are drained by independent threads, so their
/// chunks interleave in arrival order rather than in the "all stdout, then a `[stderr]` section"
/// order the formatted tool result uses. The discriminator is what lets the page colour stderr
/// without having to guess.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ShellOutputStream {
    Stdout,
    Stderr,
}

/// One command's live output, streamed to whichever page is watching it.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ShellOutputEvent {
    Output {
        #[serde(rename = "shellTaskId")]
        shell_task_id: String,
        stream: ShellOutputStream,
        /// Monotonic per command, assigned under the registry lock so the two pipe threads cannot
        /// hand the renderer two chunks with the same number.
        seq: u64,
        text: String,
    },
    End {
        #[serde(rename = "shellTaskId")]
        shell_task_id: String,
        outcome: ShellTaskOutcome,
        #[serde(rename = "exitCode")]
        exit_code: Option<i32>,
    },
}

/// What a page gets when it starts watching: everything retained so far, plus whether more is
/// coming. A finished command answers `live: false` and never installs a sink.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShellTaskOutputHandle {
    pub snapshot: String,
    pub live: bool,
    /// Bytes the tail buffer dropped off the front. Non-zero means the page is showing a suffix.
    pub dropped_head_bytes: u64,
    /// Names the sink this subscription installed, so its detach can be told apart from a newer
    /// subscriber's. Zero for a finished command, which installs no sink.
    pub subscription_id: u64,
}


/// Finished commands kept per conversation. Rows are also bounded by their retained transcript
/// bytes, so a conversation with many chatty commands cannot consume memory indefinitely.
const MAX_FINISHED_PER_CONVERSATION: usize = 400;
const MAX_RETAINED_OUTPUT_PER_CONVERSATION: usize = 32 * 1024 * 1024;

/// How a shell command ended. The sidebar paints a failed row differently from one that merely
/// finished, and "a person stopped this" is a third thing that is neither success nor breakage.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ShellTaskOutcome {
    /// The command exited on its own with a success status.
    Succeeded,
    /// The command exited on its own with a failure status.
    Failed,
    /// Someone pressed stop — the row's own button, or the cancellation signal
    /// of the run/task this command belongs to — and the process tree was killed.
    Stopped,
}

/// One shell command, as the task sidebar sees it. A row is running while `outcome` is `None`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShellTaskSnapshot {
    pub shell_task_id: String,
    pub conversation_id: String,
    /// `bash` or `powershell` — the tool name, so the row can say which one is running.
    pub tool_name: String,
    /// Display-truncated command text. Never the full 64 KiB.
    pub command: String,
    /// Set once someone asked for this command to stop, so the row can say so before the process
    /// has actually gone away.
    pub stopping: bool,
    /// When the command was registered, RFC3339. Stamped by the host rather than by the renderer
    /// because a command can start before anyone is subscribed — a reload, or arriving at the
    /// conversation later — and the reconcile that recovers it would otherwise restart the clock
    /// at the moment it was first *seen* instead of when it began.
    pub started_at: String,
    /// When the command ended, RFC3339. `None` also represents an unknown end
    /// time after host recovery; `outcome` alone determines whether it is live.
    pub ended_at: Option<String>,
    /// How it ended, or `None` while it runs.
    pub outcome: Option<ShellTaskOutcome>,
    /// Process exit code when the command exited on its own and the platform reported one.
    pub exit_code: Option<i32>,
    /// Background commands use the `run_in_background` leg. They survive run
    /// settlement and cancellation; only their own stop button or app exit stops
    /// them. Synchronous commands use their dispatching run/task cancellation signal.
    pub background: bool,
}

struct ShellTaskEntry {
    conversation_id: String,
    tool_name: String,
    command: String,
    /// Stamped once at registration and never recomputed, so every later snapshot of the same
    /// command reports the same start and the row's clock only ever counts up.
    started_at: String,
    /// Raised by `request_stop`; the running tool call polls it.
    stop: Arc<AtomicBool>,
    /// Set once, when the command ends. A finished entry is never revived.
    ended_at: Option<String>,
    outcome: Option<ShellTaskOutcome>,
    exit_code: Option<i32>,
    /// Background-command marker; see [`ShellTaskSnapshot::background`].
    background: bool,
    /// Mint order, so finished rows evict oldest-first and sort stably alongside running ones.
    sequence: u64,
    /// Live output and its single watcher. Only one page can be looking at a command at a time,
    /// so this is one sink rather than a list.
    output: ShellTaskOutput,
}

#[derive(Default)]
struct ShellTaskOutput {
    /// Tail buffer. A deque so trimming the front is not a memmove of the whole retained window
    /// on every 8 KiB chunk of a chatty build.
    buffer: VecDeque<u8>,
    dropped_head_bytes: u64,
    next_seq: u64,
    sink: Option<Channel<ShellOutputEvent>>,
    /// Which subscription installed `sink`; see [`ShellTaskRegistry::detach_output`].
    sink_subscription: u64,
    next_subscription: u64,
}

impl ShellTaskOutput {
    fn push(&mut self, bytes: &[u8]) -> u64 {
        self.buffer.extend(bytes.iter().copied());
        if self.buffer.len() > MAX_LIVE_OUTPUT {
            let mut excess = self.buffer.len() - MAX_LIVE_OUTPUT;
            // The cut lands wherever the byte count says; carrying on through
            // the continuation bytes of the character it split keeps the
            // replayed suffix from opening with U+FFFD.
            while self
                .buffer
                .get(excess)
                .is_some_and(|byte| (byte & 0xC0) == 0x80)
            {
                excess += 1;
            }
            self.buffer.drain(..excess);
            self.dropped_head_bytes = self.dropped_head_bytes.saturating_add(excess as u64);
        }
        let seq = self.next_seq;
        self.next_seq = self.next_seq.saturating_add(1);
        seq
    }

    fn snapshot(&self) -> String {
        let bytes = self.buffer.iter().copied().collect::<Vec<_>>();
        String::from_utf8_lossy(&bytes).into_owned()
    }
}


impl ShellTaskEntry {
    fn snapshot(&self, shell_task_id: &str) -> ShellTaskSnapshot {
        ShellTaskSnapshot {
            shell_task_id: shell_task_id.to_owned(),
            conversation_id: self.conversation_id.clone(),
            tool_name: self.tool_name.clone(),
            command: self.command.clone(),
            // A finished row is not "stopping" however it ended: the process is already gone, and a
            // row that still claimed to be winding down would keep its spinner forever.
            stopping: !self.finished() && self.stop.load(Ordering::Acquire),
            started_at: self.started_at.clone(),
            ended_at: self.ended_at.clone(),
            outcome: self.outcome,
            exit_code: self.exit_code,
            background: self.background,
        }
    }

    fn finished(&self) -> bool {
        self.outcome.is_some()
    }
}

#[derive(Default)]
struct Registry {
    entries: HashMap<String, ShellTaskEntry>,
    store: Option<crate::shell_task_store::ShellTaskStore>,
    write_error: Option<String>,
}

impl Registry {
    fn persist(&mut self) -> Result<(), String> {
        let Some(store) = self.store.as_ref() else {
            #[cfg(test)]
            return Ok(());
            #[cfg(not(test))]
            return Err("命令历史存储尚未初始化，拒绝无耐久记录的任务交接".into());
        };
        let records = self.entries.iter().map(|(id, entry)| crate::shell_task_store::ShellTaskRecord {
            snapshot: entry.snapshot(id),
            output: entry.output.snapshot(),
            dropped_head_bytes: entry.output.dropped_head_bytes,
            sequence: entry.sequence,
        }).collect::<Vec<_>>();
        let result = store.replace(&records);
        self.write_error = result.as_ref().err().cloned();
        result
    }

    /// Drops the oldest finished rows of one conversation until both retention bounds hold. Only
    /// finished ones are candidates — a running command is never evicted however old it is, because
    /// dropping it would take its stop button with it.
    fn evict_finished(&mut self, conversation_id: &str) -> Vec<String> {
        let mut finished = self
            .entries
            .iter()
            .filter(|(_, entry)| entry.conversation_id == conversation_id && entry.finished())
            .map(|(shell_task_id, entry)| {
                (entry.sequence, shell_task_id.clone(), entry.output.buffer.len())
            })
            .collect::<Vec<_>>();
        finished.sort_by_key(|(sequence, _, _)| *sequence);
        let mut retained_bytes = finished
            .iter()
            .map(|(_, _, bytes)| *bytes)
            .sum::<usize>();
        let mut remaining = finished.len();
        let mut evicted = Vec::new();
        for (_, shell_task_id, bytes) in finished {
            if remaining <= MAX_FINISHED_PER_CONVERSATION
                && retained_bytes <= MAX_RETAINED_OUTPUT_PER_CONVERSATION
            {
                break;
            }
            self.entries.remove(&shell_task_id);
            retained_bytes = retained_bytes.saturating_sub(bytes);
            remaining -= 1;
            evicted.push(shell_task_id);
        }
        evicted
    }
}

#[derive(Clone, Default)]
pub struct ShellTaskRegistry {
    inner: Arc<Mutex<Registry>>,
    next_id: Arc<AtomicU64>,
    /// Push hub the start/end events go out through. `None` in tests and until
    /// the hub is attached at startup; publishing is then simply skipped, which
    /// keeps the registry itself usable without a Tauri runtime.
    events: Arc<Mutex<Option<AppEventHub>>>,
}

/// Marks its entry finished when the tool call ends, however it ends — normal exit, kill, or an
/// early `?` on a spawn failure. The row survives as a finished one; what must never survive is a
/// row that still claims to be running, because its stop button could do nothing.
pub struct ShellTaskGuard {
    registry: ShellTaskRegistry,
    shell_task_id: String,
    stop: Arc<AtomicBool>,
    /// Outcome the `Drop` records. `finish` sets it from the real exit; a call that unwinds before
    /// reporting one leaves it `None`, and `Drop` then records a stop — an abandoned command did
    /// not succeed, and claiming it did would be the one lie the row must not tell.
    outcome: Option<(ShellTaskOutcome, Option<i32>)>,
}

impl ShellTaskGuard {
    /// True once a stop has been requested for this task. The running command polls this.
    pub fn stop_requested(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }

    /// Records how the command ended, before the guard drops. The `Drop` still runs and is what
    /// actually retires the row; this only supplies the outcome it will record.
    pub fn finish(&mut self, outcome: ShellTaskOutcome, exit_code: Option<i32>) {
        self.outcome = Some((outcome, exit_code));
    }

    /// The registry id (`shell-N`) this guard's row was minted under. The
    /// background leg returns it to the model as the `shell:<id>` task address
    /// in the spawn receipt, and links the pool entry to the registry row
    /// through it. The synchronous leg never needs it — by the time its
    /// receipt exists the command has already finished.
    pub fn shell_task_id(&self) -> &str {
        &self.shell_task_id
    }

    /// A handle the pipe-reader threads append through. `collect_pipe` spawns a `move` closure per
    /// pipe, so this has to be owned and `Send + 'static` rather than a borrow of the guard.
    pub fn output_sink(&self, stream: ShellOutputStream) -> ShellOutputSink {
        ShellOutputSink {
            registry: self.registry.clone(),
            shell_task_id: self.shell_task_id.clone(),
            stream,
            decoder: ConsoleTextDecoder::new(),
        }
    }
}

/// One pipe's write end into a command's live output buffer.
#[derive(Clone)]
pub struct ShellOutputSink {
    registry: ShellTaskRegistry,
    shell_task_id: String,
    stream: ShellOutputStream,
    decoder: ConsoleTextDecoder,
}

impl ShellOutputSink {
    pub fn append(&mut self, bytes: &[u8]) {
        let text = self.decoder.push(bytes);
        self.forward(&text);
    }

    pub fn finish(&mut self) {
        let text = self.decoder.finish();
        self.forward(&text);
    }

    fn forward(&self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.registry
            .append_output(&self.shell_task_id, self.stream, text);
    }
}

impl Drop for ShellTaskGuard {
    fn drop(&mut self) {
        let (outcome, exit_code) = self
            .outcome
            .unwrap_or((ShellTaskOutcome::Stopped, None));
        self.registry.finish(&self.shell_task_id, outcome, exit_code);
    }
}

impl ShellTaskRegistry {
    pub(crate) fn install_store(&self, directory: &std::path::Path) -> Result<(), String> {
        let mut registry = self.lock();
        if registry.store.is_some() {
            return Ok(());
        }
        if !registry.entries.is_empty() {
            return Err("命令历史必须在首个任务登记前初始化".into());
        }
        let store = crate::shell_task_store::ShellTaskStore::open(directory)?;
        let records = store.load()?;
        let mut restored = Registry { store: Some(store), ..Registry::default() };
        let mut sequence = 0;
        for record in records {
            sequence = sequence.max(record.sequence);
            let mut snapshot = record.snapshot;
            let mut output = ShellTaskOutput::default();
            output.push(record.output.as_bytes());
            output.dropped_head_bytes = record.dropped_head_bytes;
            if snapshot.outcome.is_none() {
                snapshot.outcome = Some(ShellTaskOutcome::Failed);
                snapshot.exit_code = None;
                snapshot.ended_at = None;
                output.push("\r\n[恢复说明] 宿主退出，最终退出码与结束时间未知，未自动重跑。\r\n[Recovery] Host exited; final exit code and end time are unknown. The command was not rerun.\r\n".as_bytes());
            }
            restored.entries.insert(snapshot.shell_task_id, ShellTaskEntry {
                conversation_id: snapshot.conversation_id,
                tool_name: snapshot.tool_name,
                command: snapshot.command,
                started_at: snapshot.started_at,
                stop: Arc::new(AtomicBool::new(false)),
                ended_at: snapshot.ended_at,
                outcome: snapshot.outcome,
                exit_code: snapshot.exit_code,
                background: snapshot.background,
                sequence: record.sequence,
                output,
            });
        }
        let conversations = restored.entries.values().map(|entry| entry.conversation_id.clone())
            .collect::<std::collections::HashSet<_>>();
        for conversation in conversations {
            restored.evict_finished(&conversation);
        }
        restored.persist()?;
        self.next_id.store(sequence, Ordering::Relaxed);
        *registry = restored;
        Ok(())
    }

    pub(crate) fn flush(&self) -> Result<(), String> {
        self.lock().persist()
    }

    fn report_write_error(&self) {
        let error = self.lock().write_error.clone();
        if let Some(error) = error {
            self.publish(AppPushEvent::DocumentWriteFailure {
                message: format!("命令历史保存失败 / Shell history save failed: {error}"),
            });
        }
    }

    /// Attaches the push hub every later start/end event is published through.
    /// Called once at startup, before any model run can begin.
    pub fn attach_events(&self, hub: AppEventHub) {
        *self
            .events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(hub);
    }

    fn publish(&self, event: AppPushEvent) {
        let hub = self
            .events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        if let Some(hub) = hub {
            hub.publish(event);
        }
    }

    /// Registers a shell command about to start. The returned guard carries the
    /// stop flag and retires the row on drop. Background commands survive run
    /// settlement and cancellation; synchronous commands pass false.
    #[cfg(test)]
    pub fn register(&self, conversation_id: &str, tool_name: &str, command: &str, background: bool) -> ShellTaskGuard {
        self.try_register(conversation_id, tool_name, command, background).unwrap()
    }

    pub fn try_register(
        &self,
        conversation_id: &str,
        tool_name: &str,
        command: &str,
        background: bool,
    ) -> Result<ShellTaskGuard, String> {
        // The counter, not a random id: two commands started in the same millisecond must still be
        // two rows, and a monotonic id keeps the sidebar order stable.
        let sequence = self.next_id.fetch_add(1, Ordering::Relaxed).saturating_add(1);
        let shell_task_id = format!("shell-{}-{sequence}", uuid::Uuid::new_v4());
        let stop = Arc::new(AtomicBool::new(false));
        let started_at = Utc::now().to_rfc3339();
        let entry = ShellTaskEntry {
            conversation_id: conversation_id.to_owned(),
            tool_name: tool_name.to_owned(),
            command: display_command(command),
            started_at,
            stop: stop.clone(),
            ended_at: None,
            outcome: None,
            exit_code: None,
            background,
            sequence,
            output: ShellTaskOutput::default(),
        };
        let snapshot = entry.snapshot(&shell_task_id);
        let evicted = {
            let mut registry = self.lock();
            registry.entries.insert(shell_task_id.clone(), entry);
            let evicted = registry.evict_finished(conversation_id);
            if let Err(error) = registry.persist() {
                registry.entries.remove(&shell_task_id);
                return Err(error);
            }
            evicted
        };
        for evicted_id in evicted {
            self.publish(AppPushEvent::ShellTaskEvicted {
                conversation_id: conversation_id.to_owned(),
                shell_task_id: evicted_id,
            });
        }
        // Published after the insert, so a renderer that answers by asking for the
        // list gets one that already contains this row.
        self.publish(AppPushEvent::ShellTaskStarted { task: snapshot });
        Ok(ShellTaskGuard {
            registry: self.clone(),
            shell_task_id,
            stop,
            outcome: None,
        })
    }

    /// Reclassifies a running command as a background task.
    ///
    /// A command registered by the synchronous leg starts as a foreground row,
    /// and that is what decides who may stop it: a foreground command dies with
    /// its run's cancellation, a background one only with its own stop button.
    /// When a deadline hands a running command to a task slot, that ownership
    /// really has changed, and the row has to say so or the next cancellation
    /// would kill a command the model was told is still running.
    ///
    /// Returns false for an unknown, finished, or foreign-conversation row.
    pub fn mark_background(&self, conversation_id: &str, shell_task_id: &str) -> bool {
        let snapshot = {
            let mut registry = self.lock();
            let Some(entry) = registry
                .entries
                .get_mut(shell_task_id)
                .filter(|entry| entry.conversation_id == conversation_id && !entry.finished())
            else {
                return false;
            };
            let previous_background = entry.background;
            entry.background = true;
            let snapshot = entry.snapshot(shell_task_id);
            if registry.persist().is_err() {
                registry.entries.get_mut(shell_task_id).unwrap().background = previous_background;
                drop(registry);
                self.report_write_error();
                return false;
            }
            snapshot
        };
        // Published so the sidebar row stops being attributed to the turn that
        // started it while that turn is still running.
        self.publish(AppPushEvent::ShellTaskStarted { task: snapshot });
        true
    }

    /// Copies every finished row of one conversation into another under fresh ids, output tail
    /// included, and publishes `ShellTaskEnded` for each clone so the renderer paints the rows.
    ///
    /// A forked conversation inherits the parent's transcript, and a command the parent ran is part
    /// of that story: the copied tool cards reference rows the child would otherwise not have.
    ///
    /// Running rows are not copied. A live command belongs to the source's tool call — one process,
    /// one stop button — and a second row pointing at it would hand a stop to a conversation that
    /// never started it. Clones are minted in ascending source order so the sidebar reads the same
    /// way in both conversations, and each gets a fresh stop flag rather than sharing the source's.
    /// Returns the clones that survived the target's retention bound.
    #[cfg(test)]
    pub fn clone_finished_rows(&self, source: &str, target: &str) -> Vec<ShellTaskSnapshot> {
        self.try_clone_finished_rows(source, target).unwrap()
    }

    pub fn try_clone_finished_rows(
        &self,
        source_conversation_id: &str,
        target_conversation_id: &str,
    ) -> Result<Vec<ShellTaskSnapshot>, String> {
        let (snapshots, evicted) = {
            let mut registry = self.lock();
            let mut sources = registry
                .entries
                .values()
                .filter(|entry| {
                    entry.conversation_id == source_conversation_id && entry.finished()
                })
                .map(|entry| {
                    (
                        entry.sequence,
                        ShellTaskEntry {
                            conversation_id: target_conversation_id.to_owned(),
                            tool_name: entry.tool_name.clone(),
                            command: entry.command.clone(),
                            started_at: entry.started_at.clone(),
                            // Never the source's `Arc`: a finished row has nothing left to stop, and
                            // sharing the flag would let one conversation raise another's.
                            stop: Arc::new(AtomicBool::new(false)),
                            ended_at: entry.ended_at.clone(),
                            outcome: entry.outcome,
                            exit_code: entry.exit_code,
                            background: entry.background,
                            // Replaced below by the clone's own minted number.
                            sequence: entry.sequence,
                            output: ShellTaskOutput {
                                buffer: entry.output.buffer.clone(),
                                dropped_head_bytes: entry.output.dropped_head_bytes,
                                next_seq: entry.output.next_seq,
                                // The sink and its subscription belong to whoever is watching the
                                // source; a clone starts with no watcher and no writer.
                                ..ShellTaskOutput::default()
                            },
                        },
                    )
                })
                .collect::<Vec<_>>();
            sources.sort_by_key(|(sequence, _)| *sequence);
            let mut snapshots = Vec::with_capacity(sources.len());
            for (_, mut entry) in sources {
                let sequence = self.next_id.fetch_add(1, Ordering::Relaxed).saturating_add(1);
                let shell_task_id = format!("shell-{}-{sequence}", uuid::Uuid::new_v4());
                entry.sequence = sequence;
                snapshots.push(entry.snapshot(&shell_task_id));
                registry.entries.insert(shell_task_id, entry);
            }
            let evicted = registry.evict_finished(target_conversation_id);
            if let Err(error) = registry.persist() {
                for snapshot in &snapshots {
                    registry.entries.remove(&snapshot.shell_task_id);
                }
                return Err(error);
            }
            (snapshots, evicted)
        };
        for evicted_id in &evicted {
            self.publish(AppPushEvent::ShellTaskEvicted {
                conversation_id: target_conversation_id.to_owned(),
                shell_task_id: evicted_id.clone(),
            });
        }
        // A clone the target's own retention bound just dropped is not a row anyone can open, so
        // only the survivors are announced and returned.
        let survivors = snapshots
            .into_iter()
            .filter(|snapshot| !evicted.contains(&snapshot.shell_task_id))
            .collect::<Vec<_>>();
        for snapshot in &survivors {
            self.publish(AppPushEvent::ShellTaskEnded {
                task: snapshot.clone(),
            });
        }
        Ok(survivors)
    }

    /// Asks the named command to stop. Returns false when the task is unknown, already finished, or
    /// belongs to another conversation. The caller must supply the owning conversation: a stop is a
    /// per-conversation action, and one conversation may never reach into another's processes.
    pub fn request_stop(&self, conversation_id: &str, shell_task_id: &str) -> bool {
        self.lock()
            .entries
            .get(shell_task_id)
            .filter(|entry| entry.conversation_id == conversation_id && !entry.finished())
            .map(|entry| {
                entry.stop.store(true, Ordering::Release);
                true
            })
            .unwrap_or(false)
    }

    /// Stops every running command owned by a conversation. Finished rows remain.
    /// This bulk operation is reserved for tests and an explicit future stop-all
    /// action; production cancellation follows the owning run/task signal.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn stop_conversation(&self, conversation_id: &str) -> usize {
        let registry = self.lock();
        registry
            .entries
            .values()
            .filter(|entry| entry.conversation_id == conversation_id && !entry.finished())
            .map(|entry| entry.stop.store(true, Ordering::Release))
            .count()
    }

    /// Every command owned by a conversation, running and finished, ordered by mint sequence so the
    /// sidebar does not reshuffle between polls.
    pub fn task_snapshots(&self, conversation_id: &str) -> Vec<ShellTaskSnapshot> {
        let registry = self.lock();
        let mut snapshots = registry
            .entries
            .iter()
            .filter(|(_, entry)| entry.conversation_id == conversation_id)
            .map(|(shell_task_id, entry)| (entry.sequence, entry.snapshot(shell_task_id)))
            .collect::<Vec<_>>();
        snapshots.sort_by_key(|(sequence, _)| *sequence);
        snapshots
            .into_iter()
            .map(|(_, snapshot)| snapshot)
            .collect()
    }

    /// One command by id, scoped to its owning conversation. Finished commands are included; the
    /// snapshot's `outcome` is what tells them apart.
    pub fn task_snapshot(
        &self,
        conversation_id: &str,
        shell_task_id: &str,
    ) -> Option<ShellTaskSnapshot> {
        self.lock()
            .entries
            .get(shell_task_id)
            .filter(|entry| entry.conversation_id == conversation_id)
            .map(|entry| entry.snapshot(shell_task_id))
    }

    /// Whether this command is still running. `task_wait` settles on it: a finished row stays in
    /// the registry forever, so presence alone can no longer answer the question.
    pub fn is_running(&self, conversation_id: &str, shell_task_id: &str) -> bool {
        self.lock()
            .entries
            .get(shell_task_id)
            .is_some_and(|entry| entry.conversation_id == conversation_id && !entry.finished())
    }

    /// Appends one chunk of live output and forwards it to whoever is watching.
    ///
    /// The send happens **after** the lock is released. The registry is a single mutex that
    /// `task_snapshots` holds while it clones and sorts a whole conversation, so holding it across
    /// an IPC send would block every other reader for the duration of that send — and calling back
    /// into the registry from inside it would deadlock outright.
    fn append_output(&self, shell_task_id: &str, stream: ShellOutputStream, text: &str) {
        let delivery = {
            let mut registry = self.lock();
            let Some(entry) = registry.entries.get_mut(shell_task_id) else {
                return;
            };
            let seq = entry.output.push(text.as_bytes());
            entry
                .output
                .sink
                .clone()
                .map(|sink| (sink, seq))
        };
        let Some((sink, seq)) = delivery else {
            return;
        };
        let _ = sink.send(ShellOutputEvent::Output {
            shell_task_id: shell_task_id.to_owned(),
            stream,
            seq,
            text: text.to_owned(),
        });
    }

    /// Starts watching one command: installs the sink and takes the retained buffer in the **same**
    /// critical section. Reading the buffer and subscribing as two steps would drop whatever
    /// arrived in between, which for a command mid-build is exactly the interesting part.
    ///
    /// A command that has already finished gets its buffer and no sink — there is nothing more to
    /// send, and an installed sink would outlive its only possible writer.
    pub fn subscribe_output(
        &self,
        conversation_id: &str,
        shell_task_id: &str,
        sink: Channel<ShellOutputEvent>,
    ) -> Option<ShellTaskOutputHandle> {
        let mut registry = self.lock();
        let entry = registry
            .entries
            .get_mut(shell_task_id)
            .filter(|entry| entry.conversation_id == conversation_id)?;
        let live = !entry.finished();
        let mut subscription_id = 0;
        if live {
            entry.output.next_subscription += 1;
            subscription_id = entry.output.next_subscription;
            entry.output.sink = Some(sink);
            entry.output.sink_subscription = subscription_id;
        }
        Some(ShellTaskOutputHandle {
            snapshot: entry.output.snapshot(),
            live,
            dropped_head_bytes: entry.output.dropped_head_bytes,
            subscription_id,
        })
    }

    /// Stops watching. Idempotent, and scoped to the owning conversation like every other entry
    /// point here.
    ///
    /// `subscription` is the id the matching subscribe handed back. A page is torn down and
    /// rebuilt in one breath — React runs an effect, its cleanup, and the effect again — and the
    /// three calls that produces do not have to reach the host in order. Detaching only the
    /// sink the caller installed keeps the second subscriber's sink where a late first detach
    /// would otherwise have removed it, leaving a page that shows the replay and then nothing.
    /// `None` detaches whatever is installed, for a caller that never saw a handle.
    pub fn detach_output(
        &self,
        conversation_id: &str,
        shell_task_id: &str,
        subscription: Option<u64>,
    ) -> bool {
        let mut registry = self.lock();
        let Some(entry) = registry
            .entries
            .get_mut(shell_task_id)
            .filter(|entry| entry.conversation_id == conversation_id)
        else {
            return false;
        };
        if subscription.is_none_or(|id| id == entry.output.sink_subscription) {
            entry.output.sink = None;
        }
        true
    }

    /// Drops the rows of every conversation not in `retained`. Called after a document save, which
    /// is the one event that can make a conversation — and with it the whole point of its command
    /// history — go away. Retention is otherwise unbounded in time on purpose: a finished row is
    /// the sidebar's only record that a command ever ran.
    ///
    /// A dropped row may still have a **live** process behind it. Its stop flag is raised before
    /// the row goes: the guard shares the same `Arc`, the command's poll loop sees it and kills
    /// the tree, and the guard's eventual `finish` lands on a missing row and is a silent no-op
    /// (idempotent by design). Dropping without stopping would orphan the process — the row that
    /// held the only stop button is gone, but the command keeps running until it exits on its own.
    #[cfg(test)]
    pub fn forget_missing<'a>(&self, retained: impl IntoIterator<Item = &'a str>) {
        self.try_forget_missing(retained).unwrap();
    }

    pub fn try_forget_missing<'a>(&self, retained: impl IntoIterator<Item = &'a str>) -> Result<(), String> {
        let retained = retained.into_iter().collect::<std::collections::HashSet<_>>();
        let mut registry = self.lock();
        registry.entries.retain(|_, entry| {
            if retained.contains(entry.conversation_id.as_str()) {
                return true;
            }
            if !entry.finished() {
                entry.stop.store(true, Ordering::Release);
            }
            false
        });
        registry.persist()
    }

    /// Marks a command finished and publishes the terminal snapshot. Idempotent: a second call for
    /// the same id — a `finish` followed by the guard's own `Drop` — leaves the first outcome
    /// standing, because that one described the real exit.
    fn finish(&self, shell_task_id: &str, outcome: ShellTaskOutcome, exit_code: Option<i32>) {
        let (snapshot, output_sink, conversation_id, evicted) = {
            let mut registry = self.lock();
            let Some(entry) = registry.entries.get_mut(shell_task_id) else {
                return;
            };
            if entry.finished() {
                return;
            }
            entry.ended_at = Some(Utc::now().to_rfc3339());
            entry.outcome = Some(outcome);
            entry.exit_code = exit_code;
            // Taken, not cloned: nothing can write to this command again, so the watcher's channel
            // must not be held past the one event that says so.
            let output_sink = entry.output.sink.take();
            let conversation_id = entry.conversation_id.clone();
            let snapshot = entry.snapshot(shell_task_id);
            let evicted = registry.evict_finished(&conversation_id);
            let _ = registry.persist();
            (snapshot, output_sink, conversation_id, evicted)
        };
        self.report_write_error();
        if let Some(sink) = output_sink {
            let _ = sink.send(ShellOutputEvent::End {
                shell_task_id: shell_task_id.to_owned(),
                outcome,
                exit_code,
            });
        }
        self.publish(AppPushEvent::ShellTaskEnded { task: snapshot });
        for evicted_id in evicted {
            self.publish(AppPushEvent::ShellTaskEvicted {
                conversation_id: conversation_id.clone(),
                shell_task_id: evicted_id,
            });
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Registry> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Sorting by the minted number rather than the string keeps `shell-10` after `shell-9`. The
/// registry itself sorts on the entry's `sequence`; this is how a test checks the id the renderer
/// sees agrees with that order.
#[cfg(test)]
fn numeric_suffix(shell_task_id: &str) -> u64 {
    shell_task_id
        .rsplit('-')
        .next()
        .and_then(|digits| digits.parse().ok())
        .unwrap_or(0)
}

/// Collapses a command to one display line. Newlines would break the sidebar row, and a heredoc or
/// a multi-line script is not readable there anyway.
fn display_command(command: &str) -> String {
    let single_line = command.split_whitespace().collect::<Vec<_>>().join(" ");
    if single_line.chars().count() <= MAX_DISPLAY_COMMAND {
        return single_line;
    }
    let truncated = single_line
        .chars()
        .take(MAX_DISPLAY_COMMAND)
        .collect::<String>();
    format!("{truncated}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_registry_generations_recover_background_history_without_restarting_processes() {
        let directory = tempfile::tempdir().unwrap();
        let registry = ShellTaskRegistry::default();
        registry.install_store(directory.path()).unwrap();
        let mut guard = registry.register("conversation-a", "bash", "long command", true);
        let id = guard.shell_task_id().to_owned();
        let second = ShellTaskRegistry::default();
        assert!(second.install_store(directory.path()).is_err());
        assert_eq!(registry.task_snapshot("conversation-a", &id).unwrap().outcome, None);
        drop(second);
        // Simulate host death, not a guard's graceful completion. Dispose of
        // every first-generation registry/hub reference before reopening.
        guard.registry = ShellTaskRegistry::default();
        drop(guard);
        drop(registry);
        let recovered = ShellTaskRegistry::default();
        recovered.install_store(directory.path()).unwrap();
        let rows = recovered.task_snapshots("conversation-a");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].shell_task_id, id);
        assert_eq!(rows[0].outcome, Some(ShellTaskOutcome::Failed));
        assert_eq!(rows[0].exit_code, None);
        assert_eq!(rows[0].ended_at, None);
        assert!(!recovered.request_stop("conversation-a", &id));
        let output = recovered.subscribe_output("conversation-a", &id, Channel::new(|_| Ok(()))).unwrap();
        assert!(!output.live);
        assert!(output.snapshot.contains("宿主退出"));
        assert!(output.snapshot.contains("未自动重跑"));
        assert!(recovered.task_snapshots("conversation-b").is_empty());
        drop(directory);
    }

    #[test]
    fn durable_shell_outcomes_promotion_and_ids_survive_two_restarts() {
        let directory = tempfile::tempdir().unwrap();
        let registry = ShellTaskRegistry::default();
        registry.install_store(directory.path()).unwrap();
        let mut expected = Vec::new();
        for (outcome, code) in [
            (ShellTaskOutcome::Succeeded, Some(0)),
            (ShellTaskOutcome::Failed, Some(17)),
            (ShellTaskOutcome::Stopped, None),
        ] {
            let mut guard = registry.register("conv", "bash", "completed command", true);
            expected.push((guard.shell_task_id().to_owned(), outcome, code));
            registry.append_output(guard.shell_task_id(), ShellOutputStream::Stdout, "durable output tail");
            guard.finish(outcome, code);
        }
        let mut promoted = registry.register("conv", "powershell", "promoted command", false);
        let promoted_id = promoted.shell_task_id().to_owned();
        assert!(registry.mark_background("conv", &promoted_id));
        promoted.registry = ShellTaskRegistry::default();
        drop(promoted);
        drop(registry);
        let mut prior_recovery = None;
        for _ in 0..2 {
            let registry = ShellTaskRegistry::default();
            registry.install_store(directory.path()).unwrap();
            for (id, outcome, code) in &expected {
                let row = registry.task_snapshot("conv", id).unwrap();
                assert_eq!(row.outcome, Some(*outcome));
                assert_eq!(row.exit_code, *code);
                assert!(row.ended_at.is_some());
                let output = registry.subscribe_output("conv", id, Channel::new(|_| Ok(()))).unwrap();
                assert_eq!(output.snapshot, "durable output tail");
                assert!(!output.live);
            }
            let row = registry.task_snapshot("conv", &promoted_id).unwrap();
            assert!(row.background);
            assert_eq!(row.outcome, Some(ShellTaskOutcome::Failed));
            let output = registry.subscribe_output("conv", &promoted_id, Channel::new(|_| Ok(()))).unwrap().snapshot;
            if let Some(previous) = prior_recovery.as_ref() {
                assert_eq!(previous, &output);
            }
            prior_recovery = Some(output);
            let guard = registry.register("conv", "bash", "new generation", true);
            assert_ne!(guard.shell_task_id(), promoted_id);
            assert!(expected.iter().all(|(id, _, _)| id != guard.shell_task_id()));
            drop(guard);
            drop(registry);
        }
    }

    #[test]
    fn durable_shell_write_failure_refuses_registration_and_promotion_and_retries_completion() {
        let directory = tempfile::tempdir().unwrap();
        let registry = ShellTaskRegistry::default();
        registry.install_store(directory.path()).unwrap();
        let hub = AppEventHub::default();
        let (channel, received) = collecting_push_channel();
        hub.subscribe(channel);
        registry.attach_events(hub);
        let mut guard = registry.register("conv", "bash", "foreground", false);
        let id = guard.shell_task_id().to_owned();
        let connection = rusqlite::Connection::open(directory.path().join(crate::shell_task_store::DATABASE_FILE_NAME)).unwrap();
        connection.execute_batch("CREATE TRIGGER refuse_shell_write BEFORE INSERT ON shell_task BEGIN SELECT RAISE(FAIL, 'injected shell write failure'); END;").unwrap();
        assert!(registry.try_register("conv", "bash", "must not start", true).is_err());
        assert_eq!(registry.task_snapshots("conv").len(), 1);
        assert!(!registry.mark_background("conv", &id));
        assert!(!registry.task_snapshot("conv", &id).unwrap().background);
        guard.finish(ShellTaskOutcome::Succeeded, Some(0));
        drop(guard);
        assert!(registry.flush().unwrap_err().contains("injected shell write failure"));
        assert!(received.lock().unwrap().iter().any(|event| event["type"] == "documentWriteFailure"));
        connection.execute_batch("DROP TRIGGER refuse_shell_write;").unwrap();
        registry.flush().unwrap();
        drop(connection);
        drop(registry);
        let restored = ShellTaskRegistry::default();
        restored.install_store(directory.path()).unwrap();
        assert_eq!(restored.task_snapshot("conv", &id).unwrap().outcome, Some(ShellTaskOutcome::Succeeded));
    }

    #[test]
    fn durable_shell_forks_and_deletions_do_not_resurrect_removed_rows() {
        let directory = tempfile::tempdir().unwrap();
        let registry = ShellTaskRegistry::default();
        registry.install_store(directory.path()).unwrap();
        let mut guard = registry.register("parent", "bash", "copy me", true);
        registry.append_output(guard.shell_task_id(), ShellOutputStream::Stdout, "copied output");
        guard.finish(ShellTaskOutcome::Succeeded, Some(0));
        drop(guard);
        let mut running = registry.register("parent", "bash", "do not copy me", true);
        let copies = registry.try_clone_finished_rows("parent", "child").unwrap();
        assert_eq!(copies.len(), 1);
        registry.try_forget_missing(["child"]).unwrap();
        assert!(running.stop_requested());
        running.registry = ShellTaskRegistry::default();
        drop(running);
        drop(registry);
        let restored = ShellTaskRegistry::default();
        restored.install_store(directory.path()).unwrap();
        assert!(restored.task_snapshots("parent").is_empty());
        assert_eq!(restored.task_snapshots("child"), copies);
        let output = restored.subscribe_output("child", &copies[0].shell_task_id, Channel::new(|_| Ok(()))).unwrap();
        assert_eq!(output.snapshot, "copied output");
        assert!(restored.subscribe_output("parent", &copies[0].shell_task_id, Channel::new(|_| Ok(()))).is_none());
    }

    #[test]
    fn durable_shell_ids_are_not_reused_after_all_history_is_deleted() {
        let directory = tempfile::tempdir().unwrap();
        let registry = ShellTaskRegistry::default();
        registry.install_store(directory.path()).unwrap();
        let guard = registry.register("old", "bash", "old command", true);
        let id = guard.shell_task_id().to_owned();
        drop(guard);
        registry.try_forget_missing(std::iter::empty()).unwrap();
        drop(registry);
        let registry = ShellTaskRegistry::default();
        registry.install_store(directory.path()).unwrap();
        let guard = registry.register("new", "bash", "new command", true);
        assert_ne!(guard.shell_task_id(), id);
    }

    #[test]
    fn durable_shell_retention_evictions_stay_deleted_after_restart() {
        let directory = tempfile::tempdir().unwrap();
        let registry = ShellTaskRegistry::default();
        registry.install_store(directory.path()).unwrap();
        let mut first_id = String::new();
        for index in 0..=MAX_FINISHED_PER_CONVERSATION {
            let mut guard = registry.register("conv", "bash", "small retained command", true);
            if index == 0 { first_id = guard.shell_task_id().to_owned(); }
            guard.finish(ShellTaskOutcome::Succeeded, Some(0));
        }
        assert!(registry.task_snapshot("conv", &first_id).is_none());
        drop(registry);
        let restored = ShellTaskRegistry::default();
        restored.install_store(directory.path()).unwrap();
        assert_eq!(restored.task_snapshots("conv").len(), MAX_FINISHED_PER_CONVERSATION);
        assert!(restored.task_snapshot("conv", &first_id).is_none());
    }

    #[test]
    fn a_registered_command_is_visible_to_its_own_conversation_only() {
        let registry = ShellTaskRegistry::default();
        let _guard = registry.register("conversation-a", "bash", "npm install", false);

        let mine = registry.task_snapshots("conversation-a");
        assert_eq!(mine.len(), 1);
        assert_eq!(mine[0].tool_name, "bash");
        assert_eq!(mine[0].command, "npm install");
        assert!(!mine[0].stopping);
        assert!(registry.task_snapshots("conversation-b").is_empty());
    }

    /// A command that ends must stay in the list as a finished row. The user's question after a
    /// build is "did it pass", and a row that deleted itself the instant it could answer that never
    /// got to — which is exactly what this registry used to do.
    #[test]
    fn a_finished_command_is_retained_with_its_outcome() {
        let registry = ShellTaskRegistry::default();
        let shell_task_id = {
            let mut guard = registry.register("conversation-a", "bash", "npm test", false);
            assert_eq!(registry.task_snapshots("conversation-a").len(), 1);
            guard.finish(ShellTaskOutcome::Failed, Some(1));
            guard.shell_task_id().to_owned()
        };

        let snapshot = registry
            .task_snapshot("conversation-a", &shell_task_id)
            .expect("a finished command stays in the list");
        assert_eq!(snapshot.outcome, Some(ShellTaskOutcome::Failed));
        assert_eq!(snapshot.exit_code, Some(1));
        assert!(snapshot.ended_at.is_some());
        assert_eq!(snapshot.command, "npm test");
        assert!(!registry.is_running("conversation-a", &shell_task_id));
    }

    /// The outcome the guard reported wins over the one `Drop` would have invented. Losing the real
    /// exit here would repaint every completed command as "stopped by a person".
    #[test]
    fn the_reported_outcome_survives_the_drop_that_follows_it() {
        let registry = ShellTaskRegistry::default();
        let shell_task_id = {
            let mut guard = registry.register("conversation-a", "bash", "echo ok", false);
            guard.finish(ShellTaskOutcome::Succeeded, Some(0));
            guard.shell_task_id().to_owned()
        };

        let snapshot = registry
            .task_snapshot("conversation-a", &shell_task_id)
            .expect("retained");
        assert_eq!(snapshot.outcome, Some(ShellTaskOutcome::Succeeded));
        assert_eq!(snapshot.exit_code, Some(0));
    }

    /// A tool call that unwinds before it can report an exit — a spawn failure, a panic — leaves a
    /// row that must not claim success. `Drop` is the backstop, and "stopped" is the only honest
    /// thing to say about a command nobody watched finish.
    #[test]
    fn a_guard_dropped_without_an_outcome_records_a_stop() {
        let registry = ShellTaskRegistry::default();
        let shell_task_id = {
            let guard = registry.register("conversation-a", "bash", "sleep 100", false);
            guard.shell_task_id().to_owned()
        };

        let snapshot = registry
            .task_snapshot("conversation-a", &shell_task_id)
            .expect("retained");
        assert_eq!(snapshot.outcome, Some(ShellTaskOutcome::Stopped));
        assert_eq!(snapshot.exit_code, None);
    }

    /// A finished row must not keep a spinner. `stopping` says "winding down", and a process that is
    /// already gone is not winding down however it got there.
    #[test]
    fn a_finished_row_stops_reporting_that_it_is_stopping() {
        let registry = ShellTaskRegistry::default();
        let shell_task_id = {
            let mut guard = registry.register("conversation-a", "bash", "sleep 100", false);
            let id = guard.shell_task_id().to_owned();
            assert!(registry.request_stop("conversation-a", &id));
            assert!(
                registry
                    .task_snapshot("conversation-a", &id)
                    .expect("still running")
                    .stopping
            );
            guard.finish(ShellTaskOutcome::Stopped, None);
            id
        };

        assert!(
            !registry
                .task_snapshot("conversation-a", &shell_task_id)
                .expect("retained")
                .stopping
        );
    }

    /// Stopping a finished command is a refusal, not a no-op that reports success: the renderer uses
    /// the return to decide whether to paint the row as winding down, and there is no process left.
    #[test]
    fn a_finished_command_cannot_be_stopped_again() {
        let registry = ShellTaskRegistry::default();
        let shell_task_id = {
            let mut guard = registry.register("conversation-a", "bash", "echo ok", false);
            guard.finish(ShellTaskOutcome::Succeeded, Some(0));
            guard.shell_task_id().to_owned()
        };

        assert!(!registry.request_stop("conversation-a", &shell_task_id));
        assert_eq!(registry.stop_conversation("conversation-a"), 0);
    }

    /// Retention is bounded. A session that runs hundreds of commands would otherwise grow the list
    /// — and the payload of every reconcile — without limit.
    #[test]
    fn finished_rows_are_capped_oldest_first() {
        let registry = ShellTaskRegistry::default();
        for index in 0..(MAX_FINISHED_PER_CONVERSATION + 10) {
            let mut guard = registry.register("conversation-a", "bash", &format!("echo {index}"), false);
            guard.finish(ShellTaskOutcome::Succeeded, Some(0));
        }

        let snapshots = registry.task_snapshots("conversation-a");
        assert_eq!(snapshots.len(), MAX_FINISHED_PER_CONVERSATION);
        // The survivors are the newest: the oldest ten were evicted, so the list starts at `echo 10`.
        assert_eq!(snapshots[0].command, "echo 10");
        assert_eq!(
            snapshots[snapshots.len() - 1].command,
            format!("echo {}", MAX_FINISHED_PER_CONVERSATION + 9)
        );
    }

    #[test]
    fn count_eviction_publishes_each_removed_row() {
        let registry = ShellTaskRegistry::default();
        let hub = AppEventHub::default();
        let (channel, received) = collecting_push_channel();
        hub.subscribe(channel);
        registry.attach_events(hub);

        for index in 0..(MAX_FINISHED_PER_CONVERSATION + 3) {
            let mut guard =
                registry.register("conversation-a", "bash", &format!("echo {index}"), false);
            guard.finish(ShellTaskOutcome::Succeeded, Some(0));
        }

        let events = received.lock().unwrap();
        let evicted = events
            .iter()
            .filter(|event| event["type"] == "shellTaskEvicted")
            .collect::<Vec<_>>();
        assert_eq!(evicted.len(), 3);
        assert_eq!(evicted[0]["conversationId"], "conversation-a");
        assert_eq!(numeric_suffix(evicted[0]["shellTaskId"].as_str().unwrap()), 1);
        assert_eq!(numeric_suffix(evicted[2]["shellTaskId"].as_str().unwrap()), 3);
    }

    #[test]
    fn finished_output_is_bounded_by_conversation_byte_budget() {
        let registry = ShellTaskRegistry::default();
        let chunk = "x".repeat(MAX_LIVE_OUTPUT);
        let rows_to_exceed_budget = MAX_RETAINED_OUTPUT_PER_CONVERSATION / MAX_LIVE_OUTPUT + 1;

        for index in 0..rows_to_exceed_budget {
            let mut guard =
                registry.register("conversation-a", "bash", &format!("echo {index}"), false);
            registry.append_output(
                guard.shell_task_id(),
                ShellOutputStream::Stdout,
                &chunk,
            );
            guard.finish(ShellTaskOutcome::Succeeded, Some(0));
        }

        let snapshots = registry.task_snapshots("conversation-a");
        assert_eq!(snapshots.len(), rows_to_exceed_budget - 1);
        assert_eq!(snapshots[0].command, "echo 1");
    }

    /// Eviction may never take a running command, however old. Dropping one would take its stop
    /// button with it and leave a live process nobody can reach.
    #[test]
    fn a_running_command_is_never_evicted_by_the_cap() {
        let registry = ShellTaskRegistry::default();
        let running = registry.register("conversation-a", "bash", "sleep 100", false);
        let running_id = running.shell_task_id().to_owned();
        for index in 0..(MAX_FINISHED_PER_CONVERSATION + 10) {
            let mut guard = registry.register("conversation-a", "bash", &format!("echo {index}"), false);
            guard.finish(ShellTaskOutcome::Succeeded, Some(0));
        }

        assert!(registry.is_running("conversation-a", &running_id));
        assert!(registry
            .task_snapshots("conversation-a")
            .iter()
            .any(|snapshot| snapshot.shell_task_id == running_id));
    }

    /// The cap is per conversation. One busy conversation must not evict another's history.
    #[test]
    fn the_cap_does_not_reach_across_conversations() {
        let registry = ShellTaskRegistry::default();
        let mut kept = registry.register("conversation-b", "bash", "echo keep me", false);
        kept.finish(ShellTaskOutcome::Succeeded, Some(0));
        for index in 0..(MAX_FINISHED_PER_CONVERSATION + 10) {
            let mut guard = registry.register("conversation-a", "bash", &format!("echo {index}"), false);
            guard.finish(ShellTaskOutcome::Succeeded, Some(0));
        }

        let other = registry.task_snapshots("conversation-b");
        assert_eq!(other.len(), 1);
        assert_eq!(other[0].command, "echo keep me");
    }

    /// A deleted conversation's command history is meaningless, and nothing will ever ask for it
    /// again. This is the one thing that clears retained rows.
    #[test]
    fn forgetting_a_conversation_clears_its_retained_rows() {
        let registry = ShellTaskRegistry::default();
        {
            let mut mine = registry.register("conversation-a", "bash", "echo mine", false);
            mine.finish(ShellTaskOutcome::Succeeded, Some(0));
            let mut theirs = registry.register("conversation-b", "bash", "echo theirs", false);
            theirs.finish(ShellTaskOutcome::Succeeded, Some(0));
        }

        registry.forget_missing(["conversation-b"]);
        assert!(registry.task_snapshots("conversation-a").is_empty());
        assert_eq!(registry.task_snapshots("conversation-b").len(), 1);
    }

    /// Removing a conversation may remove a row while its command still runs.
    /// Raise the shared stop flag before removing the row so the polling guard
    /// kills the process tree rather than orphaning it.
    #[test]
    fn forgetting_a_conversation_stops_its_live_commands_instead_of_orphaning_them() {
        let registry = ShellTaskRegistry::default();
        let live = registry.register("conversation-gone", "bash", "sleep 100", true);
        let kept = registry.register("conversation-kept", "bash", "sleep 100", true);
        assert!(!live.stop_requested());

        registry.forget_missing(["conversation-kept"]);

        // The row is gone, but the stop flag must already be raised for the
        // polling command to kill its process tree.
        assert!(registry.task_snapshots("conversation-gone").is_empty());
        assert!(
            live.stop_requested(),
            "被清扫的活命令必须收到停止信号，而不是变成孤儿"
        );
        // A live command in an unrelated conversation remains untouched.
        assert!(!kept.stop_requested());
        assert_eq!(registry.task_snapshots("conversation-kept").len(), 1);
        // Natural guard cleanup after process exit is a silent no-op for a
        // missing row.
        drop(live);
    }

    #[test]
    fn requesting_a_stop_raises_the_flag_the_running_command_polls() {
        let registry = ShellTaskRegistry::default();
        let guard = registry.register("conversation-a", "bash", "sleep 100", false);

        assert!(!guard.stop_requested());
        assert!(registry.request_stop("conversation-a", guard.shell_task_id()));
        assert!(guard.stop_requested());
        assert!(
            registry
                .task_snapshot("conversation-a", guard.shell_task_id())
                .expect("the task is still registered while it winds down")
                .stopping
        );
    }

    /// A stop is a per-conversation action. Accepting the id alone would let any conversation kill
    /// any other conversation's processes.
    #[test]
    fn a_stop_from_another_conversation_is_refused() {
        let registry = ShellTaskRegistry::default();
        let guard = registry.register("conversation-a", "bash", "sleep 100", false);

        assert!(!registry.request_stop("conversation-b", guard.shell_task_id()));
        assert!(!guard.stop_requested());
        assert!(registry
            .task_snapshot("conversation-b", guard.shell_task_id())
            .is_none());
    }

    #[test]
    fn stopping_a_conversation_reaches_all_of_its_commands_and_no_others() {
        let registry = ShellTaskRegistry::default();
        let first = registry.register("conversation-a", "bash", "sleep 100", false);
        let second = registry.register("conversation-a", "powershell", "Start-Sleep 100", false);
        let other = registry.register("conversation-b", "bash", "sleep 100", false);

        assert_eq!(registry.stop_conversation("conversation-a"), 2);
        assert!(first.stop_requested());
        assert!(second.stop_requested());
        assert!(!other.stop_requested());
    }

    #[test]
    fn stopping_an_unknown_task_is_a_refusal_rather_than_an_error() {
        let registry = ShellTaskRegistry::default();
        assert!(!registry.request_stop("conversation-a", "shell-404"));
        assert_eq!(registry.stop_conversation("conversation-a"), 0);
    }

    /// Ids must never collide: two commands started back to back are two rows and two stop buttons.
    #[test]
    fn every_registration_mints_a_distinct_ordered_id() {
        let registry = ShellTaskRegistry::default();
        let guards = (0..12)
            .map(|index| registry.register("conversation-a", "bash", &format!("echo {index}"), false))
            .collect::<Vec<_>>();

        let ids = guards
            .iter()
            .map(|guard| guard.shell_task_id().to_owned())
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(ids.len(), guards.len());
        // Ordering is numeric, so the tenth row does not sort between the first and the second.
        let snapshots = registry.task_snapshots("conversation-a");
        let order = snapshots
            .iter()
            .map(|snapshot| numeric_suffix(&snapshot.shell_task_id))
            .collect::<Vec<_>>();
        let mut sorted = order.clone();
        sorted.sort_unstable();
        assert_eq!(order, sorted);
        assert_eq!(order.first(), Some(&1));
        assert_eq!(order.last(), Some(&12));
    }

    /// A command is up to 64 KiB and may be a whole script. The row is a label.
    #[test]
    fn the_displayed_command_is_one_bounded_line() {        let registry = ShellTaskRegistry::default();
        let _multiline = registry.register("conversation-a", "bash", "set -e\n\nnpm  test\n", false);
        let _long = registry.register("conversation-a", "bash", &"x".repeat(5_000), false);

        let snapshots = registry.task_snapshots("conversation-a");
        assert_eq!(snapshots[0].command, "set -e npm test");
        assert_eq!(snapshots[1].command.chars().count(), MAX_DISPLAY_COMMAND + 1);
        assert!(snapshots[1].command.ends_with('…'));
    }

    /// The row shows how long the command has been running, so the start it counts from has to be a
    /// real instant the renderer can parse — not a placeholder that would render as a blank column.
    #[test]
    fn a_registered_command_reports_a_parseable_start_time() {
        let registry = ShellTaskRegistry::default();
        let before = Utc::now();
        let _guard = registry.register("conversation-a", "bash", "npm install", false);
        let after = Utc::now();

        let snapshot = registry.task_snapshots("conversation-a").remove(0);
        let started = chrono::DateTime::parse_from_rfc3339(&snapshot.started_at)
            .expect("the start time is RFC3339, which is what the renderer parses")
            .with_timezone(&Utc);
        // Inside the window the registration actually happened in: a stamp taken anywhere else —
        // at first render, at the first poll — would fall outside it.
        assert!(started >= before && started <= after);
    }

    /// The elapsed column must only ever count up. Re-stamping on each read would make a command
    /// that has run for a minute report a fresh zero every time the sidebar reconciled.
    #[test]
    fn the_start_time_is_stamped_once_and_never_moves() {
        let registry = ShellTaskRegistry::default();
        let guard = registry.register("conversation-a", "bash", "sleep 100", false);

        let first = registry.task_snapshots("conversation-a").remove(0).started_at;
        std::thread::sleep(std::time::Duration::from_millis(20));
        let later_list = registry.task_snapshots("conversation-a").remove(0).started_at;
        let later_single = registry
            .task_snapshot("conversation-a", guard.shell_task_id())
            .expect("still registered")
            .started_at;

        assert_eq!(first, later_list);
        // Both read paths report the same instant: the sidebar mixes them — pushes arrive through
        // one and the reconcile through the other — and a row must not jump between them.
        assert_eq!(first, later_single);
    }

    fn collecting_channel() -> (
        Channel<ShellOutputEvent>,
        Arc<Mutex<Vec<serde_json::Value>>>,
    ) {
        let received = Arc::new(Mutex::new(Vec::new()));
        let sink = received.clone();
        let channel = Channel::new(move |body| {
            let value = match body {
                tauri::ipc::InvokeResponseBody::Json(json) => serde_json::from_str(&json)?,
                tauri::ipc::InvokeResponseBody::Raw(bytes) => serde_json::to_value(bytes)?,
            };
            sink.lock().unwrap().push(value);
            Ok(())
        });
        (channel, received)
    }

    fn collecting_push_channel() -> (
        Channel<AppPushEvent>,
        Arc<Mutex<Vec<serde_json::Value>>>,
    ) {
        let received = Arc::new(Mutex::new(Vec::new()));
        let sink = received.clone();
        let channel = Channel::new(move |body| {
            let value = match body {
                tauri::ipc::InvokeResponseBody::Json(json) => serde_json::from_str(&json)?,
                tauri::ipc::InvokeResponseBody::Raw(bytes) => serde_json::to_value(bytes)?,
            };
            sink.lock().unwrap().push(value);
            Ok(())
        });
        (channel, received)
    }

    fn sink_with_code_page(
        registry: &ShellTaskRegistry,
        shell_task_id: &str,
        stream: ShellOutputStream,
        ansi_code_page: Option<u32>,
    ) -> ShellOutputSink {
        ShellOutputSink {
            registry: registry.clone(),
            shell_task_id: shell_task_id.to_owned(),
            stream,
            decoder: ConsoleTextDecoder::with_ansi_code_page(ansi_code_page),
        }
    }

    /// The buffer exists so a page opened halfway through a build still shows what it missed.
    #[test]
    fn output_written_before_anyone_watches_is_replayed_on_subscribe() {
        let registry = ShellTaskRegistry::default();
        let guard = registry.register("conversation-a", "bash", "npm run build", false);
        let mut stdout = guard.output_sink(ShellOutputStream::Stdout);
        stdout.append(b"compiling\n");
        stdout.append(b"done\n");

        let (channel, _received) = collecting_channel();
        let handle = registry
            .subscribe_output("conversation-a", guard.shell_task_id(), channel)
            .expect("the command is registered");

        assert_eq!(handle.snapshot, "compiling\ndone\n");
        assert!(handle.live);
        assert_eq!(handle.dropped_head_bytes, 0);
    }

    #[cfg(windows)]
    #[test]
    fn gbk_appended_through_a_sink_arrives_as_unicode_text() {
        let registry = ShellTaskRegistry::default();
        let guard = registry.register("conversation-a", "powershell", "Write-Output 中文", false);
        let mut stdout = sink_with_code_page(
            &registry,
            guard.shell_task_id(),
            ShellOutputStream::Stdout,
            Some(936),
        );
        stdout.append(&[0xD6, 0xD0, 0xCE, 0xC4, b'\n']);

        let (channel, _received) = collecting_channel();
        let handle = registry
            .subscribe_output("conversation-a", guard.shell_task_id(), channel)
            .expect("registered");
        assert_eq!(handle.snapshot, "中文\n");
    }

    #[test]
    fn a_sink_carries_a_split_utf8_character_between_appends() {
        let registry = ShellTaskRegistry::default();
        let guard = registry.register("conversation-a", "bash", "printf 中", false);
        let mut stdout = sink_with_code_page(
            &registry,
            guard.shell_task_id(),
            ShellOutputStream::Stdout,
            None,
        );
        stdout.append(&[0xE4, 0xB8]);
        stdout.append(&[0xAD]);

        let (channel, _received) = collecting_channel();
        let handle = registry
            .subscribe_output("conversation-a", guard.shell_task_id(), channel)
            .expect("registered");
        assert_eq!(handle.snapshot, "中");
    }

    #[test]
    fn finishing_a_sink_flushes_its_dangling_tail() {
        let registry = ShellTaskRegistry::default();
        let guard = registry.register("conversation-a", "bash", "printf partial", false);
        let mut stdout = sink_with_code_page(
            &registry,
            guard.shell_task_id(),
            ShellOutputStream::Stdout,
            None,
        );
        stdout.append(&[0xE4, 0xB8]);
        stdout.finish();

        let (channel, _received) = collecting_channel();
        let handle = registry
            .subscribe_output("conversation-a", guard.shell_task_id(), channel)
            .expect("registered");
        assert_eq!(handle.snapshot, "�");
    }

    /// Subscribing installs the sink, so everything printed afterwards arrives without a poll.
    #[test]
    fn output_written_after_subscribe_is_streamed_to_the_watcher() {
        let registry = ShellTaskRegistry::default();
        let guard = registry.register("conversation-a", "bash", "npm test", false);
        let (channel, received) = collecting_channel();
        registry
            .subscribe_output("conversation-a", guard.shell_task_id(), channel)
            .expect("the command is registered");

        guard
            .output_sink(ShellOutputStream::Stderr)
            .append(b"warning\n");

        let events = received.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["type"], "output");
        assert_eq!(events[0]["stream"], "stderr");
        assert_eq!(events[0]["seq"], 0);
        assert_eq!(events[0]["text"], "warning\n");
    }

    /// The two pipes are drained by independent threads, so the sequence has to be minted under
    /// the registry lock — otherwise two chunks could claim the same position.
    #[test]
    fn sequence_numbers_are_shared_across_both_streams() {
        let registry = ShellTaskRegistry::default();
        let guard = registry.register("conversation-a", "bash", "build", false);
        let (channel, received) = collecting_channel();
        registry
            .subscribe_output("conversation-a", guard.shell_task_id(), channel)
            .expect("the command is registered");

        guard.output_sink(ShellOutputStream::Stdout).append(b"a");
        guard.output_sink(ShellOutputStream::Stderr).append(b"b");
        guard.output_sink(ShellOutputStream::Stdout).append(b"c");

        let events = received.lock().unwrap();
        let seqs = events
            .iter()
            .map(|event| event["seq"].as_u64().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(seqs, vec![0, 1, 2]);
    }

    /// The page keeps the tail and says so. The model's copy of the same command keeps the head,
    /// because `MAX_TOOL_OUTPUT` truncates from the other end — the two deliberately disagree, and
    /// a page that hid that would be presenting a suffix as the whole output.
    #[test]
    fn the_buffer_keeps_the_tail_and_reports_what_it_dropped() {
        let registry = ShellTaskRegistry::default();
        let guard = registry.register("conversation-a", "bash", "cat huge", false);
        let mut stdout = guard.output_sink(ShellOutputStream::Stdout);
        stdout.append(&vec![b'x'; MAX_LIVE_OUTPUT]);
        stdout.append(b"TAIL");

        let (channel, _received) = collecting_channel();
        let handle = registry
            .subscribe_output("conversation-a", guard.shell_task_id(), channel)
            .expect("the command is registered");

        assert_eq!(handle.snapshot.len(), MAX_LIVE_OUTPUT);
        assert_eq!(handle.dropped_head_bytes, 4);
        assert!(handle.snapshot.ends_with("TAIL"));
    }

    /// A finished command has a transcript but no future. Installing a sink on it would leave a
    /// channel alive that nothing can ever write to.
    #[test]
    fn a_finished_command_replays_without_going_live() {
        let registry = ShellTaskRegistry::default();
        let shell_task_id = {
            let mut guard = registry.register("conversation-a", "bash", "ls", false);
            guard.output_sink(ShellOutputStream::Stdout).append(b"file\n");
            guard.finish(ShellTaskOutcome::Succeeded, Some(0));
            guard.shell_task_id().to_owned()
        };

        let (channel, received) = collecting_channel();
        let handle = registry
            .subscribe_output("conversation-a", &shell_task_id, channel)
            .expect("a finished row is still in the registry");

        assert_eq!(handle.snapshot, "file\n");
        assert!(!handle.live);
        // The command ended before anyone subscribed, so the terminal event belonged to a watcher
        // that did not exist yet; the snapshot is the whole story.
        assert!(received.lock().unwrap().is_empty());
    }

    /// A watcher present at the end gets told how it ended, so the page can stop its spinner
    /// without polling the task list.
    #[test]
    fn a_watcher_is_told_how_the_command_ended() {
        let registry = ShellTaskRegistry::default();
        let mut guard = registry.register("conversation-a", "bash", "false", false);
        let (channel, received) = collecting_channel();
        registry
            .subscribe_output("conversation-a", guard.shell_task_id(), channel)
            .expect("the command is registered");

        guard.finish(ShellTaskOutcome::Failed, Some(1));
        drop(guard);

        let events = received.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["type"], "end");
        assert_eq!(events[0]["outcome"], "failed");
        assert_eq!(events[0]["exitCode"], 1);
    }

    /// Watching is a per-conversation action for the same reason stopping is: one conversation
    /// may never read another's processes.
    #[test]
    fn another_conversation_cannot_watch_this_ones_output() {
        let registry = ShellTaskRegistry::default();
        let guard = registry.register("conversation-a", "bash", "secret", false);
        guard.output_sink(ShellOutputStream::Stdout).append(b"token");

        let (channel, _received) = collecting_channel();
        assert!(registry
            .subscribe_output("conversation-b", guard.shell_task_id(), channel)
            .is_none());
        assert!(!registry.detach_output("conversation-b", guard.shell_task_id(), None));
    }

    /// Detaching stops delivery. The page that navigated away must not keep a dead channel
    /// receiving a build's whole output.
    #[test]
    fn detaching_stops_delivery() {
        let registry = ShellTaskRegistry::default();
        let guard = registry.register("conversation-a", "bash", "build", false);
        let (channel, received) = collecting_channel();
        registry
            .subscribe_output("conversation-a", guard.shell_task_id(), channel)
            .expect("the command is registered");

        assert!(registry.detach_output("conversation-a", guard.shell_task_id(), None));
        guard.output_sink(ShellOutputStream::Stdout).append(b"more");

        assert!(received.lock().unwrap().is_empty());
        // Detaching only ends delivery; the buffer is still what the next page will replay.
        let (channel, _) = collecting_channel();
        let handle = registry
            .subscribe_output("conversation-a", guard.shell_task_id(), channel)
            .expect("still registered");
        assert_eq!(handle.snapshot, "more");
    }

    /// A page torn down and rebuilt in one breath subscribes, detaches, and subscribes again,
    /// and the host may see the detach last. Only the sink the detaching subscription installed
    /// goes; the newer subscriber keeps receiving.
    #[test]
    fn a_stale_detach_leaves_the_newer_subscriber_attached() {
        let registry = ShellTaskRegistry::default();
        let guard = registry.register("conversation-a", "bash", "build", false);
        let (first, first_received) = collecting_channel();
        let first_handle = registry
            .subscribe_output("conversation-a", guard.shell_task_id(), first)
            .expect("the command is registered");
        let (second, second_received) = collecting_channel();
        let second_handle = registry
            .subscribe_output("conversation-a", guard.shell_task_id(), second)
            .expect("the command is registered");
        assert_ne!(first_handle.subscription_id, second_handle.subscription_id);

        assert!(registry.detach_output(
            "conversation-a",
            guard.shell_task_id(),
            Some(first_handle.subscription_id)
        ));
        guard.output_sink(ShellOutputStream::Stdout).append(b"still watching");

        assert!(first_received.lock().unwrap().is_empty());
        assert_eq!(second_received.lock().unwrap().len(), 1);

        // The newer subscriber's own detach is the one that ends delivery.
        assert!(registry.detach_output(
            "conversation-a",
            guard.shell_task_id(),
            Some(second_handle.subscription_id)
        ));
        guard.output_sink(ShellOutputStream::Stdout).append(b"nobody");
        assert_eq!(second_received.lock().unwrap().len(), 1);
    }

    /// A forked conversation inherits the parent's finished rows: same command, same outcome, same
    /// transcript, under its own ids. The parent's rows are the record of what really ran and must
    /// come through the copy untouched.
    #[test]
    fn a_fork_inherits_finished_rows_under_fresh_ids_with_their_output() {
        let registry = ShellTaskRegistry::default();
        let first_id = {
            let mut guard = registry.register("conversation-a", "bash", "npm test", false);
            guard.output_sink(ShellOutputStream::Stdout).append(b"3 passed\n");
            guard.finish(ShellTaskOutcome::Failed, Some(1));
            guard.shell_task_id().to_owned()
        };
        let second_id = {
            let mut guard = registry.register("conversation-a", "powershell", "Get-Date", true);
            guard.finish(ShellTaskOutcome::Succeeded, Some(0));
            guard.shell_task_id().to_owned()
        };
        // Attached after the source rows ended, so only the clones' events are collected.
        let hub = AppEventHub::default();
        let (channel, received) = collecting_push_channel();
        hub.subscribe(channel);
        registry.attach_events(hub);

        let clones = registry.clone_finished_rows("conversation-a", "conversation-child");

        assert_eq!(clones.len(), 2);
        // Ascending source order, so both sidebars read the same way.
        assert_eq!(clones[0].command, "npm test");
        assert_eq!(clones[1].command, "Get-Date");
        assert_eq!(clones[0].tool_name, "bash");
        assert_eq!(clones[0].conversation_id, "conversation-child");
        assert_ne!(clones[0].shell_task_id, first_id);
        assert_ne!(clones[1].shell_task_id, second_id);
        assert_eq!(clones[0].outcome, Some(ShellTaskOutcome::Failed));
        assert_eq!(clones[0].exit_code, Some(1));
        assert!(clones[1].background);

        let source = registry
            .task_snapshot("conversation-a", &first_id)
            .expect("the source row is retained");
        assert_eq!(clones[0].started_at, source.started_at);
        assert_eq!(clones[0].ended_at, source.ended_at);

        let (channel, _received) = collecting_channel();
        let handle = registry
            .subscribe_output("conversation-child", &clones[0].shell_task_id, channel)
            .expect("the clone is registered");
        assert_eq!(handle.snapshot, "3 passed\n");
        assert!(!handle.live);

        // The source keeps exactly its own two rows, still owned by its own conversation.
        let sources = registry.task_snapshots("conversation-a");
        assert_eq!(sources.len(), 2);
        assert_eq!(sources[0].shell_task_id, first_id);
        assert_eq!(sources[0].conversation_id, "conversation-a");

        // The renderer paints an inherited row from the terminal event, like any other finished one.
        let events = received.lock().unwrap();
        let ended = events
            .iter()
            .filter(|event| event["type"] == "shellTaskEnded")
            .collect::<Vec<_>>();
        assert_eq!(ended.len(), 2);
        assert_eq!(ended[0]["task"]["conversationId"], "conversation-child");
        assert_eq!(ended[0]["task"]["command"], "npm test");
    }

    /// A running command is one process with one stop button. Copying its row would give the child
    /// a stop for something it never started, so only finished rows are inherited.
    #[test]
    fn a_fork_does_not_inherit_a_running_row() {
        let registry = ShellTaskRegistry::default();
        let running = registry.register("conversation-a", "bash", "sleep 100", true);
        let mut finished = registry.register("conversation-a", "bash", "echo ok", false);
        finished.finish(ShellTaskOutcome::Succeeded, Some(0));
        drop(finished);

        let clones = registry.clone_finished_rows("conversation-a", "conversation-child");

        assert_eq!(clones.len(), 1);
        assert_eq!(clones[0].command, "echo ok");
        assert!(registry
            .task_snapshots("conversation-child")
            .iter()
            .all(|snapshot| snapshot.command != "sleep 100"));
        assert!(!running.stop_requested());
    }

    /// Cloning twice mints two rows rather than colliding on the ids of the first copy.
    #[test]
    fn cloning_into_one_conversation_twice_mints_distinct_ids() {
        let registry = ShellTaskRegistry::default();
        {
            let mut guard = registry.register("conversation-a", "bash", "echo ok", false);
            guard.finish(ShellTaskOutcome::Succeeded, Some(0));
        }

        let first = registry.clone_finished_rows("conversation-a", "conversation-child");
        let second = registry.clone_finished_rows("conversation-a", "conversation-child");

        assert_eq!(first.len(), 1);
        assert_eq!(second.len(), 1);
        assert_ne!(first[0].shell_task_id, second[0].shell_task_id);
        assert_eq!(registry.task_snapshots("conversation-child").len(), 2);
    }
}
