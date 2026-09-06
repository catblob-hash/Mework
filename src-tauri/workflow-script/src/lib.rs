#![deny(unsafe_code)]
// `deny` permits the narrowly scoped read-only JSValue tag access in engine.rs.
#![allow(clippy::module_name_repetitions)]

//! workflow-script: the rquickjs implementation of the workflow-core
//! [`StepSource`] boundary.
//!
//! Script parsing validates metadata and syntax before dispatch. Runtime state is
//! created only on the driver worker thread because rquickjs Runtime and Context
//! are not `Send`.

mod engine;

pub use engine::ScriptSource;

use serde_json::Value;
use workflow_core::StepRolePolicy;
use workflow_core::WorkflowError;
use workflow_core::MAX_SCRIPT_BYTES;

/// Maximum synchronous JavaScript execution time per `advance`.
///
/// Scripts must only assemble prompts and classify results. The limit prevents an
/// infinite JavaScript loop from monopolizing the driver worker.
pub const SCRIPT_SYNC_SLICE_MS: u64 = 5_000;

/// Heap-memory limit for the script engine.
pub const SCRIPT_MEMORY_LIMIT_BYTES: usize = 64 * 1024 * 1024;

/// A `meta.phases` entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MetaPhase {
    pub title: String,
    pub detail: Option<String>,
}

/// Validated `export const meta` literal from a script header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScriptMeta {
    pub name: String,
    pub description: String,
    pub when_to_use: Option<String>,
    pub phases: Vec<MetaPhase>,
}

/// A script specification that passed pre-dispatch validation.
///
/// It can cross threads because it contains only text and JSON. The engine is
/// created by [`ScriptSpec::start`].
#[derive(Clone, Debug)]
pub struct ScriptSpec {
    /// Script body with the meta statement blanked while preserving line numbers.
    body: String,
    meta: ScriptMeta,
    args: Option<Value>,
    budget_total: Option<u64>,
    role_policy: StepRolePolicy,
}

impl ScriptSpec {
    /// Perform all pre-dispatch validation on the dispatch thread.
    pub fn parse(
        source: &str,
        args: Option<Value>,
        budget_total: Option<u64>,
        role_policy: StepRolePolicy,
    ) -> Result<(ScriptMeta, Self), WorkflowError> {
        if source.len() > MAX_SCRIPT_BYTES {
            return Err(WorkflowError::Invalid(format!(
                "Script is {} bytes, exceeding the limit of {MAX_SCRIPT_BYTES}",
                source.len()
            )));
        }
        if source.trim().is_empty() {
            return Err(WorkflowError::Invalid("Script must not be empty".into()));
        }
        let (meta_literal, body) = extract_meta(source)?;
        let meta = engine::evaluate_meta(&meta_literal)?;
        engine::check_body_syntax(&body)?;
        if let Some(args) = &args {
            // Reject oversized or deeply nested arguments before execution.
            workflow_core::boundary::graph_from_json(args)
                .map_err(|error| WorkflowError::Invalid(format!("args cannot cross the boundary: {error}")))?;
        }
        let spec = Self {
            body,
            meta: meta.clone(),
            args,
            budget_total,
            role_policy,
        };
        Ok((meta, spec))
    }

    pub fn meta(&self) -> &ScriptMeta {
        &self.meta
    }

    /// Create the engine on the current driver worker thread.
    pub fn start(self) -> Result<ScriptSource, WorkflowError> {
        ScriptSource::start(
            self.body,
            self.meta,
            self.args,
            self.budget_total,
            self.role_policy,
        )
    }
}

