//! The shadow kernel feeds protocol events observed on the main turn thread to
//! `agent_kernel::Kernel`, the implementation projection of the `formal/`
//! specifications. Observation never changes host control flow. Divergences are
//! logged and become panics when `MEWORK_KERNEL_SHADOW=strict`. Every divergence
//! is triaged as an implementation defect first; the formal model changes only
//! when a deliberate safety decision demands it.
//!
//! Main-thread observation satisfies environment assumption A2. Approval callbacks
//! can arrive on worker threads, so state is protected by a `Mutex`; a poisoned lock
//! disables observation rather than failing the host.
//!
//! ## Projection limits and invariants
//!
//! * Slots are reusable containers. Calls map by call ID and tasks by agent name;
//!   settled entries release their slots. Entities outside the finite shadow scope
//!   increment `scope_gaps` and are ignored.
//! * The danger bit means approval is known to be required before dispatch. Wrapped
//!   executor callbacks observe its outcome, then emit `Approve -> ExecStart ->
//!   ExecEnd` in causal order. A successful dangerous call with no observed approval
//!   emits `ExecStart` and is rejected by S1.
//! * Synchronous calls carry no identity. The host keeps one `ToolCall` from request
//!   through settlement, and the shadow audits A3 by comparing parameter digests at
//!   `tool_request` and `tool_finished`.
//! * Task identities are host-generated `TaskIdentity` values. The shadow binds each
//!   tracked identity to a finite rid: the tracked identity uses its rid, a newer
//!   same-name incarnation is a scope gap, and stale or mismatched identities use a
//!   non-current rid so the kernel rejects them. This requires monotonic generations
//!   and host guards that discard stale worker writes.
//! * `run_task_wait` starts `TaskWait` at the blocking-window boundary, completes it
//!   with settlement and `TaskWaitDeliver`, and withdraws it with `TaskWaitTimeout`
//!   when no delivery occurs. `TurnCancel` voids all pending waits.
//! * Aborted turns emit `TurnCancel`, settle stranded calls by phase, and close the
//!   active round before teardown. `TurnEnd` retains running tasks in idle so they
//!   survive across turns.
//! * Undelivered terminal envelopes are observed before TurnStart without claiming
//!   them, satisfying S11's requirement to fold them before the first RoundStart.
//!   Completions observed later are emitted when fold or wait consumes their envelope.
//!   Every terminal envelope is projected as `Complete`, whatever ended the task.
//! * Round boundaries close active rounds. Host injections are projected as queued
//!   steer events immediately before the next `RoundStart`.
//! * Folded tasks emit `AgentFold` (preceded by `AgentComplete` if not yet observed)
//!   and create delivery work for a later round — a task the user closed included.
//!   Folding during abort is a divergence.
//! * Worker-owned followup turns are outside the main-thread observation scope. When
//!   a main-thread followup leaves an old envelope unclaimed, its existing mapping is
//!   retained so delivery remains attributed to the old incarnation.
//! * Workflow drivers and background shell commands use the outer task pool and the
//!   same spawn, wait, fold, and settlement projection as agents. Their internal
//!   workers remain outside this scope. `web_search` and `web_fetch` use the call
//!   pipeline as asynchronous tools.
//! * `PolicyTighten` and `ContextEdit` have no host emission point and are covered by
//!   crate consistency tests and CSP scenarios.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use agent_kernel::{CallPhase, Kernel, KernelConfig, KernelEvent, RoundPhase, TurnPhase};

use crate::agents::TaskIdentity;

/// Stable digest of the serialized final parameter object for the A3 audit.
/// Both values are submitted by the host observation surface.
pub fn call_params_digest(input: &crate::model::JsonObject) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    serde_json::to_string(input).unwrap_or_default().hash(&mut hasher);
    hasher.finish()
}

/// Shadow scope accommodates serialized calls and tasks awaiting consumption.
const SHADOW_CALL_SLOTS: usize = 4;
const SHADOW_AGENT_SLOTS: usize = 32;
/// Finite rid space for host task identities. The current and non-current rid
/// must differ while matching the model scope width.
const SHADOW_TASK_IDS: usize = 2;

/// Return the alternate rid for both new tracked incarnations and stale identities.
fn other_rid(rid: usize) -> usize {
    rid % SHADOW_TASK_IDS + 1
}

struct CallEntry {
    slot: usize,
    danger: bool,
    prompt_outcome: Option<bool>,
    /// Parameter digest captured after final classification and checked at completion.
    params_digest: u64,
}

struct AgentEntry {
    slot: usize,
    /// Rid assigned by the shadow to the tracked incarnation and submitted with spawn.
    rid: usize,
    /// Host-visible identity of the tracked incarnation.
    identity: TaskIdentity,
}

/// Classification of an event identity relative to the tracked incarnation.
enum IdentityMatch {
    /// The tracked incarnation uses its assigned rid.
    Tracked,
    /// A newer same-name incarnation is outside the kernel scope.
    NewerIncarnation,
    /// A stale or parameter-mismatched identity maps to a non-current rid.
    Stale,
}

impl AgentEntry {
    fn classify(&self, identity: TaskIdentity) -> IdentityMatch {
        if identity == self.identity {
            IdentityMatch::Tracked
        } else if identity.generation > self.identity.generation {
            IdentityMatch::NewerIncarnation
        } else {
            IdentityMatch::Stale
        }
    }
}

