use std::{
    collections::{HashMap, HashSet},
    fs,
    path::Path,
    sync::Mutex,
    time::{Duration, Instant},
};

use serde::Serialize;
use uuid::Uuid;

use crate::model::ToolExecutionRequest;

const TOOL_APPROVAL_TTL: Duration = Duration::from_secs(90);
const MAX_TOOL_APPROVALS: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq)]
struct ToolApprovalBinding {
    conversation_id: String,
    workspace: String,
    tool_name: String,
    input: String,
    /// Trusted run-environment fingerprint (`ShellRunner::fingerprint`). Both
    /// issuance and consumption resolve it from the current persisted
    /// conversation state, so a changed location or variable invalidates the
    /// nonce rather than running a locally approved command remotely.
    run_environment: String,
}

#[derive(Clone, Debug)]
struct ToolApproval {
    binding: ToolApprovalBinding,
    expires_at: Instant,
}

#[derive(Default)]
pub struct ApprovalRegistry {
    tool_approvals: Mutex<HashMap<String, ToolApproval>>,
    workspace_paths: Mutex<HashSet<String>>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolApprovalGrant {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nonce: Option<String>,
    pub expires_in_ms: u64,
    /// Set when the call still needs the user's answer. The renderer draws this
    /// card above the composer and calls `resolve_tool_prompt`, which is what
    /// mints the nonce. Absent nonce plus absent prompt means "no approval was
    /// needed at all".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<crate::tool_prompt::PendingToolPrompt>,
}

impl ToolApprovalGrant {
    pub fn not_required() -> Self {
        Self {
            nonce: None,
            expires_in_ms: 0,
            prompt: None,
        }
    }

    pub fn pending(prompt: crate::tool_prompt::PendingToolPrompt) -> Self {
        Self {
            nonce: None,
            expires_in_ms: 0,
            prompt: Some(prompt),
        }
    }
}

impl ApprovalRegistry {
    pub fn issue_tool_approval(
        &self,
        request: &ToolExecutionRequest,
        run_environment: &str,
    ) -> Result<ToolApprovalGrant, String> {
        self.issue_tool_approval_with_ttl(request, run_environment, TOOL_APPROVAL_TTL)
    }

    fn issue_tool_approval_with_ttl(
        &self,
        request: &ToolExecutionRequest,
        run_environment: &str,
        ttl: Duration,
    ) -> Result<ToolApprovalGrant, String> {
        let binding = tool_binding(request, run_environment)?;
        let now = Instant::now();
        let mut approvals = self
            .tool_approvals
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        approvals.retain(|_, approval| approval.expires_at > now);
        if approvals.len() >= MAX_TOOL_APPROVALS {
            // At capacity, evict the oldest nonce from the same conversation
            // first. A burst in one conversation must not invalidate pending
            // approval in another; use the global oldest only when no local
            // approval exists.
            let evict = approvals
                .iter()
                .filter(|(_, approval)| {
                    approval.binding.conversation_id == binding.conversation_id
                })
                .min_by_key(|(_, approval)| approval.expires_at)
                .map(|(nonce, _)| nonce.clone())
                .or_else(|| {
                    approvals
                        .iter()
                        .min_by_key(|(_, approval)| approval.expires_at)
                        .map(|(nonce, _)| nonce.clone())
                });
            if let Some(oldest) = evict {
                approvals.remove(&oldest);
            }
        }

        let nonce = loop {
            let candidate = Uuid::new_v4().to_string();
            if !approvals.contains_key(&candidate) {
                break candidate;
            }
        };
        approvals.insert(
            nonce.clone(),
            ToolApproval {
                binding,
                expires_at: now + ttl,
            },
        );
        Ok(ToolApprovalGrant {
            nonce: Some(nonce),
            expires_in_ms: ttl.as_millis().min(u64::MAX as u128) as u64,
            prompt: None,
        })
    }

    pub fn consume_tool_approval(
        &self,
        nonce: &str,
        request: &ToolExecutionRequest,
        run_environment: &str,
    ) -> Result<(), String> {
        if nonce.trim().is_empty() {
            return Err("High-risk tool call is missing a valid one-time approval".into());
        }
        let expected = tool_binding(request, run_environment)?;
        let approval = self
            .tool_approvals
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(nonce)
            .ok_or_else(|| "High-risk tool approval is invalid, already used, or expired".to_owned())?;
        if approval.expires_at <= Instant::now() {
            return Err("High-risk tool approval expired; request approval again".into());
        }
        if approval.binding != expected {
            return Err("High-risk tool approval does not match the workspace, execution environment, tool, or arguments".into());
        }
        Ok(())
    }

    pub fn authorize_workspace(&self, path: &Path) -> Result<String, String> {
        let (key, display) = canonical_workspace(path)?;
        self.workspace_paths
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(key);
        Ok(display)
    }

    pub fn require_workspace_authorization(&self, path: &Path) -> Result<(), String> {
        let (key, _) = canonical_workspace(path)?;
        if self
            .workspace_paths
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .contains(&key)
        {
            Ok(())
        } else {
            Err("新增或更改工作区路径必须先通过系统目录选择器授权".into())
        }
    }

    pub fn workspace_key(path: &Path) -> Option<String> {
        canonical_workspace(path).ok().map(|(key, _)| key)
    }

