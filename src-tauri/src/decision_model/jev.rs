//! Wire protocol for TypeSafe's `POST /v1/systemone`.
//!
//! One request carries a `state` (the material to judge), a `model`, and a map of `questions`
//! keyed by IDs the caller picks; the response answers under the same keys. Every question
//! in a request is evaluated in parallel, and only input tokens are billed.
//!
//! Only the two question types the host needs are modelled: `score` (ordered levels, answered
//! with a probability-weighted position on the scale) and `choice` (a fixed option set,
//! answered with the top option and the full distribution).

use std::sync::Mutex;
use std::time::{Duration, Instant};

use reqwest::blocking::Client;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use reqwest::StatusCode;
use serde_json::{json, Map, Value};
use url::Url;

use crate::http_util::{api_error_message, read_body, sanitize_error};

use super::{round3, DecisionError, DecisionProviderKind, KEY_MISSING_HINT};

pub const JEV_ENDPOINT: &str = "https://api.typesafe.ai/v1/systemone";
/// The alias TypeSafe recommends; it resolves to the current versioned build and the response
/// reports which one answered.
pub const JEV_MODEL: &str = "jev-latest";

/// A `choice` question accepts at most this many options.
pub const MAX_CHOICE_OPTIONS: usize = 255;
/// Jev's context budget for the `state` plus the longest question of a request.
pub const STATE_TOKEN_LIMIT: usize = 32_000;

/// A rough token count, for budgeting a request against [`STATE_TOKEN_LIMIT`]: three ASCII
/// characters to a token, and one token for every other character — CJK text tokenizes close to
/// that. Rough on purpose, and on the high side.
pub fn estimate_tokens(text: &str) -> usize {
    let (ascii, other) = text.chars().fold((0_usize, 0_usize), |(ascii, other), character| {
        if character.is_ascii() {
            (ascii + 1, other)
        } else {
            (ascii, other + 1)
        }
    });
    ascii.div_ceil(3) + other
}

const MAX_RESPONSE_BODY: usize = 1024 * 1024;
/// Jev answers in well under a second; anything slower than this is the network, not the model.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Back-off for `429 Too Many Requests` and `529 Overloaded`, the two statuses TypeSafe
/// documents as retryable. A `retry-after` header, when present, overrides the delay.
const RETRY_DELAYS: [Duration; 2] = [Duration::from_secs(1), Duration::from_secs(2)];
const MAX_RETRY_AFTER: Duration = Duration::from_secs(10);
/// Sustained request rate, shared by every tool call in the process. TypeSafe publishes
/// 1,200 requests a minute per account — twenty a second — and returns `429` past it; a
/// little under that leaves room for the retries themselves. Tool calls cut their material
/// into as many requests as it takes, so a large file would otherwise run straight into the
/// limit and come back with half its chunks unscored.
const REQUESTS_PER_SECOND: f64 = 18.0;
/// Requests that may start at once after a quiet spell, so an ordinary call of a few dozen
/// chunks is not slowed down at all.
const REQUEST_BURST: f64 = 32.0;

static PACER: Mutex<Pacer> = Mutex::new(Pacer::new());

/// One `score` question: the rating instructions and the ordered levels, low to high. Jev
/// judges each level independently against the state and never sees level numbers, so a
/// level must describe itself without referring to its neighbours.
///
/// The instructions point at one candidate of the state by `{c}` — `candidates[3]` — because a
/// request carries several candidates, each with a question of its own; TypeSafe's own examples
/// point a question at one item of the state the same way.
#[derive(Clone, Copy, Debug)]
pub struct ScoreRubric {
    pub instructions: &'static str,
    /// Two to ten levels; index is the level number.
    pub levels: &'static [&'static str],
}

impl ScoreRubric {
    fn question_about(&self, candidate: &str) -> Value {
        json!({
            "type": "score",
            "instructions": self.instructions.replace("{c}", candidate),
            "criteria": self.levels,
        })
    }
}