struct ShadowInner {
    kernel: Kernel,
    calls: HashMap<String, CallEntry>,
    agents: HashMap<String, AgentEntry>,
    free_calls: Vec<usize>,
    free_agents: Vec<usize>,
    /// Call IDs and task names ignored because they exceed shadow scope.
    unmapped: HashSet<String>,
    /// Dangerous call currently inside the executor window.
    active_prompt_call: Option<String>,
    /// Continuation content injected between rounds, emitted as steer events before
    /// the next round starts.
    pending_injection: bool,
    divergences: Vec<String>,
    scope_gaps: usize,
}

pub struct KernelShadow {
    strict: bool,
    inner: Mutex<ShadowInner>,
}

impl KernelShadow {
    pub fn new(max_live_agents: usize) -> Self {
        KernelShadow {
            strict: std::env::var("MEWORK_KERNEL_SHADOW").as_deref() == Ok("strict"),
            inner: Mutex::new(ShadowInner {
                kernel: Kernel::new(KernelConfig {
                    call_slots: SHADOW_CALL_SLOTS,
                    agent_slots: SHADOW_AGENT_SLOTS,
                    max_live_agents,
                    task_ids: SHADOW_TASK_IDS,
                }),
                calls: HashMap::new(),
                agents: HashMap::new(),
                free_calls: (1..=SHADOW_CALL_SLOTS).rev().collect(),
                free_agents: (1..=SHADOW_AGENT_SLOTS).rev().collect(),
                unmapped: HashSet::new(),
                active_prompt_call: None,
                pending_injection: false,
                divergences: Vec::new(),
                scope_gaps: 0,
            }),
        }
    }

    #[cfg(test)]
    fn strict_for_tests() -> Self {
        let mut shadow = Self::new(KernelConfig::MODEL_SCOPE.max_live_agents);
        shadow.strict = true;
        shadow
    }

    fn with_inner(&self, f: impl FnOnce(&mut ShadowInner, bool)) {
        if let Ok(mut inner) = self.inner.lock() {
            f(&mut inner, self.strict);
        }
    }

    /// Every divergence recorded so far.
    ///
    /// Test-only: production reports divergences through the shadow's own logging
    /// and strict mode, never by reading this list, so leaving it in the non-test
    /// build only adds a `dead_code` warning. Delete the attribute the day a
    /// production caller appears.
    #[cfg(test)]
    pub fn divergences(&self) -> Vec<String> {
        self.inner
            .lock()
            .map(|inner| inner.divergences.clone())
            .unwrap_or_default()
    }

    #[cfg(test)]
    pub fn begin_turn(&self) {
        self.begin_turn_with_completed(std::iter::empty());
    }

    /// Observe the wake snapshot before TurnStart; inherited done results then
    /// participate in S11's first-round guard without claiming envelopes.
    pub fn begin_turn_with_completed(
        &self,
        completed: impl IntoIterator<Item = (String, TaskIdentity)>,
    ) {
        self.with_inner(|inner, strict| {
            for (name, identity) in completed {
                let Some(entry) = inner.agents.get(&name) else {
                    inner.scope_gaps += 1;
                    continue;
                };
                let slot = entry.slot;
                let r = match entry.classify(identity) {
                    IdentityMatch::Tracked => {
                        // A cancelled wake can leave the same result done in
                        // the shadow and still unclaimed in the pool.
                        if inner.kernel.state().agents[slot - 1].phase == agent_kernel::AgentPhase::Done {
                            continue;
                        }
                        entry.rid
                    }
                    IdentityMatch::NewerIncarnation => {
                        inner.scope_gaps += 1;
                        continue;
                    }
                    IdentityMatch::Stale => other_rid(entry.rid),
                };
                inner.step(strict, KernelEvent::AgentComplete { a: slot, r });
            }
            inner.step(strict, KernelEvent::TurnStart);
        });
    }

    /// Close an active round. Calls before the first round and repeated calls are no-ops.
    pub fn round_boundary(&self) {
        self.with_inner(|inner, strict| {
            if inner.kernel.state().round == RoundPhase::InRound {
                inner.step(strict, KernelEvent::RoundEnd);
            }
        });
    }

    /// A batch drained at a round boundary becomes one serialized steer enqueue and join.
    pub fn steer_joined(&self) {
        self.with_inner(|inner, strict| {
            inner.step(strict, KernelEvent::SteerEnqueue);
            inner.step(strict, KernelEvent::SteerJoin);
        });
    }

    /// Queue a steer-equivalent host injection for the next `round_started` call.
    pub fn host_injection(&self) {
        self.with_inner(|inner, _strict| inner.pending_injection = true);
    }

    /// Abort a turn before teardown folds: cancel pending waits, settle stranded
    /// calls by phase, and close the active round. Executing calls need `ToolExecEnd`
    /// before `ToolSettle`, because a failed `tool_finished` may leave them executing.
    pub fn turn_aborting(&self) {
        self.with_inner(|inner, strict| {
            if inner.kernel.state().turn == TurnPhase::Running {
                inner.step(strict, KernelEvent::TurnCancel);
            }
            inner.sweep_calls(strict);
            if inner.kernel.state().round == RoundPhase::InRound {
                inner.step(strict, KernelEvent::RoundEnd);
            }
        });
    }

