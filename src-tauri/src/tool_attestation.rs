//! Per-card proof that a tool result came from this application's own
//! execution rather than from the renderer.
//!
//! # Why this is not a receipt book
//!
//! The original design kept a process-local `HashMap` of every executed
//! payload and asked, at save time, whether the card being saved was in it.
//! That map is bounded (512 entries), lives only in memory, and is keyed by the
//! full serialized payload. Each of those three properties turned an ordinary
//! session into an unusable app:
//!
//! - **Bounded**: past 512 tool calls the oldest entries were evicted, so a
//!   card that had not been saved yet could lose its proof to unrelated later
//!   activity.
//! - **In memory**: a restart discarded every proof. Anything executed but not
//!   yet persisted could never be saved again.
//! - **Keyed by payload bytes**: any difference at all between the bytes the
//!   host attested and the bytes the renderer returned — a float that crossed
//!   JavaScript, a re-serialized object, a field the renderer rebuilt — meant
//!   no match, and the mismatch was permanent because the card kept coming back.
//!
//! A token inverts that. The host computes a MAC over the card's meaning and
//! hands it to the renderer *with* the card; the renderer stores it like any
//! other field and returns it on save; the host recomputes the MAC and compares.
//! Nothing has to be remembered between the execution and the save, so there is
//! no capacity to exhaust and nothing a restart can lose. The proof travels
//! with the thing it proves.
//!
//! # What the token commits to
//!
//! The MAC covers exactly what must not change after execution: the owning
//! conversation, the card's identity, the tool, its executed and requested
//! input, the result, and the complete recursive child record. It deliberately
//! does *not* cover the workspace path — a workspace can be renamed or moved on
//! disk without any of the above changing meaning — nor timestamps that the
//! host itself rewrites.
//!
//! # The key
//!
//! One random key per app-data directory, stored beside the document. It is not
//! a user secret and does not protect against someone who can read the data
//! directory: anyone with that access can edit the document itself. It exists
//! to stop the *renderer* — which is the untrusted party here — from writing a
//! tool result the host never produced. Losing the key is survivable: cards
//! attested under it stop verifying and are quarantined, exactly as if they had
//! been tampered with, and everything else still saves.

use std::{
    fs,
    path::{Path, PathBuf},
};

use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::model::{JsonObject, SubagentRunRecord, ToolResult};

type HmacSha256 = Hmac<Sha256>;

/// Everything a tool card's token commits to.
pub struct AttestationSubject<'a> {
    pub conversation_id: &'a str,
    pub context_id: &'a str,
    pub tool_name: &'a str,
    pub input: &'a JsonObject,
    pub requested_input: Option<&'a JsonObject>,
    pub result: &'a ToolResult,
    pub subagent: Option<&'a SubagentRunRecord>,
}

/// The process-wide signing key for one app-data directory.
#[derive(Clone)]
pub struct AttestationKey {
    secret: [u8; 32],
}

impl std::fmt::Debug for AttestationKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never render the bytes: this type ends up inside larger Debug output.
        formatter
            .debug_struct("AttestationKey")
            .finish_non_exhaustive()
    }
}

/// Where the key lives relative to the app-data directory.
const KEY_FILE_NAME: &str = "tool-attestation.key";

