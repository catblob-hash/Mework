//! End-to-end tests for the host-sidecar boundary.
//!
//! These tests launch a sidecar process, send HTTP to a local `TcpListener`,
//! consume SSE, and translate NDJSON into `ModelStreamEvent`.
//!
//! Only end-to-end tests cover cross-language mismatches such as field casing,
//! enum slugs, cancellation probes, and provider-executed tool filtering.
//!
//! The sidecar must be present: a missing sidecar is a test failure.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use super::process::run_step;
use super::protocol::{Family, StepRequest, ToolSpec};
use crate::api::ModelEventSink;
use crate::model::ModelStreamEvent;

/// The sidecar path and `run_step` handle are process-global, so tests share
/// one multiplexed sidecar.
///
/// Missing sidecars must panic rather than skip the test.
pub(crate) fn ensure_sidecar() {
    static ONCE: Mutex<bool> = Mutex::new(false);
    let mut done = ONCE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if *done {
        return;
    }
    let binary = super::process::source_tree_sidecar().expect(
        "AI SDK 侧车缺席，整环测试无法进行。补法：cd aisdk-service && npm ci && npm run build（或 npm run build:sea）",
    );
    // SAFETY: Set once in the test process before starting any sidecar.
    unsafe {
        std::env::set_var("MEWORK_AISDK_BIN", &binary);
    }
    *done = true;
}

// ------------------------------------------------------------------ Mock upstream

/// A mock upstream that responds once. Returns its base URL and the request
/// actually written by the host.
pub(crate) struct Upstream {
    pub(crate) base_url: String,
    seen: Arc<Mutex<Option<(String, Value)>>>,
}

fn spawn_upstream(script: Vec<String>) -> Upstream {
    let listener = TcpListener::bind("127.0.0.1:0").expect("绑定本机端口");
    let port = listener.local_addr().expect("本机地址").port();
    let seen = Arc::new(Mutex::new(None));
    let recorder = Arc::clone(&seen);

    std::thread::spawn(move || {
        let Ok((mut stream, _)) = listener.accept() else {
            return;
        };
        let (auth, body) = read_request(&mut stream);
        *recorder.lock().unwrap_or_else(|p| p.into_inner()) = Some((auth, body));
        let mut response = String::from(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n",
        );
        for chunk in &script {
            response.push_str("data: ");
            response.push_str(chunk);
            response.push_str("\n\n");
        }
        response.push_str("data: [DONE]\n\n");
        let _ = stream.write_all(response.as_bytes());
        let _ = stream.flush();
    });

    Upstream {
        base_url: format!("http://127.0.0.1:{port}/v1"),
        seen,
    }
}

fn read_request(stream: &mut TcpStream) -> (String, Value) {
    let mut reader = BufReader::new(stream.try_clone().expect("克隆连接"));
    let mut length = 0usize;
    let mut auth = String::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            break;
        }
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            break;
        }
        let lower = trimmed.to_ascii_lowercase();
        if let Some(value) = lower.strip_prefix("content-length:") {
            length = value.trim().parse().unwrap_or(0);
        }
        if lower.starts_with("authorization:") {
            auth = trimmed[trimmed.find(':').map_or(0, |at| at + 1)..].trim().to_owned();
        }
    }
    let mut body = vec![0u8; length];
    let _ = reader.read_exact(&mut body);
    let value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    (auth, value)
}

fn chunk(delta: Value, finish: Option<&str>, usage: Option<Value>) -> String {
    let mut value = json!({
        "id": "chatcmpl-test",
        "object": "chat.completion.chunk",
        "created": 1,
        "model": "fixture-model",
        "choices": [{ "index": 0, "delta": delta, "finish_reason": finish }],
    });
    if let Some(usage) = usage {
        value["usage"] = usage;
    }
    value.to_string()
}

fn request_for(upstream: &Upstream, model_id: &str) -> StepRequest {
    StepRequest {
        family: Family::OpenaiCompatible,
        base_url: Some(upstream.base_url.clone()),
        api_key: Some("sk-fixture-not-a-real-key".into()),
        headers: Default::default(),
        settings: Default::default(),
        model_id: model_id.into(),
        system: None,
        system_dynamic: None,
        messages: vec![json!({ "role": "user", "content": "hi" })],
        tools: Vec::new(),
        tool_choice: None,
        max_steps: 1,
        max_output_tokens: None,
        temperature: None,
        reasoning: None,
        reasoning_content: None,
        prompt_cache: None,
        provider_options: None,
        native_search: None,
        agent: None,
    }
}

/// Event sink that returns an error after `cancel_after` text deltas, which
/// is how run cancellation is observable at this layer.
fn collecting_sink(
    events: Arc<Mutex<Vec<ModelStreamEvent>>>,
    cancel_after_text_deltas: Option<usize>,
) -> impl Fn(ModelStreamEvent) -> Result<(), String> + Send + Sync {
    let cancelled = AtomicBool::new(false);
    move |event| {
        let mut log = events.lock().unwrap_or_else(|p| p.into_inner());
        log.push(event);
        if let Some(limit) = cancel_after_text_deltas {
            let deltas = log
                .iter()
                .filter(|event| matches!(event, ModelStreamEvent::TextDelta { .. }))
                .count();
            if deltas >= limit {
                cancelled.store(true, Ordering::SeqCst);
            }
        }
        if cancelled.load(Ordering::SeqCst) {
            return Err("运行已取消".into());
        }
        Ok(())
    }
}

fn text_of(events: &[ModelStreamEvent]) -> String {
    events
        .iter()
        .filter_map(|event| match event {
            ModelStreamEvent::TextDelta { delta, .. } => Some(delta.as_str()),
            _ => None,
        })
        .collect()
}

