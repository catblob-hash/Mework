//! Kernel state machine. Guards and updates mirror `formal/tla/AgentKernel.tla`
//! line by line; method comments name their TLA+ action and safety invariant.

use serde::{Deserialize, Serialize};

/// One-based slot number, aligned with TLA+ `Calls == 1..N` and `Agents == 1..N`.
pub type SlotId = usize;

/// One-based task identity aligned with TLA+ `TaskIds == 1..N`.
/// Adjacent generations in one slot must differ, and task events must present
/// the current identity. Synchronous call pipelines carry no identity.
pub type TaskId = usize;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum TurnPhase {
    Idle,
    Running,
    Cancelling,
}

/// TLA+ `round`: `prep` is the inter-round boundary and `in_round` is a provider round.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum RoundPhase {
    Prep,
    InRound,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum CallPhase {
    Unused,
    Requested,
    Allowed,
    Denied,
    Executing,
    /// TLA+ `"execbg"`: still executing, but its work has already been handed to
    /// a task slot, so it may not hand off again. Everything else — the receipt
    /// it still owes, and settlement — is unchanged.
    ExecBg,
    Done,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum AgentPhase {
    None,
    Running,
    Done,
}

/// Finite scope. Traces can be replayed only under an equally enlarged model
/// scope when the host uses values larger than the model-checking scope.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct KernelConfig {
    pub call_slots: usize,
    pub agent_slots: usize,
    pub max_live_agents: usize,
    /// Size of the TLA+ `TaskIds == 1..task_ids` identity space.
    pub task_ids: usize,
}

impl KernelConfig {
    /// Configuration matching the checked `formal/tla/AgentKernel.tla` scope.
    /// Tests exporting traces for ProB replay must use this configuration.
    pub const MODEL_SCOPE: KernelConfig = KernelConfig {
        call_slots: 2,
        agent_slots: 2,
        max_live_agents: 1,
        task_ids: 2,
    };
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct CallSlot {
    pub phase: CallPhase,
    /// TLA+ `callDanger`: dangerous calls require `ToolApprove`; others use `ToolAllow`.
    pub danger: bool,
    /// TLA+ `callFresh`: an unconsumed allowance or approval not invalidated by policy tightening.
    pub fresh: bool,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct AgentSlot {
    pub phase: AgentPhase,
    /// TLA+ `taskRid`: current or previous task identity. `AgentSpawn` must
    /// rotate it; every later event for that task must present it. Settlement
    /// retains it as the baseline for the next generation.
    pub rid: TaskId,
    /// TLA+ `agentWaited`: one pending wait. `TaskWaitDeliver` clears it, and
    /// `TurnCancel` voids every pending wait.
    pub waited: bool,
}

/// Kernel events correspond to TLA+ actions and CSP channels. Variant and
/// parameter names must exactly match the TLA+ model; `verify:formal` checks
/// the mirrored vocabulary.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum KernelEvent {
    TurnStart,
    TurnCancel,
    TurnEnd,
    ContextEdit,
    RoundStart,
    RoundEnd,
    SteerEnqueue,
    SteerJoin,
    ToolRequest { c: SlotId, d: bool },
    ToolAllow { c: SlotId },
    ToolApprove { c: SlotId },
    ToolDeny { c: SlotId },
    ToolExecStart { c: SlotId },
    ToolExecEnd { c: SlotId },
    ToolSettle { c: SlotId },
    /// A call whose deadline expired hands its still-running work to task slot
    /// `a` under fresh identity `r`, instead of abandoning it.
    ToolBackground { c: SlotId, a: SlotId, r: TaskId },
    PolicyTighten,
    AgentSpawn { a: SlotId, r: TaskId },
    AgentComplete { a: SlotId, r: TaskId },
    AgentFold { a: SlotId, r: TaskId },
    TaskWait { a: SlotId, r: TaskId },
    TaskWaitTimeout { a: SlotId, r: TaskId },
    TaskWaitDeliver { a: SlotId, r: TaskId },
}

impl KernelEvent {
    /// TLA+ action name and trace JSON transition name.
    pub fn name(&self) -> &'static str {
        match self {
            KernelEvent::TurnStart => "TurnStart",
            KernelEvent::TurnCancel => "TurnCancel",
            KernelEvent::TurnEnd => "TurnEnd",
            KernelEvent::ContextEdit => "ContextEdit",
            KernelEvent::RoundStart => "RoundStart",
            KernelEvent::RoundEnd => "RoundEnd",
            KernelEvent::SteerEnqueue => "SteerEnqueue",
            KernelEvent::SteerJoin => "SteerJoin",
            KernelEvent::ToolRequest { .. } => "ToolRequest",
            KernelEvent::ToolAllow { .. } => "ToolAllow",
            KernelEvent::ToolApprove { .. } => "ToolApprove",
            KernelEvent::ToolDeny { .. } => "ToolDeny",
            KernelEvent::ToolExecStart { .. } => "ToolExecStart",
            KernelEvent::ToolExecEnd { .. } => "ToolExecEnd",
            KernelEvent::ToolSettle { .. } => "ToolSettle",
            KernelEvent::ToolBackground { .. } => "ToolBackground",
            KernelEvent::PolicyTighten => "PolicyTighten",
            KernelEvent::AgentSpawn { .. } => "AgentSpawn",
            KernelEvent::AgentComplete { .. } => "AgentComplete",
            KernelEvent::AgentFold { .. } => "AgentFold",
            KernelEvent::TaskWait { .. } => "TaskWait",
            KernelEvent::TaskWaitTimeout { .. } => "TaskWaitTimeout",
            KernelEvent::TaskWaitDeliver { .. } => "TaskWaitDeliver",
        }
    }

    /// All event names in TLA+ action-definition order, used by the vocabulary guard.
    pub const NAMES: [&'static str; 23] = [
        "TurnStart",
        "TurnCancel",
        "TurnEnd",
        "ContextEdit",
        "RoundStart",
        "RoundEnd",
        "SteerEnqueue",
        "SteerJoin",
        "ToolRequest",
        "ToolAllow",
        "ToolApprove",
        "ToolDeny",
        "PolicyTighten",
        "ToolExecStart",
        "ToolExecEnd",
        "ToolSettle",
        "ToolBackground",
        "AgentSpawn",
        "AgentComplete",
        "AgentFold",
        "TaskWait",
        "TaskWaitTimeout",
        "TaskWaitDeliver",
    ];
}

