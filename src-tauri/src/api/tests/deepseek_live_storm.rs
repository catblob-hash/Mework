//! Live stress tests for subagent and workflow message integrity.
//!
//! The tests distinguish queued-message merging, stale-incarnation result rejection,
//! cancellation-timeout finalization, and persistence gaps. They collect event-stream,
//! process-output, and SQLite evidence so failures remain diagnosable.
//!
//! All tests are `#[ignore]`: they require network access and a real key, which must
//! never be committed.
//!
//! ```powershell
//! npm run test:deepseek-e2e -- --filter=storm
//! # Or run manually:
//! $env:MEWORK_DEEPSEEK_LIVE_API_KEY = "sk-..."
//! cargo test --lib -- deepseek_live_storm --ignored --nocapture --test-threads=1
//! ```

use super::*;
use crate::model::ModelCapability;

const KEY_ENV: &str = "MEWORK_DEEPSEEK_LIVE_API_KEY";
const MODEL_ENV: &str = "MEWORK_DEEPSEEK_LIVE_MODEL";
const CHAT_BASE_ENV: &str = "MEWORK_DEEPSEEK_LIVE_CHAT_BASE_URL";
const DEFAULT_OPENAI_BASE: &str = "https://api.deepseek.com";
/// The production conversation uses this model, while `deepseek_live.rs` defaults to
/// the flash family.
const DEFAULT_STORM_MODEL: &str = "deepseek-v4-flash-vision-exp";

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
                "设置 {KEY_ENV} 后再运行 deepseek_live_storm 测试（推荐通过 \
                 scripts/deepseek-live-e2e.mjs --filter=storm 启动）"
            )
        })
}

/// Shared event buffer for the recording sink. Top-level and subagent-surface events
/// must share one timeline to preserve their interleaving.
#[derive(Clone, Default)]
struct EventRecorder {
    events: Arc<Mutex<Vec<ModelStreamEvent>>>,
}

impl EventRecorder {
    fn push(&self, event: ModelStreamEvent) {
        self.events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(event);
    }