#[test]
fn provider_key_environment_never_reaches_the_sidecar() {
    // The allowlist prevents unrelated development keys from reaching a
    // user-configured relay. A denylist silently expires as providers are added.
    // SAFETY: Set once in the test process.
    unsafe {
        std::env::set_var("OPENAI_API_KEY", "sk-should-never-leak");
        std::env::set_var("ANTHROPIC_API_KEY", "sk-ant-should-never-leak");
        std::env::set_var("AWS_SECRET_ACCESS_KEY", "should-never-leak");
    }
    let inherited = super::process::inherited_env();
    for (name, value) in &inherited {
        assert!(
            !value.contains("should-never-leak"),
            "{name} 把一把 Key 漏给了侧车"
        );
    }
    let names: Vec<&str> = inherited.iter().map(|(name, _)| name.as_str()).collect();
    for leaked in ["OPENAI_API_KEY", "ANTHROPIC_API_KEY", "AWS_SECRET_ACCESS_KEY"] {
        assert!(!names.contains(&leaked), "{leaked} 不该在继承名单里");
    }
    // The allowlist must retain enough environment for Node to start. On
    // Windows, Node crashes in `InitializeOncePerProcess` without SystemRoot.
    #[cfg(windows)]
    assert!(
        names.contains(&"SystemRoot"),
        "SystemRoot 必须继承，否则 Node 在进程自举阶段就崩"
    );
}

// ------------------------------------------------------------------ Regression tests

#[test]
fn a_step_streams_text_usage_and_a_continuation_block() {
    ensure_sidecar();
    let upstream = spawn_upstream(vec![
        chunk(json!({ "role": "assistant", "content": "" }), None, None),
        chunk(json!({ "content": "你好" }), None, None),
        chunk(json!({ "content": "，世界" }), None, None),
        chunk(
            json!({}),
            Some("stop"),
            Some(json!({ "prompt_tokens": 11, "completion_tokens": 7, "total_tokens": 18 })),
        ),
    ]);
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = collecting_sink(Arc::clone(&events), None);
    let sink: &ModelEventSink<'_> = &sink;

    let result = run_step(&request_for(&upstream, "text"), 0, sink).expect("一次成功的 step");

    assert_eq!(result.text, "你好，世界");
    let events = events.lock().unwrap_or_else(|p| p.into_inner());
    assert_eq!(text_of(&events), "你好，世界", "delta 必须拼回同一段正文");

    // Usage must be propagated through a stream event for gateway accounting.
    assert_eq!(result.usage.input_tokens, Some(11));
    assert_eq!(result.usage.output_tokens, Some(7));
    assert!(
        events
            .iter()
            .any(|event| matches!(event, ModelStreamEvent::UsageUpdated { .. })),
        "用量必须也走一次流事件"
    );

    // Continuation blocks carry provider state across rounds and must be nonempty.
    assert!(!result.response_messages.is_empty(), "续接块不能为空");

    // Authentication and request bodies are host-written bytes invisible to the UI.
    let seen = upstream.seen.lock().unwrap_or_else(|p| p.into_inner());
    let (auth, body) = seen.as_ref().expect("上游必须收到一次请求");
    assert_eq!(auth, "Bearer sk-fixture-not-a-real-key");
    assert_eq!(body["model"], "text");
}

#[test]
fn a_client_tool_call_comes_back_instead_of_being_executed() {
    ensure_sidecar();
    let upstream = spawn_upstream(vec![
        chunk(json!({ "role": "assistant", "content": "" }), None, None),
        chunk(
            json!({ "tool_calls": [{ "index": 0, "id": "call_1", "type": "function", "function": { "name": "ls", "arguments": "" } }] }),
            None,
            None,
        ),
        chunk(
            json!({ "tool_calls": [{ "index": 0, "function": { "arguments": "{\"path\":\"src\"}" } }] }),
            None,
            None,
        ),
        chunk(json!({}), Some("tool_calls"), None),
    ]);
    let mut request = request_for(&upstream, "tool");
    request.tools = vec![ToolSpec {
        name: "ls".into(),
        description: "列目录".into(),
        input_schema: json!({
            "type": "object",
            "properties": { "path": { "type": "string" } },
            "required": ["path"],
        }),
    }];

    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = collecting_sink(Arc::clone(&events), None);
    let sink: &ModelEventSink<'_> = &sink;
    let result = run_step(&request, 0, sink).expect("一次成功的 step");

    // Tool execution belongs to the host: the SDK must return calls rather
    // than execute them.
    assert_eq!(result.calls.len(), 1);
    assert_eq!(result.calls[0].tool_name, "ls");
    assert_eq!(result.calls[0].input["path"], "src");

    let events = events.lock().unwrap_or_else(|p| p.into_inner());
    assert!(
        events
            .iter()
            .any(|event| matches!(event, ModelStreamEvent::ToolCallAnnounced { tool_name, .. } if tool_name == "ls")),
        "必须先报名"
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, ModelStreamEvent::ToolCallArgumentsReady { .. })),
        "再给参数"
    );

    // The model-visible schema must be forwarded byte-for-byte. Re-deriving it
    // in the sidecar would diverge from the authoritative `builtin_schemas`.
    let seen = upstream.seen.lock().unwrap_or_else(|p| p.into_inner());
    let (_, body) = seen.as_ref().expect("上游必须收到一次请求");
    assert_eq!(
        body["tools"][0]["function"]["parameters"]["properties"]["path"]["type"],
        "string"
    );
}

