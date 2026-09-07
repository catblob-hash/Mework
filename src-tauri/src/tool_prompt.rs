//! In-app approval prompts for model tool calls.
//!
//! The native `MessageDialog` this replaces was modal to the whole OS and
//! carried no state beyond the single answer. Approval is now a card above the
//! composer, driven by two shapes of prompt that share one registry:
//!
//! * **Model prompts.** A worker thread blocks on a one-shot channel while the
//!   card travels to the renderer, which answers through `resolve_tool_prompt`
//!   and wakes the worker. The waiter may be the top-level run's own thread or
//!   a detached task worker; [`PromptOwner`] is what tells them apart.
//! * **Manual prompts.** The tool editor's own calls have no run to stream
//!   through, so `request_tool_approval` returns the prompt descriptor instead
//!   of blocking, and the nonce is minted only when the answer arrives. The
//!   classified request stays host-side across those two calls, so the renderer
//!   cannot widen between asking and answering.
//!
//! # A card is not owned by a turn
//!
//! Tasks outlive the turn that created them (S11), so a workflow step or a
//! subagent can request approval after the top-level round ends. Cards travel
//! the app push channel when no run stream is open, and the renderer draws them
//! exactly as it draws cards from a run stream.
//!
//! A run end retracts only the cards that the run itself raised
//! ([`ToolPromptRegistry::cancel_run_prompts`]); a task's card survives the turn
//! exactly as the task does.
//!
//! `Ok(false)` means the user said no. Every other ending — stopped, timed out,
//! or no channel to ask through — is an `Err` that identifies the actual
//! outcome, so an unseen question is never attributed to the user as a refusal.
//!
//! Two invariants keep the "always allow" affordance from becoming a hole:
//!
//! * It is offered per (conversation, tool name) only, so allowing `read`
//!   never allows `write`, and it never crosses into another conversation.
//! * A decision the security classifier marked `mandatory_prompt` — VM guest
//!   authorization, global memory mutation, an MCP server that declares it
//!   needs interaction — can neither offer nor consume an allowance. Those
//!   cross a boundary the conversation's security level does not describe, so
//!   every one of them asks again.

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{sync_channel, RecvTimeoutError, SyncSender},
        Mutex,
    },
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::model::{JsonObject, ToolExecutionRequest};
use crate::security::RiskLevel;

/// How long a model prompt may stay unanswered before it is denied. The native
/// dialog blocked forever; a renderer that navigated away or crashed would
/// otherwise pin a worker thread for the process lifetime.
const PROMPT_TIMEOUT: Duration = Duration::from_secs(30 * 60);
/// Cancellation is polled rather than signalled, so a stopped run releases its
/// worker within this window instead of at the prompt timeout.
const CANCELLATION_POLL: Duration = Duration::from_millis(150);
/// Manual prompts have no blocked thread to notice they were abandoned, so they
/// are evicted on the next registry write instead.
const MANUAL_PROMPT_TTL: Duration = Duration::from_secs(30 * 60);
/// Maximum number of unanswered cards per conversation. A saturated conversation
/// must not block prompts in another conversation.
const MAX_PENDING_PROMPTS_PER_CONVERSATION: usize = 64;
/// The card is one or two lines above the composer, not a document viewer.
const MAX_SUMMARY_CHARS: usize = 240;
/// Feedback on a plan card is a paragraph of guidance, not a document. It is
/// quoted into a tool result, so it is bounded like every other model input.
const MAX_FEEDBACK_CHARS: usize = 4_000;

/// True for the tools whose whole argument is an arbitrary command line. The
/// user's instruction is explicit: a shell call is never blanket-allowed,
/// because "this class of call" is not a meaningful category when the class is
/// "run anything".
pub fn is_shell_tool(tool_name: &str) -> bool {
    matches!(tool_name, "bash" | "powershell")
}

/// Tools that may never carry a standing allowance, whatever the level says.
///
/// Two members, two different reasons:
///
/// * shell — see [`is_shell_tool`]: the class is "run anything".
/// * `playwright` — it is the one tool with an **unconditional human gate**
///   underneath the ordinary classifier. `browser_credential_takeover_denial`
///   asks the user directly before the Agent may act on a tab they signed in
///   to, and its comment says why a hook may not answer it: "this protects the
///   user's own signed-in session, and the user asked to be the one who
///   answers." An allowance recorded from any earlier `playwright` card would
///   have answered that gate too, silently, because the registry keyed
///   allowances by tool name alone. The takeover prompt is raised through the
///   same `ask`, so the only place that distinction can live is here.
pub fn never_blanket_allowed(tool_name: &str) -> bool {
    is_shell_tool(tool_name) || tool_name == "playwright"
}

/// One display line describing what the model wants to do, built from the
/// call's own arguments.
///
/// `input` must already be the redacted projection (`public_tool_input`), so
/// this never has to know which keys are secret — it only has to be readable.
/// Unknown tools fall back to their most descriptive scalar argument rather
/// than dumping JSON, because a card that shows a wall of braces is a card
/// nobody reads before clicking.
pub fn summarize_tool_input(tool_name: &str, input: &JsonObject) -> String {
    let text = |key: &str| input.get(key).and_then(Value::as_str).map(str::trim);
    let summary = match tool_name {
        // The background marker rides in front of the command so the user
        // approving the card knows this call returns immediately and the
        // command then runs detached from the round.
        "bash" | "powershell" => text("command").map(|command| {
            if input.get("run_in_background") == Some(&Value::Bool(true)) {
                format!("[后台] {command}")
            } else {
                command.to_owned()
            }
        }),
        "read" | "write" | "edit" | "delete" | "list" => text("path").map(str::to_owned),
        "search" | "grep" => match (text("pattern"), text("path")) {
            (Some(pattern), Some(path)) => Some(format!("{pattern} · {path}")),
            (Some(pattern), None) => Some(pattern.to_owned()),
            _ => None,
        },
        // Use the workflow run name rather than its script body.
        "workflow" => text("name").map(str::to_owned),
        _ => None,
    };
    let summary = summary
        .filter(|value| !value.is_empty())
        .or_else(|| most_descriptive_scalar(input))
        .unwrap_or_default();
    truncate_for_display(&escape_display_text(&summary))
}

/// Falls back to the longest string argument, then to any scalar. "Longest"
/// beats "first" because argument order is the model's, and the most
/// informative field is rarely the alphabetically first one.
fn most_descriptive_scalar(input: &JsonObject) -> Option<String> {
    input
        .values()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .max_by_key(|value| value.chars().count())
        .map(str::to_owned)
        .or_else(|| {
            input
                .values()
                .find(|value| value.is_number() || value.is_boolean())
                .map(ToString::to_string)
        })
}