/// How well a block of text contains what a query asks for. The levels are about *containing*
/// rather than *being about*, because a block is usually a slice of something larger — and, cut
/// into parts, a slice of a block — and carries unrelated material around the part that matters.
pub const RELEVANCE_RUBRIC: ScoreRubric = ScoreRubric {
    instructions: "Rate how well `{c}.text` contains what `query` asks for. Judge only that one \
                   candidate's content — the other candidates in the state are separate material; \
                   it may contain unrelated material as well, which does not count against it. \
                   `{c}.source` says where the text came from.",
    levels: &[
        "Unrelated: nothing in the candidate has to do with what the query asks for.",
        "Loosely related: the candidate shares a topic or a keyword with the query but does \
         not contain what it asks for.",
        "Relevant: the candidate contains part of what the query asks for, or strong evidence \
         that the answer is in it.",
        "Direct hit: the candidate contains exactly what the query asks for.",
    ],
};

/// How well one page element — a line of an accessibility snapshot — matches a description of
/// the element to act on.
pub const ELEMENT_RUBRIC: ScoreRubric = ScoreRubric {
    instructions: "Rate how well the page element `{c}.text` is the element that `query` \
                   describes. Elements are lines of an accessibility snapshot, formatted as \
                   `[uid] role: \"name\" (value: \"...\")`, indented by depth; `{c}.context` \
                   holds the neighbouring lines. Judge that one element by role, name, value and \
                   position; the other candidates in the state are other elements.",
    levels: &[
        "Not the element: role, name and context do not match the description.",
        "Similar: the same role or a related name, but probably not the described element.",
        "Likely: role and name fit the description.",
        "Exactly the described element.",
    ],
};

/// One `score` answer, reduced to what the host acts on.
#[derive(Clone, Debug, PartialEq)]
pub struct ScoreAnswer {
    /// Probability-weighted level, `0.0..=(levels - 1)`.
    pub score: f64,
    pub confidence: f64,
    pub levels: usize,
}

impl ScoreAnswer {
    /// The score on a `0..=1` scale, rounded to the threshold's precision.
    pub fn normalized(&self) -> f64 {
        let top = self.levels.saturating_sub(1).max(1) as f64;
        round3((self.score / top).clamp(0.0, 1.0))
    }
}

/// One `choice` answer. `probabilities` is sorted highest first and sums to one.
#[derive(Clone, Debug, PartialEq)]
pub struct ChoiceAnswer {
    pub choice: String,
    pub confidence: f64,
    pub probabilities: Vec<(String, f64)>,
}

pub struct SystemOneResponse {
    pub answers: Map<String, Value>,
}

/// A client bound to one key. Cloning is cheap: the reqwest client is a handle to a shared pool.
#[derive(Clone)]
pub struct Jev {
    client: Client,
    api_key: String,
    endpoint: Url,
    model: String,
}

