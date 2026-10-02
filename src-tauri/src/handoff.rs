//! Automatic handoff: what the composer's "auto-compact" switch does.
//!
//! Nothing is summarized on the model's behalf. When a top-level conversation's
//! context crosses the threshold the user set, the run loop *arms* it: the host
//! asks the model to hand off — an instruction, so by the carrier the model has
//! for one (`host_append::instruction_carrier`): an appended system prompt
//! where the model and endpoint take a system message mid-conversation, and
//! otherwise the `box` delivery that carries a background task's result
//! between rounds — and from that step on it offers four tools it derives for
//! itself, as it derives the plan tools from the security level —
//!
//! - `create_handoff_note`, `edit_handoff_note` and `read_handoff_note`, the
//!   memory tools' shape pointed at a notebook of the conversation's own under
//!   the app data directory (`handoffs/<conversation id>/`), where no file tool
//!   reaches and no other conversation writes;
//! - `handoff`, which takes no arguments. It opens the continuation — a new
//!   conversation, not a fork of this one — starts its first run, and ends
//!   this one.
//!
//! The continuation inherits the conversation's system prompt and nothing of its
//! history: the system prompt the user gave it (its timeline's first context,
//! when that is a system card of the user's — usually the preset's; one
//! anywhere else counts for nothing), the settings — hence the same tools and
//! the same host-assembled system prompt — and a copy of the notebook. It inherits
//! none of the cache state: the settings arrive without the tool lock, since
//! the cache that lock describes rode on the history left behind, and the
//! continuation's first request writes a lock of its own. Its timeline
//! is `[that card, the notebook index, the host's opening message]` on a model
//! that reads its tools ahead of its system prompt
//! (`host_append::tools_precede_system`): the index is a system card, so it
//! reaches the model as the system prompt's very last section, right after the
//! conversation's own prompt (`aisdk::step::system_prompt_parts`), and the
//! tools and host sections before it stay the prefix this continuation shares
//! with any other conversation of the same setup. On any other model that end
//! comes before the tools, so the timeline is `[that card, the opening
//! message]` and the first run hands the index over at its first round
//! boundary instead, behind the opening message — as an instruction, by the
//! carrier the model has for one (`host_append::instruction_carrier`). Either
//! way the index is frozen once given: later writes to the notebook never move
//! what the continuation was told. The
//! opening message is the prompt profile's text, so what the continuation is
//! told to do first is the user's to reword. A conversation with notes is offered
//! `read_handoff_note` from its first request on, and gains the other three once
//! it is armed itself; its notebook starts as the copy, so a chain of handoffs
//! keeps editing one set of notes rather than starting over.
//!
//! The continuation is titled `<origin title>-handover-<number>`. A
//! continuation's own handoff counts under the same origin, so a chain reads
//! `-handover-1`, `-handover-2`, … rather than stacking suffixes. It takes the
//! source's place in the conversation tree instead of hanging beneath it.
//!
//! The source is left exactly as it was. Its timeline ends on the `handoff`
//! card, which names the continuation.

use std::{
    fs,
    path::{Path, PathBuf},
};

use chrono::Utc;

use crate::{
    api::{failed_tool_execution, ToolCall, ToolExecution},
    memory_archive_file::{read_bounded_nofollow_labeled, write_all_nofollow_labeled},
    model::{
        ContextItem, Conversation, ConversationHandoffOrigin, ConversationSettings, JsonObject,
        RunModelRequest, ToolResult,
    },
    prompt_profile::{PromptKey, PromptProfile},
    state::AppState,
};

/// Lowest threshold the setting accepts, in percent of the context window.
pub const MIN_THRESHOLD_PERCENT: u32 = 20;
/// Highest threshold the setting accepts, in percent of the context window.
pub const MAX_THRESHOLD_PERCENT: u32 = 97;
/// The threshold a fresh install starts with.
pub const DEFAULT_THRESHOLD_PERCENT: u32 = 80;

/// The stop reason a run ends with once its continuation exists.
pub const STOP_REASON: &str = "handed_off";

pub(crate) const READ_NOTE_TOOL: &str = "read_handoff_note";
pub(crate) const CREATE_NOTE_TOOL: &str = "create_handoff_note";
pub(crate) const EDIT_NOTE_TOOL: &str = "edit_handoff_note";
pub(crate) const HANDOFF_TOOL: &str = "handoff";

/// The four tools this module owns.
pub(crate) const TOOL_NAMES: [&str; 4] =
    [READ_NOTE_TOOL, CREATE_NOTE_TOOL, EDIT_NOTE_TOOL, HANDOFF_TOOL];
/// What an armed step offers: the four. `box`, the carrier the notice came in,
/// is not among them — every run declares it from its first request, so the
/// notice's fabricated call names a tool the conversation already had.
const ARMED_TOOLS: [&str; 4] = [READ_NOTE_TOOL, CREATE_NOTE_TOOL, EDIT_NOTE_TOOL, HANDOFF_TOOL];
const READ_ONLY_TOOLS: [&str; 1] = [READ_NOTE_TOOL];

