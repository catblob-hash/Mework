//! Conformance bridge: exports kernel accept/refuse behavior as ProB-replayable traces.
//!
//! When `AGENT_KERNEL_TRACE_DIR` is set, `scripts/formal-verify.mjs` replays each
//! accepted trace and rejects each trace whose final event the kernel refused.
//! Tests still validate kernel behavior without writing files when it is unset.
//!
//! The fixed-seed xorshift walk chooses uniformly among accepted candidates and
//! periodically records refused probes, exposing both overly broad and overly
//! narrow kernel behavior during replay.

use std::env;
use std::fs;
use std::path::PathBuf;

use agent_kernel::{Kernel, KernelConfig, KernelEvent, KernelRefusal, TraceRecorder};

struct Exporter {
    dir: Option<PathBuf>,
}

impl Exporter {
    fn from_env() -> Self {
        let dir = env::var("AGENT_KERNEL_TRACE_DIR").ok().map(PathBuf::from);
        if let Some(dir) = &dir {
            fs::create_dir_all(dir).expect("创建 trace 导出目录失败");
        }
        Exporter { dir }
    }

    fn write(&self, name: &str, value: &serde_json::Value) {
        if let Some(dir) = &self.dir {
            let path = dir.join(name);
            fs::write(&path, serde_json::to_string_pretty(value).expect("序列化 trace"))
                .unwrap_or_else(|error| panic!("写 {} 失败:{error}", path.display()));
        }
    }
}

/// Kernel plus recorder: only accepted events enter the trace.
struct Driven {
    kernel: Kernel,
    recorder: TraceRecorder,
}

impl Driven {
    fn new() -> Self {
        Driven { kernel: Kernel::new(KernelConfig::MODEL_SCOPE), recorder: TraceRecorder::new() }
    }

    fn apply(&mut self, event: KernelEvent) -> Result<(), KernelRefusal> {
        self.kernel.step(event)?;
        self.recorder.record(event);
        Ok(())
    }

    /// Appends one refused event to the exported JSON as a refusal probe.
    fn probe_json(&self, refused: KernelEvent) -> serde_json::Value {
        let mut probe = self.recorder.clone();
        probe.record(refused);
        probe.to_prob_json()
    }
}

fn drive(driven: &mut Driven, events: &[KernelEvent]) {
    for event in events {
        driven.apply(*event).unwrap_or_else(|refusal| panic!("{refusal:?}"));
    }
}

// ---------------------------------------------------------------------------
// Deterministic scenario traces for documented safety boundaries.
// ---------------------------------------------------------------------------

fn assert_refused_unchanged(
    t: &mut Driven,
    exporter: &Exporter,
    name: &str,
    event: KernelEvent,
    guard: &str,
) {
    let before = serde_json::to_value(t.kernel.state()).unwrap();
    let refusal = t.kernel.step(event).expect_err("the protocol must reject this event");
    assert_eq!(refusal.guard, guard);
    assert_eq!(serde_json::to_value(t.kernel.state()).unwrap(), before);
    exporter.write(name, &t.probe_json(event));
}

#[test]
fn wake_first_round_requires_the_idle_result() {
    use KernelEvent as E;
    let exporter = Exporter::from_env();
    let mut t = Driven::new();
    drive(&mut t, &[
        E::TurnStart, E::RoundStart, E::AgentSpawn { a: 1, r: 2 },
        E::RoundEnd, E::RoundStart, E::RoundEnd, E::TurnEnd,
        E::AgentComplete { a: 1, r: 2 }, E::TurnStart,
    ]);
    assert_refused_unchanged(&mut t, &exporter, "wake-first-round.refused.prob2trace",
        E::RoundStart, "wakeFoldPending = {}");
    drive(&mut t, &[
        E::AgentFold { a: 1, r: 2 }, E::RoundStart, E::RoundEnd, E::TurnEnd,
    ]);
    exporter.write("wake-first-round.prob2trace", &t.recorder.to_prob_json());
}

