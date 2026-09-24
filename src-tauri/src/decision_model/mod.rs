//! Host-side decision model: TypeSafe's Jev, a "System One" model.
//!
//! Jev answers typed questions about a `state` instead of generating text: a `score` rates
//! content against ordered levels, a `choice` picks one option out of a fixed set. The host
//! uses it to keep bulk material — page elements, logs, file contents, directory listings,
//! command output — out of the conversation model's context. The material is chunked and
//! scored here, and only the chunks that clear the model's threshold (or the one chosen
//! element) go back to the conversation.
//!
//! The provider secret lives in the operating-system credential store under a reserved
//! synthetic provider ID, reusing the API credential implementation's locking and identity
//! semantics exactly as the search providers do (`web_search::credential_id`).

pub mod chunk;
pub mod jev;
pub mod search;

use crate::model::{ApiKeyStatus, JsonObject};

/// Prefix for synthetic provider IDs. It reserves a namespace because provider ID alone
/// determines credential identity and binding; `storage::validate_shape` rejects ordinary
/// providers using it.
pub const DECISION_PROVIDER_ID_PREFIX: &str = "decision-provider:";

/// Longest query the find tools accept. Long enough for a paragraph of intent, short enough
/// that the query never dominates the state Jev evaluates.
pub const MAX_QUERY_CHARS: usize = 2000;

/// A decision-model provider in the fixed catalog. One today; the enum keeps the settings page,
/// the credential namespace and the tool code from hard-coding a vendor string.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecisionProviderKind {
    Typesafe,
}

impl DecisionProviderKind {
    pub const CATALOG: &'static [Self] = &[Self::Typesafe];

    pub fn slug(self) -> &'static str {
        match self {
            Self::Typesafe => "typesafe",
        }
    }

    pub fn from_slug(slug: &str) -> Option<Self> {
        Self::CATALOG.iter().copied().find(|kind| kind.slug() == slug)
    }
}

/// Builds the synthetic provider ID that keys a catalog provider's secret.
pub fn credential_id(kind: DecisionProviderKind) -> String {
    format!("{DECISION_PROVIDER_ID_PREFIX}{}", kind.slug())
}

// ----------------------------------------------------------------- Credential commands
//
// Commands accept only catalog kind slugs, preventing orphaned credential records with no
// read path.

fn credential_target(kind_slug: &str) -> Result<String, String> {
    let kind = DecisionProviderKind::from_slug(kind_slug)
        .ok_or_else(|| format!("未知的决策模型提供商：{kind_slug}"))?;
    Ok(credential_id(kind))
}

pub fn save_provider_api_key(kind_slug: &str, api_key: &str) -> Result<ApiKeyStatus, String> {
    let target = credential_target(kind_slug)?;
    let api_key = api_key.trim();
    if api_key.is_empty() {
        return Err("决策模型提供商凭据不能为空".into());
    }
    // Keys enter HTTP authorization headers, where control characters are header-injection primitives.
    if api_key.chars().any(char::is_control) {
        return Err("决策模型提供商凭据不能包含控制字符".into());
    }
    crate::api::save_api_key(&target, api_key)
}

pub fn get_provider_key_status(kind_slug: &str) -> Result<ApiKeyStatus, String> {
    crate::api::api_key_status(&credential_target(kind_slug)?)
}

/// Returns plaintext only for the explicit reveal command.
pub fn reveal_provider_api_key(kind_slug: &str) -> Result<String, String> {
    crate::api::reveal_api_key(&credential_target(kind_slug)?)
}

pub fn delete_provider_api_key(kind_slug: &str) -> Result<ApiKeyStatus, String> {
    crate::api::delete_api_key(&credential_target(kind_slug)?)
}