/// The instruction that arms a conversation: its topic as an appended system
/// prompt, and its `kind` as a host notice in `box`. Its presence in the
/// timeline, in either carrier, *is* the armed state, so it outlives the run
/// that delivered it.
pub(crate) const ARMED_TOPIC: &str = "handoff";
pub(crate) const NOTICE_KIND: &str = "handoff";
/// The notebook index a continuation starts with, as a system card the system
/// prompt puts last.
pub(crate) const INDEX_CONTEXT_ID: &str = "ctx_handoff_index";
/// The index handed over at the first round boundary instead: its topic as an
/// appended system prompt, and its `kind` and fixed card id in `box`.
pub(crate) const INDEX_TOPIC: &str = "handoff-index";
pub(crate) const INDEX_NOTICE_KIND: &str = "handoff_index";
pub(crate) const INDEX_NOTICE_ID: &str = "ctx_agent-result_handoff_index";
/// The context kind of a continuation's opening message, whose presence makes
/// a timeline a continuation's.
const START_CONTEXT_KIND: &str = "handoff-start";

/// Directory under the app data directory that holds every notebook.
const NOTEBOOKS_DIR: &str = "handoffs";
/// The host-owned index inside each notebook.
const INDEX_NAME: &str = "HANDOFF.md";
const NOTE_NOUN: &str = "Handoff note";
/// Largest single note, as for a memory document.
const MAX_NOTE_BYTES: usize = 256 * 1024;
/// Largest index. It is pure pointers, so it stays small.
const MAX_INDEX_BYTES: usize = 64 * 1024;
const MAX_DESCRIPTION_CHARS: usize = 300;
/// Skipped calls after a successful `handoff` in the same round get this.
pub(crate) const SKIPPED_AFTER_HANDOFF: &str =
    "Not run: the conversation was handed off earlier in this round.";

/// True for the four names this module derives.
pub(crate) fn is_handoff_tool_name(name: &str) -> bool {
    TOOL_NAMES.contains(&name)
}

/// Input tokens at which a conversation arms: `percent` of the window, rounded
/// down. The percent is clamped to the range the setting offers, so a
/// hand-edited document cannot arm on every request or never.
pub fn threshold_tokens(context_window: u64, percent: u32) -> u64 {
    let percent = percent.clamp(MIN_THRESHOLD_PERCENT, MAX_THRESHOLD_PERCENT) as u128;
    (context_window as u128 * percent / 100) as u64
}

/// A conversation's notebook directory. Holding one means the host chose the
/// directory from the conversation id; tools only ever name notes inside it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notebook {
    dir: PathBuf,
}

/// One line of a notebook's index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexEntry {
    pub name: String,
    pub description: String,
}

impl Notebook {
    /// `<app_data>/handoffs/<conversation_id>`, or `None` for an id that is not
    /// a plain directory name — a conversation id is minted by the renderer, and
    /// one that could name a path is refused rather than sanitized.
    pub fn for_conversation(app_data: &Path, conversation_id: &str) -> Option<Self> {
        let plain = !conversation_id.is_empty()
            && conversation_id.len() <= 128
            && conversation_id
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'));
        (plain && !app_data.as_os_str().is_empty()).then(|| Self {
            dir: app_data.join(NOTEBOOKS_DIR).join(conversation_id),
        })
    }

    fn index_path(&self) -> PathBuf {
        self.dir.join(INDEX_NAME)
    }

    fn note_path(&self, validated_name: &str) -> PathBuf {
        self.dir.join(validated_name)
    }

    /// The index as the host last wrote it, without entries whose note is gone.
    /// A missing or unreadable index is an empty notebook.
    pub fn entries(&self) -> Vec<IndexEntry> {
        let Ok(bytes) = read_bounded_nofollow_labeled(&self.index_path(), MAX_INDEX_BYTES, "交接索引")
        else {
            return Vec::new();
        };
        let Ok(text) = String::from_utf8(bytes) else {
            return Vec::new();
        };
        let mut entries = parse_index(&text);
        entries.retain(|entry| fs::symlink_metadata(self.note_path(&entry.name)).is_ok());
        entries
    }

    pub fn read(&self, raw_name: &str) -> Result<(String, String), String> {
        let name = normalize_note_name(raw_name)?;
        let bytes = read_bounded_nofollow_labeled(&self.note_path(&name), MAX_NOTE_BYTES, "交接文档")
            .map_err(|_| format!("No handoff note named {name} exists"))?;
        let content = String::from_utf8(bytes)
            .map_err(|_| format!("Handoff note {name} is not valid UTF-8 text"))?;
        Ok((name, content))
    }

    /// Creates a note and records its index line. Refuses to overwrite: an
    /// accidental re-create must not silently discard a note.
    pub fn create(&self, raw_name: &str, content: &str, raw_description: &str) -> Result<String, String> {
        let name = normalize_note_name(raw_name)?;
        let description = normalize_description(raw_description)?;
        validate_content(content)?;
        let path = self.note_path(&name);
        if fs::symlink_metadata(&path).is_ok() {
            return Err(format!(
                "{name} already exists; change it with edit_handoff_note or choose another name"
            ));
        }
        self.ensure_dir()?;
        write_all_nofollow_labeled(&path, content.as_bytes(), MAX_NOTE_BYTES, "交接文档")
            .map_err(|_| format!("Could not write handoff note {name}"))?;
        // A note the index does not list is invisible to the continuation, so
        // an index failure un-creates the note instead of orphaning it.
        if let Err(error) = self.upsert_entry(&name, &description) {
            let _ = fs::remove_file(&path);
            return Err(error);
        }
        Ok(name)
    }