impl AttestationKey {
    /// Loads the app-data directory's key, creating it on first use.
    ///
    /// A key that cannot be read is replaced rather than treated as fatal. The
    /// cost of a fresh key is that previously attested cards stop verifying and
    /// are quarantined; the cost of failing here would be an app that will not
    /// start.
    pub fn load_or_create(app_data: &Path) -> Result<Self, String> {
        let path = Self::path(app_data);
        if let Some(existing) = Self::read(&path) {
            return Ok(existing);
        }
        let secret = Self::random_secret();
        // A partially written key is worse than a missing one: it would verify
        // nothing and never be replaced. Write to a temporary file and rename.
        let temporary = path.with_extension("key.tmp");
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("无法创建工具回执密钥目录: {error}"))?;
        }
        fs::write(&temporary, secret).map_err(|error| format!("无法写入工具回执密钥: {error}"))?;
        fs::rename(&temporary, &path).map_err(|error| format!("无法提交工具回执密钥: {error}"))?;
        Ok(Self { secret })
    }

    /// An ephemeral key, for tests and for any path with no app-data directory.
    /// Cards attested under it are valid for this process only.
    pub fn ephemeral() -> Self {
        Self {
            secret: Self::random_secret(),
        }
    }

    fn path(app_data: &Path) -> PathBuf {
        app_data.join(KEY_FILE_NAME)
    }

    fn read(path: &Path) -> Option<Self> {
        let bytes = fs::read(path).ok()?;
        let secret = <[u8; 32]>::try_from(bytes.as_slice()).ok()?;
        Some(Self { secret })
    }

    /// 256 random bits. `Uuid::new_v4` is the crate already used for every
    /// other unguessable value in this codebase and draws from the OS CSPRNG;
    /// two of them supply the full width.
    fn random_secret() -> [u8; 32] {
        let mut secret = [0u8; 32];
        secret[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        secret[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        secret
    }

    /// The token for one card: lowercase hex, stable for identical subjects.
    pub fn attest(&self, subject: &AttestationSubject<'_>) -> String {
        let mut mac =
            HmacSha256::new_from_slice(&self.secret).expect("HMAC accepts a key of any length");
        // Length-prefixed so no two different subjects can produce the same
        // byte stream — without this, moving text from one field to its
        // neighbour would go unnoticed.
        for part in [
            ATTESTATION_VERSION.as_bytes(),
            subject.conversation_id.as_bytes(),
            subject.context_id.as_bytes(),
            subject.tool_name.as_bytes(),
            canonical_object(subject.input).as_bytes(),
            subject
                .requested_input
                .map(canonical_object)
                .unwrap_or_default()
                .as_bytes(),
            canonical_result(subject.result).as_bytes(),
            subject
                .subagent
                .map(canonical_subagent)
                .unwrap_or_default()
                .as_bytes(),
        ] {
            mac.update(&(part.len() as u64).to_le_bytes());
            mac.update(part);
        }
        let digest = mac.finalize().into_bytes();
        digest.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    /// Whether `token` is this key's token for `subject`.
    ///
    /// The comparison is constant-time in the token, so a caller cannot learn a
    /// valid token by measuring how long a wrong one takes to reject.
    pub fn verify(&self, subject: &AttestationSubject<'_>, token: &str) -> bool {
        let expected = self.attest(subject);
        if expected.len() != token.len() {
            return false;
        }
        expected
            .bytes()
            .zip(token.bytes())
            .fold(0u8, |difference, (left, right)| difference | (left ^ right))
            == 0
    }
}

/// Bumped when the committed field set changes, so tokens from an older shape
/// stop verifying instead of silently covering less than they appear to.
const ATTESTATION_VERSION: &str = "tool-attestation-v3";

fn canonical_object(object: &JsonObject) -> String {
    // `serde_json::Map` is a BTreeMap in this build, so its key order is
    // already canonical.
    serde_json::to_string(object).unwrap_or_default()
}

fn canonical_result(result: &ToolResult) -> String {
    serde_json::to_string(result).unwrap_or_default()
}

fn canonical_subagent(subagent: &SubagentRunRecord) -> String {
    serde_json::to_string(subagent).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ContextItem, SubagentRunKind, SubagentRunStatus};

    fn result() -> ToolResult {
        ToolResult {
            success: true,
            output: "ok".into(),
            images: Vec::new(),
            diff: None,
            executed_at: "2026-08-09T00:00:00Z".into(),
            duration_ms: 1,
        }
    }

    fn tool_input(value: &str) -> JsonObject {
        serde_json::from_value(serde_json::json!({ "path": value })).unwrap()
    }

    fn subject<'a>(
        input: &'a JsonObject,
        result: &'a ToolResult,
        subagent: Option<&'a SubagentRunRecord>,
    ) -> AttestationSubject<'a> {
        AttestationSubject {
            conversation_id: "conv_1",
            context_id: "ctx_tool_1",
            tool_name: "read",
            input,
            requested_input: None,
            result,
            subagent,
        }
    }

    #[test]
    fn a_token_verifies_only_against_the_card_it_was_issued_for() {
        let key = AttestationKey::ephemeral();
        let input = tool_input("README.md");
        let result = result();
        let token = key.attest(&subject(&input, &result, None));

        assert!(key.verify(&subject(&input, &result, None), &token));
        assert!(!key.verify(&subject(&input, &result, None), "not a token"));
        assert!(!key.verify(&subject(&input, &result, None), ""));
    }

    /// Every field the token exists to pin. If any of these stopped mattering,
    /// the renderer could change it after execution and still save.
    #[test]
    fn changing_any_committed_field_invalidates_the_token() {
        let key = AttestationKey::ephemeral();
        let input = tool_input("README.md");
        let result = result();
        let base = subject(&input, &result, None);
        let token = key.attest(&base);

        let other_input = tool_input("SECRET.md");
        let mut other_result = result.clone();
        other_result.output = "different".into();
        let requested = tool_input("requested.md");

        let variants = [
            AttestationSubject {
                conversation_id: "conv_2",
                ..subject(&input, &result, None)
            },
            AttestationSubject {
                context_id: "ctx_tool_2",
                ..subject(&input, &result, None)
            },
            AttestationSubject {
                tool_name: "write",
                ..subject(&input, &result, None)
            },
            subject(&other_input, &result, None),
            subject(&input, &other_result, None),
            AttestationSubject {
                requested_input: Some(&requested),
                ..subject(&input, &result, None)
            },
        ];
        for variant in variants {
            assert!(
                !key.verify(&variant, &token),
                "a changed field must invalidate the token"
            );
        }
    }

    /// The child record is renderer-writable and is what a subagent card's
    /// audit trail lives in, so it has to be inside the MAC.
    #[test]
    fn the_child_record_is_committed_including_its_nested_cards() {
        let key = AttestationKey::ephemeral();
        let input = tool_input("README.md");
        let result = result();
        let record = SubagentRunRecord {
            kind: SubagentRunKind::General,
            name: None,
            label: None,
            inherits_model_memory: false,
            fork_model_binding: None,
            agent_definition: None,
            execution_mode_receipt: String::new(),
            task: "child task".into(),
            status: SubagentRunStatus::Completed,
            contexts: vec![ContextItem::User {
                id: "ctx_child_user".into(),
                content: "hello".into(),
                images: Vec::new(),
                created_at: "2026-08-09T00:00:00Z".into(),
            }],
            updates: Vec::new(),
            queued_messages: Vec::new(),
            structured_output: None,
            output_schema: None,
            usage: crate::model::ModelUsage::default(),
        };
        let token = key.attest(&subject(&input, &result, Some(&record)));
        assert!(key.verify(&subject(&input, &result, Some(&record)), &token));

        let mut retasked = record.clone();
        retasked.task = "a task the child never ran".into();
        assert!(!key.verify(&subject(&input, &result, Some(&retasked)), &token));

        let mut nested_edited = record.clone();
        nested_edited.contexts = vec![ContextItem::User {
            id: "ctx_child_user".into(),
            content: "something else entirely".into(),
            images: Vec::new(),
            created_at: "2026-08-09T00:00:00Z".into(),
        }];
        assert!(!key.verify(&subject(&input, &result, Some(&nested_edited)), &token));

        // Dropping the record is also a change, not a no-op.
        assert!(!key.verify(&subject(&input, &result, None), &token));
    }

    /// Field boundaries must be unambiguous: text moved from one field into the
    /// next has to change the token even though the concatenation is identical.
    #[test]
    fn field_boundaries_cannot_be_shifted_without_changing_the_token() {
        let key = AttestationKey::ephemeral();
        let result = result();
        let input = JsonObject::new();
        let left = AttestationSubject {
            conversation_id: "conv",
            context_id: "1ctx",
            ..subject(&input, &result, None)
        };
        let right = AttestationSubject {
            conversation_id: "conv1",
            context_id: "ctx",
            ..subject(&input, &result, None)
        };
        assert_ne!(key.attest(&left), key.attest(&right));
    }

    #[test]
    fn a_token_from_another_key_never_verifies() {
        let input = tool_input("README.md");
        let result = result();
        let issued = AttestationKey::ephemeral().attest(&subject(&input, &result, None));
        assert!(!AttestationKey::ephemeral().verify(&subject(&input, &result, None), &issued));
    }

    /// The key has to outlive the process, or every restart would strand every
    /// executed-but-unsaved card — the exact failure this replaces.
    #[test]
    fn the_key_survives_a_restart_of_the_process() {
        let directory = tempfile::tempdir().unwrap();
        let input = tool_input("README.md");
        let result = result();

        let first = AttestationKey::load_or_create(directory.path()).unwrap();
        let token = first.attest(&subject(&input, &result, None));

        let reloaded = AttestationKey::load_or_create(directory.path()).unwrap();
        assert!(
            reloaded.verify(&subject(&input, &result, None), &token),
            "a card attested before a restart must still verify after one"
        );
    }

    /// A key file that is missing or damaged must not stop the app from
    /// starting. The cost is quarantined cards, which is recoverable; refusing
    /// to launch is not.
    #[test]
    fn an_unreadable_key_is_replaced_rather_than_fatal() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join(KEY_FILE_NAME), b"too short").unwrap();

        let key = AttestationKey::load_or_create(directory.path())
            .expect("a damaged key must not be fatal");
        let input = tool_input("README.md");
        let result = result();
        let token = key.attest(&subject(&input, &result, None));
        assert!(key.verify(&subject(&input, &result, None), &token));

        // And the replacement is now the durable one.
        let reloaded = AttestationKey::load_or_create(directory.path()).unwrap();
        assert!(reloaded.verify(&subject(&input, &result, None), &token));
    }
}
