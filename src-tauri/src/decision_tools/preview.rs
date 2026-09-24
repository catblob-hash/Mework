//! The decision model over the conversation's preview page: `preview_find_logs`; the scored
//! `query`/`threshold` form of `preview_console_logs` and `preview_snapshot`; and the chosen
//! `query` form of `preview_click`, `preview_fill` and `preview_inspect`, which asks the model to
//! pick one element or "none of the above" (see [`super::decision_parameters`]).
//!
//! All of it runs **host-side**, outside the page's automation lock. `dispatch_preview_page_tool`
//! runs a page tool on its own thread under a 30 s wall clock while holding that lock, and a
//! scoring pass is dozens of network round trips: running it there would hold the page for the
//! whole pass and blow the clock. So the page is touched only for short reads — the element
//! lines, one element's selector, the console buffer — the decision model is consulted between
//! them, and the action itself goes back to `preview_click` / `preview_fill` / `preview_inspect`
//! through the ordinary dispatcher, carrying the selector of the element the model chose.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::Path;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use crate::browser::{
    render_ax_line, AxLine, BrowserRuntime, BrowserToolGrants, PreviewTool, PreviewToolOutput,
    PREVIEW_ELEMENT_LINES_CAP,
};
use crate::cancel::CancelSignal;
use crate::decision_model::chunk::{chunk_lines, group_entries};
use crate::decision_model::jev::{
    ChoiceAnswer, ELEMENT_RUBRIC, MAX_CHOICE_OPTIONS, RELEVANCE_RUBRIC,
};
use crate::decision_model::search::{render_report, search, Candidate, Scorer, SearchReport};
use crate::decision_model::{parse_query, parse_threshold, DecisionError};
use crate::model::{JsonObject, ToolExecutionRequest};
use crate::state::AppState;
use crate::tool_executor::{
    dispatch_preview_page_tool, first_running_preview_server, preview_server_for_session,
    resolve_preview_page_session, Outcome,
};

use super::files::{chunk_candidates, refine_line_chunk};
use super::{failure, jev_client, jev_scorer};

/// Wall clock one page read gets. The same 30 s `dispatch_preview_page_tool` gives a page tool,
/// kept here as its own constant because that one is private to the executor and because these
/// reads are not dispatched through it.
const PAGE_READ_TIMEOUT: Duration = Duration::from_secs(30);

/// Coarse scoring unit for page elements: a run of consecutive snapshot lines, and the cap on
/// how many runs one call scores. Both mirror the line chunker's shape for text.
const ELEMENTS_PER_RUN: usize = 12;
const MAX_ELEMENT_RUNS: usize = 48;

/// Element lines one `choice` question may offer, with the last option reserved for `none`.
const MAX_ELEMENT_CHOICES: usize = MAX_CHOICE_OPTIONS - 1;

const CHOICE_INSTRUCTIONS: &str = "Which element in `elements` is the one that `description` \
refers to? Every element line starts with its uid in square brackets; answer with that uid. \
Answer none when no element on the page is the described one.";
/// The way out the host always appends to an element choice, after every element it offers, so
/// the model is never forced to name an element the description does not fit.
const NO_MATCH_OPTION: &str = "None of the above: no element on the page matches the description";
const NO_MATCH_CHOICE: &str = "none";

const NO_ELEMENTS: &str =
    "No accessible content found on the preview page; there is nothing to score.";
const NO_LOG_LINES: &str = "There are no console or server log lines to score.";
const NO_CONSOLE_LINES: &str = "There are no console log lines to score.";
/// A dev server is optional material for `preview_find_logs`, so its absence is reported in the
/// answer rather than raised — unless the call asked for the server logs and nothing else.
const NO_RUNNING_SERVER: &str =
    "No dev server is running for this workspace, so only the page console was scored.";
const NO_SERVER_LOGS: &str = "No dev server is running for this workspace, so there are no server \
logs to score. Start one with preview_start, or search the page console with source \"console\".";

/// The most console entries `lines` can ask for: the page keeps no more than this many, and the
/// plain listing clamps to the same number.
const MAX_CONSOLE_LINES: u64 = 200;

const SELECTOR_UNAVAILABLE: &str =
    "unavailable (the element is not reachable by CSS; use preview_snapshot)";

// ----------------------------------------------------------------- Entry points

/// `preview_snapshot` with `query`: which elements of the page a description points at, each with
/// the selector that addresses it, instead of the whole snapshot.
fn find_element(
    request: &ToolExecutionRequest,
    state: &AppState,
    workspace: &Path,
    cancel: &CancelSignal,
) -> Result<Outcome, String> {
    let query = parse_query(&request.input, "query")?;
    let threshold = parse_threshold(&request.input)?;
    let session_id = resolve_preview_page_session(request, state, workspace)?;
    // The key is read before the page is touched: a call that cannot score anything should not
    // cost a page read, and the hint the model relays must not arrive behind a page error.
    let scorer = jev_scorer(ELEMENT_RUBRIC)?;
    let (lines, capped) = element_lines(state, &session_id, PreviewTool::Snapshot)?;
    if lines.is_empty() {
        return Ok(Outcome::success(NO_ELEMENTS.to_owned()));
    }
    let rendered = render_lines(&lines);
    let (coarse, by_label) = element_runs(&lines, &rendered);
    let report = search(
        scorer,
        &query,
        threshold,
        coarse,
        |candidate| refine_element_run(&lines, &rendered, &by_label, candidate),
        cancel,
    )
    .map_err(failure)?;
    Ok(Outcome::success(render_element_report(
        &report,
        &query,
        threshold,
        &lines,
        capped,
        |line| selector_for(state, &session_id, PreviewTool::Snapshot, line),
    )))
}

/// `preview_find_logs`: which console and dev-server log lines say what the query asks about.
pub(crate) fn find_logs(
    request: &ToolExecutionRequest,
    state: &AppState,
    workspace: &Path,
    cancel: &CancelSignal,
) -> Result<Outcome, String> {
    let query = parse_query(&request.input, "query")?;
    let threshold = parse_threshold(&request.input)?;
    let source = parse_source(&request.input)?;
    let session_id = resolve_preview_page_session(request, state, workspace)?;
    // As in `find_element`: the key before the page, so a call that cannot score anything says so
    // instead of reading a console first.
    let scorer = jev_scorer(RELEVANCE_RUBRIC)?;

    let mut notes = Vec::new();
    let mut server = Vec::new();
    if source.reads_server() {
        match server_for_logs(request, state, workspace) {
            Some(server_id) => server = server_log_lines(state, &server_id),
            None if source == LogSource::Server => return Err(NO_SERVER_LOGS.to_owned()),
            None => notes.push(NO_RUNNING_SERVER.to_owned()),
        }
    }
    let console = if source.reads_console() {
        console_lines(state, &session_id, None)?
    } else {
        Vec::new()
    };

    let console_plan = chunk_lines(&console.join("\n"));
    let server_plan = chunk_lines(&server.join("\n"));
    let (mut coarse, mut by_label) = chunk_candidates("console", &console_plan.chunks);
    let (server_coarse, server_by_label) = chunk_candidates("server", &server_plan.chunks);
    coarse.extend(server_coarse);
    by_label.extend(server_by_label);

    let mut text = vec![scored_lines_line(console.len(), server.len())];
    text.append(&mut notes);
    if coarse.is_empty() {
        text.push(NO_LOG_LINES.to_owned());
        return Ok(Outcome::success(text.join("\n")));
    }
    let report = search(
        scorer,
        &query,
        threshold,
        coarse,
        |candidate| refine_line_chunk(label_source(&candidate.label), &by_label, candidate),
        cancel,
    )
    .map_err(failure)?;
    text.push(render_report(&report, &query, threshold, "chunks of logs", true));
    Ok(Outcome::success(text.join("\n")))
}

