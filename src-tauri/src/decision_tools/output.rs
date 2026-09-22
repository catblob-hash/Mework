//! `find_output` and the `*_find_output` shell variants: which parts of a command's output
//! contain what a query asks for.
//!
//! Two entry points over one scoring pass. [`find_output`] reads the output a command already
//! produced — running or finished — out of the shell-task registry, so a long build can be
//! interrogated without pulling its transcript into the conversation. [`score_command_output`]
//! is the same pass applied to the output of a command the `bash_find_output` /
//! `powershell_find_output` variants just ran, in place of the whole thing.
//!
//! Both keep the status out of the scoring. How the command ended is the one thing the model
//! must always see, so it is printed verbatim above the report rather than left to clear a
//! threshold.

use std::sync::Arc;

use crate::cancel::CancelSignal;
use crate::decision_model::chunk::chunk_lines;
use crate::decision_model::jev::RELEVANCE_RUBRIC;
use crate::decision_model::search::{render_report, search, Scorer};
use crate::decision_model::{parse_query, parse_threshold};
use crate::model::{JsonObject, ToolExecutionRequest};
use crate::shell_tasks::{ShellTaskOutcome, ShellTaskSnapshot};
use crate::state::AppState;
use crate::tool_executor::required_string;

use super::files::{chunk_candidates, refine_line_chunk};
use super::{failure, jev_scorer};

/// Longest task address accepted, matching the schema's `maxLength`.
const MAX_TASK_ADDRESS_CHARS: usize = 64;

/// What a command that produced nothing at all gets in place of a report.
const NO_OUTPUT: &str = "(no output)";

/// `find_output`: the retained output of one shell command of this conversation, scored.
pub(crate) fn find_output(
    request: &ToolExecutionRequest,
    state: &AppState,
    cancel: &CancelSignal,
) -> Result<String, String> {
    let input = &request.input;
    let address = required_string(input, "task", MAX_TASK_ADDRESS_CHARS, false)?;
    let query = parse_query(input, "query")?;
    let threshold = parse_threshold(input)?;
    let id = parse_shell_task_address(&address)?;
    // Both reads are scoped to the conversation, so one conversation cannot name another's
    // command: an address that belongs to someone else reads exactly like one that never was.
    let snapshot = state
        .shell_tasks
        .task_snapshot(&request.conversation_id, &id)
        .ok_or_else(|| unknown_task(&id))?;
    let (text, dropped_head_bytes) = state
        .shell_tasks
        .output_snapshot(&request.conversation_id, &id)
        .ok_or_else(|| unknown_task(&id))?;

    let source = format!("shell:{id} output");
    let mut lines = vec![status_line(&id, &snapshot)];
    if dropped_head_bytes > 0 {
        lines.push(format!(
            "The first {dropped_head_bytes} bytes of this command's output are no longer \
             retained; only the last part of it was scored."
        ));
    }
    let plan = chunk_lines(&text);
    if plan.chunks.is_empty() {
        lines.push(format!("shell:{id} has produced no output to score."));
        return Ok(lines.join("\n"));
    }
    let scorer = jev_scorer(RELEVANCE_RUBRIC)?;
    lines.push(score_chunks(
        scorer,
        &source,
        &plan,
        &query,
        threshold,
        &format!("chunks of shell:{id} output"),
        cancel,
    )?);
    if plan.covered_lines < plan.total_lines {
        lines.push(format!(
            "Only lines 1-{} of the {} retained lines were scored; this command's output is \
             longer than one call can score.",
            plan.covered_lines, plan.total_lines
        ));
    }
    Ok(lines.join("\n"))
}

/// `bash_find_output` / `powershell_find_output`: the output of the command that just ran, in
/// place of the command's own result text.
pub(crate) fn score_command_output(
    output: &str,
    success: bool,
    input: &JsonObject,
    cancel: &CancelSignal,
) -> Result<String, String> {
    let query = parse_query(input, "query")?;
    let threshold = parse_threshold(input)?;
    let (status, body) = split_status_line(output, success);
    let mut lines = Vec::new();
    lines.extend(status.map(str::to_owned));
    let plan = chunk_lines(body);
    if plan.chunks.is_empty() {
        lines.push(NO_OUTPUT.to_owned());
        return Ok(lines.join("\n"));
    }
    let scorer = jev_scorer(RELEVANCE_RUBRIC)?;
    lines.push(score_chunks(
        scorer,
        "output",
        &plan,
        &query,
        threshold,
        "chunks of the command output",
        cancel,
    )?);
    if plan.covered_lines < plan.total_lines {
        lines.push(format!(
            "Only lines 1-{} of the command's {} lines of output were scored; the rest was too \
             long for one call.",
            plan.covered_lines, plan.total_lines
        ));
    }
    Ok(lines.join("\n"))
}

