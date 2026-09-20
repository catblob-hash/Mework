//! Two-tier, plain-Markdown memory.
//!
//! Memory here is exactly what it looks like on disk: a directory of Markdown
//! documents the model maintains itself. There is no owner identity, no
//! database, no CAS version and no revision history — a memory belongs to a
//! *location*, not to a model.
//!
//! ```text
//! ~/.mework/                 <workspace>/.mework/
//!   MEWORK.md                  MEWORK.md          <- always-on instructions
//!   memory/                    memory/
//!     MEMORY.md                  MEMORY.md        <- host-owned index
//!     <topic>.md                 <topic>.md       <- model-owned documents
//! ```
//!
//! `MEWORK.md` and `MEMORY.md` are concatenated into the conversation context
//! for each tier the conversation enabled — the two tiers switch on and off
//! independently. Topic documents are not: the model reads them on demand
//! through the read tool.
//!
//! `MEMORY.md` is deliberately **not** reachable by any tool. The host owns it
//! and rewrites it from the index descriptions supplied to the create/edit
//! tools, so the model can never desynchronize the index from the documents it
//! describes, and a write can never silently change what the next run loads.

use crate::{
    memory_archive_file::{read_bounded_nofollow, write_all_nofollow},
    model::JsonObject,
    prompt_profile::{PromptKey, PromptProfile},
};
use std::{
    fs,
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
};

/// Filename of the always-on instruction document at each tier's root.
pub const INSTRUCTIONS_NAME: &str = "MEWORK.md";
/// Filename of the host-owned index inside each tier's `memory/` directory.
pub const INDEX_NAME: &str = "MEMORY.md";
/// Directory holding the index and every topic document.
pub const MEMORY_DIR: &str = "memory";
/// Root directory of a memory tier, under the home directory or the workspace.
pub const MEWORK_DIR: &str = ".mework";

/// Largest single document accepted, in bytes. Generous for prose, small
/// enough that a runaway write cannot exhaust the context or the disk.
pub const MAX_DOCUMENT_BYTES: usize = 256 * 1024;
/// Largest index accepted. The index is pure pointers, so it stays small.
pub const MAX_INDEX_BYTES: usize = 64 * 1024;
/// Largest instruction document read into context per tier.
pub const MAX_INSTRUCTIONS_BYTES: usize = 128 * 1024;
/// Longest accepted document name, including the `.md` suffix.
const MAX_NAME_CHARS: usize = 120;
/// Longest accepted one-line index description.
const MAX_DESCRIPTION_CHARS: usize = 300;

/// Which tier a memory operation addresses.
///
/// The two tiers are independent directories with identical structure. Neither
/// shadows the other: both are loaded, and a document name may exist in both.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryTier {
    /// `~/.mework` — shared by every workspace on this machine.
    Global,
    /// `<workspace>/.mework` — scoped to the open workspace.
    Project,
}

impl MemoryTier {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Project => "project",
        }
    }

    pub fn prompt_key(self) -> PromptKey {
        match self {
            Self::Global => PromptKey::MemoryTierGlobal,
            Self::Project => PromptKey::MemoryTierProject,
        }
    }

    /// Human-facing tier label used in errors.
    pub fn label(self) -> &'static str {
        match self {
            Self::Global => "global memory",
            Self::Project => "project memory",
        }
    }
}

/// A resolved, existing-or-creatable memory tier root.
///
/// Holding one of these means the host — not the model — chose the directory.
/// Tools name documents; they never supply paths.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemoryRoot {
    tier: MemoryTier,
    root: PathBuf,
}

impl MemoryRoot {
    pub fn tier(&self) -> MemoryTier {
        self.tier
    }

    /// `<root>/MEWORK.md`
    pub fn instructions_path(&self) -> PathBuf {
        self.root.join(INSTRUCTIONS_NAME)
    }

    /// `<root>/memory`
    pub fn memory_dir(&self) -> PathBuf {
        self.root.join(MEMORY_DIR)
    }

    /// `<root>/memory/MEMORY.md`
    pub fn index_path(&self) -> PathBuf {
        self.memory_dir().join(INDEX_NAME)
    }

    /// `<root>/memory/<name>` for an already-validated document name.
    fn document_path(&self, validated_name: &str) -> PathBuf {
        self.memory_dir().join(validated_name)
    }
}

/// Resolves the global tier at `~/.mework`.
///
/// Returns `None` when the platform reports no home directory, which makes
/// global memory unavailable rather than falling back to another location.
pub fn global_root(home: Option<&Path>) -> Option<MemoryRoot> {
    home.map(|home| MemoryRoot {
        tier: MemoryTier::Global,
        root: home.join(MEWORK_DIR),
    })
}

/// Resolves the project tier at `<workspace>/.mework`.
///
/// Returns `None` for a workspace with no stable directory (a temporary or
/// unsupported workspace), so project memory fails closed instead of leaking
/// into the global tier.
pub fn project_root(workspace: Option<&Path>) -> Option<MemoryRoot> {
    workspace.map(|workspace| MemoryRoot {
        tier: MemoryTier::Project,
        root: workspace.join(MEWORK_DIR),
    })
}