fn truncate_for_display(value: &str) -> String {
    // Collapse whitespace: a multi-line command must not push the composer off
    // screen, and the card is not where a diff gets reviewed.
    let single_line = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if single_line.chars().count() <= MAX_SUMMARY_CHARS {
        return single_line;
    }
    let kept = single_line
        .chars()
        .take(MAX_SUMMARY_CHARS)
        .collect::<String>();
    format!("{kept}…")
}

/// Neutralizes the bidi and zero-width characters that could make a displayed
/// command read as something other than what will run. Mirrors
/// `escape_approval_display_text` in `lib.rs`, which guards the same class of
/// spoofing for the text the native dialog used to show.
pub(crate) fn escape_display_text(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        if matches!(
            character,
            '\u{00ad}'
                | '\u{061c}'
                | '\u{180e}'
                | '\u{200b}'..='\u{200f}'
                | '\u{2028}'..='\u{202e}'
                | '\u{2060}'..='\u{206f}'
                | '\u{feff}'
        ) {
            escaped.push_str(&format!("\\u{{{:04X}}}", character as u32));
        } else {
            escaped.push(character);
        }
    }
    escaped
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolPromptDecision {
    Deny,
    AllowOnce,
    /// Allow this tool name for the rest of this conversation. Never offered
    /// for shell or MCP tools, nor for any `mandatory_prompt` decision.
    AllowAlways,
}

impl ToolPromptDecision {
    pub fn allows(self) -> bool {
        !matches!(self, Self::Deny)
    }
}

/// Which question a card is asking. The three kinds share one registry and one
/// resolve command, but only `Tool` is about authorizing a side effect, so only
/// `Tool` participates in blanket allowances.
///
/// The renderer draws the plan kinds differently: no "always" button, and a
/// feedback box on the denial path, because "not yet, because…" is the answer
/// that keeps a planning turn going.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptKind {
    #[default]
    Tool,
    /// The model finished a plan and wants to start implementing.
    PlanExit,
    /// The model wants the conversation to enter plan mode.
    PlanEnter,
}

/// One answer to one card. `feedback` is the user's prose, which only the plan
/// cards collect; it travels with the decision so the blocked worker can put it
/// in the tool result the model reads next.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromptAnswer {
    pub decision: ToolPromptDecision,
    pub feedback: Option<String>,
}

impl PromptAnswer {
    pub fn new(decision: ToolPromptDecision) -> Self {
        Self {
            decision,
            feedback: None,
        }
    }
}

/// Normalizes renderer-supplied card feedback: trimmed, empty treated as
/// absent, and capped, because this text is quoted verbatim into the tool
/// result the model reads next.
pub fn sanitize_prompt_feedback(feedback: Option<String>) -> Option<String> {
    let feedback = feedback?;
    let trimmed = feedback.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.chars().take(MAX_FEEDBACK_CHARS).collect())
}

/// What the renderer needs to draw one card. Serialized straight to the
/// manual-path command result and mirrored field-for-field by
/// `ModelStreamEvent::ToolApprovalRequested`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PendingToolPrompt {
    pub prompt_id: String,
    pub tool_name: String,
    /// Absent in cards persisted or sent before plan mode existed, which were
    /// all ordinary tool approvals.
    #[serde(default)]
    pub kind: PromptKind,
    pub label: String,
    pub summary: String,
    pub risk_level: String,
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requester: Option<String>,
    /// Machine-readable requester address, unlike `requester` which is
    /// display-escaped text. The addressable child name (`sourceAgent`) plus
    /// the parent-timeline call id of the child's turn (`sourceCallId`); both
    /// absent for the main session's own calls. The renderer uses them to open
    /// the requesting child's page when the card arrives — the streaming view
    /// of a workflow step is keyed by the call id, not the pool name the card
    /// displays.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_agent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_call_id: Option<String>,
    pub allow_always_offered: bool,
    /// Whether this card appears regardless of the conversation's security
    /// level. `allow_always_offered` cannot answer that question: an ordinary
    /// shell or MCP call also declines blanket permission while still being an
    /// approval the level decides. Without this the user sees an identical card
    /// under full access and reasonably concludes the level is broken, when what
    /// they are looking at is a deliberate circuit breaker.
    #[serde(default)]
    pub mandatory: bool,
}

/// Why a prompt stopped waiting. Only `Answered` carries the user's intent.
#[derive(Debug)]
enum PromptOutcome {
    Answered(PromptAnswer),
    /// A flag the waiter asked to be watched went up: the run was stopped, or
    /// the task that raised the card was stopped from the sidebar.
    Cancelled,
    TimedOut,
    /// The registry entry disappeared without an answer — today only
    /// [`ToolPromptRegistry::cancel_run_prompts`] and `clear` do that.
    Retracted,
}

/// Who is blocked on a model prompt, which is what decides whether the end of a
/// turn is allowed to answer it.
///
/// A run end retracts only cards raised by that run. A detached task's card
/// remains pending until it receives an answer, its task stops, or it times out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PromptOwner {
    /// Raised on the thread of the named top-level model run.
    Run(String),
    /// Raised by a detached task worker (subagent, workflow step, workflow
    /// driver). Tasks outlive the turn that spawned them (S11), so no turn
    /// boundary retracts this card; only an answer, the task's own stop flag,
    /// or the timeout ends it.
    Task,
}

enum PendingPrompt {
    /// A blocked worker thread. The conversation is kept so one run ending
    /// cannot cancel a concurrent run's card — runs hold a shared lease, so
    /// more than one can be live at a time.
    Model {
        conversation_id: String,
        owner: PromptOwner,
        /// The card as the renderer would draw it. Held rather than rebuilt so
        /// a renderer that reloaded (or connected late) can be handed the open
        /// cards verbatim instead of leaving a worker blocked behind a card
        /// nobody can see.
        card: PendingToolPrompt,
        sender: SyncSender<PromptAnswer>,
    },
    /// A renderer-initiated tool-editor call. The classified request is held
    /// here rather than resent with the answer, so the renderer cannot widen
    /// the call between being asked about it and answering.
    Manual {
        request: Box<ToolExecutionRequest>,
        /// The risk the classifier gave this call when the card was raised. An
        /// "always" answer is recorded at this level, never at "any".
        risk_level: RiskLevel,
        opened_at: Instant,
    },
}