    /// The kernel rejects a round start without pending delivery work.
    pub fn round_started(&self) {
        self.with_inner(|inner, strict| {
            if inner.pending_injection {
                inner.pending_injection = false;
                inner.step(strict, KernelEvent::SteerEnqueue);
                inner.step(strict, KernelEvent::SteerJoin);
            }
            inner.step(strict, KernelEvent::RoundStart);
        });
    }

    /// Project calls rejected before classification or approval as request, deny, settle.
    pub fn rejected_precheck(&self, call_id: &str) {
        self.with_inner(|inner, strict| {
            let Some(slot) = inner.allocate_call(call_id, false, 0) else { return };
            inner.step(strict, KernelEvent::ToolRequest { c: slot, d: false });
            inner.step(strict, KernelEvent::ToolDeny { c: slot });
            inner.step(strict, KernelEvent::ToolSettle { c: slot });
            inner.release_call(call_id);
        });
    }

    /// `danger` records whether approval is required before dispatch; `params_digest`
    /// captures the final parameters for the A3 audit.
    pub fn tool_request(&self, call_id: &str, danger: bool, params_digest: u64) {
        self.with_inner(|inner, strict| {
            let Some(slot) = inner.allocate_call(call_id, danger, params_digest) else { return };
            inner.step(strict, KernelEvent::ToolRequest { c: slot, d: danger });
        });
    }

    /// Safe calls are allowed and started here. Dangerous-call approval is observed
    /// inside the executor and emitted by `tool_finished`.
    pub fn tool_dispatched(&self, call_id: &str) {
        self.with_inner(|inner, strict| {
            let Some(entry) = inner.calls.get_mut(call_id) else { return };
            let slot = entry.slot;
            if entry.danger {
                entry.prompt_outcome = None;
                inner.active_prompt_call = Some(call_id.to_owned());
            } else {
                inner.step(strict, KernelEvent::ToolAllow { c: slot });
                inner.step(strict, KernelEvent::ToolExecStart { c: slot });
            }
        });
    }

    /// Record the first approval outcome observed in the executor window.
    pub fn record_prompt_outcome(&self, granted: bool) {
        self.with_inner(|inner, _strict| {
            let Some(call_id) = inner.active_prompt_call.clone() else { return };
            if let Some(entry) = inner.calls.get_mut(&call_id) {
                entry.prompt_outcome.get_or_insert(granted);
            }
        });
    }

    /// Compare the completed call's parameter digest to the classified digest for
    /// the A3 host-ownership audit.
    pub fn tool_finished(
        &self,
        call_id: &str,
        rejected: bool,
        hook_allows: bool,
        success: bool,
        params_digest: u64,
    ) {
        self.with_inner(|inner, strict| {
            if inner.unmapped.contains(call_id) {
                return;
            }
            let Some(entry) = inner.calls.get(call_id) else {
                inner.divergences.push(format!("tool_finished 收到未登记的调用：{call_id}"));
                return;
            };
            if entry.params_digest != params_digest {
                let note = format!(
                    "A3 审计：调用 {call_id} 批准时与执行后的参数摘要不一致（批准 A 执行 A′）"
                );
                eprintln!("[kernel-shadow] 与规范分歧：{note}");
                inner.divergences.push(note);
                if strict {
                    panic!("MEWORK_KERNEL_SHADOW=strict：实现与内核规范分歧（spec-first：实现缺陷）");
                }
            }
            let (slot, danger, prompt_outcome) = (entry.slot, entry.danger, entry.prompt_outcome);
            inner.active_prompt_call = None;
            if rejected {
                inner.step(strict, KernelEvent::ToolDeny { c: slot });
            } else if !danger {
                inner.step(strict, KernelEvent::ToolExecEnd { c: slot });
            } else {
                let granted = prompt_outcome == Some(true) || hook_allows;
                if granted {
                    inner.step(strict, KernelEvent::ToolApprove { c: slot });
                    inner.step(strict, KernelEvent::ToolExecStart { c: slot });
                    inner.step(strict, KernelEvent::ToolExecEnd { c: slot });
                } else if prompt_outcome == Some(false) || !success {
                    // Project a user denial or pre-gate failure as a denial.
                    inner.step(strict, KernelEvent::ToolDeny { c: slot });
                } else {
                    // Report successful execution without an observed approval so S1 rejects it.
                    inner.step(strict, KernelEvent::ToolExecStart { c: slot });
                    inner.step(strict, KernelEvent::ToolDeny { c: slot });
                }
            }
            inner.step(strict, KernelEvent::ToolSettle { c: slot });
            inner.release_call(call_id);
        });
    }

    /// A synchronous call that reached its deadline hands its still-running work
    /// to a task slot. This is the one event that crosses the call and task
    /// surfaces, so both sides have to be resolvable: an unmapped call or an
    /// agent slot the projection cannot allocate is a scope gap, not a step.
    ///
    /// The call stays executing afterwards — it still owes its receipt — so no
    /// `ToolExecEnd` is emitted here. `tool_finished` continues to close it.
    pub fn tool_backgrounded(&self, call_id: &str, name: &str, identity: TaskIdentity) {
        self.with_inner(|inner, strict| {
            if inner.unmapped.contains(call_id) {
                return;
            }
            let Some(entry) = inner.calls.get(call_id) else {
                inner
                    .divergences
                    .push(format!("tool_backgrounded 收到未登记的调用：{call_id}"));
                return;
            };
            let c = entry.slot;
            let Some((a, r)) = inner.allocate_agent(name, identity) else { return };
            inner.step(strict, KernelEvent::ToolBackground { c, a, r });
        });
    }