impl Jev {
    /// The client a tool call uses: the stored TypeSafe key against the production endpoint.
    ///
    /// A browser-dev build honours `MEWORK_TYPESAFE_ENDPOINT` the way the Codex login honours
    /// `MEWORK_CODEX_OAUTH_ISSUER`: it lets the in-app end-to-end runs talk to a local stand-in
    /// for Jev. Release builds ignore the variable.
    pub fn from_credentials() -> Result<Self, DecisionError> {
        let api_key = super::stored_secret(DecisionProviderKind::Typesafe)
            .ok_or_else(|| DecisionError::Config(KEY_MISSING_HINT.to_owned()))?;
        #[cfg(test)]
        let endpoint = super::test_overrides::endpoint().unwrap_or_else(|| JEV_ENDPOINT.to_owned());
        #[cfg(all(not(test), feature = "browser-dev"))]
        let endpoint = std::env::var("MEWORK_TYPESAFE_ENDPOINT")
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| JEV_ENDPOINT.to_owned());
        #[cfg(all(not(test), not(feature = "browser-dev")))]
        let endpoint = JEV_ENDPOINT.to_owned();
        let endpoint = Url::parse(&endpoint)
            .map_err(|error| DecisionError::Config(format!("Bad Jev endpoint: {error}")))?;
        Self::new(api_key, endpoint)
    }

    pub fn new(api_key: String, endpoint: Url) -> Result<Self, DecisionError> {
        let client = crate::http_util::client().map_err(DecisionError::Transient)?;
        Ok(Self {
            client,
            api_key,
            endpoint,
            model: JEV_MODEL.to_owned(),
        })
    }

    /// Asks one `score` question about each of the `count` candidates in `state`'s `candidates`
    /// array, all in one request. An answer that is missing or malformed is `None`; the others
    /// stand.
    pub fn score_each(
        &self,
        state: Value,
        rubric: &ScoreRubric,
        count: usize,
    ) -> Result<Vec<Option<ScoreAnswer>>, DecisionError> {
        let questions = (0..count)
            .map(|index| {
                (
                    format!("c{index}"),
                    rubric.question_about(&format!("candidates[{index}]")),
                )
            })
            .collect::<Map<_, _>>();
        let response = self.system_one(state, Value::Object(questions))?;
        Ok((0..count)
            .map(|index| {
                response
                    .answers
                    .get(&format!("c{index}"))
                    .and_then(|answer| parse_score_answer(answer).ok())
            })
            .collect())
    }

    /// Asks one `choice` question about `state`. `options` are `(name, description)` pairs;
    /// the name is what comes back as the choice.
    pub fn choose(
        &self,
        state: Value,
        instructions: &str,
        options: &[(String, String)],
    ) -> Result<ChoiceAnswer, DecisionError> {
        if options.len() < 2 {
            return Err(DecisionError::Config(
                "A choice needs at least two options".to_owned(),
            ));
        }
        if options.len() > MAX_CHOICE_OPTIONS {
            return Err(DecisionError::Config(format!(
                "A choice accepts at most {MAX_CHOICE_OPTIONS} options; got {}",
                options.len()
            )));
        }
        let criteria = options
            .iter()
            .map(|(name, description)| (name.clone(), Value::String(description.clone())))
            .collect::<Map<_, _>>();
        let question = json!({
            "type": "choice",
            "instructions": instructions,
            "criteria": criteria,
        });
        let response = self.system_one(state, json!({ "answer": question }))?;
        let answer = response
            .answers
            .get("answer")
            .ok_or_else(|| DecisionError::Transient("Jev returned no answer".to_owned()))?;
        parse_choice_answer(answer)
    }

    /// One raw request. Retries the two documented retryable statuses with a short back-off.
    /// Every attempt, retries included, first waits for a slot under the process-wide rate.
    pub fn system_one(
        &self,
        state: Value,
        questions: Value,
    ) -> Result<SystemOneResponse, DecisionError> {
        let body = json!({
            "model": self.model,
            "state": state,
            "questions": questions,
        });
        let mut attempt = 0;
        loop {
            pace();
            match self.send_once(&body) {
                Ok(response) => return Ok(response),
                Err(Attempt::Retry { after, error }) => {
                    if attempt >= RETRY_DELAYS.len() {
                        return Err(error);
                    }
                    std::thread::sleep(after.unwrap_or(RETRY_DELAYS[attempt]));
                    attempt += 1;
                }
                Err(Attempt::Fail(error)) => return Err(error),
            }
        }
    }

    fn send_once(&self, body: &Value) -> Result<SystemOneResponse, Attempt> {
        let key = Some(self.api_key.as_str());
        let response = self
            .client
            .post(self.endpoint.clone())
            .header(AUTHORIZATION, format!("Bearer {}", self.api_key))
            .header(CONTENT_TYPE, "application/json")
            .timeout(REQUEST_TIMEOUT)
            .json(body)
            .send()
            .map_err(|error| {
                Attempt::Fail(DecisionError::Transient(sanitize_error(
                    &format!("TypeSafe request failed: {error}"),
                    key,
                )))
            })?;
        let status = response.status();
        let retry_after = response
            .headers()
            .get("retry-after")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.trim().parse::<u64>().ok())
            .map(|seconds| Duration::from_secs(seconds).min(MAX_RETRY_AFTER));
        let (bytes, _truncated) = read_body(response, MAX_RESPONSE_BODY).map_err(|error| {
            Attempt::Fail(DecisionError::Transient(sanitize_error(&error, key)))
        })?;
        if !status.is_success() {
            let message = sanitize_error(&api_error_message(status, &bytes), key);
            return Err(match status {
                StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                    Attempt::Fail(DecisionError::Config(format!(
                        "TypeSafe rejected the API key (HTTP {}). Check the key under \
                         Settings → Decision model providers → TypeSafe.",
                        status.as_u16()
                    )))
                }
                StatusCode::UNPROCESSABLE_ENTITY => Attempt::Fail(DecisionError::Config(
                    format!("TypeSafe rejected the request: {message}"),
                )),
                StatusCode::TOO_MANY_REQUESTS => Attempt::Retry {
                    after: retry_after,
                    error: DecisionError::Transient(format!(
                        "TypeSafe rate limit exceeded: {message}"
                    )),
                },
                status if status.as_u16() == 529 => Attempt::Retry {
                    after: retry_after,
                    error: DecisionError::Transient(format!("TypeSafe is overloaded: {message}")),
                },
                _ => Attempt::Fail(DecisionError::Transient(format!(
                    "TypeSafe request failed: {message}"
                ))),
            });
        }
        let payload: Value = serde_json::from_slice(&bytes).map_err(|error| {
            Attempt::Fail(DecisionError::Transient(format!(
                "TypeSafe returned invalid JSON: {error}"
            )))
        })?;
        let answers = payload
            .get("answers")
            .and_then(Value::as_object)
            .cloned()
            .ok_or_else(|| {
                Attempt::Fail(DecisionError::Transient(
                    "TypeSafe response carries no answers".to_owned(),
                ))
            })?;
        Ok(SystemOneResponse { answers })
    }
}