/// `preview_console_logs` with `query`: which of the console entries that pass `level` say what
/// the query asks about.
///
/// `level` filters exactly as the plain listing's does, so the only difference is where the
/// entries go. `lines` narrows them to the most recent ones; without it every entry that passes
/// the filter is scored, because the point of asking the decision model is not having to guess
/// how far back the answer is.
fn score_console_logs(
    request: &ToolExecutionRequest,
    state: &AppState,
    workspace: &Path,
    cancel: &CancelSignal,
) -> Result<Outcome, String> {
    let query = parse_query(&request.input, "query")?;
    let threshold = parse_threshold(&request.input)?;
    let level = parse_console_level(&request.input)?;
    let limit = parse_console_lines(&request.input)?;
    let session_id = resolve_preview_page_session(request, state, workspace)?;
    let scorer = jev_scorer(RELEVANCE_RUBRIC)?;

    let mut console = console_lines(state, &session_id, level)?;
    if let Some(limit) = limit {
        let skip = console.len().saturating_sub(limit);
        console.drain(..skip);
    }
    let plan = chunk_lines(&console.join("\n"));
    let (coarse, by_label) = chunk_candidates("console", &plan.chunks);
    let mut text = vec![scored_console_line(console.len())];
    if coarse.is_empty() {
        text.push(NO_CONSOLE_LINES.to_owned());
        return Ok(Outcome::success(text.join("\n")));
    }
    let report = search(
        scorer,
        &query,
        threshold,
        coarse,
        |candidate| refine_line_chunk("console", &by_label, candidate),
        cancel,
    )
    .map_err(failure)?;
    text.push(render_report(&report, &query, threshold, "chunks of console logs", true));
    Ok(Outcome::success(text.join("\n")))
}

/// The decision-model form one decision-parameter tool's call runs as, for the executor's single
/// arm over [`super::DECISION_PARAMETER_TOOLS`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Decision {
    ConsoleLogs,
    Snapshot,
    Act(DescribedAction),
}

impl Decision {
    pub(crate) fn of(tool: &str) -> Option<Self> {
        match tool {
            "preview_console_logs" => Some(Self::ConsoleLogs),
            "preview_snapshot" => Some(Self::Snapshot),
            "preview_click" => Some(Self::Act(DescribedAction::Click)),
            "preview_fill" => Some(Self::Act(DescribedAction::Fill)),
            "preview_inspect" => Some(Self::Act(DescribedAction::Inspect)),
            _ => None,
        }
    }

    /// `score_misses` is whether this run scores the element lines behind a "none of the above"
    /// (`RunModelRequest::decision_miss_scoring`); only the choice forms can miss, so the
    /// scoring forms ignore it.
    pub(crate) fn run(
        self,
        request: &ToolExecutionRequest,
        state: &AppState,
        workspace: &Path,
        cancel: &CancelSignal,
        score_misses: bool,
    ) -> Result<Outcome, String> {
        match self {
            Self::ConsoleLogs => score_console_logs(request, state, workspace, cancel),
            Self::Snapshot => find_element(request, state, workspace, cancel),
            Self::Act(action) => {
                act_by_query(request, state, workspace, cancel, action, score_misses)
            }
        }
    }
}

// ----------------------------------------------------------------- The choice forms

/// Which base tool a `query` call ends in, and what it carries over to it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DescribedAction {
    Click,
    Fill,
    Inspect,
}

impl DescribedAction {
    fn tool(self) -> PreviewTool {
        match self {
            Self::Click => PreviewTool::Click,
            Self::Fill => PreviewTool::Fill,
            Self::Inspect => PreviewTool::Inspect,
        }
    }

    fn name(self) -> &'static str {
        self.tool().as_str()
    }

    /// The base tool's own parameters, handed over untouched.
    fn pass_through(self) -> &'static [&'static str] {
        match self {
            Self::Click => &["doubleClick"],
            Self::Fill => &["value"],
            Self::Inspect => &["styles"],
        }
    }

    /// Whether "no element matched" is a failure. Reading an element the page does not have is
    /// the ordinary answer `preview_inspect` gives; clicking or filling nothing is not.
    fn misses_are_errors(self) -> bool {
        self != Self::Inspect
    }
}

/// Lists the page's elements, asks the decision model which one `query` names — or "none of the
/// above", the option the host always appends — and hands that element's selector to the base
/// tool.
///
/// This is a choice, not a score: there is no threshold to cut at. "None of the above" is how the
/// model declines, and it is a miss reported with the elements it nearly chose and every element
/// line it was shown, so the caller can sharpen the query or fall back to a selector without
/// reading the page again. With `score_misses` those lines are first scored against the
/// description one by one and listed highest first — a second opinion on the same material, from
/// a question that cannot decline.
fn act_by_query(
    request: &ToolExecutionRequest,
    state: &AppState,
    workspace: &Path,
    cancel: &CancelSignal,
    action: DescribedAction,
    score_misses: bool,
) -> Result<Outcome, String> {
    let description = parse_query(&request.input, "query")?;
    if action == DescribedAction::Fill && !request.input.contains_key("value") {
        return Err("Missing parameter value".to_owned());
    }
    let session_id = resolve_preview_page_session(request, state, workspace)?;
    let jev = jev_client()?;
    let (lines, capped) = element_lines(state, &session_id, action.tool())?;
    let candidates = narrow_candidates(&lines, &description, cancel)?;
    let shown = Shown::new(&candidates, lines.len(), capped);
    if candidates.is_empty() {
        return missed(action, &description, None, &shown, &MissListing::Plain)
            .map(Outcome::success);
    }
    let options = choice_options(&candidates);
    let answer = jev
        .choose(
            json!({
                "description": description,
                "elements": shown.text(),
            }),
            CHOICE_INSTRUCTIONS,
            &options,
        )
        .map_err(failure)?;
    let by_uid = uid_index(&candidates);
    let Some(chosen) = answer
        .choice
        .parse::<u64>()
        .ok()
        .and_then(|uid| by_uid.get(&uid).copied())
    else {
        let listing = if score_misses {
            score_shown(jev_scorer(ELEMENT_RUBRIC), &shown, &description, cancel)?
        } else {
            MissListing::Plain
        };
        return missed(action, &description, Some(&answer), &shown, &listing)
            .map(Outcome::success);
    };
    let selector = chosen
        .backend_node_id
        .map(|backend_node_id| {
            selector_for_blocking(state, &session_id, action.tool(), backend_node_id)
        })
        .transpose()?
        .flatten()
        .ok_or_else(|| {
            format!(
                "The described element {} is not reachable by a CSS selector, so {} cannot act on \
                 it. Use preview_snapshot and a selector you build yourself.",
                render_ax_line(chosen, false),
                action.name()
            )
        })?;

    let mut input = JsonObject::new();
    input.insert("selector".to_owned(), Value::String(selector.clone()));
    for key in action.pass_through() {
        if let Some(value) = request.input.get(*key) {
            input.insert((*key).to_owned(), value.clone());
        }
    }
    if let Some(server_id) = request.input.get("serverId") {
        input.insert("serverId".to_owned(), server_id.clone());
    }
    let output = dispatch_preview_page_tool(
        state,
        &session_id,
        action.tool(),
        &input,
        BrowserToolGrants::default(),
    )?;
    let PreviewToolOutput::Text(text) = output else {
        return Err(format!("{} answered with pixels instead of text", action.name()));
    };
    Ok(Outcome::success(format!(
        "{}\n{text}",
        chose_line(chosen, &answer, &by_uid, &selector)
    )))
}

/// What one choice question showed the decision model, kept so that a miss can hand the very same
/// element lines back: the caller reads what the model read, and can judge the page or take its
/// next step without another snapshot.
struct Shown<'a> {
    /// The element lines the question offered, in page order.
    elements: &'a [AxLine],
    /// Those lines exactly as the question's `elements` carried them, one entry per line.
    rendered: Vec<String>,
    /// How many element lines the page read gave before they were narrowed to fit the question.
    page_elements: usize,
    /// Whether that read stopped at [`PREVIEW_ELEMENT_LINES_CAP`].
    capped: bool,
}

impl<'a> Shown<'a> {
    fn new(elements: &'a [AxLine], page_elements: usize, capped: bool) -> Self {
        Self {
            elements,
            rendered: render_lines(elements),
            page_elements,
            capped,
        }
    }

