//! Live end-to-end verification of three DeepSeek API protocols.
//!
//! These ignored tests send the public tool schemas, omitted empty descriptions, and
//! tool-result receipts to real models, verifying the complete tool-call lifecycle.
//!
//! They require network access and a real key, which must never be committed.
//!
//! ```powershell
//! npm run test:deepseek-e2e            # The script detects a model and sets environment variables.
//! # Or run manually:
//! $env:MEWORK_DEEPSEEK_LIVE_API_KEY = "sk-..."
//! cargo test --lib -- deepseek_live --ignored --nocapture --test-threads=1
//! ```
//!
//! Under `cfg(test)`, `save_api_key` uses the in-process `credential_guard` keyring
//! and never writes to the system credential store.

use super::*;
use crate::model::ModelCapability;

const KEY_ENV: &str = "MEWORK_DEEPSEEK_LIVE_API_KEY";
const MODEL_ENV: &str = "MEWORK_DEEPSEEK_LIVE_MODEL";
/// Each protocol's base URL can be overridden independently. The OpenAI-compatible API
/// uses its root path; Anthropic uses `/anthropic/v1` because Mework appends `messages`.
const CHAT_BASE_ENV: &str = "MEWORK_DEEPSEEK_LIVE_CHAT_BASE_URL";
const RESPONSES_BASE_ENV: &str = "MEWORK_DEEPSEEK_LIVE_RESPONSES_BASE_URL";
const ANTHROPIC_BASE_ENV: &str = "MEWORK_DEEPSEEK_LIVE_ANTHROPIC_BASE_URL";

const DEFAULT_OPENAI_BASE: &str = "https://api.deepseek.com";
const DEFAULT_ANTHROPIC_BASE: &str = "https://api.deepseek.com/anthropic/v1";

fn env_or(name: &str, fallback: &str) -> String {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| fallback.to_owned())
}

fn live_api_key() -> String {
    std::env::var(KEY_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| {
            panic!(
                "设置 {KEY_ENV} 后再运行 deepseek_live 测试（推荐通过 \
                 scripts/deepseek-live-e2e.mjs 启动）"
            )
        })
}

/// Enable the complete public catalog to test the injection surface rather than an
/// easy passing subset.
/// Shared with `capabilities_live`: the same live request shape.
pub(super) fn live_request_for_test(
    format: ProviderFamily,
    base_url: &str,
    workspace: &std::path::Path,
) -> RunModelRequest {
    live_request(format, base_url, workspace)
}

fn live_request(
    format: ProviderFamily,
    base_url: &str,
    workspace: &std::path::Path,
) -> RunModelRequest {
    let mut request = run_request(format);
    request.provider.id = "deepseek-live".into();
    request.provider.name = "DeepSeek Live".into();
    request.provider.base_url = base_url.into();
    request.model.id = env_or(MODEL_ENV, "deepseek-chat");
    request.model.set_capability(ModelCapability::ImageRecognition, false);
    request.model.max_output_tokens = Some(2048);
    request.reasoning_effort = ReasoningEffort::Disabled;
    request.tools = catalog::tool_catalog();
    request.enabled_tools = request
        .tools
        .iter()
        .map(|tool| tool.name.clone())
        .collect();
    request.workspace_path = workspace.to_string_lossy().into_owned();
    request.system_prompt = "你是 Mework 的工程代理。先理解任务，再使用可用工具完成工作，并简洁报告结果。".into();
    request.contexts = vec![ContextItem::User {
        id: "live-user-1".into(),
        content: "用 ls 工具查看当前工作区根目录（path 填 \".\"），\
                  然后在最终回复里只写一行：MEWORK_E2E_OK <你看到的条目数>"
            .into(),
        images: Vec::new(),
        created_at: "2026-08-15T00:00:00Z".into(),
    }];
    request
}

fn seeded_workspace() -> tempfile::TempDir {
    let workspace = tempfile::tempdir().expect("live e2e workspace");
    std::fs::write(workspace.path().join("README.md"), "# live e2e\n").unwrap();
    std::fs::write(workspace.path().join("notes.txt"), "cobalt finch 274\n").unwrap();
    workspace
}