/// A token bucket over request starts: [`REQUEST_BURST`] slots, refilled at
/// [`REQUESTS_PER_SECOND`].
struct Pacer {
    slots: f64,
    updated: Option<Instant>,
}

impl Pacer {
    const fn new() -> Self {
        Self {
            slots: REQUEST_BURST,
            updated: None,
        }
    }

    /// Takes one slot at `now`, or says how long until one is free.
    ///
    /// A slot counts as free a hair before the refill reaches exactly one: waiting the quoted
    /// time lands a rounding error short of it, and the quote for that remainder would round to
    /// zero nanoseconds — a wait that never ends on a clock that does not move by itself.
    fn take(&mut self, now: Instant) -> Result<(), Duration> {
        if let Some(updated) = self.updated {
            let refill = now.saturating_duration_since(updated).as_secs_f64() * REQUESTS_PER_SECOND;
            self.slots = (self.slots + refill).min(REQUEST_BURST);
        }
        self.updated = Some(now);
        if self.slots >= 1.0 - 1e-6 {
            self.slots = (self.slots - 1.0).max(0.0);
            return Ok(());
        }
        Err(Duration::from_secs_f64(
            (1.0 - self.slots) / REQUESTS_PER_SECOND,
        ))
    }
}

/// Blocks until the process-wide pacer gives this request a slot. The lock is held only to
/// take or price a slot, never across the sleep.
fn pace() {
    loop {
        let taken = PACER
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take(Instant::now());
        match taken {
            Ok(()) => return,
            Err(wait) => std::thread::sleep(wait),
        }
    }
}

enum Attempt {
    Retry {
        after: Option<Duration>,
        error: DecisionError,
    },
    Fail(DecisionError),
}

pub fn parse_score_answer(value: &Value) -> Result<ScoreAnswer, DecisionError> {
    let kind = value.get("type").and_then(Value::as_str).unwrap_or_default();
    if kind != "score" {
        return Err(DecisionError::Transient(format!(
            "Expected a score answer, got {kind:?}"
        )));
    }
    let score = value
        .get("score")
        .and_then(Value::as_f64)
        .ok_or_else(|| DecisionError::Transient("Score answer carries no score".to_owned()))?;
    let levels = value
        .get("legend")
        .and_then(Value::as_object)
        .map(Map::len)
        .or_else(|| {
            value
                .get("probabilities")
                .and_then(Value::as_object)
                .map(Map::len)
        })
        .filter(|levels| *levels >= 2)
        .ok_or_else(|| {
            DecisionError::Transient("Score answer carries no level legend".to_owned())
        })?;
    Ok(ScoreAnswer {
        score,
        confidence: value
            .get("confidence")
            .and_then(Value::as_f64)
            .unwrap_or(0.0),
        levels,
    })
}