#[test]
fn a_failing_sink_cancels_the_step_and_keeps_the_partial() {
    ensure_sidecar();
    // Cancellation must take effect before the upstream completes.
    let mut script = vec![chunk(json!({ "role": "assistant", "content": "" }), None, None)];
    for index in 0..400 {
        script.push(chunk(json!({ "content": format!("{index};") }), None, None));
    }
    script.push(chunk(json!({}), Some("stop"), None));
    let upstream = spawn_upstream(script);

    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = collecting_sink(Arc::clone(&events), Some(3));
    let sink: &ModelEventSink<'_> = &sink;

    let failure = run_step(&request_for(&upstream, "slow"), 0, sink)
        .expect_err("sink 报错必须让这一步失败");

    // Cancellation is not retryable; classifying it as an API error would issue
    // another request for a round that is already concluding.
    assert!(
        matches!(
            failure.error,
            crate::aisdk::ModelRequestError::EventSink(_)
        ),
        "取消必须以 EventSink 失败收场，实际为 {:?}",
        failure.error
    );
    // Preserve already streamed text because the renderer is displaying it.
    assert!(!failure.partial.text.is_empty(), "保留的部分正文不能为空");
}

#[test]
fn an_upstream_5xx_is_classified_retryable_and_a_4xx_is_not() {
    ensure_sidecar();
    for (status, retryable) in [("500 Internal Server Error", true), ("400 Bad Request", false)] {
        let listener = TcpListener::bind("127.0.0.1:0").expect("绑定本机端口");
        let port = listener.local_addr().expect("本机地址").port();
        std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            read_request(&mut stream);
            let body = json!({ "error": { "message": "夹具故障" } }).to_string();
            let _ = stream.write_all(
                format!(
                    "HTTP/1.1 {status}\r\nRetry-After: 60\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            );
        });

        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = collecting_sink(Arc::clone(&events), None);
        let sink: &ModelEventSink<'_> = &sink;
        let mut request = request_for(
            &Upstream {
                base_url: format!("http://127.0.0.1:{port}/v1"),
                seen: Arc::new(Mutex::new(None)),
            },
            "boom",
        );
        request.max_steps = 1;

        let failure = run_step(&request, 0, sink).expect_err("上游报错必须失败").sanitize(Some("test-key"));
        assert_eq!(failure.retry_after_ms, Some(60_000), "hint survives transport and sanitization");
        let is_retryable = matches!(
            failure.error,
            crate::aisdk::ModelRequestError::Api(_)
        );
        assert_eq!(
            is_retryable, retryable,
            "{status} 的重试判定错了：{:?}",
            failure.error
        );
    }
}

// ------------------------------------- Protocol adaptation regressions

/// DeepSeek Responses plaintext reasoning, carried in `content`.
/// The full chain must emit `ReasoningDelta` and retain nonempty
/// `StepResult.reasoning`.
#[test]
fn deepseek_responses_plaintext_reasoning_reaches_the_host() {
    ensure_sidecar();
    let upstream = spawn_upstream(vec![
        r#"{"type":"response.output_item.added","item":{"type":"reasoning","id":"rs_1","status":"in_progress","content":[],"summary":[],"encrypted_content":"resp-0"},"output_index":0,"sequence_number":2}"#.into(),
        r#"{"type":"response.content_part.added","content_index":0,"item_id":"rs_1","output_index":0,"part":{"type":"reasoning_text","text":""},"sequence_number":3}"#.into(),
        r#"{"type":"response.reasoning_text.delta","content_index":0,"delta":"先想","item_id":"rs_1","output_index":0,"sequence_number":4}"#.into(),
        r#"{"type":"response.reasoning_text.delta","content_index":0,"delta":"一想","item_id":"rs_1","output_index":0,"sequence_number":5}"#.into(),
        r#"{"type":"response.reasoning_text.done","content_index":0,"item_id":"rs_1","output_index":0,"sequence_number":6,"text":"先想一想"}"#.into(),
        r#"{"type":"response.content_part.done","content_index":0,"item_id":"rs_1","output_index":0,"part":{"type":"reasoning_text","text":"先想一想"},"sequence_number":7}"#.into(),
        r#"{"type":"response.output_item.done","item":{"type":"reasoning","id":"rs_1","status":"completed","content":[{"type":"reasoning_text","text":"先想一想"}],"summary":[],"encrypted_content":"resp-0"},"output_index":0,"sequence_number":8}"#.into(),
        r#"{"type":"response.output_item.added","item":{"type":"message","id":"msg_1","status":"in_progress","role":"assistant","content":[]},"output_index":1,"sequence_number":9}"#.into(),
        r#"{"type":"response.output_text.delta","content_index":0,"delta":"答案","item_id":"msg_1","logprobs":[],"output_index":1,"sequence_number":10}"#.into(),
        r#"{"type":"response.output_item.done","item":{"type":"message","id":"msg_1","status":"completed","role":"assistant","content":[{"type":"output_text","annotations":[],"logprobs":[],"text":"答案"}]},"output_index":1,"sequence_number":11}"#.into(),
    ]);
    let mut request = request_for(&upstream, "deepseek-reasoner");
    request.family = Family::OpenaiResponses;
    request.reasoning_content = Some("plaintext");

    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = collecting_sink(Arc::clone(&events), None);
    let sink: &ModelEventSink<'_> = &sink;
    let result = run_step(&request, 0, sink).expect("一次成功的 step");

    assert_eq!(result.reasoning, vec!["先想一想".to_owned()], "明文思考必须整段抵达结算");
    assert!(result.reasoning_ms.is_some(), "思考过就要有耗时");
    assert_eq!(result.text, "答案");

    let events = events.lock().unwrap_or_else(|p| p.into_inner());
    let streamed: String = events
        .iter()
        .filter_map(|event| match event {
            ModelStreamEvent::ReasoningDelta { delta, .. } => Some(delta.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(streamed, "先想一想", "思考 delta 必须流式抵达宿主");
    assert!(
        events.iter().any(|event| matches!(event, ModelStreamEvent::ReasoningStart { .. })),
        "思考开始信号必须在"
    );
}

/// Empty Anthropic thinking scaffolds must not produce reasoning lifecycle
/// events or `reasoningMs`; ordinary text must still arrive.
///
/// DeepSeek's Anthropic-compatible endpoint sends this on every tool
/// continuation: a thinking block whose only delta is a pseudo-signature, with
/// no plaintext frame and no thinking tokens. Nothing was thought.
/// `encrypted_anthropic_thinking_earns_a_card_without_any_summary` is the
/// contrasting shape that differs by exactly those two signals.
#[test]
fn an_empty_anthropic_thinking_scaffold_produces_no_reasoning_card() {
    ensure_sidecar();
    let upstream = spawn_upstream(vec![
        r#"{"type":"message_start","message":{"id":"m1","type":"message","role":"assistant","model":"fixture-model","content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":10,"cache_creation_input_tokens":0,"cache_read_input_tokens":0,"output_tokens":0}}}"#.into(),
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}"#.into(),
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"m1-pseudo-signature"}}"#.into(),
        r#"{"type":"content_block_stop","index":0}"#.into(),
        r#"{"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}"#.into(),
        r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"回答"}}"#.into(),
        r#"{"type":"content_block_stop","index":1}"#.into(),
        r#"{"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":5}}"#.into(),
        r#"{"type":"message_stop"}"#.into(),
    ]);
    let mut request = request_for(&upstream, "deepseek-reasoner");
    request.family = Family::Anthropic;

    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = collecting_sink(Arc::clone(&events), None);
    let sink: &ModelEventSink<'_> = &sink;
    let result = run_step(&request, 0, sink).expect("一次成功的 step");

    assert!(result.reasoning.is_empty());
    assert!(
        result.reasoning_ms.is_none(),
        "空脚手架不算思考过——`reasoningMs` 的存在性就是宿主的出卡判据"
    );
    assert_eq!(result.text, "回答");
    let events = events.lock().unwrap_or_else(|p| p.into_inner());
    assert!(
        !events.iter().any(|event| matches!(
            event,
            ModelStreamEvent::ReasoningStart { .. }
                | ModelStreamEvent::ReasoningDelta { .. }
                | ModelStreamEvent::ReasoningDone { .. }
        )),
        "空脚手架不得产生任何 reasoning 直播事件"
    );
}