#[test]
fn wake_first_round_requires_all_results_even_after_cancel_or_stale_fold() {
    use KernelEvent as E;
    let exporter = Exporter::from_env();
    let mut t = Driven::new();
    // Complete before the second spawn, respecting the one-live-task scope.
    drive(&mut t, &[
        E::TurnStart, E::RoundStart, E::AgentSpawn { a: 1, r: 2 },
        E::AgentComplete { a: 1, r: 2 }, E::AgentSpawn { a: 2, r: 2 },
        E::AgentComplete { a: 2, r: 2 }, E::TurnCancel, E::RoundEnd,
        E::TurnEnd, E::TurnStart,
    ]);
    let mut all = Driven { kernel: t.kernel.clone(), recorder: t.recorder.clone() };
    drive(&mut all, &[
        E::AgentFold { a: 2, r: 2 }, E::AgentFold { a: 1, r: 2 },
        E::RoundStart, E::RoundEnd, E::TurnEnd,
    ]);
    exporter.write("wake-two-folds.prob2trace", &all.recorder.to_prob_json());
    assert_refused_unchanged(&mut t, &exporter, "wake-stale-fold.refused.prob2trace",
        E::AgentFold { a: 1, r: 1 }, "r = taskRid[a]");
    assert_refused_unchanged(&mut t, &exporter, "wake-after-stale-fold.refused.prob2trace",
        E::RoundStart, "wakeFoldPending = {}");
    drive(&mut t, &[E::AgentFold { a: 1, r: 2 }]);
    assert_refused_unchanged(&mut t, &exporter, "wake-partial-fold.refused.prob2trace",
        E::RoundStart, "wakeFoldPending = {}");
    drive(&mut t, &[E::TurnCancel, E::TurnEnd, E::TurnStart]);
    assert_refused_unchanged(&mut t, &exporter, "wake-cancel-retry.refused.prob2trace",
        E::RoundStart, "wakeFoldPending = {}");
    drive(&mut t, &[
        E::AgentFold { a: 2, r: 2 }, E::RoundStart, E::RoundEnd, E::TurnEnd,
    ]);
    exporter.write("wake-two-cancel-retry.prob2trace", &t.recorder.to_prob_json());
}

#[test]
fn completion_after_turn_start_does_not_block_the_first_round() {
    use KernelEvent as E;
    let exporter = Exporter::from_env();
    let mut t = Driven::new();
    drive(&mut t, &[
        E::TurnStart, E::RoundStart, E::AgentSpawn { a: 1, r: 2 },
        E::RoundEnd, E::RoundStart, E::RoundEnd, E::TurnEnd, E::TurnStart,
        E::AgentComplete { a: 1, r: 2 }, E::RoundStart, E::TaskWait { a: 1, r: 2 },
        E::TaskWaitDeliver { a: 1, r: 2 }, E::RoundEnd, E::RoundStart,
        E::RoundEnd, E::TurnEnd,
    ]);
    exporter.write("wake-late-complete.prob2trace", &t.recorder.to_prob_json());
}

