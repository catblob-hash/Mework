//! Hermetic regression tests for subagent message integrity.

use super::*;
use std::sync::Condvar;

/// Fallback timeout for barrier orchestration. It must remain well below the
/// parent round's 30-second `task_wait`, so a barrier timeout identifies the
/// orchestration failure rather than surfacing first as `task_wait` timeout.
const GATE_TIMEOUT: Duration = Duration::from_secs(10);

/// One-shot gate backed by `Mutex<GateState>` and `Condvar`.
///
/// Use a barrier rather than `thread::sleep`: only a barrier establishes the
/// required ordering, while a delay merely depends on scheduling.
#[derive(Default)]
struct Gate {
    state: Mutex<GateState>,
    changed: Condvar,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum GateState {
    #[default]
    Waiting,
    Open,
    /// A waiter has exhausted the timeout. Later waiters must fail immediately
    /// so one orchestration failure is not charged repeatedly.
    Abandoned,
}

impl Gate {
    fn open(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if *state == GateState::Waiting {
            *state = GateState::Open;
        }
        drop(state);
        self.changed.notify_all();
    }

    /// Wait for the gate to open. Returning `false` records an orchestration
    /// failure instead of allowing a server thread to hang the test suite.
    fn wait(&self, deadline: Duration) -> bool {
        let started = Instant::now();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        while *state == GateState::Waiting {
            let Some(left) = deadline.checked_sub(started.elapsed()) else {
                *state = GateState::Abandoned;
                break;
            };
            let (guard, timeout) = self
                .changed
                .wait_timeout(state, left)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state = guard;
            if timeout.timed_out() && *state == GateState::Waiting {
                *state = GateState::Abandoned;
            }
        }
        *state == GateState::Open
    }
}

/// Condition-based rather than timed waiting. Polling affects only how quickly
/// this test continues, not its outcome.
fn wait_until(deadline: Duration, condition: &(dyn Fn() -> bool + Send + Sync)) -> bool {
    let started = Instant::now();
    while started.elapsed() < deadline {
        if condition() {
            return true;
        }
        thread::sleep(Duration::from_millis(2));
    }
    condition()
}

/// Observations collected by the fake upstream for the followup race test.
#[derive(Default)]
struct RaceObservations {
    /// Child round request bodies in arrival order.
    child_bodies: Mutex<Vec<String>>,
    /// Parent request count, which also identifies the next parent round because
    /// parent rounds are strictly serial.
    parent_requests: Mutex<usize>,
    /// Barrier failures. Read these before downstream assertions because they
    /// establish whether the intended interleaving occurred.
    gate_faults: Mutex<Vec<String>>,
}

impl RaceObservations {
    /// Record a child request and return its ordinal.
    fn record_child(&self, body: &str) -> usize {
        let mut bodies = self
            .child_bodies
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        bodies.push(body.to_owned());
        bodies.len() - 1
    }

    /// Record a parent request and return its ordinal.
    fn record_parent(&self) -> usize {
        let mut seen = self
            .parent_requests
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let ordinal = *seen;
        *seen += 1;
        ordinal
    }

    fn fault(&self, note: &str) {
        self.gate_faults
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(note.to_owned());
    }