    /// The question's `elements`.
    fn text(&self) -> String {
        self.rendered.join("\n")
    }

    fn narrowed(&self) -> bool {
        self.elements.len() < self.page_elements
    }

    /// Which lines the decision model chose among — all of the page, or the shortlist a long page
    /// was narrowed to — with no punctuation after it, so each listing ends it in its own way.
    fn header(&self) -> String {
        let count = self.elements.len();
        if self.narrowed() {
            format!(
                "The page has {} element lines; the decision model was shown the {count} in the \
                 runs that scored best against the description",
                self.page_elements
            )
        } else if count == 1 {
            "The decision model was shown the page's only element line".to_owned()
        } else {
            format!("The decision model was shown all {count} element lines of the page")
        }
    }

    /// The header, the note on a read that stopped short, and then `lines`.
    fn listing(&self, header: String, lines: impl IntoIterator<Item = String>) -> String {
        let mut text = vec![header];
        if self.capped {
            text.push(format!(
                "(Only the first {PREVIEW_ELEMENT_LINES_CAP} element lines of the page were read; \
                 the page has more.)"
            ));
        }
        text.extend(lines);
        text.join("\n")
    }

    /// The lines the decision model chose among, as it read them.
    fn render(&self) -> String {
        if self.elements.is_empty() {
            return NO_CHOICE_ELEMENTS.to_owned();
        }
        let order = if self.narrowed() { ", in page order" } else { "" };
        self.listing(format!("{}{order}:", self.header()), self.rendered.clone())
    }

    /// The same lines after each was scored against the description on its own: highest first,
    /// each with its score, and any a failed request left unscored at the end in page order.
    fn render_scored(&self, report: &SearchReport) -> String {
        let by_uid = uid_index(self.elements);
        let mut scored = HashSet::new();
        let mut lines = report
            .hits
            .iter()
            .filter_map(|hit| {
                let line = by_uid.get(&uid_from_element_label(&hit.candidate.label)?)?;
                scored.insert(line.uid);
                Some(format!(
                    "{} (score {:.3})",
                    render_ax_line(line, false),
                    hit.score
                ))
            })
            .collect::<Vec<_>>();
        lines.extend(
            self.elements
                .iter()
                .filter(|line| !scored.contains(&line.uid))
                .map(|line| format!("{} (unscored)", render_ax_line(line, false))),
        );
        let mut header = format!(
            "{}. Each was then scored against the description on its own ({}), highest first:",
            self.header(),
            counted_requests(report.requests)
        );
        if report.failed_requests > 0 {
            header.push_str(&format!(
                "\n{} of those requests failed; the elements they left unscored are listed last.",
                report.failed_requests
            ));
        }
        self.listing(header, lines)
    }

    /// One scoring candidate per line, labelled by uid and carrying the two lines on either side,
    /// the same shape `preview_snapshot`'s refinement scores.
    fn candidates(&self) -> Vec<Candidate> {
        self.elements
            .iter()
            .enumerate()
            .map(|(index, line)| {
                Candidate::new(element_label(line.uid), self.rendered[index].clone())
                    .with_context(line_context(&self.rendered, index))
            })
            .collect()
    }
}

fn counted_requests(count: usize) -> String {
    format!("{count} request{}", if count == 1 { "" } else { "s" })
}

const NO_CHOICE_ELEMENTS: &str =
    "No accessible content found on the preview page; there was nothing to choose among.";

/// How a miss lists the element lines the decision model was shown.
enum MissListing {
    /// As they went into the question.
    Plain,
    /// Each scored against the description on its own, highest first.
    Scored(SearchReport),
    /// Scoring was asked for and could not be done; the lines follow as the question had them.
    ScoringFailed(String),
}

/// Scores every line a choice question showed, one request per line, for a miss's listing.
///
/// Threshold zero and no refinement: every line is its own candidate and every score is wanted,
/// so the listing can rank them all. Cancellation stops the call; any other failure is reported
/// in the listing, because the miss itself is still worth answering.
fn score_shown(
    scorer: Result<Arc<dyn Scorer>, String>,
    shown: &Shown<'_>,
    description: &str,
    cancel: &CancelSignal,
) -> Result<MissListing, String> {
    let scorer = match scorer {
        Ok(scorer) => scorer,
        Err(error) => return Ok(MissListing::ScoringFailed(error)),
    };
    match search(scorer, description, 0.0, shown.candidates(), |_| Vec::new(), cancel) {
        Ok(report) => Ok(MissListing::Scored(report)),
        Err(DecisionError::Cancelled) => Err(failure(DecisionError::Cancelled)),
        Err(error) => Ok(MissListing::ScoringFailed(failure(error))),
    }
}

/// The answer when the decision model named no element of the page — or was never asked, because
/// the page offered none: a headline with the model's ranking, then every element line it was
/// shown. `Ok` is a tool result the model reads; `Err` is a failed call.
fn missed(
    action: DescribedAction,
    description: &str,
    answer: Option<&ChoiceAnswer>,
    shown: &Shown<'_>,
    listing: &MissListing,
) -> Result<String, String> {
    let mut headline = if action.misses_are_errors() {
        format!("No element on the page matched the description {description:?}")
    } else {
        format!("Element not found: {description}")
    };
    if let Some(answer) = answer {
        headline.push(' ');
        headline.push_str(&miss_ranking(answer, &uid_index(shown.elements)));
    }
    let body = match listing {
        MissListing::Plain => shown.render(),
        MissListing::Scored(report) => shown.render_scored(report),
        MissListing::ScoringFailed(error) => format!(
            "Scoring the elements one by one failed, so they follow unscored: {error}\n{}",
            shown.render()
        ),
    };
    let text = format!("{headline}\n{body}");
    if action.misses_are_errors() {
        Err(text)
    } else {
        Ok(text)
    }
}

/// How sure the model was that nothing matched, and the elements it came closest to choosing.
fn miss_ranking(answer: &ChoiceAnswer, by_uid: &HashMap<u64, &AxLine>) -> String {
    let mut text = format!("(confidence {:.2}", answer.confidence);
    let closest = ranked_elements(answer, by_uid, NO_MATCH_CHOICE);
    if !closest.is_empty() {
        text.push_str(&format!("; closest: {}", closest.join(", ")));
    }
    text.push(')');
    text
}

fn uid_index(lines: &[AxLine]) -> HashMap<u64, &AxLine> {
    lines.iter().map(|line| (line.uid, line)).collect()
}

/// The one line that precedes the base tool's own text: which element was chosen, how sure the
/// model was, what it nearly chose instead, and the selector the action actually ran on.
fn chose_line(
    chosen: &AxLine,
    answer: &ChoiceAnswer,
    by_uid: &HashMap<u64, &AxLine>,
    selector: &str,
) -> String {
    let mut text = format!(
        "Chose {} (confidence {:.2}",
        render_ax_line(chosen, false),
        answer.confidence
    );
    let runners_up = ranked_elements(answer, by_uid, &answer.choice);
    if !runners_up.is_empty() {
        text.push_str(&format!("; runners-up: {}", runners_up.join(", ")));
    }
    text.push_str(&format!(") \u{2192} selector {selector}"));
    text
}

/// The distribution's top elements other than `skip`, highest first, as `[uid] role: "name" p`.
/// `none` is never one of them: "the model also considered giving up" is not a runner-up.
fn ranked_elements(
    answer: &ChoiceAnswer,
    by_uid: &HashMap<u64, &AxLine>,
    skip: &str,
) -> Vec<String> {
    answer
        .probabilities
        .iter()
        .filter(|(name, _)| name != skip && name != NO_MATCH_CHOICE)
        .filter_map(|(name, probability)| {
            let line = by_uid.get(&name.parse::<u64>().ok()?)?;
            Some(format!("{} {probability:.2}", render_ax_line(line, false)))
        })
        .take(3)
        .collect()
}