/// Reason returned when `step()` rejects an event because a safety guard failed.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct KernelRefusal {
    /// Rejected event name.
    pub event: &'static str,
    /// Failed guard, expressed as the original TLA+ predicate.
    pub guard: &'static str,
}

impl std::fmt::Display for KernelRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} 被拒绝:违反守卫 {}", self.event, self.guard)
    }
}

impl std::error::Error for KernelRefusal {}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KernelState {
    pub turn: TurnPhase,
    /// TLA+ `round`.
    pub round: RoundPhase,
    /// TLA+ `roundDue` delivery obligation.
    pub round_due: bool,
    /// TLA+ `steerPending`, retained across turns.
    pub steer_pending: bool,
    /// TLA+ `wakeFoldPending`: slots already done at TurnStart, owed before its first round.
    pub wake_fold_pending: Vec<SlotId>,
    pub calls: Vec<CallSlot>,
    pub agents: Vec<AgentSlot>,
}

impl KernelState {
    /// TLA+ `Init`。
    pub fn new(config: KernelConfig) -> Self {
        KernelState {
            turn: TurnPhase::Idle,
            round: RoundPhase::Prep,
            round_due: false,
            steer_pending: false,
            wake_fold_pending: Vec::new(),
            calls: vec![
                CallSlot { phase: CallPhase::Unused, danger: false, fresh: false };
                config.call_slots
            ],
            agents: vec![
                // TLA+ Init uses `rid = 1`; the first task must use a different identity.
                AgentSlot { phase: AgentPhase::None, rid: 1, waited: false };
                config.agent_slots
            ],
        }
    }

    /// Cardinality of TLA+ `RunningAgents`.
    pub fn running_agents(&self) -> usize {
        self.agents.iter().filter(|a| a.phase == AgentPhase::Running).count()
    }
}

/// State machine plus scope configuration. `step()` is the only state-transition entry point.
#[derive(Clone, Debug)]
pub struct Kernel {
    config: KernelConfig,
    state: KernelState,
}

macro_rules! guard {
    ($cond:expr, $event:expr, $guard:literal) => {
        if !($cond) {
            return Err(KernelRefusal { event: $event, guard: $guard });
        }
    };
}

impl Kernel {
    pub fn new(config: KernelConfig) -> Self {
        Kernel { config, state: KernelState::new(config) }
    }

    pub fn state(&self) -> &KernelState {
        &self.state
    }

    pub fn config(&self) -> KernelConfig {
        self.config
    }

    fn call(&self, c: SlotId, event: &'static str) -> Result<&CallSlot, KernelRefusal> {
        self.state
            .calls
            .get(c.wrapping_sub(1))
            .ok_or(KernelRefusal { event, guard: "c \\in Calls" })
    }

    fn agent(&self, a: SlotId, event: &'static str) -> Result<&AgentSlot, KernelRefusal> {
        self.state
            .agents
            .get(a.wrapping_sub(1))
            .ok_or(KernelRefusal { event, guard: "a \\in Agents" })
    }

    // Task identities outside `TaskIds` are invalid for every action.
    fn tid_in_scope(&self, r: TaskId, event: &'static str) -> Result<(), KernelRefusal> {
        if (1..=self.config.task_ids).contains(&r) {
            Ok(())
        } else {
            Err(KernelRefusal { event, guard: "r \\in TaskIds" })
        }
    }