/// Normalizes a model-supplied document name to a safe `<name>.md` leaf.
///
/// The model may write `notes`, `notes.md` or `Notes.MD`; all resolve to the
/// same document. Anything that could escape the memory directory — a path
/// separator, a drive letter, `..`, a NUL, a leading dot — is rejected rather
/// than sanitized, so a rejected name never silently becomes a different one.
pub fn normalize_document_name(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("Memory document name must not be empty".into());
    }
    if trimmed.chars().count() > MAX_NAME_CHARS {
        return Err(format!(
            "Memory document name must not exceed {MAX_NAME_CHARS} characters"
        ));
    }

    // Strip an optional, case-insensitive `.md` suffix before validating the
    // stem, so `.md` itself (an empty stem) is rejected like any other empty
    // name instead of becoming a dotfile.
    let stem = match trimmed.len().checked_sub(3) {
        Some(cut) if trimmed[cut..].eq_ignore_ascii_case(".md") => &trimmed[..cut],
        _ => trimmed,
    };
    if stem.is_empty() {
        return Err("Memory document name must not be empty".into());
    }

    if stem.contains('/') || stem.contains('\\') {
        return Err("Memory document name must be a single file in the memory directory, without path separators".into());
    }
    if stem.contains(':') {
        return Err(
            "Memory document name must not contain a drive or data-stream separator".into(),
        );
    }
    if stem.contains('\0') {
        return Err("Memory document name must not contain a NUL character".into());
    }
    if stem.starts_with('.') {
        return Err("Memory document name must not start with a dot".into());
    }
    if stem.chars().any(|character| character.is_control()) {
        return Err("Memory document name must not contain control characters".into());
    }
    // `.` and `..` are already excluded by the leading-dot rule; this covers
    // trailing-dot and trailing-space names, which Windows silently truncates
    // and would therefore resolve to a different file than the one named.
    if stem.ends_with('.') || stem.ends_with(' ') {
        return Err("Memory document name must not end with a dot or space".into());
    }
    if stem
        .chars()
        .any(|character| matches!(character, '<' | '>' | '"' | '|' | '?' | '*'))
    {
        return Err(
            "Memory document name must not contain reserved characters: < > \" | ? *".into(),
        );
    }
    // Windows reserved device names (CON, NUL, COM1, and others) resolve as
    // devices rather than files. Reject their case-insensitive stem before any
    // dot suffix.
    let device_stem = stem.split('.').next().unwrap_or(stem);
    let is_reserved_device = matches!(
        device_stem.to_ascii_uppercase().as_str(),
        "CON" | "PRN" | "AUX" | "NUL"
    ) || {
        let upper = device_stem.to_ascii_uppercase();
        (upper.starts_with("COM") || upper.starts_with("LPT"))
            && upper.len() == 4
            && upper.as_bytes()[3].is_ascii_digit()
            && upper.as_bytes()[3] != b'0'
    };
    if is_reserved_device {
        return Err("Memory document name must not use a Windows reserved device name (CON, NUL, COM1, and similar names)".into());
    }

    let normalized = format!("{stem}.md");
    if normalized.eq_ignore_ascii_case(INDEX_NAME) {
        return Err(format!(
            "{INDEX_NAME} is the host-managed memory index and cannot be read or written directly; provide its description when creating or editing memory"
        ));
    }
    Ok(normalized)
}

/// Validates the one-line index description supplied with a write.
fn normalize_description(raw: &str) -> Result<String, String> {
    let collapsed = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return Err("Memory index description must not be empty; describe what this memory records in one sentence".into());
    }
    if collapsed.chars().count() > MAX_DESCRIPTION_CHARS {
        return Err(format!(
            "Memory index description must not exceed {MAX_DESCRIPTION_CHARS} characters; it is only an index entry, so put the full text in the memory document"
        ));
    }
    Ok(collapsed)
}

/// One entry of a tier's index: a document and its one-line description.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexEntry {
    pub name: String,
    pub description: String,
}

/// Reads a tier's index into entries. A missing or unreadable index is an
/// empty index: memory is best-effort context, never a hard runtime failure.
///
/// Entries whose document file no longer exists are dropped here: an external
/// editor may delete or rename a topic file without touching `MEMORY.md`, and
/// a dangling entry would keep describing a document the read tool cannot
/// open. The cleaned list persists naturally on the next index rewrite.
pub fn read_index(root: &MemoryRoot) -> Vec<IndexEntry> {
    let Ok(bytes) = read_bounded_nofollow(&root.index_path(), MAX_INDEX_BYTES) else {
        return Vec::new();
    };
    let Ok(text) = String::from_utf8(bytes) else {
        return Vec::new();
    };
    let mut entries = parse_index(&text);
    entries.retain(|entry| fs::symlink_metadata(root.document_path(&entry.name)).is_ok());
    entries
}

/// Parses the `- [name](name) — description` lines the host writes.
///
/// Unrecognized lines are skipped rather than treated as an error, so a
/// hand-edited index degrades to the entries that are still well-formed.
fn parse_index(text: &str) -> Vec<IndexEntry> {
    let mut entries = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("- [") else {
            continue;
        };
        let Some((label, rest)) = rest.split_once("](") else {
            continue;
        };
        let Some((_target, rest)) = rest.split_once(')') else {
            continue;
        };
        let Ok(name) = normalize_document_name(label) else {
            continue;
        };
        let description = rest
            .trim_start()
            .trim_start_matches('—')
            .trim_start_matches('-')
            .trim()
            .to_owned();
        if entries.iter().any(|entry: &IndexEntry| entry.name == name) {
            continue;
        }
        entries.push(IndexEntry { name, description });
    }
    entries
}

/// Renders entries back into the index document.
fn render_index(tier: MemoryTier, entries: &[IndexEntry]) -> String {
    let mut out = String::new();
    out.push_str("# ");
    out.push_str(tier.label());
    out.push_str(" index\n\n");
    if entries.is_empty() {
        out.push_str("(no memory documents yet)\n");
        return out;
    }
    out.push_str("<!-- Maintained automatically by Mework: descriptions supplied when memory is created or edited are recorded here. -->\n\n");
    for entry in entries {
        out.push_str("- [");
        out.push_str(&entry.name);
        out.push_str("](");
        out.push_str(&entry.name);
        out.push(')');
        if !entry.description.is_empty() {
            out.push_str(" — ");
            out.push_str(&entry.description);
        }
        out.push('\n');
    }
    out
}

/// Rewrites the index so `name` carries `description`, preserving the order of
/// existing entries and appending a genuinely new document at the end.
fn upsert_index_entry(root: &MemoryRoot, name: &str, description: &str) -> Result<(), String> {
    let mut entries = read_index(root);
    match entries.iter_mut().find(|entry| entry.name == name) {
        Some(existing) => existing.description = description.to_owned(),
        None => entries.push(IndexEntry {
            name: name.to_owned(),
            description: description.to_owned(),
        }),
    }
    write_index(root, &entries)
}

fn write_index(root: &MemoryRoot, entries: &[IndexEntry]) -> Result<(), String> {
    let rendered = render_index(root.tier(), entries);
    if rendered.len() > MAX_INDEX_BYTES {
        return Err(format!(
            "Memory index exceeds the {MAX_INDEX_BYTES}-byte limit; delete or merge some memory documents first"
        ));
    }
    ensure_memory_dir(root)?;
    write_all_nofollow(&root.index_path(), rendered.as_bytes(), MAX_INDEX_BYTES)
        .map_err(|_| "Could not write the memory index".to_owned())
}

fn ensure_memory_dir(root: &MemoryRoot) -> Result<(), String> {
    let directory = root.memory_dir();
    // `create_dir_all` succeeds on an existing directory. A pre-existing
    // non-directory (or a link pointing at one) is rejected here, and the
    // no-follow write below rejects a linked document even if this passes.
    if let Ok(metadata) = fs::symlink_metadata(&directory) {
        if !metadata.is_dir() {
            return Err("Memory directory is occupied by a file or link with the same name".into());
        }
    }
    fs::create_dir_all(&directory).map_err(|_| "Could not create the memory directory".to_owned())
}

/// Filename of the cross-process mutation lock inside each tier's `memory/`
/// directory. A dotfile so it never collides with a normalized document name.
const MUTATION_LOCK_NAME: &str = ".memory.lock";

