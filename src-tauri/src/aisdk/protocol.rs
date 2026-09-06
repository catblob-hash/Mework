//! Rust endpoint for the sidecar protocol.
//!
//! This must exactly match `aisdk-service/src/protocol.ts`. Both sides restart on a
//! `v` mismatch; they do not negotiate backward compatibility.

// This file mirrors the wire contract rather than collecting ordinary data types.
// Retain fields that the peer sends even before a local consumer needs them, so the
// contract cannot silently narrow.
#![allow(dead_code)]

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Protocol generation. Must exactly equal the TypeScript `PROTOCOL_VERSION`.
///
/// Increment this when a stale sidecar could silently suppress required behavior;
/// the generation gate turns that condition into an explicit startup failure.
pub(crate) const PROTOCOL_VERSION: u32 = 9;

/// Maximum line size (16 MiB). Both sides enforce it because neither side trusts
/// the other.
pub(crate) const MAX_LINE_BYTES: usize = 16 * 1024 * 1024;

/// Adapter family. Maps one-to-one to `crate::model::ProviderFamily`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Family {
    OpenaiResponses,
    OpenaiCodex,
    OpenaiChat,
    Anthropic,
    /// Claude Agent SDK driving the locally installed Claude Code executable.
    /// Not an HTTP dialect: the sidecar owns a CLI session per host run and
    /// parks the CLI's tool handlers between steps.
    ClaudeAgent,
    Google,
    Xai,
    Azure,
    Bedrock,
    Vertex,
    OpenaiCompatible,
}

impl Family {
    /// Map a persisted family to its wire-format slug.
    ///
    /// Keep these enums separate because persistence uses `snake_case` while the
    /// sidecar protocol uses `kebab-case`; each has an independent compatibility
    /// boundary. This exhaustive mapping must reject newly added families at compile
    /// time rather than silently routing them through a generic adapter.
    pub(crate) fn for_format(family: crate::model::ProviderFamily) -> Self {
        use crate::model::ProviderFamily as Persisted;
        match family {
            Persisted::OpenaiResponses => Self::OpenaiResponses,
            Persisted::OpenaiCodex => Self::OpenaiCodex,
            Persisted::OpenaiChat => Self::OpenaiChat,
            Persisted::Anthropic => Self::Anthropic,
            Persisted::ClaudeAgent => Self::ClaudeAgent,
            Persisted::Google => Self::Google,
            Persisted::Xai => Self::Xai,
            Persisted::Azure => Self::Azure,
            Persisted::Bedrock => Self::Bedrock,
            Persisted::Vertex => Self::Vertex,
            Persisted::OpenaiCompatible => Self::OpenaiCompatible,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ToolSpec {
    pub(crate) name: String,
    pub(crate) description: String,
    /// Copied verbatim from the `input_schema` in `builtin_schemas` or the descriptor.
    #[serde(rename = "inputSchema")]
    pub(crate) input_schema: Value,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct NativeSearch {
    #[serde(rename = "maxUses")]
    pub(crate) max_uses: u32,
    #[serde(rename = "previousCallIds", skip_serializing_if = "Vec::is_empty")]
    pub(crate) previous_call_ids: Vec<String>,
}

/// Claude Code session parameters for the `claude-agent` family.
///
/// Absent for every other family. The sidecar keys its parked CLI session by
/// `session`; the host mints one per run and sends `release` when the run ends.
/// `executable` is host-resolved so the sidecar never searches the disk, and
/// `env` carries only the profile-location variables the CLI needs to find its
/// own configuration (the sidecar spawns with a cleared environment). This
/// family has no credential at all — the CLI uses its own login — so
/// `api_key`/`base_url` are always absent; `env` is the sole channel by which a
/// loopback test double can be pointed at, and the sidecar rejects a remote one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct AgentSession {
    pub(crate) session: String,
    pub(crate) executable: String,
    pub(crate) cwd: String,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) env: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StepRequest {
    pub(crate) family: Family,
    /// Address that passed the host security gate. The sidecar must not revalidate it.
    ///
    /// The explicit rename is required because serde's `camelCase` yields `baseUrl`,
    /// while the sidecar reads the AI SDK spelling, `baseURL`.
    ///
    /// Absence selects the provider default. Vertex and Bedrock derive endpoints from
    /// `project`, `location`, or `region` in `settings`.
    #[serde(rename = "baseURL", skip_serializing_if = "Option::is_none")]
    pub(crate) base_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) api_key: Option<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) headers: BTreeMap<String, String>,
    /// Family-specific identity fields keyed by `FamilySetting::wire_name()`
    /// (`region`, `project`, `location`, or `apiVersion`).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) settings: BTreeMap<String, String>,
    pub(crate) model_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) system: Option<String>,
    /// AI SDK `ModelMessage[]` produced by the host's canonical projection.
    pub(crate) messages: Vec<Value>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) tools: Vec<ToolSpec>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) tool_choice: Option<Value>,
    /// Always 1 for ordinary turns; host-created one-shot native-search requests may exceed it.
    pub(crate) max_steps: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) max_output_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) temperature: Option<f64>,
    /// Reasoning effort using the AI SDK 7 vocabulary (`provider-default`, `none`,
    /// `minimal`, `low`, `medium`, `high`, or `xhigh`), not a provider dialect.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) reasoning: Option<&'static str>,
    /// The model's reasoning response form: `"plaintext"` or `"encrypted"`.
    ///
    /// The stored model attribute is always one of the two, so this carries no
    /// default policy. Absence means this provider has no reasoning-form
    /// consumer. Plaintext mode enables response dialect translation, but all
    /// Responses modes request encrypted replay data.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) reasoning_content: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) provider_options: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) native_search: Option<NativeSearch>,
    /// Present only for the `claude-agent` family; see [`AgentSession`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) agent: Option<AgentSession>,
}