/// Encrypted-only thinking: a relay in front of Claude strips the plaintext but
/// forwards the frame and the signature, and bills the thinking tokens.
///
/// The card must exist with a duration even though there is nothing to display,
/// and the signature must reach the opaque continuation so the next request can
/// replay it.
#[test]
fn encrypted_anthropic_thinking_earns_a_card_without_any_summary() {
    ensure_sidecar();
    let upstream = spawn_upstream(vec![
        r#"{"type":"message_start","message":{"id":"m3","type":"message","role":"assistant","model":"fixture-model","content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":10,"cache_creation_input_tokens":0,"cache_read_input_tokens":0,"output_tokens":0}}}"#.into(),
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}"#.into(),
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":""}}"#.into(),
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"CAIS-opaque-blob"}}"#.into(),
        r#"{"type":"content_block_stop","index":0}"#.into(),
        r#"{"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}"#.into(),
        r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"回答"}}"#.into(),
        r#"{"type":"content_block_stop","index":1}"#.into(),
        r#"{"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":5,"output_tokens_details":{"thinking_tokens":75}}}"#.into(),
        r#"{"type":"message_stop"}"#.into(),
    ]);
    let mut request = request_for(&upstream, "claude-opus-5");
    request.family = Family::Anthropic;

    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = collecting_sink(Arc::clone(&events), None);
    let sink: &ModelEventSink<'_> = &sink;
    let result = run_step(&request, 0, sink).expect("一次成功的 step");

    assert_eq!(result.reasoning, [""], "密文思考保留空摘要槽位，不压缩 item 身份");
    assert!(
        result.reasoning_ms.is_some(),
        "密文思考也思考过——`reasoningMs` 的存在性就是宿主的出卡判据"
    );
    assert_eq!(result.usage.reasoning_tokens, Some(75));
    assert_eq!(result.text, "回答");
    let continuation = serde_json::to_string(&result.response_messages).unwrap();
    assert!(
        continuation.contains("CAIS-opaque-blob"),
        "密文必须进不透明续接块，否则下一轮回放会丢掉思考：{continuation}"
    );
    let events = events.lock().unwrap_or_else(|p| p.into_inner());
    assert!(
        events.iter().any(|event| matches!(event, ModelStreamEvent::ReasoningStart { .. })),
        "密文思考必须有开始信号，否则直播期整段隐形"
    );
}

#[test]
fn redacted_reasoning_form_reaches_the_host_without_losing_its_empty_slot() {
    ensure_sidecar();
    let upstream = spawn_upstream(vec![
        r#"{"type":"message_start","message":{"id":"mr","type":"message","role":"assistant","model":"fixture-model","content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":1,"output_tokens":0}}}"#.into(),
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"redacted_thinking","data":"opaque-redacted"}}"#.into(),
        r#"{"type":"content_block_stop","index":0}"#.into(),
        r#"{"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}"#.into(),
        r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"answer"}}"#.into(),
        r#"{"type":"content_block_stop","index":1}"#.into(),
        r#"{"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":2}}"#.into(),
        r#"{"type":"message_stop"}"#.into(),
    ]);
    let mut request = request_for(&upstream, "claude-opus-5");
    request.family = Family::Anthropic;
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = collecting_sink(Arc::clone(&events), None);
    let result = run_step(&request, 0, &sink).unwrap();
    assert_eq!(result.reasoning, [""]);
    assert!(events.lock().unwrap().iter().any(|event| matches!(event,
        ModelStreamEvent::ReasoningStart { item: 0, form: Some(crate::model::ReasoningForm::Encrypted), .. }
    )));
    assert!(serde_json::to_string(&result.response_messages).unwrap().contains("opaque-redacted"));
}