/// Takes the tier's cross-process mutation lock for one read→rewrite section.
///
/// Two Mework instances (for example production and browser-dev) share the
/// same user-level `~/.mework` tree, and every mutation here is a
/// read-modify-write of `MEMORY.md` plus a body file. The in-process storage
/// lock cannot see the other process, so concurrent mutations silently lost
/// updates. The lock is blocking — mutations are tiny, so waiting beats
/// failing — and releases when the returned handle drops.
fn acquire_mutation_lock(root: &MemoryRoot) -> Result<File, String> {
    ensure_memory_dir(root)?;
    let lock_path = root.memory_dir().join(MUTATION_LOCK_NAME);
    match fs::symlink_metadata(&lock_path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return Err("Memory mutation lock path is not a regular file".into());
        }
        _ => {}
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ, FILE_SHARE_WRITE,
        };
        options
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options
        .open(&lock_path)
        .map_err(|_| "Could not open the memory mutation lock file".to_owned())?;
    let metadata = file
        .metadata()
        .map_err(|_| "Could not validate the memory mutation lock file".to_owned())?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("Memory mutation lock path is not a regular file".into());
    }
    fs2::FileExt::lock_exclusive(&file)
        .map_err(|_| "Could not acquire the memory mutation lock".to_owned())?;
    Ok(file)
}

/// Result of reading one memory document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemoryDocument {
    pub tier: MemoryTier,
    pub name: String,
    pub content: String,
}

/// Reads one topic document by name.
pub fn read_document(root: &MemoryRoot, raw_name: &str) -> Result<MemoryDocument, String> {
    let name = normalize_document_name(raw_name)?;
    let bytes =
        read_bounded_nofollow(&root.document_path(&name), MAX_DOCUMENT_BYTES).map_err(|_| {
            format!(
                "No memory document named {name} exists in {}",
                root.tier().label()
            )
        })?;
    let content = String::from_utf8(bytes)
        .map_err(|_| format!("Memory document {name} is not valid UTF-8 text"))?;
    Ok(MemoryDocument {
        tier: root.tier(),
        name,
        content,
    })
}

/// Creates a new document and records its index description.
///
/// Refuses to overwrite an existing document: creation and modification stay
/// distinct so an accidental re-create cannot silently discard prior content.
pub fn create_document(
    root: &MemoryRoot,
    raw_name: &str,
    content: &str,
    raw_description: &str,
) -> Result<MemoryDocument, String> {
    let name = normalize_document_name(raw_name)?;
    let description = normalize_description(raw_description)?;
    validate_content(content)?;

    // The existence check through index rewrite is a cross-process critical section.
    let _mutation_lock = acquire_mutation_lock(root)?;
    let path = root.document_path(&name);
    if fs::symlink_metadata(&path).is_ok() {
        return Err(format!(
            "{name} already exists in {}; use the edit-memory tool or choose another name",
            root.tier().label()
        ));
    }

    ensure_memory_dir(root)?;
    write_all_nofollow(&path, content.as_bytes(), MAX_DOCUMENT_BYTES)
        .map_err(|_| format!("Could not write memory document {name}"))?;
    // A document the index does not mention is invisible to the next run, so
    // an index failure un-creates the document rather than leaving an
    // orphan the model has no way to discover.
    if let Err(error) = upsert_index_entry(root, &name, &description) {
        let _ = fs::remove_file(&path);
        return Err(error);
    }
    Ok(MemoryDocument {
        tier: root.tier(),
        name,
        content: content.to_owned(),
    })
}

/// Replaces one exact substring in an existing document, and refreshes its
/// index description.
///
/// The match must be unique, mirroring the file-editing tools: a non-unique
/// `old_text` is an ambiguous edit, not a request to change the first hit.
pub fn edit_document(
    root: &MemoryRoot,
    raw_name: &str,
    old_text: &str,
    new_text: &str,
    raw_description: &str,
) -> Result<MemoryDocument, String> {
    let name = normalize_document_name(raw_name)?;
    let description = normalize_description(raw_description)?;
    if old_text.is_empty() {
        return Err(
            "Text to replace must not be empty; use the create-memory tool for a new memory".into(),
        );
    }
    if old_text == new_text {
        return Err(
            "The old and replacement text are identical; there is no change to write".into(),
        );
    }

    // Reading, modifying, and rewriting the body and index is a cross-process critical section.
    let _mutation_lock = acquire_mutation_lock(root)?;
    let existing = read_document(root, &name)?;
    let occurrences = existing.content.matches(old_text).count();
    if occurrences == 0 {
        return Err(format!(
            "Could not find the text to replace in memory document {name}"
        ));
    }
    if occurrences > 1 {
        return Err(format!(
            "The text to replace occurs {occurrences} times in memory document {name}; provide a longer unique match"
        ));
    }

    let content = existing.content.replacen(old_text, new_text, 1);
    validate_content(&content)?;
    // Update the index first. The document already exists and is already
    // listed, so a failure here leaves the prior description in place and the
    // body untouched — consistent, just not yet edited. Writing the body first
    // could instead leave a document whose index line describes the old text.
    upsert_index_entry(root, &name, &description)?;
    write_all_nofollow(
        &root.document_path(&name),
        content.as_bytes(),
        MAX_DOCUMENT_BYTES,
    )
    .map_err(|_| format!("Could not write memory document {name}"))?;
    Ok(MemoryDocument {
        tier: root.tier(),
        name,
        content,
    })
}

/// Deletes one topic document and removes its index entry.
///
/// The host settings UI may remove documents; model tools never expose this
/// operation. The index is rewritten **before** the document is removed:
/// a failure between the two steps then leaves an unlisted body file —
/// invisible to the next run, recoverable by hand — rather than a dangling
/// index entry that describes a document which no longer exists.
pub fn delete_document(root: &MemoryRoot, raw_name: &str) -> Result<(), String> {
    let name = normalize_document_name(raw_name)?;
    // Deleting the body and rewriting the index is a cross-process critical section.
    let _mutation_lock = acquire_mutation_lock(root)?;
    let path = root.document_path(&name);
    if fs::symlink_metadata(&path).is_err() {
        return Err(format!(
            "No memory document named {name} exists in {}",
            root.tier().label()
        ));
    }
    let mut entries = read_index(root);
    entries.retain(|entry| entry.name != name);
    write_index(root, &entries)?;
    fs::remove_file(&path).map_err(|_| format!("Could not delete memory document {name}"))?;
    Ok(())
}

fn validate_content(content: &str) -> Result<(), String> {
    if content.len() > MAX_DOCUMENT_BYTES {
        return Err(format!(
            "Memory document exceeds the {MAX_DOCUMENT_BYTES}-byte limit; split it into multiple memories"
        ));
    }
    if content.contains('\0') {
        return Err("Memory document must not contain a NUL character".into());
    }
    Ok(())
}