/// The `choice` options for one element set: one per line, plus the way out.
fn choice_options(lines: &[AxLine]) -> Vec<(String, String)> {
    let mut options = lines
        .iter()
        .take(MAX_ELEMENT_CHOICES)
        .map(|line| (line.uid.to_string(), choice_option_text(line)))
        .collect::<Vec<_>>();
    options.push((NO_MATCH_CHOICE.to_owned(), NO_MATCH_OPTION.to_owned()));
    options
}

/// One option's description: the element's line without its uid, which is the option's name.
fn choice_option_text(line: &AxLine) -> String {
    render_ax_line(line, false)
        .split_once("] ")
        .map_or_else(|| line.role.clone(), |(_, rest)| rest.to_owned())
}

/// The element lines one choice question is asked about.
///
/// A page with more elements than the question holds is narrowed first: the runs are scored
/// against the description and the best ones are kept, in page order, until the budget is full.
fn narrow_candidates(
    lines: &[AxLine],
    description: &str,
    cancel: &CancelSignal,
) -> Result<Vec<AxLine>, String> {
    if lines.len() <= MAX_ELEMENT_CHOICES {
        return Ok(lines.to_vec());
    }
    narrow_with(jev_scorer(ELEMENT_RUBRIC)?, lines, description, cancel)
}

fn narrow_with(
    scorer: Arc<dyn Scorer>,
    lines: &[AxLine],
    description: &str,
    cancel: &CancelSignal,
) -> Result<Vec<AxLine>, String> {
    let rendered = render_lines(lines);
    let (coarse, by_label) = element_runs(lines, &rendered);
    // One coarse pass and no refinement: this is a shortlist, not an answer. Threshold zero so
    // every run is ranked; the budget below is what actually selects.
    let report = search(scorer, description, 0.0, coarse, |_| Vec::new(), cancel).map_err(failure)?;
    Ok(select_runs(lines, &report, &by_label))
}

/// The best-scoring runs that fit the option budget, returned in page order. The best run is
/// always kept, even if it alone would overflow the budget — a shortlist of nothing is useless.
fn select_runs(
    lines: &[AxLine],
    report: &SearchReport,
    by_label: &HashMap<String, Range<usize>>,
) -> Vec<AxLine> {
    let mut ranges: Vec<Range<usize>> = Vec::new();
    let mut collected = 0;
    for hit in &report.hits {
        let Some(range) = by_label.get(&hit.candidate.label) else {
            continue;
        };
        if collected + range.len() > MAX_ELEMENT_CHOICES {
            break;
        }
        collected += range.len();
        ranges.push(range.clone());
    }
    if ranges.is_empty() {
        let best = report
            .hits
            .first()
            .and_then(|hit| by_label.get(&hit.candidate.label));
        let Some(best) = best else {
            return Vec::new();
        };
        ranges.push(best.start..best.end.min(best.start + MAX_ELEMENT_CHOICES));
    }
    ranges.sort_by_key(|range| range.start);
    ranges
        .into_iter()
        .flat_map(|range| lines[range].to_vec())
        .collect()
}

// ----------------------------------------------------------------- Element candidates

fn render_lines(lines: &[AxLine]) -> Vec<String> {
    lines.iter().map(|line| render_ax_line(line, true)).collect()
}

/// The coarse candidates for a page's elements: runs of consecutive lines, labelled by the uids
/// at each end so a whole-run hit is still addressable.
fn element_runs(
    lines: &[AxLine],
    rendered: &[String],
) -> (Vec<Candidate>, HashMap<String, Range<usize>>) {
    let runs = group_entries(lines.len(), MAX_ELEMENT_RUNS, ELEMENTS_PER_RUN);
    let mut by_label = HashMap::with_capacity(runs.len());
    let candidates = runs
        .into_iter()
        .map(|range| {
            let label = run_label(&lines[range.clone()]);
            by_label.insert(label.clone(), range.clone());
            Candidate::new(label, rendered[range].join("\n"))
        })
        .collect();
    (candidates, by_label)
}

/// A winning run refined into its individual elements, each carrying its neighbours as context.
fn refine_element_run(
    lines: &[AxLine],
    rendered: &[String],
    by_label: &HashMap<String, Range<usize>>,
    candidate: &Candidate,
) -> Vec<Candidate> {
    let Some(range) = by_label.get(&candidate.label) else {
        return Vec::new();
    };
    range
        .clone()
        .map(|index| {
            Candidate::new(element_label(lines[index].uid), rendered[index].clone())
                .with_context(line_context(rendered, index))
        })
        .collect()
}

fn run_label(lines: &[AxLine]) -> String {
    let first = lines.first().map_or(0, |line| line.uid);
    let last = lines.last().map_or(0, |line| line.uid);
    format!("elements {first}-{last}")
}

fn element_label(uid: u64) -> String {
    format!("element {uid}")
}

fn uid_from_element_label(label: &str) -> Option<u64> {
    label.strip_prefix("element ")?.parse().ok()
}

/// The two lines on either side of one element, which is what the element rubric reads as
/// `candidate.context`.
fn line_context(rendered: &[String], index: usize) -> String {
    let before = &rendered[index.saturating_sub(2)..index];
    let after = &rendered[(index + 1).min(rendered.len())..(index + 3).min(rendered.len())];
    before
        .iter()
        .chain(after)
        .cloned()
        .collect::<Vec<_>>()
        .join("\n")
}

// ----------------------------------------------------------------- Rendering

/// The report's summary lines — everything `render_report` prints that is not a hit — so a tool
/// that renders its own hits still says "no page elements scored…" in exactly the same words.
fn report_summary(report: &SearchReport, query: &str, threshold: f64, subject: &str) -> String {
    let rendered = render_report(report, query, threshold, subject, false);
    let summary = rendered.lines().count().saturating_sub(report.hits.len());
    rendered
        .lines()
        .take(summary)
        .collect::<Vec<_>>()
        .join("\n")
}

/// `preview_snapshot`'s answer to a `query`: the summary, then each hit as its own snapshot line with the
/// selector that addresses it, or — for a run that never got refined — the run itself.
fn render_element_report(
    report: &SearchReport,
    query: &str,
    threshold: f64,
    lines: &[AxLine],
    capped: bool,
    selector_of: impl Fn(&AxLine) -> Option<String>,
) -> String {
    let by_uid = lines
        .iter()
        .map(|line| (line.uid, line))
        .collect::<HashMap<_, _>>();
    let mut text = vec![report_summary(report, query, threshold, "runs of page elements")];
    if capped {
        text.push(format!(
            "Only the first {PREVIEW_ELEMENT_LINES_CAP} element lines of the page were scored; \
             the page has more."
        ));
    }
    for hit in &report.hits {
        let line = uid_from_element_label(&hit.candidate.label)
            .and_then(|uid| by_uid.get(&uid).copied());
        match line {
            Some(line) => {
                let selector =
                    selector_of(line).unwrap_or_else(|| SELECTOR_UNAVAILABLE.to_owned());
                text.push(format!(
                    "{} (score {:.3}) \u{2014} selector: {selector}",
                    render_ax_line(line, false),
                    hit.score
                ));
            }
            None => {
                text.push(String::new());
                text.push(format!(
                    "--- {} (score {:.3}, whole run) ---",
                    hit.candidate.label, hit.score
                ));
                text.push(hit.candidate.text.clone());
            }
        }
    }
    text.join("\n")
}

fn scored_console_line(console: usize) -> String {
    format!(
        "Scored {console} console line{}.",
        if console == 1 { "" } else { "s" }
    )
}

fn scored_lines_line(console: usize, server: usize) -> String {
    format!(
        "Scored {console} console line{} and {server} server line{}.",
        if console == 1 { "" } else { "s" },
        if server == 1 { "" } else { "s" }
    )
}

// ----------------------------------------------------------------- Log sources

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LogSource {
    All,
    Console,
    Server,
}

impl LogSource {
    fn reads_console(self) -> bool {
        matches!(self, Self::All | Self::Console)
    }

    fn reads_server(self) -> bool {
        matches!(self, Self::All | Self::Server)
    }
}

