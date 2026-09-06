//! Host-owned model-run stream hub, corresponding to Cherry Studio's
//! `AiStreamManager` in Tauri.
//!
//! Runs belong to the host; renderers are subscribers. Each conversation has at
//! most one active run with a bounded replay buffer, one subscriber slot, and a
//! settlement slot.
//!
//! A failed delivery only detaches the subscriber. `attach` replays the buffer
//! and installs a replacement subscriber under the same lock, preserving order.
//! Settlements remain available until claimed or replaced by a later run.
//!
//! Cancellation is addressed through `AppState::model_runs`, but its flag stays
//! on `RunModelRequest::run_cancel`. This module routes events only.

use std::{
    collections::{HashMap, VecDeque},
    sync::Mutex,
};

use serde_json::Value;

use crate::model::{ModelStreamEvent, RunModelResponse};

/// Maximum buffered event count. Normal runs remain below this after delta
/// merging; overflow evicts the oldest events.
const MAX_BUFFERED_EVENTS: usize = 10_000;
/// Approximate buffered-byte budget, comparable to `MAX_STREAM_TEXT` (16 MiB)
/// with room for tool receipts.
const MAX_BUFFERED_BYTES: usize = 64 * 1024 * 1024;
/// Maximum size for one merged adjacent text delta. This changes replay
/// granularity only; live events are delivered unmerged.
const DELTA_MERGE_LIMIT: usize = 16 * 1024;

/// A fallible subscriber callback. Failure detaches the subscriber without
/// retrying or categorizing the cause.
pub type RunEventSubscriber = Box<dyn Fn(ModelStreamEvent) -> Result<(), String> + Send>;

/// Terminal state for a completed run, isomorphic to the `run_model` IPC
/// result: `Failed` maps to the invoke error branch.
#[derive(Clone)]
pub enum RunSettlement {
    Completed(Box<RunModelResponse>),
    Failed(String),
}

/// `attach` outcomes. `Running` means buffered events were replayed and live
/// delivery was installed; `Finished` returns and removes the settlement.
pub enum AttachOutcome {
    Running {
        request_id: String,
        /// IPC-shaped serialized request. Host-only serde-skipped fields are
        /// omitted; the renderer uses it to reconstruct `ModelRunState.request`.
        request: Box<Value>,
        dropped_events: u64,
    },
    Finished {
        request_id: String,
        request: Box<Value>,
        settlement: RunSettlement,
    },
    NotFound,
}

/// A `list` row for a conversation with a resumable or claimable run.
pub struct ResumableRun {
    pub conversation_id: String,
    pub request_id: String,
    pub running: bool,
}

struct EventBuffer {
    events: VecDeque<ModelStreamEvent>,
    bytes: usize,
    dropped: u64,
}

impl EventBuffer {
    fn new() -> Self {
        Self {
            events: VecDeque::new(),
            bytes: 0,
            dropped: 0,
        }
    }

    fn push(&mut self, event: ModelStreamEvent) {
        if let Some(merged) = self.try_merge(&event) {
            self.bytes += merged;
            return;
        }
        let cost = event_cost(&event);
        self.events.push_back(event);
        self.bytes += cost;
        while self.events.len() > MAX_BUFFERED_EVENTS || self.bytes > MAX_BUFFERED_BYTES {
            let Some(evicted) = self.events.pop_front() else {
                break;
            };
            self.bytes = self.bytes.saturating_sub(event_cost(&evicted));
            self.dropped += 1;
        }
    }

    /// Merge adjacent text or reasoning deltas from the same round. Reasoning
    /// deltas must also share an `item` to preserve separate streamed rows.
    fn try_merge(&mut self, event: &ModelStreamEvent) -> Option<usize> {
        let last = self.events.back_mut()?;
        match (last, event) {
            (
                ModelStreamEvent::TextDelta { round, delta },
                ModelStreamEvent::TextDelta {
                    round: new_round,
                    delta: new_delta,
                },
            ) if round == new_round && delta.len() + new_delta.len() <= DELTA_MERGE_LIMIT => {
                delta.push_str(new_delta);
                Some(new_delta.len())
            }
            (
                ModelStreamEvent::ReasoningDelta { round, item, delta },
                ModelStreamEvent::ReasoningDelta {
                    round: new_round,
                    item: new_item,
                    delta: new_delta,
                },
            ) if round == new_round
                && item == new_item
                && delta.len() + new_delta.len() <= DELTA_MERGE_LIMIT =>
            {
                delta.push_str(new_delta);
                Some(new_delta.len())
            }
            _ => None,
        }
    }
}

