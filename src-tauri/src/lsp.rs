//! The `lsp` tool: nine code-navigation operations over whatever language
//! server claims the file.
//!
//! Everything a model reads here — the operation names, the request shapes, the
//! empty-result sentences, the grouped-by-file layout, the SymbolKind spellings
//! — is ported from Claude Code's `LSP` tool rather than invented, so a
//! transcript from one product reads the same in the other.
//!
//! Two behaviours are deliberately not verbatim, and both are marked where they
//! occur. Requests get a deadline: the source has none, because its tool runs
//! against an abort signal rather than a synchronous call slot. And the cases
//! where the tool could not reach an answer at all — no server claims the file,
//! the file is unreadable or larger than the 10 MB ceiling — fail the call,
//! where the source returns a successful result whose text explains the
//! failure. Mework already distinguishes those two outcomes on every other
//! tool, and a `success: true` card whose body is an error reads as a working
//! call in the timeline. An LSP answer that is legitimately empty ("no
//! references found") is still a success, exactly as in the source.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

use serde_json::{json, Value};

use crate::{
    lsp_config::LspServerConfig,
    lsp_servers::{uri_to_path, LspRegistry, ServerHost},
};

/// Largest file handed to a language server, matching the source's limit.
pub const MAX_FILE_BYTES: u64 = 10_000_000;

/// How long the gitignore filter waits on `git check-ignore`, and how many
/// paths one invocation carries. Both from the source.
pub const CHECK_IGNORE_TIMEOUT: Duration = Duration::from_secs(5);
const CHECK_IGNORE_BATCH: usize = 50;

/// How a call reaches the file it names and the repository around it.
///
/// The navigation logic is the same wherever the workspace is; what differs is
/// whether the text comes off this disk or over a shell transport, and whether
/// `git check-ignore` runs here or there. Both legs implement this.
pub trait LspFiles {
    /// The full text of the file the call names, read only when the server
    /// does not hold it yet.
    fn read_text(&self) -> Result<String, String>;

    /// `git check-ignore` over `paths`, all of them inside `root`: its stdout
    /// when it reported matches, `None` for any other outcome — git missing,
    /// not a repository, timeout — which keeps every result.
    fn check_ignore(&self, root: &Path, paths: &[String]) -> Option<String>;
}

/// The host leg: the file is on this disk and git runs here.
pub struct LocalFiles<'a> {
    /// The canonical path the path guard handed back.
    pub path: &'a Path,
    /// The path as the model wrote it, for messages.
    pub requested: &'a str,
}

impl LspFiles for LocalFiles<'_> {
    fn read_text(&self) -> Result<String, String> {
        let size = std::fs::metadata(self.path)
            .map_err(|error| format!("Cannot access file: {}. {error}", self.requested))?
            .len();
        if size > MAX_FILE_BYTES {
            return Err(too_large(size));
        }
        std::fs::read_to_string(self.path)
            .map_err(|error| format!("Cannot read file: {}. {error}", self.requested))
    }

    fn check_ignore(&self, root: &Path, paths: &[String]) -> Option<String> {
        let git = crate::environment_tools::resolve_on_path("git")?;
        if !root.is_dir() {
            return None;
        }
        check_ignore(&git, root, paths)
    }
}

/// The sentence for a file over the limit, sized as the source prints it.
pub fn too_large(size: u64) -> String {
    format!(
        "File too large for LSP analysis ({}MB exceeds 10MB limit)",
        size.div_ceil(1_000_000)
    )
}

/// The nine operations, in the order the schema lists them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
    GoToDefinition,
    FindReferences,
    Hover,
    DocumentSymbol,
    WorkspaceSymbol,
    GoToImplementation,
    PrepareCallHierarchy,
    IncomingCalls,
    OutgoingCalls,
}

impl Operation {
    pub const ALL: [Operation; 9] = [
        Operation::GoToDefinition,
        Operation::FindReferences,
        Operation::Hover,
        Operation::DocumentSymbol,
        Operation::WorkspaceSymbol,
        Operation::GoToImplementation,
        Operation::PrepareCallHierarchy,
        Operation::IncomingCalls,
        Operation::OutgoingCalls,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Operation::GoToDefinition => "goToDefinition",
            Operation::FindReferences => "findReferences",
            Operation::Hover => "hover",
            Operation::DocumentSymbol => "documentSymbol",
            Operation::WorkspaceSymbol => "workspaceSymbol",
            Operation::GoToImplementation => "goToImplementation",
            Operation::PrepareCallHierarchy => "prepareCallHierarchy",
            Operation::IncomingCalls => "incomingCalls",
            Operation::OutgoingCalls => "outgoingCalls",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|operation| operation.name() == value)
    }
}

/// One validated call.
pub struct LspCall {
    pub operation: Operation,
    /// The path as the model wrote it, for messages.
    pub requested_path: String,
    pub line: u64,
    pub character: u64,
    pub query: Option<String>,
}

