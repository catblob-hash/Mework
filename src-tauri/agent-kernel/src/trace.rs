//! Records and exports event traces in the `probcli -trace_replay json` format.
//!
//! Exported JSON has this shape:
//! ```json
//! {"transitionList": [
//!   {"name": "$initialise_machine"},
//!   {"name": "TurnStart"},
//!   {"name": "ToolRequest", "params": {"c": "1", "d": "TRUE"}}
//! ]}
//! ```
//! The initial `$initialise_machine` maps to the B-side initialization transition.
//! Every parameter value is a string; Booleans use `"TRUE"`/`"FALSE"`, matching
//! ProB's B-value rendering.

use serde_json::{Map, Value, json};

use crate::machine::KernelEvent;

#[derive(Clone, Debug, Default)]
pub struct TraceRecorder {
    events: Vec<KernelEvent>,
}

impl TraceRecorder {
    pub fn new() -> Self {
        TraceRecorder::default()
    }

    /// Records only events accepted by the kernel: replay validates the claimed
    /// legal path transition by transition.
    pub fn record(&mut self, event: KernelEvent) {
        self.events.push(event);
    }

    pub fn events(&self) -> &[KernelEvent] {
        &self.events
    }

    pub fn to_prob_json(&self) -> Value {
        let mut transitions = Vec::with_capacity(self.events.len() + 1);
        transitions.push(json!({ "name": "$initialise_machine" }));
        for event in &self.events {
            transitions.push(transition_json(event));
        }
        json!({ "transitionList": transitions })
    }
}

fn bool_param(value: bool) -> Value {
    Value::String(if value { "TRUE" } else { "FALSE" }.to_string())
}

fn slot_param(value: usize) -> Value {
    Value::String(value.to_string())
}

fn transition_json(event: &KernelEvent) -> Value {
    use KernelEvent as E;
    let mut params = Map::new();
    match event {
        E::TurnStart
        | E::TurnCancel
        | E::TurnEnd
        | E::ContextEdit
        | E::RoundStart
        | E::RoundEnd
        | E::SteerEnqueue
        | E::SteerJoin
        | E::PolicyTighten => {}
        E::ToolRequest { c, d } => {
            params.insert("c".into(), slot_param(*c));
            params.insert("d".into(), bool_param(*d));
        }
        E::ToolAllow { c }
        | E::ToolApprove { c }
        | E::ToolDeny { c }
        | E::ToolExecStart { c }
        | E::ToolExecEnd { c }
        | E::ToolSettle { c } => {
            params.insert("c".into(), slot_param(*c));
        }
        E::AgentSpawn { a, r }
        | E::AgentComplete { a, r }
        | E::AgentFold { a, r }
        | E::TaskWait { a, r }
        | E::TaskWaitTimeout { a, r }
        | E::TaskWaitDeliver { a, r } => {
            params.insert("a".into(), slot_param(*a));
            params.insert("r".into(), slot_param(*r));
        }
        // The one event that names a call slot and a task slot at once.
        E::ToolBackground { c, a, r } => {
            params.insert("c".into(), slot_param(*c));
            params.insert("a".into(), slot_param(*a));
            params.insert("r".into(), slot_param(*r));
        }
    }
    if params.is_empty() {
        json!({ "name": event.name() })
    } else {
        json!({ "name": event.name(), "params": Value::Object(params) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machine::KernelEvent as E;

    #[test]
    fn json_shape_matches_prob_replay_format() {
        let mut recorder = TraceRecorder::new();
        recorder.record(E::TurnStart);
        recorder.record(E::ToolRequest { c: 1, d: true });
        recorder.record(E::AgentSpawn { a: 1, r: 2 });
        recorder.record(E::PolicyTighten);
        let value = recorder.to_prob_json();
        let list = value["transitionList"].as_array().unwrap();
        assert_eq!(list.len(), 5);
        assert_eq!(list[0]["name"], "$initialise_machine");
        assert_eq!(list[1]["name"], "TurnStart");
        assert!(list[1].get("params").is_none());
        assert_eq!(list[2]["params"]["c"], "1");
        assert_eq!(list[2]["params"]["d"], "TRUE");
        assert!(list[2]["params"].get("r").is_none());
        assert_eq!(list[3]["params"]["a"], "1");
        assert_eq!(list[3]["params"]["r"], "2");
        assert_eq!(list[4]["name"], "PolicyTighten");
    }
}