/// Sidecar-to-host frame.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub(crate) enum SidecarFrame {
    Ready {
        protocol: u32,
        #[serde(default)]
        versions: BTreeMap<String, String>,
    },
    Event {
        id: String,
        event: StepEvent,
    },
    Done {
        id: String,
        result: StepResult,
    },
    Error {
        id: String,
        error: StepError,
    },
}

impl SidecarFrame {
    /// Return this frame's request, if any. `ready` is not tied to a request.
    pub(crate) fn request_id(&self) -> Option<&str> {
        match self {
            Self::Ready { .. } => None,
            Self::Event { id, .. } | Self::Done { id, .. } | Self::Error { id, .. } => Some(id),
        }
    }
}

/// Stream event. Values map one-to-one to `crate::model::ModelStreamEvent`; the
/// host passes them through.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "k", rename_all = "kebab-case")]
pub(crate) enum StepEvent {
    TextDelta {
        delta: String,
    },
    /// A provider opened a reasoning item.
    ///
    /// This is not a `ReasoningDelta` prefix: an item may produce no summary text,
    /// making this event the only live reasoning signal. `item` is a zero-based
    /// sequence number among evidence-gated items and identifies each reasoning
    /// segment consistently in host and renderer projections.
    ReasoningStart {
        #[serde(default)]
        item: usize,
        #[serde(default)]
        form: Option<crate::model::ReasoningForm>,
    },
    ReasoningDelta {
        #[serde(default)]
        item: usize,
        delta: String,
    },
    ReasoningDone {
        #[serde(default)]
        item: usize,
        /// Cumulative reasoning wall-clock milliseconds for this step. The sidecar
        /// measures stream timing and sends this value both live and in
        /// `StepResult::reasoning_ms` so reloads retain the same duration.
        #[serde(rename = "durationMs", default)]
        duration_ms: Option<u64>,
    },
    #[serde(rename_all = "camelCase")]
    ToolCallAnnounced {
        call_id: String,
        tool_name: String,
    },
    #[serde(rename_all = "camelCase")]
    ToolCall {
        call_id: String,
        tool_name: String,
        input: Value,
    },
    Usage {
        usage: SidecarUsage,
    },
    Source(SidecarSource),
    /// Emitted every 500 ms. The host uses it as a cancellation probe.
    Heartbeat,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SidecarUsage {
    pub(crate) input_tokens: Option<u64>,
    pub(crate) output_tokens: Option<u64>,
    pub(crate) total_tokens: Option<u64>,
    pub(crate) reasoning_tokens: Option<u64>,
    pub(crate) cache_read_tokens: Option<u64>,
    pub(crate) cache_write_tokens: Option<u64>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SidecarSource {
    pub(crate) id: String,
    #[serde(default)]
    pub(crate) url: Option<String>,
    #[serde(default)]
    pub(crate) title: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SidecarCall {
    pub(crate) call_id: String,
    pub(crate) tool_name: String,
    pub(crate) input: Value,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StepResult {
    #[serde(default)]
    pub(crate) text: String,
    #[serde(default)]
    pub(crate) reasoning: Vec<String>,
    /// Cumulative reasoning wall-clock milliseconds for this step.
    ///
    /// Unlike `reasoning`, which only contains nonempty summaries, this may exist
    /// for encrypted reasoning with no summary text. Its presence determines whether
    /// the host renders a reasoning card; absence means no reasoning item occurred.
    #[serde(default)]
    pub(crate) reasoning_ms: Option<u64>,
    /// Contains only client-side tool calls. The sidecar filters provider-executed calls.
    #[serde(default)]
    pub(crate) calls: Vec<SidecarCall>,
    #[serde(default)]
    pub(crate) usage: SidecarUsage,
    #[serde(default)]
    pub(crate) model: Option<String>,
    #[serde(default)]
    pub(crate) finish_reason: Option<String>,
    /// Raw upstream stop reason, without AI SDK normalization.
    ///
    /// `pause_turn` normalizes to `finishReason:"stop"`, so continuation decisions
    /// must use this field. Absence means the provider did not report or forward it.
    #[serde(default)]
    pub(crate) raw_finish_reason: Option<String>,
    /// Opaque continuation blocks from AI SDK `response.messages`. The host persists
    /// and replays them without interpretation so encrypted content, reasoning
    /// signatures, and reasoning items survive across turns.
    #[serde(default)]
    pub(crate) response_messages: Vec<Value>,
    #[serde(default)]
    pub(crate) sources: Vec<SidecarSource>,
    /// Newly executed unique provider search calls, including failed calls and
    /// excluding IDs replayed in request history. Present on native-search steps.
    #[serde(default)]
    pub(crate) native_search_uses: Option<u32>,
    #[serde(default)]
    pub(crate) native_search_call_ids: Vec<String>,
    /// Failures from provider-executed tools such as server-side search or fetch.
    /// These calls must never be executed again by the host; native search uses the
    /// failures to report an accurate result envelope.
    #[serde(default)]
    pub(crate) provider_tool_errors: Vec<ProviderToolError>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProviderToolError {
    #[serde(default)]
    pub(crate) call_id: String,
    #[serde(default)]
    pub(crate) tool_name: String,
    #[serde(default)]
    pub(crate) message: String,
}

/// Failure classification. The host retry loop uses only `kind`.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ErrorKind {
    Transient,
    Permanent,
    Cancelled,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct StepError {
    pub(crate) kind: ErrorKind,
    pub(crate) message: String,
    #[serde(default)]
    pub(crate) status: Option<u16>,
    #[serde(default, rename = "retryAfterMs")]
    pub(crate) retry_after_ms: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_after_hint_decodes_and_is_optional() {
        let error: StepError = serde_json::from_value(serde_json::json!({
            "kind": "transient", "message": "later", "status": 429, "retryAfterMs": 60000
        })).unwrap();
        assert_eq!(error.retry_after_ms, Some(60000));
        let bare: StepError = serde_json::from_value(serde_json::json!({
            "kind": "transient", "message": "later"
        })).unwrap();
        assert_eq!(bare.retry_after_ms, None);
    }

    /// Family slugs must exactly match the TypeScript `ProviderFamily` literals.
    #[test]
    fn family_slugs_match_the_sidecar() {
        let cases = [
            (Family::OpenaiResponses, "openai-responses"),
            (Family::OpenaiCodex, "openai-codex"),
            (Family::OpenaiChat, "openai-chat"),
            (Family::Anthropic, "anthropic"),
            (Family::ClaudeAgent, "claude-agent"),
            (Family::Google, "google"),
            (Family::Xai, "xai"),
            (Family::Azure, "azure"),
            (Family::Bedrock, "bedrock"),
            (Family::Vertex, "vertex"),
            (Family::OpenaiCompatible, "openai-compatible"),
        ];
        for (family, slug) in cases {
            assert_eq!(serde_json::to_value(family).unwrap(), Value::String(slug.into()));
        }
    }

    #[test]
    fn stream_events_decode_from_the_sidecar_shape() {
        let text: StepEvent = serde_json::from_str(r#"{"k":"text-delta","delta":"你好"}"#).unwrap();
        assert!(matches!(text, StepEvent::TextDelta { delta } if delta == "你好"));

        let announced: StepEvent =
            serde_json::from_str(r#"{"k":"tool-call-announced","callId":"c1","toolName":"ls"}"#)
                .unwrap();
        assert!(
            matches!(announced, StepEvent::ToolCallAnnounced { call_id, tool_name } if call_id == "c1" && tool_name == "ls")
        );

        let heartbeat: StepEvent = serde_json::from_str(r#"{"k":"heartbeat"}"#).unwrap();
        assert!(matches!(heartbeat, StepEvent::Heartbeat));
    }

    /// `reasoning-start` is the only reasoning event without text, so it signals
    /// live reasoning when encrypted reasoning produces no delta. All three events
    /// carry the `item` sequence number.
    #[test]
    fn reasoning_events_carry_the_start_signal_and_the_duration() {
        let start: StepEvent = serde_json::from_str(r#"{"k":"reasoning-start","item":0}"#).unwrap();
        assert!(matches!(start, StepEvent::ReasoningStart { item: 0, form: None }));

        let second: StepEvent =
            serde_json::from_str(r#"{"k":"reasoning-delta","item":1,"delta":"想"}"#).unwrap();
        assert!(
            matches!(second, StepEvent::ReasoningDelta { item: 1, delta } if delta == "想")
        );

        let done: StepEvent =
            serde_json::from_str(r#"{"k":"reasoning-done","item":1,"durationMs":1840}"#).unwrap();
        assert!(matches!(
            done,
            StepEvent::ReasoningDone { item: 1, duration_ms } if duration_ms == Some(1840)
        ));

        // A missing duration remains decodable for cancelled streams or providers
        // that omit `reasoning-start`. Missing `item` defaults to 0 for test fixtures.
        let bare: StepEvent = serde_json::from_str(r#"{"k":"reasoning-done"}"#).unwrap();
        assert!(matches!(
            bare,
            StepEvent::ReasoningDone { item: 0, duration_ms } if duration_ms.is_none()
        ));
    }

    /// `reasoningMs` remains present for encrypted reasoning with no summary text,
    /// allowing the host to create a textless reasoning card.
    #[test]
    fn a_step_result_reports_reasoning_time_even_with_no_summary_text() {
        let result: StepResult = serde_json::from_str(
            r#"{"text":"","reasoning":[],"reasoningMs":2400,"calls":[],
                "usage":{"outputTokens":700,"reasoningTokens":512},
                "responseMessages":[],"sources":[]}"#,
        )
        .unwrap();
        assert!(result.reasoning.is_empty());
        assert_eq!(result.reasoning_ms, Some(2400));
        assert_eq!(result.usage.reasoning_tokens, Some(512));
        // Older frames without `providerToolErrors` remain decodable.
        assert!(result.provider_tool_errors.is_empty());
    }

    #[test]
    fn native_search_consumption_decodes_and_reaches_the_host() {
        let result: StepResult = serde_json::from_str(r#"{"nativeSearchUses":2,"nativeSearchCallIds":["a","b"]}"#).unwrap();
        assert_eq!(result.native_search_uses, Some(2));
        let parsed = super::super::ParsedModelResponse::from(result);
        assert_eq!(parsed.native_search_uses, Some(2));
        assert_eq!(parsed.native_search_call_ids, vec!["a", "b"]);
        assert!(serde_json::from_str::<StepResult>(r#"{"nativeSearchUses":-1}"#).is_err());
        assert!(serde_json::from_str::<StepResult>(r#"{"nativeSearchUses":1.5}"#).is_err());
        assert_eq!(PROTOCOL_VERSION, 9);
    }

    /// Provider-executed tool failures use cross-language field names, so pin each
    /// field's decoding.
    #[test]
    fn provider_tool_errors_decode_from_the_sidecar_spelling() {
        let result: StepResult = serde_json::from_str(
            r#"{"text":"","reasoning":[],"calls":[],"usage":{},
                "responseMessages":[],"sources":[],
                "providerToolErrors":[{"callId":"srvtoolu_1","toolName":"web_search","message":"max_uses_exceeded"}]}"#,
        )
        .unwrap();
        assert_eq!(result.provider_tool_errors.len(), 1);
        let failure = &result.provider_tool_errors[0];
        assert_eq!(failure.call_id, "srvtoolu_1");
        assert_eq!(failure.tool_name, "web_search");
        assert_eq!(failure.message, "max_uses_exceeded");
    }

    /// `pause_turn` normalizes to `stop`, so continuation logic must decode the
    /// raw stop-reason field.
    #[test]
    fn the_raw_finish_reason_decodes_from_the_sidecar_spelling() {
        let result: StepResult = serde_json::from_str(
            r#"{"text":"稍等","reasoning":[],"calls":[],"usage":{},
                "finishReason":"stop","rawFinishReason":"pause_turn",
                "responseMessages":[],"sources":[]}"#,
        )
        .unwrap();
        assert_eq!(result.finish_reason.as_deref(), Some("stop"));
        assert_eq!(result.raw_finish_reason.as_deref(), Some("pause_turn"));

        // Older frames without `rawFinishReason` remain decodable.
        let bare: StepResult = serde_json::from_str(
            r#"{"text":"","reasoning":[],"calls":[],"usage":{},
                "responseMessages":[],"sources":[]}"#,
        )
        .unwrap();
        assert!(bare.raw_finish_reason.is_none());
    }

    #[test]
    fn a_step_request_omits_absent_options_instead_of_sending_null() {
        let request = StepRequest {
            family: Family::Anthropic,
            base_url: Some("https://example.test/v1".into()),
            api_key: None,
            headers: BTreeMap::new(),
            settings: BTreeMap::new(),
            model_id: "m".into(),
            system: None,
            messages: vec![],
            tools: vec![],
            tool_choice: None,
            max_steps: 1,
            max_output_tokens: None,
            temperature: None,
            reasoning: None,
            reasoning_content: None,
            provider_options: None,
            native_search: None,
            agent: None,
        };
        let value = serde_json::to_value(&request).unwrap();
        // Upstream options distinguish explicit null from an omitted field.
        for absent in ["apiKey", "system", "toolChoice", "maxOutputTokens", "temperature", "reasoning", "reasoningContent", "providerOptions", "nativeSearch", "headers", "tools", "agent"] {
            assert!(value.get(absent).is_none(), "{absent} 不该出现");
        }
        assert_eq!(value["maxSteps"], 1);
    }

    #[test]
    fn the_step_request_field_names_are_pinned_to_the_sidecar_spelling() {
        // These names are the cross-language contract and must match the sidecar.
        let request = StepRequest {
            family: Family::OpenaiCompatible,
            base_url: Some("https://example.test/v1".into()),
            api_key: Some("k".into()),
            headers: BTreeMap::from([("x-a".to_owned(), "b".to_owned())]),
            settings: BTreeMap::from([("region".to_owned(), "us-east-1".to_owned())]),
            model_id: "m".into(),
            system: Some("s".into()),
            messages: vec![Value::Null],
            tools: vec![ToolSpec {
                name: "t".into(),
                description: "d".into(),
                input_schema: serde_json::json!({}),
            }],
            tool_choice: Some(Value::String("auto".into())),
            max_steps: 1,
            max_output_tokens: Some(8),
            temperature: Some(0.5),
            reasoning: Some("high"),
            reasoning_content: Some("plaintext"),
            provider_options: Some(Value::Null),
            native_search: Some(NativeSearch { max_uses: 3, previous_call_ids: vec!["old-call".into()] }),
            agent: None,
        };
        let value = serde_json::to_value(&request).unwrap();
        assert_eq!(value["nativeSearch"]["previousCallIds"], serde_json::json!(["old-call"]));
        for expected in [
            "family",
            "baseURL",
            "apiKey",
            "headers",
            "modelId",
            "system",
            "messages",
            "tools",
            "toolChoice",
            "maxSteps",
            "maxOutputTokens",
            "temperature",
            // Pin this field to catch a spelling drift that would select the provider default.
            "reasoning",
            "reasoningContent",
            "providerOptions",
            "nativeSearch",
        ] {
            assert!(value.get(expected).is_some(), "缺少字段 {expected}");
        }
        assert_eq!(value["tools"][0]["inputSchema"], serde_json::json!({}));
        assert_eq!(value["nativeSearch"]["maxUses"], 3);
        // The default serde spelling must not appear.
        assert!(value.get("baseUrl").is_none(), "baseUrl 是错误拼法");
    }

    #[test]
    fn an_error_frame_carries_the_retry_decision() {
        let frame: SidecarFrame = serde_json::from_str(
            r#"{"v":1,"seq":3,"type":"error","id":"r1","error":{"kind":"transient","message":"上游临时故障","status":500}}"#,
        )
        .unwrap();
        let SidecarFrame::Error { id, error } = frame else {
            panic!("应当是 error 帧");
        };
        assert_eq!(id, "r1");
        assert_eq!(error.kind, ErrorKind::Transient);
        assert_eq!(error.status, Some(500));
    }

    /// The `agent` block is the `claude-agent` wire contract: its four field
    /// names are read by the sidecar's session table, so pin them byte-for-byte.
    /// `env` is omitted when empty, like every other optional map.
    #[test]
    fn the_agent_session_serializes_to_the_sidecar_spelling() {
        let session = AgentSession {
            session: "run-1".into(),
            executable: r"C:\Users\me\.local\bin\claude.exe".into(),
            cwd: r"C:\data\claude-agent".into(),
            env: BTreeMap::from([("USERPROFILE".to_owned(), r"C:\Users\me".to_owned())]),
        };
        let value = serde_json::to_value(&session).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "session": "run-1",
                "executable": r"C:\Users\me\.local\bin\claude.exe",
                "cwd": r"C:\data\claude-agent",
                "env": { "USERPROFILE": r"C:\Users\me" }
            })
        );

        let bare = AgentSession {
            env: BTreeMap::new(),
            ..session
        };
        let value = serde_json::to_value(&bare).unwrap();
        assert!(value.get("env").is_none(), "空 env 不该出现");
        assert_eq!(value["session"], "run-1");
    }
}