/// Approximate event byte cost. This is an eviction threshold, not quota
/// accounting, so a constant-factor error is acceptable.
fn event_cost(event: &ModelStreamEvent) -> usize {
    const EVENT_OVERHEAD: usize = 160;
    match event {
        ModelStreamEvent::TextDelta { delta, .. }
        | ModelStreamEvent::ReasoningDelta { delta, .. } => EVENT_OVERHEAD + delta.len(),
        ModelStreamEvent::UserInputReceived { content, .. } => EVENT_OVERHEAD + content.len(),
        ModelStreamEvent::ToolCallArgumentsReady { input, .. } => {
            EVENT_OVERHEAD
                + serde_json::to_string(input)
                    .map(|s| s.len())
                    .unwrap_or(1024)
        }
        ModelStreamEvent::ToolExecutionCompleted { result, .. } => {
            EVENT_OVERHEAD
                + serde_json::to_string(result)
                    .map(|s| s.len())
                    .unwrap_or(4096)
        }
        ModelStreamEvent::SubagentDelta { delta, .. } => EVENT_OVERHEAD + delta.len(),
        // Nested subagent deltas need recursive accounting; treating them as a
        // constant would undercount memory as their content grows.
        ModelStreamEvent::SubagentEvent { event, .. } => EVENT_OVERHEAD + event_cost(event),
        _ => EVENT_OVERHEAD,
    }
}

/// Whether an event belongs in the replay buffer. Heartbeats and debug request
/// bodies are useful only to the live subscriber and must not consume the budget.
fn buffer_worthy(event: &ModelStreamEvent) -> bool {
    #[cfg(debug_assertions)]
    if matches!(event, ModelStreamEvent::DebugRequestBody { .. }) {
        return false;
    }
    !matches!(event, ModelStreamEvent::Ping)
}

struct ConversationStream {
    request_id: String,
    request: Box<Value>,
    buffer: EventBuffer,
    subscriber: Option<RunEventSubscriber>,
    settlement: Option<RunSettlement>,
}

#[derive(Default)]
pub struct RunStreamHub {
    inner: Mutex<HashMap<String, ConversationStream>>,
}

impl RunStreamHub {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, ConversationStream>> {
        self.inner.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Register a run and install the caller channel. A new run atomically
    /// replaces any existing entry and unclaimed settlement for this conversation.
    pub fn begin(
        &self,
        conversation_id: &str,
        request_id: &str,
        request: Value,
        subscriber: RunEventSubscriber,
    ) {
        self.lock().insert(
            conversation_id.to_owned(),
            ConversationStream {
                request_id: request_id.to_owned(),
                request: Box::new(request),
                buffer: EventBuffer::new(),
                subscriber: Some(subscriber),
                settlement: None,
            },
        );
    }

    /// Return the request ID of this conversation's unsettled run, if any.
    /// Approval cards delivered through this stream belong to that run; cards
    /// delivered through the push channel belong to their task instead.
    pub fn live_request_id(&self, conversation_id: &str) -> Option<String> {
        let streams = self.lock();
        let stream = streams.get(conversation_id)?;
        stream
            .settlement
            .is_none()
            .then(|| stream.request_id.clone())
    }

    /// Buffer an eligible event, then attempt subscriber delivery. Delivery
    /// failure only detaches the subscriber and never fails the run.
    pub fn publish(&self, conversation_id: &str, event: ModelStreamEvent) {
        let mut streams = self.lock();
        let Some(stream) = streams.get_mut(conversation_id) else {
            return;
        };
        if buffer_worthy(&event) {
            stream.buffer.push(event.clone());
        }
        if let Some(subscriber) = &stream.subscriber {
            if subscriber(event).is_err() {
                stream.subscriber = None;
            }
        }
    }