/// Reads the tool arguments. Line and character are 1-based for every
/// operation, as the source's discriminated schema requires — including
/// `documentSymbol` and `workspaceSymbol`, which ignore them.
pub fn parse_call(
    operation: &str,
    file_path: String,
    line: u64,
    character: u64,
    query: Option<String>,
) -> Result<LspCall, String> {
    let Some(operation) = Operation::parse(operation) else {
        return Err(format!(
            "Unknown operation \"{operation}\". Valid operations are: {}",
            Operation::ALL
                .iter()
                .map(|value| value.name())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    };
    if line == 0 {
        return Err("line is 1-based, as shown in editors; it must be at least 1".into());
    }
    if character == 0 {
        return Err("character is 1-based, as shown in editors; it must be at least 1".into());
    }
    Ok(LspCall {
        operation,
        requested_path: file_path,
        line,
        character,
        query,
    })
}

/// Runs one call against the server that claims the file.
///
/// `path` is already resolved and guarded by the caller; `workspace` is the
/// root a server is started in when its configuration names none, and `host`
/// is the machine both are on. `files` is how the text and the repository are
/// reached there.
pub fn execute(
    registry: &LspRegistry,
    host: &ServerHost,
    configs: &[LspServerConfig],
    workspace: &Path,
    path: &Path,
    call: &LspCall,
    conversation_id: &str,
    files: &dyn LspFiles,
) -> Result<String, String> {
    let (connection, language_id, root) =
        registry.connection_for(host, configs, workspace, path)?;
    // A conversation that reached a language server is owed its diagnostics
    // from here on — for the root that server actually publishes under, so one
    // project's problems never land in another project's conversation, and so a
    // server with its own `workspaceFolder` is not registered against a root
    // nothing publishes to.
    registry.register_conversation(conversation_id, &root);

    if !connection.has_document(path) {
        let text = files.read_text()?;
        connection.sync_document(path, &language_id, &text)?;
    }

    let uri = crate::lsp_servers::path_to_uri(path);
    let (method, params) = request_for(call, &uri);
    let mut result = connection.request_with_retry(method, params)?;

    // Call hierarchy is two round trips: the position resolves to an item, and
    // the item is what the calls question is actually about.
    if matches!(
        call.operation,
        Operation::IncomingCalls | Operation::OutgoingCalls
    ) {
        let items = result.as_array().cloned().unwrap_or_default();
        let Some(item) = items.first() else {
            return Ok("No call hierarchy item found at this position".into());
        };
        let method = if call.operation == Operation::IncomingCalls {
            "callHierarchy/incomingCalls"
        } else {
            "callHierarchy/outgoingCalls"
        };
        result = connection.request_with_retry(method, json!({ "item": item }))?;
    }

    let result = filter_ignored(result, call.operation, workspace, files);
    Ok(format_result(call.operation, &result, workspace))
}

/// Operation to LSP method and params, including the 1-based to 0-based
/// conversion the schema's wording promises.
fn request_for(call: &LspCall, uri: &str) -> (&'static str, Value) {
    let document = json!({ "uri": uri });
    let position = json!({ "line": call.line - 1, "character": call.character - 1 });
    match call.operation {
        Operation::GoToDefinition => (
            "textDocument/definition",
            json!({ "textDocument": document, "position": position }),
        ),
        Operation::FindReferences => (
            "textDocument/references",
            json!({
                "textDocument": document,
                "position": position,
                "context": { "includeDeclaration": true },
            }),
        ),
        Operation::Hover => (
            "textDocument/hover",
            json!({ "textDocument": document, "position": position }),
        ),
        Operation::DocumentSymbol => (
            "textDocument/documentSymbol",
            json!({ "textDocument": document }),
        ),
        Operation::WorkspaceSymbol => (
            "workspace/symbol",
            json!({ "query": call.query.clone().unwrap_or_default() }),
        ),
        Operation::GoToImplementation => (
            "textDocument/implementation",
            json!({ "textDocument": document, "position": position }),
        ),
        // All three call-hierarchy operations start at the same request; the
        // two "calls" ones send a second one with the item it returns.
        Operation::PrepareCallHierarchy | Operation::IncomingCalls | Operation::OutgoingCalls => (
            "textDocument/prepareCallHierarchy",
            json!({ "textDocument": document, "position": position }),
        ),
    }
}

// ---------------------------------------------------------------------------
// gitignore filtering
// ---------------------------------------------------------------------------

/// Drops results that live in gitignored files.
///
/// A definition inside `target/` or `node_modules/` is almost never the answer
/// the question was about, and listing it crowds out the one that is. A path
/// outside the repository is never asked about, which is how a hit in a
/// toolchain source directory survives.
fn filter_ignored(result: Value, operation: Operation, root: &Path, files: &dyn LspFiles) -> Value {
    if !matches!(
        operation,
        Operation::GoToDefinition
            | Operation::FindReferences
            | Operation::GoToImplementation
            | Operation::WorkspaceSymbol
    ) {
        return result;
    }
    let Some(items) = result.as_array() else {
        return result;
    };
    if items.is_empty() {
        return result;
    }
    let uris: Vec<String> = items.iter().filter_map(uri_of).collect();
    if uris.is_empty() {
        return result;
    }
    let ignored = ignored_uris(&uris, root, files);
    if ignored.is_empty() {
        return result;
    }
    Value::Array(
        items
            .iter()
            .filter(|item| uri_of(item).is_none_or(|uri| !ignored.contains(&uri)))
            .cloned()
            .collect(),
    )
}

/// The URI of one result, whichever shape it arrived in.
fn uri_of(item: &Value) -> Option<String> {
    let direct = item
        .get("uri")
        .or_else(|| item.get("targetUri"))
        .or_else(|| item.get("location").and_then(|value| value.get("uri")));
    direct.and_then(Value::as_str).map(str::to_owned)
}

fn ignored_uris(
    uris: &[String],
    root: &Path,
    files: &dyn LspFiles,
) -> std::collections::HashSet<String> {
    let mut ignored = std::collections::HashSet::new();
    // One path may appear many times over; ask about each only once.
    let mut by_path: BTreeMap<String, Vec<&String>> = BTreeMap::new();
    for uri in uris {
        by_path.entry(uri_to_path(uri)).or_default().push(uri);
    }
    // Only paths inside the tree are asked about. `git check-ignore` does not
    // skip an argument it cannot place — it fatals on the whole invocation with
    // "is outside repository" and reports nothing, so a single definition in a
    // rustup toolchain or a node_modules elsewhere would turn the filter off
    // for the other forty-nine paths in its batch. Out-of-tree hits are kept
    // either way; this only makes the in-tree half work.
    let paths: Vec<String> = by_path
        .keys()
        .filter(|path| is_inside(Path::new(path), root))
        .cloned()
        .collect();
    for batch in paths.chunks(CHECK_IGNORE_BATCH) {
        let Some(output) = files.check_ignore(root, batch) else {
            continue;
        };
        for line in output.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Some(uris) = by_path.get(line) {
                for uri in uris {
                    ignored.insert((*uri).clone());
                }
            }
        }
    }
    ignored
}

/// Whether `path` is inside `root`, comparing components the way
/// [`pathdiff`] does — case-insensitively under a Windows root, where a
/// server's answer routinely spells the drive letter differently from the
/// workspace.
fn is_inside(path: &Path, root: &Path) -> bool {
    let fold = folds_case(root);
    let root: Vec<_> = root.components().collect();
    let path: Vec<_> = path.components().collect();
    if path.len() < root.len() {
        return false;
    }
    root.iter()
        .zip(path.iter())
        .all(|(left, right)| same_component(left, right, fold))
}

/// Whether paths under `root` compare case-insensitively.
///
/// Decided by the root's own shape rather than by the host: a drive or UNC
/// prefix is a Windows filesystem, a bare `/` is a POSIX one. The root of a
/// workspace on a Linux machine is `/srv/app` whichever host is looking at it,
/// and folding its case would put `/srv/App/x.rs` inside it — a path the
/// remote `git check-ignore` then refuses along with the whole batch.
fn folds_case(root: &Path) -> bool {
    matches!(
        root.components().next(),
        Some(std::path::Component::Prefix(_))
    )
}

fn same_component(
    left: &std::path::Component<'_>,
    right: &std::path::Component<'_>,
    fold: bool,
) -> bool {
    if fold {
        left.as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case(&right.as_os_str().to_string_lossy())
    } else {
        left == right
    }
}

/// Runs one `git check-ignore` batch, returning its stdout when it reported
/// matches. Any other outcome — git missing, not a repository, timeout — means
/// no filtering, which keeps every result.
fn check_ignore(git: &Path, root: &Path, paths: &[String]) -> Option<String> {
    use wait_timeout::ChildExt;

    let mut command = Command::new(git);
    command
        .arg("check-ignore")
        .args(paths)
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let mut child = command.spawn().ok()?;
    let status = match child.wait_timeout(CHECK_IGNORE_TIMEOUT) {
        Ok(Some(status)) => status,
        _ => {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
    };
    if !status.success() {
        return None;
    }
    let mut stdout = child.stdout.take()?;
    let mut text = String::new();
    std::io::Read::read_to_string(&mut stdout, &mut text).ok()?;
    Some(text)
}

// ---------------------------------------------------------------------------
// Formatting
// ---------------------------------------------------------------------------

const NO_DEFINITION: &str = "No definition found. This may occur if the cursor is not on a symbol, or if the definition is in an external library not indexed by the LSP server.";
const NO_REFERENCES: &str = "No references found. This may occur if the symbol has no usages, or if the LSP server has not fully indexed the workspace.";
const NO_HOVER: &str = "No hover information available. This may occur if the cursor is not on a symbol, or if the LSP server has not fully indexed the file.";
const NO_DOCUMENT_SYMBOLS: &str = "No symbols found in document. This may occur if the file is empty, not supported by the LSP server, or if the server has not fully indexed the file.";
const NO_WORKSPACE_SYMBOLS: &str = "No symbols found in workspace. This may occur if the workspace is empty, or if the LSP server has not finished indexing the project.";
const NO_CALL_HIERARCHY: &str = "No call hierarchy item found at this position";
const NO_INCOMING_CALLS: &str = "No incoming calls found (nothing calls this function)";
const NO_OUTGOING_CALLS: &str = "No outgoing calls found (this function calls nothing)";

/// `SymbolKind` as the protocol numbers it, in the source's spellings.
fn symbol_kind(kind: Option<&Value>) -> &'static str {
    match kind.and_then(Value::as_u64) {
        Some(1) => "File",
        Some(2) => "Module",
        Some(3) => "Namespace",
        Some(4) => "Package",
        Some(5) => "Class",
        Some(6) => "Method",
        Some(7) => "Property",
        Some(8) => "Field",
        Some(9) => "Constructor",
        Some(10) => "Enum",
        Some(11) => "Interface",
        Some(12) => "Function",
        Some(13) => "Variable",
        Some(14) => "Constant",
        Some(15) => "String",
        Some(16) => "Number",
        Some(17) => "Boolean",
        Some(18) => "Array",
        Some(19) => "Object",
        Some(20) => "Key",
        Some(21) => "Null",
        Some(22) => "EnumMember",
        Some(23) => "Struct",
        Some(24) => "Event",
        Some(25) => "Operator",
        Some(26) => "TypeParameter",
        _ => "Unknown",
    }
}

fn plural(count: usize, word: &str) -> String {
    if count == 1 {
        word.to_owned()
    } else {
        format!("{word}s")
    }
}

/// A `file://` URI as the result prints it: relative to the root when that is
/// shorter and does not climb out of the tree, absolute otherwise, always with
/// forward slashes.
fn format_uri(uri: &str, root: &Path) -> String {
    if uri.is_empty() {
        return "<unknown location>".to_owned();
    }
    let absolute = uri_to_path(uri);
    let relative = pathdiff(Path::new(&absolute), root).replace('\\', "/");
    if !relative.is_empty() && relative.len() < absolute.len() && !relative.starts_with("../../") {
        return relative;
    }
    absolute.replace('\\', "/")
}

/// `path` expressed against `base`, in the spelling Node's `path.relative`
/// produces. Empty when the two share no root, which keeps the absolute form.
///
/// Component comparison is case-insensitive under a Windows base, as
/// `path.relative` is there: a server that answers `C:\Users\...` for a
/// workspace spelled `C:\users\...` would otherwise never produce a relative
/// path. A POSIX base — a workspace on a Linux machine, whichever host is
/// looking — compares exactly.
fn pathdiff(path: &Path, base: &Path) -> String {
    let fold = folds_case(base);
    let same = |left: &std::path::Component<'_>, right: &std::path::Component<'_>| {
        same_component(left, right, fold)
    };

    let path: Vec<_> = path.components().collect();
    let base: Vec<_> = base.components().collect();
    match (path.first(), base.first()) {
        (Some(left), Some(right)) if same(left, right) => {}
        _ => return String::new(),
    }
    let shared = path
        .iter()
        .zip(base.iter())
        .take_while(|(left, right)| same(left, right))
        .count();
    let mut parts: Vec<String> = std::iter::repeat_n("..".to_owned(), base.len() - shared).collect();
    parts.extend(
        path[shared..]
            .iter()
            .map(|part| part.as_os_str().to_string_lossy().into_owned()),
    );
    parts.join("/")
}

/// `file:line:character`, 1-based, as every location in the output prints.
fn format_location(item: &Value, root: &Path) -> String {
    let uri = uri_of(item).unwrap_or_default();
    let range = range_of(item);
    let line = range
        .and_then(|range| range.get("start"))
        .and_then(|start| start.get("line"))
        .and_then(Value::as_u64)
        .unwrap_or(0)
        + 1;
    let character = range
        .and_then(|range| range.get("start"))
        .and_then(|start| start.get("character"))
        .and_then(Value::as_u64)
        .unwrap_or(0)
        + 1;
    format!("{}:{line}:{character}", format_uri(&uri, root))
}

/// The range of a result, taking a `LocationLink`'s selection range in
/// preference to its full range, as the source's normalizer does.
fn range_of(item: &Value) -> Option<&Value> {
    if item.get("targetUri").is_some() {
        return item
            .get("targetSelectionRange")
            .or_else(|| item.get("targetRange"));
    }
    item.get("range")
        .or_else(|| item.get("location").and_then(|value| value.get("range")))
}

fn start_line(item: &Value) -> u64 {
    range_of(item)
        .and_then(|range| range.get("start"))
        .and_then(|start| start.get("line"))
        .and_then(Value::as_u64)
        .unwrap_or(0)
        + 1
}

/// Groups results by the file they are in, preserving first-seen file order.
fn group_by_file(items: &[Value], root: &Path) -> Vec<(String, Vec<Value>)> {
    let mut order: Vec<String> = Vec::new();
    let mut groups: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    for item in items {
        let file = format_uri(&uri_of(item).unwrap_or_default(), root);
        if !groups.contains_key(&file) {
            order.push(file.clone());
        }
        groups.entry(file).or_default().push(item.clone());
    }
    order
        .into_iter()
        .filter_map(|file| groups.remove(&file).map(|items| (file, items)))
        .collect()
}

/// Renders one LSP response into the text the model reads.
pub fn format_result(operation: Operation, value: &Value, root: &Path) -> String {
    match operation {
        Operation::GoToDefinition | Operation::GoToImplementation => {
            format_definition(value, root)
        }
        Operation::FindReferences => format_references(value, root),
        Operation::Hover => format_hover(value, root),
        Operation::DocumentSymbol => format_document_symbols(value, root),
        Operation::WorkspaceSymbol => format_workspace_symbols(value, root),
        Operation::PrepareCallHierarchy => format_call_hierarchy(value, root),
        Operation::IncomingCalls => format_calls(value, root, true),
        Operation::OutgoingCalls => format_calls(value, root, false),
    }
}

fn format_definition(value: &Value, root: &Path) -> String {
    if value.is_null() {
        return NO_DEFINITION.to_owned();
    }
    let Some(items) = value.as_array() else {
        return format!("Defined in {}", format_location(value, root));
    };
    let valid: Vec<&Value> = items
        .iter()
        .filter(|item| uri_of(item).is_some_and(|uri| !uri.is_empty()))
        .collect();
    match valid.len() {
        0 => NO_DEFINITION.to_owned(),
        1 => format!("Defined in {}", format_location(valid[0], root)),
        count => {
            let lines = valid
                .iter()
                .map(|item| format!("  {}", format_location(item, root)))
                .collect::<Vec<_>>()
                .join("\n");
            format!("Found {count} definitions:\n{lines}")
        }
    }
}

fn format_references(value: &Value, root: &Path) -> String {
    let items: Vec<Value> = value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter(|item| uri_of(item).is_some_and(|uri| !uri.is_empty()))
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    if items.is_empty() {
        return NO_REFERENCES.to_owned();
    }
    if items.len() == 1 {
        return format!(
            "Found 1 reference:\n  {}",
            format_location(&items[0], root)
        );
    }
    let groups = group_by_file(&items, root);
    let mut lines = vec![format!(
        "Found {} references across {} files:",
        items.len(),
        groups.len()
    )];
    for (file, entries) in groups {
        lines.push(format!("\n{file}:"));
        for entry in entries {
            let range = range_of(&entry).and_then(|range| range.get("start"));
            let line = range
                .and_then(|start| start.get("line"))
                .and_then(Value::as_u64)
                .unwrap_or(0)
                + 1;
            let character = range
                .and_then(|start| start.get("character"))
                .and_then(Value::as_u64)
                .unwrap_or(0)
                + 1;
            lines.push(format!("  Line {line}:{character}"));
        }
    }
    lines.join("\n")
}

fn format_hover(value: &Value, _root: &Path) -> String {
    if value.is_null() {
        return NO_HOVER.to_owned();
    }
    let contents = match value.get("contents") {
        None | Some(Value::Null) => return NO_HOVER.to_owned(),
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| match item {
                Value::String(text) => text.clone(),
                other => other
                    .get("value")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            })
            .collect::<Vec<_>>()
            .join("\n\n"),
        Some(other) => other
            .get("value")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
    };
    match value.get("range").and_then(|range| range.get("start")) {
        Some(start) => {
            let line = start.get("line").and_then(Value::as_u64).unwrap_or(0) + 1;
            let character = start.get("character").and_then(Value::as_u64).unwrap_or(0) + 1;
            format!("Hover info at {line}:{character}:\n\n{contents}")
        }
        None => contents,
    }
}