fn run_live_roundtrip(format: ProviderFamily, base_url: &str) {
    let workspace = seeded_workspace();
    let request = live_request(format, base_url, workspace.path());
    save_api_key(&request.provider.id, &live_api_key()).expect("stash key in test keyring");

    let response = run_model(
        request,
        &AppState::default(),
        &discard_event,
        &approve_tool,
    )
    .unwrap_or_else(|error| panic!("{format:?} live run failed: {error}"));

    let mut tool_rounds = Vec::new();
    let mut final_text = String::new();
    for context in &response.contexts {
        match context {
            ContextItem::Tool {
                tool_name, result, ..
            } => tool_rounds.push((tool_name.clone(), result.success)),
            ContextItem::Assistant { content, .. } => {
                if !content.trim().is_empty() {
                    final_text = content.clone();
                }
            }
            _ => {}
        }
    }
    eprintln!(
        "[deepseek-live {format:?}] tools={tool_rounds:?} final_text={final_text:?}"
    );

    // Confirm that the model selects and calls `ls` from the schema, the host succeeds,
    // and the model produces a final answer after receiving the result.
    assert!(
        tool_rounds
            .iter()
            .any(|(name, success)| name == "ls" && *success),
        "{format:?}: 模型未成功完成 ls 工具循环；tools={tool_rounds:?}"
    );
    assert!(
        !final_text.trim().is_empty(),
        "{format:?}: 工具循环后没有最终回答"
    );
    assert!(
        final_text.contains("MEWORK_E2E_OK"),
        "{format:?}: 最终回答未按约定收尾（允许人工复核 --nocapture 输出）：{final_text}"
    );
}

/// Live native-search test through production `run_web_search` with
/// `SearchBackend::Native`. It covers streaming parsing, pause-turn continuation, and
/// text collection against real upstream event shapes.
fn run_native_search_roundtrip(format: ProviderFamily, base_url: &str) {
    let workspace = seeded_workspace();
    let mut parent = live_request(format, base_url, workspace.path());
    parent.web_search = crate::model::WebSearchSettings {
        max_searches_per_call: 3,
        backend: Some(SearchBackend::Native),
        ..Default::default()
    };
    save_api_key(&parent.provider.id, &live_api_key()).expect("stash key in test keyring");
    // Drive the worker's prepare-and-perform search core directly.
    let output = run_prepared_web_search_for_test(
        &parent,
        "今天的 UTC 日期是几号？给出来源页面",
        &discard_event,
        1,
    )
    .unwrap_or_else(|error| match error {
        AsyncToolError::Cancelled => {
            panic!("{format:?} native search unexpectedly cancelled")
        }
        AsyncToolError::Failed(message) => {
            panic!("{format:?} native search failed: {message}")
        }
    });
    eprintln!("[deepseek-live-native {format:?}] output={output:?}");
    // The JSON envelope is never empty because `notice` is a fallback, so assert that
    // `findings` itself is non-empty to avoid accepting lost search text.
    let envelope: serde_json::Value = serde_json::from_str(&output)
        .expect("native search output is the web_search_output JSON envelope");
    assert!(
        !envelope["findings"]
            .as_str()
            .unwrap_or_default()
            .trim()
            .is_empty(),
        "{format:?}: native search findings is empty: {output}"
    );
}

#[test]
#[ignore = "live network test against api.deepseek.com; run via scripts/deepseek-live-e2e.mjs"]
fn deepseek_live_native_search_responses() {
    run_native_search_roundtrip(
        ProviderFamily::OpenaiResponses,
        &env_or(RESPONSES_BASE_ENV, DEFAULT_OPENAI_BASE),
    );
}

#[test]
#[ignore = "live network test against api.deepseek.com; run via scripts/deepseek-live-e2e.mjs"]
fn deepseek_live_native_search_anthropic() {
    run_native_search_roundtrip(
        ProviderFamily::Anthropic,
        &env_or(ANTHROPIC_BASE_ENV, DEFAULT_ANTHROPIC_BASE),
    );
}

/// Chat Completions does not support provider-executed tools: it accepts only
/// `function` tools and ignores `web_search_options`. Native mode must therefore
/// produce a repairable refusal for this protocol.
#[test]
#[ignore = "live network test against api.deepseek.com; run via scripts/deepseek-live-e2e.mjs"]
fn deepseek_live_native_search_chat_is_a_repairable_refusal() {
    let workspace = seeded_workspace();
    let mut parent = live_request(
        ProviderFamily::OpenaiChat,
        &env_or(CHAT_BASE_ENV, DEFAULT_OPENAI_BASE),
        workspace.path(),
    );
    parent.web_search = crate::model::WebSearchSettings {
        max_searches_per_call: 3,
        backend: Some(SearchBackend::Native),
        ..Default::default()
    };
    save_api_key(&parent.provider.id, &live_api_key()).expect("stash key in test keyring");
    // This is a locally detectable configuration error: reject it before dispatch,
    // without sending a request or creating a task.
    let error = match prepare_web_search_backend(
        &parent,
        "今天的 UTC 日期是几号？给出来源页面".into(),
    ) {
        Err(error) => error,
        Ok(_) => panic!("chat native refusal must be a repairable preflight refusal"),
    };
    eprintln!("[deepseek-live-native OpenaiChat] refusal={error:?}");
    // A repairable refusal identifies the unsupported family, unavailable tool, and
    // remediation. Assert the product contract rather than a transient wording.
    assert!(error.contains("openai_chat"), "{error}");
    assert!(error.contains("没有原生联网搜索工具"), "{error}");
    assert!(error.contains(WEB_SEARCH_REPAIR_HINT), "{error}");
}