/// The scoring pass both entry points share: coarse line chunks, winners refined into their
/// pieces, rendered with the source and the line numbers of the output itself.
fn score_chunks(
    scorer: Arc<dyn Scorer>,
    source: &str,
    plan: &crate::decision_model::chunk::ChunkPlan,
    query: &str,
    threshold: f64,
    subject: &str,
    cancel: &CancelSignal,
) -> Result<String, String> {
    let (coarse, by_label) = chunk_candidates(source, &plan.chunks);
    let report = search(
        scorer,
        query,
        threshold,
        coarse,
        |candidate| refine_line_chunk(source, &by_label, candidate),
        cancel,
    )
    .map_err(failure)?;
    Ok(render_report(&report, query, threshold, subject, true))
}

/// A failed command's result opens with its status — `Exit code 2`, the abort notice, or the
/// timeout notice, whichever `format_process_output` put there. That line is what explains the
/// failure, so it is kept verbatim at the top instead of being scored with the output. A
/// command that succeeded has no such line: its result is output from the first character.
fn split_status_line(output: &str, success: bool) -> (Option<&str>, &str) {
    if success || output.is_empty() {
        return (None, output);
    }
    match output.split_once('\n') {
        Some((status, rest)) => (Some(status), rest),
        None => (Some(output), ""),
    }
}

/// The shell task id inside a `task` argument. `task_list` prints `shell:3`, and that is the
/// spelling the tool documents; a bare id is accepted too, because it is what the address looks
/// like once a model has stripped the prefix, and it can only ever resolve to the same command.
fn parse_shell_task_address(raw: &str) -> Result<String, String> {
    let raw = raw.trim();
    // `TaskRef::parse` owns the address grammar; a bare id is not one of its forms (it reads as
    // an agent name, and a numeric one is not even that), so it is taken here.
    if !raw.contains(':') {
        return plausible_id(raw);
    }
    match crate::orchestration::TaskRef::parse(raw) {
        Ok(crate::orchestration::TaskRef::Shell(id)) => plausible_id(&id),
        _ => Err(not_a_shell_task(raw)),
    }
}

fn plausible_id(id: &str) -> Result<String, String> {
    if id.is_empty() || id.chars().count() > MAX_TASK_ADDRESS_CHARS {
        return Err(not_a_shell_task(id));
    }
    Ok(id.to_owned())
}

fn not_a_shell_task(address: &str) -> String {
    format!(
        "{address} is not a shell task address; call task_list for the current addresses, which \
         spell a command as shell:<id>"
    )
}

fn unknown_task(id: &str) -> String {
    format!("No shell task shell:{id} in this conversation; call task_list for the current addresses")
}