/// Extract `export const meta = {...}` from a script.
///
/// The returned body replaces the statement with spaces while preserving newlines,
/// so compiler diagnostics retain the user's line numbers. The statement must
/// occur before non-comment code because metadata is the approval and run manifest.
fn extract_meta(source: &str) -> Result<(String, String), WorkflowError> {
    let bytes = source.as_bytes();
    let mut cursor = 0usize;

    // Skip leading whitespace and comments.
    loop {
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if source[cursor..].starts_with("//") {
            match source[cursor..].find('\n') {
                Some(offset) => cursor += offset + 1,
                None => cursor = bytes.len(),
            }
            continue;
        }
        if source[cursor..].starts_with("/*") {
            match source[cursor..].find("*/") {
                Some(offset) => cursor += offset + 2,
                None => {
                    return Err(WorkflowError::Invalid("Unterminated block comment at the start of the script".into()))
                }
            }
            continue;
        }
        break;
    }

    let statement_start = cursor;
    for keyword in ["export", "const", "meta"] {
        if !source[cursor..].starts_with(keyword) {
            return Err(WorkflowError::Invalid(
                "Script must begin with `export const meta = { ... }` (name and description are required)".into(),
            ));
        }
        cursor += keyword.len();
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
    }
    if cursor >= bytes.len() || bytes[cursor] != b'=' {
        return Err(WorkflowError::Invalid(
            "Script must begin with `export const meta = { ... }` (name and description are required)".into(),
        ));
    }
    cursor += 1;
    while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
        cursor += 1;
    }
    if cursor >= bytes.len() || bytes[cursor] != b'{' {
        return Err(WorkflowError::Invalid("meta must be an object literal".into()));
    }

    let literal_start = cursor;
    let literal_end = match_literal_end(source, literal_start)?;

    let mut statement_end = literal_end + 1;
    while statement_end < bytes.len() && bytes[statement_end].is_ascii_whitespace()
        && bytes[statement_end] != b'\n'
    {
        statement_end += 1;
    }
    if statement_end < bytes.len() && bytes[statement_end] == b';' {
        statement_end += 1;
    }

    let meta_literal = source[literal_start..=literal_end].to_owned();
    let mut body = String::with_capacity(source.len());
    for (index, ch) in source.char_indices() {
        if index >= statement_start && index < statement_end && ch != '\n' && ch != '\r' {
            body.push(' ');
        } else {
            body.push(ch);
        }
    }
    Ok((meta_literal, body))
}

/// Find the byte index of the `}` that matches the opening `{`.
///
/// The scanner recognizes quoted strings, template strings with `${}` nesting, and
/// both comment forms. Literal validation is performed separately in an empty
/// evaluation context.
fn match_literal_end(source: &str, open: usize) -> Result<usize, WorkflowError> {
    enum Frame {
        Code { depth: i32 },
        Str(char),
        Template,
    }
    enum Op {
        None,
        Push(Frame),
        Pop,
        Done,
    }
    let mut frames = vec![Frame::Code { depth: 0 }];
    let mut chars = source[open..].char_indices().peekable();
    while let Some((offset, ch)) = chars.next() {
        let at = open + offset;
        let top = frames.len() - 1;
        let op = match &mut frames[top] {
            Frame::Code { depth } => match ch {
                '{' => {
                    *depth += 1;
                    Op::None
                }
                '}' => {
                    *depth -= 1;
                    if *depth == 0 {
                        if top == 0 {
                            Op::Done
                        } else {
                            Op::Pop
                        }
                    } else {
                        Op::None
                    }
                }
                '"' | '\'' => Op::Push(Frame::Str(ch)),
                '`' => Op::Push(Frame::Template),
                '/' => match chars.peek().map(|(_, next)| *next) {
                    Some('/') => {
                        for (_, skipped) in chars.by_ref() {
                            if skipped == '\n' {
                                break;
                            }
                        }
                        Op::None
                    }
                    Some('*') => {
                        chars.next();
                        let mut star = false;
                        let mut closed = false;
                        for (_, skipped) in chars.by_ref() {
                            if star && skipped == '/' {
                                closed = true;
                                break;
                            }
                            star = skipped == '*';
                        }
                        if !closed {
                            return Err(WorkflowError::Invalid(
                                "Unterminated block comment in meta literal".into(),
                            ));
                        }
                        Op::None
                    }
                    _ => Op::None,
                },
                _ => Op::None,
            },
            Frame::Str(quote) => match ch {
                '\\' => {
                    chars.next();
                    Op::None
                }
                _ if ch == *quote || ch == '\n' => Op::Pop,
                _ => Op::None,
            },
            Frame::Template => match ch {
                '\\' => {
                    chars.next();
                    Op::None
                }
                '`' => Op::Pop,
                '$' => {
                    if matches!(chars.peek(), Some((_, '{'))) {
                        chars.next();
                        // `${` opens a nested code frame; its matching `}`
                        // returns to the template frame.
                        Op::Push(Frame::Code { depth: 1 })
                    } else {
                        Op::None
                    }
                }
                _ => Op::None,
            },
        };
        match op {
            Op::None => {}
            Op::Push(frame) => frames.push(frame),
            Op::Pop => {
                frames.pop();
            }
            Op::Done => return Ok(at),
        }
    }
    Err(WorkflowError::Invalid("meta object literal is not closed".into()))
}