#[test]
#[ignore = "live network test against api.deepseek.com; run via scripts/deepseek-live-e2e.mjs"]
fn deepseek_live_chat_completions_tool_loop() {
    run_live_roundtrip(
        ProviderFamily::OpenaiChat,
        &env_or(CHAT_BASE_ENV, DEFAULT_OPENAI_BASE),
    );
}

#[test]
#[ignore = "live network test against api.deepseek.com; run via scripts/deepseek-live-e2e.mjs"]
fn deepseek_live_responses_tool_loop() {
    run_live_roundtrip(
        ProviderFamily::OpenaiResponses,
        &env_or(RESPONSES_BASE_ENV, DEFAULT_OPENAI_BASE),
    );
}

#[test]
#[ignore = "live network test against api.deepseek.com; run via scripts/deepseek-live-e2e.mjs"]
fn deepseek_live_anthropic_messages_tool_loop() {
    run_live_roundtrip(
        ProviderFamily::Anthropic,
        &env_or(ANTHROPIC_BASE_ENV, DEFAULT_ANTHROPIC_BASE),
    );
}

// ===========================================================================
// Background workflow-step writes
// ===========================================================================

/// Verifies that a workflow step can obtain approval and write after its dispatching
/// top-level turn ends. The test requires a written file, at least one approval card,
/// and an `Idle` driver result. It runs all three protocols because their post-tool
/// result handling differs.
fn run_background_workflow_write_roundtrip(format: ProviderFamily, base_url: &str) {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    const MARKER: &str = "MEWORK_BG_WRITE_OK";
    const TARGET: &str = "bg-step-output.txt";

    let workspace = seeded_workspace();
    let app_data = tempfile::tempdir().expect("live e2e app data");
    let mut parent = live_request(format, base_url, workspace.path());
    parent.app_data_path = app_data.path().to_string_lossy().into_owned();
    parent.conversation_id = format!("conv-bg-workflow-{format:?}");
    parent.request_id = format!("run-bg-workflow-{format:?}");
    // Request approval is required to exercise the approval path; full access bypasses it.
    parent.security_level = SecurityLevel::RequestApproval;
    save_api_key(&parent.provider.id, &live_api_key()).expect("stash key in test keyring");

    let state = AppState::default();
    // Use the production session-level approval callback. An always-allow callback
    // would bypass the path under test.
    state.register_task_surface(
        &parent.conversation_id,
        Arc::new(crate::state::TaskSurface {
            sink: Arc::new(|_: ModelStreamEvent| Ok(())),
            approve: session_task_approval(
                &state,
                parent.conversation_id.clone(),
                parent.security_level,
                parent.app_data_path.clone(),
            ),
        }),
    );

    let script = format!(
        "export const meta = {{ name: \"bg-write\", description: \"one step writes one file\" }}\n\
         return await agent(\"Use the write tool exactly once to create a file at path \
         '{TARGET}' in the workspace root whose entire content is the single line {MARKER}. \
         After the write tool reports success, reply with just: done\")\n"
    );
    let call = ToolCall {
        id: "wf-bg-1".into(),
        name: "workflow".into(),
        input: json!({"script": script, "name": "bg-write-run"})
            .as_object()
            .unwrap()
            .clone(),
    };

    // Dispatch the workflow, then end its top-level turn so the driver and steps run
    // in a conversation without an active model run.
    let (cancellation, _) = state
        .begin_model_run(&parent.request_id, &parent.conversation_id)
        .expect("begin the dispatching turn");
    let tasks = state.conversation_tasks(&parent.conversation_id);
    // Direct tool execution must advance the shadow kernel into an active turn and
    // round, matching production `run_model` setup.
    tasks.shadow.begin_turn();
    tasks.shadow.round_started();
    let allow_plan: &DangerousToolApproval<'_> = &|_, _, _| Ok(true);
    let dispatch = crate::workflow::run_workflow_tool(
        &tasks.pool,
        &tasks.shadow,
        &parent,
        call,
        &state,
        &discard_event,
        allow_plan,
        true,
        1,
    )
    .expect("workflow dispatch");
    assert!(
        dispatch.result.success,
        "{format:?}: workflow 派发本身就失败了：{}",
        dispatch.result.output
    );
    state
        .finish_model_run_checked(&parent.request_id, &cancellation)
        .expect("end the dispatching turn");
    assert!(
        !state.conversation_model_run_active(&parent.conversation_id),
        "{format:?}: 这条腿要求回合确实已经结束"
    );

    // Allow every approval card promptly and count approvals.
    let answered = Arc::new(AtomicUsize::new(0));
    let stop_clicking = Arc::new(AtomicBool::new(false));
    let clicker = {
        let state = state.clone();
        let conversation_id = parent.conversation_id.clone();
        let answered = Arc::clone(&answered);
        let stop_clicking = Arc::clone(&stop_clicking);
        std::thread::spawn(move || {
            while !stop_clicking.load(Ordering::Acquire) {
                for (owner, card) in state.tool_prompts().all_pending_cards() {
                    if owner != conversation_id {
                        continue;
                    }
                    if state
                        .tool_prompts()
                        .resolve(&card.prompt_id, crate::tool_prompt::ToolPromptDecision::AllowOnce)
                        .is_ok()
                    {
                        answered.fetch_add(1, Ordering::Release);
                        eprintln!(
                            "[deepseek-live-bg-workflow] 已批准 {} · {} （请求者 {:?}）",
                            card.tool_name, card.summary, card.requester
                        );
                    }
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        })
    };

    // A watchdog prevents a blocked task layer from hanging the test indefinitely.
    let deadline = Instant::now() + Duration::from_secs(600);
    let final_status = loop {
        let status = tasks
            .pool
            .find("bg-write-run")
            .map(|agent| agent.status());
        match status {
            Some(AgentLiveStatus::Running) | None if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(250));
            }
            Some(status) if status != AgentLiveStatus::Running => break status,
            _ => panic!(
                "{format:?}: 后台 workflow 在 600 秒内没有收束（已批准 {} 张卡）",
                answered.load(Ordering::Acquire)
            ),
        }
    };
    stop_clicking.store(true, Ordering::Release);
    clicker.join().expect("clicker thread");

    let written = workspace.path().join(TARGET);
    let cards = answered.load(Ordering::Acquire);
    eprintln!(
        "[deepseek-live-bg-workflow {format:?}] status={final_status:?} cards={cards} \
         written={}",
        written.is_file()
    );
    assert!(
        written.is_file(),
        "{format:?}: 回合结束后的 workflow 步骤没能写出 {TARGET}（已批准 {cards} 张卡，\
         驱动器终态 {final_status:?}）——这正是报障的形态"
    );
    let body = std::fs::read_to_string(&written).expect("read the step's file");
    assert!(
        body.contains(MARKER),
        "{format:?}: 写出来了但内容不对：{body:?}"
    );
    // Require an approval card so an unconditional background allow cannot pass.
    assert!(
        cards >= 1,
        "{format:?}: 这条腿必须真的经过一次批准卡；一次都没有说明它没覆盖被修的那条路径"
    );
    assert_eq!(
        final_status,
        AgentLiveStatus::Idle,
        "{format:?}: 驱动器该正常完成，而不是以中断/失败收场"
    );
}

#[test]
#[ignore = "live network test against api.deepseek.com; run via scripts/deepseek-live-e2e.mjs"]
fn deepseek_live_background_workflow_write_chat() {
    run_background_workflow_write_roundtrip(
        ProviderFamily::OpenaiChat,
        &env_or(CHAT_BASE_ENV, DEFAULT_OPENAI_BASE),
    );
}

#[test]
#[ignore = "live network test against api.deepseek.com; run via scripts/deepseek-live-e2e.mjs"]
fn deepseek_live_background_workflow_write_responses() {
    run_background_workflow_write_roundtrip(
        ProviderFamily::OpenaiResponses,
        &env_or(RESPONSES_BASE_ENV, DEFAULT_OPENAI_BASE),
    );
}

#[test]
#[ignore = "live network test against api.deepseek.com; run via scripts/deepseek-live-e2e.mjs"]
fn deepseek_live_background_workflow_write_anthropic() {
    run_background_workflow_write_roundtrip(
        ProviderFamily::Anthropic,
        &env_or(ANTHROPIC_BASE_ENV, DEFAULT_ANTHROPIC_BASE),
    );
}