    fn snapshot(&self) -> Vec<ModelStreamEvent> {
        self.events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Writes JSONL evidence for diagnosing assertion failures.
    fn dump(&self, directory: &std::path::Path, name: &str) -> std::path::PathBuf {
        let path = directory.join(name);
        let mut file = std::fs::File::create(&path).expect("event dump");
        for event in self.snapshot() {
            let line = serde_json::to_string(&event).unwrap_or_else(|error| {
                format!("{{\"unserializable\":{:?}}}", error.to_string())
            });
            let _ = writeln!(file, "{line}");
        }
        path
    }
}

/// Subagent events use the conversation-level surface rather than the top-level sink.
/// Install this surface so the recorder captures their increments.
fn install_recording_surface(state: &AppState, conversation_id: &str, recorder: &EventRecorder) {
    let sink_recorder = recorder.clone();
    let state_recorder = recorder.clone();
    let _ = state_recorder;
    state.register_task_surface(
        conversation_id,
        Arc::new(crate::state::TaskSurface {
            sink: Arc::new(move |event: ModelStreamEvent| {
                sink_recorder.push(event);
                Ok(())
            }),
            approve: Arc::new(
                |_: &ToolExecutionRequest,
                 _: &ToolDescriptor,
                 _: crate::api::ApprovalRequester<'_>,
                 _: Option<&std::sync::atomic::AtomicBool>| Ok(true),
            ),
        }),
    );
}

fn storm_workspace() -> tempfile::TempDir {
    let workspace = tempfile::tempdir().expect("storm workspace");
    std::fs::write(workspace.path().join("README.md"), "# storm\n").unwrap();
    std::fs::write(workspace.path().join("notes.txt"), "cobalt finch 274\n").unwrap();
    std::fs::create_dir_all(workspace.path().join("src")).unwrap();
    std::fs::write(
        workspace.path().join("src/lib.rs"),
        "pub fn answer() -> u32 { 42 }\n",
    )
    .unwrap();
    workspace
}

/// Builds a Chat Completions request using the production model and a dedicated
/// `app_data_path` so the test exercises SQLite persistence.
fn storm_request(
    workspace: &std::path::Path,
    app_data: &std::path::Path,
    conversation_id: &str,
) -> RunModelRequest {
    let mut request = run_request(ProviderFamily::OpenaiChat);
    request.provider.id = "deepseek-storm".into();
    request.provider.name = "DeepSeek Storm".into();
    request.provider.base_url = env_or(CHAT_BASE_ENV, DEFAULT_OPENAI_BASE);
    request.model.id = env_or(MODEL_ENV, DEFAULT_STORM_MODEL);
    request.model.set_capability(ModelCapability::ImageRecognition, false);
    request.model.max_output_tokens = Some(4096);
    request.reasoning_effort = ReasoningEffort::Disabled;
    request.conversation_id = conversation_id.into();
    request.tools = catalog::tool_catalog();
    request.enabled_tools = request
        .tools
        .iter()
        .map(|tool| tool.name.clone())
        .collect();
    request.workspace_path = workspace.to_string_lossy().into_owned();
    request.app_data_path = app_data.to_string_lossy().into_owned();
    request.security_level = SecurityLevel::RequestApproval;
    request.system_prompt =
        "你是 Mework 的工程代理。严格按用户给出的编号步骤执行，不要跳步、不要提前收尾。\
         每一步都要真的调用工具，不要只在文字里描述你打算做什么。"
            .into();
    request
}

/// A numbered scenario serves as a checklist for the model's actual tool use.
fn storm_script(agents: usize, message_rounds: usize) -> String {
    format!(
        "请严格按以下编号步骤执行一次子代理压力测试。每一步都必须真的调用工具。\n\
         \n\
         1. 连续调用 {agents} 次 `agent_spawn`，派生 {agents} 个子代理。\
            每个的 prompt 都写：「你是 STORM-<序号> 号工人。请只回复一行：STORM-<序号> READY」，\
            序号从 1 到 {agents}。记住每次派生回执里返回的子代理名字。\n\
         2. 对**每一个**刚派生的子代理，先调用一次 `send_message`，message 写\
            「PING-A-<子代理名>」；紧接着再调用一次 `followup_task`，message 写\
            「PING-B-<子代理名>，请把你收到的所有 PING 原样列出来」。\n\
         3. 重复第 2 步共 {message_rounds} 轮，每轮把 PING-A / PING-B 换成\
            PING-A2 / PING-B2、PING-A3 / PING-B3……以此类推。\n\
         4. 对每一个子代理再单独调用一次 `followup_task`，message 写\
            「SOLO-<子代理名>，请只回复这一行」。这一步**只用 followup_task**，\
            不要配 send_message。\n\
         5. 调用一次 `workflow`，脚本正文如下（原样使用，不要改动）：\n\
         ```js\n\
         export const meta = {{ name: \"storm\", description: \"fanout\" }}\n\
         const items = [1,2,3,4,5,6]\n\
         const first = await parallel(items.map((n) => () => agent(`只回复一行：WF-A-${{n}}`)))\n\
         const second = await pipeline(items, (n) => agent(`只回复一行：WF-B-${{n}}`))\n\
         return {{ first, second }}\n\
         ```\n\
         6. 调用 `task_wait` 等待所有任务（tasks 留空即等全部），\
            timeout_seconds 填 120。如果还有没结束的就再等一次。\n\
         7. 最后一条回复只写一行：STORM_DONE <你一共收到了多少条子代理结果>\n"
    )
}

/// Terminates hung live stress tests after recording diagnostic evidence.
fn arm_watchdog(
    recorder: &EventRecorder,
    evidence_dir: std::path::PathBuf,
    label: &'static str,
    limit: Duration,
) -> Arc<std::sync::atomic::AtomicBool> {
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = Arc::clone(&done);
    let recorder = recorder.clone();
    thread::spawn(move || {
        let deadline = Instant::now() + limit;
        while Instant::now() < deadline {
            if flag.load(std::sync::atomic::Ordering::Acquire) {
                return;
            }
            thread::sleep(Duration::from_millis(500));
        }
        if flag.load(std::sync::atomic::Ordering::Acquire) {
            return;
        }
        let _ = std::fs::create_dir_all(&evidence_dir);
        let path = recorder.dump(&evidence_dir, "storm-events-timeout.jsonl");
        eprintln!(
            "[storm] {label} 超过 {:?} 没有收束——任务层疑似死锁。\
             已录到 {} 条事件：{}",
            limit,
            recorder.snapshot().len(),
            path.display()
        );
        std::process::exit(1);
    });
    done
}

/// Extracts an agent-message call as `(tool name, target, message)`.
fn agent_message_call(context: &ContextItem) -> Option<(String, String, String)> {
    let ContextItem::Tool {
        tool_name, input, ..
    } = context
    else {
        return None;
    };
    if tool_name != "send_message" && tool_name != "followup_task" {
        return None;
    }
    let target = input.get("target")?.as_str()?.to_owned();
    let message = input.get("message")?.as_str()?.to_owned();
    Some((tool_name.clone(), target, message))
}

/// Final records for each subagent in timeline order. Records move to the latest call,
/// so later entries replace earlier ones.
fn final_agent_records(contexts: &[ContextItem]) -> HashMap<String, SubagentRunRecord> {
    let mut records: HashMap<String, SubagentRunRecord> = HashMap::new();
    for context in contexts {
        let ContextItem::Tool {
            subagent: Some(record),
            ..
        } = context
        else {
            continue;
        };
        if let Some(name) = record.name.clone() {
            records.insert(name, record.clone());
        }
    }
    records
}

/// Only general subagents are addressable through `send_message` and `followup_task`.
/// Managed tasks retain records but reject direct messages.
fn addressable_records(contexts: &[ContextItem]) -> HashMap<String, SubagentRunRecord> {
    final_agent_records(contexts)
        .into_iter()
        .filter(|(_, record)| record.kind == SubagentRunKind::General)
        .collect()
}

/// A message is either transcribed, still queued, or missing from both locations.
#[derive(Debug, PartialEq, Eq)]
enum MessageFate {
    InTranscript,
    StillQueued,
    Lost,
}

fn message_fate(record: &SubagentRunRecord, message: &str) -> MessageFate {
    let in_transcript = record.contexts.iter().any(|context| match context {
        ContextItem::User { content, .. } => content.contains(message),
        _ => false,
    });
    if in_transcript {
        return MessageFate::InTranscript;
    }
    if record
        .queued_messages
        .iter()
        .any(|queued| queued.content.contains(message))
    {
        return MessageFate::StillQueued;
    }
    MessageFate::Lost
}

/// Finds consecutive user-context runs lacking a subsequent assistant context.
fn unanswered_user_runs(contexts: &[ContextItem]) -> Vec<(usize, String)> {
    let mut runs = Vec::new();
    let mut current: Vec<String> = Vec::new();
    let mut answered_since_run = true;
    for context in contexts {
        match context {
            ContextItem::User { content, .. } => {
                if answered_since_run {
                    current.clear();
                    answered_since_run = false;
                }
                current.push(content.chars().take(40).collect());
            }
            ContextItem::Assistant { .. } => {
                current.clear();
                answered_since_run = true;
            }
            _ => {}
        }
    }
    if !current.is_empty() && !answered_since_run {
        runs.push((current.len(), current.first().cloned().unwrap_or_default()));
    }
    runs
}

/// Loads persisted conversation contexts in `order_key` order.
fn contexts_from_store(app_data: &std::path::Path, conversation_id: &str) -> Vec<ContextItem> {
    let anchor = app_data.join("document.v1.json");
    let store = crate::conversation_store::store_for(&anchor).expect("open conversation store");
    let conversation = store
        .conversation(conversation_id)
        .expect("read conversation")
        .unwrap_or_else(|| panic!("对话 {conversation_id} 不在库里——落盘那条线根本没走"));
    conversation.contexts
}

/// Seed the conversation row before runtime `upsert_contexts`; it otherwise has no
/// persistence target and skips the write.
fn seed_conversation_row(
    app_data: &std::path::Path,
    workspace_id: &str,
    conversation_id: &str,
) {
    let anchor = app_data.join("document.v1.json");
    let store = crate::conversation_store::store_for(&anchor).expect("open conversation store");
    // Deserialize defaults so added defaulted fields do not break this test helper.
    let settings: crate::model::ConversationSettings =
        serde_json::from_value(json!({"systemPrompt": "", "enabledTools": []}))
            .expect("default conversation settings");
    store
        .put_conversation(
            workspace_id,
            &crate::model::Conversation {
                id: conversation_id.to_owned(),
                title: "storm".into(),
                created_at: "2026-08-27T00:00:00Z".into(),
                updated_at: "2026-08-27T00:00:00Z".into(),
                settings,
                contexts: Vec::new(),
                queued_messages: Vec::new(),
                branches: Vec::new(),
                user_aborted_tasks: Vec::new(),
                worktree: None,
                run_target: None,
                parent_conversation_id: None,
            },
        )
        .expect("seed conversation row");
}

// ------------------------------------------------------------------ Leg A

#[test]
#[ignore = "live network test against api.deepseek.com; run via scripts/deepseek-live-e2e.mjs --filter=storm"]
fn deepseek_live_storm_keeps_every_subagent_message() {
    let workspace = storm_workspace();
    let app_data = tempfile::tempdir().expect("storm app data");
    let conversation_id = "conv_storm";
    let mut request = storm_request(workspace.path(), app_data.path(), conversation_id);
    seed_conversation_row(app_data.path(), &request.workspace_id, conversation_id);
    request.contexts = vec![ContextItem::User {
        id: "storm-user-1".into(),
        content: storm_script(8, 3),
        images: Vec::new(),
        created_at: "2026-08-27T00:00:00Z".into(),
    }];
    save_api_key(&request.provider.id, &live_api_key()).expect("stash key in test keyring");

    let recorder = EventRecorder::default();
    let state = AppState::default();
    install_recording_surface(&state, conversation_id, &recorder);
    let sink_recorder = recorder.clone();
    let sink = move |event: ModelStreamEvent| -> Result<(), String> {
        sink_recorder.push(event);
        Ok(())
    };

    let keep = std::env::temp_dir().join("mework-storm-evidence");
    let done = arm_watchdog(&recorder, keep.clone(), "storm", Duration::from_secs(600));
    let started = Instant::now();
    let response = run_model(request, &state, &sink, &approve_tool)
        .unwrap_or_else(|error| panic!("storm run failed: {error}"));
    done.store(true, std::sync::atomic::Ordering::Release);
    let elapsed = started.elapsed();

    let dump = recorder.dump(app_data.path(), "storm-events.jsonl");
    // Preserve key evidence beyond the temporary directory lifetime.
    let _ = std::fs::create_dir_all(&keep);
    let _ = std::fs::copy(&dump, keep.join("storm-events.jsonl"));

    let records = final_agent_records(&response.contexts);
    let messages: Vec<(String, String, String)> = response
        .contexts
        .iter()
        .filter_map(agent_message_call)
        .collect();

    eprintln!(
        "[storm] 用时 {:?}｜上下文 {}｜代理消息调用 {}｜留下记录的子代理 {}｜证据 {}",
        elapsed,
        response.contexts.len(),
        messages.len(),
        records.len(),
        keep.display()
    );
    for (name, record) in &records {
        eprintln!(
            "[storm]   {name}: status={:?} 转录 {} 条 排队 {} 条 token {:?}",
            record.status,
            record.contexts.len(),
            record.queued_messages.len(),
            record.usage.total_tokens
        );
    }

    // Require enough activity for the assertions below to be meaningful.
    assert!(
        records.len() >= 2,
        "模型没有真的派生子代理（只有 {} 条记录）；证据：{}",
        records.len(),
        keep.display()
    );
    assert!(
        messages.len() >= 4,
        "模型没有真的高频通信（只有 {} 次 send_message/followup_task）；证据：{}",
        messages.len(),
        keep.display()
    );

    // Every dispatched message must be transcribed or legally remain queued.
    // `followup_task` promises to continue after the current child turn, so it may
    // not remain queued once the run ends. `send_message` only queues work.
    let mut lost = Vec::new();
    for (tool, target, message) in &messages {
        let Some(record) = records.get(target) else {
            lost.push(format!("{tool}→{target}「{message}」：该 target 在时间线上没有任何记录"));
            continue;
        };
        match message_fate(record, message) {
            MessageFate::InTranscript => {}
            MessageFate::StillQueued if tool == "send_message" => {}
            MessageFate::StillQueued => lost.push(format!(
                "{tool}→{target}「{message}」：运行结束时仍停在排队里——回执却已经\
                 承诺它会被消费"
            )),
            MessageFate::Lost => lost.push(format!(
                "{tool}→{target}「{message}」：既不在转录里也不在排队里"
            )),
        }
    }
    assert!(
        lost.is_empty(),
        "{} 条子代理消息凭空消失：\n  {}\n证据：{}",
        lost.len(),
        lost.join("\n  "),
        keep.display()
    );

    // Completed transcripts must not end with unanswered consecutive user contexts.
    let mut unanswered = Vec::new();
    for (name, record) in &records {
        for (length, preview) in unanswered_user_runs(&record.contexts) {
            if length >= 1 && record.status == SubagentRunStatus::Completed {
                unanswered.push(format!(
                    "{name}: {length} 条连续 user 消息之后没有 assistant 回复（首条「{preview}…」）"
                ));
            }
        }
    }
    assert!(
        unanswered.is_empty(),
        "复现到用户症状「整轮回复丢失，只剩连续 user 消息」：\n  {}\n证据：{}",
        unanswered.join("\n  "),
        keep.display()
    );

    // Persisted records must be as complete as in-memory records.
    let persisted = contexts_from_store(app_data.path(), conversation_id);
    let persisted_records = final_agent_records(&persisted);
    let mut shrunk = Vec::new();
    for (name, record) in &records {
        match persisted_records.get(name) {
            None => shrunk.push(format!("{name}: 内存里有记录，库里整条没有")),
            Some(stored) if stored.contexts.len() < record.contexts.len() => {
                shrunk.push(format!(
                    "{name}: 内存转录 {} 条，库里只有 {} 条",
                    record.contexts.len(),
                    stored.contexts.len()
                ));
            }
            Some(_) => {}
        }
    }
    assert!(
        shrunk.is_empty(),
        "子代理记录没有完整落盘：\n  {}\n证据：{}",
        shrunk.join("\n  "),
        keep.display()
    );
}

// ------------------------------------------------------------------ Leg C

/// Simulates a restart by discarding memory and addressing existing subagents only
/// through contexts loaded from the store.
#[test]
#[ignore = "live network test against api.deepseek.com; run via scripts/deepseek-live-e2e.mjs --filter=storm"]
fn deepseek_live_storm_subagents_survive_a_simulated_restart() {
    let workspace = storm_workspace();
    let app_data = tempfile::tempdir().expect("storm app data");
    let conversation_id = "conv_storm_restart";
    let mut request = storm_request(workspace.path(), app_data.path(), conversation_id);
    seed_conversation_row(app_data.path(), &request.workspace_id, conversation_id);
    request.contexts = vec![ContextItem::User {
        id: "storm-restart-user-1".into(),
        content: storm_script(3, 1),
        images: Vec::new(),
        created_at: "2026-08-27T00:00:00Z".into(),
    }];
    save_api_key(&request.provider.id, &live_api_key()).expect("stash key in test keyring");

    let recorder = EventRecorder::default();
    let first_state = AppState::default();
    install_recording_surface(&first_state, conversation_id, &recorder);
    let sink_recorder = recorder.clone();
    let sink = move |event: ModelStreamEvent| -> Result<(), String> {
        sink_recorder.push(event);
        Ok(())
    };
    let done = arm_watchdog(
        &recorder,
        std::env::temp_dir().join("mework-storm-evidence"),
        "storm-restart",
        Duration::from_secs(420),
    );
    let response = run_model(request, &first_state, &sink, &approve_tool)
        .unwrap_or_else(|error| panic!("storm run failed: {error}"));
    done.store(true, std::sync::atomic::Ordering::Release);

    let live_names: Vec<String> = addressable_records(&response.contexts)
        .keys()
        .cloned()
        .collect();
    assert!(
        !live_names.is_empty(),
        "第一段风暴没有派生出任何可寻址子代理，重启腿无从谈起"
    );

    // Simulate a process restart: discard memory and retain disk state only.
    drop(response);
    drop(first_state);
    crate::conversation_store::close_store_for(&app_data.path().join("document.v1.json"));

    let restored = contexts_from_store(app_data.path(), conversation_id);
    let restored_records = addressable_records(&restored);
    eprintln!(
        "[storm-restart] 库里读回 {} 条上下文，{} 个子代理记录（{:?}）",
        restored.len(),
        restored_records.len(),
        restored_records.keys().collect::<Vec<_>>()
    );
    for name in &live_names {
        assert!(
            restored_records.contains_key(name),
            "子代理 {name} 的记录没能从库里读回来——它此后永远无法被寻址（恢复丢失）"
        );
    }

    // Continue using the persisted history.
    let second_state = AppState::default();
    install_recording_surface(&second_state, conversation_id, &recorder);
    let pool = AgentPool::new();
    let mut parent = storm_request(workspace.path(), app_data.path(), conversation_id);
    parent.contexts = restored;

    let mut failures = Vec::new();
    for name in &live_names {
        let call = ToolCall {
            id: format!("restart-followup-{name}"),
            name: "followup_task".into(),
            input: json!({
                "target": name,
                "message": format!("RESTART-PING-{name}，请原样回复这一行")
            })
            .as_object()
            .unwrap()
            .clone(),
        };
        let execution = run_followup_task(
            &pool,
            &test_kernel_shadow(),
            &parent,
            call,
            &second_state,
            &discard_event,
            1,
        )
        .unwrap_or_else(|error| panic!("followup_task 直接报错：{error}"));
        if !execution.result.success {
            failures.push(format!("{name}: {}", execution.result.output));
        }
    }
    assert!(
        failures.is_empty(),
        "重启后无法向既有子代理投递消息（恢复丢失的直接复现）：\n  {}",
        failures.join("\n  ")
    );
}