    pub fn clear(&self) {
        self.tool_approvals
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
        self.workspace_paths
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
    }
}

fn tool_binding(
    request: &ToolExecutionRequest,
    run_environment: &str,
) -> Result<ToolApprovalBinding, String> {
    let workspace = canonical_workspace(Path::new(&request.workspace_path))?.0;
    if request.conversation_id.trim().is_empty() {
        return Err("Conversation ID must not be empty".into());
    }
    if request.tool_name.trim().is_empty() {
        return Err("Tool name must not be empty".into());
    }
    let input = serde_json::to_string(&request.input)
        .map_err(|error| format!("Could not canonicalize tool arguments: {error}"))?;
    Ok(ToolApprovalBinding {
        conversation_id: request.conversation_id.clone(),
        workspace,
        tool_name: request.tool_name.clone(),
        input,
        run_environment: run_environment.to_owned(),
    })
}

fn canonical_workspace(path: &Path) -> Result<(String, String), String> {
    if !path.is_absolute() {
        return Err("Workspace path must be absolute".into());
    }
    let canonical =
        fs::canonicalize(path).map_err(|error| format!("Workspace path does not exist or cannot be accessed: {error}"))?;
    if !canonical.is_dir() {
        return Err("Workspace path must point to a directory".into());
    }
    let display = path.to_string_lossy().into_owned();
    let key = canonical.to_string_lossy().into_owned();
    #[cfg(windows)]
    let key = key.to_lowercase();
    Ok((key, display))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn request(workspace: &Path) -> ToolExecutionRequest {
        ToolExecutionRequest {
            conversation_id: "conversation-test".into(),
            workspace_path: workspace.to_string_lossy().into_owned(),
            tool_name: "write".into(),
            input: serde_json::from_value(json!({"path":"note.txt","content":"hello"})).unwrap(),
        }
    }

    #[test]
    fn tool_approval_is_bound_and_single_use() {
        let directory = tempfile::tempdir().unwrap();
        let registry = ApprovalRegistry::default();
        let approved = request(directory.path());
        let grant = registry.issue_tool_approval(&approved, "env-local").unwrap();

        let mut mismatched = approved.clone();
        mismatched.input.insert("content".into(), json!("changed"));
        assert!(registry
            .consume_tool_approval(grant.nonce.as_deref().unwrap(), &mismatched, "env-local")
            .is_err());
        assert!(registry
            .consume_tool_approval(grant.nonce.as_deref().unwrap(), &approved, "env-local")
            .is_err());

        // A changed run environment must invalidate the nonce.
        let grant = registry.issue_tool_approval(&approved, "env-local").unwrap();
        assert!(registry
            .consume_tool_approval(grant.nonce.as_deref().unwrap(), &approved, "env-ssh")
            .is_err());

        let grant = registry.issue_tool_approval(&approved, "env-local").unwrap();
        assert!(registry
            .consume_tool_approval(grant.nonce.as_deref().unwrap(), &approved, "env-local")
            .is_ok());
        assert!(registry
            .consume_tool_approval(grant.nonce.as_deref().unwrap(), &approved, "env-local")
            .is_err());

        let grant = registry.issue_tool_approval(&approved, "env-local").unwrap();
        let mut other_conversation = approved.clone();
        other_conversation.conversation_id = "conversation-other".into();
        assert!(registry
            .consume_tool_approval(grant.nonce.as_deref().unwrap(), &other_conversation, "env-local")
            .is_err());
    }

    #[test]
    fn expired_tool_approval_is_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let registry = ApprovalRegistry::default();
        let request = request(directory.path());
        let grant = registry
            .issue_tool_approval_with_ttl(&request, "env-local", Duration::ZERO)
            .unwrap();
        assert!(registry
            .consume_tool_approval(grant.nonce.as_deref().unwrap(), &request, "env-local")
            .is_err());
    }

    #[test]
    fn directory_workspace_authorization_is_required() {
        let directory = tempfile::tempdir().unwrap();
        let registry = ApprovalRegistry::default();
        assert!(registry
            .require_workspace_authorization(directory.path())
            .is_err());

        registry.authorize_workspace(directory.path()).unwrap();
        assert!(registry
            .require_workspace_authorization(directory.path())
            .is_ok());
    }

    /// Capacity eviction is scoped to the requesting conversation before it can
    /// affect pending approvals in another conversation.
    #[test]
    fn an_approval_flood_evicts_its_own_conversation_before_touching_others() {
        let directory = tempfile::tempdir().unwrap();
        let registry = ApprovalRegistry::default();
        let quiet = request(directory.path());
        let quiet_grant = registry.issue_tool_approval(&quiet, "env-local").unwrap();

        let mut noisy = request(directory.path());
        noisy.conversation_id = "conversation-noisy".into();
        for _ in 0..(MAX_TOOL_APPROVALS + 16) {
            registry.issue_tool_approval(&noisy, "env-local").unwrap();
        }

        assert!(
            registry
                .consume_tool_approval(quiet_grant.nonce.as_deref().unwrap(), &quiet, "env-local")
                .is_ok(),
            "洪峰会话必须回收自己的 nonce，安静会话的批准不受牵连"
        );
    }
}