    /// Track a newly registered host incarnation with an alternate current rid.
    pub fn agent_spawned(&self, name: &str, identity: TaskIdentity) {
        self.with_inner(|inner, strict| {
            let Some((slot, r)) = inner.allocate_agent(name, identity) else { return };
            inner.step(strict, KernelEvent::AgentSpawn { a: slot, r });
        });
    }

    /// When a followup leaves an old envelope unclaimed, retain its mapping so its
    /// later delivery remains attributed to the old incarnation. The new incarnation
    /// is outside scope until the prior entry has been released.
    pub fn agent_refollowed(&self, name: &str, identity: TaskIdentity) {
        self.with_inner(|inner, strict| {
            if inner.unmapped.contains(name) {
                return;
            }
            if inner.agents.contains_key(name) {
                inner.scope_gaps += 1;
                return;
            }
            let Some((slot, r)) = inner.allocate_agent(name, identity) else { return };
            inner.step(strict, KernelEvent::AgentSpawn { a: slot, r });
        });
    }

    /// Begin `TaskWait` at the blocking-window boundary. A newer same-name identity
    /// still projects to the tracked incarnation because the wait covers its unclaimed envelope.
    pub fn task_wait_started(&self, name: &str, identity: TaskIdentity) {
        self.with_inner(|inner, strict| {
            let Some(entry) = inner.agents.get(name) else {
                // Rehydrated and worker-owned-followup incarnations are outside scope.
                inner.scope_gaps += 1;
                return;
            };
            let (slot, r) = match entry.classify(identity) {
                IdentityMatch::Tracked | IdentityMatch::NewerIncarnation => {
                    (entry.slot, entry.rid)
                }
                // A stale requested identity uses a non-current rid so the kernel rejects it.
                IdentityMatch::Stale => (entry.slot, other_rid(entry.rid)),
            };
            inner.step(strict, KernelEvent::TaskWait { a: slot, r });
        });
    }

    /// Complete a wait with the result envelope's own identity. Tracked identities
    /// settle and release their entry; newer ones are scope gaps; stale identities
    /// use a non-current rid and retain the entry for real delivery.
    pub fn task_wait_claimed(&self, name: &str, identity: TaskIdentity) {
        self.with_inner(|inner, strict| {
            let Some(entry) = inner.agents.get(name) else {
                inner.scope_gaps += 1;
                return;
            };
            let slot = entry.slot;
            match entry.classify(identity) {
                IdentityMatch::Tracked => {
                    let r = entry.rid;
                    inner.step(strict, KernelEvent::AgentComplete { a: slot, r });
                    inner.step(strict, KernelEvent::TaskWaitDeliver { a: slot, r });
                    inner.release_agent(name);
                }
                IdentityMatch::NewerIncarnation => {
                    inner.scope_gaps += 1;
                }
                IdentityMatch::Stale => {
                    let r = other_rid(entry.rid);
                    inner.step(strict, KernelEvent::AgentComplete { a: slot, r });
                    inner.step(strict, KernelEvent::TaskWaitDeliver { a: slot, r });
                }
            }
        });
    }

    /// Withdraw an undelivered wait when its deadline expires, another observed task
    /// delivers first, or this task stops without posting an envelope. Progress updates
    /// do not withdraw waits. The mapping remains running, so the kernel guard holds.
    pub fn task_wait_timed_out(&self, name: &str, identity: TaskIdentity) {
        self.with_inner(|inner, strict| {
            let Some(entry) = inner.agents.get(name) else {
                inner.scope_gaps += 1;
                return;
            };
            let (slot, r) = match entry.classify(identity) {
                IdentityMatch::Tracked | IdentityMatch::NewerIncarnation => {
                    (entry.slot, entry.rid)
                }
                IdentityMatch::Stale => (entry.slot, other_rid(entry.rid)),
            };
            inner.step(strict, KernelEvent::TaskWaitTimeout { a: slot, r });
        });
    }

    /// At a round boundary, a claimed terminal envelope emits AgentFold, preceded
    /// by AgentComplete only if the wake snapshot has not already observed it.
    /// Every result creates delivery work, including one from a user-closed task.
    /// Folding during abort is intentionally rejected.
    pub fn agent_folded(&self, name: &str, identity: TaskIdentity) {
        self.with_inner(|inner, strict| {
            let Some(entry) = inner.agents.get(name) else {
                inner.scope_gaps += 1;
                return;
            };
            let slot = entry.slot;
            let (r, tracked) = match entry.classify(identity) {
                IdentityMatch::Tracked => (entry.rid, true),
                IdentityMatch::NewerIncarnation => {
                    inner.scope_gaps += 1;
                    return;
                }
                IdentityMatch::Stale => (other_rid(entry.rid), false),
            };
            if !tracked || inner.kernel.state().agents[slot - 1].phase != agent_kernel::AgentPhase::Done {
                inner.step(strict, KernelEvent::AgentComplete { a: slot, r });
            }
            inner.step(strict, KernelEvent::AgentFold { a: slot, r });
            if tracked {
                inner.release_agent(name);
            }
        });
    }

