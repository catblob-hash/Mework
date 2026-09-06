//! Backend-initiated push events.
//!
//! Every other renderer/backend exchange in Mework is renderer-initiated
//! (invoke + per-call streaming channels). This hub is the one seam through
//! which the backend may speak first: the renderer subscribes once at startup
//! with a long-lived channel, and backend subsystems publish through the hub
//! whenever something happens that must not wait for the next renderer call —
//! the first consumer is background document-write failure.
//!
//! Delivery rules:
//! - At most one live subscriber (the main renderer document). A new
//!   subscription replaces the previous channel — after a browser-dev
//!   reconnect the old channel writes into a closed socket, so latest wins.
//! - Events published while no subscriber is reachable are buffered in a
//!   bounded backlog (oldest dropped) and flushed to the next subscriber, so
//!   a failure during startup or between reconnects is reported late rather
//!   than never.

use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
};

use serde::Serialize;
use tauri::ipc::Channel;

/// Bounded number of undelivered events kept for the next subscriber. The
/// backlog exists to survive subscription gaps measured in seconds, not to be
/// a durable queue; oldest events are dropped first.
const BACKLOG_LIMIT: usize = 64;

/// One tool card the host could not attest, addressed so the renderer can find
/// it and say what was lost.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuarantinedToolContext {
    pub workspace_id: String,
    pub conversation_id: String,
    pub context_id: String,
    pub tool_name: String,
    /// Exact local-only system marker committed in place of the refused card.
    /// The renderer installs this value rather than deleting the card, so the
    /// user-visible evidence promised by quarantine survives the next save.
    pub replacement: crate::model::ContextItem,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum AppPushEvent {
    /// A queued background document write failed. Published on the first
    /// failure and again only when the failure message changes, never once
    /// per retry.
    DocumentWriteFailure { message: String },
    /// A background write succeeded after a reported failure; the renderer
    /// may clear its failure surface.
    DocumentWriteRecovered,
    /// One or more tool cards could not be shown to have come from this
    /// process's own execution and were replaced with local markers so the
    /// save could go through.
    ///
    /// This used to be a hard save failure that took the whole document with
    /// it, including unrelated new conversations, and repeated on every later
    /// save. Dropping just the affected cards keeps the app usable, but the
    /// user still loses something, so it is reported rather than silent.
    ToolContextsQuarantined {
        /// The affected cards, most recently walked last.
        contexts: Vec<QuarantinedToolContext>,
    },
    /// A `bash` / `powershell` tool call just started. Nothing the renderer
    /// does begins one — the model does — so this is the only way a shell
    /// command can reach the task sidebar while it is still running.
    ShellTaskStarted {
        task: crate::shell_tasks::ShellTaskSnapshot,
    },
    /// A shell command ended, however it ended. Carries the terminal snapshot rather than just an
    /// id: the row does not go away, it becomes a finished one, and the renderer needs the end time
    /// and the outcome to paint it.
    ShellTaskEnded {
        task: crate::shell_tasks::ShellTaskSnapshot,
    },
    /// A retained finished shell command was evicted, so the renderer must remove its stale row.
    #[serde(rename_all = "camelCase")]
    ShellTaskEvicted {
        conversation_id: String,
        shell_task_id: String,
    },
    /// A background task (subagent, shell, or workflow) produced a deliverable
    /// completed, failed, or round-limit result for a conversation without an
    /// active model run. The renderer starts a message-free wake run so the
    /// result is folded into the model's next round. Stopped or interrupted
    /// tasks never trigger a wake.
    #[serde(rename_all = "camelCase")]
    TaskSettled { conversation_id: String },
    /// A tool call needs the user's approval and there is no live run stream to
    /// carry the card.
    ///
    /// Tasks outlive the turn that created them (S11), so a workflow step or a
    /// subagent can reach a `write` after the round that dispatched it ends.
    /// Card delivery uses this channel; the renderer draws it exactly as it
    /// draws a card that arrived on a run stream and answers with
    /// `resolve_tool_prompt`.
    /// The bounded backlog means a card raised between renderer connections is
    /// delivered late rather than never, and `list_pending_tool_prompts`
    /// re-lists whatever is still waiting after a reload.
    #[serde(rename_all = "camelCase")]
    ToolApprovalRequested {
        conversation_id: String,
        /// Flattened on purpose: the renderer draws this with the same
        /// `PendingToolPrompt` shape it gets from `list_pending_tool_prompts`
        /// and from the run stream's `tool_approval_requested`. One shape, one
        /// card renderer — a nested `prompt` here would be a third projection
        /// of the same card and the first place the three drift apart.
        #[serde(flatten)]
        prompt: crate::tool_prompt::PendingToolPrompt,
    },
    /// The card named here is over — answered, stopped, or timed out — and the
    /// renderer should take it down. Published on the same channel as the
    /// request so the two cannot be routed differently.
    #[serde(rename_all = "camelCase")]
    ToolApprovalResolved {
        conversation_id: String,
        prompt_id: String,
        approved: bool,
    },
    /// The model called `fork` in a conversation whose security level does not
    /// delegate the decision. The tool call has already returned; this card is
    /// the whole of what asks the user, so it takes the push channel however
    /// the run is doing, and `list_pending_fork_requests` re-lists it after a
    /// reload. Flattened for the same reason as `ToolApprovalRequested`: one
    /// card shape on every path.
    ForkRequested {
        #[serde(flatten)]
        request: crate::fork_requests::PendingForkRequest,
    },
    /// A fork request is over: answered by the user, created outright under
    /// full access, or retracted because its source conversation went away.
    /// `child_conversation_id` is set exactly when a child was created; the
    /// renderer loads that conversation and starts its first run.
    #[serde(rename_all = "camelCase")]
    ForkResolved {
        fork_id: String,
        workspace_id: String,
        source_conversation_id: String,
        approved: bool,
        child_conversation_id: Option<String>,
    },
}

