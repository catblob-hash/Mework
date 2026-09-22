//! Resolving a renderer-supplied conversation id against the persisted document.
//!
//! This module used to back the sidebar's file browser as well — a directory lister and a text
//! reader, both hardened against traversal, symlinks and oversized files. That page is gone, and
//! with it the only caller either function ever had. What remains is the lookup every *trusted*
//! workspace operation still starts from: the renderer names a conversation, and the host decides
//! which workspace that is by reading its own saved document rather than by believing a path the
//! renderer handed over.

use serde::Deserialize;

use crate::model::{AppDocument, AttachedWorkspace, Conversation, Workspace, WorkspaceKind};

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
    /// A project (the `Workspace` row) addressed without a conversation.
    #[serde(rename_all = "camelCase")]
    Workspace {
        workspace_id: String,
        /// Which of the project's workspaces, 1-based: absent or `1` is
        /// workspace 1 (the project root), `k >= 2` is its `k - 2`th further
        /// workspace. A position, never a path, for the same reason the target
        /// carries only IDs.
        #[serde(default)]
        member: Option<u32>,
    },
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
        /// `None` is workspace 1. `Some` is the project's further workspace the
        /// target's `member` named, whose directory — not `workspace.path` — is
        /// the one the request acts on.
        member: Option<&'a AttachedWorkspace>,
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
        GitTarget::Workspace {
            workspace_id,
            member,
        } => {
            let workspace = find_workspace_by_id(document, workspace_id, request_label)?;
            // Temporary workspaces are created per conversation and have no shared
            // root. Only directory workspaces may be addressed directly.
            if workspace.kind != WorkspaceKind::Directory {
                return Err(format!("{request_label}只能按目录工作区寻址"));
            }
            if workspace.path.trim().is_empty() {
                return Err(format!("工作区 {} 的路径为空", workspace.id));
            }
            let member = resolve_project_member(workspace, *member, request_label)?;
            Ok(ResolvedGitTarget::Workspace { workspace, member })
        }
    }
}

/// Looks a project workspace up by its 1-based position.
///
/// Out of range is an error rather than a fall back to workspace 1: a Git write
/// that lands in a different checkout than the one the user selected is the one
/// outcome no caller can undo.
fn resolve_project_member<'a>(
    workspace: &'a Workspace,
    position: Option<u32>,
    request_label: &str,
) -> Result<Option<&'a AttachedWorkspace>, String> {
    let position = match position {
        None | Some(1) => return Ok(None),
        Some(position) => position,
    };
    let count = workspace.member_workspaces().len() + 1;
    let entry = usize::try_from(position)
        .ok()
        .and_then(|position| position.checked_sub(2))
        .and_then(|offset| workspace.member_workspaces().get(offset))
        .ok_or_else(|| {
            format!(
                "{request_label}的项目 {} 没有工作区 {position}（共 {count} 个）",
                workspace.id
            )
        })?;
    if entry.path.trim().is_empty() {
        return Err(format!(
            "项目 {} 的工作区 {position} 路径为空",
            workspace.id
        ));
    }
    Ok(Some(entry))
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

    fn document_with_conversation(workspace_id: &str, conversation_id: &str) -> AppDocument {
        let mut document = default_document();
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
        for candidate in [
            "",
            "   ",
            " conversation-a",
            "conversation-\u{7}a",
            "missing",
        ] {
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
            member: None,
        };
        match resolve_git_target(&document, &target, "测试请求").expect("目录工作区可以解析")
        {
            ResolvedGitTarget::Workspace { workspace, member } => {
                assert_eq!(workspace.id, "workspace-a");
                assert!(member.is_none(), "未指定成员时指向工作区 1");
            }
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
            member: None,
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
            member: None,
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
            GitTarget::Workspace { workspace_id, member: None } if workspace_id == "ws_1"
        ));
        let member: GitTarget =
            serde_json::from_str(r#"{"kind":"workspace","workspaceId":"x","member":2}"#)
                .expect("项目成员寻址可以反序列化");
        assert!(matches!(
            member,
            GitTarget::Workspace { ref workspace_id, member: Some(2) } if workspace_id == "x"
        ));
        let first: GitTarget =
            serde_json::from_str(r#"{"kind":"workspace","workspaceId":"x","member":1}"#)
                .expect("显式的工作区 1 可以反序列化");
        assert!(matches!(
            first,
            GitTarget::Workspace {
                member: Some(1),
                ..
            }
        ));
        assert!(serde_json::from_str::<GitTarget>(r#"{"conversationId":"conv_1"}"#).is_err());
        assert!(serde_json::from_str::<GitTarget>(
            r#"{"kind":"workspace","workspaceId":"x","member":-1}"#
        )
        .is_err());
    }

    fn document_with_members() -> AppDocument {
        let mut document = document_with_conversation("workspace-a", "conversation-a");
        let workspace = document
            .workspaces
            .first_mut()
            .expect("默认文档至少有一个工作区");
        workspace.path = "C:/repo".to_owned();
        workspace.additional_workspaces = vec![
            AttachedWorkspace {
                machine: None,
                path: "C:/shared".to_owned(),
            },
            AttachedWorkspace {
                machine: Some(crate::model::RunTarget::Ssh {
                    machine_id: "m1".to_owned(),
                }),
                path: "~/services".to_owned(),
            },
        ];
        document
    }

    fn member_target(member: Option<u32>) -> GitTarget {
        GitTarget::Workspace {
            workspace_id: "workspace-a".to_owned(),
            member,
        }
    }

    /// Position 1 (explicit or absent) is the project root; 2..k are the
    /// project's further workspaces in their recorded order.
    #[test]
    fn a_member_position_resolves_to_that_project_workspace() {
        let document = document_with_members();
        for primary in [None, Some(1)] {
            match resolve_git_target(&document, &member_target(primary), "测试请求").unwrap() {
                ResolvedGitTarget::Workspace { member: None, .. } => {}
                _ => panic!("{primary:?} 应解析为工作区 1"),
            }
        }
        match resolve_git_target(&document, &member_target(Some(2)), "测试请求").unwrap() {
            ResolvedGitTarget::Workspace {
                workspace,
                member: Some(member),
            } => {
                assert_eq!(workspace.id, "workspace-a");
                assert_eq!(member.path, "C:/shared");
                assert!(member.machine.is_none());
            }
            _ => panic!("成员 2 应解析为第一个额外工作区"),
        }
        match resolve_git_target(&document, &member_target(Some(3)), "测试请求").unwrap() {
            ResolvedGitTarget::Workspace {
                member: Some(member),
                ..
            } => {
                assert_eq!(member.path, "~/services");
                assert!(member.machine.is_some());
            }
            _ => panic!("成员 3 应解析为第二个额外工作区"),
        }
    }

    /// An out-of-range position must fail rather than fall back to the root: a
    /// Git write in a checkout the user did not select cannot be undone.
    #[test]
    fn an_out_of_range_member_is_refused() {
        let document = document_with_members();
        for position in [0, 4, u32::MAX] {
            let error =
                match resolve_git_target(&document, &member_target(Some(position)), "测试请求")
                {
                    Ok(_) => panic!("成员 {position} 不应解析成功"),
                    Err(error) => error,
                };
            assert!(error.contains(&format!("没有工作区 {position}")), "{error}");
        }
        // A project with no further workspaces has only position 1.
        let plain = document_with_conversation("workspace-a", "conversation-a");
        assert!(resolve_git_target(&plain, &member_target(Some(2)), "测试请求").is_err());
    }
}