    /// Replaces one exact, unique passage of a note and refreshes its index line.
    pub fn edit(
        &self,
        raw_name: &str,
        old_text: &str,
        new_text: &str,
        raw_description: &str,
    ) -> Result<String, String> {
        let name = normalize_note_name(raw_name)?;
        let description = normalize_description(raw_description)?;
        if old_text.is_empty() {
            return Err("Text to replace must not be empty; use create_handoff_note for a new note".into());
        }
        if old_text == new_text {
            return Err("The old and replacement text are identical; there is no change to write".into());
        }
        let (_, existing) = self.read(&name)?;
        let occurrences = existing.matches(old_text).count();
        if occurrences == 0 {
            return Err(format!("Could not find the text to replace in handoff note {name}"));
        }
        if occurrences > 1 {
            return Err(format!(
                "The text to replace occurs {occurrences} times in handoff note {name}; provide a longer unique match"
            ));
        }
        let content = existing.replacen(old_text, new_text, 1);
        validate_content(&content)?;
        // Index first: on failure the note keeps its old body and old line.
        self.upsert_entry(&name, &description)?;
        write_all_nofollow_labeled(&self.note_path(&name), content.as_bytes(), MAX_NOTE_BYTES, "交接文档")
            .map_err(|_| format!("Could not write handoff note {name}"))?;
        Ok(name)
    }

    /// Copies every indexed note, and the index, into `target`.
    pub fn copy_into(&self, target: &Notebook) -> Result<(), String> {
        let entries = self.entries();
        if entries.is_empty() {
            return Ok(());
        }
        target.ensure_dir()?;
        for entry in &entries {
            let (_, content) = self.read(&entry.name)?;
            write_all_nofollow_labeled(
                &target.note_path(&entry.name),
                content.as_bytes(),
                MAX_NOTE_BYTES,
                "交接文档",
            )
            .map_err(|_| format!("Could not copy handoff note {}", entry.name))?;
        }
        target.write_index(&entries)
    }

    /// Removes the notebook with its conversation. Best effort: a notebook
    /// nothing points at is only disk space.
    pub fn remove(&self) {
        if fs::symlink_metadata(&self.dir).is_ok_and(|metadata| metadata.is_dir()) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    fn upsert_entry(&self, name: &str, description: &str) -> Result<(), String> {
        let mut entries = self.entries();
        // Case-insensitively: on the filesystems that fold case, `State.md`
        // opened the note listed as `state.md`, and the index keeps one line.
        match entries
            .iter_mut()
            .find(|entry| entry.name.eq_ignore_ascii_case(name))
        {
            Some(existing) => existing.description = description.to_owned(),
            None => entries.push(IndexEntry {
                name: name.to_owned(),
                description: description.to_owned(),
            }),
        }
        self.write_index(&entries)
    }

    fn write_index(&self, entries: &[IndexEntry]) -> Result<(), String> {
        let rendered = render_index(entries);
        if rendered.len() > MAX_INDEX_BYTES {
            return Err(format!(
                "The handoff index would exceed {MAX_INDEX_BYTES} bytes; merge some notes into fewer, longer ones"
            ));
        }
        self.ensure_dir()?;
        write_all_nofollow_labeled(&self.index_path(), rendered.as_bytes(), MAX_INDEX_BYTES, "交接索引")
            .map_err(|_| "Could not write the handoff index".to_owned())
    }

    fn ensure_dir(&self) -> Result<(), String> {
        if let Ok(metadata) = fs::symlink_metadata(&self.dir) {
            if !metadata.is_dir() {
                return Err("The handoff notebook is occupied by a file or link".into());
            }
        }
        fs::create_dir_all(&self.dir).map_err(|_| "Could not create the handoff notebook".to_owned())
    }
}

/// Removes a deleted conversation's notebook.
pub(crate) fn remove_notebook(app_data: &Path, conversation_id: &str) {
    if let Some(notebook) = Notebook::for_conversation(app_data, conversation_id) {
        notebook.remove();
    }
}

fn normalize_note_name(raw: &str) -> Result<String, String> {
    let name = crate::mework_memory::normalize_named_document(raw, NOTE_NOUN)?;
    if name.eq_ignore_ascii_case(INDEX_NAME) {
        return Err(format!(
            "{INDEX_NAME} is the host-managed handoff index; give each note a description instead"
        ));
    }
    Ok(name)
}

fn normalize_description(raw: &str) -> Result<String, String> {
    let collapsed = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return Err("The index description must not be empty; say in one sentence what the note holds".into());
    }
    if collapsed.chars().count() > MAX_DESCRIPTION_CHARS {
        return Err(format!(
            "The index description must not exceed {MAX_DESCRIPTION_CHARS} characters; put the detail in the note itself"
        ));
    }
    Ok(collapsed)
}

fn validate_content(content: &str) -> Result<(), String> {
    if content.trim().is_empty() {
        return Err("A handoff note must not be empty".into());
    }
    if content.len() > MAX_NOTE_BYTES {
        return Err(format!(
            "A handoff note must not exceed {MAX_NOTE_BYTES} bytes; split it into several notes"
        ));
    }
    if content.contains('\0') {
        return Err("A handoff note must not contain a NUL character".into());
    }
    Ok(())
}

