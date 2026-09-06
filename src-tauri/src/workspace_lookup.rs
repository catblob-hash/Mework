//! Resolving a renderer-supplied conversation id against the persisted document.
//!
//! This module used to back the sidebar's file browser as well — a directory lister and a text
//! reader, both hardened against traversal, symlinks and oversized files. That page is gone, and
//! with it the only caller either function ever had. What remains is the lookup every *trusted*
//! workspace operation still starts from: the renderer names a conversation, and the host decides
//! which workspace that is by reading its own saved document rather than by believing a path the
//! renderer handed over.

use serde::Deserialize;

use crate::model::{AppDocument, Conversation, Workspace, WorkspaceKind};

/// Identifies the persisted checkout for a Git request.
///
/// Both variants carry only IDs; the host derives paths from its persisted
/// document, so the renderer never supplies a repository path or Git directory.
/// Each variant needs `rename_all`: enum-level renaming affects variant names,
/// while fields require their own declaration.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum GitTarget {
    #[serde(rename_all = "camelCase")]
    Conversation { conversation_id: String },
    #[serde(rename_all = "camelCase")]
    Workspace { workspace_id: String },
}

/// Resolved coordinates. Workspace targets have no conversation, so callers must
/// handle operations that belong to no conversation rather than inventing one.
pub enum ResolvedGitTarget<'a> {
    Conversation {
        workspace: &'a Workspace,
        conversation: &'a Conversation,
    },
    Workspace {
        workspace: &'a Workspace,
    },
}

pub fn resolve_git_target<'a>(
    document: &'a AppDocument,
    target: &GitTarget,
    request_label: &str,
) -> Result<ResolvedGitTarget<'a>, String> {
    match target {
        GitTarget::Conversation { conversation_id } => {
            let (workspace, conversation) =
                find_workspace_for_conversation(document, conversation_id, request_label)?;
            Ok(ResolvedGitTarget::Conversation {
                workspace,
                conversation,
            })
        }
        GitTarget::Workspace { workspace_id } => {
            let workspace = find_workspace_by_id(document, workspace_id, request_label)?;
            // Temporary workspaces are created per conversation and have no shared
            // root. Only directory workspaces may be addressed directly.
            if workspace.kind != WorkspaceKind::Directory {
                return Err(format!("{request_label}只能按目录工作区寻址"));
            }
            if workspace.path.trim().is_empty() {
                return Err(format!("工作区 {} 的路径为空", workspace.id));
            }
            Ok(ResolvedGitTarget::Workspace { workspace })
        }
    }
}

/// Resolves a renderer-supplied conversation ID against the persisted document. A caller must
/// never accept a workspace path from the renderer for trusted workspace access.
pub fn find_workspace_for_conversation<'a>(
    document: &'a AppDocument,
    conversation_id: &str,
    request_label: &str,
) -> Result<(&'a Workspace, &'a Conversation), String> {
    validate_requested_id(conversation_id, request_label, "对话")?;
    let mut found = None;
    for workspace in &document.workspaces {
        for conversation in &workspace.conversations {
            if conversation.id != conversation_id {
                continue;
            }
            if found.is_some() {
                return Err(format!("{request_label}的对话 ID 在多个工作区中重复"));
            }
            found = Some((workspace, conversation));
        }
    }
    found.ok_or_else(|| format!("{request_label}的对话不在后端已保存文档中"))
}

/// Resolves a renderer-supplied workspace ID against the persisted document.
///
/// Apply the same fail-closed validation as conversation IDs: renderer input
/// cannot select a default workspace when malformed or unknown.
pub fn find_workspace_by_id<'a>(
    document: &'a AppDocument,
    workspace_id: &str,
    request_label: &str,
) -> Result<&'a Workspace, String> {
    validate_requested_id(workspace_id, request_label, "工作区")?;
    let mut found = None;
    for workspace in &document.workspaces {
        if workspace.id != workspace_id {
            continue;
        }
        if found.is_some() {
            return Err(format!("{request_label}的工作区 ID 重复"));
        }
        found = Some(workspace);
    }
    found.ok_or_else(|| format!("{request_label}的工作区不在后端已保存文档中"))
}