/// Read a display name from the header without creating an engine.
///
/// This is only for receipt summaries and timeline cards. Unsupported forms return
/// `None` so callers can degrade without failing.
pub fn script_display_name(source: &str) -> Option<String> {
    let (literal, _) = extract_meta(source).ok()?;
    let bytes = literal.as_bytes();
    let mut cursor = 0usize;
    while let Some(offset) = literal[cursor..].find("name") {
        let start = cursor + offset;
        cursor = start + "name".len();
        // Accept only a top-level key followed by a colon.
        let before = literal[..start]
            .trim_end()
            .chars()
            .next_back();
        if !matches!(before, Some('{') | Some(',')) {
            continue;
        }
        let mut rest = cursor;
        while rest < bytes.len() && bytes[rest].is_ascii_whitespace() {
            rest += 1;
        }
        if rest >= bytes.len() || bytes[rest] != b':' {
            continue;
        }
        rest += 1;
        while rest < bytes.len() && bytes[rest].is_ascii_whitespace() {
            rest += 1;
        }
        let quote = match bytes.get(rest) {
            Some(b'"') => '"',
            Some(b'\'') => '\'',
            _ => continue,
        };
        let mut name = String::new();
        let mut chars = literal[rest + 1..].chars();
        loop {
            match chars.next() {
                None => return None,
                Some('\\') => match chars.next() {
                    Some(escaped) => name.push(escaped),
                    None => return None,
                },
                Some(ch) if ch == quote => {
                    let name = name.trim();
                    return (!name.is_empty()).then(|| name.to_owned());
                }
                Some(ch) => name.push(ch),
            }
        }
    }
    None
}