    /// Publish only while this conversation has an unsettled run. Events cannot
    /// be appended after `RunConcluded`, because replay would place them after the
    /// conclusion marker. Returns whether an event was published.
    pub fn publish_live(&self, conversation_id: &str, event: ModelStreamEvent) -> bool {
        let mut streams = self.lock();
        let Some(stream) = streams.get_mut(conversation_id) else {
            return false;
        };
        if stream.settlement.is_some() {
            return false;
        }
        if buffer_worthy(&event) {
            stream.buffer.push(event.clone());
        }
        if let Some(subscriber) = &stream.subscriber {
            if subscriber(event).is_err() {
                stream.subscriber = None;
            }
        }
        true
    }

    /// Record a settlement and publish [`ModelStreamEvent::RunConcluded`]. The
    /// settlement is visible whenever the marker is visible because both occur
    /// under the same lock.
    pub fn settle(&self, conversation_id: &str, request_id: &str, settlement: RunSettlement) {
        let mut streams = self.lock();
        let Some(stream) = streams.get_mut(conversation_id) else {
            return;
        };
        if stream.request_id != request_id {
            return;
        }
        stream.settlement = Some(settlement);
        let marker = ModelStreamEvent::RunConcluded {
            request_id: request_id.to_owned(),
        };
        stream.buffer.push(marker.clone());
        if let Some(subscriber) = &stream.subscriber {
            if subscriber(marker).is_err() {
                stream.subscriber = None;
            }
        }
    }

    /// Discard a registration that never started. The request-ID guard limits
    /// removal to the registering generation.
    pub fn discard(&self, conversation_id: &str, request_id: &str) {
        let mut streams = self.lock();
        if streams
            .get(conversation_id)
            .is_some_and(|stream| stream.request_id == request_id)
        {
            streams.remove(conversation_id);
        }
    }

    /// Take over a conversation stream. Under one lock, replay buffered events
    /// and install the subscriber for a running stream, or return and remove a
    /// finished settlement. Failed replay leaves the existing subscriber intact.
    pub fn attach(
        &self,
        conversation_id: &str,
        subscriber: RunEventSubscriber,
    ) -> AttachOutcome {
        let mut streams = self.lock();
        let Some(stream) = streams.get_mut(conversation_id) else {
            return AttachOutcome::NotFound;
        };
        if let Some(settlement) = stream.settlement.clone() {
            let request_id = stream.request_id.clone();
            let request = stream.request.clone();
            streams.remove(conversation_id);
            return AttachOutcome::Finished {
                request_id,
                request,
                settlement,
            };
        }
        for event in &stream.buffer.events {
            if subscriber(event.clone()).is_err() {
                return AttachOutcome::NotFound;
            }
        }
        let request_id = stream.request_id.clone();
        let request = stream.request.clone();
        let dropped_events = stream.buffer.dropped;
        stream.subscriber = Some(subscriber);
        AttachOutcome::Running {
            request_id,
            request,
            dropped_events,
        }
    }

    /// Take and remove a settlement only after the run has settled. Running
    /// entries remain untouched.
    pub fn take_settlement(&self, conversation_id: &str) -> Option<(String, RunSettlement)> {
        let mut streams = self.lock();
        let settled = streams
            .get(conversation_id)
            .is_some_and(|stream| stream.settlement.is_some());
        if !settled {
            return None;
        }
        streams.remove(conversation_id).and_then(|stream| {
            stream
                .settlement
                .map(|settlement| (stream.request_id, settlement))
        })
    }