/// Nonempty `thinking_delta` values must pass the evidence gate.
#[test]
fn real_anthropic_thinking_still_streams_through_the_gate() {
    ensure_sidecar();
    let upstream = spawn_upstream(vec![
        r#"{"type":"message_start","message":{"id":"m2","type":"message","role":"assistant","model":"fixture-model","content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":10,"cache_creation_input_tokens":0,"cache_read_input_tokens":0,"output_tokens":0}}}"#.into(),
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}"#.into(),
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"认真想"}}"#.into(),
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig"}}"#.into(),
        r#"{"type":"content_block_stop","index":0}"#.into(),
        r#"{"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}"#.into(),
        r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"答"}}"#.into(),
        r#"{"type":"content_block_stop","index":1}"#.into(),
        r#"{"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":5}}"#.into(),
        r#"{"type":"message_stop"}"#.into(),
    ]);
    let mut request = request_for(&upstream, "deepseek-reasoner");
    request.family = Family::Anthropic;

    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = collecting_sink(Arc::clone(&events), None);
    let sink: &ModelEventSink<'_> = &sink;
    let result = run_step(&request, 0, sink).expect("一次成功的 step");

    assert_eq!(result.reasoning, vec!["认真想".to_owned()]);
    assert!(result.reasoning_ms.is_some());
    let events = events.lock().unwrap_or_else(|p| p.into_inner());
    assert!(
        events.iter().any(|event| matches!(event, ModelStreamEvent::ReasoningStart { .. })),
        "真思考必须有开始信号"
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, ModelStreamEvent::ReasoningDelta { delta, .. } if delta == "认真想")),
    );
}

/// Legacy `function_call` frames must be translated to `tool_calls` with a
/// deterministic ID. Requests must explicitly enable usage in the final frame.
#[test]
fn a_legacy_function_call_dialect_still_yields_a_tool_call() {
    ensure_sidecar();
    let upstream = spawn_upstream(vec![
        chunk(json!({ "role": "assistant", "content": "" }), None, None),
        chunk(json!({ "function_call": { "name": "ls", "arguments": "" } }), None, None),
        chunk(json!({ "function_call": { "arguments": "{\"path\":\"src\"}" } }), None, None),
        chunk(json!({}), Some("function_call"), None),
    ]);
    let mut request = request_for(&upstream, "legacy");
    request.tools = vec![ToolSpec {
        name: "ls".into(),
        description: "列目录".into(),
        input_schema: json!({
            "type": "object",
            "properties": { "path": { "type": "string" } },
            "required": ["path"],
        }),
    }];

    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = collecting_sink(Arc::clone(&events), None);
    let sink: &ModelEventSink<'_> = &sink;
    let result = run_step(&request, 0, sink).expect("一次成功的 step");

    assert_eq!(result.calls.len(), 1, "旧方言的调用不能被吞");
    assert_eq!(result.calls[0].tool_name, "ls");
    assert_eq!(result.calls[0].input["path"], "src");
    assert!(
        result.calls[0].call_id.starts_with("legacy_"),
        "id 由方言层确定性合成，实际为 {}",
        result.calls[0].call_id
    );

    let seen = upstream.seen.lock().unwrap_or_else(|p| p.into_inner());
    let (_, body) = seen.as_ref().expect("上游必须收到一次请求");
    assert_eq!(
        body["stream_options"]["include_usage"], true,
        "chat 腿必须显式请求 usage 终帧"
    );
}

/// An SSE rate-limit error must be retryable, like an HTTP 429 response.
#[test]
fn an_in_band_rate_limit_error_is_classified_transient() {
    ensure_sidecar();
    let upstream = spawn_upstream(vec![
        chunk(json!({ "role": "assistant", "content": "开头" }), None, None),
        r#"{"error":{"message":"Rate limit reached","type":"rate_limit_exceeded","code":"rate_limit_exceeded"}}"#.into(),
    ]);
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = collecting_sink(Arc::clone(&events), None);
    let sink: &ModelEventSink<'_> = &sink;

    let failure = run_step(&request_for(&upstream, "limited"), 0, sink)
        .expect_err("流内错误必须让这一步失败");
    assert!(
        matches!(failure.error, crate::aisdk::ModelRequestError::Api(_)),
        "流内限流必须可重试，实际为 {:?}",
        failure.error
    );
}