/// Reads a provider's secret for a tool call. `None` is "not configured"; the caller turns
/// that into a configuration error the model can relay to the user.
///
/// Under test this never touches the operating-system store: it answers whatever
/// [`test_overrides::install`] put in place, and "not configured" otherwise.
pub fn stored_secret(kind: DecisionProviderKind) -> Option<String> {
    #[cfg(test)]
    {
        let _ = kind;
        return test_overrides::secret();
    }
    #[cfg(not(test))]
    {
        let id = credential_id(kind);
        crate::api::api_key_status(&id)
            .ok()
            .filter(|status| status.configured)
            .and_then(|_| crate::api::reveal_api_key(&id).ok())
            .map(|secret| secret.trim().to_owned())
            .filter(|secret| !secret.is_empty())
    }
}

/// Test seam for the credential and the endpoint, so a tool test can run against a fixture
/// server without a key in the operating-system store. Tests that install an override are
/// serialized on one lock, because the override is process-wide.
#[cfg(test)]
pub(crate) mod test_overrides {
    use std::sync::{Mutex, MutexGuard, OnceLock};

    struct Overrides {
        secret: Option<String>,
        endpoint: Option<String>,
    }

    static STATE: Mutex<Overrides> = Mutex::new(Overrides {
        secret: None,
        endpoint: None,
    });
    static SERIAL: OnceLock<Mutex<()>> = OnceLock::new();

    pub struct Guard {
        _serial: MutexGuard<'static, ()>,
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            let mut state = STATE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            state.secret = None;
            state.endpoint = None;
        }
    }

    /// Installs a secret and an endpoint for the rest of the test; both revert on drop.
    pub fn install(secret: &str, endpoint: &str) -> Guard {
        let serial = serialize();
        let mut state = STATE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        state.secret = Some(secret.to_owned());
        state.endpoint = Some(endpoint.to_owned());
        drop(state);
        Guard { _serial: serial }
    }

    /// Takes the same lock [`install`] takes, and puts nothing in place.
    ///
    /// A test that asserts what a decision tool does with **no** key has to hold this for the
    /// length of that assertion. The override is one process-wide slot: without the lock, a
    /// concurrent test that installed a key would make the tool succeed, and the assertion
    /// would read as if the key check were broken.
    pub fn without_key() -> Guard {
        let serial = serialize();
        let mut state = STATE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        state.secret = None;
        state.endpoint = None;
        drop(state);
        Guard { _serial: serial }
    }

    fn serialize() -> MutexGuard<'static, ()> {
        SERIAL
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn secret() -> Option<String> {
        STATE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .secret
            .clone()
    }

    pub fn endpoint() -> Option<String> {
        STATE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .endpoint
            .clone()
    }
}

/// What the model reads when a decision tool runs without a key.
pub const KEY_MISSING_HINT: &str = "TypeSafe API key is not configured. Ask the user to add it \
under Settings → Decision model providers → TypeSafe; the find/choice tools cannot run without it.";

/// A decision-model failure. The variants distinguish non-retryable configuration errors,
/// retryable transient failures, and cancellation of the turn that asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecisionError {
    /// Configuration prevents success until changed: no key, rejected key, malformed request.
    Config(String),
    /// This attempt failed, but retrying may succeed.
    Transient(String),
    /// The run or task was stopped while requests were in flight.
    Cancelled,
}

impl DecisionError {
    pub fn message(&self) -> String {
        match self {
            Self::Config(message) | Self::Transient(message) => message.clone(),
            Self::Cancelled => "The run or task was stopped while the decision model was still \
                                scoring; nothing was returned."
                .to_owned(),
        }
    }
}

impl std::fmt::Display for DecisionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message())
    }
}

// ----------------------------------------------------------------- Shared parameter parsing

