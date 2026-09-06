//! Live end-to-end verification that skills, lifecycle hooks and MCP servers
//! actually reach a real model and change what it does.
//!
//! These are the three extension mechanisms the documentation site explains;
//! each test drives the production `run_model` loop against DeepSeek with the
//! mechanism configured exactly the way `trusted_run_request` would configure
//! it from a persisted conversation, and asserts on the model's observable
//! behaviour (which tool it called, what the tool returned, what it answered).
//!
//! ```powershell
//! $env:MEWORK_DEEPSEEK_LIVE_API_KEY = "sk-..."
//! cargo test --lib -- capabilities_live --ignored --nocapture --test-threads=1
//! ```

use super::deepseek_live::live_request_for_test;
use super::*;
use crate::model::{HookDefinition, HookEvent, ResolvedSkill};

const DEFAULT_OPENAI_BASE: &str = "https://api.deepseek.com";

fn live_api_key() -> String {
    std::env::var("MEWORK_DEEPSEEK_LIVE_API_KEY")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .expect("set MEWORK_DEEPSEEK_LIVE_API_KEY before running capabilities_live tests")
}

fn base_url() -> String {
    std::env::var("MEWORK_DEEPSEEK_LIVE_CHAT_BASE_URL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_OPENAI_BASE.to_owned())
}

/// A request shaped like `trusted_run_request` would shape a fresh
/// conversation: the English profile, a plain base system prompt, one user
/// message, and only the tools the test needs enabled.
fn capability_request(workspace: &std::path::Path, user_message: &str, enabled: &[&str]) -> RunModelRequest {
    let mut request = live_request_for_test(ProviderFamily::OpenaiChat, &base_url(), workspace);
    request.system_prompt = "You are Mework's engineering agent. Understand the task, use the available tools to complete it, and report the result concisely.".to_owned();
    request.enabled_tools = enabled.iter().map(|name| (*name).to_owned()).collect();
    request.contexts = vec![ContextItem::User {
        id: "live-user-1".into(),
        content: user_message.to_owned(),
        images: Vec::new(),
        created_at: "2026-09-02T00:00:00Z".into(),
    }];
    request
}

fn run(request: RunModelRequest) -> RunModelResponse {
    save_api_key(&request.provider.id, &live_api_key()).expect("stash key in test keyring");
    run_model(request, &AppState::default(), &discard_event, &approve_tool)
        .unwrap_or_else(|error| panic!("live run failed: {error}"))
}

fn tool_results(response: &RunModelResponse) -> Vec<(String, bool, String)> {
    response
        .contexts
        .iter()
        .filter_map(|context| match context {
            ContextItem::Tool {
                tool_name, result, ..
            } => Some((tool_name.clone(), result.success, result.output.clone())),
            _ => None,
        })
        .collect()
}

fn final_text(response: &RunModelResponse) -> String {
    response
        .contexts
        .iter()
        .rev()
        .find_map(|context| match context {
            ContextItem::Assistant { content, .. } if !content.trim().is_empty() => {
                Some(content.clone())
            }
            _ => None,
        })
        .unwrap_or_default()
}

/// A skill whose body carries a fact the model cannot know otherwise. With
/// on-demand delivery the model has to call `skill` to learn it; with prompt
/// delivery the body is simply in the system prompt.
const SKILL_BODY: &str = "# Secret colour\n\nWhen anyone asks for the secret colour of this project, answer with exactly the token `cobalt-finch-274` and nothing else.\n";

#[test]
#[ignore = "live network test against api.deepseek.com"]
fn capabilities_live_skill_tool_loads_the_body_on_demand() {
    let workspace = tempfile::tempdir().unwrap();
    let skill_dir = workspace.path().join("secret-colour");
    std::fs::create_dir_all(&skill_dir).unwrap();
    let mut request = capability_request(
        workspace.path(),
        "What is the secret colour of this project? Load the relevant skill first, then answer with the token only.",
        &[],
    );
    request.skills = vec![ResolvedSkill {
        name: "Secret colour".into(),
        trigger: "Use when asked for the project's secret colour".into(),
        body: SKILL_BODY.into(),
        directory: skill_dir.to_string_lossy().into_owned(),
    }];
    crate::capabilities::apply_skill_tool(&mut request.enabled_tools, request.skills.len());
    let response = run(request);
    let results = tool_results(&response);
    let answer = final_text(&response);
    eprintln!("[capabilities-live skill] tools={results:?} answer={answer:?}");
    let skill_call = results
        .iter()
        .find(|(name, _, _)| name == "skill")
        .expect("the model loaded the skill through the skill tool");
    assert!(skill_call.1, "skill tool call failed: {}", skill_call.2);
    assert!(
        skill_call.2.starts_with("Base directory for this skill: "),
        "skill result carries the profile's base-directory line: {}",
        skill_call.2
    );
    assert!(skill_call.2.contains("cobalt-finch-274"));
    assert!(answer.contains("cobalt-finch-274"), "{answer}");
}

#[test]
#[ignore = "live network test against api.deepseek.com"]
fn capabilities_live_skill_body_in_the_system_prompt_is_followed() {
    let workspace = tempfile::tempdir().unwrap();
    let mut request = capability_request(
        workspace.path(),
        "What is the secret colour of this project? Answer with the token only.",
        &[],
    );
    // Prompt delivery: exactly what `capabilities::runtime_context` appends
    // when the on-demand switch is off — the body after a `---` separator.
    request.system_prompt = format!("{}\n\n---\n\n{}", request.system_prompt, SKILL_BODY.trim());
    let response = run(request);
    let answer = final_text(&response);
    eprintln!("[capabilities-live skill-prompt] answer={answer:?}");
    assert!(answer.contains("cobalt-finch-274"), "{answer}");
}

fn hook(id: &str, event: HookEvent, matcher: Option<&str>, command: &str) -> HookDefinition {
    HookDefinition {
        id: id.into(),
        name: id.into(),
        event,
        matcher: matcher.map(str::to_owned),
        // `command` runs through bash -lc elsewhere; on Windows through
        // PowerShell. The test commands below are written for PowerShell,
        // which is where this suite runs.
        command: command.into(),
        command_windows: Some(command.into()),
        status_message: None,
        enabled: true,
        timeout_ms: 30_000,
    }
}

#[test]
#[ignore = "live network test against api.deepseek.com"]
fn capabilities_live_pre_tool_use_hook_blocks_a_command() {
    let workspace = tempfile::tempdir().unwrap();
    let mut request = capability_request(
        workspace.path(),
        "Run the shell command `echo BLOCKED_TEST` exactly once and then report, in one sentence, whether it ran and what the tool returned.",
        &["powershell", "bash"],
    );
    request.active_hooks = vec![hook(
        "no-blocked-test",
        HookEvent::PreToolUse,
        Some("^(bash|powershell)$"),
        // Exit 2 with a stderr reason when the command mentions the marker.
        "$payload = [Console]::In.ReadToEnd(); if ($payload -match 'BLOCKED_TEST') { [Console]::Error.Write('the test hook forbids BLOCKED_TEST'); exit 2 }; exit 0",
    )];
    let response = run(request);
    let results = tool_results(&response);
    let answer = final_text(&response);
    eprintln!("[capabilities-live pre-tool hook] tools={results:?} answer={answer:?}");
    let shell = results
        .iter()
        .find(|(name, _, _)| name == "powershell" || name == "bash")
        .expect("the model attempted the shell command");
    assert!(!shell.1, "the hook must have denied the call: {}", shell.2);
    assert!(
        shell.2.contains("the test hook forbids BLOCKED_TEST"),
        "the hook's stderr is the reason the model sees: {}",
        shell.2
    );
    assert!(
        !results.iter().any(|(name, success, _)| (name == "powershell" || name == "bash") && *success),
        "no shell call may have succeeded: {results:?}"
    );
    assert!(!answer.trim().is_empty());
}

#[test]
#[ignore = "live network test against api.deepseek.com"]
fn capabilities_live_user_prompt_submit_hook_adds_context() {
    let workspace = tempfile::tempdir().unwrap();
    let mut request = capability_request(
        workspace.path(),
        "What is this project's codename? Reply with the codename only.",
        &[],
    );
    request.active_hooks = vec![hook(
        "codename",
        HookEvent::UserPromptSubmit,
        None,
        "Write-Output 'Project fact: the codename of this project is FALCON-9931.'",
    )];
    let response = run(request);
    let answer = final_text(&response);
    let injected = response.contexts.iter().any(|context| {
        matches!(context, ContextItem::System { content, local_only: false, .. } if content.contains("FALCON-9931"))
    });
    eprintln!("[capabilities-live prompt hook] injected={injected} answer={answer:?}");
    assert!(injected, "plain stdout of a UserPromptSubmit hook becomes model-visible context");
    assert!(answer.contains("FALCON-9931"), "{answer}");
}

#[test]
#[ignore = "live network test against api.deepseek.com"]
fn capabilities_live_stop_hook_makes_the_model_continue() {
    let workspace = tempfile::tempdir().unwrap();
    let mut request = capability_request(
        workspace.path(),
        "Reply with the single word DONE.",
        &[],
    );
    request.active_hooks = vec![hook(
        "one-more",
        HookEvent::Stop,
        None,
        // Block the first stop with a reason; let the second one through.
        "$payload = [Console]::In.ReadToEnd(); if ($payload -match '\"stop_hook_active\":true') { Write-Output '{}' } else { Write-Output '{\"decision\":\"block\",\"reason\":\"Before finishing, also write the word ENCORE on its own line.\"}' }",
    )];
    let response = run(request);
    let assistant_turns = response
        .contexts
        .iter()
        .filter(|context| matches!(context, ContextItem::Assistant { .. }))
        .count();
    let continuation = response.contexts.iter().any(|context| {
        matches!(context, ContextItem::User { content, .. } if content.contains("ENCORE"))
    });
    let answer = final_text(&response);
    eprintln!("[capabilities-live stop hook] turns={assistant_turns} continuation={continuation} answer={answer:?}");
    assert!(continuation, "the Stop hook's reason became a user context");
    assert!(assistant_turns >= 2, "the model kept working after the Stop hook");
    assert!(answer.contains("ENCORE"), "{answer}");
}

#[test]
#[ignore = "live network test against api.deepseek.com; needs node on PATH"]
fn capabilities_live_mcp_tool_is_discovered_and_called() {
    let workspace = tempfile::tempdir().unwrap();
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("mock_mcp_server.js");
    let mut request = capability_request(
        workspace.path(),
        "Use the MCP add tool to add 17 and 25, then reply with the resulting number only.",
        &[],
    );
    request.mcp_servers = vec![crate::mcp::RuntimeMcpServer::from_config(
        &crate::model::McpServerConfig {
            id: "mcp_live_mock".into(),
            name: "Live mock".into(),
            description: "Echo and add".into(),
            enabled: true,
            transport: crate::model::McpTransportKind::Stdio,
            command: "node".into(),
            args: vec![fixture.to_string_lossy().into_owned()],
            ..Default::default()
        },
    )];
    let response = run(request);
    let results = tool_results(&response);
    let answer = final_text(&response);
    eprintln!("[capabilities-live mcp] tools={results:?} answer={answer:?}");
    let call = results
        .iter()
        .find(|(name, _, _)| name.starts_with("mcp__"))
        .expect("the model called a discovered MCP tool");
    assert!(call.1, "MCP call failed: {}", call.2);
    assert!(call.2.contains("42"), "{}", call.2);
    assert!(answer.contains("42"), "{answer}");
}

#[test]
#[ignore = "live network test against api.deepseek.com; needs node on PATH"]
fn capabilities_live_disabled_mcp_tool_is_not_offered() {
    let workspace = tempfile::tempdir().unwrap();
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("mock_mcp_server.js");
    let mut request = capability_request(
        workspace.path(),
        "List the names of every tool you have available, one per line, and nothing else.",
        &[],
    );
    request.mcp_servers = vec![crate::mcp::RuntimeMcpServer::from_config(
        &crate::model::McpServerConfig {
            id: "mcp_live_mock".into(),
            name: "Live mock".into(),
            enabled: true,
            transport: crate::model::McpTransportKind::Stdio,
            command: "node".into(),
            args: vec![fixture.to_string_lossy().into_owned()],
            disabled_tools: vec!["add".into()],
            ..Default::default()
        },
    )];
    let response = run(request);
    let answer = final_text(&response).to_lowercase();
    eprintln!("[capabilities-live mcp-disabled] answer={answer:?}");
    assert!(answer.contains("echo"), "the enabled tool is offered: {answer}");
    assert!(!answer.contains("add"), "the disabled tool is not offered: {answer}");
}