/// Parses the `- [name](name) — description` lines the host writes. A line in
/// any other shape is skipped, so a hand-edited index degrades gracefully.
fn parse_index(text: &str) -> Vec<IndexEntry> {
    let mut entries: Vec<IndexEntry> = Vec::new();
    for line in text.lines() {
        let Some(rest) = line.trim().strip_prefix("- [") else {
            continue;
        };
        let Some((label, rest)) = rest.split_once("](") else {
            continue;
        };
        let Some((_, rest)) = rest.split_once(')') else {
            continue;
        };
        let Ok(name) = normalize_note_name(label) else {
            continue;
        };
        if entries.iter().any(|entry| entry.name.eq_ignore_ascii_case(&name)) {
            continue;
        }
        let description = rest
            .trim_start()
            .trim_start_matches('—')
            .trim()
            .to_owned();
        entries.push(IndexEntry { name, description });
    }
    entries
}

fn render_index(entries: &[IndexEntry]) -> String {
    let mut out = String::from("# Handoff notes\n\n");
    for entry in entries {
        out.push_str(&format!("- [{0}]({0}) — {1}\n", entry.name, entry.description));
    }
    out
}

/// What a run knows about its conversation's handoff, resolved once when the
/// run starts and moved by the run itself: the boundary check arms it, a note
/// written gives it notes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HandoffRun {
    notebook: Option<Notebook>,
    armed: bool,
    has_notes: bool,
}

impl HandoffRun {
    /// A top-level conversation run with an app data directory has a notebook;
    /// a child agent, a schema-bound run or a run outside the app has none, and
    /// therefore never any of the tools.
    pub(crate) fn resolve(request: &RunModelRequest) -> Self {
        if request.subagent_depth > 0 || request.output_schema.is_some() {
            return Self::default();
        }
        let Some(notebook) =
            Notebook::for_conversation(Path::new(&request.app_data_path), &request.conversation_id)
        else {
            return Self::default();
        };
        let armed = request.contexts.iter().any(|context| {
            // A card that calls one of the tools needs them declared to
            // replay, whatever became of the instruction (a timeline edit, a
            // branch that leaves deliveries behind).
            is_arming_instruction(context)
                || matches!(context, ContextItem::Tool { tool_name, .. } if is_handoff_tool_name(tool_name))
        });
        let has_notes = !notebook.entries().is_empty();
        Self {
            notebook: Some(notebook),
            armed,
            has_notes,
        }
    }

    pub(crate) fn notebook(&self) -> Option<&Notebook> {
        self.notebook.as_ref()
    }

    /// Whether this run may still arm: it has a notebook and is not armed yet.
    pub(crate) fn can_arm(&self) -> bool {
        self.notebook.is_some() && !self.armed
    }

    pub(crate) fn arm(&mut self) {
        self.armed = true;
    }

    pub(crate) fn note_written(&mut self) {
        self.has_notes = true;
    }

    /// The tools this step offers. The set only ever grows over a
    /// conversation's life: notes are never deleted and the notice stays.
    pub(crate) fn derived_tools(&self) -> &'static [&'static str] {
        if self.notebook.is_none() {
            &[]
        } else if self.armed {
            &ARMED_TOOLS
        } else if self.has_notes {
            &READ_ONLY_TOOLS
        } else {
            &[]
        }
    }
}

/// The tools a step of `request` offers. A child agent's request is cloned from
/// its parent's and so carries the parent's handoff state; it never offers any.
pub(crate) fn derived_tools(request: &RunModelRequest) -> &'static [&'static str] {
    if request.subagent_depth > 0 {
        return &[];
    }
    request.handoff.derived_tools()
}

/// Whether `context` is the instruction that armed the conversation, in either
/// carrier.
fn is_arming_instruction(context: &ContextItem) -> bool {
    crate::system_append::topic(context) == Some(ARMED_TOPIC)
        || crate::wire_history::host_notice_kind(context) == Some(NOTICE_KIND)
}

/// The arming instruction, the run's prompt profile's text. It is the whole
/// message in either carrier: its `box` form has no `<summary>`.
pub(crate) fn notice(profile: &PromptProfile) -> crate::model::HostNotice {
    crate::model::HostNotice {
        kind: NOTICE_KIND,
        summary: String::new(),
        body: profile.text(PromptKey::HandoffArmedNotice).to_owned(),
        id: None,
    }
}

/// The notebook index as the model reads it.
fn index_text(profile: &PromptProfile, entries: &[IndexEntry]) -> String {
    let notes = entries
        .iter()
        .map(|entry| format!("- {} — {}", entry.name, entry.description))
        .collect::<Vec<_>>()
        .join("\n");
    profile.render(PromptKey::HandoffIndexContext, &[("notes", &notes)])
}

/// The notebook index a continuation is still owed: `None` unless the timeline
/// is a continuation's (it holds the opening message) that has not been given
/// the index yet — in its system prompt, as an appended system prompt or in
/// `box`. A child agent never is. `delivered` is what the run has added so far,
/// which may not have reached `request.contexts` yet.
pub(crate) fn owed_index<'a>(
    request: &RunModelRequest,
    delivered: impl IntoIterator<Item = &'a ContextItem>,
) -> Option<String> {
    if request.subagent_depth > 0 {
        return None;
    }
    let notebook = request.handoff.notebook()?;
    let opening = format!("ctx_{START_CONTEXT_KIND}_");
    let continuation = request.contexts.iter().any(|context| {
        matches!(context, ContextItem::User { id, .. } if id.starts_with(&opening))
    });
    let tells = |context: &ContextItem| {
        matches!(context.id(), INDEX_CONTEXT_ID | INDEX_NOTICE_ID)
            || crate::system_append::topic(context) == Some(INDEX_TOPIC)
    };
    if !continuation || request.contexts.iter().any(tells) || delivered.into_iter().any(tells) {
        return None;
    }
    let entries = notebook.entries();
    (!entries.is_empty()).then(|| index_text(&request.prompt_profile, &entries))
}

