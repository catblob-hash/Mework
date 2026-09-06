//! AI SDK sidecar boundary.
//!
//! `api.rs` is the host for turn looping, tool dispatch, approvals, hooks,
//! credentials, image budgets, persistence, cancellation, and token accounting. This
//! layer only sends a model step and receives its result.
//!
//! Provider request formats and SSE parsing live in the `aisdk-service/` Node sidecar
//! through the Vercel AI SDK. This module owns the NDJSON protocol, process lifecycle,
//! and event translation across that boundary.
//!
//! The sidecar runs exactly one model step and declares tools without `execute`, so it
//! stops at tool calls. Tool execution, approval, and subsequent-turn decisions remain
//! in Rust alongside Mework's tool loop, agent-kernel specification, agent pool, and
//! task abstraction.

pub(crate) mod agent;
pub(crate) mod process;
pub(crate) mod project;
pub(crate) mod protocol;
pub(crate) mod step;
pub(crate) mod tools;
#[cfg(test)]
pub(crate) mod tests;

use std::collections::HashMap;

use serde_json::Value;

use crate::api::ToolCall;
use crate::http_util::sanitize_error;
use crate::model::ModelUsage;

use self::protocol::StepResult;

// ---------------------------------------------------------- Request-result vocabulary
//
// These types describe host policy rather than HTTP transport: `Api` versus `Fatal`
// determines retry eligibility, and `StreamPartial` contains content already visible
// to the renderer when a request fails.

#[derive(Debug)]
pub(crate) enum ModelRequestError {
    /// Transient API failure (network, 5xx/429, broken stream, malformed
    /// transfer). Bounded automatic retries are worthwhile.
    Api(String),
    /// Permanent API failure (auth, bad request, provider contract violation,
    /// truncation by policy). Retrying would repeat the same outcome, so it
    /// surfaces immediately.
    Fatal(String),
    EventSink(String),
}

impl ModelRequestError {
    pub(crate) fn sanitize(self, key: Option<&str>) -> Self {
        match self {
            Self::Api(message) => Self::Api(sanitize_error(&message, key)),
            Self::Fatal(message) => Self::Fatal(sanitize_error(&message, key)),
            Self::EventSink(message) => Self::EventSink(message),
        }
    }

    pub(crate) fn message(&self) -> &str {
        match self {
            Self::Api(message) | Self::Fatal(message) | Self::EventSink(message) => message,
        }
    }
}

/// Visible content already streamed to the renderer when a request died
/// mid-stream. Preserved so an exhausted retry loop can keep the partial
/// message in the timeline instead of deleting it.
#[derive(Clone, Debug, Default)]
pub(crate) struct StreamPartial {
    pub(crate) text: String,
    pub(crate) reasoning: Vec<String>,
}

impl StreamPartial {
    pub(crate) fn is_empty(&self) -> bool {
        self.text.is_empty() && self.reasoning.iter().all(|item| item.trim().is_empty())
    }
}

/// One failed model request attempt: the classified error plus whatever
/// partial content that attempt had already streamed.
#[derive(Debug)]
pub(crate) struct RequestFailure {
    pub(crate) error: ModelRequestError,
    pub(crate) partial: StreamPartial,
    pub(crate) retry_after_ms: Option<u64>,
}

impl RequestFailure {
    pub(crate) fn bare(error: ModelRequestError) -> Self {
        Self {
            error,
            partial: StreamPartial::default(),
            retry_after_ms: None,
        }
    }

    pub(crate) fn sanitize(self, key: Option<&str>) -> Self {
        Self {
            error: self.error.sanitize(key),
            partial: self.partial,
            retry_after_ms: self.retry_after_ms,
        }
    }
}

/// Host representation of a model-step result.
///
/// Its stable shape keeps the turn loop independent of the active provider protocol;
/// sidecar `done` frames may change its source but not its fields.
#[derive(Clone, Debug)]
pub(crate) struct ParsedModelResponse {
    pub(crate) text: String,
    pub(crate) reasoning: Vec<String>,
    /// Cumulative reasoning wall-clock milliseconds for this step.
    ///
    /// `reasoning` retains one slot per evidence-gated item, including empty
    /// encrypted items. Duration also supports metadata-only legacy responses.
    pub(crate) reasoning_ms: Option<u64>,
    pub(crate) calls: Vec<ToolCall>,
    /// Calls announced in the stream before their arguments were complete. The turn
    /// loop uses these to avoid duplicate announcements.
    pub(crate) announced_tool_calls: HashMap<String, String>,
    pub(crate) usage: ModelUsage,
    pub(crate) model: Option<String>,
    pub(crate) stop_reason: Option<String>,
    /// Raw provider stop reason before AI SDK normalization. `pause_turn` appears
    /// only here: after normalization it becomes `stop`, but pausing and completing
    /// have distinct turn-loop semantics. `None` means the provider omitted it.
    pub(crate) raw_stop_reason: Option<String>,
    /// Opaque continuation payload. The host persists and replays it unchanged.
    pub(crate) continuation: Value,
    /// Failure facts from provider-executed tools such as server search or fetch.
    /// Normal turns have none; native-search results use them to report retrieval
    /// failures rather than claiming retrieval completed.
    pub(crate) provider_tool_errors: Vec<protocol::ProviderToolError>,
    /// Sources cited by the provider for server retrieval or grounding. Normal turns
    /// attach them to this round's assistant card; native-search writes them into its
    /// result envelope.
    pub(crate) sources: Vec<protocol::SidecarSource>,
    pub(crate) native_search_uses: Option<u32>,
    pub(crate) native_search_call_ids: Vec<String>,
}

impl From<StepResult> for ParsedModelResponse {
    fn from(result: StepResult) -> Self {
        // The sidecar always announces a call before delivering its arguments, so
        // every call here was announced. This map is therefore the call table.
        let announced_tool_calls = result
            .calls
            .iter()
            .map(|call| (call.call_id.clone(), call.tool_name.clone()))
            .collect();
        Self {
            text: result.text,
            reasoning: result.reasoning,
            reasoning_ms: result.reasoning_ms,
            calls: result
                .calls
                .into_iter()
                .map(|call| ToolCall {
                    id: call.call_id,
                    name: call.tool_name,
                    // Non-object arguments become an empty object because tool
                    // dispatch expects `JsonObject` and the sidecar already bounds
                    // argument size.
                    input: call.input.as_object().cloned().unwrap_or_default(),
                })
                .collect(),
            announced_tool_calls,
            usage: ModelUsage {
                input_tokens: result.usage.input_tokens,
                cached_input_tokens: result.usage.cache_read_tokens,
                output_tokens: result.usage.output_tokens,
                total_tokens: result.usage.total_tokens,
                // A subset of `output_tokens`, retained for display only and never
                // added to any total.
                reasoning_tokens: result.usage.reasoning_tokens,
            },
            model: result.model,
            stop_reason: result.finish_reason,
            raw_stop_reason: result.raw_finish_reason,
            // AI SDK `response.messages` retains Anthropic `encryptedContent`,
            // reasoning signatures, and Responses reasoning items.
            continuation: Value::Array(result.response_messages),
            provider_tool_errors: result.provider_tool_errors,
            sources: result.sources,
            native_search_uses: result.native_search_uses,
            native_search_call_ids: result.native_search_call_ids,
        }
    }
}

// Consumers use module paths such as `aisdk::protocol::StepRequest` and
// `aisdk::process::run_step`. Keep the module boundary explicit: protocol shape,
// process multiplexing, timeline projection, request assembly, and tool policy have
// distinct responsibilities. `ParsedModelResponse` is the external-shape exception.