/// One tier's contribution to the conversation context.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TierContext {
    pub instructions: Option<String>,
    pub index: Option<String>,
}

impl TierContext {
    fn is_empty(&self) -> bool {
        self.instructions.is_none() && self.index.is_none()
    }
}

/// Reads the two always-on documents of one tier.
pub fn read_tier_context(root: &MemoryRoot) -> TierContext {
    TierContext {
        instructions: read_text(&root.instructions_path(), MAX_INSTRUCTIONS_BYTES),
        index: read_text(&root.index_path(), MAX_INDEX_BYTES),
    }
}

fn read_text(path: &Path, maximum_bytes: usize) -> Option<String> {
    let bytes = read_bounded_nofollow(path, maximum_bytes).ok()?;
    let text = String::from_utf8(bytes).ok()?;
    (!text.trim().is_empty()).then_some(text)
}

/// Delimiters framing the memory block in the conversation context. They let
/// the host recognize and replace its own block without re-parsing prose.
pub const MEMORY_CONTEXT_START: &str = "<mework-memory>";
pub const MEMORY_CONTEXT_END: &str = "</mework-memory>";

/// Assembles the memory block injected when the conversation enables memory.
///
/// Global comes first and project second, so the more specific tier is nearest
/// the conversation. Returns `None` when both tiers are empty, so an enabled
/// toggle with nothing stored costs no context at all.
pub fn render_memory_context(
    global: &TierContext,
    project: &TierContext,
    profile: &PromptProfile,
) -> Option<String> {
    if global.is_empty() && project.is_empty() {
        return None;
    }

    let mut out = String::from(MEMORY_CONTEXT_START);
    out.push('\n');
    out.push_str(profile.text(PromptKey::MemoryContextIntro));
    out.push('\n');
    push_tier(&mut out, MemoryTier::Global, global, profile);
    push_tier(&mut out, MemoryTier::Project, project, profile);
    out.push_str(MEMORY_CONTEXT_END);
    Some(out)
}

fn push_tier(out: &mut String, tier: MemoryTier, context: &TierContext, profile: &PromptProfile) {
    if context.is_empty() {
        return;
    }
    let tier = profile.text(tier.prompt_key());
    if let Some(instructions) = context.instructions.as_deref() {
        out.push('\n');
        out.push_str(&profile.render(PromptKey::MemoryInstructionsHeading, &[("tier", tier)]));
        out.push_str("\n\n");
        out.push_str(instructions.trim_end());
        out.push('\n');
    }
    if let Some(index) = context.index.as_deref() {
        out.push('\n');
        out.push_str(&profile.render(PromptKey::MemoryIndexHeading, &[("tier", tier)]));
        out.push_str("\n\n");
        out.push_str(index.trim_end());
        out.push('\n');
    }
}

/// Every memory tool name, both tiers.
///
/// Two verbs per tier, plus read. There is deliberately no list tool (the
/// index is already in context), no search tool (the index is small enough to
/// scan), and no delete tool (removing a memory is a user action in the
/// settings UI, not something a model should do mid-turn).
///
/// This is the *broad* list: stripping, redaction and UI exclusion all address
/// memory as one family regardless of which tier a conversation turned on.
/// Granting tools is the opposite — it goes through [`tool_names_for_tier`],
/// because a conversation enables the two tiers independently.
pub const MEMORY_TOOL_NAMES: [&str; 6] = [
    "read_global_memory",
    "read_project_memory",
    "create_global_memory",
    "create_project_memory",
    "edit_global_memory",
    "edit_project_memory",
];

/// The three tools a conversation gains by enabling global memory.
pub const GLOBAL_MEMORY_TOOL_NAMES: [&str; 3] = [
    "read_global_memory",
    "create_global_memory",
    "edit_global_memory",
];

/// The three tools a conversation gains by enabling project memory.
pub const PROJECT_MEMORY_TOOL_NAMES: [&str; 3] = [
    "read_project_memory",
    "create_project_memory",
    "edit_project_memory",
];

/// The exact three tools of one tier. Grant paths use this instead of the
/// six-name list so an enabled tier can never drag the other one along.
pub fn tool_names_for_tier(tier: MemoryTier) -> [&'static str; 3] {
    match tier {
        MemoryTier::Global => GLOBAL_MEMORY_TOOL_NAMES,
        MemoryTier::Project => PROJECT_MEMORY_TOOL_NAMES,
    }
}

/// True when `tool_name` is one of the six memory tools.
pub fn is_memory_tool(tool_name: &str) -> bool {
    MEMORY_TOOL_NAMES.contains(&tool_name)
}

/// The tier a memory tool addresses, or `None` if it is not a memory tool.
pub fn tool_tier(tool_name: &str) -> Option<MemoryTier> {
    if !is_memory_tool(tool_name) {
        return None;
    }
    Some(if tool_name.contains("_global_") {
        MemoryTier::Global
    } else {
        MemoryTier::Project
    })
}

/// Which tiers the conversation turned on.
///
/// Deliberately distinct from *availability*: an enabled tier can still be
/// unresolvable (no home directory, no workspace on disk), and a disabled tier
/// is refused even though its directory is sitting right there. Keeping the two
/// apart is what lets an error say which of the two reasons applies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemoryTierAccess {
    pub global: bool,
    pub project: bool,
}

impl MemoryTierAccess {
    /// Both tiers open. The settings-page IPCs use this: they manage the
    /// directories themselves and are not bound to any conversation's switches.
    pub const ALL: Self = Self {
        global: true,
        project: true,
    };

    pub fn allows(self, tier: MemoryTier) -> bool {
        match tier {
            MemoryTier::Global => self.global,
            MemoryTier::Project => self.project,
        }
    }
}

impl Default for MemoryTierAccess {
    fn default() -> Self {
        Self::ALL
    }
}

/// Host-resolved roots for one run. A tier absent here is unavailable for the
/// whole run, and its tools report that instead of writing somewhere else.
#[derive(Clone, Debug, Default)]
pub struct MemoryRoots {
    pub global: Option<MemoryRoot>,
    pub project: Option<MemoryRoot>,
    /// Which tiers this run is allowed to touch at all.
    pub access: MemoryTierAccess,
}

impl MemoryRoots {
    /// Resolves both tiers with neither one gated. For the settings-page
    /// directory management, which is not part of any conversation.
    pub fn resolve(home: Option<&Path>, workspace: Option<&Path>) -> Self {
        Self::resolve_enabled(home, workspace, MemoryTierAccess::ALL)
    }

    /// Resolves both tiers from the trusted home and workspace directories,
    /// recording which of them the conversation actually enabled.
    pub fn resolve_enabled(
        home: Option<&Path>,
        workspace: Option<&Path>,
        access: MemoryTierAccess,
    ) -> Self {
        Self {
            global: global_root(home),
            project: project_root(workspace),
            access,
        }
    }