/// The continuation's timeline: the conversation's own system prompt — kept
/// first, which is what makes it one — the notebook index when it goes in the
/// system prompt (`index_in_system_prompt`), and the opening message its first
/// run is anchored on. A source whose timeline does not open on a system
/// prompt of the user's passes none on.
pub(crate) fn child_contexts(
    profile: &PromptProfile,
    source: &[ContextItem],
    entries: &[IndexEntry],
    index_in_system_prompt: bool,
) -> Vec<ContextItem> {
    let now = Utc::now().to_rfc3339();
    let mut contexts = Vec::with_capacity(3);
    if let Some(prompt) = crate::aisdk::step::conversation_system_prompt(source) {
        contexts.push(ContextItem::System {
            id: crate::api::new_context_id("handoff-system"),
            content: prompt.to_owned(),
            local_only: false,
            hook_execution: None,
            tools_added: Vec::new(),
            created_at: now.clone(),
        });
    }
    if index_in_system_prompt {
        contexts.push(ContextItem::System {
            id: INDEX_CONTEXT_ID.to_owned(),
            content: index_text(profile, entries),
            local_only: false,
            hook_execution: None,
            tools_added: Vec::new(),
            created_at: now.clone(),
        });
    }
    contexts.push(ContextItem::User {
        id: crate::api::new_context_id(START_CONTEXT_KIND),
        content: profile.text(PromptKey::HandoffStartMessage).to_owned(),
        images: Vec::new(),
        files: Vec::new(),
        created_at: now,
    });
    contexts
}

/// Runs one of the four tools. `running_tasks` counts the agents and workflows
/// still working for this conversation: their results come back here, so a
/// handoff waits for them.
pub(crate) fn run_tool(
    request: &RunModelRequest,
    call: ToolCall,
    state: &AppState,
    running_tasks: usize,
) -> ToolExecution {
    let Some(notebook) = request.handoff.notebook() else {
        return failed_tool_execution(
            call,
            "Handoff is only available to a top-level conversation run".into(),
        );
    };
    let profile = &request.prompt_profile;
    let outcome = match call.name.as_str() {
        READ_NOTE_TOOL => notebook
            .read(&text(&call.input, "name"))
            .map(|(_, content)| content),
        CREATE_NOTE_TOOL => notebook
            .create(
                &text(&call.input, "name"),
                &text(&call.input, "content"),
                &text(&call.input, "description"),
            )
            .map(|name| profile.render(PromptKey::HandoffNoteCreated, &[("name", &name)])),
        EDIT_NOTE_TOOL => notebook
            .edit(
                &text(&call.input, "name"),
                &text(&call.input, "old_text"),
                &text(&call.input, "new_text"),
                &text(&call.input, "description"),
            )
            .map(|name| profile.render(PromptKey::HandoffNoteUpdated, &[("name", &name)])),
        HANDOFF_TOOL => hand_off(request, notebook, state, running_tasks)
            .map(|title| profile.render(PromptKey::HandoffCompleted, &[("title", &title)])),
        other => Err(format!("Unknown handoff tool: {other}")),
    };
    match outcome {
        Ok(output) => ToolExecution {
            call,
            result: ToolResult {
                success: true,
                output,
                images: Vec::new(),
                diff: None,
                executed_at: Utc::now().to_rfc3339(),
                duration_ms: 0,
            },
            subagent: None,
        },
        Err(error) => failed_tool_execution(call, error),
    }
}