fn parse_source(input: &JsonObject) -> Result<LogSource, String> {
    let value = match input.get("source") {
        None | Some(Value::Null) => return Ok(LogSource::All),
        Some(value) => value
            .as_str()
            .ok_or_else(|| "Parameter source must be a string".to_owned())?,
    };
    match value.trim() {
        "all" => Ok(LogSource::All),
        "console" => Ok(LogSource::Console),
        "server" => Ok(LogSource::Server),
        other => Err(format!(
            "Parameter source must be one of \"all\", \"console\", \"server\"; got {other:?}"
        )),
    }
}

/// `preview_console_logs`'s `level`, checked the way the plain listing checks it. `None` is
/// `all`, which filters nothing.
fn parse_console_level(input: &JsonObject) -> Result<Option<String>, String> {
    let value = match input.get("level") {
        None | Some(Value::Null) => return Ok(None),
        Some(value) => value
            .as_str()
            .ok_or_else(|| "Parameter level must be a string".to_owned())?,
    };
    match value.trim() {
        "all" => Ok(None),
        level @ ("error" | "warn") => Ok(Some(level.to_owned())),
        other => Err(format!(
            "Parameter level must be one of \"all\", \"error\", \"warn\"; got {other:?}"
        )),
    }
}

/// `preview_console_logs`'s `lines`, clamped to the same ceiling the plain listing uses. `None`
/// scores every entry the level filter kept.
fn parse_console_lines(input: &JsonObject) -> Result<Option<usize>, String> {
    match input.get("lines") {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .map(|lines| Some(lines.clamp(1, MAX_CONSOLE_LINES) as usize))
            .ok_or_else(|| "Parameter lines must be a non-negative integer".to_owned()),
    }
}

/// Which source a merged chunk label came from, so the refinement keeps labelling it the same way.
fn label_source(label: &str) -> &'static str {
    if label.starts_with("console ") {
        "console"
    } else {
        "server"
    }
}

/// The dev server whose output this call scores: the one it named, else the first one running.
///
/// A `serverId` that names nothing was already refused by `resolve_preview_page_session`, so the
/// only `None` here is "this workspace has no running server".
fn server_for_logs(
    request: &ToolExecutionRequest,
    state: &AppState,
    workspace: &Path,
) -> Option<String> {
    match request.input.get("serverId").and_then(Value::as_str) {
        Some(server_id) => preview_server_for_session(
            state,
            workspace,
            &request.conversation_id,
            server_id,
        )
        .map(|server| server.server_id),
        None => first_running_preview_server(state, workspace, &request.conversation_id)
            .map(|server| server.server_id),
    }
}