fn format_document_symbols(value: &Value, root: &Path) -> String {
    let items = value.as_array().cloned().unwrap_or_default();
    if items.is_empty() {
        return NO_DOCUMENT_SYMBOLS.to_owned();
    }
    // A server that answers with `SymbolInformation` rather than the
    // hierarchical `DocumentSymbol` is formatted as a workspace symbol list,
    // which is the shape that carries a location.
    if items[0].get("location").is_some() {
        return format_workspace_symbols(value, root);
    }
    let mut lines = vec!["Document symbols:".to_owned()];
    for item in &items {
        push_symbol_tree(&mut lines, item, 0);
    }
    lines.join("\n")
}

fn push_symbol_tree(lines: &mut Vec<String>, symbol: &Value, depth: usize) {
    let indent = "  ".repeat(depth);
    let name = symbol.get("name").and_then(Value::as_str).unwrap_or_default();
    let kind = symbol_kind(symbol.get("kind"));
    let mut line = format!("{indent}{name} ({kind})");
    if let Some(detail) = symbol.get("detail").and_then(Value::as_str) {
        if !detail.is_empty() {
            line.push_str(&format!(" {detail}"));
        }
    }
    line.push_str(&format!(" - Line {}", start_line(symbol)));
    lines.push(line);
    if let Some(children) = symbol.get("children").and_then(Value::as_array) {
        for child in children {
            push_symbol_tree(lines, child, depth + 1);
        }
    }
}