fn validate_requested_id(value: &str, request_label: &str, noun: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err(format!("{request_label}的{noun} ID 不能为空"));
    }
    if value.trim() != value || value.len() > 256 || value.chars().any(char::is_control) {
        return Err(format!("{request_label}的{noun} ID 无效"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::default_document;
    use std::path::Path;

    fn document_with_conversation(workspace_id: &str, conversation_id: &str) -> AppDocument {
        let mut document = default_document(Path::new("."));
        let workspace = document
            .workspaces
            .first_mut()
            .expect("默认文档至少有一个工作区");
        workspace.id = workspace_id.to_owned();
        let conversation = workspace
            .conversations
            .first_mut()
            .expect("默认工作区至少有一个对话");
        conversation.id = conversation_id.to_owned();
        document
    }

    #[test]
    fn resolves_a_conversation_to_its_own_workspace() {
        let document = document_with_conversation("workspace-a", "conversation-a");
        let (workspace, conversation) =
            find_workspace_for_conversation(&document, "conversation-a", "测试请求")
                .expect("已保存的对话可以解析");
        assert_eq!(workspace.id, "workspace-a");
        assert_eq!(conversation.id, "conversation-a");
    }

    /// The renderer is not trusted to name a real conversation, so every malformed or unknown id
    /// has to fail closed rather than fall through to some default workspace.
    #[test]
    fn rejects_blank_padded_control_and_unknown_ids() {
        let document = document_with_conversation("workspace-a", "conversation-a");
        for candidate in ["", "   ", " conversation-a", "conversation-\u{7}a", "missing"] {
            assert!(
                find_workspace_for_conversation(&document, candidate, "测试请求").is_err(),
                "{candidate:?} 不应解析成功"
            );
        }
    }

    /// Workspace ID validation is fail-closed just like conversation ID
    /// validation; drafts use a different persisted coordinate, not weaker checks.
    #[test]
    fn rejects_blank_padded_control_and_unknown_workspace_ids() {
        let document = document_with_conversation("workspace-a", "conversation-a");
        for candidate in ["", "   ", " workspace-a", "workspace-\u{7}a", "missing"] {
            assert!(
                find_workspace_by_id(&document, candidate, "测试请求").is_err(),
                "{candidate:?} 不应解析成功"
            );
        }
    }

    #[test]
    fn resolves_a_workspace_target_to_the_workspace_row() {
        let mut document = document_with_conversation("workspace-a", "conversation-a");
        document
            .workspaces
            .first_mut()
            .expect("默认文档至少有一个工作区")
            .path = "C:/repo".to_owned();
        let target = GitTarget::Workspace {
            workspace_id: "workspace-a".to_owned(),
        };
        match resolve_git_target(&document, &target, "测试请求").expect("目录工作区可以解析") {
            ResolvedGitTarget::Workspace { workspace } => assert_eq!(workspace.id, "workspace-a"),
            ResolvedGitTarget::Conversation { .. } => panic!("工作区寻址不该解析出对话"),
        }
    }

    /// Temporary workspaces have no shared root and cannot be addressed by workspace ID.
    #[test]
    fn rejects_a_workspace_target_that_is_not_a_directory_workspace() {
        let mut document = document_with_conversation("workspace-a", "conversation-a");
        let workspace = document
            .workspaces
            .first_mut()
            .expect("默认文档至少有一个工作区");
        workspace.kind = WorkspaceKind::Temporary;
        workspace.path = String::new();
        let target = GitTarget::Workspace {
            workspace_id: "workspace-a".to_owned(),
        };
        assert!(resolve_git_target(&document, &target, "测试请求").is_err());
    }

    /// Reject directory workspaces with empty paths so a request cannot target the process cwd.
    #[test]
    fn rejects_a_workspace_target_with_an_empty_path() {
        let mut document = document_with_conversation("workspace-a", "conversation-a");
        document
            .workspaces
            .first_mut()
            .expect("默认文档至少有一个工作区")
            .path = "   ".to_owned();
        let target = GitTarget::Workspace {
            workspace_id: "workspace-a".to_owned(),
        };
        assert!(resolve_git_target(&document, &target, "测试请求").is_err());
    }

    /// Renderer input is JSON, so the tagged wire shape is part of the contract.
    #[test]
    fn deserializes_both_target_shapes_from_the_renderer_wire() {
        let conversation: GitTarget =
            serde_json::from_str(r#"{"kind":"conversation","conversationId":"conv_1"}"#)
                .expect("对话寻址可以反序列化");
        assert!(matches!(
            conversation,
            GitTarget::Conversation { conversation_id } if conversation_id == "conv_1"
        ));
        let workspace: GitTarget =
            serde_json::from_str(r#"{"kind":"workspace","workspaceId":"ws_1"}"#)
                .expect("工作区寻址可以反序列化");
        assert!(matches!(
            workspace,
            GitTarget::Workspace { workspace_id } if workspace_id == "ws_1"
        ));
        assert!(serde_json::from_str::<GitTarget>(r#"{"conversationId":"conv_1"}"#).is_err());
    }
}