#[test]
fn scenario_traces() {
    use KernelEvent as E;
    let exporter = Exporter::from_env();

    // S1/S2/S7/S9: dangerous-call approval, execution, receipt, and archival;
    // the receipt is delivered in the final round.
    let mut t = Driven::new();
    drive(
        &mut t,
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
    exporter.write("scenario-approve.prob2trace", &t.recorder.to_prob_json());

    // S3: tightening policy after approval requires reapproval before execution.
    let mut t = Driven::new();
    drive(
        &mut t,
        &[
            E::TurnStart,
            E::RoundStart,
            E::ToolRequest { c: 1, d: true },
            E::ToolApprove { c: 1 },
            E::PolicyTighten,
        ],
    );
    let stale = t.kernel.step(E::ToolExecStart { c: 1 }).unwrap_err();
    assert_eq!(stale.guard, "callFresh[c]");
    exporter.write("scenario-tighten-probe.refused.prob2trace", &t.probe_json(E::ToolExecStart { c: 1 }));
    exporter.write("scenario-tighten-prefix.prob2trace", &t.recorder.to_prob_json());

    // S3: reapproval after tightening must appear in an accepted trace.
    let mut t = Driven::new();
    drive(
        &mut t,
        &[
            E::TurnStart,
            E::RoundStart,
            E::ToolRequest { c: 1, d: true },
            E::ToolApprove { c: 1 },
            E::PolicyTighten,
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
    exporter.write("scenario-reapprove.prob2trace", &t.recorder.to_prob_json());

    // S4/S5: cancellation settles in-flight execution, discards unresolved
    // calls, and abandons delivery obligations. A task settling during
    // cancellation is retained for a later fold.
    let mut t = Driven::new();
    drive(
        &mut t,
        &[
            E::TurnStart,
            E::RoundStart,
            E::ToolRequest { c: 1, d: false },
            E::ToolAllow { c: 1 },
            E::ToolExecStart { c: 1 },
            E::ToolRequest { c: 2, d: true },
            E::AgentSpawn { a: 1, r: 2 },
            E::TurnCancel,
            E::ToolExecEnd { c: 1 },
            E::ToolSettle { c: 1 },
            E::ToolSettle { c: 2 },
            E::AgentComplete { a: 1, r: 2 },
            E::RoundEnd,
            E::TurnEnd,
        ],
    );
    exporter.write("scenario-cancel.prob2trace", &t.recorder.to_prob_json());

    // S6: sequential capacity reuse with two slots and one concurrent task;
    // waiting observes each slot's terminal result.
    let mut t = Driven::new();
    drive(
        &mut t,
        &[
            E::TurnStart,
            E::RoundStart,
            E::AgentSpawn { a: 1, r: 2 },
            E::AgentComplete { a: 1, r: 2 },
            E::TaskWait { a: 1, r: 2 },
            E::TaskWaitDeliver { a: 1, r: 2 },
            E::AgentSpawn { a: 2, r: 2 },
            E::AgentComplete { a: 2, r: 2 },
            E::TaskWait { a: 2, r: 2 },
            E::TaskWaitDeliver { a: 2, r: 2 },
            E::RoundEnd,
            E::RoundStart,
            E::RoundEnd,
            E::TurnEnd,
        ],
    );
    exporter.write("scenario-capacity.prob2trace", &t.recorder.to_prob_json());

    // S8'/S9: context edits occur between turns. Waiting blocks the round until
    // delivery; an uncollected result folds during preparation and starts a new round.
    let mut t = Driven::new();
    drive(
        &mut t,
        &[
            E::ContextEdit,
            E::TurnStart,
            E::RoundStart,
            E::AgentSpawn { a: 1, r: 2 },
            E::ToolRequest { c: 1, d: false },
            E::ToolAllow { c: 1 },
            E::ToolExecStart { c: 1 },
            E::TaskWait { a: 1, r: 2 },
            E::ToolExecEnd { c: 1 },
            E::AgentComplete { a: 1, r: 2 },
            E::TaskWaitDeliver { a: 1, r: 2 },
            E::ToolSettle { c: 1 },
            E::AgentSpawn { a: 2, r: 2 },
            E::RoundEnd,
            E::AgentComplete { a: 2, r: 2 },
            E::AgentFold { a: 2, r: 2 },
            E::RoundStart,
            E::RoundEnd,
            E::TurnEnd,
            E::ContextEdit,
        ],
    );
    exporter.write("scenario-taskwait.prob2trace", &t.recorder.to_prob_json());

    // S8' refusal probes: a pending wait blocks round closure, delivery requires
    // settlement, and each task permits at most one pending wait.
    let mut t = Driven::new();
    drive(&mut t, &[E::TurnStart, E::RoundStart, E::AgentSpawn { a: 1, r: 2 }, E::TaskWait { a: 1, r: 2 }]);
    let blocked = t.kernel.step(E::RoundEnd).unwrap_err();
    assert_eq!(blocked.guard, "\\A a \\in Agents : ~agentWaited[a]");
    exporter.write("scenario-wait-blocks-round.refused.prob2trace", &t.probe_json(E::RoundEnd));
    let unsettled = t.kernel.step(E::TaskWaitDeliver { a: 1, r: 2 }).unwrap_err();
    assert_eq!(unsettled.guard, "agentPhase[a] = \"done\"");
    exporter.write(
        "scenario-deliver-unsettled.refused.prob2trace",
        &t.probe_json(E::TaskWaitDeliver { a: 1, r: 2 }),
    );
    let double = t.kernel.step(E::TaskWait { a: 1, r: 2 }).unwrap_err();
    assert_eq!(double.guard, "~agentWaited[a]");
    exporter.write("scenario-double-wait.refused.prob2trace", &t.probe_json(E::TaskWait { a: 1, r: 2 }));

    // S8' refusal probes: delivery requires an initiated wait, and waiting
    // requires an existing task.
    let mut t = Driven::new();
    drive(&mut t, &[E::TurnStart, E::RoundStart, E::AgentSpawn { a: 1, r: 2 }, E::AgentComplete { a: 1, r: 2 }]);
    let no_wait = t.kernel.step(E::TaskWaitDeliver { a: 1, r: 2 }).unwrap_err();
    assert_eq!(no_wait.guard, "agentWaited[a]");
    exporter.write("scenario-deliver-no-wait.refused.prob2trace", &t.probe_json(E::TaskWaitDeliver { a: 1, r: 2 }));
    let no_task = t.kernel.step(E::TaskWait { a: 2, r: 2 }).unwrap_err();
    assert_eq!(no_task.guard, "agentPhase[a] # \"none\"");
    exporter.write("scenario-wait-no-task.refused.prob2trace", &t.probe_json(E::TaskWait { a: 2, r: 2 }));

    // S8': a wait timeout leaves the task running; its timeout receipt is
    // delivered next round. A settled wait cannot time out.
    let mut t = Driven::new();
    drive(
        &mut t,
        &[
            E::TurnStart,
            E::RoundStart,
            E::AgentSpawn { a: 1, r: 2 },
            E::TaskWait { a: 1, r: 2 },
            E::TaskWaitTimeout { a: 1, r: 2 },
            E::TaskWait { a: 1, r: 2 },
            E::AgentComplete { a: 1, r: 2 },
        ],
    );
    let timeout_settled = t.kernel.step(E::TaskWaitTimeout { a: 1, r: 2 }).unwrap_err();
    assert_eq!(timeout_settled.guard, "agentPhase[a] = \"running\"");
    exporter.write(
        "scenario-timeout-settled.refused.prob2trace",
        &t.probe_json(E::TaskWaitTimeout { a: 1, r: 2 }),
    );
    drive(
        &mut t,
        &[
            E::TaskWaitDeliver { a: 1, r: 2 },
            E::RoundEnd,
            E::RoundStart,
            E::RoundEnd,
            E::TurnEnd,
        ],
    );
    exporter.write("scenario-wait-timeout.prob2trace", &t.recorder.to_prob_json());

    // S9: steer input queues within a round, joins during preparation, then is
    // consumed by the following round.
    let mut t = Driven::new();
    drive(
        &mut t,
        &[
            E::TurnStart,
            E::RoundStart,
            E::SteerEnqueue,
            E::RoundEnd,
            E::SteerJoin,
            E::RoundStart,
            E::RoundEnd,
            E::TurnEnd,
        ],
    );
    exporter.write("scenario-steer.prob2trace", &t.recorder.to_prob_json());

    // S9: cancellation preserves pending steer input for the next turn.
    let mut t = Driven::new();
    drive(
        &mut t,
        &[
            E::TurnStart,
            E::RoundStart,
            E::SteerEnqueue,
            E::TurnCancel,
            E::RoundEnd,
            E::TurnEnd,
            E::TurnStart,
            E::SteerJoin,
            E::RoundStart,
            E::RoundEnd,
            E::TurnEnd,
        ],
    );
    exporter.write("scenario-steer-carryover.prob2trace", &t.recorder.to_prob_json());

    // S9: a user-stopped task still yields a terminal result, so its fold
    // charges the delivery obligation and forces another round.
    let mut t = Driven::new();
    drive(
        &mut t,
        &[
            E::TurnStart,
            E::RoundStart,
            E::AgentSpawn { a: 1, r: 2 },
            E::RoundEnd,
            E::RoundStart,
            E::RoundEnd,
            E::AgentComplete { a: 1, r: 2 },
            E::AgentFold { a: 1, r: 2 },
            E::RoundStart,
            E::RoundEnd,
            E::TurnEnd,
        ],
    );
    exporter.write("scenario-stop-retriggers.prob2trace", &t.recorder.to_prob_json());

    // S11: a running task survives normal turn completion. A completed idle task
    // wakes a host-started round and must not fold while idle.
    let mut t = Driven::new();
    drive(
        &mut t,
        &[
            E::TurnStart,
            E::RoundStart,
            E::AgentSpawn { a: 1, r: 2 },
            E::RoundEnd,
            E::RoundStart,
            E::RoundEnd,
            E::TurnEnd,
            E::AgentComplete { a: 1, r: 2 },
        ],
    );
    let idle_fold = t.kernel.step(E::AgentFold { a: 1, r: 2 }).unwrap_err();
    assert_eq!(idle_fold.guard, "turn = \"running\"");
    exporter.write("scenario-idle-fold.refused.prob2trace", &t.probe_json(E::AgentFold { a: 1, r: 2 }));
    drive(&mut t, &[E::TurnStart, E::AgentFold { a: 1, r: 2 }, E::RoundStart, E::RoundEnd, E::TurnEnd]);
    exporter.write("scenario-survive-wake.prob2trace", &t.recorder.to_prob_json());

    // S11 refusal probe: normal closure requires every completed task to have
    // been folded for in-turn delivery.
    let mut t = Driven::new();
    drive(
        &mut t,
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
    let end_pending_done = t.kernel.step(E::TurnEnd).unwrap_err();
    assert_eq!(
        end_pending_done.guard,
        "\\A a \\in Agents : agentPhase[a] \\in {\"none\",\"running\"} \\/ (turn = \"cancelling\" /\\ agentPhase[a] = \"done\")"
    );
    exporter.write("scenario-end-pending-done.refused.prob2trace", &t.probe_json(E::TurnEnd));

    // S11: cancellation preserves completed tasks for next-turn folding; the
    // cancelling turn itself may not fold them.
    let mut t = Driven::new();
    drive(
        &mut t,
        &[
            E::TurnStart,
            E::RoundStart,
            E::AgentSpawn { a: 1, r: 2 },
            E::AgentComplete { a: 1, r: 2 },
            E::TurnCancel,
            E::RoundEnd,
        ],
    );
    let cancel_fold_done = t.kernel.step(E::AgentFold { a: 1, r: 2 }).unwrap_err();
    assert_eq!(cancel_fold_done.guard, "turn = \"running\"");
    exporter.write(
        "scenario-cancel-fold-done.refused.prob2trace",
        &t.probe_json(E::AgentFold { a: 1, r: 2 }),
    );
    drive(
        &mut t,
        &[E::TurnEnd, E::TurnStart, E::AgentFold { a: 1, r: 2 }, E::RoundStart, E::RoundEnd, E::TurnEnd],
    );
    exporter.write("scenario-cancel-done-carryover.prob2trace", &t.recorder.to_prob_json());

    // S11: stopping an idle task produces a deliverable result that waits for the
    // host's wake turn; task identity remains bound while the task is idle.
    let mut t = Driven::new();
    drive(
        &mut t,
        &[
            E::TurnStart,
            E::RoundStart,
            E::AgentSpawn { a: 1, r: 2 },
            E::RoundEnd,
            E::RoundStart,
            E::RoundEnd,
            E::TurnEnd,
        ],
    );
    let tid_idle = t.kernel.step(E::AgentComplete { a: 1, r: 1 }).unwrap_err();
    assert_eq!(tid_idle.guard, "r = taskRid[a]");
    exporter.write(
        "scenario-tid-idle-complete.refused.prob2trace",
        &t.probe_json(E::AgentComplete { a: 1, r: 1 }),
    );
    drive(
        &mut t,
        &[
            E::AgentComplete { a: 1, r: 2 },
            E::TurnStart,
            E::AgentFold { a: 1, r: 2 },
            E::RoundStart,
            E::RoundEnd,
            E::TurnEnd,
        ],
    );
    exporter.write("scenario-idle-stop-wakes.prob2trace", &t.recorder.to_prob_json());

    // S11: the next user turn may adopt a surviving task, wait for it, and deliver it.
    let mut t = Driven::new();
    drive(
        &mut t,
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
    exporter.write("scenario-adopt.prob2trace", &t.recorder.to_prob_json());

    // S9 refusal probe: no delivery obligation means no empty round.
    let mut t = Driven::new();
    drive(&mut t, &[E::TurnStart, E::RoundStart, E::RoundEnd]);
    let idle_round = t.kernel.step(E::RoundStart).unwrap_err();
    assert_eq!(idle_round.guard, "roundDue");
    exporter.write("scenario-idle-round.refused.prob2trace", &t.probe_json(E::RoundStart));

    // S9 refusal probe: folding creates a delivery obligation that must run
    // before turn closure.
    let mut t = Driven::new();
    drive(
        &mut t,
        &[
            E::TurnStart,
            E::RoundStart,
            E::AgentSpawn { a: 1, r: 2 },
            E::RoundEnd,
            E::RoundStart,
            E::RoundEnd,
            E::AgentComplete { a: 1, r: 2 },
            E::AgentFold { a: 1, r: 2 },
        ],
    );
    let fold_due = t.kernel.step(E::TurnEnd).unwrap_err();
    assert_eq!(fold_due.guard, "turn = \"cancelling\" \\/ (~roundDue /\\ ~steerPending)");
    exporter.write("scenario-fold-forces-round.refused.prob2trace", &t.probe_json(E::TurnEnd));

    // S8: contexts are immutable while a turn runs. Cancellation clears a
    // pending wait, which cannot subsequently start or deliver.
    let mut t = Driven::new();
    drive(
        &mut t,
        &[E::TurnStart, E::RoundStart, E::AgentSpawn { a: 1, r: 2 }, E::AgentComplete { a: 1, r: 2 }],
    );
    let frozen = t.kernel.step(E::ContextEdit).unwrap_err();
    assert_eq!(frozen.guard, "turn = \"idle\"");
    exporter.write("scenario-edit-frozen.refused.prob2trace", &t.probe_json(E::ContextEdit));
    drive(&mut t, &[E::TaskWait { a: 1, r: 2 }, E::TurnCancel]);
    let late_deliver = t.kernel.step(E::TaskWaitDeliver { a: 1, r: 2 }).unwrap_err();
    assert_eq!(late_deliver.guard, "agentWaited[a]");
    exporter.write("scenario-late-deliver.refused.prob2trace", &t.probe_json(E::TaskWaitDeliver { a: 1, r: 2 }));
    let late_wait = t.kernel.step(E::TaskWait { a: 1, r: 2 }).unwrap_err();
    assert_eq!(late_wait.guard, "turn = \"running\"");
    exporter.write("scenario-late-wait.refused.prob2trace", &t.probe_json(E::TaskWait { a: 1, r: 2 }));

    // S10: task identity rotates when a slot is reused. Repeated identities and
    // late completion from an old generation are rejected.
    let mut t = Driven::new();
    drive(
        &mut t,
        &[
            E::TurnStart,
            E::RoundStart,
            E::AgentSpawn { a: 1, r: 2 },
            E::AgentComplete { a: 1, r: 2 },
            E::TaskWait { a: 1, r: 2 },
            E::TaskWaitDeliver { a: 1, r: 2 },
        ],
    );
    let repeat = t.kernel.step(E::AgentSpawn { a: 1, r: 2 }).unwrap_err();
    assert_eq!(repeat.guard, "r # taskRid[a]");
    exporter.write("scenario-tid-repeat.refused.prob2trace", &t.probe_json(E::AgentSpawn { a: 1, r: 2 }));
    drive(&mut t, &[E::AgentSpawn { a: 1, r: 1 }]);
    let stale = t.kernel.step(E::AgentComplete { a: 1, r: 2 }).unwrap_err();
    assert_eq!(stale.guard, "r = taskRid[a]");
    exporter.write(
        "scenario-tid-stale-complete.refused.prob2trace",
        &t.probe_json(E::AgentComplete { a: 1, r: 2 }),
    );
    drive(
        &mut t,
        &[
            E::AgentComplete { a: 1, r: 1 },
            E::TaskWait { a: 1, r: 1 },
            E::TaskWaitDeliver { a: 1, r: 1 },
            E::RoundEnd,
            E::RoundStart,
            E::RoundEnd,
            E::TurnEnd,
        ],
    );
    exporter.write("scenario-tid-rotate.prob2trace", &t.recorder.to_prob_json());

    // S12: an executing call whose deadline expires hands its work to a free
    // task slot and still settles normally. What it left behind is an ordinary
    // task: it completes later and folds in prep.
    let mut t = Driven::new();
    drive(
        &mut t,
        &[
            E::TurnStart,
            E::RoundStart,
            E::ToolRequest { c: 1, d: false },
            E::ToolAllow { c: 1 },
            E::ToolExecStart { c: 1 },
            E::ToolBackground { c: 1, a: 1, r: 2 },
            E::ToolExecEnd { c: 1 },
            E::ToolSettle { c: 1 },
            E::RoundEnd,
            E::AgentComplete { a: 1, r: 2 },
            E::AgentFold { a: 1, r: 2 },
            E::RoundStart,
            E::RoundEnd,
            E::TurnEnd,
        ],
    );
    exporter.write("scenario-bg-timeout.prob2trace", &t.recorder.to_prob_json());

    // S12: the handed-off task is addressable as soon as the call settles, so
    // the same round may wait on it and take its result in-round.
    let mut t = Driven::new();
    drive(
        &mut t,
        &[
            E::TurnStart,
            E::RoundStart,
            E::ToolRequest { c: 1, d: false },
            E::ToolAllow { c: 1 },
            E::ToolExecStart { c: 1 },
            E::ToolBackground { c: 1, a: 1, r: 2 },
            E::ToolExecEnd { c: 1 },
            E::ToolSettle { c: 1 },
            E::TaskWait { a: 1, r: 2 },
            E::AgentComplete { a: 1, r: 2 },
            E::TaskWaitDeliver { a: 1, r: 2 },
            E::RoundEnd,
            E::RoundStart,
            E::RoundEnd,
            E::TurnEnd,
        ],
    );
    exporter.write("scenario-bg-wait.prob2trace", &t.recorder.to_prob_json());

    // S12 refusal probes: only an executing call may hand off, and only once.
    // An authorized-but-not-started call has nothing running to give away.
    let mut t = Driven::new();
    drive(
        &mut t,
        &[
            E::TurnStart,
            E::RoundStart,
            E::ToolRequest { c: 1, d: false },
            E::ToolAllow { c: 1 },
        ],
    );
    let early = t.kernel.step(E::ToolBackground { c: 1, a: 1, r: 2 }).unwrap_err();
    assert_eq!(early.guard, "callPhase[c] = \"executing\"");
    exporter.write(
        "scenario-bg-before-exec.refused.prob2trace",
        &t.probe_json(E::ToolBackground { c: 1, a: 1, r: 2 }),
    );

    // A call that already produced its receipt is no longer running anything.
    drive(&mut t, &[E::ToolExecStart { c: 1 }, E::ToolExecEnd { c: 1 }]);
    let late = t.kernel.step(E::ToolBackground { c: 1, a: 1, r: 2 }).unwrap_err();
    assert_eq!(late.guard, "callPhase[c] = \"executing\"");
    exporter.write(
        "scenario-bg-after-end.refused.prob2trace",
        &t.probe_json(E::ToolBackground { c: 1, a: 1, r: 2 }),
    );

    // One call, one task: `ExecBg` is executing minus the right to hand off
    // again. The first task is completed first so capacity is not the refuser.
    let mut t = Driven::new();
    drive(
        &mut t,
        &[
            E::TurnStart,
            E::RoundStart,
            E::ToolRequest { c: 1, d: false },
            E::ToolAllow { c: 1 },
            E::ToolExecStart { c: 1 },
            E::ToolBackground { c: 1, a: 1, r: 2 },
            E::AgentComplete { a: 1, r: 2 },
        ],
    );
    let twice = t.kernel.step(E::ToolBackground { c: 1, a: 2, r: 2 }).unwrap_err();
    assert_eq!(twice.guard, "callPhase[c] = \"executing\"");
    exporter.write(
        "scenario-bg-twice.refused.prob2trace",
        &t.probe_json(E::ToolBackground { c: 1, a: 2, r: 2 }),
    );

    // S12 x S10: a handoff mints the same fresh identity a spawn would.
    let mut t = Driven::new();
    drive(
        &mut t,
        &[
            E::TurnStart,
            E::RoundStart,
            E::ToolRequest { c: 1, d: false },
            E::ToolAllow { c: 1 },
            E::ToolExecStart { c: 1 },
        ],
    );
    let stale_tid = t.kernel.step(E::ToolBackground { c: 1, a: 1, r: 1 }).unwrap_err();
    assert_eq!(stale_tid.guard, "r # taskRid[a]");
    exporter.write(
        "scenario-bg-stale-tid.refused.prob2trace",
        &t.probe_json(E::ToolBackground { c: 1, a: 1, r: 1 }),
    );

    // S12 x S9: a slot still holding an unclaimed result is not free to take one.
    let mut t = Driven::new();
    drive(
        &mut t,
        &[
            E::TurnStart,
            E::RoundStart,
            E::AgentSpawn { a: 1, r: 2 },
            E::AgentComplete { a: 1, r: 2 },
            E::ToolRequest { c: 1, d: false },
            E::ToolAllow { c: 1 },
            E::ToolExecStart { c: 1 },
        ],
    );
    let occupied = t.kernel.step(E::ToolBackground { c: 1, a: 1, r: 1 }).unwrap_err();
    assert_eq!(occupied.guard, "agentPhase[a] = \"none\"");
    exporter.write(
        "scenario-bg-slot-done.refused.prob2trace",
        &t.probe_json(E::ToolBackground { c: 1, a: 1, r: 1 }),
    );

    // S12 x S6: a handoff is counted by the same concurrency cap as a spawn.
    let mut t = Driven::new();
    drive(
        &mut t,
        &[
            E::TurnStart,
            E::RoundStart,
            E::AgentSpawn { a: 1, r: 2 },
            E::ToolRequest { c: 1, d: false },
            E::ToolAllow { c: 1 },
            E::ToolExecStart { c: 1 },
        ],
    );
    let overcap = t.kernel.step(E::ToolBackground { c: 1, a: 2, r: 2 }).unwrap_err();
    assert_eq!(overcap.guard, "Cardinality(RunningAgents) < MaxLiveAgents");
    exporter.write(
        "scenario-bg-overcap.refused.prob2trace",
        &t.probe_json(E::ToolBackground { c: 1, a: 2, r: 2 }),
    );
}

// ---------------------------------------------------------------------------
// Fixed-seed random walk; ProB is the oracle.
// ---------------------------------------------------------------------------

fn xorshift(state: &mut u64) -> u64 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *state = x;
    x
}

/// Every candidate event and parameter combination under `MODEL_SCOPE`.
fn candidates() -> Vec<KernelEvent> {
    use KernelEvent as E;
    let mut all = vec![
        E::TurnStart,
        E::TurnCancel,
        E::TurnEnd,
        E::ContextEdit,
        E::RoundStart,
        E::RoundEnd,
        E::SteerEnqueue,
        E::SteerJoin,
        E::PolicyTighten,
    ];
    for c in 1..=KernelConfig::MODEL_SCOPE.call_slots {
        all.push(E::ToolRequest { c, d: false });
        all.push(E::ToolAllow { c });
        all.push(E::ToolApprove { c });
        all.push(E::ToolDeny { c });
        all.push(E::ToolExecStart { c });
        all.push(E::ToolExecEnd { c });
        all.push(E::ToolSettle { c });
    }
    for a in 1..=KernelConfig::MODEL_SCOPE.agent_slots {
        for r in 1..=KernelConfig::MODEL_SCOPE.task_ids {
            all.push(E::AgentSpawn { a, r });
            all.push(E::AgentComplete { a, r });
            all.push(E::AgentFold { a, r });
            all.push(E::TaskWait { a, r });
            all.push(E::TaskWaitTimeout { a, r });
            all.push(E::TaskWaitDeliver { a, r });
        }
    }
    // The only event naming both surfaces, so it needs the full cross product.
    for c in 1..=KernelConfig::MODEL_SCOPE.call_slots {
        for a in 1..=KernelConfig::MODEL_SCOPE.agent_slots {
            for r in 1..=KernelConfig::MODEL_SCOPE.task_ids {
                all.push(E::ToolBackground { c, a, r });
            }
        }
    }
    // Place dangerous requests last. Otherwise cyclic first-accepted selection
    // systematically masks the dangerous pipeline behind the same-slot safe request.
    for c in 1..=KernelConfig::MODEL_SCOPE.call_slots {
        all.push(E::ToolRequest { c, d: true });
    }
    all
}

#[test]
fn random_walk_traces() {
    const SEEDS: [u64; 6] = [
        0x4d65_776f_726b_0001,
        0x4d65_776f_726b_0002,
        0x4d65_776f_726b_0003,
        0x4d65_776f_726b_0004,
        0x4d65_776f_726b_0005,
        0x4d65_776f_726b_0006,
    ];
    const STEPS: usize = 400;
    const PROBE_EVERY: usize = 50;

    let exporter = Exporter::from_env();
    let pool = candidates();

    // Every event name must be accepted and refused at least once across all
    // walks, except always-accepted `PolicyTighten`. Fixed seeds expose gaps
    // deterministically.
    let mut accepted_by_name: std::collections::HashMap<&'static str, usize> =
        std::collections::HashMap::new();
    let mut refused_by_name: std::collections::HashMap<&'static str, usize> =
        std::collections::HashMap::new();

    for (walk_index, seed) in SEEDS.iter().enumerate() {
        let mut rng = *seed;
        // Use an independent RNG stream for probes so probe cadence changes do
        // not perturb the walk or its coverage.
        let mut probe_rng = seed ^ 0x9e37_79b9_7f4a_7c15;
        let mut driven = Driven::new();
        let mut accepted = 0usize;
        let mut probes = 0usize;

        while accepted < STEPS {
            // Classify every candidate on a clone, then choose uniformly from
            // accepted events. First-accepted cyclic scans create ordering bias
            // that starves deep pipeline states.
            let mut enabled = Vec::new();
            for event in &pool {
                let mut scratch = driven.kernel.clone();
                match scratch.step(*event) {
                    Ok(()) => {
                        *accepted_by_name.entry(event.name()).or_default() += 1;
                        enabled.push(*event);
                    }
                    Err(_) => *refused_by_name.entry(event.name()).or_default() += 1,
                }
            }
            assert!(!enabled.is_empty(), "候选事件里必须始终存在可接受者(PolicyTighten 恒可用)");
            let mut chosen = enabled[(xorshift(&mut rng) as usize) % enabled.len()];
            // Redraw once after `TurnCancel`: it is enabled in every running
            // state and would otherwise shorten rounds enough to starve deep states.
            if matches!(chosen, KernelEvent::TurnCancel) {
                chosen = enabled[(xorshift(&mut rng) as usize) % enabled.len()];
            }
            driven.apply(chosen).expect("普查已证明该事件在当前状态可接受");
            accepted += 1;

            // Sample a refused event uniformly after advancing. Probe a clone so
            // an accepted trial cannot silently advance the recorded kernel state.
            if accepted % PROBE_EVERY == 0 {
                let refused_now: Vec<KernelEvent> = pool
                    .iter()
                    .copied()
                    .filter(|event| {
                        let mut scratch = driven.kernel.clone();
                        scratch.step(*event).is_err()
                    })
                    .collect();
                if !refused_now.is_empty() {
                    let probe = refused_now[(xorshift(&mut probe_rng) as usize) % refused_now.len()];
                    probes += 1;
                    exporter.write(
                        &format!("walk-{walk_index}-probe-{probes}.refused.prob2trace"),
                        &driven.probe_json(probe),
                    );
                }
            }
        }

        exporter.write(&format!("walk-{walk_index}.prob2trace"), &driven.recorder.to_prob_json());
        assert!(probes > 0, "每条随机游走都应采到拒绝探针");
    }

    for name in KernelEvent::NAMES {
        assert!(
            accepted_by_name.get(name).copied().unwrap_or(0) > 0,
            "事件 {name} 从未被随机游走接受——覆盖缺口"
        );
        if name != "PolicyTighten" {
            assert!(
                refused_by_name.get(name).copied().unwrap_or(0) > 0,
                "事件 {name} 从未被随机游走拒绝——覆盖缺口"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Alphabet export: the verification script compares it byte-for-byte with TLA+ actions.
// ---------------------------------------------------------------------------

#[test]
fn alphabet_export() {
    let exporter = Exporter::from_env();
    exporter.write("alphabet.json", &serde_json::json!({ "events": KernelEvent::NAMES }));
}