fn format_workspace_symbols(value: &Value, root: &Path) -> String {
    let items: Vec<Value> = value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter(|item| {
                    item.get("location")
                        .and_then(|location| location.get("uri"))
                        .and_then(Value::as_str)
                        .is_some_and(|uri| !uri.is_empty())
                })
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    if items.is_empty() {
        return NO_WORKSPACE_SYMBOLS.to_owned();
    }
    let mut lines = vec![format!(
        "Found {} {} in workspace:",
        items.len(),
        plural(items.len(), "symbol")
    )];
    for (file, entries) in group_by_file(&items, root) {
        lines.push(format!("\n{file}:"));
        for entry in entries {
            let name = entry.get("name").and_then(Value::as_str).unwrap_or_default();
            let kind = symbol_kind(entry.get("kind"));
            let mut line = format!("  {name} ({kind}) - Line {}", start_line(&entry));
            if let Some(container) = entry.get("containerName").and_then(Value::as_str) {
                if !container.is_empty() {
                    line.push_str(&format!(" in {container}"));
                }
            }
            lines.push(line);
        }
    }
    lines.join("\n")
}

/// One `CallHierarchyItem`, as both the prepare and the calls formatters print
/// it.
fn format_hierarchy_item(item: &Value, root: &Path) -> String {
    let name = item.get("name").and_then(Value::as_str).unwrap_or_default();
    let kind = symbol_kind(item.get("kind"));
    let Some(uri) = item
        .get("uri")
        .and_then(Value::as_str)
        .filter(|uri| !uri.is_empty())
    else {
        return format!("{name} ({kind}) - <unknown location>");
    };
    let mut line = format!(
        "{name} ({kind}) - {}:{}",
        format_uri(uri, root),
        start_line(item)
    );
    if let Some(detail) = item.get("detail").and_then(Value::as_str) {
        if !detail.is_empty() {
            line.push_str(&format!(" [{detail}]"));
        }
    }
    line
}

