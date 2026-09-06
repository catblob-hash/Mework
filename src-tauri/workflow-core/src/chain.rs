//! Cache-key chain and replay divergence state.
//!
//! Each key hashes the preceding key, step prompt, and canonical options in entry
//! order. Once a journal lookup misses, every later lookup is untrusted because
//! downstream results depend on recomputed upstream output.

use std::fmt::Write as _;

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{WorkflowStepRequest, CACHE_KEY_PREFIX};

/// Cache-key chain and divergence state for one run or replay.
///
/// This intentionally does not implement `Clone`: cloning a pre-miss snapshot
/// could bypass the one-way divergence latch.
#[derive(Debug, Default)]
pub struct CacheKeyChain {
    prev: String,
    diverged: bool,
}

impl CacheKeyChain {
    pub fn new() -> Self {
        Self::default()
    }

    /// Advance the chain with the previous key and this step request.
    ///
    /// The key is `prefix:hex(sha256(prev || 0x00 || prompt || 0x00 || options))`.
    /// It depends only on the prompt and canonical option fields, never display or
    /// scheduling parameters. Advancing computes a key but does not alter trust.
    pub fn advance(&mut self, request: &WorkflowStepRequest) -> String {
        let opts = canonical_opts(request);
        let mut hasher = Sha256::new();
        hasher.update(self.prev.as_bytes());
        hasher.update([0u8]);
        hasher.update(request.prompt.as_bytes());
        hasher.update([0u8]);
        hasher.update(opts.as_bytes());
        let digest = hasher.finalize();
        let mut key = String::with_capacity(CACHE_KEY_PREFIX.len() + 1 + digest.len() * 2);
        key.push_str(CACHE_KEY_PREFIX);
        key.push(':');
        for byte in digest {
            let _ = write!(key, "{byte:02x}");
        }
        self.prev = key.clone();
        key
    }

    /// Return whether the newly advanced key may reuse its journal result.
    ///
    /// The first miss permanently latches the chain as divergent, including when
    /// later journal lookups would otherwise hit.
    pub fn consult(&mut self, journal_has_result: bool) -> bool {
        if self.diverged {
            return false;
        }
        if journal_has_result {
            true
        } else {
            self.diverged = true;
            false
        }
    }

    pub fn is_diverged(&self) -> bool {
        self.diverged
    }

    /// Permanently mark the chain divergent when an external condition requires it.
    pub fn mark_diverged(&mut self) {
        self.diverged = true;
    }
}

/// Canonical text for the five request options that participate in a cache key.
///
/// Fields are emitted alphabetically in compact JSON; absent fields are omitted.
/// Schemas are recursively canonicalized with byte-sorted object keys, dropped
/// `__proto__` keys, and preserved array order. Iterative serialization avoids
/// exhausting the native stack before [`crate::MAX_SCHEMA_DEPTH`].
pub fn canonical_opts(request: &WorkflowStepRequest) -> String {
    let mut out = String::from("{");
    let mut first = true;
    let field = |out: &mut String, first: &mut bool, key: &str, value: Option<&str>| {
        if let Some(value) = value {
            if !*first {
                out.push(',');
            }
            *first = false;
            out.push('"');
            out.push_str(key);
            out.push_str("\":");
            out.push_str(&serde_json::to_string(value).expect("strings always serialize"));
        }
    };
    field(&mut out, &mut first, "agentType", request.agent_type.as_deref());
    field(&mut out, &mut first, "effort", request.effort.as_deref());
    field(&mut out, &mut first, "isolation", request.isolation.as_deref());
    field(&mut out, &mut first, "model", request.model.as_deref());
    if let Some(schema) = &request.schema {
        if !first {
            out.push(',');
        }
        out.push_str("\"schema\":");
        write_canonical_json(schema, &mut out);
    }
    out.push('}');
    out
}