/// A log entry is a whole read from the pipe rather than one line, so every entry is split.
fn server_log_lines(state: &AppState, server_id: &str) -> Vec<String> {
    state
        .preview_servers
        .logs(server_id)
        .into_iter()
        .flat_map(|entry| {
            entry
                .line
                .split('\n')
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .collect()
}

// ----------------------------------------------------------------- Page reads

/// One page read on its own thread under [`PAGE_READ_TIMEOUT`].
///
/// The read holds the page's automation lock, and a wedged page has no cancellation of its own;
/// the orphaned thread keeps the lock until it finishes, which is exactly the "the pane may be
/// stuck" the dispatcher's own timeout describes.
fn page_read<T, F>(state: &AppState, session_id: &str, what: &str, read: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce(&BrowserRuntime, &str) -> Result<T, String> + Send + 'static,
{
    let (sender, receiver) = mpsc::channel();
    let runtime = state.browser.clone();
    let owned_session = session_id.to_owned();
    std::thread::spawn(move || {
        let _ = sender.send(read(&runtime, &owned_session));
    });
    match receiver.recv_timeout(PAGE_READ_TIMEOUT) {
        Ok(result) => result,
        Err(RecvTimeoutError::Disconnected) => {
            Err(format!("{what} failed inside the browser runtime"))
        }
        Err(RecvTimeoutError::Timeout) => Err(format!(
            "{what} timed out after {}s. The pane may be stuck (modal dialog, navigation hang, or \
             unresponsive renderer). Check preview_console_logs for errors.",
            PAGE_READ_TIMEOUT.as_secs()
        )),
    }
}

/// The page's element lines, minus the text leaves.
///
/// A `StaticText` or `InlineTextBox` line is the text of the element above it, not an element:
/// it has no DOM node a selector could reach, so it can never be clicked, filled or inspected,
/// and it repeats a name the element line already carries. Left in, the decision model — like
/// any reader — picks "the Email textbox" out of the `InlineTextBox: "Email "` line as readily
/// as out of the textbox itself, and the call then fails on a node nobody can act on.
fn element_lines(
    state: &AppState,
    session_id: &str,
    tool: PreviewTool,
) -> Result<(Vec<AxLine>, bool), String> {
    let (lines, capped) = page_read(
        state,
        session_id,
        "Reading the preview page's elements",
        move |runtime: &BrowserRuntime, session_id: &str| {
            runtime.element_lines_blocking(session_id, tool)
        },
    )?;
    Ok((actionable_lines(lines), capped))
}

fn is_text_leaf(line: &AxLine) -> bool {
    matches!(line.role.as_str(), "StaticText" | "InlineTextBox")
}

fn actionable_lines(lines: Vec<AxLine>) -> Vec<AxLine> {
    lines.into_iter().filter(|line| !is_text_leaf(line)).collect()
}

fn selector_for_blocking(
    state: &AppState,
    session_id: &str,
    tool: PreviewTool,
    backend_node_id: i64,
) -> Result<Option<String>, String> {
    page_read(
        state,
        session_id,
        "Resolving the element's selector",
        move |runtime: &BrowserRuntime, session_id: &str| {
            runtime.selector_for_blocking(session_id, tool, backend_node_id)
        },
    )
}

/// The selector for one hit, or `None` when the page cannot give one. A transport failure is not
/// worth failing the whole listing over: the hit is reported without a selector like any other
/// element the page will not address.
fn selector_for(
    state: &AppState,
    session_id: &str,
    tool: PreviewTool,
    line: &AxLine,
) -> Option<String> {
    selector_for_blocking(state, session_id, tool, line.backend_node_id?)
        .ok()
        .flatten()
}

fn console_lines(
    state: &AppState,
    session_id: &str,
    level: Option<String>,
) -> Result<Vec<String>, String> {
    page_read(
        state,
        session_id,
        "Reading the preview page's console",
        move |runtime: &BrowserRuntime, session_id: &str| {
            runtime.console_log_lines_blocking(session_id, level)
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decision_model::search::Hit;
    use crate::decision_model::DecisionError;

    fn line(uid: u64, role: &str, name: &str) -> AxLine {
        AxLine {
            indent: 1,
            uid,
            role: role.to_owned(),
            name: name.to_owned(),
            value: String::new(),
            backend_node_id: Some(uid as i64 * 10),
            descendants_note: None,
        }
    }

    fn lines(count: u64) -> Vec<AxLine> {
        (1..=count)
            .map(|uid| line(uid, "button", &format!("Button {uid}")))
            .collect()
    }

    fn input(pairs: &[(&str, serde_json::Value)]) -> JsonObject {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), value.clone()))
            .collect()
    }

    /// The text under a button is not a second button: text leaves leave the candidate set,
    /// elements of every other role stay, in page order.
    #[test]
    fn text_leaves_are_not_element_candidates() {
        let page = vec![
            line(1, "button", "Log in"),
            line(2, "StaticText", "Log in"),
            line(3, "InlineTextBox", "Log in"),
            line(4, "textbox", "Email "),
            line(5, "generic", ""),
        ];
        let kept = actionable_lines(page)
            .into_iter()
            .map(|line| line.uid)
            .collect::<Vec<_>>();
        assert_eq!(kept, vec![1, 4, 5]);
    }

    fn answer(choice: &str, confidence: f64, probabilities: &[(&str, f64)]) -> ChoiceAnswer {
        ChoiceAnswer {
            choice: choice.to_owned(),
            confidence,
            probabilities: probabilities
                .iter()
                .map(|(name, probability)| ((*name).to_owned(), *probability))
                .collect(),
        }
    }

    /// Every element is offered by its uid, described without it, and the way out is always last.
    #[test]
    fn choice_options_name_every_element_and_the_way_out() {
        let mut elements = vec![
            line(7, "link", "Save draft"),
            AxLine {
                value: "hello".to_owned(),
                ..line(9, "textbox", "Search")
            },
            AxLine {
                name: String::new(),
                ..line(11, "button", "")
            },
        ];
        let options = choice_options(&elements);
        assert_eq!(options.len(), 4);
        assert_eq!(options[0], ("7".to_owned(), "link: \"Save draft\"".to_owned()));
        assert_eq!(
            options[1],
            (
                "9".to_owned(),
                "textbox: \"Search\" (value: \"hello\")".to_owned()
            )
        );
        assert_eq!(options[2], ("11".to_owned(), "button".to_owned()));
        assert_eq!(
            options[3],
            (NO_MATCH_CHOICE.to_owned(), NO_MATCH_OPTION.to_owned())
        );

        // The option set is what the API accepts, never one entry more.
        elements = lines(MAX_CHOICE_OPTIONS as u64 + 40);
        let options = choice_options(&elements);
        assert_eq!(options.len(), MAX_CHOICE_OPTIONS);
        assert_eq!(options[MAX_ELEMENT_CHOICES].0, NO_MATCH_CHOICE);
        assert_eq!(options[MAX_ELEMENT_CHOICES - 1].0, "254");
    }

    /// The chosen element, its confidence and what it beat, then the selector that was acted on.
    #[test]
    fn the_chosen_element_is_reported_with_its_runners_up() {
        let elements = vec![
            line(42, "button", "Save"),
            line(7, "link", "Save draft"),
            line(9, "button", "Cancel"),
        ];
        let by_uid = uid_index(&elements);
        let answered = answer(
            "42",
            0.81,
            &[("42", 0.81), ("7", 0.13), ("9", 0.04), ("none", 0.02)],
        );
        assert_eq!(
            chose_line(
                &elements[0],
                &answered,
                &by_uid,
                "form > button:nth-of-type(2)"
            ),
            "Chose [42] button: \"Save\" (confidence 0.81; runners-up: [7] link: \"Save draft\" \
             0.13, [9] button: \"Cancel\" 0.04) \u{2192} selector form > button:nth-of-type(2)"
        );

    }

    fn shown(elements: &[AxLine], page_elements: usize, capped: bool) -> Shown<'_> {
        Shown::new(elements, page_elements, capped)
    }

    /// "None of the above" hands back the ranking and then every element line the decision model
    /// was shown, exactly as it went into the question — a failure for click and fill, an answer
    /// for inspect.
    #[test]
    fn a_miss_hands_back_the_elements_the_model_was_shown() {
        let elements = vec![
            line(42, "button", "Save"),
            line(7, "link", "Save draft"),
            AxLine {
                indent: 2,
                ..line(9, "button", "Cancel")
            },
        ];
        let whole_page = shown(&elements, 3, false);
        let nothing = answer("none", 0.92, &[("none", 0.92), ("7", 0.05)]);
        let plain = &MissListing::Plain;
        let listing = "The decision model was shown all 3 element lines of the page:\n  \
                       [42] button: \"Save\"\n  [7] link: \"Save draft\"\n    [9] button: \"Cancel\"";

        let error = missed(
            DescribedAction::Click,
            "the save button",
            Some(&nothing),
            &whole_page,
            plain,
        )
        .expect_err("clicking nothing is a failure");
        assert_eq!(
            error,
            format!(
                "No element on the page matched the description \"the save button\" (confidence \
                 0.92; closest: [7] link: \"Save draft\" 0.05)\n{listing}"
            )
        );
        // What comes back is what the question carried.
        assert!(error.ends_with(&whole_page.text()), "{error}");
        assert!(missed(DescribedAction::Fill, "x", Some(&nothing), &whole_page, plain).is_err());
        assert_eq!(
            missed(
                DescribedAction::Inspect,
                "the save button",
                Some(&nothing),
                &whole_page,
                plain
            )
            .expect("inspect answers rather than fails"),
            format!(
                "Element not found: the save button (confidence 0.92; closest: [7] link: \"Save \
                 draft\" 0.05)\n{listing}"
            )
        );

        // A long page was narrowed before the question: the header says so, and says when the
        // read itself stopped short.
        let narrowed = missed(
            DescribedAction::Inspect,
            "x",
            Some(&answer("none", 0.6, &[("none", 0.6)])),
            &shown(&elements[..1], 2_400, true),
            plain,
        )
        .unwrap();
        assert_eq!(
            narrowed,
            format!(
                "Element not found: x (confidence 0.60)\nThe page has 2400 element lines; the \
                 decision model was shown the 1 in the runs that scored best against the \
                 description, in page order:\n(Only the first {PREVIEW_ELEMENT_LINES_CAP} element \
                 lines of the page were read; the page has more.)\n  [42] button: \"Save\""
            )
        );
        assert!(shown(&elements[..1], 1, false)
            .render()
            .starts_with("The decision model was shown the page's only element line:\n"));

        // A page with nothing to choose among is the same miss, with no question behind it.
        assert_eq!(
            missed(
                DescribedAction::Click,
                "a button",
                None,
                &shown(&[], 0, false),
                plain
            )
            .unwrap_err(),
            format!(
                "No element on the page matched the description \"a button\"\n{NO_CHOICE_ELEMENTS}"
            )
        );
    }

    fn scored(uid: u64, score: f64) -> Hit {
        Hit {
            candidate: Candidate::new(element_label(uid), "x"),
            score,
            refined: false,
        }
    }

    /// A scored miss lists the same lines highest first, each with its own score; a line a failed
    /// request left unscored still comes back, last, and the failure is counted.
    #[test]
    fn a_scored_miss_ranks_every_line_it_was_shown() {
        let elements = vec![
            line(42, "button", "Save"),
            line(7, "link", "Save draft"),
            AxLine {
                indent: 2,
                ..line(9, "button", "Cancel")
            },
        ];
        let whole_page = shown(&elements, 3, false);
        let nothing = answer("none", 0.9, &[("none", 0.9)]);
        let report = SearchReport {
            hits: vec![scored(7, 0.41), scored(9, 0.052)],
            coarse_count: 3,
            requests: 3,
            failed_requests: 1,
            ..SearchReport::default()
        };
        assert_eq!(
            missed(
                DescribedAction::Inspect,
                "the publish button",
                Some(&nothing),
                &whole_page,
                &MissListing::Scored(report),
            )
            .unwrap(),
            "Element not found: the publish button (confidence 0.90)\nThe decision model was \
             shown all 3 element lines of the page. Each was then scored against the description \
             on its own (3 requests), highest first:\n1 of those requests failed; the elements \
             they left unscored are listed last.\n[7] link: \"Save draft\" (score 0.410)\n[9] button: \
             \"Cancel\" (score 0.052)\n[42] button: \"Save\" (unscored)"
        );

        // A shortlist says so and keeps the note on a read that stopped short.
        let one = SearchReport {
            hits: vec![scored(42, 1.0)],
            requests: 1,
            ..SearchReport::default()
        };
        assert_eq!(
            shown(&elements[..1], 900, true).render_scored(&one),
            format!(
                "The page has 900 element lines; the decision model was shown the 1 in the runs \
                 that scored best against the description. Each was then scored against the \
                 description on its own (1 request), highest first:\n(Only the first \
                 {PREVIEW_ELEMENT_LINES_CAP} element lines of the page were read; the page has \
                 more.)\n[42] button: \"Save\" (score 1.000)"
            )
        );

        // Scoring that could not be done still answers the miss, with the lines unscored.
        let failed = missed(
            DescribedAction::Click,
            "x",
            Some(&nothing),
            &whole_page,
            &MissListing::ScoringFailed("TypeSafe is unreachable".to_owned()),
        )
        .unwrap_err();
        assert!(
            failed.contains(
                "\nScoring the elements one by one failed, so they follow unscored: TypeSafe is \
                 unreachable\nThe decision model was shown all 3 element lines of the page:\n"
            ),
            "{failed}"
        );
        assert!(failed.ends_with(&whole_page.text()), "{failed}");
    }

    /// Every line is scored as its own candidate with its neighbours as context — nothing is cut,
    /// nothing refined — and only cancellation turns a scoring failure into a failed call.
    #[test]
    fn a_miss_scores_each_shown_line_on_its_own() {
        /// Scores by how close the candidate's uid is to the one named in the query, and records
        /// what it was asked.
        struct NearestElement(std::sync::Mutex<Vec<Candidate>>);
        impl Scorer for NearestElement {
            fn score(&self, query: &str, candidate: &Candidate) -> Result<f64, DecisionError> {
                self.0.lock().unwrap().push(candidate.clone());
                let wanted: f64 = query.parse().unwrap_or(0.0);
                let uid = uid_from_element_label(&candidate.label).unwrap_or(0) as f64;
                Ok(1.0 / (1.0 + (uid - wanted).abs()))
            }
        }

        let page = lines(5);
        let shown = shown(&page, 5, false);
        let scorer = Arc::new(NearestElement(Default::default()));
        let listing = score_shown(
            Ok(scorer.clone() as Arc<dyn Scorer>),
            &shown,
            "4",
            &CancelSignal::default(),
        )
        .expect("scored");
        let MissListing::Scored(report) = listing else {
            panic!("expected a scored listing");
        };
        assert_eq!(report.requests, 5);
        let order = report
            .hits
            .iter()
            .map(|hit| uid_from_element_label(&hit.candidate.label).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(order, vec![4, 3, 5, 2, 1], "every line, highest first");
        let mut asked = scorer.0.lock().unwrap().clone();
        asked.sort_by_key(|candidate| uid_from_element_label(&candidate.label));
        assert_eq!(asked.len(), 5, "one request per line, no refinement pass");
        assert_eq!(asked[0].text, "  [1] button: \"Button 1\"");
        assert_eq!(
            asked[2].context.as_deref(),
            Some(
                "  [1] button: \"Button 1\"\n  [2] button: \"Button 2\"\n  [4] button: \
                 \"Button 4\"\n  [5] button: \"Button 5\""
            )
        );

        // A scorer that cannot be built, or a pass that fails outright, is reported in the listing.
        assert!(matches!(
            score_shown(Err("no key".to_owned()), &shown, "4", &CancelSignal::default()),
            Ok(MissListing::ScoringFailed(error)) if error == "no key"
        ));
        struct Broken(DecisionError);
        impl Scorer for Broken {
            fn score(&self, _: &str, _: &Candidate) -> Result<f64, DecisionError> {
                Err(self.0.clone())
            }
        }
        let broken = |error| Ok(Arc::new(Broken(error)) as Arc<dyn Scorer>);
        assert!(matches!(
            score_shown(
                broken(DecisionError::Transient("down".into())),
                &shown,
                "4",
                &CancelSignal::default()
            ),
            Ok(MissListing::ScoringFailed(_))
        ));
        // A stopped turn is not a miss to answer.
        assert!(score_shown(
            broken(DecisionError::Cancelled),
            &shown,
            "4",
            &CancelSignal::default()
        )
        .is_err());
    }

    /// A page with more elements than one choice question holds is shortlisted by run score, and
    /// the shortlist stays in page order so the model reads the page as it is laid out.
    #[test]
    fn a_long_page_is_narrowed_to_the_best_runs_in_page_order() {
        /// Scores a run by how close its first uid is to the one named in the query.
        struct NearestRun;
        impl Scorer for NearestRun {
            fn score(&self, query: &str, candidate: &Candidate) -> Result<f64, DecisionError> {
                let wanted: f64 = query.parse().unwrap_or(0.0);
                let first: f64 = candidate
                    .label
                    .trim_start_matches("elements ")
                    .split('-')
                    .next()
                    .and_then(|uid| uid.parse().ok())
                    .unwrap_or(0.0);
                Ok(1.0 / (1.0 + (first - wanted).abs()))
            }
        }

        let page = lines(600);
        let narrowed = narrow_with(
            Arc::new(NearestRun),
            &page,
            "300",
            &CancelSignal::default(),
        )
        .expect("narrowed");
        assert!(narrowed.len() <= MAX_ELEMENT_CHOICES, "{}", narrowed.len());
        assert!(narrowed.len() > MAX_ELEMENT_CHOICES - 13, "{}", narrowed.len());
        assert!(
            narrowed.windows(2).all(|pair| pair[0].uid < pair[1].uid),
            "the shortlist keeps page order"
        );
        // The run around the query is in; the far ends of the page are not.
        assert!(narrowed.iter().any(|element| element.uid == 300));
        assert!(!narrowed.iter().any(|element| element.uid == 1));
        assert!(!narrowed.iter().any(|element| element.uid == 600));

        // A page that fits needs no scoring at all.
        let short = lines(MAX_ELEMENT_CHOICES as u64);
        assert_eq!(
            narrow_candidates(&short, "anything", &CancelSignal::default()).unwrap(),
            short
        );
    }

    /// The best run is kept even when every run is wider than the option budget.
    #[test]
    fn the_best_run_survives_a_budget_no_run_fits() {
        let page = lines(600);
        let mut by_label = HashMap::new();
        by_label.insert("elements 1-600".to_owned(), 0..600);
        let report = SearchReport {
            hits: vec![Hit {
                candidate: Candidate::new("elements 1-600", "text"),
                score: 1.0,
                refined: false,
            }],
            ..SearchReport::default()
        };
        let selected = select_runs(&page, &report, &by_label);
        assert_eq!(selected.len(), MAX_ELEMENT_CHOICES);
        assert_eq!(selected[0].uid, 1);
        // Nothing to select from is empty rather than a panic.
        assert!(select_runs(&page, &SearchReport::default(), &by_label).is_empty());
    }

    /// Runs are labelled by the uids at their ends, refined one element at a time, and each
    /// element carries the two lines on either side as context.
    #[test]
    fn element_runs_are_labelled_by_uid_and_refine_to_single_elements() {
        let page = lines(30);
        let rendered = render_lines(&page);
        let (coarse, by_label) = element_runs(&page, &rendered);
        assert_eq!(coarse[0].label, "elements 1-12");
        assert_eq!(coarse[0].text.lines().count(), 12);
        assert_eq!(coarse[2].label, "elements 25-30");
        assert_eq!(by_label["elements 13-24"], 12..24);

        let pieces = refine_element_run(&page, &rendered, &by_label, &coarse[1]);
        assert_eq!(pieces.len(), 12);
        assert_eq!(pieces[0].label, "element 13");
        assert_eq!(pieces[0].text, "  [13] button: \"Button 13\"");
        assert_eq!(
            pieces[0].context.as_deref(),
            Some(
                "  [11] button: \"Button 11\"\n  [12] button: \"Button 12\"\n  [14] button: \
                 \"Button 14\"\n  [15] button: \"Button 15\""
            )
        );
        // The first element of the page has no lines before it.
        let first = refine_element_run(&page, &rendered, &by_label, &coarse[0]);
        assert_eq!(
            first[0].context.as_deref(),
            Some("  [2] button: \"Button 2\"\n  [3] button: \"Button 3\"")
        );
        // A label from another plan refines to nothing.
        assert!(
            refine_element_run(&page, &rendered, &by_label, &Candidate::new("x", "y")).is_empty()
        );
        assert_eq!(uid_from_element_label("element 13"), Some(13));
        assert_eq!(uid_from_element_label("elements 1-12"), None);
    }

    /// Hits come back as their own snapshot line plus a selector; a run that never refined comes
    /// back whole; and the summary is `render_report`'s own wording.
    #[test]
    fn the_element_report_addresses_every_hit() {
        let page = vec![
            line(42, "button", "Save"),
            line(43, "link", "Help"),
            line(44, "button", "Cancel"),
        ];
        let report = SearchReport {
            hits: vec![
                Hit {
                    candidate: Candidate::new("element 42", "  [42] button: \"Save\""),
                    score: 0.933,
                    refined: true,
                },
                Hit {
                    candidate: Candidate::new("element 44", "  [44] button: \"Cancel\""),
                    score: 0.8,
                    refined: true,
                },
                Hit {
                    candidate: Candidate::new(
                        "elements 40-51",
                        "  [40] listitem\n  [41] listitem",
                    ),
                    score: 0.7,
                    refined: false,
                },
            ],
            coarse_count: 4,
            requests: 9,
            ..SearchReport::default()
        };
        let text = render_element_report(&report, "the save button", 0.6, &page, true, |line| {
            (line.uid == 42).then(|| "form > button:nth-of-type(2)".to_owned())
        });
        assert!(
            text.starts_with(
                "3 hits at or above 0.600 for query \"the save button\" (4 runs of page elements scored, \
                 9 requests)."
            ),
            "{text}"
        );
        assert!(
            text.contains(&format!(
                "Only the first {PREVIEW_ELEMENT_LINES_CAP} element lines"
            )),
            "{text}"
        );
        assert!(
            text.contains(
                "[42] button: \"Save\" (score 0.933) \u{2014} selector: form > button:nth-of-type(2)"
            ),
            "{text}"
        );
        assert!(
            text.contains(&format!(
                "[44] button: \"Cancel\" (score 0.800) \u{2014} selector: {SELECTOR_UNAVAILABLE}"
            )),
            "{text}"
        );
        assert!(
            text.contains(
                "--- elements 40-51 (score 0.700, whole run) ---\n  [40] listitem\n  [41] listitem"
            ),
            "{text}"
        );

        // Nothing passed: the summary is the only thing the model reads, near-misses included.
        let empty = SearchReport {
            coarse_count: 3,
            requests: 3,
            near_misses: vec![("elements 1-12".into(), 0.333)],
            ..SearchReport::default()
        };
        let text = render_element_report(&empty, "x", 0.5, &page, false, |_| None);
        assert_eq!(
            text,
            "No runs of page elements scored at or above 0.500 for query \"x\" (3 scored, 3 requests).\n\
             Highest scores: elements 1-12 (0.333). Lower the threshold to see them."
        );
    }

    /// The summary keeps every note `render_report` prints and drops only the hit lines.
    #[test]
    fn the_summary_keeps_the_notes_and_drops_the_hits() {
        let report = SearchReport {
            hits: vec![Hit {
                candidate: Candidate::new("element 1", "x"),
                score: 1.0,
                refined: true,
            }],
            coarse_count: 2,
            requests: 3,
            failed_requests: 1,
            refine_budget_exhausted: true,
            ..SearchReport::default()
        };
        let summary = report_summary(&report, "q", 0.5, "runs of page elements");
        assert_eq!(summary.lines().count(), 3, "{summary}");
        assert!(summary.contains("1 request failed"), "{summary}");
        assert!(summary.contains("refinement budget ran out"), "{summary}");
        assert!(!summary.contains("element 1"), "{summary}");
    }

    #[test]
    fn the_log_source_defaults_to_both_and_refuses_anything_else() {
        assert_eq!(parse_source(&input(&[])).unwrap(), LogSource::All);
        assert_eq!(
            parse_source(&input(&[("source", json!("all"))])).unwrap(),
            LogSource::All
        );
        assert_eq!(
            parse_source(&input(&[("source", json!(" console "))])).unwrap(),
            LogSource::Console
        );
        assert_eq!(
            parse_source(&input(&[("source", json!("server"))])).unwrap(),
            LogSource::Server
        );
        assert_eq!(
            parse_source(&input(&[("source", Value::Null)])).unwrap(),
            LogSource::All
        );
        let error = parse_source(&input(&[("source", json!("network"))])).unwrap_err();
        assert!(error.contains("\"network\""), "{error}");
        assert!(parse_source(&input(&[("source", json!(3))])).is_err());
        assert!(LogSource::All.reads_console() && LogSource::All.reads_server());
        assert!(!LogSource::Console.reads_server());
        assert!(!LogSource::Server.reads_console());
    }

    /// A missing dev server is an answer for `all`, a failure only when the call wanted nothing
    /// else, and the merged candidate labels keep saying which log a chunk came from.
    #[test]
    fn a_missing_dev_server_is_reported_rather_than_raised() {
        assert!(NO_RUNNING_SERVER.starts_with("No dev server is running"));
        assert!(NO_RUNNING_SERVER.ends_with("only the page console was scored."));
        assert!(NO_SERVER_LOGS.contains("preview_start"), "{NO_SERVER_LOGS}");
        assert_eq!(
            scored_lines_line(1, 0),
            "Scored 1 console line and 0 server lines."
        );
        assert_eq!(
            scored_lines_line(40, 1),
            "Scored 40 console lines and 1 server line."
        );

        let console = (1..=20).map(|index| format!("[log] c{index}")).collect::<Vec<_>>();
        let server = (1..=20).map(|index| format!("s{index}")).collect::<Vec<_>>();
        let console_plan = chunk_lines(&console.join("\n"));
        let server_plan = chunk_lines(&server.join("\n"));
        let (mut coarse, mut by_label) = chunk_candidates("console", &console_plan.chunks);
        let (server_coarse, server_by_label) = chunk_candidates("server", &server_plan.chunks);
        coarse.extend(server_coarse);
        by_label.extend(server_by_label);
        assert_eq!(coarse[0].label, "console lines 1-16");
        assert_eq!(coarse[2].label, "server lines 1-16");
        assert_eq!(label_source(&coarse[0].label), "console");
        assert_eq!(label_source(&coarse[2].label), "server");
        let pieces = refine_line_chunk(label_source(&coarse[2].label), &by_label, &coarse[2]);
        assert_eq!(pieces[0].label, "server lines 1-3");
    }

    /// Argument errors surface before any credential is read, page touched or request made.
    #[test]
    fn missing_arguments_fail_before_anything_is_consulted() {
        assert!(parse_query(&input(&[("threshold", json!(0.5))]), "query")
            .unwrap_err()
            .contains("query"));
        assert!(parse_threshold(&input(&[("query", json!("q"))]))
            .unwrap_err()
            .contains("threshold"));
        assert!(parse_query(&input(&[("query", json!("  "))]), "query").is_err());
    }

    /// The console form filters by level exactly as the plain listing does, and `lines` is the
    /// same ceiling.
    #[test]
    fn console_arguments_follow_the_plain_listing() {
        assert_eq!(parse_console_level(&input(&[])).unwrap(), None);
        assert_eq!(parse_console_level(&input(&[("level", json!("all"))])).unwrap(), None);
        assert_eq!(
            parse_console_level(&input(&[("level", json!("warn"))])).unwrap(),
            Some("warn".to_owned())
        );
        assert!(parse_console_level(&input(&[("level", json!("info"))])).is_err());
        assert_eq!(parse_console_lines(&input(&[])).unwrap(), None);
        assert_eq!(parse_console_lines(&input(&[("lines", json!(0))])).unwrap(), Some(1));
        assert_eq!(parse_console_lines(&input(&[("lines", json!(5000))])).unwrap(), Some(200));
        assert!(parse_console_lines(&input(&[("lines", json!("ten"))])).is_err());
    }

    /// Only the base tool's own parameters travel with the selector, under the base tool's name.
    #[test]
    fn each_form_carries_its_own_base_parameters() {
        assert_eq!(DescribedAction::Click.tool(), PreviewTool::Click);
        assert_eq!(DescribedAction::Fill.tool(), PreviewTool::Fill);
        assert_eq!(DescribedAction::Inspect.tool(), PreviewTool::Inspect);
        assert_eq!(DescribedAction::Click.pass_through(), ["doubleClick"]);
        assert_eq!(DescribedAction::Fill.pass_through(), ["value"]);
        assert_eq!(DescribedAction::Inspect.pass_through(), ["styles"]);
        assert!(DescribedAction::Click.misses_are_errors());
        assert!(!DescribedAction::Inspect.misses_are_errors());
        assert_eq!(DescribedAction::Fill.name(), "preview_fill");
    }
}