fn text(input: &JsonObject, field: &str) -> String {
    input
        .get(field)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// Opens the continuation and returns its title.
fn hand_off(
    request: &RunModelRequest,
    notebook: &Notebook,
    state: &AppState,
    running_tasks: usize,
) -> Result<String, String> {
    let entries = notebook.entries();
    if entries.is_empty() {
        return Err("Write the handoff first: the next conversation starts from your notes and nothing else. Record the task, what is done, what is left and where you stopped with create_handoff_note, then call handoff again.".into());
    }
    if running_tasks > 0 {
        return Err(format!(
            "{running_tasks} background agent or workflow task(s) are still running, and their results come back to this conversation, which stops at the handoff. Wait for them with task_wait (or stop them), record what they found, then call handoff again."
        ));
    }
    let app_data = Path::new(&request.app_data_path);
    let anchor = app_data.join("document.v1.json");
    let child_id = format!("conv_{}", uuid::Uuid::new_v4());
    let child_notebook = Notebook::for_conversation(app_data, &child_id)
        .ok_or_else(|| "Could not place the continuation's notebook".to_owned())?;
    notebook.copy_into(&child_notebook)?;
    let contexts = child_contexts(
        &request.prompt_profile,
        &request.contexts,
        &entries,
        crate::host_append::tools_precede_system(&request.model.id),
    );
    let child = match create_child(
        state,
        &anchor,
        &request.workspace_id,
        &request.conversation_id,
        child_id,
        contexts,
    ) {
        Ok(child) => child,
        Err(error) => {
            child_notebook.remove();
            return Err(format!("Could not open the continuation: {error}"));
        }
    };
    state
        .push_events
        .publish(crate::push_events::AppPushEvent::ConversationHandedOff {
            workspace_id: request.workspace_id.clone(),
            source_conversation_id: request.conversation_id.clone(),
            child_conversation_id: child.id.clone(),
        });
    Ok(child.title)
}

/// A continuation's title. Not localized: the suffix is part of the name, and a
/// language switch must not rename anything.
fn handover_title(origin_title: &str, number: u32) -> String {
    format!("{origin_title}-handover-{number}")
}

/// Which conversation a new continuation of `source` counts under, the title it
/// is named after, and the number it takes, among `conversations` — every
/// conversation the document lists. A continuation of a continuation counts
/// under the same origin, and the numbering is keyed by the origin's id, so two
/// conversations that happen to share a title number theirs separately. Once
/// the origin is gone, the title the source still carries, less its suffix, is
/// all that is left of the name.
fn handover_naming<'a>(
    conversations: impl Iterator<Item = &'a Conversation> + Clone,
    source: &Conversation,
) -> (String, String, u32) {
    let (origin_id, origin_title) = match &source.handoff_of {
        None => (source.id.clone(), source.title.clone()),
        Some(origin) => match conversations
            .clone()
            .find(|candidate| candidate.id == origin.conversation_id)
        {
            Some(found) => (found.id.clone(), found.title.clone()),
            None => {
                let suffix = handover_title("", origin.number);
                let title = source
                    .title
                    .strip_suffix(&suffix)
                    .unwrap_or(&source.title)
                    .to_owned();
                (origin.conversation_id.clone(), title)
            }
        },
    };
    let highest = conversations
        .filter_map(|candidate| candidate.handoff_of.as_ref())
        .filter(|handoff| handoff.conversation_id == origin_id)
        .map(|handoff| handoff.number)
        .max()
        .unwrap_or(0);
    (origin_id, origin_title, highest + 1)
}