    /// Finalize calls before closing the active round and emitting `TurnEnd`. Aborted
    /// paths cancel unresolved delivery work. Running tasks and their mappings persist
    /// into idle for the next turn.
    pub fn turn_finished(&self, aborted: bool) {
        self.with_inner(|inner, strict| {
            if aborted && inner.kernel.state().turn == TurnPhase::Running {
                inner.step(strict, KernelEvent::TurnCancel);
            }
            inner.sweep_calls(strict);
            if inner.kernel.state().round == RoundPhase::InRound {
                inner.step(strict, KernelEvent::RoundEnd);
            }
            // Never carry an unconsumed injection into the next turn.
            inner.pending_injection = false;
            inner.step(strict, KernelEvent::TurnEnd);
        });
    }
}

impl ShadowInner {
    /// Settle residual calls by phase. Executing calls require `ToolExecEnd` before
    /// `ToolSettle`, because error propagation can bypass `tool_finished`.
    fn sweep_calls(&mut self, strict: bool) {
        let ids: Vec<String> = self.calls.keys().cloned().collect();
        for call_id in ids {
            self.divergences
                .push(format!("回合收束时仍有未完成的调用槽位：{call_id}"));
            let Some(entry) = self.calls.get(&call_id) else { continue };
            let slot = entry.slot;
            if self.kernel.state().calls[slot - 1].phase == CallPhase::Executing {
                self.step(strict, KernelEvent::ToolExecEnd { c: slot });
            }
            self.step(strict, KernelEvent::ToolSettle { c: slot });
            self.release_call(&call_id);
        }
    }

    fn task_rid(&self, slot: usize) -> usize {
        self.kernel.state().agents[slot - 1].rid
    }

    fn step(&mut self, strict: bool, event: KernelEvent) {
        match self.kernel.step(event) {
            Ok(()) => {}
            Err(refusal) => {
                let note = format!("{refusal}（event={event:?}）");
                eprintln!("[kernel-shadow] 与规范分歧：{note}");
                self.divergences.push(note);
                if strict {
                    panic!("MEWORK_KERNEL_SHADOW=strict：实现与内核规范分歧（spec-first：实现缺陷）");
                }
            }
        }
    }

    fn allocate_call(&mut self, call_id: &str, danger: bool, params_digest: u64) -> Option<usize> {
        if self.calls.contains_key(call_id) {
            self.divergences.push(format!("调用 {call_id} 被重复登记"));
            return None;
        }
        let Some(slot) = self.free_calls.pop() else {
            self.scope_gaps += 1;
            self.unmapped.insert(call_id.to_owned());
            return None;
        };
        self.calls.insert(
            call_id.to_owned(),
            CallEntry { slot, danger, prompt_outcome: None, params_digest },
        );
        Some(slot)
    }

    fn release_call(&mut self, call_id: &str) {
        if let Some(entry) = self.calls.remove(call_id) {
            self.free_calls.push(entry.slot);
        }
    }

    /// Allocate a tracked incarnation and a rid distinct from the slot's current rid.
    fn allocate_agent(&mut self, name: &str, identity: TaskIdentity) -> Option<(usize, usize)> {
        if self.agents.contains_key(name) {
            self.divergences.push(format!("任务 {name} 被重复登记"));
            return None;
        }
        let Some(slot) = self.free_agents.pop() else {
            self.scope_gaps += 1;
            self.unmapped.insert(name.to_owned());
            return None;
        };
        let rid = other_rid(self.task_rid(slot));
        self.agents
            .insert(name.to_owned(), AgentEntry { slot, rid, identity });
        Some((slot, rid))
    }