/// Each upstream thinking block is an independent item. Ordinals keep streamed
/// reasoning cards aligned with separately settled segments.
#[test]
fn interleaved_reasoning_items_keep_their_own_ordinals_and_segments() {
    ensure_sidecar();
    let upstream = spawn_upstream(vec![
        r#"{"type":"message_start","message":{"id":"m3","type":"message","role":"assistant","model":"fixture-model","content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":10,"cache_creation_input_tokens":0,"cache_read_input_tokens":0,"output_tokens":0}}}"#.into(),
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}"#.into(),
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"第一段"}}"#.into(),
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"m3-pseudo-signature-0"}}"#.into(),
        r#"{"type":"content_block_stop","index":0}"#.into(),
        r#"{"type":"content_block_start","index":1,"content_block":{"type":"thinking","thinking":"","signature":""}}"#.into(),
        r#"{"type":"content_block_delta","index":1,"delta":{"type":"thinking_delta","thinking":"第二段"}}"#.into(),
        r#"{"type":"content_block_delta","index":1,"delta":{"type":"signature_delta","signature":"m3-pseudo-signature-1"}}"#.into(),
        r#"{"type":"content_block_stop","index":1}"#.into(),
        r#"{"type":"content_block_start","index":2,"content_block":{"type":"text","text":""}}"#.into(),
        r#"{"type":"content_block_delta","index":2,"delta":{"type":"text_delta","text":"回答"}}"#.into(),
        r#"{"type":"content_block_stop","index":2}"#.into(),
        r#"{"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":5}}"#.into(),
        r#"{"type":"message_stop"}"#.into(),
    ]);
    let mut request = request_for(&upstream, "deepseek-reasoner");
    request.family = Family::Anthropic;

    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = collecting_sink(Arc::clone(&events), None);
    let sink: &ModelEventSink<'_> = &sink;
    let result = run_step(&request, 0, sink).expect("一次成功的 step");

    assert_eq!(result.reasoning, vec!["第一段", "第二段"]);
    let events = events.lock().unwrap_or_else(|p| p.into_inner());
    let lifecycle: Vec<(usize, &str)> = events
        .iter()
        .filter_map(|event| match event {
            ModelStreamEvent::ReasoningStart { item, .. } => Some((*item, "start")),
            ModelStreamEvent::ReasoningDelta { item, .. } => Some((*item, "delta")),
            ModelStreamEvent::ReasoningDone { item, .. } => Some((*item, "done")),
            _ => None,
        })
        .collect();
    assert_eq!(
        lifecycle,
        vec![
            (0, "start"),
            (0, "delta"),
            (0, "done"),
            (1, "start"),
            (1, "delta"),
            (1, "done"),
        ],
        "两段思考的生命周期必须按各自 ordinal 成组抵达"
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            ModelStreamEvent::ReasoningDelta { item: 0, delta, .. } if delta == "第一段"
        )),
        "第一段不能被记到第二个 item"
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            ModelStreamEvent::ReasoningDelta { item: 1, delta, .. } if delta == "第二段"
        )),
        "第二段不能被记到第一个 item"
    );
}

/// Preserve the raw finish reason alongside AI SDK normalization so a pause
/// message is not mistaken for a final result.
#[test]
fn the_raw_finish_reason_survives_normalization() {
    ensure_sidecar();
    for (raw, normalized) in [("pause_turn", "stop"), ("max_tokens", "length"), ("model_context_window_exceeded", "length")] {
    let upstream = spawn_upstream(vec![
        r#"{"type":"message_start","message":{"id":"m4","type":"message","role":"assistant","model":"fixture-model","content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":10,"cache_creation_input_tokens":0,"cache_read_input_tokens":0,"output_tokens":0}}}"#.into(),
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#.into(),
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"稍等"}}"#.into(),
        r#"{"type":"content_block_stop","index":0}"#.into(),
        json!({"type":"message_delta","delta":{"stop_reason":raw,"stop_sequence":null},"usage":{"output_tokens":5}}).to_string(),
        r#"{"type":"message_stop"}"#.into(),
    ]);
    let mut request = request_for(&upstream, "deepseek-reasoner");
    request.family = Family::Anthropic;

    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = collecting_sink(Arc::clone(&events), None);
    let sink: &ModelEventSink<'_> = &sink;
    let result = run_step(&request, 0, sink).expect("一次成功的 step");

    assert_eq!(result.finish_reason.as_deref(), Some(normalized));
    assert_eq!(result.raw_finish_reason.as_deref(), Some(raw));
    }
}

/// `@ai-sdk/anthropic` replays reasoning only with a signature or redacted
/// data. Unsigned reasoning parts are dropped before the request, while the
/// visible text of the same assistant message survives.
#[test]
fn an_unsigned_reasoning_part_is_dropped_from_replay_without_a_forged_signature() {
    ensure_sidecar();
    let upstream = spawn_upstream(vec![
        r#"{"type":"message_start","message":{"id":"m5","type":"message","role":"assistant","model":"fixture-model","content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":10,"cache_creation_input_tokens":0,"cache_read_input_tokens":0,"output_tokens":0}}}"#.into(),
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#.into(),
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"继续回答"}}"#.into(),
        r#"{"type":"content_block_stop","index":0}"#.into(),
        r#"{"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":5}}"#.into(),
        r#"{"type":"message_stop"}"#.into(),
    ]);
    let mut request = request_for(&upstream, "deepseek-reasoner");
    request.family = Family::Anthropic;
    request.messages = vec![
        json!({ "role": "user", "content": "继续" }),
        json!({
            "role": "assistant",
            "content": [
                { "type": "reasoning", "text": "先想一步" },
                { "type": "text", "text": "好的" },
            ],
        }),
        json!({ "role": "user", "content": "然后呢" }),
    ];

    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = collecting_sink(Arc::clone(&events), None);
    let sink: &ModelEventSink<'_> = &sink;
    run_step(&request, 0, sink).expect("一次成功的 step");

    let seen = upstream.seen.lock().unwrap_or_else(|p| p.into_inner());
    let (_, body) = seen.as_ref().expect("上游必须收到一次请求");
    let assistant_content = body["messages"]
        .as_array()
        .expect("Anthropic 请求必须有 messages")
        .iter()
        .find(|message| message["role"] == "assistant")
        .and_then(|message| message["content"].as_array())
        .expect("助手历史必须仍在请求里");
    assert!(
        !assistant_content.iter().any(|part| part["type"] == "thinking"),
        "无签名思考不得带伪造签名回放，必须在请求前被过滤：{assistant_content:?}"
    );
    assert!(
        !assistant_content.iter().any(|part| part["signature"] == "mework-unsigned"),
        "哨兵签名 mework-unsigned 不得再出现在上游请求里：{assistant_content:?}"
    );
    assert!(
        assistant_content
            .iter()
            .any(|part| part["type"] == "text" && part["text"] == "好的"),
        "同一条助手历史中的正文不能随思考一起丢失"
    );
}