/// Commits the continuation and arms its first run on the opening message.
///
/// It is a new conversation, not a fork: no `fork_of`, and it takes the
/// source's place in the tree rather than hanging beneath it — a top-level
/// source's continuation is top-level, a child's is that child's sibling. It
/// inherits the settings less the tool lock, the workspaces, the grants and the
/// preset, and no cache state: the lock records a surface and a request time
/// the continuation never sent, and would have it draw cache warnings and
/// frozen rows over a prompt cache it does not have.
fn create_child(
    state: &AppState,
    anchor: &Path,
    workspace_id: &str,
    source_id: &str,
    child_id: String,
    contexts: Vec<ContextItem>,
) -> Result<Conversation, String> {
    let store = crate::conversations::store(anchor)?;
    let source = store
        .conversation(source_id)?
        .ok_or_else(|| format!("源对话 {source_id} 不存在"))?;
    let snapshot = state.document_store.current_snapshot(anchor).ok();
    let listed = snapshot
        .iter()
        .flat_map(|document| document.workspaces.iter())
        .flat_map(|workspace| workspace.conversations.iter());
    let (origin_id, origin_title, number) = handover_naming(listed, &source);
    let now = Utc::now().to_rfc3339();
    let child = Conversation {
        id: child_id,
        title: handover_title(&origin_title, number),
        created_at: now.clone(),
        updated_at: now,
        settings: ConversationSettings {
            tool_lock: None,
            ..source.settings.clone()
        },
        contexts,
        queued_messages: Vec::new(),
        branches: Vec::new(),
        user_aborted_tasks: Vec::new(),
        worktrees: source.worktrees.clone(),
        run_target: source.run_target.clone(),
        attached_workspaces: source.attached_workspaces.clone(),
        additional_directories: source.additional_directories.clone(),
        parent_conversation_id: source.parent_conversation_id.clone(),
        fork_of: None,
        handoff_of: Some(ConversationHandoffOrigin {
            conversation_id: origin_id,
            number,
        }),
        preset_id: source.preset_id.clone(),
        template_id: String::new(),
    };
    let created = {
        let _guard = state
            .storage_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        crate::conversations::create_with_fork_start(
            state,
            anchor,
            workspace_id,
            &child,
            child.contexts.last().map(ContextItem::id),
        )?
    };
    // The continuation's name is the host's; the local model must not rename
    // it from the opening message when its first run starts.
    if let Err(error) = store.set_title_settled(&created.id, true) {
        eprintln!("交接续接对话的标题状态未能写入：{error}");
    }
    Ok(created)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notebook(directory: &tempfile::TempDir) -> Notebook {
        Notebook::for_conversation(directory.path(), "conv_1").unwrap()
    }

    fn system(id: &str, content: &str) -> ContextItem {
        ContextItem::System {
            id: id.into(),
            content: content.into(),
            local_only: false,
            hook_execution: None,
            tools_added: Vec::new(),
            created_at: String::new(),
        }
    }

    fn listed(id: &str, title: &str, handoff_of: Option<(&str, u32)>) -> Conversation {
        let mut conversation =
            crate::catalog::default_document().workspaces[0].conversations[0].clone();
        conversation.id = id.into();
        conversation.title = title.into();
        conversation.handoff_of = handoff_of.map(|(origin, number)| ConversationHandoffOrigin {
            conversation_id: origin.into(),
            number,
        });
        conversation
    }

    #[test]
    fn a_chain_of_handoffs_counts_under_its_origin() {
        let origin = listed("conv_a", "长任务", None);
        let first = listed("conv_b", "长任务-handover-1", Some(("conv_a", 1)));
        // A continuation the user renamed still counts under its origin.
        let second = listed("conv_c", "重命名了", Some(("conv_a", 2)));
        let unrelated = listed("conv_x", "长任务", None);
        let document = [
            origin.clone(),
            first.clone(),
            second.clone(),
            unrelated.clone(),
        ];

        assert_eq!(
            handover_naming(document.iter(), &origin),
            ("conv_a".into(), "长任务".into(), 3)
        );
        assert_eq!(
            handover_naming(document.iter(), &second),
            ("conv_a".into(), "长任务".into(), 3),
            "not 重命名了-handover-1: a chain does not stack suffixes"
        );
        assert_eq!(handover_title("长任务", 3), "长任务-handover-3");
        // Numbering is keyed by id: a namesake starts its own.
        assert_eq!(
            handover_naming(document.iter(), &unrelated),
            ("conv_x".into(), "长任务".into(), 1)
        );
        // A fork is not a continuation and takes no number from the chain.
        let mut fork = listed("conv_f", "长任务-fork-1", None);
        fork.fork_of = Some(crate::model::ConversationForkOrigin {
            conversation_id: "conv_a".into(),
            number: 1,
        });
        assert_eq!(
            handover_naming([origin.clone(), fork.clone()].iter(), &fork),
            ("conv_f".into(), "长任务-fork-1".into(), 1)
        );

        // With the origin gone, the source's own title less its suffix names
        // the chain, and a renamed source keeps its whole title.
        let orphans = [first.clone(), second.clone()];
        assert_eq!(
            handover_naming(orphans.iter(), &first),
            ("conv_a".into(), "长任务".into(), 3)
        );
        assert_eq!(
            handover_naming(orphans.iter(), &second),
            ("conv_a".into(), "重命名了".into(), 3)
        );
    }

    #[test]
    fn the_threshold_is_the_percent_of_the_window_rounded_down() {
        assert_eq!(threshold_tokens(200_000, 80), 160_000);
        assert_eq!(threshold_tokens(128_000, 33), 42_240);
        assert_eq!(threshold_tokens(999, 97), 969, "969.03 rounds down");
        // Out-of-range settings are clamped rather than obeyed.
        assert_eq!(threshold_tokens(1_000, 5), 200);
        assert_eq!(threshold_tokens(1_000, 100), 970);
    }

    #[test]
    fn a_notebook_lives_under_app_data_and_refuses_path_like_ids() {
        let directory = tempfile::tempdir().unwrap();
        assert_eq!(
            notebook(&directory).dir,
            directory.path().join("handoffs").join("conv_1")
        );
        for id in ["", "../x", "a/b", "a\\b", "c:d", "."] {
            assert!(
                Notebook::for_conversation(directory.path(), id).is_none(),
                "{id:?}"
            );
        }
        assert!(Notebook::for_conversation(Path::new(""), "conv_1").is_none());
    }

    #[test]
    fn notes_are_created_edited_read_and_indexed() {
        let directory = tempfile::tempdir().unwrap();
        let notebook = notebook(&directory);
        assert!(notebook.entries().is_empty());
        assert_eq!(
            notebook.create("state", "step 2 of 3", "where the work stands").unwrap(),
            "state.md"
        );
        assert!(notebook
            .create("state.md", "again", "dup")
            .unwrap_err()
            .contains("already exists"));
        notebook
            .edit("state.md", "step 2", "step 3", "where the work stands now")
            .unwrap();
        assert_eq!(notebook.read("state").unwrap().1, "step 3 of 3");
        assert_eq!(
            notebook.entries(),
            vec![IndexEntry {
                name: "state.md".into(),
                description: "where the work stands now".into()
            }]
        );
        // The index is the host's, and names never leave the notebook.
        assert!(notebook.read("HANDOFF").unwrap_err().contains("host-managed"));
        assert!(notebook
            .create("../escape", "x", "y")
            .unwrap_err()
            .starts_with("Handoff note name"));
        assert!(notebook.create("empty", "  ", "y").is_err());
        assert!(notebook.edit("state", "", "x", "y").is_err());
    }

    #[test]
    fn a_copy_carries_every_note_and_the_index() {
        let directory = tempfile::tempdir().unwrap();
        let source = notebook(&directory);
        source.create("a", "alpha", "first").unwrap();
        source.create("b", "beta", "second").unwrap();
        let target = Notebook::for_conversation(directory.path(), "conv_2").unwrap();
        source.copy_into(&target).unwrap();
        assert_eq!(target.entries(), source.entries());
        assert_eq!(target.read("b").unwrap().1, "beta");
        // The copy is independent of its source.
        target.edit("a", "alpha", "omega", "first").unwrap();
        assert_eq!(source.read("a").unwrap().1, "alpha");
        target.remove();
        assert!(target.entries().is_empty());
    }

    #[test]
    fn the_tools_follow_the_notebook_the_notes_and_the_arming() {
        let directory = tempfile::tempdir().unwrap();
        let mut run = HandoffRun {
            notebook: Some(notebook(&directory)),
            armed: false,
            has_notes: false,
        };
        assert!(run.derived_tools().is_empty());
        run.note_written();
        assert_eq!(run.derived_tools(), &[READ_NOTE_TOOL]);
        run.arm();
        assert_eq!(run.derived_tools(), &ARMED_TOOLS);
        assert!(!run.derived_tools().contains(&crate::api::BOX_TOOL));
        assert!(HandoffRun::default().derived_tools().is_empty());
    }

    #[test]
    fn the_continuation_carries_the_first_system_card_the_index_and_the_opening() {
        let profile = PromptProfile::builtin_english();
        let source = vec![
            system("preset", "You are the project's reviewer."),
            system("second", "a later card"),
            crate::system_append::card("plan-mode", "Plan first.".into(), String::new()),
        ];
        let entries = vec![IndexEntry {
            name: "state.md".into(),
            description: "where the work stands".into(),
        }];
        let contexts = child_contexts(&profile, &source, &entries, true);
        assert_eq!(contexts.len(), 3);
        let ContextItem::System { content, .. } = &contexts[0] else {
            panic!("the system card comes first");
        };
        assert_eq!(content, "You are the project's reviewer.");
        let ContextItem::System { id, content, .. } = &contexts[1] else {
            panic!("then the index");
        };
        assert_eq!(id, INDEX_CONTEXT_ID);
        assert!(content.contains("- state.md — where the work stands"), "{content}");
        let ContextItem::User { content, .. } = &contexts[2] else {
            panic!("the fork-start anchor is a user message that comes last");
        };
        assert_eq!(content, profile.text(PromptKey::HandoffStartMessage));

        // Where the system prompt's end comes before the tools, the index is
        // not a card of the timeline: the first run hands it over.
        let deferred = child_contexts(&profile, &source, &entries, false);
        assert_eq!(deferred.len(), 2);
        assert!(matches!(&deferred[0], ContextItem::System { content, .. } if content == "You are the project's reviewer."));
        assert!(matches!(&deferred[1], ContextItem::User { .. }));

        // A system prompt is the timeline's first context or nothing: behind
        // anything else — an earlier continuation's index, a skill an older
        // build delivered, a hook's record, a user message — it is not passed on.
        let hook = ContextItem::System {
            id: "hook".into(),
            content: "hook output".into(),
            local_only: false,
            hook_execution: Some(crate::model::HookContextMetadata {
                execution_id: "run".into(),
                hook_id: "hook".into(),
                hook_name: "hook".into(),
                event: "SessionStart".into(),
                status: "completed".into(),
                context_injected: true,
            }),
            tools_added: Vec::new(),
            created_at: String::new(),
        };
        let user = ContextItem::User {
            id: "ctx_user".into(),
            content: "Hello.".into(),
            images: Vec::new(),
            files: Vec::new(),
            created_at: String::new(),
        };
        for first in [system(INDEX_CONTEXT_ID, "old index"), system("ctx_skill_x", "a skill"), hook, user] {
            let bare = child_contexts(&profile, &[first.clone(), source[0].clone()], &entries, true);
            assert_eq!(bare.len(), 2, "behind {}", first.id());
            assert_eq!(bare[0].id(), INDEX_CONTEXT_ID);
        }
    }

    #[test]
    fn a_continuation_without_the_index_in_its_system_prompt_is_owed_it_once() {
        let directory = tempfile::tempdir().unwrap();
        let notes = notebook(&directory);
        notes
            .create("state", "Next, buy the milk.", "where the work stands")
            .unwrap();
        let profile = PromptProfile::builtin_english();
        let mut request = crate::agents::tests::template();
        request.app_data_path = directory.path().to_string_lossy().into_owned();
        request.conversation_id = "conv_1".into();
        request.subagent_depth = 0;
        request.output_schema = None;
        request.contexts =
            child_contexts(&profile, &[system("preset", "You review.")], &notes.entries(), false);
        request.handoff = HandoffRun::resolve(&request);

        let index = owed_index(&request, &[]).expect("owed the index");
        assert!(index.contains("- state.md — where the work stands"), "{index}");
        // Told once, by whichever carrier — even one this run added and has
        // not synced back yet.
        let appended = crate::system_append::card(INDEX_TOPIC, index, String::new());
        assert!(owed_index(&request, [&appended]).is_none());
        for told in [system(INDEX_NOTICE_ID, "the index"), system(INDEX_CONTEXT_ID, "the index"), appended] {
            let mut again = request.clone();
            again.contexts.push(told.clone());
            assert!(owed_index(&again, &[]).is_none(), "{}", told.id());
        }
        // The system prompt already carries it.
        let mut in_prompt = request.clone();
        in_prompt.contexts =
            child_contexts(&profile, &[system("preset", "You review.")], &notes.entries(), true);
        assert!(owed_index(&in_prompt, &[]).is_none());
        // A conversation that was not opened by a handoff is owed nothing, nor
        // is a child agent.
        let mut source = request.clone();
        source.contexts.retain(|context| !matches!(context, ContextItem::User { .. }));
        assert!(owed_index(&source, &[]).is_none());
        let mut child = request.clone();
        child.subagent_depth = 1;
        assert!(owed_index(&child, &[]).is_none());
    }
}