/// Iteratively write a JSON value as compact text with deterministic key order.
fn write_canonical_json(root: &Value, out: &mut String) {
    enum Task<'a> {
        Value(&'a Value),
        Literal(&'static str),
        Key(&'a str),
    }
    let mut stack = vec![Task::Value(root)];
    while let Some(task) = stack.pop() {
        match task {
            Task::Literal(text) => out.push_str(text),
            Task::Key(key) => {
                out.push_str(&serde_json::to_string(key).expect("strings always serialize"));
                out.push(':');
            }
            Task::Value(value) => match value {
                Value::Object(map) => {
                    out.push('{');
                    let mut entries: Vec<(&String, &Value)> = map
                        .iter()
                        .filter(|(key, _)| key.as_str() != "__proto__")
                        .collect();
                    entries.sort_by(|left, right| left.0.cmp(right.0));
                    stack.push(Task::Literal("}"));
                    for (position, (key, child)) in entries.iter().enumerate().rev() {
                        stack.push(Task::Value(child));
                        stack.push(Task::Key(key));
                        if position > 0 {
                            stack.push(Task::Literal(","));
                        }
                    }
                }
                Value::Array(items) => {
                    out.push('[');
                    stack.push(Task::Literal("]"));
                    for (position, child) in items.iter().enumerate().rev() {
                        stack.push(Task::Value(child));
                        if position > 0 {
                            stack.push(Task::Literal(","));
                        }
                    }
                }
                scalar => {
                    out.push_str(&serde_json::to_string(scalar).expect("scalars always serialize"));
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{json, Map, Value};

    use super::*;

    fn request(prompt: &str) -> WorkflowStepRequest {
        WorkflowStepRequest::from_prompt(prompt)
    }

    /// Independently computed golden keys pin byte stability across builds and
    /// platforms because cache keys persist in workflow journals.
    const GOLDEN_STEP_ONE_KEY: &str =
        "mw1:e37d53952667c9984b6a4a52b836a8df52686ec479bfe4f279eb448fff833595";
    const GOLDEN_STEP_TWO_KEY: &str =
        "mw1:933620909cce5aa0bfc38cde937f40ea6051f7fecea5f2f5267ec82c440eb42a";

    #[test]
    fn the_fixed_two_step_sequence_reproduces_its_golden_keys_deterministically() {
        let mut chain = CacheKeyChain::new();
        let first = chain.advance(&request("p1"));
        let mut second_request = request("p2");
        second_request.model = Some("m".into());
        let second = chain.advance(&second_request);

        assert_eq!(first, GOLDEN_STEP_ONE_KEY);
        assert_eq!(second, GOLDEN_STEP_TWO_KEY);
        for key in [&first, &second] {
            let suffix = key
                .strip_prefix(&format!("{CACHE_KEY_PREFIX}:"))
                .expect("键带前缀");
            assert_eq!(suffix.len(), 64);
            assert!(suffix
                .chars()
                .all(|ch| ch.is_ascii_hexdigit() && !ch.is_ascii_uppercase()));
        }

        let mut rebuilt = CacheKeyChain::new();
        rebuilt.advance(&request("p1"));
        assert_eq!(rebuilt.advance(&second_request), second);
    }

    #[test]
    fn the_key_changes_when_prompt_model_or_schema_change() {
        let baseline = CacheKeyChain::new().advance(&request("p"));

        assert_ne!(CacheKeyChain::new().advance(&request("q")), baseline);

        let mut with_model = request("p");
        with_model.model = Some("m".into());
        assert_ne!(CacheKeyChain::new().advance(&with_model), baseline);

        let mut with_schema = request("p");
        with_schema.schema = Some(json!({"type": "object"}));
        let schema_key = CacheKeyChain::new().advance(&with_schema);
        assert_ne!(schema_key, baseline);
        let mut other_schema = request("p");
        other_schema.schema = Some(json!({"type": "object", "required": ["a"]}));
        assert_ne!(CacheKeyChain::new().advance(&other_schema), schema_key);
    }

    /// `isolation` affects the key, while its absence must leave no bytes behind.
    ///
    /// Isolation changes the visible filesystem and therefore must change the key.
    /// Absent options must be omitted so adding a new optional field cannot alter
    /// keys persisted by prior journals.
    #[test]
    fn isolation_participates_in_the_key_while_its_absence_leaves_no_trace() {
        let baseline = CacheKeyChain::new().advance(&request("p"));

        let mut isolated = request("p");
        isolated.isolation = Some("worktree".into());
        assert_ne!(CacheKeyChain::new().advance(&isolated), baseline);

        // An absent option canonicalizes to `{}`, not `{"isolation":null}`.
        assert_eq!(canonical_opts(&request("p")), "{}");
        assert_eq!(
            canonical_opts(&isolated),
            "{\"isolation\":\"worktree\"}"
        );
    }

    #[test]
    fn display_and_scheduling_parameters_do_not_touch_the_key() {
        let baseline = CacheKeyChain::new().advance(&request("p"));
        let mut decorated = request("p");
        decorated.label = Some("标签".into());
        decorated.phase = Some("阶段".into());
        decorated.phase_index = Some(7);
        decorated.stall_ms = Some(1);
        assert_eq!(CacheKeyChain::new().advance(&decorated), baseline);
    }

    #[test]
    fn entry_order_drives_the_chain_so_swapped_steps_produce_different_keys() {
        let mut forward = CacheKeyChain::new();
        forward.advance(&request("a"));
        let forward_second = forward.advance(&request("b"));

        let mut swapped = CacheKeyChain::new();
        swapped.advance(&request("b"));
        let swapped_second = swapped.advance(&request("a"));

        assert_ne!(forward_second, swapped_second);
    }

    #[test]
    fn schema_canonicalization_ignores_key_order_and_drops_proto_keys() {
        let mut ordered = Map::new();
        ordered.insert("a".into(), json!(1));
        ordered.insert("b".into(), json!([1, 2]));
        let mut reversed = Map::new();
        reversed.insert("b".into(), json!([1, 2]));
        reversed.insert("a".into(), json!(1));

        let mut first = request("p");
        first.schema = Some(Value::Object(ordered));
        let mut second = request("p");
        second.schema = Some(Value::Object(reversed));
        assert_eq!(
            CacheKeyChain::new().advance(&first),
            CacheKeyChain::new().advance(&second)
        );

        let mut poisoned = Map::new();
        poisoned.insert("__proto__".into(), json!({"x": 1}));
        poisoned.insert("a".into(), json!(1));
        let mut clean = Map::new();
        clean.insert("a".into(), json!(1));
        let mut with_proto = request("p");
        with_proto.schema = Some(Value::Object(poisoned));
        let mut without_proto = request("p");
        without_proto.schema = Some(Value::Object(clean));
        assert_eq!(
            CacheKeyChain::new().advance(&with_proto),
            CacheKeyChain::new().advance(&without_proto)
        );
    }

    #[test]
    fn one_miss_latches_the_chain_so_a_later_would_be_hit_is_never_trusted() {
        let mut chain = CacheKeyChain::new();
        chain.advance(&request("a"));
        assert!(chain.consult(true), "分歧前的命中可复用");
        chain.advance(&request("b"));
        assert!(!chain.consult(false), "miss 本身不可复用");
        assert!(chain.is_diverged());
        chain.advance(&request("c"));
        assert!(
            !chain.consult(true),
            "分歧后的「本可命中」不得再被采信——上游已经重跑，它是旧输入的结果"
        );

        let mut marked = CacheKeyChain::new();
        marked.advance(&request("a"));
        marked.mark_diverged();
        assert!(marked.is_diverged());
        assert!(!marked.consult(true));

        let mut fresh = CacheKeyChain::new();
        fresh.advance(&request("a"));
        assert!(fresh.consult(true), "新链未分歧时命中可复用");
    }

    #[test]
    fn canonical_opts_is_compact_sorted_and_omits_absent_fields() {
        assert_eq!(canonical_opts(&request("p")), "{}");

        let mut effort_only = request("p");
        effort_only.effort = Some("high".into());
        assert_eq!(canonical_opts(&effort_only), r#"{"effort":"high"}"#);

        let mut full = request("p");
        full.agent_type = Some("worker".into());
        full.effort = Some("low".into());
        full.model = Some("m".into());
        full.schema = Some(json!({"type": "object"}));
        assert_eq!(
            canonical_opts(&full),
            r#"{"agentType":"worker","effort":"low","model":"m","schema":{"type":"object"}}"#
        );
    }

    #[test]
    fn escaped_model_text_cannot_collide_with_a_separate_schema_field() {
        let mut injected_model = request("p");
        injected_model.model = Some(r#"x","schema":null"#.into());
        let mut separate_schema = request("p");
        separate_schema.model = Some("x".into());
        separate_schema.schema = Some(Value::Null);

        let injected_text = canonical_opts(&injected_model);
        let separate_text = canonical_opts(&separate_schema);
        assert!(injected_text.contains(r#"\""#), "model 内的引号必须由 JSON 转义");
        assert_ne!(injected_text, separate_text);
        assert_ne!(
            CacheKeyChain::new().advance(&injected_model),
            CacheKeyChain::new().advance(&separate_schema)
        );
    }

    #[test]
    fn schema_array_order_changes_the_canonical_text_and_key() {
        let mut forward = request("p");
        forward.schema = Some(json!({"enum": [1, 2]}));
        let mut reversed = request("p");
        reversed.schema = Some(json!({"enum": [2, 1]}));

        assert_ne!(canonical_opts(&forward), canonical_opts(&reversed));
        assert_ne!(
            CacheKeyChain::new().advance(&forward),
            CacheKeyChain::new().advance(&reversed)
        );
    }

    #[test]
    fn nested_proto_keys_are_removed_from_the_canonical_text_and_key() {
        let mut poisoned = request("p");
        poisoned.schema = Some(json!({
            "properties": {
                "__proto__": {"x": 1},
                "a": {"type": "string"}
            }
        }));
        let mut clean = request("p");
        clean.schema = Some(json!({
            "properties": {
                "a": {"type": "string"}
            }
        }));

        assert_eq!(canonical_opts(&poisoned), canonical_opts(&clean));
        assert_eq!(
            CacheKeyChain::new().advance(&poisoned),
            CacheKeyChain::new().advance(&clean)
        );
    }

    #[test]
    fn reverse_inserted_schema_keys_have_a_fixed_canonical_text() {
        let mut schema = Map::new();
        schema.insert("b".into(), json!([2, 1]));
        schema.insert("a".into(), json!(1));
        let mut with_schema = request("p");
        with_schema.schema = Some(Value::Object(schema));

        assert_eq!(
            canonical_opts(&with_schema),
            r#"{"schema":{"a":1,"b":[2,1]}}"#
        );
    }

    #[test]
    fn non_ascii_schema_keys_are_sorted_by_their_utf8_bytes() {
        let mut schema = Map::new();
        schema.insert("\u{10000}".into(), json!(2));
        schema.insert("\u{e000}".into(), json!(1));
        let mut with_schema = request("p");
        with_schema.schema = Some(Value::Object(schema));

        let canonical = canonical_opts(&with_schema);
        let private_use_position = canonical.find('\u{e000}').expect("输出包含 U+E000 键");
        let supplementary_position = canonical.find('\u{10000}').expect("输出包含 U+10000 键");
        assert!(
            private_use_position < supplementary_position,
            "UTF-8 字节序 EE8080 必须排在 F0908080 之前"
        );
    }

    #[test]
    fn effort_and_agent_type_each_participate_in_the_key() {
        let baseline = CacheKeyChain::new().advance(&request("p"));
        let mut with_effort = request("p");
        with_effort.effort = Some("high".into());
        let mut with_agent_type = request("p");
        with_agent_type.agent_type = Some("worker".into());

        assert_ne!(CacheKeyChain::new().advance(&with_effort), baseline);
        assert_ne!(CacheKeyChain::new().advance(&with_agent_type), baseline);
    }

    #[test]
    fn canonicalization_handles_fifty_thousand_nested_arrays_iteratively() {
        let mut schema = Value::Null;
        for _ in 0..50_000 {
            schema = Value::Array(vec![schema]);
        }
        let mut deeply_nested = request("p");
        deeply_nested.schema = Some(schema);

        let _canonical = canonical_opts(&deeply_nested);
        let _key = CacheKeyChain::new().advance(&deeply_nested);

        // Value destruction is recursive; leak the test value to avoid stack
        // overflow after verifying iterative canonicalization.
        std::mem::forget(deeply_nested);
    }
}