    fn release_agent(&mut self, name: &str) {
        if let Some(entry) = self.agents.remove(name) {
            self.free_agents.push(entry.slot);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test identity with a fixed digest; `tid_params` supplies a mismatched digest.
    fn tid(generation: u64) -> TaskIdentity {
        TaskIdentity { generation, params_digest: 99 }
    }

    fn tid_params(generation: u64, params_digest: u64) -> TaskIdentity {
        TaskIdentity { generation, params_digest }
    }

    /// Consume outstanding delivery work in a clean final round before ending the turn.
    fn finish_clean(shadow: &KernelShadow) {
        shadow.round_boundary();
        shadow.round_started();
        shadow.round_boundary();
        shadow.turn_finished(false);
    }

    #[test]
    fn safe_call_and_task_wait_flow_matches_spec() {
        let shadow = KernelShadow::strict_for_tests();
        shadow.begin_turn();
        shadow.round_started();
        shadow.tool_request("call-1", false, 7);
        shadow.tool_dispatched("call-1");
        shadow.tool_finished("call-1", false, false, true, 7);
        shadow.agent_spawned("worker", tid(1));
        shadow.task_wait_started("worker", tid(1));
        shadow.task_wait_claimed("worker", tid(1));
        finish_clean(&shadow);
        assert!(shadow.divergences().is_empty(), "{:?}", shadow.divergences());
    }

    #[test]
    fn task_wait_timeout_withdraws_and_task_survives() {
        let shadow = KernelShadow::strict_for_tests();
        shadow.begin_turn();
        shadow.round_started();
        shadow.agent_spawned("worker", tid(1));
        shadow.task_wait_started("worker", tid(1));
        // A timeout leaves the task running and creates delivery work for the next round.
        shadow.task_wait_timed_out("worker", tid(1));
        shadow.round_boundary();
        shadow.round_started();
        shadow.round_boundary();
        // Running tasks enter idle without being settled.
        shadow.turn_finished(false);
        assert!(shadow.divergences().is_empty(), "{:?}", shadow.divergences());
    }

    #[test]
    fn dangerous_call_approved_inside_executor() {
        let shadow = KernelShadow::strict_for_tests();
        shadow.begin_turn();
        shadow.round_started();
        shadow.tool_request("call-1", true, 7);
        shadow.tool_dispatched("call-1");
        shadow.record_prompt_outcome(true);
        shadow.tool_finished("call-1", false, false, true, 7);
        finish_clean(&shadow);
        assert!(shadow.divergences().is_empty(), "{:?}", shadow.divergences());
    }

    #[test]
    fn dangerous_call_denied_by_user() {
        let shadow = KernelShadow::strict_for_tests();
        shadow.begin_turn();
        shadow.round_started();
        shadow.tool_request("call-1", true, 7);
        shadow.tool_dispatched("call-1");
        shadow.record_prompt_outcome(false);
        shadow.tool_finished("call-1", false, false, false, 7);
        finish_clean(&shadow);
        assert!(shadow.divergences().is_empty(), "{:?}", shadow.divergences());
    }

    #[test]
    fn dangerous_success_without_any_grant_is_a_divergence() {
        let shadow = KernelShadow::new(1);
        shadow.begin_turn();
        shadow.round_started();
        shadow.tool_request("call-1", true, 7);
        shadow.tool_dispatched("call-1");
        shadow.tool_finished("call-1", false, false, true, 7);
        let divergences = shadow.divergences();
        assert!(
            divergences.iter().any(|d| d.contains("callFresh")),
            "期望 S1 分歧被记录：{divergences:?}"
        );
    }

    #[test]
    fn fold_retriggers_and_cancel_sweep_flow() {
        let shadow = KernelShadow::strict_for_tests();
        shadow.begin_turn();
        shadow.round_started();
        shadow.agent_spawned("a", tid(1));
        shadow.round_boundary();
        // A completed envelope folds during preparation and creates next-round work.
        shadow.agent_folded("a", tid(1));
        shadow.round_started();
        shadow.agent_spawned("b", tid(1));
        shadow.turn_finished(true);
        assert!(shadow.divergences().is_empty(), "{:?}", shadow.divergences());
    }

    /// A user-stopped result is an ordinary terminal result: it folds, which charges
    /// the delivery obligation, so the turn cannot close without another round.
    #[test]
    fn stopped_fold_charges_a_delivery_round() {
        let shadow = KernelShadow::strict_for_tests();
        shadow.begin_turn();
        shadow.round_started();
        shadow.agent_spawned("a", tid(1));
        shadow.round_boundary();
        shadow.round_started();
        shadow.round_boundary();
        shadow.agent_folded("a", tid(1));
        shadow.round_started();
        shadow.round_boundary();
        shadow.turn_finished(false);
        assert!(shadow.divergences().is_empty(), "{:?}", shadow.divergences());
    }

    #[test]
    fn steer_joins_at_boundary_and_forces_round() {
        let shadow = KernelShadow::strict_for_tests();
        shadow.begin_turn();
        shadow.round_started();
        shadow.round_boundary();
        shadow.steer_joined();
        shadow.round_started();
        shadow.round_boundary();
        shadow.turn_finished(false);
        assert!(shadow.divergences().is_empty(), "{:?}", shadow.divergences());
    }

    #[test]
    fn host_injection_defers_to_next_round_start() {
        let shadow = KernelShadow::strict_for_tests();
        shadow.begin_turn();
        shadow.round_started();
        // Record the in-round decision without emitting events.
        shadow.host_injection();
        shadow.round_boundary();
        shadow.round_started();
        shadow.round_boundary();
        shadow.turn_finished(false);
        assert!(shadow.divergences().is_empty(), "{:?}", shadow.divergences());
    }

    #[test]
    fn followup_keeps_old_incarnation_for_its_pending_result() {
        // Preserve an unclaimed old envelope for real delivery; the new incarnation is a scope gap.
        let shadow = KernelShadow::strict_for_tests();
        shadow.begin_turn();
        shadow.round_started();
        shadow.agent_spawned("a", tid(1));
        shadow.agent_refollowed("a", tid(2));
        shadow.task_wait_started("a", tid(1));
        shadow.task_wait_claimed("a", tid(1));
        finish_clean(&shadow);
        assert!(shadow.divergences().is_empty(), "{:?}", shadow.divergences());
    }

    #[test]
    fn followup_after_claim_registers_fresh_incarnation() {
        // Once the old entry is released, a followup registers the new incarnation.
        let shadow = KernelShadow::strict_for_tests();
        shadow.begin_turn();
        shadow.round_started();
        shadow.agent_spawned("a", tid(1));
        shadow.task_wait_started("a", tid(1));
        shadow.task_wait_claimed("a", tid(1));
        shadow.agent_refollowed("a", tid(2));
        finish_clean(&shadow);
        assert!(shadow.divergences().is_empty(), "{:?}", shadow.divergences());
    }

    #[test]
    fn rejected_precheck_projects_deny() {
        let shadow = KernelShadow::strict_for_tests();
        shadow.begin_turn();
        shadow.round_started();
        shadow.rejected_precheck("call-9");
        finish_clean(&shadow);
        assert!(shadow.divergences().is_empty(), "{:?}", shadow.divergences());
    }

    #[test]
    fn stranded_executing_call_on_abort_recovers_cleanly() {
        // Error propagation can bypass `tool_finished`; abort cleanup must end the
        // execution before settlement so strict mode sees no kernel refusal.
        let shadow = KernelShadow::strict_for_tests();
        shadow.begin_turn();
        shadow.round_started();
        shadow.tool_request("call-1", false, 7);
        shadow.tool_dispatched("call-1");
        shadow.turn_aborting();
        shadow.turn_finished(true);
        let divergences = shadow.divergences();
        assert_eq!(divergences.len(), 1, "{divergences:?}");
        assert!(divergences[0].contains("仍有未完成的调用槽位"));
    }

    #[test]
    fn aborted_wait_is_voided_by_cancel() {
        // Cancel voids the pending wait while the task survives abort settlement.
        let shadow = KernelShadow::strict_for_tests();
        shadow.begin_turn();
        shadow.round_started();
        shadow.agent_spawned("a", tid(1));
        shadow.task_wait_started("a", tid(1));
        shadow.turn_aborting();
        shadow.turn_finished(true);
        assert!(shadow.divergences().is_empty(), "{:?}", shadow.divergences());
    }

    #[test]
    fn fold_during_abort_is_a_visible_divergence() {
        // Folding during abort must be rejected rather than treated as a special projection.
        let shadow = KernelShadow::new(1);
        shadow.begin_turn();
        shadow.round_started();
        shadow.agent_spawned("a", tid(1));
        shadow.turn_aborting();
        shadow.agent_folded("a", tid(1));
        let divergences = shadow.divergences();
        assert!(
            divergences.iter().any(|d| d.contains("AgentFold")),
            "期望取消期间 fold 的分歧被记录:{divergences:?}"
        );
    }

    #[test]
    fn s11_preobserved_idle_completion_is_not_completed_again_at_fold() {
        let shadow = KernelShadow::strict_for_tests();
        shadow.begin_turn();
        shadow.round_started();
        shadow.agent_spawned("a", tid(1));
        finish_clean(&shadow);
        // The main-thread wake snapshot observes completion before TurnStart.
        shadow.with_inner(|inner, strict| {
            let entry = &inner.agents["a"];
            inner.step(strict, KernelEvent::AgentComplete { a: entry.slot, r: entry.rid });
        });
        shadow.begin_turn();
        assert_eq!(shadow.inner.lock().unwrap().kernel.state().wake_fold_pending.len(), 1);
        shadow.agent_folded("a", tid(1));
        shadow.round_started();
        shadow.round_boundary();
        shadow.turn_finished(false);
        assert!(shadow.divergences().is_empty());
    }

    #[test]
    fn s11_wake_snapshot_requires_every_inherited_result_before_round() {
        let shadow = KernelShadow::new(2);
        shadow.begin_turn();
        shadow.round_started();
        shadow.agent_spawned("a", tid(1));
        shadow.agent_spawned("b", tid(1));
        finish_clean(&shadow);
        assert!(shadow.divergences().is_empty());
        shadow.begin_turn_with_completed(vec![("a".into(), tid(1)), ("b".into(), tid(1))]);
        assert_eq!(shadow.inner.lock().unwrap().kernel.state().wake_fold_pending.len(), 2);
        shadow.agent_folded("a", tid(1));
        let before = serde_json::to_value(shadow.inner.lock().unwrap().kernel.state()).unwrap();
        shadow.round_started();
        assert_eq!(shadow.divergences().len(), 1);
        assert!(shadow.divergences()[0].contains("wakeFoldPending = {}"));
        assert_eq!(serde_json::to_value(shadow.inner.lock().unwrap().kernel.state()).unwrap(), before);
        shadow.agent_folded("b", tid(1));
        shadow.round_started();
        shadow.round_boundary();
        shadow.turn_finished(false);
        assert_eq!(shadow.divergences().len(), 1);
    }

    #[test]
    fn s11_cancelled_wake_can_observe_the_same_unclaimed_result_again() {
        let shadow = KernelShadow::strict_for_tests();
        shadow.begin_turn();
        shadow.round_started();
        shadow.agent_spawned("a", tid(1));
        finish_clean(&shadow);
        shadow.begin_turn_with_completed(vec![("a".into(), tid(1))]);
        shadow.turn_aborting();
        shadow.turn_finished(true);
        shadow.begin_turn_with_completed(vec![("a".into(), tid(1))]);
        assert_eq!(shadow.inner.lock().unwrap().kernel.state().wake_fold_pending.len(), 1);
        shadow.agent_folded("a", tid(1));
        finish_clean(&shadow);
        assert!(shadow.divergences().is_empty());
    }

    #[test]
    fn s11_wake_snapshot_rejects_a_mismatched_result_identity() {
        let shadow = KernelShadow::new(1);
        shadow.begin_turn();
        shadow.round_started();
        shadow.agent_spawned("a", tid(1));
        finish_clean(&shadow);
        shadow.begin_turn_with_completed(vec![("a".into(), tid_params(1, 100))]);
        assert_eq!(shadow.divergences().len(), 1);
        assert!(shadow.divergences()[0].contains("r = taskRid[a]"));
        let inner = shadow.inner.lock().unwrap();
        assert_eq!(inner.kernel.state().running_agents(), 1);
        assert!(inner.kernel.state().wake_fold_pending.is_empty());
    }

    #[test]
    fn s11_task_survives_turn_and_folds_in_next_turn() {
        // The mapping survives into the next turn, where a boundary fold delivers it.
        let shadow = KernelShadow::strict_for_tests();
        shadow.begin_turn();
        shadow.round_started();
        shadow.agent_spawned("a", tid(1));
        shadow.round_boundary();
        shadow.round_started();
        shadow.round_boundary();
        shadow.turn_finished(false);
        // The next turn consumes the delivered fold before normal completion.
        shadow.begin_turn();
        shadow.agent_folded("a", tid(1));
        shadow.round_started();
        shadow.round_boundary();
        shadow.turn_finished(false);
        assert!(shadow.divergences().is_empty(), "{:?}", shadow.divergences());
    }

    #[test]
    fn s11_task_survives_aborted_turn() {
        // Aborting a turn does not interrupt the task; the next turn can deliver it.
        let shadow = KernelShadow::strict_for_tests();
        shadow.begin_turn();
        shadow.round_started();
        shadow.agent_spawned("a", tid(1));
        shadow.turn_aborting();
        shadow.turn_finished(true);
        shadow.begin_turn();
        shadow.round_started();
        shadow.task_wait_started("a", tid(1));
        shadow.task_wait_claimed("a", tid(1));
        finish_clean(&shadow);
        assert!(shadow.divergences().is_empty(), "{:?}", shadow.divergences());
    }

    #[test]
    fn aborted_turn_projects_cancel_collapse() {
        let shadow = KernelShadow::strict_for_tests();
        shadow.begin_turn();
        shadow.round_started();
        shadow.tool_request("call-1", false, 7);
        shadow.tool_dispatched("call-1");
        shadow.tool_finished("call-1", false, false, true, 7);
        // An early stop with unresolved delivery work uses the cancellation projection.
        shadow.turn_finished(true);
        assert!(shadow.divergences().is_empty(), "{:?}", shadow.divergences());
    }

    #[test]
    fn stale_envelope_redelivery_is_a_visible_divergence() {
        // A duplicate stale envelope uses a non-current rid and must be rejected.
        let shadow = KernelShadow::new(1);
        shadow.begin_turn();
        shadow.round_started();
        shadow.agent_spawned("a", tid(1));
        shadow.task_wait_started("a", tid(1));
        shadow.task_wait_claimed("a", tid(1));
        shadow.agent_refollowed("a", tid(2));
        shadow.task_wait_started("a", tid(2));
        // Inject a duplicate envelope for the old incarnation.
        shadow.task_wait_claimed("a", tid(1));
        let divergences = shadow.divergences();
        assert!(
            divergences.iter().any(|d| d.contains("r = taskRid[a]")),
            "期望身份守卫分歧被记录:{divergences:?}"
        );
    }

    #[test]
    fn newer_incarnation_envelope_is_a_blind_spot_not_a_divergence() {
        // A newer envelope is a scope gap; the retained old envelope can still deliver.
        let shadow = KernelShadow::strict_for_tests();
        shadow.begin_turn();
        shadow.round_started();
        shadow.agent_spawned("a", tid(1));
        shadow.agent_refollowed("a", tid(2)); // Retain the old incarnation entry.
        shadow.task_wait_started("a", tid(2)); // The current wait covers the tracked envelope.
        shadow.task_wait_claimed("a", tid(2)); // The newer envelope is outside scope.
        shadow.task_wait_claimed("a", tid(1)); // Deliver the tracked old envelope.
        finish_clean(&shadow);
        assert!(shadow.divergences().is_empty(), "{:?}", shadow.divergences());
    }

    #[test]
    fn foreign_digest_same_generation_is_a_divergence() {
        // A same-generation identity with a mismatched digest must use a non-current rid.
        let shadow = KernelShadow::new(1);
        shadow.begin_turn();
        shadow.round_started();
        shadow.agent_spawned("a", tid(1));
        shadow.task_wait_started("a", tid(1));
        shadow.task_wait_claimed("a", tid_params(1, 7));
        let divergences = shadow.divergences();
        assert!(
            divergences.iter().any(|d| d.contains("r = taskRid[a]")),
            "期望身份守卫分歧被记录:{divergences:?}"
        );
    }

    #[test]
    fn a3_params_rewrite_between_request_and_finish_is_a_divergence() {
        // A changed completion digest violates the A3 host-ownership audit.
        let shadow = KernelShadow::new(1);
        shadow.begin_turn();
        shadow.round_started();
        shadow.tool_request("call-1", false, 7);
        shadow.tool_dispatched("call-1");
        shadow.tool_finished("call-1", false, false, true, 8);
        let divergences = shadow.divergences();
        assert!(
            divergences.iter().any(|d| d.contains("A3")),
            "期望 A3 审计分歧被记录:{divergences:?}"
        );
    }
}