    /// The root of one tier, but only if the conversation enabled it.
    fn enabled_root(&self, tier: MemoryTier) -> Option<&MemoryRoot> {
        if !self.access.allows(tier) {
            return None;
        }
        match tier {
            MemoryTier::Global => self.global.as_ref(),
            MemoryTier::Project => self.project.as_ref(),
        }
    }

    fn tier(&self, tier: MemoryTier) -> Result<&MemoryRoot, String> {
        // A disabled tier fails before availability is even consulted, so the
        // message never blames a missing directory for a switch the user left
        // off.
        if !self.access.allows(tier) {
            return Err(format!(
                "{} is not enabled for this conversation; enable it in conversation settings and try again",
                tier.label()
            ));
        }
        match tier {
            MemoryTier::Global => self
                .global
                .as_ref()
                .ok_or_else(|| "Global memory is unavailable because the host did not resolve a user home directory".to_owned()),
            MemoryTier::Project => self.project.as_ref().ok_or_else(|| {
                "Project memory is unavailable because this conversation is not bound to a workspace directory on disk".to_owned()
            }),
        }
    }

    /// Builds the always-on memory block for the conversation context.
    ///
    /// Only enabled tiers are read. A disabled tier contributes nothing — not
    /// its instructions, not its index — so turning a tier off is observable in
    /// the context, not just in the tool list.
    pub fn render_context(&self, profile: &PromptProfile) -> Option<String> {
        let global = self
            .enabled_root(MemoryTier::Global)
            .map(read_tier_context)
            .unwrap_or_default();
        let project = self
            .enabled_root(MemoryTier::Project)
            .map(read_tier_context)
            .unwrap_or_default();
        render_memory_context(&global, &project, profile)
    }
}

/// Executes one memory tool call.
///
/// Every argument is a plain document name or text; the tier comes from the
/// tool name and the directory comes from `roots`. No caller-supplied path
/// ever reaches the filesystem.
pub fn execute_tool(
    roots: &MemoryRoots,
    tool_name: &str,
    input: &JsonObject,
    profile: &PromptProfile,
) -> Result<String, String> {
    let tier = tool_tier(tool_name).ok_or_else(|| format!("Unknown memory tool: {tool_name}"))?;
    let root = roots.tier(tier)?;

    match tool_name {
        "read_global_memory" | "read_project_memory" => {
            let document = read_document(root, &required_text(input, "name")?)?;
            Ok(document.content)
        }
        "create_global_memory" | "create_project_memory" => {
            let document = create_document(
                root,
                &required_text(input, "name")?,
                &required_text(input, "content")?,
                &required_text(input, "description")?,
            )?;
            Ok(profile.render(
                PromptKey::MemoryCreated,
                &[
                    ("tier", profile.text(tier.prompt_key())),
                    ("name", &document.name),
                ],
            ))
        }
        "edit_global_memory" | "edit_project_memory" => {
            let document = edit_document(
                root,
                &required_text(input, "name")?,
                &required_text(input, "old_text")?,
                &required_text(input, "new_text")?,
                &required_text(input, "description")?,
            )?;
            Ok(profile.render(
                PromptKey::MemoryUpdated,
                &[
                    ("tier", profile.text(tier.prompt_key())),
                    ("name", &document.name),
                ],
            ))
        }
        other => Err(format!("Unknown memory tool: {other}")),
    }
}