/// What `resolve` found. The manual arm hands the request back so the caller
/// can mint a nonce for exactly what was classified.
pub enum PromptResolution {
    Model,
    Manual {
        request: Box<ToolExecutionRequest>,
        decision: ToolPromptDecision,
    },
}

/// Identifies the conversation that owns a card for capacity and fairness.
fn prompt_conversation(prompt: &PendingPrompt) -> &str {
    match prompt {
        PendingPrompt::Model {
            conversation_id, ..
        } => conversation_id,
        PendingPrompt::Manual { request, .. } => &request.conversation_id,
    }
}

#[derive(Default)]
pub struct ToolPromptRegistry {
    pending: Mutex<HashMap<String, PendingPrompt>>,
    /// (conversation_id, tool_name) → the risk level of the call the user was
    /// looking at when they said "always". Process-local and never persisted:
    /// a restart asks again.
    ///
    /// The risk level is part of the key's meaning. An allowance may answer
    /// only calls no riskier than the one for which it was granted.
    allowances: Mutex<HashMap<(String, String), RiskLevel>>,
}

impl ToolPromptRegistry {
    /// True when this conversation already blanket-allowed this tool name **at
    /// or above** `risk_level`. Callers must not consult this for a
    /// `mandatory_prompt` decision.
    ///
    /// The risk bound is what keeps "stop asking about this class of call" from
    /// meaning "stop asking about this tool, whatever it is about to touch".
    pub fn is_always_allowed(
        &self,
        conversation_id: &str,
        tool_name: &str,
        risk_level: RiskLevel,
    ) -> bool {
        self.allowances
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&(conversation_id.to_owned(), tool_name.to_owned()))
            .is_some_and(|granted| risk_level <= *granted)
    }

    /// Records an allowance, keeping the **highest** risk explicitly granted
    /// for the pair. Widening only ever happens by the user answering a card
    /// they were shown: a High call is only ever seen because the standing
    /// Medium grant did not cover it.
    fn remember_allowance(&self, conversation_id: &str, tool_name: &str, risk_level: RiskLevel) {
        self.allowances
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .entry((conversation_id.to_owned(), tool_name.to_owned()))
            .and_modify(|granted| *granted = (*granted).max(risk_level))
            .or_insert(risk_level);
    }

    /// Registers a prompt under a fresh id. Abandoned manual prompts are
    /// evicted here, which is the only moment anything sweeps them.
    ///
    /// `build` receives the minted id because a model prompt stores the card
    /// the renderer will draw, and the card carries its own id.
    ///
    /// Capacity is per conversation: only cards in this card's conversation
    /// count toward [`MAX_PENDING_PROMPTS_PER_CONVERSATION`].
    fn register(
        &self,
        build: impl FnOnce(&str) -> PendingPrompt,
    ) -> Result<String, String> {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Instant::now();
        pending.retain(|_, entry| match entry {
            PendingPrompt::Manual { opened_at, .. } => {
                now.duration_since(*opened_at) < MANUAL_PROMPT_TTL
            }
            PendingPrompt::Model { .. } => true,
        });
        let id = loop {
            let candidate = Uuid::new_v4().to_string();
            if !pending.contains_key(&candidate) {
                break candidate;
            }
        };
        let prompt = build(&id);
        let conversation = prompt_conversation(&prompt).to_owned();
        if pending
            .values()
            .filter(|entry| prompt_conversation(entry) == conversation)
            .count()
            >= MAX_PENDING_PROMPTS_PER_CONVERSATION
        {
            return Err("Too many tool calls are awaiting confirmation; resolve existing confirmations first".into());
        }
        pending.insert(id.clone(), prompt);
        Ok(id)
    }

    fn close(&self, prompt_id: &str) {
        self.pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(prompt_id);
    }

    /// Opens a manual prompt for a renderer-initiated call and returns the id
    /// the renderer must answer with. The request is retained until then.
    pub fn open_manual(
        &self,
        request: &ToolExecutionRequest,
        risk_level: RiskLevel,
    ) -> Result<String, String> {
        self.register(|_| PendingPrompt::Manual {
            request: Box::new(request.clone()),
            risk_level,
            opened_at: Instant::now(),
        })
    }

    /// Every waiting model card, paired with the conversation it belongs to.
    pub fn all_pending_cards(&self) -> Vec<(String, PendingToolPrompt)> {
        self.pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .values()
            .filter_map(|entry| match entry {
                PendingPrompt::Model {
                    conversation_id,
                    card,
                    ..
                } => Some((conversation_id.clone(), card.clone())),
                PendingPrompt::Manual { .. } => None,
            })
            .collect()
    }

    /// Renderer-side resolution. Unknown ids are an error rather than a silent
    /// success so a double-click or a stale card cannot read as consent for
    /// whatever prompt happens to be open next.
    ///
    /// `feedback` is the prose the plan cards collect. The manual path has no
    /// card that asks for it and drops it.
    pub fn resolve(
        &self,
        prompt_id: &str,
        decision: ToolPromptDecision,
        feedback: Option<String>,
    ) -> Result<PromptResolution, String> {
        let entry = self
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(prompt_id)
            .ok_or_else(|| "This tool confirmation has ended or does not exist".to_owned())?;
        match entry {
            PendingPrompt::Model { sender, .. } => {
                // The receiver is gone only if the waiter already bailed out;
                // the prompt is over either way.
                let _ = sender.try_send(PromptAnswer { decision, feedback });
                Ok(PromptResolution::Model)
            }
            PendingPrompt::Manual {
                request, risk_level, ..
            } => {
                // A manual prompt is never mandatory — the classifier's
                // mandatory decisions all arise inside a model run — so an
                // "always" answer here is honoured, at the risk it was shown at.
                if decision == ToolPromptDecision::AllowAlways {
                    self.remember_allowance(&request.conversation_id, &request.tool_name, risk_level);
                }
                Ok(PromptResolution::Manual { request, decision })
            }
        }
    }

    /// Retracts the cards **one finished run** raised on its own thread.
    ///
    /// Scoped to `PromptOwner::Run(request_id)` on purpose. The conversation-wide
    /// version this replaced took a task's card down with the turn and caused a
    /// fabricated refusal. A task's card is retracted by its own stop flag or by an answer,
    /// never by a turn boundary. Concurrent runs, other conversations, and the
    /// renderer's own tool-editor prompts are left alone.
    pub fn cancel_run_prompts(&self, conversation_id: &str, request_id: &str) {
        self.pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .retain(|_, entry| match entry {
                PendingPrompt::Model {
                    conversation_id: owner,
                    owner: PromptOwner::Run(run),
                    ..
                } => !(owner == conversation_id && run == request_id),
                _ => true,
            });
    }

    pub fn clear(&self) {
        self.pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
        self.allowances
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
    }

    /// The boolean projection of [`ask_answer`](Self::ask_answer), for the
    /// yes/no cards whose prose is never read. Production callers go through
    /// `api::ask_announced_prompt`, which needs the prose for the plan cards.
    #[cfg(test)]
    pub fn ask(
        &self,
        conversation_id: &str,
        owner: PromptOwner,
        mandatory: bool,
        risk_level: RiskLevel,
        cancellations: &[&AtomicBool],
        card: PendingToolPrompt,
        announce: impl FnOnce(&PendingToolPrompt) -> Result<(), String>,
    ) -> Result<bool, String> {
        self.ask_answer(
            conversation_id,
            owner,
            mandatory,
            risk_level,
            cancellations,
            card,
            announce,
        )
        .map(|answer| answer.decision.allows())
    }

    /// Opens a model prompt, hands the card to `announce`, and blocks until the
    /// renderer answers, one of `cancellations` goes up, or the prompt times
    /// out.
    ///
    /// `card.prompt_id` is ignored; the registry mints it and hands `announce`
    /// the finished card.
    ///
    /// `cancellations` is a list rather than one flag because a task worker
    /// watches two things at once: the run that happens to be live (so stopping
    /// generation retracts the card) and its own task stop flag (so the sidebar
    /// stop button releases it). A background task with no live run passes just
    /// its own flag — and a card raised with no flags at all is still bounded by
    /// `PROMPT_TIMEOUT`.
    ///
    /// `announce` is what puts the card on screen. If it fails, the prompt is
    /// closed immediately rather than waiting out a card nobody can see.
    ///
    /// Only a user's `Deny` returns an answer that does not allow. Stopped,
    /// timed out, and retracted are `Err`, because callers must not attribute a
    /// refusal to the user when the user did not answer. The user's prose comes
    /// back with the decision; only the plan cards have anything to do with it.
    ///
    /// Blanket allowances are a property of authorizing a *tool*, so a card
    /// whose `kind` is not `Tool` neither consults nor records one — a user who
    /// once said "always allow" for some tool has not thereby agreed to leave
    /// plan mode, and agreeing to leave plan mode this once must not silently
    /// answer a later card.
    pub fn ask_answer(
        &self,
        conversation_id: &str,
        owner: PromptOwner,
        mandatory: bool,
        risk_level: RiskLevel,
        cancellations: &[&AtomicBool],
        card: PendingToolPrompt,
        announce: impl FnOnce(&PendingToolPrompt) -> Result<(), String>,
    ) -> Result<PromptAnswer, String> {
        let tool_name = card.tool_name.clone();
        let blanket_allowable =
            card.kind == PromptKind::Tool && !mandatory && !never_blanket_allowed(&tool_name);
        if blanket_allowable && self.is_always_allowed(conversation_id, &tool_name, risk_level) {
            return Ok(PromptAnswer::new(ToolPromptDecision::AllowAlways));
        }
        let stopped = || cancellations.iter().any(|flag| flag.load(Ordering::Acquire));
        // Asking at all is pointless once the waiter is already stopped, and it
        // would flash a card the renderer must then retract.
        if stopped() {
            return Err("This tool call was stopped before requesting confirmation".into());
        }
        let (sender, receiver) = sync_channel(1);
        let conversation = conversation_id.to_owned();
        let mut announced = None;
        let prompt_id = self.register(|id| {
            let card = PendingToolPrompt {
                prompt_id: id.to_owned(),
                ..card.clone()
            };
            announced = Some(card.clone());
            PendingPrompt::Model {
                conversation_id: conversation.clone(),
                owner,
                card,
                sender,
            }
        })?;
        let announced = announced.expect("register always runs the builder");
        if let Err(error) = announce(&announced) {
            self.close(&prompt_id);
            return Err(format!("Could not display the tool confirmation card: {error}"));
        }

        let mut waited = Duration::ZERO;
        let outcome = loop {
            if stopped() {
                break PromptOutcome::Cancelled;
            }
            match receiver.recv_timeout(CANCELLATION_POLL) {
                Ok(answer) => break PromptOutcome::Answered(answer),
                Err(RecvTimeoutError::Disconnected) => break PromptOutcome::Retracted,
                Err(RecvTimeoutError::Timeout) => {
                    waited += CANCELLATION_POLL;
                    if waited >= PROMPT_TIMEOUT {
                        break PromptOutcome::TimedOut;
                    }
                }
            }
        };
        self.close(&prompt_id);

        match outcome {
            PromptOutcome::Answered(answer) => {
                // Cancellation wins if it races with an Allow decision. Recheck
                // after receiving the decision so execution cannot proceed in a
                // cancelled run.
                if stopped() {
                    return Err("This tool call was stopped while awaiting confirmation".into());
                }
                // An allowance is only ever recorded for a decision the level,
                // not the tool, was allowed to settle.
                if blanket_allowable && answer.decision == ToolPromptDecision::AllowAlways {
                    self.remember_allowance(conversation_id, &tool_name, risk_level);
                }
                Ok(answer)
            }
            PromptOutcome::Cancelled => Err("This tool call was stopped while awaiting confirmation".into()),
            PromptOutcome::Retracted => {
                Err("This tool call's confirmation card was retracted without authorization".into())
            }
            PromptOutcome::TimedOut => Err("Tool confirmation timed out without a response; this call was denied".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Arc;
    use std::thread;

    fn registry() -> Arc<ToolPromptRegistry> {
        Arc::new(ToolPromptRegistry::default())
    }

    fn object(value: Value) -> JsonObject {
        value.as_object().cloned().unwrap()
    }

    fn manual_request(conversation: &str, tool: &str) -> ToolExecutionRequest {
        ToolExecutionRequest {
            conversation_id: conversation.into(),
            workspace_path: "/workspace".into(),
            tool_name: tool.into(),
            input: object(json!({"path": "notes.md"})),
        }
    }

    /// The display half of a model prompt. `prompt_id` is whatever the registry
    /// mints, so the placeholder here is deliberately wrong.
    fn card(tool: &str) -> PendingToolPrompt {
        PendingToolPrompt {
            prompt_id: "minted-by-the-registry".into(),
            tool_name: tool.into(),
            kind: PromptKind::Tool,
            label: tool.into(),
            summary: "notes.md".into(),
            risk_level: "中".into(),
            reason: "测试".into(),
            requester: None,
            source_agent: None,
            source_call_id: None,
            allow_always_offered: true,
            mandatory: false,
        }
    }

    /// Answers the prompt `ask` is about to open, retrying until the waiter has
    /// registered it. Returns a handle the test joins to surface panics.
    fn answer_next_prompt(
        registry: &Arc<ToolPromptRegistry>,
        decision: ToolPromptDecision,
    ) -> (SyncSender<String>, thread::JoinHandle<()>) {
        let answering = Arc::clone(registry);
        let (announce, announced) = sync_channel(1);
        let responder = thread::spawn(move || {
            let id: String = announced.recv().expect("prompt was never announced");
            let deadline = Instant::now() + Duration::from_secs(10);
            while Instant::now() < deadline {
                if answering.resolve(&id, decision, None).is_ok() {
                    return;
                }
                thread::sleep(Duration::from_millis(5));
            }
            panic!("prompt never became resolvable");
        });
        (announce, responder)
    }

    /// The whole point of the card: the worker blocks, the renderer answers,
    /// and the answer — not a default — decides.
    #[test]
    fn a_renderer_decision_unblocks_the_waiting_worker() {
        for (decision, expected) in [
            (ToolPromptDecision::Deny, false),
            (ToolPromptDecision::AllowOnce, true),
            (ToolPromptDecision::AllowAlways, true),
        ] {
            let registry = registry();
            let (announce, responder) = answer_next_prompt(&registry, decision);
            let allowed = registry
                .ask(
                    "conversation-a",
                    PromptOwner::Run("run-1".into()),
                    false,
                    RiskLevel::Medium,
                    &[],
                    card("write"),
                    |prompt| {
                        announce.send(prompt.prompt_id.clone()).unwrap();
                        Ok(())
                    },
                )
                .unwrap();
            responder.join().unwrap();
            assert_eq!(allowed, expected, "{decision:?}");
        }
    }

    /// A standing allowance covers only the same tool and conversation. Anything
    /// wider would authorize calls the user never saw.
    #[test]
    fn an_allowance_covers_only_its_own_tool_and_conversation() {
        let registry = registry();
        let (announce, responder) = answer_next_prompt(&registry, ToolPromptDecision::AllowAlways);
        registry
            .ask(
                "conversation-a",
                PromptOwner::Run("run-1".into()),
                false,
                RiskLevel::Medium,
                &[],
                card("write"),
                |prompt| {
                    announce.send(prompt.prompt_id.clone()).unwrap();
                    Ok(())
                },
            )
            .unwrap();
        responder.join().unwrap();

        assert!(registry.is_always_allowed("conversation-a", "write", RiskLevel::Medium));
        assert!(!registry.is_always_allowed("conversation-a", "bash", RiskLevel::Medium));
        assert!(!registry.is_always_allowed("conversation-b", "write", RiskLevel::Medium));

        // The allowance now answers without ever announcing a card.
        let allowed = registry
            .ask(
                "conversation-a",
                PromptOwner::Run("run-1".into()),
                false,
                RiskLevel::Medium,
                &[],
                card("write"),
                |_| panic!("an allowed tool must not open another prompt"),
            )
            .unwrap();
        assert!(allowed);
    }

    /// The user's prose is part of the answer, not a side channel: a denied
    /// plan card is only useful if what the user wants changed comes back with
    /// the refusal.
    #[test]
    fn a_plan_card_carries_the_users_prose_back_to_the_waiting_worker() {
        let registry = registry();
        let answering = Arc::clone(&registry);
        let (announce, announced) = sync_channel(1);
        let responder = thread::spawn(move || {
            let id: String = announced.recv().expect("prompt was never announced");
            let deadline = Instant::now() + Duration::from_secs(10);
            while Instant::now() < deadline {
                // Padded and over-long prose is normalized on the way in, the
                // same way the command does it for the renderer.
                let feedback = sanitize_prompt_feedback(Some(format!(
                    "  start with the storage layer{}  ",
                    "!".repeat(MAX_FEEDBACK_CHARS)
                )));
                if answering
                    .resolve(&id, ToolPromptDecision::Deny, feedback.clone())
                    .is_ok()
                {
                    return;
                }
                thread::sleep(Duration::from_millis(5));
            }
            panic!("prompt never became resolvable");
        });
        let mut exit = card("exit_plan_mode");
        exit.kind = PromptKind::PlanExit;
        exit.mandatory = true;
        let answer = registry
            .ask_answer(
                "conversation-a",
                PromptOwner::Run("run-1".into()),
                true,
                RiskLevel::Low,
                &[],
                exit,
                |prompt| {
                    assert_eq!(prompt.kind, PromptKind::PlanExit);
                    announce.send(prompt.prompt_id.clone()).unwrap();
                    Ok(())
                },
            )
            .unwrap();
        responder.join().unwrap();
        assert_eq!(answer.decision, ToolPromptDecision::Deny);
        let feedback = answer.feedback.expect("the refusal carries its reason");
        assert!(feedback.starts_with("start with the storage layer"));
        assert_eq!(feedback.chars().count(), MAX_FEEDBACK_CHARS);

        // Nothing to say is `None`, not an empty string a caller would quote.
        assert_eq!(sanitize_prompt_feedback(Some("   ".into())), None);
        assert_eq!(sanitize_prompt_feedback(None), None);
    }

    /// Blanket allowances authorize a *tool*. Saying "always allow" to some
    /// tool has not agreed to leave plan mode, and agreeing to leave plan mode
    /// this once must not answer a later card by itself.
    #[test]
    fn a_plan_card_neither_records_nor_consults_an_allowance() {
        let registry = registry();
        let plan_card = || {
            let mut exit = card("exit_plan_mode");
            exit.kind = PromptKind::PlanExit;
            exit
        };

        // `mandatory` is false here so the only thing keeping the allowance
        // machinery away is the card's kind.
        let (announce, responder) = answer_next_prompt(&registry, ToolPromptDecision::AllowAlways);
        registry
            .ask_answer(
                "conversation-a",
                PromptOwner::Run("run-1".into()),
                false,
                RiskLevel::Low,
                &[],
                plan_card(),
                |prompt| {
                    announce.send(prompt.prompt_id.clone()).unwrap();
                    Ok(())
                },
            )
            .unwrap();
        responder.join().unwrap();
        assert!(!registry.is_always_allowed("conversation-a", "exit_plan_mode", RiskLevel::Low));

        // Even with an allowance recorded for that name by an ordinary tool
        // card, the plan card still goes to the user.
        let (announce, responder) = answer_next_prompt(&registry, ToolPromptDecision::AllowAlways);
        let mut ordinary = card("exit_plan_mode");
        ordinary.kind = PromptKind::Tool;
        registry
            .ask(
                "conversation-a",
                PromptOwner::Run("run-1".into()),
                false,
                RiskLevel::Low,
                &[],
                ordinary,
                |prompt| {
                    announce.send(prompt.prompt_id.clone()).unwrap();
                    Ok(())
                },
            )
            .unwrap();
        responder.join().unwrap();
        assert!(registry.is_always_allowed("conversation-a", "exit_plan_mode", RiskLevel::Low));

        let (announce, responder) = answer_next_prompt(&registry, ToolPromptDecision::Deny);
        let answer = registry
            .ask_answer(
                "conversation-a",
                PromptOwner::Run("run-1".into()),
                false,
                RiskLevel::Low,
                &[],
                plan_card(),
                |prompt| {
                    announce.send(prompt.prompt_id.clone()).unwrap();
                    Ok(())
                },
            )
            .unwrap();
        responder.join().unwrap();
        assert_eq!(answer.decision, ToolPromptDecision::Deny);
    }

    /// A standing allowance covers a call class, not every use of that tool.
    /// A Medium in-workspace `write` allowance cannot answer a High `write`
    /// outside the workspace or application data directory.
    #[test]
    fn an_allowance_does_not_answer_a_riskier_call_of_the_same_tool() {
        let registry = registry();
        let (announce, responder) = answer_next_prompt(&registry, ToolPromptDecision::AllowAlways);
        registry
            .ask(
                "conversation-a",
                PromptOwner::Run("run-1".into()),
                false,
                RiskLevel::Medium,
                &[],
                card("write"),
                |prompt| {
                    announce.send(prompt.prompt_id.clone()).unwrap();
                    Ok(())
                },
            )
            .unwrap();
        responder.join().unwrap();

        // An allowance at the same or lower risk does not ask again.
        assert!(registry.is_always_allowed("conversation-a", "write", RiskLevel::Medium));
        assert!(registry.is_always_allowed("conversation-a", "write", RiskLevel::Low));
        // A higher-risk call must ask again.
        assert!(!registry.is_always_allowed("conversation-a", "write", RiskLevel::High));

        let (announce, responder) = answer_next_prompt(&registry, ToolPromptDecision::Deny);
        let mut asked = false;
        let allowed = registry
            .ask(
                "conversation-a",
                PromptOwner::Run("run-1".into()),
                false,
                RiskLevel::High,
                &[],
                card("write"),
                |prompt| {
                    asked = true;
                    announce.send(prompt.prompt_id.clone()).unwrap();
                    Ok(())
                },
            )
            .unwrap();
        responder.join().unwrap();
        assert!(asked, "高风险的同名调用必须重新立卡");
        assert!(!allowed);

        // The allowance expands to High only after the user allows the High-risk card.
        let (announce, responder) = answer_next_prompt(&registry, ToolPromptDecision::AllowAlways);
        registry
            .ask(
                "conversation-a",
                PromptOwner::Run("run-1".into()),
                false,
                RiskLevel::High,
                &[],
                card("write"),
                |prompt| {
                    announce.send(prompt.prompt_id.clone()).unwrap();
                    Ok(())
                },
            )
            .unwrap();
        responder.join().unwrap();
        assert!(registry.is_always_allowed("conversation-a", "write", RiskLevel::High));
    }

    /// `playwright` has an unconditional human gate for signed-in tab takeover.
    /// Since it uses the same `ask` path, a standing allowance must never answer it.
    #[test]
    fn playwright_never_carries_a_standing_allowance() {
        let registry = registry();
        let (announce, responder) = answer_next_prompt(&registry, ToolPromptDecision::AllowAlways);
        registry
            .ask(
                "conversation-a",
                PromptOwner::Run("run-1".into()),
                false,
                RiskLevel::High,
                &[],
                card("playwright"),
                |prompt| {
                    announce.send(prompt.prompt_id.clone()).unwrap();
                    Ok(())
                },
            )
            .unwrap();
        responder.join().unwrap();

        // An AllowAlways decision is never persisted for this tool.
        assert!(!registry.is_always_allowed("conversation-a", "playwright", RiskLevel::High));
        let (announce, responder) = answer_next_prompt(&registry, ToolPromptDecision::Deny);
        let mut asked = false;
        let allowed = registry
            .ask(
                "conversation-a",
                PromptOwner::Run("run-1".into()),
                false,
                RiskLevel::High,
                &[],
                card("playwright"),
                |prompt| {
                    asked = true;
                    announce.send(prompt.prompt_id.clone()).unwrap();
                    Ok(())
                },
            )
            .unwrap();
        responder.join().unwrap();
        assert!(asked, "The takeover gate must ask every time");
        assert!(!allowed);
        // Shell tools follow the same rule.
        assert!(never_blanket_allowed("bash"));
        assert!(never_blanket_allowed("powershell"));
        assert!(!never_blanket_allowed("write"));
    }

    /// A `mandatory_prompt` decision neither consumes nor records an allowance:
    /// global memory mutation asks every time, even in a conversation that
    /// blanket-allowed the same tool name.
    #[test]
    fn a_mandatory_prompt_ignores_allowances_in_both_directions() {
        let registry = registry();
        registry.remember_allowance("conversation-a", "create_global_memory", RiskLevel::High);
        let (announce, responder) = answer_next_prompt(&registry, ToolPromptDecision::AllowAlways);

        let mut announced_once = false;
        let allowed = registry
            .ask(
                "conversation-a",
                PromptOwner::Run("run-1".into()),
                true,
                RiskLevel::High,
                &[],
                card("create_global_memory"),
                |prompt| {
                    announced_once = true;
                    announce.send(prompt.prompt_id.clone()).unwrap();
                    Ok(())
                },
            )
            .unwrap();
        responder.join().unwrap();
        assert!(announced_once, "强制确认必须仍然弹卡片");
        assert!(allowed);

        // And an "always" answer to a mandatory prompt is spent, not stored:
        // once the pre-seeded allowance is cleared, nothing rewrote it.
        registry.clear();
        assert!(!registry.is_always_allowed("conversation-a", "create_global_memory", RiskLevel::High));
    }

    /// A stopped run must release its worker instead of holding it until the
    /// prompt timeout — and the release is an error, not a denial. A denial
    /// must never be reported for a card the user never saw.
    #[test]
    fn cancellation_releases_the_waiter_without_claiming_the_user_refused() {
        let registry = registry();
        let cancellation = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&cancellation);
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(60));
            flag.store(true, Ordering::Release);
        });

        let started = Instant::now();
        let error = registry
            .ask(
                "conversation-a",
                PromptOwner::Run("run-1".into()),
                false,
                RiskLevel::Medium,
                &[cancellation.as_ref()],
                card("write"),
                |_| Ok(()),
            )
            .unwrap_err();
        assert!(error.contains("was stopped"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    /// A task worker watches two flags at once. Stopping the task alone — the
    /// sidebar stop button, with no run in sight — must release the card, or a
    /// background step would sit behind an unanswered prompt for half an hour.
    #[test]
    fn a_task_stop_flag_releases_a_card_raised_with_no_run() {
        let registry = registry();
        let task_stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&task_stop);
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(60));
            flag.store(true, Ordering::Release);
        });

        let started = Instant::now();
        let error = registry
            .ask(
                "conversation-a",
                PromptOwner::Task,
                false,
                RiskLevel::Medium,
                &[task_stop.as_ref()],
                card("write"),
                |_| Ok(()),
            )
            .unwrap_err();
        assert!(error.contains("was stopped"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    /// Cancellation wins when it races with Allow. The waiter is already in a
    /// receive slice when cancellation is set and the renderer sends Allow, so
    /// the post-answer cancellation check must reject execution.
    #[test]
    fn allow_arriving_after_cancellation_is_refused() {
        let registry = registry();
        let cancellation = Arc::new(AtomicBool::new(false));
        let (announce, announced) = sync_channel(1);
        let asker = Arc::clone(&registry);
        let asker_cancellation = Arc::clone(&cancellation);
        let run = thread::spawn(move || {
            asker.ask(
                "conversation-a",
                PromptOwner::Run("run-1".into()),
                false,
                RiskLevel::Medium,
                &[asker_cancellation.as_ref()],
                card("write"),
                |prompt| {
                    announce.send(prompt.prompt_id.clone()).unwrap();
                    Ok(())
                },
            )
        });
        let prompt_id = announced.recv().unwrap();
        // Let the waiter enter a recv_timeout slice after its initial flag check.
        thread::sleep(Duration::from_millis(40));
        cancellation.store(true, Ordering::Release);
        registry
            .resolve(&prompt_id, ToolPromptDecision::AllowOnce, None)
            .unwrap();
        let outcome = run.join().unwrap();
        assert!(
            outcome.is_err(),
            "an Allow racing a cancelled run must not pass: {outcome:?}"
        );
    }

    /// Ending a run retracts the cards **that run** raised and nothing else.
    ///
    /// The three bystanders are the whole point: the renderer's own tool-editor
    /// prompt, a concurrent run in another conversation, and — the one this
    /// scoping was introduced for — a detached task's card in the *same*
    /// conversation. A background workflow step outlives the turn, so a turn
    /// boundary answering for it is a fabricated rejection.
    #[test]
    fn ending_a_run_retracts_only_the_cards_that_run_raised() {
        let registry = registry();
        let manual = registry
            .open_manual(&manual_request("conversation-a", "read"), RiskLevel::Low)
            .unwrap();

        // A second conversation is mid-prompt the whole time.
        let bystander = Arc::clone(&registry);
        let (bystander_announce, bystander_announced) = sync_channel(1);
        let bystander_run = thread::spawn(move || {
            bystander
                .ask(
                    "conversation-b",
                    PromptOwner::Run("run-2".into()),
                    false,
                    RiskLevel::Medium,
                    &[],
                    card("write"),
                    |prompt| {
                        bystander_announce.send(prompt.prompt_id.clone()).unwrap();
                        Ok(())
                    },
                )
                .unwrap()
        });
        let bystander_id: String = bystander_announced.recv().unwrap();

        // A background task in the SAME conversation as the run that is about
        // to end. Its card must survive.
        let task = Arc::clone(&registry);
        let (task_announce, task_announced) = sync_channel(1);
        let task_run = thread::spawn(move || {
            task.ask(
                "conversation-a",
                PromptOwner::Task,
                false,
                RiskLevel::Medium,
                &[],
                card("write"),
                |prompt| {
                    task_announce.send(prompt.prompt_id.clone()).unwrap();
                    Ok(())
                },
            )
            .unwrap()
        });
        let task_id: String = task_announced.recv().unwrap();

        let cancelling = Arc::clone(&registry);
        let (announce, announced) = sync_channel(1);
        let canceller = thread::spawn(move || {
            let _: String = announced.recv().unwrap();
            thread::sleep(Duration::from_millis(30));
            cancelling.cancel_run_prompts("conversation-a", "run-1");
        });
        let outcome = registry.ask(
            "conversation-a",
            PromptOwner::Run("run-1".into()),
            false,
            RiskLevel::Medium,
            &[],
            card("write"),
            |prompt| {
                announce.send(prompt.prompt_id.clone()).unwrap();
                Ok(())
            },
        );
        canceller.join().unwrap();
        assert!(
            outcome.is_err(),
            "撤回的确认不是「用户拒绝」，必须报错：{outcome:?}"
        );

        // The task's card in the same conversation is untouched and still
        // answerable — this is the regression the scoping exists for.
        assert!(registry
            .resolve(&task_id, ToolPromptDecision::AllowOnce, None)
            .is_ok());
        assert!(task_run.join().unwrap());

        // The other conversation's card is still answerable and still blocking.
        assert!(registry
            .resolve(&bystander_id, ToolPromptDecision::AllowOnce, None)
            .is_ok());
        assert!(bystander_run.join().unwrap());

        // The manual prompt survived and still carries its classified request.
        match registry
            .resolve(&manual, ToolPromptDecision::AllowOnce, None)
            .unwrap()
        {
            PromptResolution::Manual { request, decision } => {
                assert_eq!(request.tool_name, "read");
                assert_eq!(decision, ToolPromptDecision::AllowOnce);
            }
            PromptResolution::Model => panic!("manual prompt resolved as a model prompt"),
        }
    }

    /// A renderer that reloaded has to be able to get the open cards back.
    /// Without this a task's card — which no longer dies with the turn — would
    /// be invisible until the 30-minute timeout.
    #[test]
    fn pending_cards_are_listable_for_a_renderer_that_reloaded() {
        let registry = registry();
        let asker = Arc::clone(&registry);
        let (announce, announced) = sync_channel(1);
        let waiting = thread::spawn(move || {
            asker.ask(
                "conversation-a",
                PromptOwner::Task,
                false,
                RiskLevel::Medium,
                &[],
                PendingToolPrompt {
                    requester: Some("ws1".into()),
                    ..card("write")
                },
                |prompt| {
                    announce.send(prompt.prompt_id.clone()).unwrap();
                    Ok(())
                },
            )
        });
        let prompt_id: String = announced.recv().unwrap();

        let listed = registry.all_pending_cards();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].0, "conversation-a");
        assert_eq!(listed[0].1.prompt_id, prompt_id);
        assert_eq!(listed[0].1.tool_name, "write");
        assert_eq!(listed[0].1.requester.as_deref(), Some("ws1"));

        // Manual prompts are the renderer's own business and never listed here.
        registry
            .open_manual(&manual_request("conversation-a", "read"), RiskLevel::Low)
            .unwrap();
        assert_eq!(registry.all_pending_cards().len(), 1);

        registry
            .resolve(&prompt_id, ToolPromptDecision::AllowOnce, None)
            .unwrap();
        assert!(waiting.join().unwrap().unwrap());
        assert!(registry.all_pending_cards().is_empty());
    }

    /// A prompt that cannot be shown is an error, not a silent allow, and it
    /// must not leave a registration behind.
    #[test]
    fn an_unannounceable_prompt_fails_closed_and_leaves_nothing_pending() {
        let registry = registry();
        let error = registry
            .ask(
                "conversation-a",
                PromptOwner::Run("run-1".into()),
                false,
                RiskLevel::Medium,
                &[],
                card("write"),
                |_| Err("通道已关闭".into()),
            )
            .unwrap_err();
        assert!(error.contains("通道已关闭"), "{error}");
        assert!(registry
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_empty());
    }

    /// Resolving twice, or resolving an id that was never opened, is an error.
    /// A stale card must not be able to answer whatever prompt is open next.
    #[test]
    fn a_prompt_id_is_single_use() {
        let registry = registry();
        assert!(registry
            .resolve("never-opened", ToolPromptDecision::AllowOnce, None)
            .is_err());

        let id = registry
            .open_manual(&manual_request("conversation-a", "read"), RiskLevel::Low)
            .unwrap();
        assert!(registry.resolve(&id, ToolPromptDecision::AllowOnce, None).is_ok());
        assert!(registry.resolve(&id, ToolPromptDecision::AllowOnce, None).is_err());
    }

    /// The renderer may not swap the arguments between being asked about a
    /// call and answering: the request it is granted is the one that was
    /// classified, because the renderer never resends it.
    #[test]
    fn a_manual_grant_returns_the_request_that_was_classified() {
        let registry = registry();
        let mut original = manual_request("conversation-a", "write");
        original.input = object(json!({"path": "notes.md", "content": "original"}));
        let id = registry.open_manual(&original, RiskLevel::Medium).unwrap();

        match registry.resolve(&id, ToolPromptDecision::AllowOnce, None).unwrap() {
            PromptResolution::Manual { request, .. } => assert_eq!(*request, original),
            PromptResolution::Model => panic!("manual prompt resolved as a model prompt"),
        }
    }

    /// The card describes the call in the caller's own terms rather than
    /// dumping JSON, and it never trusts the argument text to render safely.
    #[test]
    fn a_summary_names_the_operation_and_neutralizes_display_spoofing() {
        assert_eq!(
            summarize_tool_input("bash", &object(json!({"command": "rm -rf build"}))),
            "rm -rf build"
        );
        assert_eq!(
            summarize_tool_input("write", &object(json!({"path": "src/main.rs", "content": "x"}))),
            "src/main.rs"
        );
        assert_eq!(
            summarize_tool_input("workflow", &object(json!({"scriptName": "audit", "scriptBytes": 91}))),
            "audit"
        );
        // Unknown tools still say something: the longest string argument.
        assert_eq!(
            summarize_tool_input("mcp__notes__append", &object(json!({"id": "n1", "body": "a longer body"}))),
            "a longer body"
        );

        // A right-to-left override could otherwise make a displayed command
        // read as the reverse of what runs.
        let spoofed = summarize_tool_input(
            "bash",
            &object(json!({"command": "echo safe\u{202e}dangerous"})),
        );
        assert!(spoofed.contains("\\u{202E}"), "{spoofed}");
        assert!(!spoofed.contains('\u{202e}'), "{spoofed}");
    }

    /// A card is a line above the composer. A pasted file has to be cut down
    /// to fit, and multi-line input must not push the composer off screen.
    #[test]
    fn a_long_or_multiline_summary_is_collapsed_to_one_bounded_line() {
        let long = "x".repeat(MAX_SUMMARY_CHARS * 3);
        let summary = summarize_tool_input("bash", &object(json!({"command": long})));
        assert_eq!(summary.chars().count(), MAX_SUMMARY_CHARS + 1);
        assert!(summary.ends_with('…'));

        let multiline = summarize_tool_input(
            "bash",
            &object(json!({"command": "first\n  second\n\tthird"})),
        );
        assert_eq!(multiline, "first second third");
    }

    /// The user's rule, in one assertion: a shell call is never blanket-allowed.
    #[test]
    fn both_shell_tools_are_excluded_from_blanket_allowance() {
        assert!(is_shell_tool("bash"));
        assert!(is_shell_tool("powershell"));
        assert!(!is_shell_tool("write"));
        assert!(!is_shell_tool("playwright"));
    }

    /// Prompt capacity is per conversation: a saturated conversation blocks
    /// only its own cards, not a first approval in another conversation.
    #[test]
    fn one_conversations_full_prompt_quota_does_not_block_another_conversation() {
        let registry = registry();
        for _ in 0..MAX_PENDING_PROMPTS_PER_CONVERSATION {
            registry
                .open_manual(&manual_request("conversation-noisy", "write"), RiskLevel::Medium)
                .expect("额度内的卡照常立起");
        }
        assert!(
            registry
                .open_manual(&manual_request("conversation-noisy", "write"), RiskLevel::Medium)
                .is_err(),
            "洪峰会话自己的额度已满"
        );
        assert!(
            registry
                .open_manual(&manual_request("conversation-quiet", "write"), RiskLevel::Medium)
                .is_ok(),
            "另一个会话的第一张卡不受洪峰会话额度的牵连"
        );
    }
}