/// Validate metadata fields from boundary-crossed JSON.
pub(crate) fn validate_meta_json(meta: &Value) -> Result<ScriptMeta, WorkflowError> {
    let Some(object) = meta.as_object() else {
        return Err(WorkflowError::Invalid("meta must be an object literal".into()));
    };
    let name = object
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| WorkflowError::Invalid("meta.name must be a non-empty string".into()))?;
    if name.chars().count() > 200 {
        return Err(WorkflowError::Invalid("meta.name exceeds 200 characters".into()));
    }
    let description = object
        .get("description")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|description| !description.is_empty())
        .ok_or_else(|| {
            WorkflowError::Invalid(
                "meta.description must be a non-empty string (it appears in the approval dialog and run manifest)".into(),
            )
        })?;
    let when_to_use = object
        .get("whenToUse")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let mut phases = Vec::new();
    if let Some(declared) = object.get("phases") {
        let Some(entries) = declared.as_array() else {
            return Err(WorkflowError::Invalid("meta.phases must be an array".into()));
        };
        for (index, entry) in entries.iter().enumerate() {
            let title = entry
                .get("title")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|title| !title.is_empty())
                .ok_or_else(|| {
                    WorkflowError::Invalid(format!(
                        "meta.phases[{index}].title must be a non-empty string"
                    ))
                })?;
            phases.push(MetaPhase {
                title: title.to_owned(),
                detail: entry
                    .get("detail")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            });
        }
    }
    Ok(ScriptMeta {
        name: name.to_owned(),
        description: description.to_owned(),
        when_to_use,
        phases,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = r#"export const meta = { name: "n", description: "d" }
return 42
"#;

    #[test]
    fn a_minimal_script_parses_and_blanks_the_meta_statement_in_place() {
        let (meta, spec) = ScriptSpec::parse(MINIMAL, None, None, StepRolePolicy::permissive()).unwrap();
        assert_eq!(meta.name, "n");
        assert_eq!(meta.description, "d");
        assert!(meta.phases.is_empty());
        // The meta line becomes same-width whitespace; the next line is unchanged.
        let mut lines = spec.body.lines();
        assert_eq!(lines.next().unwrap().trim(), "");
        assert_eq!(lines.next().unwrap(), "return 42");
    }

    #[test]
    fn meta_extraction_survives_strings_templates_and_comments_inside_the_literal() {
        let source = "export const meta = { name: \"a}b\", description: 'c{', phases: [{ title: `t${\"x\"}y` /* } */ }] };\nlog(\"after\")\n";
        let (meta, spec) = ScriptSpec::parse(source, None, None, StepRolePolicy::permissive()).unwrap();
        assert_eq!(meta.name, "a}b");
        assert_eq!(meta.description, "c{");
        assert_eq!(meta.phases.len(), 1);
        assert!(spec.body.contains("log(\"after\")"));
        assert!(!spec.body.contains("meta"));
    }

    #[test]
    fn scripts_without_a_leading_meta_statement_are_rejected_with_the_shape_hint() {
        for source in [
            "return 1",
            "const meta = { name: \"n\", description: \"d\" }",
            "export let meta = { name: \"n\", description: \"d\" }",
        ] {
            let error = ScriptSpec::parse(source, None, None, StepRolePolicy::permissive()).unwrap_err();
            assert!(
                error.to_string().contains("export const meta"),
                "{source} → {error}"
            );
        }
    }

    #[test]
    fn leading_comments_and_whitespace_before_meta_are_tolerated() {
        let source = "// header\n/* block */\n  export const meta = { name: \"n\", description: \"d\" };\nreturn 1\n";
        let (meta, _) = ScriptSpec::parse(source, None, None, StepRolePolicy::permissive()).unwrap();
        assert_eq!(meta.name, "n");
    }

    #[test]
    fn meta_missing_name_or_description_is_rejected() {
        for (source, needle) in [
            ("export const meta = { description: \"d\" }\n", "meta.name"),
            ("export const meta = { name: \"n\" }\n", "meta.description"),
            (
                "export const meta = { name: \"\", description: \"d\" }\n",
                "meta.name",
            ),
        ] {
            let error = ScriptSpec::parse(source, None, None, StepRolePolicy::permissive()).unwrap_err();
            assert!(error.to_string().contains(needle), "{source} → {error}");
        }
    }

    #[test]
    fn a_computed_meta_is_rejected_because_the_bare_context_has_no_bindings() {
        let source = "export const meta = { name: someVariable, description: \"d\" }\nreturn 1\n";
        let error = ScriptSpec::parse(source, None, None, StepRolePolicy::permissive()).unwrap_err();
        assert!(error.to_string().contains("pure literal"), "{error}");
    }

    #[test]
    fn an_unterminated_meta_literal_is_rejected() {
        let error =
            ScriptSpec::parse("export const meta = { name: \"n\"", None, None, StepRolePolicy::permissive()).unwrap_err();
        assert!(error.to_string().contains("not closed"), "{error}");
    }

    #[test]
    fn a_body_syntax_error_is_rejected_before_any_execution() {
        let source = "export const meta = { name: \"n\", description: \"d\" }\nconst x = ;\n";
        let error = ScriptSpec::parse(source, None, None, StepRolePolicy::permissive()).unwrap_err();
        assert!(error.to_string().contains("syntax error"), "{error}");
    }

    #[test]
    fn an_oversized_script_is_rejected_by_the_byte_cap() {
        let mut source = String::from("export const meta = { name: \"n\", description: \"d\" }\n");
        source.push_str(&"// pad\n".repeat(MAX_SCRIPT_BYTES / 7 + 1));
        let error = ScriptSpec::parse(&source, None, None, StepRolePolicy::permissive()).unwrap_err();
        assert!(error.to_string().contains("exceeding the limit"), "{error}");
    }

    #[test]
    fn oversized_args_are_rejected_at_parse_time() {
        let args = Value::Array(vec![Value::Null; workflow_core::MAX_BOUNDARY_ITEMS + 1]);
        let error = ScriptSpec::parse(MINIMAL, Some(args), None, StepRolePolicy::permissive()).unwrap_err();
        assert!(error.to_string().contains("args"), "{error}");
    }

    #[test]
    fn the_display_name_helper_reads_the_name_without_an_engine() {
        assert_eq!(
            script_display_name(MINIMAL).as_deref(),
            Some("n")
        );
        let single_quoted =
            "export const meta = { description: \"d\", name: 'audit-run' }\nreturn 1\n";
        assert_eq!(
            script_display_name(single_quoted).as_deref(),
            Some("audit-run")
        );
        // A `name` occurrence in a string is not a key.
        let decoy = "export const meta = { description: \"name: fake\", name: \"real\" }\n";
        assert_eq!(script_display_name(decoy).as_deref(), Some("real"));
        assert_eq!(script_display_name("return 1"), None);
    }

    #[test]
    fn meta_phases_titles_are_validated_and_kept_in_declaration_order() {
        let source = "export const meta = { name: \"n\", description: \"d\", phases: [{ title: \"Scan\", detail: \"x\" }, { title: \"Fix\" }] }\nreturn 1\n";
        let (meta, _) = ScriptSpec::parse(source, None, None, StepRolePolicy::permissive()).unwrap();
        assert_eq!(
            meta.phases
                .iter()
                .map(|phase| phase.title.as_str())
                .collect::<Vec<_>>(),
            vec!["Scan", "Fix"]
        );
        assert_eq!(meta.phases[0].detail.as_deref(), Some("x"));

        let bad = "export const meta = { name: \"n\", description: \"d\", phases: [{ detail: \"x\" }] }\n";
        let error = ScriptSpec::parse(bad, None, None, StepRolePolicy::permissive()).unwrap_err();
        assert!(error.to_string().contains("phases[0].title"), "{error}");
    }
}