    fn child_bodies(&self) -> Vec<String> {
        self.child_bodies
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn parent_requests(&self) -> usize {
        *self
            .parent_requests
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn gate_faults(&self) -> Vec<String> {
        self.gate_faults
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

/// Serve a followup race with barriers that guarantee followups are queued
/// while the child round is in flight.
///
/// Parent, worker, and fake-upstream threads determine the interleaving. Barrier
/// A holds parent requests after the spawn round until the first child request
/// arrives, proving the child request body is frozen. Barrier B holds that child
/// request's failure response until both followups are queued.
///
/// Each connection needs its own thread: either the parent or child request may
/// arrive first, so sequential handling can deadlock. Threads are not joined;
/// `accept` has no natural completion point. `MAX_CONNECTIONS` bounds abnormal
/// traffic rather than defining normal completion.
fn serve_followup_race(
    listener: TcpListener,
    parent_responses: Vec<Value>,
    child_success_needle: &'static str,
    child_success_body: Value,
    followups_queued: Arc<dyn Fn() -> bool + Send + Sync>,
) -> Arc<RaceObservations> {
    /// Exceeds the expected connection count, including `run_model` 5xx retries.
    const MAX_CONNECTIONS: usize = 64;
    let observations = Arc::new(RaceObservations::default());
    let served = Arc::clone(&observations);
    let child_arrived = Arc::new(Gate::default());
    let parent_queue = Arc::new(Mutex::new(parent_responses.into_iter()));
    let success_body = Arc::new(child_success_body);
    thread::spawn(move || {
        for _ in 0..MAX_CONNECTIONS {
            let Ok((mut stream, _)) = listener.accept() else { break };
            let observations = Arc::clone(&served);
            let child_arrived = Arc::clone(&child_arrived);
            let parent_queue = Arc::clone(&parent_queue);
            let followups_queued = Arc::clone(&followups_queued);
            let success_body = Arc::clone(&success_body);
            thread::spawn(move || {
                let body = read_http_request_with_body(&mut stream);
                if body.contains("You are a child agent spawned by the main agent")
                    || body.contains("你是主代理派生的子代理")
                {
                    if observations.record_child(&body) == 0 {
                        child_arrived.open();
                        // Barrier B: the request body is fixed; now wait for queued followups.
                        if !wait_until(GATE_TIMEOUT, followups_queued.as_ref()) {
                            observations.fault(
                                "屏障 B：子回合 1 已经在飞，但两条 followup 迟迟没有进 followups 队列",
                            );
                        }
                    }
                    if body.contains(child_success_needle) {
                        write_json_response(&mut stream, "200 OK", &success_body);
                    } else {
                        write_json_response(
                            &mut stream,
                            "500 Internal Server Error",
                            &json!({"error": {"message": "provider is rate limiting this key"}}),
                        );
                    }
                    return;
                }
                // Barrier A: respond to the spawn round immediately because
                // waiting for a child round that does not exist would deadlock.
                if observations.record_parent() > 0 && !child_arrived.wait(GATE_TIMEOUT) {
                    observations.fault(
                        "屏障 A：第一条子请求迟迟没有到达，父回合只能在没有在飞子回合的情况下继续",
                    );
                }
                let response = parent_queue
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .next();
                match response {
                    Some(response) => write_json_response(&mut stream, "200 OK", &response),
                    // Return a terminating empty response when parent responses are exhausted.
                    None => {
                        write_json_response(&mut stream, "200 OK", &responses_text_output("done"))
                    }
                }
            });
        }
    });
    observations
}

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
            let Ok((mut stream, _)) = listener.accept() else { break };
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

fn tool_successes(contexts: &[ContextItem], tool: &str) -> Vec<bool> {
    contexts
        .iter()
        .filter_map(|context| match context {
            ContextItem::Tool {
                tool_name, result, ..
            } if tool_name == tool => Some(result.success),
            _ => None,
        })
        .collect()
}

/// Followups queued before a child provider failure must be delivered in a
/// later child round. The test observes delivery through the child transcript.
/// `serve_followup_race` uses barriers rather than scheduling delays to ensure
/// the child request is in flight before the followups are queued.
#[test]
fn a_child_provider_error_must_not_strand_queued_followups() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let request = loop_request_for(address, workspace.path());
    let state = AppState::default();
    // Observe the host task pool directly to verify both followups are queued.
    let followups_queued: Arc<dyn Fn() -> bool + Send + Sync> = {
        let state = state.clone();
        let conversation_id = request.conversation_id.clone();
        Arc::new(move || {
            state
                .existing_conversation_tasks(&conversation_id)
                .and_then(|tasks| tasks.pool.find("helper"))
                .is_some_and(|shared| shared.followups.snapshot().len() >= 2)
        })
    };
    let observations = serve_followup_race(
        listener,
        vec![
            // Spawn alone so Barrier A can establish an in-flight child round.
            json!({
                "model": "model-test",
                "status": "completed",
                "output": [
                    {"type":"function_call","status":"completed","call_id":"call-spawn","name":"agent_spawn",
                        "arguments": json!({"prompt":"初始任务","name":"helper"}).to_string()}
                ],
                "usage": {"input_tokens":1,"output_tokens":1,"total_tokens":2}
            }),
            // Queue both followups and wait after Barrier A releases this round.
            json!({
                "model": "model-test",
                "status": "completed",
                "output": [
                    {"type":"function_call","status":"completed","call_id":"call-follow-1","name":"followup_task",
                        "arguments": json!({"target":"helper","message":"FOLLOWUP-ONE"}).to_string()},
                    {"type":"function_call","status":"completed","call_id":"call-follow-2","name":"followup_task",
                        "arguments": json!({"target":"helper","message":"FOLLOWUP-TWO"}).to_string()},
                    {"type":"function_call","status":"completed","call_id":"call-wait-1","name":"task_wait",
                        "arguments": json!({"timeout_seconds": 30}).to_string()}
                ],
                "usage": {"input_tokens":1,"output_tokens":1,"total_tokens":2}
            }),
            // Wait again for the retried round's terminal state.
            json!({
                "model": "model-test",
                "status": "completed",
                "output": [
                    {"type":"function_call","status":"completed","call_id":"call-wait-2","name":"task_wait",
                        "arguments": json!({"timeout_seconds": 30}).to_string()}
                ],
                "usage": {"input_tokens":1,"output_tokens":1,"total_tokens":2}
            }),
            responses_text_output("主代理收尾"),
        ],
        // The resumed child round succeeds. Its marker must be independent from
        // the input-delivery needle so the test distinguishes both conditions.
        "FOLLOWUP-ONE",
        responses_text_output("CHILD-RESUMED-OK"),
        followups_queued,
    );

    let run = run_model(request, &state, &discard_event, &approve_tool);
    let child_bodies = observations.child_bodies();

    // Check barrier failures before unwrapping because `run_model` can fail on
    // the same path and otherwise obscure the recorded diagnosis.
    let gate_faults = observations.gate_faults();
    assert!(
        gate_faults.is_empty(),
        "屏障没有按期成立，这一轮根本没有复现目标交错：{gate_faults:?}"
    );
    let response = run.unwrap();

    let followup_successes = tool_successes(&response.contexts, "followup_task");
    assert_eq!(followup_successes.len(), 2, "两条 followup 都应该有工具卡");
    assert!(
        followup_successes.iter().all(|success| *success),
        "followup 回执本身是成功的——所以宿主必须兑现它"
    );

    let record = agent_record(&response.contexts, "helper")
        .expect("helper 必须留下一条累计记录");
    let delivered = |message: &str| {
        record.contexts.iter().any(|context| match context {
            ContextItem::User { content, .. } => content.contains(message),
            _ => false,
        })
    };
    let stranded: Vec<&str> = ["FOLLOWUP-ONE", "FOLLOWUP-TWO"]
        .into_iter()
        .filter(|message| !delivered(message))
        .collect();

    let first_child = child_bodies
        .first()
        .expect("worker 必须至少发出一次子回合请求");
    // Evaluate each message independently; batching queued followups is valid
    // `followups` behavior and is outside this test's scope.
    let never_resent: Vec<&str> = ["FOLLOWUP-ONE", "FOLLOWUP-TWO"]
        .into_iter()
        .filter(|message| {
            !child_bodies
                .iter()
                .skip(1)
                .any(|body| body.contains(message))
        })
        .collect();

    let wait_output = tool_outputs(&response.contexts, "task_wait").join("\n");
    eprintln!(
        "[integrity] helper: status={:?} 转录 {} 条 排队 {} 条 父请求 {} 次 子回合请求 {} 次\
         （首条自带 followup：{}；未被续轮带上：{never_resent:?}）→ 未送达 {:?}\n\
         [integrity] task_wait 回执：{wait_output}",
        record.status,
        record.contexts.len(),
        record.queued_messages.len(),
        observations.parent_requests(),
        child_bodies.len(),
        first_child.contains("FOLLOWUP-ONE"),
        stranded
    );

    // The first child round must exclude queued messages, or the tested failure
    // path did not execute.
    assert!(
        !first_child.contains("FOLLOWUP-ONE") && !first_child.contains("FOLLOWUP-TWO"),
        "子回合 1 自己就带上了排队消息，于是它一次就成功了——\
         「子回合失败后还兑不兑现 followup」这条路径没有被执行到"
    );
    assert!(
        stranded.is_empty(),
        "子回合因提供商错误结束后，{} 条已被接收（回执 success=true）的 followup \
         永远不会被消费：{stranded:?}。\n\
         它们停在 queued_messages（{} 条）里，而渲染层会把每一条合成成一条\
         没有回复的 user 上下文——这就是用户报障的「整轮回复丢失，只剩连续 user 消息」。",
        stranded.len(),
        record.queued_messages.len()
    );
    assert!(
        record.queued_messages.is_empty(),
        "消费完之后不应还有滞留的排队消息：{:?}",
        record.queued_messages
    );
    // Match later child request contents, not request count, because 5xx retries
    // are distinct from the next child round.
    assert!(
        never_resent.is_empty(),
        "宿主必须真的为排队的 followup 再开一轮子回合：{} 次子请求里，\
         {never_resent:?} 从来没有被带上过",
        child_bodies.len()
    );
    // Both the failure cause and resumed result must reach the parent agent.
    assert_eq!(
        record.status,
        SubagentRunStatus::Completed,
        "兑现 followup 的那一轮成功收场，累计记录就必须停在 Completed"
    );
    assert!(
        wait_output.contains("500") || wait_output.contains("rate limiting"),
        "第一次 task_wait 必须把子回合失败的真实原因交给父代理，实际只有：{wait_output}"
    );
    assert!(
        wait_output.contains("CHILD-RESUMED-OK"),
        "续轮的结果也必须交到父代理手上——只把消息塞进子代理转录、结果却丢在\
         信封层，模型看到的仍然只是一次限流失败。实际的 task_wait 回执：{wait_output}"
    );
}

/// A receipt for `send_message` to an idle child must state that the message
/// will not be read and identify `followup_task` as the wake-up mechanism.
#[test]
fn send_message_to_an_idle_child_must_say_it_will_not_be_read() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = serve_routed_json(
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
            json!({
                "model": "model-test",
                "status": "completed",
                "output": [
                    {"type":"function_call","status":"completed","call_id":"call-send","name":"send_message",
                        "arguments": json!({"target":"helper","message":"QUEUE-ONLY"}).to_string()}
                ],
                "usage": {"input_tokens":1,"output_tokens":1,"total_tokens":2}
            }),
            responses_text_output("主代理收尾"),
        ],
        vec![responses_text_output("子代理首轮答复")],
    );

    let workspace = tempfile::tempdir().unwrap();
    let request = loop_request_for(address, workspace.path());
    let response = run_model(request, &AppState::default(), &discard_event, &approve_tool).unwrap();
    let _ = server.join();

    let sends = tool_outputs(&response.contexts, "send_message");
    assert_eq!(sends.len(), 1, "应该恰好有一次 send_message");
    let output = &sends[0];
    eprintln!("[integrity] send_message 回执：{output}");
    assert!(
        !output.trim().is_empty(),
        "空的工具结果既没告诉模型任何事，在 Chat 协议上还是一条空 role:\"tool\" 内容"
    );
    assert!(
        output.contains("followup_task"),
        "空闲子代理的排队消息不会被读到，回执必须指出唤醒它的办法，实际是：{output}"
    );
}
///
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
    let record = agent_record(&response.contexts, "helper")
        .expect("helper 必须留下一条累计记录");

    eprintln!("[integrity] status={:?}｜task_wait 回执：{wait_output}", record.status);

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