/// How the command stands, in one line above the report.
fn status_line(id: &str, snapshot: &ShellTaskSnapshot) -> String {
    let tool = &snapshot.tool_name;
    match snapshot.outcome {
        None => format!("shell:{id} ({tool}) is still running"),
        Some(ShellTaskOutcome::Stopped) => format!("shell:{id} ({tool}) was stopped"),
        Some(_) => match snapshot.exit_code {
            Some(code) => format!("shell:{id} ({tool}) finished with exit code {code}"),
            None => format!("shell:{id} ({tool}) finished without reporting an exit code"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell_tasks::ShellTaskRegistry;
    use serde_json::json;

    fn input(pairs: &[(&str, serde_json::Value)]) -> JsonObject {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), value.clone()))
            .collect()
    }

    #[test]
    fn a_task_argument_names_a_shell_command_however_it_is_spelled() {
        assert_eq!(parse_shell_task_address("shell:3").unwrap(), "3");
        assert_eq!(parse_shell_task_address("  shell:3 ").unwrap(), "3");
        assert_eq!(parse_shell_task_address("3").unwrap(), "3");
        // Every other address family is a refusal, not a lookup that happens to miss.
        for address in ["terminal:1", "workflow:run-1", "preview:server-1", "", ":"] {
            let error = parse_shell_task_address(address).unwrap_err();
            assert!(error.contains("not a shell task address"), "{address}: {error}");
        }
    }

    /// The output read is scoped to the owning conversation exactly like `task_snapshot`, and
    /// it installs no sink — a page watching the same command keeps its subscription.
    #[test]
    fn output_snapshot_reads_the_retained_text_of_its_own_conversation_only() {
        let registry = ShellTaskRegistry::default();
        let guard = registry
            .try_register("conversation-a", "bash", "cargo test", false)
            .unwrap();
        let id = guard.shell_task_id().to_owned();
        let mut sink = guard.output_sink(crate::shell_tasks::ShellOutputStream::Stdout);
        sink.append(b"compiling\nerror[E0308]\n");
        sink.finish();
        let (text, dropped) = registry.output_snapshot("conversation-a", &id).unwrap();
        assert_eq!(text, "compiling\nerror[E0308]\n");
        assert_eq!(dropped, 0);
        assert!(registry.output_snapshot("conversation-b", &id).is_none());
        assert!(registry.output_snapshot("conversation-a", "no-such-id").is_none());
    }

    #[test]
    fn a_running_command_and_a_finished_one_say_so_differently() {
        let mut snapshot = ShellTaskSnapshot {
            shell_task_id: "3".into(),
            conversation_id: "c".into(),
            tool_name: "bash".into(),
            command: "cargo test".into(),
            stopping: false,
            started_at: "2026-01-01T00:00:00Z".into(),
            ended_at: None,
            outcome: None,
            exit_code: None,
            background: false,
        };
        assert_eq!(status_line("3", &snapshot), "shell:3 (bash) is still running");
        snapshot.outcome = Some(ShellTaskOutcome::Failed);
        snapshot.exit_code = Some(1);
        assert_eq!(
            status_line("3", &snapshot),
            "shell:3 (bash) finished with exit code 1"
        );
        snapshot.outcome = Some(ShellTaskOutcome::Stopped);
        assert_eq!(status_line("3", &snapshot), "shell:3 (bash) was stopped");
    }

    /// The exit-code line is the model's explanation of the failure, so it survives scoring
    /// whatever the query is — including when there is nothing else at all to score.
    #[test]
    fn the_exit_code_line_is_kept_above_the_report() {
        assert_eq!(
            split_status_line("Exit code 2\nerror: boom\nnext", false),
            (Some("Exit code 2"), "error: boom\nnext")
        );
        assert_eq!(split_status_line("Exit code 2", false), (Some("Exit code 2"), ""));
        assert_eq!(split_status_line("all fine\nhere", true), (None, "all fine\nhere"));

        let arguments = input(&[("query", json!("why")), ("threshold", json!(0.5))]);
        let text =
            score_command_output("Exit code 127", false, &arguments, &CancelSignal::default())
                .unwrap();
        assert_eq!(text, "Exit code 127\n(no output)");
        let text = score_command_output("", true, &arguments, &CancelSignal::default()).unwrap();
        assert_eq!(text, "(no output)");
    }

    /// Argument errors surface before the registry is read or any credential is touched.
    #[test]
    fn missing_arguments_fail_before_the_decision_model_is_consulted() {
        let cancel = CancelSignal::default();
        let error =
            score_command_output("x", true, &input(&[("threshold", json!(0.5))]), &cancel)
                .unwrap_err();
        assert!(error.contains("query"), "{error}");
        let error =
            score_command_output("x", true, &input(&[("query", json!("q"))]), &cancel).unwrap_err();
        assert!(error.contains("threshold"), "{error}");
    }

    /// The lookup is the conversation's own: a command of another conversation reads exactly
    /// like one that never existed, and a command that has printed nothing is an answer rather
    /// than a failure — neither costs a request.
    #[test]
    fn a_foreign_command_is_unknown_and_a_silent_one_is_reported() {
        let state = AppState::default();
        let guard = state
            .shell_tasks
            .try_register("conversation-a", "bash", "cargo build", false)
            .unwrap();
        let id = guard.shell_task_id().to_owned();
        let cancel = CancelSignal::default();
        let ask = |conversation: &str, task: &str| {
            let request = ToolExecutionRequest {
                conversation_id: conversation.to_owned(),
                workspace_path: ".".to_owned(),
                tool_name: "find_output".to_owned(),
                input: input(&[
                    ("task", json!(task)),
                    ("query", json!("compile errors")),
                    ("threshold", json!(0.5)),
                ]),
            };
            find_output(&request, &state, &cancel)
        };
        assert_eq!(
            ask("conversation-a", &format!("shell:{id}")).unwrap(),
            format!(
                "shell:{id} (bash) is still running\nshell:{id} has produced no output to score."
            )
        );
        let error = ask("conversation-b", &format!("shell:{id}")).unwrap_err();
        assert_eq!(
            error,
            format!("No shell task shell:{id} in this conversation; call task_list for the current addresses")
        );
        assert!(ask("conversation-a", "shell:999").unwrap_err().contains("task_list"));
    }
}
