//! Mework agent kernel state machine: an implementation projection of
//! `formal/tla/AgentKernel.tla`.
//!
//! The specification is authoritative: each guard and state update in this
//! crate mirrors a same-named TLA+ action. Events rejected by `step()` are
//! transitions the model disallows. Resolve any implementation-model divergence
//! in the implementation; model changes require a safety decision (see
//! docs/formal-methods.md).
//!
//! `npm run verify:formal` enforces conformance through:
//! * An event-vocabulary guard that compares [`machine::KernelEvent`] variant
//!   names with TLA+ action names.
//! * Trace replay: event sequences exported by tests, including ProB-oracle
//!   random walks, must replay under `probcli -trace_replay json`; events the
//!   kernel rejects must also be rejected by the model.
//!
//! The model's `turnLegal`/`callLegal`/`agentLegal` guard-audit variables and
//! `callGrant` source-of-permission variable are mutation-audit ghost state,
//! not observable state. Rust represents them in `step()` guards rather than
//! fields.

pub mod machine;
pub mod trace;

pub use machine::{
    AgentPhase, CallPhase, Kernel, KernelConfig, KernelEvent, KernelRefusal, KernelState,
    RoundPhase, SlotId, TaskId, TurnPhase,
};
pub use trace::TraceRecorder;
