//! Hermetic regression tests for subagent message integrity.

use super::*;

/// Route parent and child rounds without depending on arrival order.
///
/// Identify child rounds from their system prompt. Choose their response by
/// request content, not arrival order, because `run_model` retries 5xx failures.
/// Server threads are not joined because the accept loop has no natural end;
/// the generous connection limit lets it eventually exit after abnormal traffic.
fn serve_routed_with_child_failures(
    listener: TcpListener,
    parent_responses: Vec<Value>,
    success_needle: &'static str,
    success_body: Value,
    child_delay: Duration,
) -> Arc<Mutex<Vec<String>>> {
    /// Exceeds the expected connection count and gives the accept loop an end.
    const MAX_CONNECTIONS: usize = 64;
    let child_bodies: Arc<Mutex<Vec<String>>> = Arc::default();
    let seen = Arc::clone(&child_bodies);
    thread::spawn(move || {
        let mut parent_queue = parent_responses.into_iter();
        for _ in 0..MAX_CONNECTIONS {
            let Ok((mut stream, _)) = listener.accept() else {
                break;
            };
            let body = read_http_request_with_body(&mut stream);
            if body.contains("You are a child agent spawned by the main agent")
                || body.contains("你是主代理派生的子代理")
            {
                let first = {
                    let mut recorded = seen.lock().unwrap_or_else(|p| p.into_inner());
                    let first = recorded.is_empty();
                    recorded.push(body.clone());
                    first
                };
                if first {
                    thread::sleep(child_delay);
                }
                if body.contains(success_needle) {
                    write_json_response(&mut stream, "200 OK", &success_body);
                } else {
                    write_json_response(
                        &mut stream,
                        "500 Internal Server Error",
                        &json!({"error": {"message": "provider is rate limiting this key"}}),
                    );
                }
                continue;
            }
            let Some(response) = parent_queue.next() else {
                // Return a terminating empty response when parent responses are exhausted.
                write_json_response(&mut stream, "200 OK", &responses_text_output("done"));
                continue;
            };
            write_json_response(&mut stream, "200 OK", &response);
        }
    });
    child_bodies
}

fn agent_record<'a>(contexts: &'a [ContextItem], name: &str) -> Option<&'a SubagentRunRecord> {
    let mut found = None;
    for context in contexts {
        if let ContextItem::Tool {
            subagent: Some(record),
            ..
        } = context
        {
            if record.name.as_deref() == Some(name) {
                found = Some(record);
            }
        }
    }
    found
}

fn tool_outputs(contexts: &[ContextItem], tool: &str) -> Vec<String> {
    contexts
        .iter()
        .filter_map(|context| match context {
            ContextItem::Tool {
                tool_name, result, ..
            } if tool_name == tool => Some(result.output.clone()),
            _ => None,
        })
        .collect()
}

/// A child provider failure must surface its cause to the parent and user.
#[test]
fn a_child_provider_error_must_surface_its_cause() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let _child_bodies = serve_routed_with_child_failures(
        listener,
        vec![
            json!({
                "model": "model-test",
                "status": "completed",
                "output": [
                    {"type":"function_call","status":"completed","call_id":"call-spawn","name":"agent_spawn",
                        "arguments": json!({"prompt":"初始任务","name":"helper"}).to_string()},
                    {"type":"function_call","status":"completed","call_id":"call-wait","name":"task_wait",
                        "arguments": json!({"timeout_seconds": 30}).to_string()}
                ],
                "usage": {"input_tokens":1,"output_tokens":1,"total_tokens":2}
            }),
            responses_text_output("主代理收尾"),
        ],
        // Use an impossible needle so every child round receives a rate-limit failure.
        "NEVER-MATCHES-ANY-CHILD-REQUEST",
        responses_text_output("unused"),
        Duration::from_millis(50),
    );

    let workspace = tempfile::tempdir().unwrap();
    let request = loop_request_for(address, workspace.path());
    let response = run_model(request, &AppState::default(), &discard_event, &approve_tool).unwrap();

    let wait_output = tool_outputs(&response.contexts, "task_wait").join("\n");
    let record = agent_record(&response.contexts, "helper").expect("helper 必须留下一条累计记录");

    eprintln!(
        "[integrity] status={:?}｜task_wait 回执：{wait_output}",
        record.status
    );

    assert_eq!(
        record.status,
        SubagentRunStatus::Failed,
        "提供商错误是失败，不是「中断」——中断是任务级停止的语义"
    );
    assert!(
        wait_output.contains("500") || wait_output.contains("rate limiting"),
        "子回合失败的真实原因必须传到模型/用户面前，实际只有：{wait_output}"
    );
}