/// Rounds to the three decimals the threshold parameter is specified in. Scores are reported
/// at the same precision so a model can feed a reported score straight back as a threshold.
pub fn round3(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

/// `threshold`: a number in `[0, 1]` with at most three decimal places. Extra decimals are
/// rounded rather than refused — the parameter's precision is a reporting contract, not a
/// validation trap.
pub fn parse_threshold(input: &JsonObject) -> Result<f64, String> {
    let value = input
        .get("threshold")
        .ok_or_else(|| "Missing parameter threshold".to_owned())?;
    let number = value
        .as_f64()
        .ok_or_else(|| "Parameter threshold must be a number between 0 and 1".to_owned())?;
    if !number.is_finite() || !(0.0..=1.0).contains(&number) {
        return Err("Parameter threshold must be a number between 0 and 1".to_owned());
    }
    Ok(round3(number))
}

/// The natural-language query a find tool scores candidates against.
pub fn parse_query(input: &JsonObject, key: &str) -> Result<String, String> {
    let value = input
        .get(key)
        .ok_or_else(|| format!("Missing parameter {key}"))?
        .as_str()
        .ok_or_else(|| format!("Parameter {key} must be a string"))?
        .trim();
    if value.is_empty() {
        return Err(format!("Parameter {key} cannot be empty"));
    }
    if value.chars().count() > MAX_QUERY_CHARS {
        return Err(format!(
            "Parameter {key} exceeds the {MAX_QUERY_CHARS}-character limit"
        ));
    }
    Ok(value.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn synthetic_provider_ids_are_namespaced_and_stable() {
        let id = credential_id(DecisionProviderKind::Typesafe);
        assert_eq!(id, "decision-provider:typesafe");
        assert!(id.starts_with(DECISION_PROVIDER_ID_PREFIX));
        // The search namespace and this one must never collide: both key the same store.
        assert!(!id.starts_with(crate::web_search::SEARCH_PROVIDER_ID_PREFIX));
    }

    #[test]
    fn credential_commands_accept_only_catalog_slugs() {
        assert_eq!(
            DecisionProviderKind::from_slug("typesafe"),
            Some(DecisionProviderKind::Typesafe)
        );
        assert_eq!(DecisionProviderKind::from_slug("TypeSafe"), None);
        let error = credential_target("openai").expect_err("unknown slug");
        assert!(error.contains("openai"), "{error}");
        let error = save_provider_api_key("typesafe", "  ").expect_err("empty key");
        assert!(error.contains("不能为空"), "{error}");
        let error = save_provider_api_key("typesafe", "sk\nabc").expect_err("control char");
        assert!(error.contains("控制字符"), "{error}");
    }

    #[test]
    fn threshold_is_a_unit_interval_number_rounded_to_three_decimals() {
        let parse = |value: serde_json::Value| {
            let mut input = JsonObject::new();
            input.insert("threshold".into(), value);
            parse_threshold(&input)
        };
        assert_eq!(parse(json!(0)).unwrap(), 0.0);
        assert_eq!(parse(json!(1)).unwrap(), 1.0);
        assert_eq!(parse(json!(0.5)).unwrap(), 0.5);
        assert_eq!(parse(json!(0.12345)).unwrap(), 0.123);
        assert_eq!(parse(json!(0.9995)).unwrap(), 1.0);
        assert!(parse(json!(-0.1)).is_err());
        assert!(parse(json!(1.001)).is_err());
        assert!(parse(json!("0.5")).is_err());
        assert!(parse_threshold(&JsonObject::new()).is_err());
    }

    #[test]
    fn query_is_trimmed_and_bounded() {
        let mut input = JsonObject::new();
        input.insert("query".into(), json!("  where is the login handler  "));
        assert_eq!(
            parse_query(&input, "query").unwrap(),
            "where is the login handler"
        );
        input.insert("query".into(), json!("   "));
        assert!(parse_query(&input, "query").is_err());
        input.insert("query".into(), json!("x".repeat(MAX_QUERY_CHARS + 1)));
        assert!(parse_query(&input, "query").is_err());
        assert!(parse_query(&JsonObject::new(), "query").is_err());
    }

    #[test]
    fn cancelled_errors_explain_that_nothing_came_back() {
        assert!(DecisionError::Cancelled.message().contains("stopped"));
        assert_eq!(DecisionError::Config("x".into()).message(), "x");
    }
}