/// The whole cross-turn chain Claude Code implements in one process: the
/// signed thinking block of turn 1 comes back inside the continuation, the host
/// stores it on the reasoning card, history projects it, and turn 2's request
/// carries the very same signature — for the same model. A different model
/// gets the block dropped, never a fabricated signature.
#[test]
fn a_signed_thinking_block_is_replayed_verbatim_on_the_next_turn() {
    ensure_sidecar();
    let signed_turn = || {
        vec![
            r#"{"type":"message_start","message":{"id":"m7","type":"message","role":"assistant","model":"fixture-model","content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":10,"cache_creation_input_tokens":0,"cache_read_input_tokens":0,"output_tokens":0}}}"#.to_owned(),
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}"#.to_owned(),
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"先看文件"}}"#.to_owned(),
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"CAIS-turn-one"}}"#.to_owned(),
            r#"{"type":"content_block_stop","index":0}"#.to_owned(),
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}"#.to_owned(),
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"看完了"}}"#.to_owned(),
            r#"{"type":"content_block_stop","index":1}"#.to_owned(),
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":5}}"#.to_owned(),
            r#"{"type":"message_stop"}"#.to_owned(),
        ]
    };

    // Turn 1: the sidecar hands back the signed block inside the continuation.
    let first = spawn_upstream(signed_turn());
    let mut request = request_for(&first, "claude-opus-5");
    request.family = Family::Anthropic;
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = collecting_sink(Arc::clone(&events), None);
    let sink: &ModelEventSink<'_> = &sink;
    let result = run_step(&request, 0, sink).expect("第一轮 step");
    assert_eq!(result.reasoning, vec!["先看文件".to_owned()]);

    // The host mints the round's cards with the continuation's signed parts.
    let continuation = Value::Array(result.response_messages.clone());
    let parts = crate::api::reasoning_replay_parts(&continuation);
    assert_eq!(parts.len(), 1, "续接块里恰有一个签名思考部件：{continuation}");
    let contexts = vec![
        crate::model::ContextItem::User {
            id: "ctx_u1".into(),
            content: "看看文件".into(),
            images: Vec::new(),
            created_at: "2026-09-03T00:00:00Z".into(),
        },
        crate::model::ContextItem::Reasoning {
            id: "ctx_r1".into(),
            content: Some("先看文件".into()),
            form: Some(crate::model::ReasoningForm::Plaintext),
            round: Some(1),
            model_turn_id: Some("turn-1".into()),
            interrupted: false,
            duration_ms: result.reasoning_ms,
            tokens: None,
            replay: Some(crate::model::ReasoningReplay {
                model: "claude-opus-5".into(),
                parts,
            }),
            created_at: "2026-09-03T00:00:01Z".into(),
        },
        crate::model::ContextItem::Assistant {
            id: "ctx_a1".into(),
            content: result.text.clone(),
            round: Some(1),
            model_turn_id: Some("turn-1".into()),
            interrupted: false,
            sources: Vec::new(),
            created_at: "2026-09-03T00:00:02Z".into(),
        },
        crate::model::ContextItem::User {
            id: "ctx_u2".into(),
            content: "然后呢".into(),
            images: Vec::new(),
            created_at: "2026-09-03T00:00:03Z".into(),
        },
    ];
    let history = super::project::project_messages(Family::Anthropic, &contexts);

    // Turn 2, same model: the wire carries the original signature and no host tag.
    let second = spawn_upstream(signed_turn());
    let mut replay = request_for(&second, "claude-opus-5");
    replay.family = Family::Anthropic;
    replay.messages = history.clone();
    run_step(&replay, 0, sink).expect("第二轮 step");
    let seen = second.seen.lock().unwrap_or_else(|p| p.into_inner());
    let (_, body) = seen.as_ref().expect("上游必须收到第二轮请求");
    let assistant = body["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .find(|message| message["role"] == "assistant")
        .and_then(|message| message["content"].as_array())
        .expect("助手历史");
    assert!(
        assistant.iter().any(|block| {
            block["type"] == "thinking"
                && block["thinking"] == "先看文件"
                && block["signature"] == "CAIS-turn-one"
        }),
        "第二轮必须原样回放第一轮的签名思考块：{assistant:?}"
    );
    assert!(
        !body.to_string().contains("mework"),
        "宿主的模型标记与哨兵都不得进入请求：{body}"
    );
    drop(seen);

    // Turn 2, another model: the signed block is dropped, nothing is invented.
    let third = spawn_upstream(signed_turn());
    let mut switched = request_for(&third, "claude-sonnet-5");
    switched.family = Family::Anthropic;
    switched.messages = history;
    run_step(&switched, 0, sink).expect("换模型的第二轮 step");
    let seen = third.seen.lock().unwrap_or_else(|p| p.into_inner());
    let (_, body) = seen.as_ref().expect("上游必须收到换模型请求");
    let assistant = body["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .find(|message| message["role"] == "assistant")
        .and_then(|message| message["content"].as_array())
        .expect("助手历史");
    assert!(
        assistant.iter().all(|block| block["type"] != "thinking"),
        "换模型后不得回放别的模型签的思考块：{assistant:?}"
    );
    assert!(
        assistant.iter().any(|block| block["type"] == "text" && block["text"] == "看完了"),
        "正文照旧回放：{assistant:?}"
    );
    assert!(!body.to_string().contains("mework-unsigned"), "换模型不是伪造签名的理由：{body}");
}

/// No-index tool fragments must be tracked by response ID so interleaved calls
/// retain their own argument buffers.
#[test]
fn interleaved_no_index_tool_call_fragments_do_not_cross_wire() {
    ensure_sidecar();
    let upstream = spawn_upstream(vec![
        r#"{"id":"cmpl-1","object":"chat.completion.chunk","created":1,"model":"m","choices":[{"index":0,"delta":{"tool_calls":[{"id":"call_a","type":"function","function":{"name":"alpha","arguments":"{\"a\":"}}]},"finish_reason":null}]}"#.into(),
        r#"{"id":"cmpl-1","object":"chat.completion.chunk","created":1,"model":"m","choices":[{"index":0,"delta":{"tool_calls":[{"id":"call_b","type":"function","function":{"name":"beta","arguments":"{\"b\":"}}]},"finish_reason":null}]}"#.into(),
        r#"{"id":"cmpl-1","object":"chat.completion.chunk","created":1,"model":"m","choices":[{"index":0,"delta":{"tool_calls":[{"id":"call_a","type":"function","function":{"arguments":"1}"}}]},"finish_reason":null}]}"#.into(),
        r#"{"id":"cmpl-1","object":"chat.completion.chunk","created":1,"model":"m","choices":[{"index":0,"delta":{"tool_calls":[{"id":"call_b","type":"function","function":{"arguments":"2}"}}]},"finish_reason":null}]}"#.into(),
        r#"{"id":"cmpl-1","object":"chat.completion.chunk","created":1,"model":"m","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#.into(),
    ]);
    let mut request = request_for(&upstream, "interleaved-tools");
    request.tools = vec![
        ToolSpec {
            name: "alpha".into(),
            description: "alpha 调用".into(),
            input_schema: json!({
                "type": "object",
                "properties": { "a": { "type": "number" } },
                "required": ["a"],
            }),
        },
        ToolSpec {
            name: "beta".into(),
            description: "beta 调用".into(),
            input_schema: json!({
                "type": "object",
                "properties": { "b": { "type": "number" } },
                "required": ["b"],
            }),
        },
    ];

    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = collecting_sink(Arc::clone(&events), None);
    let sink: &ModelEventSink<'_> = &sink;
    let result = run_step(&request, 0, sink).expect("一次成功的 step");

    assert_eq!(result.calls.len(), 2);
    assert_eq!(result.calls[0].tool_name, "alpha");
    assert_eq!(result.calls[0].input, json!({ "a": 1 }));
    assert_eq!(result.calls[1].tool_name, "beta");
    assert_eq!(result.calls[1].input, json!({ "b": 2 }));
}

// ------------------------------------------------- Mock upstreams for API retry tests
//
// They share the sidecar setup and failure discipline defined here.

/// An HTTP response. Headers must use CRLF: bare-LF responses can cause some
/// HTTP clients to wait indefinitely rather than report a parse error.
fn http_response(status_line: &str, content_type: &str, body: &str) -> String {
    let mut out = String::new();
    out.push_str("HTTP/1.1 ");
    out.push_str(status_line);
    out.push_str("\r\ncontent-type: ");
    out.push_str(content_type);
    out.push_str("\r\ncontent-length: ");
    out.push_str(&body.len().to_string());
    out.push_str("\r\nconnection: close\r\n\r\n");
    out.push_str(body);
    out
}

const UPSTREAM_ERROR_BODY: &str = r#"{"error":{"message":"retry me","type":"server_error"}}"#;

/// A mock upstream that always returns 503, which the sidecar classifies as
/// transient so the host retry loop issues another attempt.
pub(crate) fn always_failing_upstream() -> Upstream {
    let listener = TcpListener::bind("127.0.0.1:0").expect("绑定本机端口");
    let port = listener.local_addr().expect("本机地址").port();
    std::thread::spawn(move || {
        for connection in listener.incoming() {
            let Ok(mut stream) = connection else { break };
            let _ = read_request(&mut stream);
            let response = http_response(
                "503 Service Unavailable",
                "application/json",
                UPSTREAM_ERROR_BODY,
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }
    });
    Upstream {
        base_url: format!("http://127.0.0.1:{port}/v1"),
        seen: Arc::new(Mutex::new(None)),
    }
}

/// A mock upstream that returns 503 once, then succeeds.
///
/// Successful usage differs from failed-attempt usage so accidental aggregation
/// has a visible symptom.
pub(crate) fn failing_then_succeeding_upstream() -> Upstream {
    let listener = TcpListener::bind("127.0.0.1:0").expect("绑定本机端口");
    let port = listener.local_addr().expect("本机地址").port();
    std::thread::spawn(move || {
        let mut attempt = 0usize;
        for connection in listener.incoming() {
            let Ok(mut stream) = connection else { break };
            let _ = read_request(&mut stream);
            attempt += 1;
            if attempt == 1 {
                let response = http_response(
                    "503 Service Unavailable",
                    "application/json",
                    UPSTREAM_ERROR_BODY,
                );
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
                continue;
            }
            let mut body = String::new();
            for part in [
                chunk(json!({ "role": "assistant", "content": "" }), None, None),
                chunk(json!({ "content": "ok" }), None, None),
                chunk(
                    json!({}),
                    Some("stop"),
                    Some(json!({ "prompt_tokens": 2, "completion_tokens": 4, "total_tokens": 6 })),
                ),
            ] {
                body.push_str("data: ");
                body.push_str(&part);
                body.push_str("\n\n");
            }
            body.push_str("data: [DONE]\n\n");
            // SSE bodies use LF delimiters, but HTTP headers must use CRLF.
            let mut response = String::from(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n",
            );
            response.push_str(&body);
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }
    });
    Upstream {
        base_url: format!("http://127.0.0.1:{port}/v1"),
        seen: Arc::new(Mutex::new(None)),
    }
}