impl AppPushEvent {
    /// Conversation addressed by this event. Fair backlog eviction buckets events
    /// by this value; global and cross-conversation events share the `None` bucket.
    fn conversation(&self) -> Option<&str> {
        match self {
            AppPushEvent::ShellTaskStarted { task } | AppPushEvent::ShellTaskEnded { task } => {
                Some(&task.conversation_id)
            }
            AppPushEvent::TaskSettled { conversation_id }
            | AppPushEvent::ShellTaskEvicted {
                conversation_id, ..
            }
            | AppPushEvent::ToolApprovalRequested {
                conversation_id, ..
            }
            | AppPushEvent::ToolApprovalResolved {
                conversation_id, ..
            } => Some(conversation_id),
            AppPushEvent::ForkRequested { request } => Some(&request.source_conversation_id),
            AppPushEvent::ForkResolved {
                source_conversation_id,
                ..
            } => Some(source_conversation_id),
            AppPushEvent::DocumentWriteFailure { .. }
            | AppPushEvent::DocumentWriteRecovered
            | AppPushEvent::ToolContextsQuarantined { .. } => None,
        }
    }
}

#[derive(Clone, Default)]
pub struct AppEventHub {
    inner: Arc<Mutex<HubInner>>,
}

#[derive(Default)]
struct HubInner {
    subscriber: Option<Channel<AppPushEvent>>,
    backlog: VecDeque<AppPushEvent>,
}

impl AppEventHub {
    /// Installs `channel` as the sole subscriber and flushes the backlog into
    /// it. If the channel dies mid-flush the remaining events stay queued for
    /// the next subscription.
    pub fn subscribe(&self, channel: Channel<AppPushEvent>) {
        let mut inner = self.lock();
        while let Some(event) = inner.backlog.pop_front() {
            if channel.send(event.clone()).is_err() {
                inner.backlog.push_front(event);
                inner.subscriber = None;
                return;
            }
        }
        inner.subscriber = Some(channel);
    }