pub fn parse_choice_answer(value: &Value) -> Result<ChoiceAnswer, DecisionError> {
    let kind = value.get("type").and_then(Value::as_str).unwrap_or_default();
    if kind != "choice" {
        return Err(DecisionError::Transient(format!(
            "Expected a choice answer, got {kind:?}"
        )));
    }
    let choice = value
        .get("choice")
        .and_then(Value::as_str)
        .filter(|choice| !choice.is_empty())
        .ok_or_else(|| DecisionError::Transient("Choice answer carries no choice".to_owned()))?
        .to_owned();
    let mut probabilities = value
        .get("probabilities")
        .and_then(Value::as_object)
        .map(|map| {
            map.iter()
                .map(|(name, probability)| {
                    (name.clone(), probability.as_f64().unwrap_or(0.0))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    probabilities.sort_by(|left, right| {
        right
            .1
            .partial_cmp(&left.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.0.cmp(&right.0))
    });
    Ok(ChoiceAnswer {
        choice,
        confidence: value
            .get("confidence")
            .and_then(Value::as_f64)
            .unwrap_or(0.0),
        probabilities,
    })
}

#[cfg(test)]
pub(crate) mod fixture {
    //! A one-shot HTTP server for the wire tests: it answers one request with a fixed status
    //! and body and hands back what it received.

    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpListener};
    use std::time::Duration;

    pub struct Received {
        pub head: String,
        pub body: String,
    }

    pub fn serve(
        responses: Vec<(u16, &'static str, &'static str)>,
    ) -> (SocketAddr, std::thread::JoinHandle<Vec<Received>>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fixture");
        let address = listener.local_addr().expect("fixture address");
        let handle = std::thread::spawn(move || {
            let mut received = Vec::new();
            for (status, extra_headers, body) in responses {
                let (mut stream, _) = listener.accept().expect("accept fixture");
                stream
                    .set_read_timeout(Some(Duration::from_secs(10)))
                    .expect("read timeout");
                let (head, payload) = read_request(&mut stream);
                let reason = match status {
                    200 => "OK",
                    401 => "Unauthorized",
                    422 => "Unprocessable Entity",
                    429 => "Too Many Requests",
                    529 => "Overloaded",
                    _ => "Status",
                };
                let response = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\n{extra_headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream
                    .write_all(response.as_bytes())
                    .expect("write response");
                stream.flush().ok();
                received.push(Received {
                    head,
                    body: payload,
                });
            }
            received
        });
        (address, handle)
    }

    pub fn client_against(address: SocketAddr) -> super::Jev {
        super::Jev::new(
            "sk-typesafe-test".into(),
            url::Url::parse(&format!("http://{address}/v1/systemone")).unwrap(),
        )
        .expect("client")
    }

    /// A concurrent fixture that plays Jev: every connection is answered on its own thread, every
    /// question `c{i}` of it with a `score` answer computed by `level_of` from the candidate it
    /// is about, `state.candidates[i]`, on the four-level rubrics. It serves until dropped; `requests()` returns what it saw so far.
    pub struct ScoringServer {
        pub address: SocketAddr,
        seen: std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
        stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    }

    impl ScoringServer {
        pub fn start(
            level_of: impl Fn(&serde_json::Value) -> f64 + Send + Sync + 'static,
        ) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind fixture");
            listener.set_nonblocking(true).expect("nonblocking");
            let address = listener.local_addr().expect("fixture address");
            let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let level_of = std::sync::Arc::new(level_of);
            let (seen_for_thread, stop_for_thread) = (seen.clone(), stop.clone());
            std::thread::spawn(move || {
                while !stop_for_thread.load(std::sync::atomic::Ordering::Acquire) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            let seen = seen_for_thread.clone();
                            let level_of = level_of.clone();
                            std::thread::spawn(move || {
                                stream.set_nonblocking(false).expect("blocking stream");
                                stream
                                    .set_read_timeout(Some(Duration::from_secs(10)))
                                    .expect("read timeout");
                                let (_head, body) = read_request(&mut stream);
                                let request: serde_json::Value =
                                    serde_json::from_str(&body).expect("json request");
                                // Every question `c{i}` is about `candidates[i]`.
                                let answers = request["questions"]
                                    .as_object()
                                    .map(|questions| {
                                        questions
                                            .keys()
                                            .map(|key| {
                                                let index = key
                                                    .trim_start_matches('c')
                                                    .parse::<usize>()
                                                    .unwrap_or(usize::MAX);
                                                let level = level_of(
                                                    &request["state"]["candidates"][index],
                                                )
                                                .clamp(0.0, 3.0);
                                                format!(
                                                    r#""{key}":{{"type":"score","score":{level},"legend":{{"0":"a","1":"b","2":"c","3":"d"}},"probabilities":{{"0":0,"1":0,"2":0,"3":1}},"confidence":1}}"#
                                                )
                                            })
                                            .collect::<Vec<_>>()
                                            .join(",")
                                    })
                                    .unwrap_or_default();
                                seen.lock().unwrap().push(request);
                                let payload = format!(
                                    r#"{{"model":"jev-1.13.0","answers":{{{answers}}},"usage":{{"input_tokens":10,"output_tokens":1}}}}"#
                                );
                                let response = format!(
                                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                                    payload.len()
                                );
                                let _ = stream.write_all(response.as_bytes());
                                let _ = stream.flush();
                            });
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(5));
                        }
                        Err(_) => break,
                    }
                }
            });
            Self {
                address,
                seen,
                stop,
            }
        }

        pub fn endpoint(&self) -> String {
            format!("http://{}/v1/systemone", self.address)
        }

        pub fn requests(&self) -> Vec<serde_json::Value> {
            self.seen.lock().unwrap().clone()
        }
    }

    impl Drop for ScoringServer {
        fn drop(&mut self) {
            self.stop
                .store(true, std::sync::atomic::Ordering::Release);
        }
    }

    /// Reads one HTTP request and returns its head and body. Framing is done on bytes: a
    /// lossy text conversion of a partial multi-byte character would miscount the body.
    fn read_request(stream: &mut std::net::TcpStream) -> (String, String) {
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 4096];
        loop {
            let read = stream.read(&mut buffer).expect("read request");
            if read == 0 {
                panic!("fixture connection closed before a full request arrived");
            }
            bytes.extend_from_slice(&buffer[..read]);
            let Some(split) = bytes.windows(4).position(|window| window == b"\r\n\r\n") else {
                continue;
            };
            let head = String::from_utf8_lossy(&bytes[..split]).into_owned();
            let length = head
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or(0);
            let body = &bytes[split + 4..];
            if body.len() >= length {
                return (head, String::from_utf8_lossy(&body[..length]).into_owned());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fixture::{client_against, serve};
    use super::*;

    const SCORE_BODY: &str = r#"{"model":"jev-1.13.0","answers":{"c0":{"type":"score","score":2.4,"legend":{"0":"a","1":"b","2":"c","3":"d"},"probabilities":{"0":0,"1":0.1,"2":0.4,"3":0.5},"confidence":0.62},"c1":{"type":"choice","choice":"x"}},"usage":{"input_tokens":120,"output_tokens":7}}"#;

    /// The request is the documented shape: bearer key, `model`, `state`, and one `score`
    /// question per candidate with ordered `criteria`, each pointed at its own candidate. The
    /// answers' weighted levels are normalised onto the threshold scale; a malformed or missing
    /// answer is that candidate's alone.
    #[test]
    fn a_score_request_asks_about_every_candidate_and_normalises_the_answers() {
        let (address, server) = serve(vec![(200, "", SCORE_BODY)]);
        let jev = client_against(address);
        let answers = jev
            .score_each(
                json!({"query": "q", "candidates": [
                    {"source": "x", "text": "t"},
                    {"source": "y", "text": "u"},
                    {"source": "z", "text": "v"}
                ]}),
                &RELEVANCE_RUBRIC,
                3,
            )
            .expect("score");
        let received = server.join().expect("fixture thread").remove(0);

        assert!(received.head.starts_with("POST /v1/systemone "), "{}", received.head);
        assert!(
            received
                .head
                .lines()
                .any(|line| line.eq_ignore_ascii_case("authorization: Bearer sk-typesafe-test")),
            "{}",
            received.head
        );
        let body: Value = serde_json::from_str(&received.body).unwrap();
        assert_eq!(body["model"], JEV_MODEL);
        assert_eq!(body["state"]["query"], "q");
        let questions = body["questions"].as_object().unwrap();
        assert_eq!(questions.len(), 3);
        assert_eq!(questions["c2"]["type"], "score");
        let instructions = questions["c2"]["instructions"].as_str().unwrap();
        assert!(instructions.contains("`candidates[2].text`"), "{instructions}");
        assert!(!instructions.contains("{c}"), "{instructions}");
        assert_eq!(
            questions["c0"]["criteria"].as_array().unwrap().len(),
            RELEVANCE_RUBRIC.levels.len()
        );
        let first = answers[0].as_ref().expect("the first answer");
        assert_eq!(first.levels, 4);
        assert_eq!(first.score, 2.4);
        assert_eq!(first.normalized(), 0.8);
        assert_eq!(first.confidence, 0.62);
        assert_eq!(answers[1], None, "an answer of the wrong type");
        assert_eq!(answers[2], None, "no answer at all");
    }

    #[test]
    fn a_choice_request_maps_options_to_criteria_and_sorts_the_distribution() {
        let (address, server) = serve(vec![(
            200,
            "",
            r#"{"model":"jev-1.13.0","answers":{"answer":{"type":"choice","choice":"42","confidence":0.8,"probabilities":{"42":0.87,"none":0.0,"7":0.13}}},"usage":{"input_tokens":1,"output_tokens":1}}"#,
        )]);
        let jev = client_against(address);
        let answer = jev
            .choose(
                json!({"description": "the save button", "elements": "[7] link\n[42] button: \"Save\""}),
                "Which element?",
                &[
                    ("7".to_owned(), "link".to_owned()),
                    ("42".to_owned(), "button \"Save\"".to_owned()),
                    ("none".to_owned(), "No element matches".to_owned()),
                ],
            )
            .expect("choice");
        let received = server.join().expect("fixture thread").remove(0);
        let body: Value = serde_json::from_str(&received.body).unwrap();
        assert_eq!(body["questions"]["answer"]["type"], "choice");
        assert_eq!(body["questions"]["answer"]["criteria"]["42"], "button \"Save\"");
        assert_eq!(answer.choice, "42");
        assert_eq!(answer.probabilities[0], ("42".to_owned(), 0.87));
        assert_eq!(answer.probabilities[1], ("7".to_owned(), 0.13));
        assert_eq!(answer.probabilities[2], ("none".to_owned(), 0.0));
    }

    #[test]
    fn a_choice_refuses_option_sets_the_api_would_refuse() {
        let jev = client_against("127.0.0.1:1".parse().unwrap());
        let one = vec![("a".to_owned(), String::new())];
        assert!(matches!(
            jev.choose(json!({}), "?", &one),
            Err(DecisionError::Config(_))
        ));
        let many = (0..=MAX_CHOICE_OPTIONS)
            .map(|index| (index.to_string(), String::new()))
            .collect::<Vec<_>>();
        assert!(matches!(
            jev.choose(json!({}), "?", &many),
            Err(DecisionError::Config(_))
        ));
    }

    /// A rejected key is configuration, not weather: the tool must say so instead of retrying.
    #[test]
    fn an_unauthorized_response_is_a_configuration_error_without_the_key_in_it() {
        let (address, server) = serve(vec![(401, "", r#"{"error":"bad key sk-typesafe-test"}"#)]);
        let jev = client_against(address);
        let error = jev
            .score_each(json!({}), &RELEVANCE_RUBRIC, 1)
            .expect_err("401 must fail");
        server.join().unwrap();
        match error {
            DecisionError::Config(message) => {
                assert!(message.contains("401"), "{message}");
                assert!(!message.contains("sk-typesafe-test"), "{message}");
            }
            other => panic!("expected a configuration error, got {other:?}"),
        }
    }

    #[test]
    fn a_validation_failure_reports_the_upstream_detail() {
        let (address, server) = serve(vec![(
            422,
            "",
            r#"{"error":{"message":"questions.answer.criteria must have at least 2 levels"}}"#,
        )]);
        let jev = client_against(address);
        let error = jev
            .score_each(json!({}), &RELEVANCE_RUBRIC, 1)
            .expect_err("422 must fail");
        server.join().unwrap();
        match error {
            DecisionError::Config(message) => {
                assert!(message.contains("at least 2 levels"), "{message}")
            }
            other => panic!("expected a configuration error, got {other:?}"),
        }
    }

    /// 429 is retried once the server says it is fine to, and the retry's answer is the result.
    #[test]
    fn a_rate_limited_request_is_retried_after_the_advertised_delay() {
        let (address, server) = serve(vec![
            (429, "Retry-After: 0\r\n", r#"{"error":"slow down"}"#),
            (200, "", SCORE_BODY),
        ]);
        let jev = client_against(address);
        let answer = jev
            .score_each(json!({}), &RELEVANCE_RUBRIC, 1)
            .expect("retried")
            .remove(0)
            .expect("an answer");
        let received = server.join().unwrap();
        assert_eq!(received.len(), 2);
        assert_eq!(answer.normalized(), 0.8);
    }

    /// A burst goes out at once; past it, requests start at the sustained rate, and a quiet
    /// spell refills the bucket but never beyond the burst.
    #[test]
    fn the_pacer_allows_a_burst_then_holds_the_sustained_rate() {
        let start = Instant::now();
        let mut pacer = Pacer::new();
        for _ in 0..REQUEST_BURST as usize {
            assert_eq!(pacer.take(start), Ok(()));
        }
        let wait = pacer.take(start).expect_err("the burst is spent");
        let interval = 1.0 / REQUESTS_PER_SECOND;
        assert!((wait.as_secs_f64() - interval).abs() < 1e-6, "{wait:?}");
        assert_eq!(pacer.take(start + wait), Ok(()));
        // A minute of silence refills the bucket to the burst, not to a minute's worth.
        let later = start + Duration::from_secs(60);
        for _ in 0..REQUEST_BURST as usize {
            assert_eq!(pacer.take(later), Ok(()));
        }
        assert!(pacer.take(later).is_err());
        // Twenty seconds of back-to-back requests start no faster than the sustained rate — and
        // every quoted wait moves the clock, so the loop ends (bounded, in case one does not).
        let mut now = later;
        let mut started = 0;
        let end = later + Duration::from_secs(20);
        for _ in 0..10_000 {
            if now >= end {
                break;
            }
            match pacer.take(now) {
                Ok(()) => started += 1,
                Err(wait) => {
                    assert!(!wait.is_zero(), "a zero wait would spin");
                    now += wait;
                }
            }
        }
        assert!(now >= end, "the pacer stopped advancing the clock");
        assert!(
            (started as f64 - 20.0 * REQUESTS_PER_SECOND).abs() <= 1.0,
            "{started} requests in 20 s"
        );
    }

    #[test]
    fn answers_of_the_wrong_type_are_transient_failures() {
        let score = json!({"type": "choice", "choice": "x"});
        assert!(matches!(
            parse_score_answer(&score),
            Err(DecisionError::Transient(_))
        ));
        let choice = json!({"type": "score", "score": 1.0});
        assert!(matches!(
            parse_choice_answer(&choice),
            Err(DecisionError::Transient(_))
        ));
        // A legend-less score falls back to the probability keys for its level count.
        let bare = json!({"type": "score", "score": 1.0, "probabilities": {"0": 0.0, "1": 1.0}});
        assert_eq!(parse_score_answer(&bare).unwrap().levels, 2);
        assert_eq!(parse_score_answer(&bare).unwrap().normalized(), 1.0);
    }
}