    /// List resumable and claimable runs for the renderer to attach after
    /// startup or document loading.
    pub fn list(&self) -> Vec<ResumableRun> {
        self.lock()
            .iter()
            .map(|(conversation_id, stream)| ResumableRun {
                conversation_id: conversation_id.clone(),
                request_id: stream.request_id.clone(),
                running: stream.settlement.is_none(),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ModelUsage;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    };

    fn channel_subscriber() -> (RunEventSubscriber, mpsc::Receiver<ModelStreamEvent>) {
        let (sender, receiver) = mpsc::channel();
        (
            Box::new(move |event| sender.send(event).map_err(|_| "closed".to_owned())),
            receiver,
        )
    }

    fn dead_subscriber() -> RunEventSubscriber {
        Box::new(|_| Err("closed".to_owned()))
    }

    fn sample_request() -> Value {
        serde_json::json!({ "conversationId": "c1" })
    }

    fn text(round: usize, delta: &str) -> ModelStreamEvent {
        ModelStreamEvent::TextDelta {
            round,
            delta: delta.to_owned(),
        }
    }

    fn sample_response() -> RunModelResponse {
        RunModelResponse {
            contexts: Vec::new(),
            usage: ModelUsage::default(),
            model: "m".into(),
            provider_name: "p".into(),
            duration_ms: 1,
            context_tokens: None,
            stop_reason: None,
            error: None,
            structured_output: None,
        }
    }

    #[test]
    fn publish_reaches_live_subscriber_and_buffer() {
        let hub = RunStreamHub::default();
        let (subscriber, receiver) = channel_subscriber();
        hub.begin("c1", "r1", sample_request(), subscriber);
        hub.publish("c1", text(1, "hello"));
        assert_eq!(receiver.try_recv().unwrap(), text(1, "hello"));

        // Replaying to a replacement subscriber yields the buffered event.
        let (replacement, replay) = channel_subscriber();
        match hub.attach("c1", replacement) {
            AttachOutcome::Running { request_id, .. } => assert_eq!(request_id, "r1"),
            _ => panic!("expected running attach"),
        }
        assert_eq!(replay.try_recv().unwrap(), text(1, "hello"));
    }

    #[test]
    fn dead_subscriber_detaches_without_failing_the_run() {
        let hub = RunStreamHub::default();
        hub.begin("c1", "r1", sample_request(), dead_subscriber());
        // Both publishes succeed and still enter the buffer.
        hub.publish("c1", text(1, "a"));
        hub.publish("c1", text(1, "b"));
        let (subscriber, replay) = channel_subscriber();
        assert!(matches!(
            hub.attach("c1", subscriber),
            AttachOutcome::Running { .. }
        ));
        // Adjacent same-round deltas are merged in the buffer.
        assert_eq!(replay.try_recv().unwrap(), text(1, "ab"));
        assert!(replay.try_recv().is_err());
    }

    #[test]
    fn attach_replays_then_streams_live_in_order() {
        let hub = RunStreamHub::default();
        hub.begin("c1", "r1", sample_request(), dead_subscriber());
        hub.publish("c1", text(1, "early"));
        let (subscriber, receiver) = channel_subscriber();
        assert!(matches!(
            hub.attach("c1", subscriber),
            AttachOutcome::Running { .. }
        ));
        hub.publish("c1", ModelStreamEvent::ReasoningDone { round: 1, item: 0, duration_ms: None });
        assert_eq!(receiver.try_recv().unwrap(), text(1, "early"));
        assert_eq!(
            receiver.try_recv().unwrap(),
            ModelStreamEvent::ReasoningDone { round: 1, item: 0, duration_ms: None }
        );
    }

    #[test]
    fn ping_is_forwarded_live_but_never_replayed() {
        let hub = RunStreamHub::default();
        let (subscriber, receiver) = channel_subscriber();
        hub.begin("c1", "r1", sample_request(), subscriber);
        hub.publish("c1", ModelStreamEvent::Ping);
        assert_eq!(receiver.try_recv().unwrap(), ModelStreamEvent::Ping);
        let (replacement, replay) = channel_subscriber();
        assert!(matches!(
            hub.attach("c1", replacement),
            AttachOutcome::Running { .. }
        ));
        assert!(replay.try_recv().is_err());
    }

    #[test]
    fn settle_emits_marker_and_take_consumes_once() {
        let hub = RunStreamHub::default();
        let (subscriber, receiver) = channel_subscriber();
        hub.begin("c1", "r1", sample_request(), subscriber);
        hub.settle(
            "c1",
            "r1",
            RunSettlement::Completed(Box::new(sample_response())),
        );
        assert_eq!(
            receiver.try_recv().unwrap(),
            ModelStreamEvent::RunConcluded {
                request_id: "r1".into()
            }
        );
        let (request_id, settlement) = hub.take_settlement("c1").expect("settlement");
        assert_eq!(request_id, "r1");
        assert!(matches!(settlement, RunSettlement::Completed(_)));
        assert!(hub.take_settlement("c1").is_none());
    }

    #[test]
    fn attach_after_settle_returns_settlement_and_clears_entry() {
        let hub = RunStreamHub::default();
        hub.begin("c1", "r1", sample_request(), dead_subscriber());
        hub.settle("c1", "r1", RunSettlement::Failed("boom".into()));
        let (subscriber, _receiver) = channel_subscriber();
        match hub.attach("c1", subscriber) {
            AttachOutcome::Finished {
                request_id,
                settlement,
                ..
            } => {
                assert_eq!(request_id, "r1");
                assert!(matches!(settlement, RunSettlement::Failed(_)));
            }
            _ => panic!("expected finished attach"),
        }
        let (subscriber, _receiver) = channel_subscriber();
        assert!(matches!(
            hub.attach("c1", subscriber),
            AttachOutcome::NotFound
        ));
    }

    #[test]
    fn settle_with_stale_request_id_is_ignored() {
        let hub = RunStreamHub::default();
        hub.begin("c1", "r2", sample_request(), dead_subscriber());
        hub.settle("c1", "r1", RunSettlement::Failed("stale".into()));
        assert!(hub.take_settlement("c1").is_none());
        assert_eq!(hub.list().len(), 1);
        assert!(hub.list()[0].running);
    }

    #[test]
    fn begin_replaces_previous_entry_and_discard_checks_generation() {
        let hub = RunStreamHub::default();
        hub.begin("c1", "r1", sample_request(), dead_subscriber());
        hub.settle("c1", "r1", RunSettlement::Failed("old".into()));
        hub.begin("c1", "r2", sample_request(), dead_subscriber());
        // The previous settlement is replaced.
        assert!(hub.take_settlement("c1").is_none());
        hub.discard("c1", "r1");
        assert_eq!(hub.list().len(), 1, "stale discard must not remove r2");
        hub.discard("c1", "r2");
        assert!(hub.list().is_empty());
    }

    #[test]
    fn buffer_eviction_counts_dropped_events() {
        let hub = RunStreamHub::default();
        hub.begin("c1", "r1", sample_request(), dead_subscriber());
        // Different rounds prevent delta merging and force the event-count cap.
        for round in 0..(MAX_BUFFERED_EVENTS + 5) {
            hub.publish("c1", text(round, "x"));
        }
        let (subscriber, replay) = channel_subscriber();
        match hub.attach("c1", subscriber) {
            AttachOutcome::Running { dropped_events, .. } => assert_eq!(dropped_events, 5),
            _ => panic!("expected running attach"),
        }
        let mut count = 0;
        while replay.try_recv().is_ok() {
            count += 1;
        }
        assert_eq!(count, MAX_BUFFERED_EVENTS);
    }

    #[test]
    fn publish_from_worker_thread_while_attaching() {
        let hub = Arc::new(RunStreamHub::default());
        hub.begin("c1", "r1", sample_request(), dead_subscriber());
        let stop = Arc::new(AtomicBool::new(false));
        let publisher = {
            let hub = Arc::clone(&hub);
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                let mut round = 0;
                while !stop.load(Ordering::Acquire) {
                    hub.publish("c1", text(round, "x"));
                    round += 1;
                }
                round
            })
        };
        // Repeated concurrent attaches are atomic with replay and subscriber
        // installation, so each stream is a buffer prefix followed by live,
        // nondecreasing round events without duplicates or reordering.
        for _ in 0..20 {
            let (subscriber, receiver) = channel_subscriber();
            assert!(matches!(
                hub.attach("c1", subscriber),
                AttachOutcome::Running { .. }
            ));
            let mut last = None;
            while let Ok(event) = receiver.try_recv() {
                if let ModelStreamEvent::TextDelta { round, .. } = event {
                    if let Some(previous) = last {
                        assert!(round >= previous, "replay/live order regressed");
                    }
                    last = Some(round);
                }
            }
        }
        stop.store(true, Ordering::Release);
        let _ = publisher.join();
    }
}