    /// Applies an event only when every guard passes; rejected events leave
    /// state unchanged. Guard order matches TLA+ action conjunct order.
    pub fn step(&mut self, event: KernelEvent) -> Result<(), KernelRefusal> {
        use KernelEvent as E;
        match event {
            // `TurnStart` creates the first-round delivery obligation.
            E::TurnStart => {
                guard!(self.state.turn == TurnPhase::Idle, "TurnStart", "turn = \"idle\"");
                self.state.turn = TurnPhase::Running;
                self.state.round_due = true;
                self.state.wake_fold_pending = self.state.agents.iter().enumerate()
                    .filter_map(|(index, slot)| (slot.phase == AgentPhase::Done).then_some(index + 1))
                    .collect();
            }
            // Cancellation abandons the delivery obligation at `TurnEnd` and
            // voids every pending wait.
            E::TurnCancel => {
                guard!(self.state.turn == TurnPhase::Running, "TurnCancel", "turn = \"running\"");
                self.state.turn = TurnPhase::Cancelling;
                for slot in &mut self.state.agents {
                    slot.waited = false;
                }
            }
            // Normal settlement occurs in `prep`, with no pending delivery or
            // steer. Running tasks survive; completed tasks must be folded,
            // except that cancellation may carry them into the next turn.
            E::TurnEnd => {
                guard!(
                    matches!(self.state.turn, TurnPhase::Running | TurnPhase::Cancelling),
                    "TurnEnd",
                    "turn \\in {\"running\", \"cancelling\"}"
                );
                guard!(self.state.round == RoundPhase::Prep, "TurnEnd", "round = \"prep\"");
                guard!(
                    self.state.agents.iter().all(|s| matches!(
                        s.phase,
                        AgentPhase::None | AgentPhase::Running
                    ) || (self.state.turn == TurnPhase::Cancelling
                        && s.phase == AgentPhase::Done)),
                    "TurnEnd",
                    "\\A a \\in Agents : agentPhase[a] \\in {\"none\",\"running\"} \\/ (turn = \"cancelling\" /\\ agentPhase[a] = \"done\")"
                );
                guard!(
                    self.state.turn == TurnPhase::Cancelling
                        || (!self.state.round_due && !self.state.steer_pending),
                    "TurnEnd",
                    "turn = \"cancelling\" \\/ (~roundDue /\\ ~steerPending)"
                );
                self.state.turn = TurnPhase::Idle;
                self.state.round_due = false;
            }
            // Context is editable only between turns; persisted data is
            // authoritative, so the event has no kernel-state effect.
            E::ContextEdit => {
                guard!(self.state.turn == TurnPhase::Idle, "ContextEdit", "turn = \"idle\"");
            }
            // A round starts only in a running `prep` state with a due
            // delivery and no pending steer; starting consumes the obligation.
            E::RoundStart => {
                guard!(self.state.turn == TurnPhase::Running, "RoundStart", "turn = \"running\"");
                guard!(self.state.wake_fold_pending.is_empty(), "RoundStart", "wakeFoldPending = {}");
                guard!(self.state.round == RoundPhase::Prep, "RoundStart", "round = \"prep\"");
                guard!(self.state.round_due, "RoundStart", "roundDue");
                guard!(!self.state.steer_pending, "RoundStart", "~steerPending");
                self.state.round = RoundPhase::InRound;
                self.state.round_due = false;
            }
            // A round cannot end with unsettled calls or pending waits.
            E::RoundEnd => {
                guard!(self.state.round == RoundPhase::InRound, "RoundEnd", "round = \"inround\"");
                guard!(
                    self.state.calls.iter().all(|s| s.phase == CallPhase::Unused),
                    "RoundEnd",
                    "\\A c \\in Calls : callPhase[c] = \"unused\""
                );
                guard!(
                    self.state.agents.iter().all(|s| !s.waited),
                    "RoundEnd",
                    "\\A a \\in Agents : ~agentWaited[a]"
                );
                self.state.round = RoundPhase::Prep;
            }
            // Steers are enqueued only during an active turn.
            E::SteerEnqueue => {
                guard!(
                    matches!(self.state.turn, TurnPhase::Running | TurnPhase::Cancelling),
                    "SteerEnqueue",
                    "turn \\in {\"running\", \"cancelling\"}"
                );
                self.state.steer_pending = true;
            }
            // Joining a steer is allowed only in a running `prep` state and
            // creates a new delivery obligation.
            E::SteerJoin => {
                guard!(self.state.turn == TurnPhase::Running, "SteerJoin", "turn = \"running\"");
                guard!(self.state.round == RoundPhase::Prep, "SteerJoin", "round = \"prep\"");
                guard!(self.state.steer_pending, "SteerJoin", "steerPending");
                self.state.steer_pending = false;
                self.state.round_due = true;
            }
            // Synchronous requests start only during running rounds and carry
            // no task identity; parameter immutability is environmental A3.
            E::ToolRequest { c, d } => {
                guard!(self.state.turn == TurnPhase::Running, "ToolRequest", "turn = \"running\"");
                guard!(self.state.round == RoundPhase::InRound, "ToolRequest", "round = \"inround\"");
                let slot = self.call(c, "ToolRequest")?;
                guard!(slot.phase == CallPhase::Unused, "ToolRequest", "callPhase[c] = \"unused\"");
                let slot = &mut self.state.calls[c - 1];
                slot.phase = CallPhase::Requested;
                slot.danger = d;
                slot.fresh = false;
            }
            // Non-dangerous calls can be re-allowed after policy tightening;
            // otherwise an allowed call could never settle naturally.
            E::ToolAllow { c } => {
                guard!(self.state.turn == TurnPhase::Running, "ToolAllow", "turn = \"running\"");
                let slot = self.call(c, "ToolAllow")?;
                guard!(
                    matches!(slot.phase, CallPhase::Requested | CallPhase::Allowed),
                    "ToolAllow",
                    "callPhase[c] \\in {\"requested\", \"allowed\"}"
                );
                guard!(!slot.danger, "ToolAllow", "~callDanger[c]");
                let slot = &mut self.state.calls[c - 1];
                slot.phase = CallPhase::Allowed;
                slot.fresh = true;
            }
            // Dangerous calls require user approval and may be re-approved;
            // environmental A3 binds approved and executed parameters.
            E::ToolApprove { c } => {
                guard!(self.state.turn == TurnPhase::Running, "ToolApprove", "turn = \"running\"");
                let slot = self.call(c, "ToolApprove")?;
                guard!(
                    matches!(slot.phase, CallPhase::Requested | CallPhase::Allowed),
                    "ToolApprove",
                    "callPhase[c] \\in {\"requested\", \"allowed\"}"
                );
                guard!(slot.danger, "ToolApprove", "callDanger[c]");
                let slot = &mut self.state.calls[c - 1];
                slot.phase = CallPhase::Allowed;
                slot.fresh = true;
            }
            E::ToolDeny { c } => {
                guard!(self.state.turn == TurnPhase::Running, "ToolDeny", "turn = \"running\"");
                let slot = self.call(c, "ToolDeny")?;
                guard!(slot.phase == CallPhase::Requested, "ToolDeny", "callPhase[c] = \"requested\"");
                let slot = &mut self.state.calls[c - 1];
                slot.phase = CallPhase::Denied;
                slot.fresh = false;
            }
            // Policy tightening invalidates every unconsumed allowance or approval.
            E::PolicyTighten => {
                for slot in &mut self.state.calls {
                    slot.fresh = false;
                }
            }
            // Execution consumes a fresh approval during a running turn. The
            // remaining guards follow structurally from the TLA+ invariants.
            E::ToolExecStart { c } => {
                guard!(self.state.turn == TurnPhase::Running, "ToolExecStart", "turn = \"running\"");
                let slot = self.call(c, "ToolExecStart")?;
                guard!(slot.fresh, "ToolExecStart", "callFresh[c]");
                let slot = &mut self.state.calls[c - 1];
                slot.phase = CallPhase::Executing;
                slot.fresh = false;
            }
            // `Done` is reachable only from execution; it may finish during
            // cancellation. A call that handed its work to a task slot is still
            // executing for receipt purposes and leaves through this same door.
            E::ToolExecEnd { c } => {
                let slot = self.call(c, "ToolExecEnd")?;
                guard!(
                    matches!(slot.phase, CallPhase::Executing | CallPhase::ExecBg),
                    "ToolExecEnd",
                    "callPhase[c] \\in {\"executing\", \"execbg\"}"
                );
                self.state.calls[c - 1].phase = CallPhase::Done;
            }
            // Settlement recycles a slot. Only cancellation may discard an
            // unresolved call; every settlement creates a delivery obligation.
            E::ToolSettle { c } => {
                let slot = self.call(c, "ToolSettle")?;
                let settleable = matches!(slot.phase, CallPhase::Done | CallPhase::Denied)
                    || (matches!(slot.phase, CallPhase::Requested | CallPhase::Allowed)
                        && self.state.turn == TurnPhase::Cancelling);
                guard!(
                    settleable,
                    "ToolSettle",
                    "callPhase[c] \\in {\"done\",\"denied\"} \\/ (callPhase[c] \\in {\"requested\",\"allowed\"} /\\ turn = \"cancelling\")"
                );
                let slot = &mut self.state.calls[c - 1];
                slot.phase = CallPhase::Unused;
                slot.fresh = false;
                self.state.round_due = true;
            }
            // A deadline does not abandon running work: the executing call hands
            // its process to an empty task slot under a fresh identity and moves
            // to `ExecBg`, which is executing minus the right to hand off again.
            // The handoff is counted by the same capacity limit a spawn obeys,
            // and charges no delivery obligation of its own — the call's own
            // `ToolSettle` charges one for the receipt that names the task.
            // `InvCallsInRound` implies the omitted round guard.
            E::ToolBackground { c, a, r } => {
                guard!(
                    self.state.turn == TurnPhase::Running,
                    "ToolBackground",
                    "turn = \"running\""
                );
                let call = self.call(c, "ToolBackground")?;
                guard!(
                    call.phase == CallPhase::Executing,
                    "ToolBackground",
                    "callPhase[c] = \"executing\""
                );
                guard!(
                    self.state.running_agents() < self.config.max_live_agents,
                    "ToolBackground",
                    "Cardinality(RunningAgents) < MaxLiveAgents"
                );
                self.tid_in_scope(r, "ToolBackground")?;
                let agent = self.agent(a, "ToolBackground")?;
                guard!(
                    agent.phase == AgentPhase::None,
                    "ToolBackground",
                    "agentPhase[a] = \"none\""
                );
                guard!(r != agent.rid, "ToolBackground", "r # taskRid[a]");
                self.state.calls[c - 1].phase = CallPhase::ExecBg;
                let agent = &mut self.state.agents[a - 1];
                agent.phase = AgentPhase::Running;
                agent.rid = r;
            }
            // Spawn only within a running round and below capacity. The new
            // task identity must differ from this slot's previous generation.
            E::AgentSpawn { a, r } => {
                guard!(self.state.turn == TurnPhase::Running, "AgentSpawn", "turn = \"running\"");
                guard!(self.state.round == RoundPhase::InRound, "AgentSpawn", "round = \"inround\"");
                self.tid_in_scope(r, "AgentSpawn")?;
                let slot = self.agent(a, "AgentSpawn")?;
                guard!(slot.phase == AgentPhase::None, "AgentSpawn", "agentPhase[a] = \"none\"");
                guard!(
                    self.state.running_agents() < self.config.max_live_agents,
                    "AgentSpawn",
                    "Cardinality(RunningAgents) < MaxLiveAgents"
                );
                guard!(r != slot.rid, "AgentSpawn", "r # taskRid[a]");
                let slot = &mut self.state.agents[a - 1];
                slot.phase = AgentPhase::Running;
                slot.rid = r;
                self.state.round_due = true;
            }
            // Completion must present the current identity. A task can complete
            // while idle, creating the host's wake obligation; pending waits
            // remain available for delivery. It is the only exit from running,
            // whatever ended the task — the model finishing, a host failure, or
            // the user closing it — so no terminal result can bypass delivery.
            E::AgentComplete { a, r } => {
                self.tid_in_scope(r, "AgentComplete")?;
                let slot = self.agent(a, "AgentComplete")?;
                guard!(
                    slot.phase == AgentPhase::Running,
                    "AgentComplete",
                    "agentPhase[a] = \"running\""
                );
                guard!(r == slot.rid, "AgentComplete", "r = taskRid[a]");
                self.state.agents[a - 1].phase = AgentPhase::Done;
            }
            // Unwaited completed results fold only in `prep`, creating a
            // delivery obligation. A running task never folds.
            E::AgentFold { a, r } => {
                guard!(self.state.turn == TurnPhase::Running, "AgentFold", "turn = \"running\"");
                guard!(self.state.round == RoundPhase::Prep, "AgentFold", "round = \"prep\"");
                self.tid_in_scope(r, "AgentFold")?;
                let slot = self.agent(a, "AgentFold")?;
                guard!(slot.phase == AgentPhase::Done, "AgentFold", "agentPhase[a] = \"done\"");
                guard!(r == slot.rid, "AgentFold", "r = taskRid[a]");
                self.state.agents[a - 1].phase = AgentPhase::None;
                self.state.wake_fold_pending.retain(|pending| *pending != a);
                self.state.round_due = true;
            }
            // A model may wait once for its current task during a running round.
            // Waiting on a running task blocks; a settled task is deliverable.
            E::TaskWait { a, r } => {
                guard!(self.state.turn == TurnPhase::Running, "TaskWait", "turn = \"running\"");
                guard!(self.state.round == RoundPhase::InRound, "TaskWait", "round = \"inround\"");
                self.tid_in_scope(r, "TaskWait")?;
                let slot = self.agent(a, "TaskWait")?;
                guard!(slot.phase != AgentPhase::None, "TaskWait", "agentPhase[a] # \"none\"");
                guard!(!slot.waited, "TaskWait", "~agentWaited[a]");
                guard!(r == slot.rid, "TaskWait", "r = taskRid[a]");
                self.state.agents[a - 1].waited = true;
            }
            // Timeout withdraws an undelivered wait on a running task and
            // creates a delivery obligation. Structural invariants imply the
            // omitted turn and round guards.
            E::TaskWaitTimeout { a, r } => {
                self.tid_in_scope(r, "TaskWaitTimeout")?;
                let slot = self.agent(a, "TaskWaitTimeout")?;
                guard!(slot.waited, "TaskWaitTimeout", "agentWaited[a]");
                guard!(
                    slot.phase == AgentPhase::Running,
                    "TaskWaitTimeout",
                    "agentPhase[a] = \"running\""
                );
                guard!(r == slot.rid, "TaskWaitTimeout", "r = taskRid[a]");
                self.state.round_due = true;
                self.state.agents[a - 1].waited = false;
            }
            // Delivery requires a pending wait and a settled current task, then
            // creates a delivery obligation. Structural invariants imply the
            // omitted turn and round guards.
            E::TaskWaitDeliver { a, r } => {
                self.tid_in_scope(r, "TaskWaitDeliver")?;
                let slot = self.agent(a, "TaskWaitDeliver")?;
                guard!(slot.waited, "TaskWaitDeliver", "agentWaited[a]");
                guard!(
                    slot.phase == AgentPhase::Done,
                    "TaskWaitDeliver",
                    "agentPhase[a] = \"done\""
                );
                guard!(r == slot.rid, "TaskWaitDeliver", "r = taskRid[a]");
                let slot = &mut self.state.agents[a - 1];
                slot.phase = AgentPhase::None;
                slot.waited = false;
                self.state.round_due = true;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use KernelEvent as E;

    fn kernel() -> Kernel {
        Kernel::new(KernelConfig::MODEL_SCOPE)
    }

    fn drive(kernel: &mut Kernel, events: &[KernelEvent]) {
        for event in events {
            kernel.step(*event).unwrap_or_else(|refusal| panic!("{refusal:?}"));
        }
    }

    #[test]
    fn approve_execute_settle_roundtrip() {
        let mut k = kernel();
        drive(
            &mut k,
            &[
                E::TurnStart,
                E::RoundStart,
                E::ToolRequest { c: 1, d: true },
                E::ToolApprove { c: 1 },
                E::ToolExecStart { c: 1 },
                E::ToolExecEnd { c: 1 },
                E::ToolSettle { c: 1 },
                E::RoundEnd,
                E::RoundStart,
                E::RoundEnd,
                E::TurnEnd,
            ],
        );
        assert_eq!(k.state().turn, TurnPhase::Idle);
    }

    #[test]
    fn s1_execution_without_approval_refused() {
        let mut k = kernel();
        drive(&mut k, &[E::TurnStart, E::RoundStart, E::ToolRequest { c: 1, d: true }]);
        let refusal = k.step(E::ToolExecStart { c: 1 }).unwrap_err();
        assert_eq!(refusal.guard, "callFresh[c]");
    }

    #[test]
    fn s1_dangerous_call_cannot_auto_allow() {
        let mut k = kernel();
        drive(&mut k, &[E::TurnStart, E::RoundStart, E::ToolRequest { c: 1, d: true }]);
        assert!(k.step(E::ToolAllow { c: 1 }).is_err());
    }

    #[test]
    fn s2_approval_consumed_once() {
        let mut k = kernel();
        drive(
            &mut k,
            &[
                E::TurnStart,
                E::RoundStart,
                E::ToolRequest { c: 1, d: true },
                E::ToolApprove { c: 1 },
                E::ToolExecStart { c: 1 },
                E::ToolExecEnd { c: 1 },
            ],
        );
        assert!(k.step(E::ToolExecStart { c: 1 }).is_err());
    }

    #[test]
    fn s3_policy_tighten_invalidates_pending_approval() {
        let mut k = kernel();
        drive(
            &mut k,
            &[
                E::TurnStart,
                E::RoundStart,
                E::ToolRequest { c: 1, d: true },
                E::ToolApprove { c: 1 },
                E::PolicyTighten,
            ],
        );
        let refusal = k.step(E::ToolExecStart { c: 1 }).unwrap_err();
        assert_eq!(refusal.guard, "callFresh[c]");
        drive(&mut k, &[E::ToolApprove { c: 1 }, E::ToolExecStart { c: 1 }]);
    }

    #[test]
    fn s4_no_new_work_after_cancel() {
        let mut k = kernel();
        drive(&mut k, &[E::TurnStart, E::RoundStart, E::TurnCancel]);
        assert!(k.step(E::ToolRequest { c: 1, d: false }).is_err());
        assert!(k.step(E::AgentSpawn { a: 1, r: 2 }).is_err());
        drive(&mut k, &[E::RoundEnd]);
        let refusal = k.step(E::RoundStart).unwrap_err();
        assert_eq!(refusal.guard, "turn = \"running\"");
    }

    #[test]
    fn s4_inflight_execution_may_finish_during_cancelling() {
        let mut k = kernel();
        drive(
            &mut k,
            &[
                E::TurnStart,
                E::RoundStart,
                E::ToolRequest { c: 1, d: false },
                E::ToolAllow { c: 1 },
                E::ToolExecStart { c: 1 },
                E::TurnCancel,
            ],
        );
        drive(&mut k, &[E::ToolExecEnd { c: 1 }, E::ToolSettle { c: 1 }, E::RoundEnd, E::TurnEnd]);
    }

    #[test]
    fn s11_running_task_survives_turn_end_and_wakes() {
        let mut k = kernel();
        drive(
            &mut k,
            &[E::TurnStart, E::RoundStart, E::AgentSpawn { a: 1, r: 2 }, E::RoundEnd, E::RoundStart, E::RoundEnd, E::TurnEnd],
        );
        assert_eq!(k.state().turn, TurnPhase::Idle);
        assert_eq!(k.state().agents[0].phase, AgentPhase::Running);
        drive(&mut k, &[E::AgentComplete { a: 1, r: 2 }]);
        assert_eq!(k.step(E::AgentFold { a: 1, r: 2 }).unwrap_err().guard, "turn = \"running\"");
        drive(&mut k, &[E::TurnStart, E::AgentFold { a: 1, r: 2 }, E::RoundStart, E::RoundEnd, E::TurnEnd]);
    }

    #[test]
    fn s11_turn_cannot_end_with_unfolded_done() {
        let mut k = kernel();
        drive(
            &mut k,
            &[
                E::TurnStart,
                E::RoundStart,
                E::AgentSpawn { a: 1, r: 2 },
                E::AgentComplete { a: 1, r: 2 },
                E::RoundEnd,
                E::RoundStart,
                E::RoundEnd,
            ],
        );
        let refusal = k.step(E::TurnEnd).unwrap_err();
        assert_eq!(
            refusal.guard,
            "\\A a \\in Agents : agentPhase[a] \\in {\"none\",\"running\"} \\/ (turn = \"cancelling\" /\\ agentPhase[a] = \"done\")"
        );
        drive(&mut k, &[E::AgentFold { a: 1, r: 2 }, E::RoundStart, E::RoundEnd, E::TurnEnd]);
    }

    /// A task the user closes while the conversation is idle settles like any
    /// other terminal result: it waits for the host's `TurnStart`, folds, and
    /// charges the delivery obligation that forces the wake round.
    #[test]
    fn s11_idle_stop_folds_and_forces_a_wake_round() {
        let mut k = kernel();
        drive(
            &mut k,
            &[E::TurnStart, E::RoundStart, E::AgentSpawn { a: 1, r: 2 }, E::RoundEnd, E::RoundStart, E::RoundEnd, E::TurnEnd],
        );
        assert_eq!(k.step(E::AgentComplete { a: 1, r: 1 }).unwrap_err().guard, "r = taskRid[a]");
        drive(&mut k, &[E::AgentComplete { a: 1, r: 2 }]);
        assert_eq!(k.state().agents[0].phase, AgentPhase::Done);
        assert_eq!(k.state().turn, TurnPhase::Idle);
        drive(&mut k, &[E::TurnStart, E::AgentFold { a: 1, r: 2 }]);
        assert!(k.step(E::TurnEnd).is_err(), "fold charges the delivery obligation");
        drive(&mut k, &[E::RoundStart, E::RoundEnd, E::TurnEnd]);
        assert_eq!(k.state().agents[0].phase, AgentPhase::None);
    }

    #[test]
    fn s11_cancel_preserves_tasks_and_done_results() {
        let mut k = kernel();
        drive(
            &mut k,
            &[
                E::TurnStart,
                E::RoundStart,
                E::AgentSpawn { a: 1, r: 2 },
                E::AgentComplete { a: 1, r: 2 },
                E::TurnCancel,
                E::RoundEnd,
            ],
        );
        assert_eq!(
            k.step(E::AgentFold { a: 1, r: 2 }).unwrap_err().guard,
            "turn = \"running\""
        );
        drive(&mut k, &[E::TurnEnd]);
        assert_eq!(k.state().agents[0].phase, AgentPhase::Done);
        drive(&mut k, &[E::TurnStart, E::AgentFold { a: 1, r: 2 }, E::RoundStart, E::RoundEnd, E::TurnEnd]);
    }

    #[test]
    fn s11_next_turn_adopts_surviving_task() {
        let mut k = kernel();
        drive(
            &mut k,
            &[
                E::TurnStart,
                E::RoundStart,
                E::AgentSpawn { a: 1, r: 2 },
                E::RoundEnd,
                E::RoundStart,
                E::RoundEnd,
                E::TurnEnd,
                E::TurnStart,
                E::RoundStart,
                E::TaskWait { a: 1, r: 2 },
                E::AgentComplete { a: 1, r: 2 },
                E::TaskWaitDeliver { a: 1, r: 2 },
                E::RoundEnd,
                E::RoundStart,
                E::RoundEnd,
                E::TurnEnd,
            ],
        );
        assert_eq!(k.state().turn, TurnPhase::Idle);
    }

    #[test]
    fn s5_round_cannot_end_with_pending_call() {
        let mut k = kernel();
        drive(&mut k, &[E::TurnStart, E::RoundStart, E::ToolRequest { c: 1, d: false }]);
        let refusal = k.step(E::RoundEnd).unwrap_err();
        assert_eq!(refusal.guard, "\\A c \\in Calls : callPhase[c] = \"unused\"");
        let refusal = k.step(E::TurnEnd).unwrap_err();
        assert_eq!(refusal.guard, "round = \"prep\"");
    }

    #[test]
    fn s5_pending_call_dropped_only_via_cancel_path() {
        let mut k = kernel();
        drive(&mut k, &[E::TurnStart, E::RoundStart, E::ToolRequest { c: 1, d: false }]);
        assert!(k.step(E::ToolSettle { c: 1 }).is_err());
        drive(&mut k, &[E::TurnCancel, E::ToolSettle { c: 1 }, E::RoundEnd, E::TurnEnd]);
    }

    #[test]
    fn s6_capacity_enforced_independent_of_slots() {
        let mut k = kernel();
        drive(&mut k, &[E::TurnStart, E::RoundStart, E::AgentSpawn { a: 1, r: 2 }]);
        let refusal = k.step(E::AgentSpawn { a: 2, r: 2 }).unwrap_err();
        assert_eq!(refusal.guard, "Cardinality(RunningAgents) < MaxLiveAgents");
    }

    #[test]
    fn s7_no_receipt_without_execution_and_no_double_receipt() {
        let mut k = kernel();
        drive(
            &mut k,
            &[E::TurnStart, E::RoundStart, E::ToolRequest { c: 1, d: false }, E::ToolAllow { c: 1 }],
        );
        assert!(k.step(E::ToolExecEnd { c: 1 }).is_err());
        drive(&mut k, &[E::ToolExecStart { c: 1 }, E::ToolExecEnd { c: 1 }]);
        assert!(k.step(E::ToolExecEnd { c: 1 }).is_err());
    }

    #[test]
    fn s8_wait_on_running_blocks_round_until_delivery() {
        let mut k = kernel();
        drive(&mut k, &[E::TurnStart, E::RoundStart, E::AgentSpawn { a: 1, r: 2 }]);
        drive(&mut k, &[E::TaskWait { a: 1, r: 2 }]);
        let refusal = k.step(E::RoundEnd).unwrap_err();
        assert_eq!(refusal.guard, "\\A a \\in Agents : ~agentWaited[a]");
        let refusal = k.step(E::TaskWaitDeliver { a: 1, r: 2 }).unwrap_err();
        assert_eq!(refusal.guard, "agentPhase[a] = \"done\"");
        drive(
            &mut k,
            &[
                E::AgentComplete { a: 1, r: 2 },
                E::TaskWaitDeliver { a: 1, r: 2 },
                E::RoundEnd,
                E::RoundStart,
                E::RoundEnd,
                E::TurnEnd,
            ],
        );
        assert_eq!(k.state().turn, TurnPhase::Idle);
    }

    #[test]
    fn s8_wait_semantics_guards() {
        let mut k = kernel();
        drive(&mut k, &[E::TurnStart, E::RoundStart]);
        let refusal = k.step(E::TaskWait { a: 1, r: 2 }).unwrap_err();
        assert_eq!(refusal.guard, "agentPhase[a] # \"none\"");
        drive(&mut k, &[E::AgentSpawn { a: 1, r: 2 }, E::AgentComplete { a: 1, r: 2 }]);
        let refusal = k.step(E::TaskWaitDeliver { a: 1, r: 2 }).unwrap_err();
        assert_eq!(refusal.guard, "agentWaited[a]");
        drive(&mut k, &[E::TaskWait { a: 1, r: 2 }]);
        let refusal = k.step(E::TaskWait { a: 1, r: 2 }).unwrap_err();
        assert_eq!(refusal.guard, "~agentWaited[a]");
        drive(&mut k, &[E::TaskWaitDeliver { a: 1, r: 2 }, E::RoundEnd, E::RoundStart, E::RoundEnd, E::TurnEnd]);
    }

    #[test]
    fn s8_wait_timeout_withdraws_and_task_survives() {
        let mut k = kernel();
        drive(
            &mut k,
            &[E::TurnStart, E::RoundStart, E::AgentSpawn { a: 1, r: 2 }, E::TaskWait { a: 1, r: 2 }],
        );
        drive(&mut k, &[E::TaskWaitTimeout { a: 1, r: 2 }]);
        assert_eq!(k.state().agents[0].phase, AgentPhase::Running);
        assert_eq!(k.step(E::TaskWaitTimeout { a: 1, r: 2 }).unwrap_err().guard, "agentWaited[a]");
        drive(&mut k, &[E::TaskWait { a: 1, r: 2 }, E::AgentComplete { a: 1, r: 2 }]);
        assert_eq!(
            k.step(E::TaskWaitTimeout { a: 1, r: 2 }).unwrap_err().guard,
            "agentPhase[a] = \"running\""
        );
        drive(
            &mut k,
            &[E::TaskWaitDeliver { a: 1, r: 2 }, E::RoundEnd, E::RoundStart, E::RoundEnd, E::TurnEnd],
        );
    }

    #[test]
    fn s8_cancel_voids_pending_wait() {
        let mut k = kernel();
        drive(
            &mut k,
            &[
                E::TurnStart,
                E::RoundStart,
                E::AgentSpawn { a: 1, r: 2 },
                E::TaskWait { a: 1, r: 2 },
                E::AgentComplete { a: 1, r: 2 },
                E::TurnCancel,
            ],
        );
        let refusal = k.step(E::TaskWaitDeliver { a: 1, r: 2 }).unwrap_err();
        assert_eq!(refusal.guard, "agentWaited[a]");
        let refusal = k.step(E::TaskWait { a: 1, r: 2 }).unwrap_err();
        assert_eq!(refusal.guard, "turn = \"running\"");
        drive(&mut k, &[E::RoundEnd, E::TurnEnd]);
        assert_eq!(k.state().agents[0].phase, AgentPhase::Done);
    }

    #[test]
    fn s8_context_edit_only_between_turns() {
        let mut k = kernel();
        drive(&mut k, &[E::ContextEdit, E::TurnStart]);
        let refusal = k.step(E::ContextEdit).unwrap_err();
        assert_eq!(refusal.guard, "turn = \"idle\"");
        drive(&mut k, &[E::TurnCancel]);
        assert!(k.step(E::ContextEdit).is_err());
        drive(&mut k, &[E::TurnEnd, E::ContextEdit]);
    }

    #[test]
    fn s9_user_message_forces_first_round() {
        let mut k = kernel();
        drive(&mut k, &[E::TurnStart]);
        let refusal = k.step(E::TurnEnd).unwrap_err();
        assert_eq!(refusal.guard, "turn = \"cancelling\" \\/ (~roundDue /\\ ~steerPending)");
        drive(&mut k, &[E::RoundStart, E::RoundEnd, E::TurnEnd]);
    }

    #[test]
    fn s9_no_idle_round_without_due() {
        let mut k = kernel();
        drive(&mut k, &[E::TurnStart, E::RoundStart, E::RoundEnd]);
        let refusal = k.step(E::RoundStart).unwrap_err();
        assert_eq!(refusal.guard, "roundDue");
        drive(&mut k, &[E::TurnEnd]);
    }

    #[test]
    fn s9_settle_receipt_forces_delivery_round() {
        let mut k = kernel();
        drive(
            &mut k,
            &[
                E::TurnStart,
                E::RoundStart,
                E::ToolRequest { c: 1, d: false },
                E::ToolAllow { c: 1 },
                E::ToolExecStart { c: 1 },
                E::ToolExecEnd { c: 1 },
                E::ToolSettle { c: 1 },
                E::RoundEnd,
            ],
        );
        assert!(k.step(E::TurnEnd).is_err());
        drive(&mut k, &[E::RoundStart, E::RoundEnd, E::TurnEnd]);
    }

    #[test]
    fn s9_fold_only_done_at_prep_and_retriggers() {
        let mut k = kernel();
        drive(&mut k, &[E::TurnStart, E::RoundStart, E::AgentSpawn { a: 1, r: 2 }, E::AgentComplete { a: 1, r: 2 }]);
        let refusal = k.step(E::AgentFold { a: 1, r: 2 }).unwrap_err();
        assert_eq!(refusal.guard, "round = \"prep\"");
        drive(&mut k, &[E::RoundEnd]);
        drive(&mut k, &[E::AgentFold { a: 1, r: 2 }]);
        assert!(k.step(E::TurnEnd).is_err());
        drive(&mut k, &[E::RoundStart, E::RoundEnd, E::TurnEnd]);
    }

    /// A task the user stopped mid-turn still yields a terminal result, so its
    /// fold charges `roundDue` and the turn cannot close without delivering it.
    #[test]
    fn s9_stopped_task_result_retriggers() {
        let mut k = kernel();
        drive(
            &mut k,
            &[
                E::TurnStart,
                E::RoundStart,
                E::AgentSpawn { a: 1, r: 2 },
                E::RoundEnd,
                E::RoundStart,
                E::RoundEnd,
                E::AgentComplete { a: 1, r: 2 },
            ],
        );
        drive(&mut k, &[E::AgentFold { a: 1, r: 2 }]);
        assert!(k.step(E::TurnEnd).is_err(), "a stopped task's result owes a round");
        drive(&mut k, &[E::RoundStart, E::RoundEnd, E::TurnEnd]);
        assert_eq!(k.state().turn, TurnPhase::Idle);
    }

    #[test]
    fn s9_steer_joins_only_at_prep_and_forces_round() {
        let mut k = kernel();
        drive(&mut k, &[E::TurnStart, E::RoundStart, E::SteerEnqueue, E::AgentSpawn { a: 1, r: 2 }]);
        let refusal = k.step(E::SteerJoin).unwrap_err();
        assert_eq!(refusal.guard, "round = \"prep\"");
        drive(&mut k, &[E::RoundEnd]);
        assert_eq!(k.step(E::RoundStart).unwrap_err().guard, "~steerPending");
        assert!(k.step(E::TurnEnd).is_err());
        drive(
            &mut k,
            &[
                E::SteerJoin,
                E::RoundStart,
                E::AgentComplete { a: 1, r: 2 },
                E::TaskWait { a: 1, r: 2 },
                E::TaskWaitDeliver { a: 1, r: 2 },
                E::RoundEnd,
                E::RoundStart,
                E::RoundEnd,
                E::TurnEnd,
            ],
        );
    }

    #[test]
    fn s9_steer_survives_cancelled_turn() {
        let mut k = kernel();
        drive(&mut k, &[E::TurnStart, E::RoundStart, E::SteerEnqueue, E::TurnCancel, E::RoundEnd, E::TurnEnd]);
        assert!(k.step(E::SteerEnqueue).is_err());
        assert!(k.state().steer_pending);
        drive(&mut k, &[E::TurnStart]);
        assert_eq!(k.step(E::RoundStart).unwrap_err().guard, "~steerPending");
        drive(&mut k, &[E::SteerJoin, E::RoundStart, E::RoundEnd, E::TurnEnd]);
    }

    #[test]
    fn refusal_leaves_state_unchanged() {
        let mut k = kernel();
        drive(&mut k, &[E::TurnStart, E::RoundStart, E::ToolRequest { c: 1, d: true }]);
        let before = format!("{:?}", k.state());
        let _ = k.step(E::ToolExecStart { c: 1 }).unwrap_err();
        let _ = k.step(E::ToolAllow { c: 1 }).unwrap_err();
        let _ = k.step(E::RoundEnd).unwrap_err();
        assert_eq!(before, format!("{:?}", k.state()));
    }

    #[test]
    fn s10_task_events_bind_identity() {
        let mut k = kernel();
        drive(&mut k, &[E::TurnStart, E::RoundStart, E::AgentSpawn { a: 1, r: 2 }]);
        assert_eq!(k.step(E::AgentComplete { a: 1, r: 1 }).unwrap_err().guard, "r = taskRid[a]");
        assert_eq!(k.step(E::TaskWait { a: 1, r: 1 }).unwrap_err().guard, "r = taskRid[a]");
        drive(&mut k, &[E::TaskWait { a: 1, r: 2 }, E::AgentComplete { a: 1, r: 2 }]);
        assert_eq!(k.step(E::TaskWaitDeliver { a: 1, r: 1 }).unwrap_err().guard, "r = taskRid[a]");
        drive(&mut k, &[E::TaskWaitDeliver { a: 1, r: 2 }]);
    }

    #[test]
    fn s10_new_spawn_rotates_identity() {
        let mut k = kernel();
        drive(
            &mut k,
            &[
                E::TurnStart,
                E::RoundStart,
                E::AgentSpawn { a: 1, r: 2 },
                E::AgentComplete { a: 1, r: 2 },
                E::TaskWait { a: 1, r: 2 },
                E::TaskWaitDeliver { a: 1, r: 2 },
            ],
        );
        assert_eq!(k.step(E::AgentSpawn { a: 1, r: 2 }).unwrap_err().guard, "r # taskRid[a]");
        assert_eq!(k.step(E::AgentSpawn { a: 2, r: 1 }).unwrap_err().guard, "r # taskRid[a]");
        drive(&mut k, &[E::AgentSpawn { a: 1, r: 1 }]);
        assert_eq!(k.step(E::AgentComplete { a: 1, r: 2 }).unwrap_err().guard, "r = taskRid[a]");
        drive(&mut k, &[E::AgentComplete { a: 1, r: 1 }, E::RoundEnd]);
        assert_eq!(k.step(E::AgentFold { a: 1, r: 2 }).unwrap_err().guard, "r = taskRid[a]");
        drive(&mut k, &[E::AgentFold { a: 1, r: 1 }, E::RoundStart, E::RoundEnd, E::TurnEnd]);
    }

    #[test]
    fn s10_tid_out_of_scope_refused() {
        let mut k = kernel();
        drive(&mut k, &[E::TurnStart, E::RoundStart]);
        assert_eq!(k.step(E::AgentSpawn { a: 1, r: 0 }).unwrap_err().guard, "r \\in TaskIds");
        assert_eq!(k.step(E::AgentSpawn { a: 1, r: 3 }).unwrap_err().guard, "r \\in TaskIds");
    }

    #[test]
    fn slot_out_of_scope_refused() {
        let mut k = kernel();
        drive(&mut k, &[E::TurnStart, E::RoundStart]);
        assert!(k.step(E::ToolRequest { c: 3, d: false }).is_err());
        assert!(k.step(E::ToolRequest { c: 0, d: false }).is_err());
        assert!(k.step(E::AgentSpawn { a: 9, r: 2 }).is_err());
    }
}