fn format_call_hierarchy(value: &Value, root: &Path) -> String {
    let items = value.as_array().cloned().unwrap_or_default();
    match items.len() {
        0 => NO_CALL_HIERARCHY.to_owned(),
        1 => format!(
            "Call hierarchy item: {}",
            format_hierarchy_item(&items[0], root)
        ),
        count => {
            let mut lines = vec![format!("Found {count} call hierarchy items:")];
            for item in &items {
                lines.push(format!("  {}", format_hierarchy_item(item, root)));
            }
            lines.join("\n")
        }
    }
}

fn format_calls(value: &Value, root: &Path, incoming: bool) -> String {
    let items = value.as_array().cloned().unwrap_or_default();
    if items.is_empty() {
        return if incoming {
            NO_INCOMING_CALLS.to_owned()
        } else {
            NO_OUTGOING_CALLS.to_owned()
        };
    }
    let side = if incoming { "from" } else { "to" };
    let direction = if incoming { "incoming" } else { "outgoing" };
    let ranges_label = if incoming { "calls at" } else { "called from" };
    let mut lines = vec![format!(
        "Found {} {direction} {}:",
        items.len(),
        plural(items.len(), "call")
    )];

    let mut order: Vec<String> = Vec::new();
    let mut groups: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    for entry in &items {
        let Some(peer) = entry.get(side) else { continue };
        let file = format_uri(
            peer.get("uri").and_then(Value::as_str).unwrap_or_default(),
            root,
        );
        if !groups.contains_key(&file) {
            order.push(file.clone());
        }
        groups.entry(file).or_default().push(entry.clone());
    }

    for file in order {
        let Some(entries) = groups.remove(&file) else {
            continue;
        };
        lines.push(format!("\n{file}:"));
        for entry in entries {
            let Some(peer) = entry.get(side) else { continue };
            let name = peer.get("name").and_then(Value::as_str).unwrap_or_default();
            let kind = symbol_kind(peer.get("kind"));
            let mut line = format!("  {name} ({kind}) - Line {}", start_line(peer));
            if let Some(ranges) = entry.get("fromRanges").and_then(Value::as_array) {
                if !ranges.is_empty() {
                    let positions = ranges
                        .iter()
                        .map(|range| {
                            let start = range.get("start");
                            let line = start
                                .and_then(|start| start.get("line"))
                                .and_then(Value::as_u64)
                                .unwrap_or(0)
                                + 1;
                            let character = start
                                .and_then(|start| start.get("character"))
                                .and_then(Value::as_u64)
                                .unwrap_or(0)
                                + 1;
                            format!("{line}:{character}")
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                    line.push_str(&format!(" [{ranges_label}: {positions}]"));
                }
            }
            lines.push(line);
        }
    }
    lines.join("\n")
}

/// The extension the tool would route on, for callers that only need to know
/// whether a path is claimed at all.
pub fn claims_path(configs: &[LspServerConfig], path: &Path) -> bool {
    LspRegistry::config_for_path(configs, path).is_some()
}

/// Resolves a file path the way the tool does, for the edit hook.
///
/// "The way the tool does" includes canonicalizing, because the navigation leg
/// gets its path from the path guard and the document map is keyed by the URI
/// built from it. Resolving only by joining leaves the edit hook asking about
/// `C:\work\.\a.rs` while the server holds `C:\work\a.rs`: `has_document` says
/// no, the hook returns without re-syncing, and the next navigation call finds
/// the document already open and does not re-sync either — so every answer
/// afterwards describes the file as it was before the edit. A path that cannot
/// be canonicalized keeps the joined form; the miss it causes is silent either
/// way.
pub fn resolved_workspace_path(workspace: &Path, path: &str) -> PathBuf {
    let candidate = Path::new(path);
    let joined = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        workspace.join(candidate)
    };
    std::fs::canonicalize(&joined).unwrap_or(joined)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> PathBuf {
        PathBuf::from(if cfg!(windows) {
            "C:/work/project"
        } else {
            "/work/project"
        })
    }

    fn uri(relative: &str) -> String {
        if cfg!(windows) {
            format!("file:///C:/work/project/{relative}")
        } else {
            format!("file:///work/project/{relative}")
        }
    }

    #[test]
    fn every_operation_name_round_trips() {
        for operation in Operation::ALL {
            assert_eq!(Operation::parse(operation.name()), Some(operation));
        }
        assert!(Operation::parse("gotoDefinition").is_none());
    }

    #[test]
    fn positions_are_converted_from_one_based_to_zero_based() {
        let call = parse_call(
            "goToDefinition",
            "src/main.rs".into(),
            10,
            5,
            None,
        )
        .expect("valid");
        let (method, params) = request_for(&call, "file:///a.rs");
        assert_eq!(method, "textDocument/definition");
        assert_eq!(params["position"], json!({ "line": 9, "character": 4 }));
    }

    #[test]
    fn a_zero_position_is_refused_because_the_schema_promises_editor_numbering() {
        assert!(parse_call("hover", "a.rs".into(), 0, 1, None).is_err());
        assert!(parse_call("hover", "a.rs".into(), 1, 0, None).is_err());
    }

    #[test]
    fn find_references_asks_for_the_declaration_too() {
        let call = parse_call("findReferences", "a.rs".into(), 1, 1, None).unwrap();
        let (_, params) = request_for(&call, "file:///a.rs");
        assert_eq!(params["context"]["includeDeclaration"], json!(true));
    }

    #[test]
    fn workspace_symbol_sends_an_empty_query_rather_than_omitting_it() {
        let call = parse_call("workspaceSymbol", "a.rs".into(), 1, 1, None).unwrap();
        let (method, params) = request_for(&call, "file:///a.rs");
        assert_eq!(method, "workspace/symbol");
        assert_eq!(params["query"], json!(""));
    }

    #[test]
    fn all_three_call_hierarchy_operations_start_at_prepare() {
        for name in ["prepareCallHierarchy", "incomingCalls", "outgoingCalls"] {
            let call = parse_call(name, "a.rs".into(), 2, 3, None).unwrap();
            let (method, _) = request_for(&call, "file:///a.rs");
            assert_eq!(
                method, "textDocument/prepareCallHierarchy",
                "{name} must resolve an item first"
            );
        }
    }

    #[test]
    fn a_single_definition_prints_one_line() {
        let value = json!([{ "uri": uri("src/lib.rs"), "range": { "start": { "line": 41, "character": 7 } } }]);
        assert_eq!(
            format_result(Operation::GoToDefinition, &value, &root()),
            "Defined in src/lib.rs:42:8"
        );
    }

    #[test]
    fn a_location_link_uses_its_selection_range() {
        let value = json!({
            "targetUri": uri("src/lib.rs"),
            "targetRange": { "start": { "line": 100, "character": 0 } },
            "targetSelectionRange": { "start": { "line": 41, "character": 7 } },
        });
        assert_eq!(
            format_result(Operation::GoToDefinition, &value, &root()),
            "Defined in src/lib.rs:42:8"
        );
    }

    #[test]
    fn no_definition_says_why_it_might_be_missing() {
        assert_eq!(
            format_result(Operation::GoToDefinition, &Value::Null, &root()),
            NO_DEFINITION
        );
        assert_eq!(
            format_result(Operation::GoToDefinition, &json!([]), &root()),
            NO_DEFINITION
        );
    }

    #[test]
    fn go_to_implementation_shares_the_definition_wording() {
        // The source formats both with one function, so an implementation with
        // no result says "No definition found". Preserved deliberately.
        assert_eq!(
            format_result(Operation::GoToImplementation, &Value::Null, &root()),
            NO_DEFINITION
        );
    }

    #[test]
    fn one_reference_and_many_references_have_different_shapes() {
        let single = json!([{ "uri": uri("a.rs"), "range": { "start": { "line": 0, "character": 0 } } }]);
        assert_eq!(
            format_result(Operation::FindReferences, &single, &root()),
            "Found 1 reference:\n  a.rs:1:1"
        );

        let many = json!([
            { "uri": uri("a.rs"), "range": { "start": { "line": 0, "character": 0 } } },
            { "uri": uri("a.rs"), "range": { "start": { "line": 4, "character": 2 } } },
            { "uri": uri("b.rs"), "range": { "start": { "line": 9, "character": 1 } } },
        ]);
        assert_eq!(
            format_result(Operation::FindReferences, &many, &root()),
            "Found 3 references across 2 files:\n\na.rs:\n  Line 1:1\n  Line 5:3\n\nb.rs:\n  Line 10:2"
        );
    }

    #[test]
    fn hover_prints_its_position_when_the_server_gives_a_range() {
        let value = json!({
            "contents": { "kind": "markdown", "value": "fn main()" },
            "range": { "start": { "line": 2, "character": 0 } },
        });
        assert_eq!(
            format_result(Operation::Hover, &value, &root()),
            "Hover info at 3:1:\n\nfn main()"
        );
        let rangeless = json!({ "contents": ["one", { "value": "two" }] });
        assert_eq!(
            format_result(Operation::Hover, &rangeless, &root()),
            "one\n\ntwo"
        );
        assert_eq!(
            format_result(Operation::Hover, &Value::Null, &root()),
            NO_HOVER
        );
    }

    #[test]
    fn document_symbols_are_indented_by_nesting_depth() {
        let value = json!([{
            "name": "Server",
            "kind": 5,
            "detail": "struct",
            "range": { "start": { "line": 9, "character": 0 } },
            "children": [{
                "name": "start",
                "kind": 6,
                "range": { "start": { "line": 11, "character": 4 } },
            }],
        }]);
        assert_eq!(
            format_result(Operation::DocumentSymbol, &value, &root()),
            "Document symbols:\nServer (Class) struct - Line 10\n  start (Method) - Line 12"
        );
    }

    #[test]
    fn a_flat_symbol_answer_falls_back_to_the_workspace_layout() {
        let value = json!([{
            "name": "main",
            "kind": 12,
            "location": { "uri": uri("a.rs"), "range": { "start": { "line": 0, "character": 0 } } },
        }]);
        let rendered = format_result(Operation::DocumentSymbol, &value, &root());
        assert!(rendered.starts_with("Found 1 symbol in workspace:"), "{rendered}");
    }

    #[test]
    fn workspace_symbols_pluralize_and_name_their_container() {
        let value = json!([
            {
                "name": "run",
                "kind": 6,
                "containerName": "Server",
                "location": { "uri": uri("a.rs"), "range": { "start": { "line": 4, "character": 0 } } },
            },
            {
                "name": "run",
                "kind": 12,
                "location": { "uri": uri("b.rs"), "range": { "start": { "line": 1, "character": 0 } } },
            },
        ]);
        assert_eq!(
            format_result(Operation::WorkspaceSymbol, &value, &root()),
            "Found 2 symbols in workspace:\n\na.rs:\n  run (Method) - Line 5 in Server\n\nb.rs:\n  run (Function) - Line 2"
        );
    }

    #[test]
    fn call_hierarchy_prints_one_item_or_a_list() {
        let one = json!([{
            "name": "main",
            "kind": 12,
            "detail": "fn()",
            "uri": uri("a.rs"),
            "range": { "start": { "line": 0, "character": 0 } },
        }]);
        assert_eq!(
            format_result(Operation::PrepareCallHierarchy, &one, &root()),
            "Call hierarchy item: main (Function) - a.rs:1 [fn()]"
        );
        assert_eq!(
            format_result(Operation::PrepareCallHierarchy, &json!([]), &root()),
            NO_CALL_HIERARCHY
        );
    }

    #[test]
    fn incoming_and_outgoing_calls_label_their_ranges_differently() {
        let incoming = json!([{
            "from": {
                "name": "caller",
                "kind": 12,
                "uri": uri("a.rs"),
                "range": { "start": { "line": 6, "character": 0 } },
            },
            "fromRanges": [{ "start": { "line": 7, "character": 8 } }],
        }]);
        assert_eq!(
            format_result(Operation::IncomingCalls, &incoming, &root()),
            "Found 1 incoming call:\n\na.rs:\n  caller (Function) - Line 7 [calls at: 8:9]"
        );

        let outgoing = json!([{
            "to": {
                "name": "callee",
                "kind": 12,
                "uri": uri("b.rs"),
                "range": { "start": { "line": 2, "character": 0 } },
            },
            "fromRanges": [{ "start": { "line": 9, "character": 0 } }],
        }]);
        assert_eq!(
            format_result(Operation::OutgoingCalls, &outgoing, &root()),
            "Found 1 outgoing call:\n\nb.rs:\n  callee (Function) - Line 3 [called from: 10:1]"
        );

        assert_eq!(
            format_result(Operation::IncomingCalls, &json!([]), &root()),
            NO_INCOMING_CALLS
        );
        assert_eq!(
            format_result(Operation::OutgoingCalls, &json!([]), &root()),
            NO_OUTGOING_CALLS
        );
    }

    #[test]
    fn a_path_outside_the_root_keeps_its_absolute_form() {
        let outside = if cfg!(windows) {
            "file:///C:/other/place/x.rs"
        } else {
            "file:///other/place/x.rs"
        };
        let rendered = format_uri(outside, &root());
        assert!(
            rendered.starts_with('/') || rendered.starts_with("C:/"),
            "a far-away path stays absolute rather than becoming ../../..: {rendered}"
        );
    }

    #[test]
    fn a_symbol_kind_outside_the_table_is_unknown_rather_than_a_number() {
        assert_eq!(symbol_kind(Some(&json!(99))), "Unknown");
        assert_eq!(symbol_kind(None), "Unknown");
        assert_eq!(symbol_kind(Some(&json!(23))), "Struct");
    }

    #[test]
    fn only_location_bearing_operations_are_gitignore_filtered() {
        struct NoGit;
        impl LspFiles for NoGit {
            fn read_text(&self) -> Result<String, String> {
                unreachable!("nothing is read by the filter")
            }
            fn check_ignore(&self, _root: &Path, _paths: &[String]) -> Option<String> {
                unreachable!("nothing is asked about when no result has a URI")
            }
        }
        let value = json!([{ "name": "x" }]);
        // No URIs, so nothing to ask git about and nothing removed.
        for operation in Operation::ALL {
            assert_eq!(
                filter_ignored(value.clone(), operation, &root(), &NoGit),
                value,
                "{} must not drop results it cannot address",
                operation.name()
            );
        }
    }

    /// Case folds under a Windows root and nowhere else: a POSIX workspace on
    /// a Linux machine is spelled the same from a Windows host, and folding it
    /// would put `/srv/App/x.rs` inside `/srv/app`.
    #[test]
    fn case_folding_follows_the_root_not_the_host() {
        let posix = Path::new("/srv/app");
        assert!(!folds_case(posix));
        assert!(is_inside(Path::new("/srv/app/src/x.rs"), posix));
        assert!(!is_inside(Path::new("/srv/App/src/x.rs"), posix));
        assert_eq!(pathdiff(Path::new("/srv/app/src/x.rs"), posix), "src/x.rs");
        assert_eq!(pathdiff(Path::new("/srv/App/src/x.rs"), posix), "../App/src/x.rs");

        if cfg!(windows) {
            let windows = Path::new("C:/work/project");
            assert!(folds_case(windows));
            assert!(is_inside(Path::new("c:/Work/project/x.rs"), windows));
            assert_eq!(pathdiff(Path::new("c:/Work/project/x.rs"), windows), "x.rs");
        }
    }

    /// The filter asks about in-tree paths only, in the order they were seen,
    /// and drops exactly the ones the answer names — through whichever machine's
    /// git the seam runs.
    #[test]
    fn ignored_results_are_dropped_through_the_files_seam() {
        struct IgnoresTarget(std::cell::RefCell<Vec<Vec<String>>>);
        impl LspFiles for IgnoresTarget {
            fn read_text(&self) -> Result<String, String> {
                unreachable!()
            }
            fn check_ignore(&self, _root: &Path, paths: &[String]) -> Option<String> {
                self.0.borrow_mut().push(paths.to_vec());
                let ignored: Vec<&String> = paths
                    .iter()
                    .filter(|path| path.contains("/target/"))
                    .collect();
                (!ignored.is_empty()).then(|| {
                    ignored
                        .into_iter()
                        .map(|path| format!("{path}\n"))
                        .collect::<String>()
                })
            }
        }
        let files = IgnoresTarget(std::cell::RefCell::new(Vec::new()));
        let value = json!([
            { "uri": uri("src/a.rs"), "range": { "start": { "line": 0, "character": 0 } } },
            { "uri": uri("target/debug/b.rs"), "range": { "start": { "line": 0, "character": 0 } } },
            { "uri": "file:///elsewhere/c.rs", "range": { "start": { "line": 0, "character": 0 } } },
        ]);
        let kept = filter_ignored(value, Operation::FindReferences, &root(), &files);
        let uris: Vec<String> = kept
            .as_array()
            .unwrap()
            .iter()
            .filter_map(uri_of)
            .collect();
        assert_eq!(uris, [uri("src/a.rs"), "file:///elsewhere/c.rs".to_owned()]);
        let asked = files.0.borrow();
        assert_eq!(asked.len(), 1, "one batch");
        assert!(
            asked[0].iter().all(|path| !path.contains("elsewhere")),
            "out-of-tree paths are never asked about: {:?}",
            asked[0]
        );
    }

    /// The document map is keyed by URI, and two legs reach it: navigation
    /// through the path guard, and the edit hook through
    /// [`resolved_workspace_path`]. If they spell one file two ways the hook's
    /// `has_document` misses, nothing is re-synced, and every later answer
    /// describes the file as it was before the edit.
    #[test]
    fn the_edit_hook_and_the_path_guard_name_one_file_the_same_way() {
        let workspace = std::env::temp_dir().join(format!(
            "mework-lsp-identity-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock is after the epoch")
                .as_nanos()
        ));
        std::fs::create_dir_all(workspace.join("src")).expect("probe directory");
        let file = workspace.join("src").join("a.rs");
        std::fs::write(&file, "fn a() {}\n").expect("probe source");

        // What `run_lsp` hands `execute`, and what the hook resolves for the
        // same file reached by three different spellings.
        let guarded = crate::lsp_servers::path_to_uri(
            &std::fs::canonicalize(&file).expect("the probe source resolves"),
        );
        for spelling in ["src/a.rs", "./src/a.rs", "src\\a.rs"] {
            assert_eq!(
                crate::lsp_servers::path_to_uri(&resolved_workspace_path(&workspace, spelling)),
                guarded,
                "the edit hook must reach the same document as navigation for {spelling}"
            );
        }

        let _ = std::fs::remove_dir_all(&workspace);
    }

    #[test]
    fn a_result_uri_is_recognized_in_all_three_shapes() {
        assert_eq!(
            uri_of(&json!({ "uri": "file:///a" })).as_deref(),
            Some("file:///a")
        );
        assert_eq!(
            uri_of(&json!({ "targetUri": "file:///b" })).as_deref(),
            Some("file:///b")
        );
        assert_eq!(
            uri_of(&json!({ "location": { "uri": "file:///c" } })).as_deref(),
            Some("file:///c")
        );
        assert!(uri_of(&json!({ "name": "x" })).is_none());
    }

    /// End to end against a real `rust-analyzer`: spawn, handshake, sync a
    /// document, and navigate.
    ///
    /// Ignored because it needs `rust-analyzer` on PATH and takes tens of
    /// seconds to index even a two-file crate. It is the only test that
    /// exercises the Content-Length codec, the `initialize` capabilities, the
    /// server-to-client request answers, and `textDocument/didOpen` against
    /// something that will actually complain if they are wrong:
    ///
    /// ```text
    /// cargo test --lib -- lsp::tests::navigates_a_real_crate_with_rust_analyzer --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "needs rust-analyzer on PATH and indexes a temporary crate"]
    fn navigates_a_real_crate_with_rust_analyzer() {
        use crate::lsp_servers::LspRegistry;
        use std::collections::BTreeMap;

        let Some(_) = crate::environment_tools::resolve_on_path("rust-analyzer") else {
            panic!("rust-analyzer is not on PATH; this test cannot run here");
        };

        let workspace = std::env::temp_dir().join(format!(
            "mework-lsp-probe-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock is after the epoch")
                .as_nanos()
        ));
        std::fs::create_dir_all(workspace.join("src")).expect("probe crate directory");
        std::fs::write(
            workspace.join("Cargo.toml"),
            "[package]\nname = \"probe\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n[workspace]\n",
        )
        .expect("probe manifest");
        // Line 1 defines `helper`; line 6 column 17 is the `h` of the call.
        let main = "fn helper() -> i32 {\n    7\n}\n\nfn main() {\n    let value = helper();\n    println!(\"{value}\");\n}\n";
        let source = workspace.join("src").join("main.rs");
        std::fs::write(&source, main).expect("probe source");
        // What `run_lsp` hands `execute`: the path guard's answer, which on
        // Windows is the extended-length form `\\?\C:\…`. Navigating the joined
        // path instead tests a shape production never produces, and that is how
        // `file:////?/C:/…` reached real users — `rust-analyzer` answers every
        // request naming one with `-32603 url is not a file`.
        let source = std::fs::canonicalize(&source).expect("the probe source resolves");

        let config = crate::lsp_config::LspServerConfig {
            id: "probe".into(),
            name: "rust-analyzer".into(),
            description: String::new(),
            command: "rust-analyzer".into(),
            args: Vec::new(),
            env: BTreeMap::new(),
            extension_to_language: [(".rs".to_owned(), "rust".to_owned())]
                .into_iter()
                .collect(),
            initialization_options: None,
            settings: None,
            workspace_folder: String::new(),
            startup_timeout_millis: 120_000,
            shutdown_timeout_millis: 5_000,
            restart_on_crash: false,
            max_restarts: 0,
            diagnostics: true,
        };
        let registry = LspRegistry::default();
        let configs = vec![config];

        // rust-analyzer answers `initialize` long before it has loaded the
        // crate graph, so the first few navigation requests legitimately come
        // back empty. Retry until it has indexed, then assert.
        let call = parse_call("goToDefinition", "src/main.rs".into(), 6, 17, None)
            .expect("the call is well formed");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(180);
        let last = loop {
            let answer = execute(
                &registry,
                &ServerHost::Local,
                &configs,
                &workspace,
                &source,
                &call,
                "probe-conversation",
                &LocalFiles {
                    path: &source,
                    requested: "src/main.rs",
                },
            )
            .expect("the request reaches the server");
            if answer.starts_with("Defined in") || std::time::Instant::now() >= deadline {
                break answer;
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        };
        registry.stop_all();
        let _ = std::fs::remove_dir_all(&workspace);

        assert!(
            last.starts_with("Defined in"),
            "rust-analyzer should resolve `helper` to its definition; got: {last}"
        );
        assert!(
            last.contains("src/main.rs:1:4"),
            "the definition is `helper` on line 1, column 4 (1-based); got: {last}"
        );
    }
}