    /// Delivers `event` to the live subscriber, or queues it (bounded) when
    /// there is none or the send fails.
    ///
    /// At capacity, evict the oldest event in the largest conversation bucket so
    /// one conversation's burst cannot evict another's sole wake or approval.
    pub fn publish(&self, event: AppPushEvent) {
        let mut inner = self.lock();
        if let Some(channel) = &inner.subscriber {
            if channel.send(event.clone()).is_ok() {
                return;
            }
            inner.subscriber = None;
        }
        if inner.backlog.len() >= BACKLOG_LIMIT {
            let position = {
                let mut counts: HashMap<Option<&str>, usize> = HashMap::new();
                for queued in &inner.backlog {
                    *counts.entry(queued.conversation()).or_default() += 1;
                }
                let largest = counts.values().copied().max().unwrap_or(0);
                // Scan from the oldest entry; ties go to the oldest largest bucket.
                inner
                    .backlog
                    .iter()
                    .position(|queued| counts[&queued.conversation()] == largest)
            };
            match position {
                Some(position) => {
                    inner.backlog.remove(position);
                }
                None => {
                    inner.backlog.pop_front();
                }
            }
        }
        inner.backlog.push_back(event);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HubInner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tauri::ipc::InvokeResponseBody;

    #[test]
    fn quarantine_event_carries_the_committed_marker() {
        let replacement = crate::model::ContextItem::System {
            id: "ctx_tool".into(),
            content: "quarantined".into(),
            local_only: true,
            hook_execution: None,
            created_at: "2026-08-24T00:00:00.000Z".into(),
        };
        let event = AppPushEvent::ToolContextsQuarantined {
            contexts: vec![QuarantinedToolContext {
                workspace_id: "ws".into(),
                conversation_id: "conv".into(),
                context_id: "ctx_tool".into(),
                tool_name: "read".into(),
                replacement,
            }],
        };

        let value = serde_json::to_value(event).unwrap();

        assert_eq!(value["contexts"][0]["replacement"]["kind"], "system");
        assert_eq!(value["contexts"][0]["replacement"]["localOnly"], true);
    }

    fn collecting_channel() -> (Channel<AppPushEvent>, Arc<Mutex<Vec<serde_json::Value>>>) {
        let received = Arc::new(Mutex::new(Vec::new()));
        let sink = received.clone();
        let channel = Channel::new(move |body| {
            let value = match body {
                InvokeResponseBody::Json(json) => serde_json::from_str(&json)?,
                InvokeResponseBody::Raw(bytes) => serde_json::to_value(bytes)?,
            };
            sink.lock().unwrap().push(value);
            Ok(())
        });
        (channel, received)
    }

    fn dead_channel() -> Channel<AppPushEvent> {
        Channel::new(|_| Err(tauri::Error::Io(std::io::Error::other("channel closed"))))
    }

    #[test]
    fn events_published_before_subscription_flush_to_the_first_subscriber() {
        let hub = AppEventHub::default();
        hub.publish(AppPushEvent::DocumentWriteFailure {
            message: "磁盘已满".into(),
        });
        hub.publish(AppPushEvent::DocumentWriteRecovered);

        let (channel, received) = collecting_channel();
        hub.subscribe(channel);

        let received = received.lock().unwrap();
        assert_eq!(received.len(), 2);
        assert_eq!(received[0]["type"], "documentWriteFailure");
        assert_eq!(received[0]["message"], "磁盘已满");
        assert_eq!(received[1]["type"], "documentWriteRecovered");
    }

    #[test]
    fn a_live_subscriber_receives_events_directly_without_backlog() {
        let hub = AppEventHub::default();
        let (channel, received) = collecting_channel();
        hub.subscribe(channel);

        hub.publish(AppPushEvent::DocumentWriteFailure {
            message: "写入被拒绝".into(),
        });

        let received = received.lock().unwrap();
        assert_eq!(received.len(), 1);
        assert_eq!(received[0]["type"], "documentWriteFailure");
    }

    #[test]
    fn a_dead_subscriber_requeues_events_for_the_next_subscription() {
        let hub = AppEventHub::default();
        hub.subscribe(dead_channel());

        hub.publish(AppPushEvent::DocumentWriteFailure {
            message: "连接已断开".into(),
        });

        let (channel, received) = collecting_channel();
        hub.subscribe(channel);
        let received = received.lock().unwrap();
        assert_eq!(received.len(), 1);
        assert_eq!(received[0]["message"], "连接已断开");
    }

    #[test]
    fn the_backlog_drops_oldest_events_at_the_bound() {
        let hub = AppEventHub::default();
        for index in 0..(BACKLOG_LIMIT + 3) {
            hub.publish(AppPushEvent::DocumentWriteFailure {
                message: format!("失败 {index}"),
            });
        }

        let (channel, received) = collecting_channel();
        hub.subscribe(channel);
        let received = received.lock().unwrap();
        assert_eq!(received.len(), BACKLOG_LIMIT);
        assert_eq!(received[0]["message"], "失败 3");
        assert_eq!(
            received[BACKLOG_LIMIT - 1]["message"],
            format!("失败 {}", BACKLOG_LIMIT + 2)
        );
    }

    /// Fair bucket eviction preserves a quiet conversation's lone wake event
    /// through a noisy conversation's burst.
    #[test]
    fn one_conversations_burst_does_not_evict_another_conversations_backlog_event() {
        let hub = AppEventHub::default();
        hub.publish(AppPushEvent::TaskSettled {
            conversation_id: "conversation-quiet".into(),
        });
        for _ in 0..(BACKLOG_LIMIT + 20) {
            hub.publish(AppPushEvent::TaskSettled {
                conversation_id: "conversation-noisy".into(),
            });
        }

        let (channel, received) = collecting_channel();
        hub.subscribe(channel);
        let received = received.lock().unwrap();
        assert_eq!(received.len(), BACKLOG_LIMIT);
        assert!(
            received
                .iter()
                .any(|event| event["conversationId"] == "conversation-quiet"),
            "安静会话的事件必须活过洪峰，而不是被全局 FIFO 挤掉"
        );
    }

    #[test]
    fn a_failed_backlog_flush_keeps_undelivered_events_in_order() {
        let hub = AppEventHub::default();
        hub.publish(AppPushEvent::DocumentWriteFailure {
            message: "第一条".into(),
        });
        hub.publish(AppPushEvent::DocumentWriteFailure {
            message: "第二条".into(),
        });

        hub.subscribe(dead_channel());

        let (channel, received) = collecting_channel();
        hub.subscribe(channel);
        let received = received.lock().unwrap();
        assert_eq!(received.len(), 2);
        assert_eq!(received[0]["message"], "第一条");
        assert_eq!(received[1]["message"], "第二条");
    }

    #[test]
    fn an_approval_card_is_flat_on_the_push_channel() {
        let event = AppPushEvent::ToolApprovalRequested {
            conversation_id: "conv-1".into(),
            prompt: crate::tool_prompt::PendingToolPrompt {
                prompt_id: "prompt-1".into(),
                tool_name: "write".into(),
                label: "写入文件".into(),
                summary: "notes.md".into(),
                risk_level: "中".into(),
                reason: "请求批准模式要求确认所有写入操作".into(),
                requester: Some("ws1".into()),
                source_agent: Some("ws1".into()),
                source_call_id: Some("call-7".into()),
                allow_always_offered: true,
                mandatory: false,
            },
        };
        let wire = serde_json::to_value(&event).unwrap();
        assert_eq!(wire["type"], "toolApprovalRequested");
        assert_eq!(wire["conversationId"], "conv-1");
        assert_eq!(wire["promptId"], "prompt-1");
        assert_eq!(wire["toolName"], "write");
        assert_eq!(wire["summary"], "notes.md");
        assert_eq!(wire["riskLevel"], "中");
        assert_eq!(wire["requester"], "ws1");
        assert_eq!(wire["sourceAgent"], "ws1");
        assert_eq!(wire["sourceCallId"], "call-7");
        assert_eq!(wire["allowAlwaysOffered"], true);
        assert!(wire.get("prompt").is_none(), "卡片不得再套一层：{wire}");

        let resolved = AppPushEvent::ToolApprovalResolved {
            conversation_id: "conv-1".into(),
            prompt_id: "prompt-1".into(),
            approved: false,
        };
        let wire = serde_json::to_value(&resolved).unwrap();
        assert_eq!(wire["type"], "toolApprovalResolved");
        assert_eq!(wire["conversationId"], "conv-1");
        assert_eq!(wire["promptId"], "prompt-1");
        assert_eq!(wire["approved"], false);
    }

    /// The fork card is flat on the wire for the same reason the approval card
    /// is: the renderer draws it with the shape `list_pending_fork_requests`
    /// returns, and a nested `request` would be a second projection.
    #[test]
    fn a_fork_request_is_flat_on_the_push_channel() {
        let event = AppPushEvent::ForkRequested {
            request: crate::fork_requests::PendingForkRequest {
                fork_id: "fork-1".into(),
                workspace_id: "ws-1".into(),
                source_conversation_id: "conv-1".into(),
                source_title: "源对话".into(),
                prompt: "继续做 B".into(),
                inherit_context: true,
                requested_at: "2026-09-05T00:00:00Z".into(),
            },
        };
        let wire = serde_json::to_value(&event).unwrap();
        assert_eq!(wire["type"], "forkRequested");
        assert_eq!(wire["forkId"], "fork-1");
        assert_eq!(wire["workspaceId"], "ws-1");
        assert_eq!(wire["sourceConversationId"], "conv-1");
        assert_eq!(wire["sourceTitle"], "源对话");
        assert_eq!(wire["prompt"], "继续做 B");
        assert_eq!(wire["inheritContext"], true);
        assert!(wire.get("request").is_none(), "卡片不得再套一层：{wire}");

        let resolved = AppPushEvent::ForkResolved {
            fork_id: "fork-1".into(),
            workspace_id: "ws-1".into(),
            source_conversation_id: "conv-1".into(),
            approved: true,
            child_conversation_id: Some("conv-2".into()),
        };
        let wire = serde_json::to_value(&resolved).unwrap();
        assert_eq!(wire["type"], "forkResolved");
        assert_eq!(wire["forkId"], "fork-1");
        assert_eq!(wire["approved"], true);
        assert_eq!(wire["childConversationId"], "conv-2");
        assert_eq!(resolved.conversation(), Some("conv-1"));
    }
}