fn required_text(input: &JsonObject, field: &str) -> Result<String, String> {
    match input.get(field) {
        Some(serde_json::Value::String(value)) => Ok(value.clone()),
        Some(serde_json::Value::Null) | None => Err(format!("Missing argument: {field}")),
        Some(_) => Err(format!("Argument {field} must be a string")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::atomic::{AtomicU64, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    static SEQUENCE: AtomicU64 = AtomicU64::new(0);

    struct TempTree {
        root: PathBuf,
    }

    impl TempTree {
        fn new(label: &str) -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!(
                "mework-memory-{}-{label}-{nonce}-{sequence}",
                std::process::id()
            ));
            fs::create_dir_all(&root).unwrap();
            Self { root }
        }

        fn global(&self) -> MemoryRoot {
            global_root(Some(&self.root.join("home"))).unwrap()
        }

        fn project(&self) -> MemoryRoot {
            project_root(Some(&self.root.join("workspace"))).unwrap()
        }
    }

    impl Drop for TempTree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn roots_resolve_to_the_documented_two_tier_layout() {
        let tree = TempTree::new("layout");
        let global = tree.global();
        assert_eq!(global.tier(), MemoryTier::Global);
        assert_eq!(
            global.instructions_path(),
            tree.root.join("home/.mework/MEWORK.md")
        );
        assert_eq!(
            global.index_path(),
            tree.root.join("home/.mework/memory/MEMORY.md")
        );

        let project = tree.project();
        assert_eq!(project.tier(), MemoryTier::Project);
        assert_eq!(
            project.instructions_path(),
            tree.root.join("workspace/.mework/MEWORK.md")
        );
        assert_eq!(
            project.index_path(),
            tree.root.join("workspace/.mework/memory/MEMORY.md")
        );

        // A missing home or an unstable workspace makes that tier unavailable
        // rather than silently redirecting it somewhere writable.
        assert!(global_root(None).is_none());
        assert!(project_root(None).is_none());
    }

    #[test]
    fn document_names_accept_bare_stems_and_reject_every_escape() {
        assert_eq!(normalize_document_name("notes").unwrap(), "notes.md");
        assert_eq!(normalize_document_name("notes.md").unwrap(), "notes.md");
        assert_eq!(normalize_document_name(" notes.MD ").unwrap(), "notes.md");
        assert_eq!(normalize_document_name("café").unwrap(), "café.md");

        for rejected in [
            "",
            "   ",
            ".md",
            "..",
            ".",
            ".hidden",
            "a/b",
            "a\\b",
            "../escape",
            "C:notes",
            "notes:stream",
            "notes.",
            "notes .md",
            "no<te",
            "no|te",
            "no*te",
        ] {
            assert!(
                normalize_document_name(rejected).is_err(),
                "{rejected:?} must be rejected"
            );
        }
        assert!(normalize_document_name("notes\0.md").is_err());
        assert!(normalize_document_name(&"a".repeat(MAX_NAME_CHARS + 1)).is_err());
    }

    #[test]
    fn the_index_is_never_addressable_as_a_document() {
        // The host owns MEMORY.md. No tool may name it under any casing or
        // with the suffix omitted, so the model cannot rewrite the index out
        // of step with the documents it points at.
        for spelling in ["MEMORY.md", "memory.md", "MEMORY", "Memory.MD", " memory "] {
            let error = normalize_document_name(spelling).unwrap_err();
            assert!(error.contains(INDEX_NAME), "{spelling:?}: {error}");
        }
    }

    #[test]
    fn create_writes_the_document_and_records_its_index_description() {
        let tree = TempTree::new("create");
        let root = tree.global();

        let created = create_document(
            &root,
            "build",
            "cargo test requires the project's bundled environment.",
            "build command",
        )
        .unwrap();
        assert_eq!(created.name, "build.md");
        assert_eq!(created.tier, MemoryTier::Global);
        assert_eq!(
            fs::read_to_string(root.memory_dir().join("build.md")).unwrap(),
            "cargo test requires the project's bundled environment."
        );

        let index = fs::read_to_string(root.index_path()).unwrap();
        assert!(index.contains("- [build.md](build.md) — build command"));

        // Re-creating must not clobber; the model is told to edit instead.
        let error =
            create_document(&root, "build", "other content", "other description").unwrap_err();
        assert!(error.contains("already exists"));
        assert_eq!(
            fs::read_to_string(root.memory_dir().join("build.md")).unwrap(),
            "cargo test requires the project's bundled environment."
        );
    }

    #[test]
    fn create_requires_a_usable_index_description() {
        let tree = TempTree::new("description");
        let root = tree.global();
        assert!(create_document(&root, "a", "body", "   ").is_err());
        assert!(
            create_document(&root, "a", "body", &"x".repeat(MAX_DESCRIPTION_CHARS + 1)).is_err()
        );
        // A failed write leaves nothing behind, index included.
        assert!(!root.memory_dir().join("a.md").exists());
        assert!(read_index(&root).is_empty());
    }

    #[test]
    fn edit_replaces_a_unique_match_and_refreshes_the_description() {
        let tree = TempTree::new("edit");
        let root = tree.project();
        create_document(&root, "notes", "The port is 3000.\nOther content.", "port").unwrap();

        let edited = edit_document(&root, "notes", "3000", "4000", "development port").unwrap();
        assert_eq!(edited.content, "The port is 4000.\nOther content.");
        let index = fs::read_to_string(root.index_path()).unwrap();
        assert!(index.contains("- [notes.md](notes.md) — development port"));
        assert!(!index.contains("— port\n"));
        // One entry per document, no duplicate line from the second write.
        assert_eq!(read_index(&root).len(), 1);
    }

    #[test]
    fn edit_refuses_ambiguous_missing_and_empty_matches() {
        let tree = TempTree::new("edit-guard");
        let root = tree.project();
        create_document(
            &root,
            "notes",
            "same paragraph\nsame paragraph",
            "duplicate",
        )
        .unwrap();

        let ambiguous = edit_document(&root, "notes", "same paragraph", "replacement", "duplicate")
            .unwrap_err();
        assert!(ambiguous.contains("occurs 2 times"), "{ambiguous}");
        assert!(edit_document(&root, "notes", "missing", "x", "duplicate").is_err());
        assert!(edit_document(&root, "notes", "", "x", "duplicate").is_err());
        assert!(edit_document(
            &root,
            "notes",
            "same paragraph",
            "same paragraph",
            "duplicate"
        )
        .is_err());
        assert!(edit_document(&root, "missing", "a", "b", "duplicate").is_err());

        // Every rejected edit left the document byte-identical.
        assert_eq!(
            read_document(&root, "notes").unwrap().content,
            "same paragraph\nsame paragraph"
        );
    }

    #[test]
    fn deleting_a_document_removes_its_index_entry() {
        let tree = TempTree::new("delete");
        let root = tree.global();
        create_document(&root, "notes", "body", "description").unwrap();
        delete_document(&root, "notes").unwrap();
        assert!(!root.memory_dir().join("notes.md").exists());
        assert!(read_index(&root).is_empty());
        assert!(delete_document(&root, "notes").is_err());
    }

    #[test]
    fn reading_a_missing_document_names_the_tier_without_leaking_a_path() {
        let tree = TempTree::new("read-missing");
        let root = tree.global();
        let error = read_document(&root, "absent").unwrap_err();
        assert!(error.contains("absent.md"));
        assert!(error.contains("global memory"));
        assert!(!error.contains(".mework"));
    }

    #[test]
    fn oversized_documents_are_refused() {
        let tree = TempTree::new("oversize");
        let root = tree.global();
        let huge = "x".repeat(MAX_DOCUMENT_BYTES + 1);
        assert!(create_document(&root, "big", &huge, "too large").is_err());
        assert!(!root.memory_dir().join("big.md").exists());
    }

    #[test]
    fn index_parsing_round_trips_and_skips_malformed_lines() {
        let entries = parse_index(
            "# Global memory index\n\n\
             - [a.md](a.md) — first entry\n\
             arbitrary prose\n\
             - entry without a link\n\
             - [b.md](b.md) second entry\n\
             - [a.md](a.md) — duplicate is ignored\n\
             - [../escape.md](../escape.md) — invalid name is ignored\n",
        );
        assert_eq!(
            entries,
            vec![
                IndexEntry {
                    name: "a.md".into(),
                    description: "first entry".into()
                },
                IndexEntry {
                    name: "b.md".into(),
                    description: "second entry".into()
                },
            ]
        );

        let rendered = render_index(MemoryTier::Global, &entries);
        assert_eq!(parse_index(&rendered), entries);
    }

    #[test]
    fn context_concatenates_instructions_and_index_but_never_topic_bodies() {
        let tree = TempTree::new("context");
        let global = tree.global();
        let project = tree.project();
        let profile = PromptProfile::builtin_english();
        fs::create_dir_all(global.memory_dir()).unwrap();
        fs::create_dir_all(project.memory_dir()).unwrap();
        fs::write(global.instructions_path(), "global standing instruction").unwrap();
        fs::write(project.instructions_path(), "project standing instruction").unwrap();
        create_document(&global, "g", "global memory body", "global entry").unwrap();
        create_document(&project, "p", "project memory body", "project entry").unwrap();

        let rendered = render_memory_context(
            &read_tier_context(&global),
            &read_tier_context(&project),
            &profile,
        )
        .unwrap();

        assert!(rendered.starts_with(MEMORY_CONTEXT_START));
        assert!(rendered.ends_with(MEMORY_CONTEXT_END));
        assert!(rendered.contains("Below is your long-term memory."));
        assert!(rendered.contains("## Global memory · MEWORK.md"));
        assert!(rendered.contains("## Project memory · MEMORY.md"));
        assert!(rendered.contains("global standing instruction"));
        assert!(rendered.contains("project standing instruction"));
        assert!(rendered.contains("- [g.md](g.md) — global entry"));
        assert!(rendered.contains("- [p.md](p.md) — project entry"));

        // Topic bodies stay on disk until the model reads them by name.
        assert!(!rendered.contains("global memory body"));
        assert!(!rendered.contains("project memory body"));

        // Global is injected before project, so the more specific tier sits
        // closest to the conversation.
        assert!(
            rendered.find("global standing instruction").unwrap()
                < rendered.find("project standing instruction").unwrap()
        );
    }

    #[test]
    fn context_is_absent_when_nothing_is_stored() {
        let tree = TempTree::new("empty-context");
        let global = read_tier_context(&tree.global());
        let project = read_tier_context(&tree.project());
        let profile = PromptProfile::builtin_english();
        assert_eq!(global, TierContext::default());
        assert!(render_memory_context(&global, &project, &profile).is_none());

        // A whitespace-only instruction file is treated as absent too.
        let root = tree.global();
        fs::create_dir_all(&root.memory_dir()).unwrap();
        fs::write(root.instructions_path(), "   \n\n").unwrap();
        assert!(render_memory_context(&read_tier_context(&root), &project, &profile).is_none());
    }

    #[test]
    fn the_two_tiers_are_independent_namespaces() {
        let tree = TempTree::new("tiers");
        let global = tree.global();
        let project = tree.project();

        create_document(&global, "notes", "global version", "global").unwrap();
        create_document(&project, "notes", "project version", "project").unwrap();

        assert_eq!(
            read_document(&global, "notes").unwrap().content,
            "global version"
        );
        assert_eq!(
            read_document(&project, "notes").unwrap().content,
            "project version"
        );

        edit_document(
            &project,
            "notes",
            "project version",
            "updated project version",
            "project",
        )
        .unwrap();
        assert_eq!(
            read_document(&global, "notes").unwrap().content,
            "global version"
        );
    }

    #[test]
    fn a_hand_written_index_survives_a_model_write() {
        let tree = TempTree::new("hand-index");
        let root = tree.global();
        fs::create_dir_all(root.memory_dir()).unwrap();
        fs::write(
            root.index_path(),
            "# Hand-written index\n\n- [existing.md](existing.md) — user description\n",
        )
        .unwrap();
        fs::write(root.memory_dir().join("existing.md"), "existing content").unwrap();

        create_document(&root, "fresh", "new content", "new entry").unwrap();
        let entries = read_index(&root);
        assert_eq!(
            entries,
            vec![
                IndexEntry {
                    name: "existing.md".into(),
                    description: "user description".into()
                },
                IndexEntry {
                    name: "fresh.md".into(),
                    description: "new entry".into()
                },
            ]
        );
    }

    fn input(pairs: &[(&str, &str)]) -> serde_json::Map<String, serde_json::Value> {
        pairs
            .iter()
            .map(|(key, value)| {
                (
                    (*key).to_owned(),
                    serde_json::Value::String((*value).to_owned()),
                )
            })
            .collect()
    }

    #[test]
    fn tool_names_map_to_exactly_one_tier_each() {
        assert_eq!(MEMORY_TOOL_NAMES.len(), 6);
        for name in MEMORY_TOOL_NAMES {
            assert!(is_memory_tool(name), "{name}");
        }
        assert_eq!(tool_tier("read_global_memory"), Some(MemoryTier::Global));
        assert_eq!(tool_tier("create_global_memory"), Some(MemoryTier::Global));
        assert_eq!(tool_tier("edit_global_memory"), Some(MemoryTier::Global));
        assert_eq!(tool_tier("read_project_memory"), Some(MemoryTier::Project));
        assert_eq!(
            tool_tier("create_project_memory"),
            Some(MemoryTier::Project)
        );
        assert_eq!(tool_tier("edit_project_memory"), Some(MemoryTier::Project));

        // The per-tier trios partition the broad list exactly: every name
        // belongs to its own tier and to no other, so granting one tier can
        // never widen into the other.
        for tier in [MemoryTier::Global, MemoryTier::Project] {
            for name in tool_names_for_tier(tier) {
                assert!(MEMORY_TOOL_NAMES.contains(&name), "{name}");
                assert_eq!(tool_tier(name), Some(tier), "{name}");
            }
        }
        assert_eq!(
            GLOBAL_MEMORY_TOOL_NAMES.len() + PROJECT_MEMORY_TOOL_NAMES.len(),
            MEMORY_TOOL_NAMES.len()
        );

        // Retired tools from the model-owned SQLite era must not resolve.
        for retired in [
            "memory_list",
            "memory_read",
            "memory_search",
            "memory_upsert",
            "memory_delete",
        ] {
            assert!(!is_memory_tool(retired), "{retired}");
            assert_eq!(tool_tier(retired), None);
        }
    }

    #[test]
    fn a_disabled_tier_contributes_no_context_and_refuses_its_own_tools() {
        let tree = TempTree::new("tier-switches");
        let home = tree.root.join("home");
        let workspace = tree.root.join("workspace");
        let profile = PromptProfile::builtin_english();
        for root in [tree.global(), tree.project()] {
            fs::create_dir_all(root.memory_dir()).unwrap();
        }
        fs::write(
            tree.global().instructions_path(),
            "global standing instruction",
        )
        .unwrap();
        fs::write(
            tree.project().instructions_path(),
            "project standing instruction",
        )
        .unwrap();
        create_document(&tree.global(), "g", "global body", "global entry").unwrap();
        create_document(&tree.project(), "p", "project body", "project entry").unwrap();

        let global_only = MemoryRoots::resolve_enabled(
            Some(&home),
            Some(&workspace),
            MemoryTierAccess {
                global: true,
                project: false,
            },
        );
        let rendered = global_only.render_context(&profile).unwrap();
        assert!(rendered.contains("global standing instruction"));
        assert!(rendered.contains("- [g.md](g.md) — global entry"));
        // Not one byte of the disabled tier, instructions or index.
        assert!(!rendered.contains("project standing instruction"));
        assert!(!rendered.contains("p.md"));
        for name in PROJECT_MEMORY_TOOL_NAMES {
            let error = execute_tool(
                &global_only,
                name,
                &input(&[
                    ("name", "p"),
                    ("content", "x"),
                    ("description", "x"),
                    ("old_text", "project body"),
                    ("new_text", "replacement"),
                ]),
                &profile,
            )
            .unwrap_err();
            assert!(
                error.contains("project memory is not enabled"),
                "{name}: {error}"
            );
        }
        assert_eq!(
            execute_tool(
                &global_only,
                "read_global_memory",
                &input(&[("name", "g")]),
                &profile,
            )
            .unwrap(),
            "global body"
        );

        // The mirror image, and then both off: no block at all.
        let project_only = MemoryRoots::resolve_enabled(
            Some(&home),
            Some(&workspace),
            MemoryTierAccess {
                global: false,
                project: true,
            },
        );
        let rendered = project_only.render_context(&profile).unwrap();
        assert!(rendered.contains("project standing instruction"));
        assert!(!rendered.contains("global standing instruction"));
        let error = execute_tool(
            &project_only,
            "read_global_memory",
            &input(&[("name", "g")]),
            &profile,
        )
        .unwrap_err();
        assert!(error.contains("global memory is not enabled"), "{error}");

        let none = MemoryRoots::resolve_enabled(
            Some(&home),
            Some(&workspace),
            MemoryTierAccess {
                global: false,
                project: false,
            },
        );
        assert!(none.render_context(&profile).is_none());
        for name in MEMORY_TOOL_NAMES {
            assert!(
                execute_tool(&none, name, &input(&[("name", "g")]), &profile).is_err(),
                "{name}"
            );
        }

        // A disabled tier is refused even though its directory exists, and the
        // message says so rather than blaming a missing directory.
        let unavailable = MemoryRoots::resolve_enabled(None, None, MemoryTierAccess::ALL);
        assert!(unavailable
            .tier(MemoryTier::Global)
            .unwrap_err()
            .contains("unavailable"));
    }

    #[test]
    fn the_dispatcher_round_trips_create_read_and_edit_per_tier() {
        let tree = TempTree::new("dispatch");
        let roots = MemoryRoots::resolve(
            Some(&tree.root.join("home")),
            Some(&tree.root.join("workspace")),
        );
        let profile = PromptProfile::builtin_english();

        for (create, read, edit, tier) in [
            (
                "create_global_memory",
                "read_global_memory",
                "edit_global_memory",
                "Global memory",
            ),
            (
                "create_project_memory",
                "read_project_memory",
                "edit_project_memory",
                "Project memory",
            ),
        ] {
            assert_eq!(
                execute_tool(
                    &roots,
                    create,
                    &input(&[
                        ("name", "build"),
                        ("content", "port 3000"),
                        ("description", "build"),
                    ]),
                    &profile,
                )
                .unwrap(),
                format!("Created build.md in {tier} and recorded its index description.")
            );
            assert_eq!(
                execute_tool(&roots, read, &input(&[("name", "build")]), &profile).unwrap(),
                "port 3000"
            );

            assert_eq!(
                execute_tool(
                    &roots,
                    edit,
                    &input(&[
                        ("name", "build.md"),
                        ("old_text", "3000"),
                        ("new_text", "4000"),
                        ("description", "build port"),
                    ]),
                    &profile,
                )
                .unwrap(),
                format!("Updated build.md in {tier} and refreshed its index description.")
            );
            assert_eq!(
                execute_tool(&roots, read, &input(&[("name", "build")]), &profile).unwrap(),
                "port 4000"
            );
        }

        // Both tiers now hold their own build.md, and the context shows both
        // index entries without either document body.
        let rendered = roots.render_context(&profile).unwrap();
        assert_eq!(
            rendered
                .matches("- [build.md](build.md) — build port")
                .count(),
            2
        );
        assert!(!rendered.contains("port 4000"));
    }

    #[test]
    fn an_unavailable_tier_fails_closed_instead_of_using_the_other_one() {
        let tree = TempTree::new("unavailable");
        let profile = PromptProfile::builtin_english();
        // No workspace: project memory is unavailable for the whole run.
        let roots = MemoryRoots::resolve(Some(&tree.root.join("home")), None);

        let error = execute_tool(
            &roots,
            "create_project_memory",
            &input(&[("name", "a"), ("content", "b"), ("description", "c")]),
            &profile,
        )
        .unwrap_err();
        assert!(error.contains("Project memory is unavailable"), "{error}");
        assert!(execute_tool(
            &roots,
            "read_project_memory",
            &input(&[("name", "a")]),
            &profile,
        )
        .is_err());

        // Nothing leaked into the global tier.
        assert!(execute_tool(
            &roots,
            "read_global_memory",
            &input(&[("name", "a")]),
            &profile,
        )
        .is_err());
        assert!(roots.render_context(&profile).is_none());

        // And the symmetric case: no home means no global tier.
        let project_only = MemoryRoots::resolve(None, Some(&tree.root.join("workspace")));
        let error = execute_tool(
            &project_only,
            "read_global_memory",
            &input(&[("name", "a")]),
            &profile,
        )
        .unwrap_err();
        assert!(error.contains("Global memory is unavailable"), "{error}");
    }

    #[test]
    fn the_dispatcher_rejects_missing_and_mistyped_arguments() {
        let tree = TempTree::new("arguments");
        let roots = MemoryRoots::resolve(Some(&tree.root.join("home")), None);
        let profile = PromptProfile::builtin_english();

        assert!(execute_tool(&roots, "read_global_memory", &input(&[]), &profile).is_err());
        // create requires a description so the index can never go stale.
        assert!(execute_tool(
            &roots,
            "create_global_memory",
            &input(&[("name", "a"), ("content", "b")]),
            &profile,
        )
        .is_err());

        let mut mistyped = serde_json::Map::new();
        mistyped.insert("name".into(), serde_json::Value::from(7));
        let error = execute_tool(&roots, "read_global_memory", &mistyped, &profile).unwrap_err();
        assert!(error.contains("must be a string"), "{error}");

        assert!(execute_tool(&roots, "memory_upsert", &input(&[]), &profile).is_err());
    }

    #[test]
    fn no_tool_can_address_the_host_owned_index() {
        let tree = TempTree::new("index-guard");
        let roots = MemoryRoots::resolve(Some(&tree.root.join("home")), None);
        let profile = PromptProfile::builtin_english();
        execute_tool(
            &roots,
            "create_global_memory",
            &input(&[
                ("name", "real"),
                ("content", "content"),
                ("description", "description"),
            ]),
            &profile,
        )
        .unwrap();
        let index_before = fs::read_to_string(roots.global.as_ref().unwrap().index_path()).unwrap();

        for spelling in ["MEMORY.md", "memory", "Memory.MD"] {
            assert!(execute_tool(
                &roots,
                "read_global_memory",
                &input(&[("name", spelling)]),
                &profile,
            )
            .is_err());
            assert!(execute_tool(
                &roots,
                "create_global_memory",
                &input(&[
                    ("name", spelling),
                    ("content", "hijack"),
                    ("description", "hijack")
                ]),
                &profile,
            )
            .is_err());
            assert!(execute_tool(
                &roots,
                "edit_global_memory",
                &input(&[
                    ("name", spelling),
                    ("old_text", "real"),
                    ("new_text", "hijack"),
                    ("description", "hijack"),
                ]),
                &profile,
            )
            .is_err());
        }

        assert_eq!(
            fs::read_to_string(roots.global.as_ref().unwrap().index_path()).unwrap(),
            index_before
        );
    }
}
