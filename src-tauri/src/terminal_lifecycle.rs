use std::collections::{HashMap, HashSet};

use crate::model::{AppDocument, WorkspaceKind};

#[derive(Clone, PartialEq)]
struct TerminalWorkspaceBinding {
    workspace_id: String,
    workspace_kind: WorkspaceKind,
    workspace_path: String,
}

#[derive(Clone, PartialEq)]
struct WorkspaceLifecycleBinding {
    workspace_kind: WorkspaceKind,
    workspace_path: String,
}

fn conversation_bindings(document: &AppDocument) -> HashMap<String, TerminalWorkspaceBinding> {
    document
        .workspaces
        .iter()
        .flat_map(|workspace| {
            workspace.conversations.iter().map(move |conversation| {
                (
                    conversation.id.clone(),
                    TerminalWorkspaceBinding {
                        workspace_id: workspace.id.clone(),
                        workspace_kind: workspace.kind,
                        workspace_path: workspace.path.clone(),
                    },
                )
            })
        })
        .collect()
}

fn workspace_lifecycle_bindings(
    document: &AppDocument,
) -> HashMap<String, WorkspaceLifecycleBinding> {
    document
        .workspaces
        .iter()
        .map(|workspace| {
            (
                workspace.id.clone(),
                WorkspaceLifecycleBinding {
                    workspace_kind: workspace.kind,
                    workspace_path: workspace.path.clone(),
                },
            )
        })
        .collect()
}

/// Returns owners of process-local terminal sessions whose persisted workspace
/// binding no longer matches the one used when their shell was launched.
///
/// Only conversations present in `previous` can own an existing session. A
/// newly-added conversation therefore needs no invalidation, while removal,
/// moving between workspaces, and changing the owning workspace's id, kind, or
/// path all invalidate the old shell.
///
/// Both inputs must authoritatively identify conversation-to-workspace ownership.
/// Only the conversation data-plane commands satisfy that requirement. Full-document
/// saves omit conversations from the renderer payload, so they must use
/// [`workspace_rebound_conversations`] instead.
pub fn invalidated_conversations(previous: &AppDocument, next: &AppDocument) -> HashSet<String> {
    let previous = conversation_bindings(previous);
    let next = conversation_bindings(next);
    previous
        .into_iter()
        .filter_map(|(conversation_id, binding)| {
            (next.get(&conversation_id) != Some(&binding)).then_some(conversation_id)
        })
        .collect()
}

/// Uses conversations only from `previous`; `next` supplies workspace shape only.
///
/// Renderer save payloads omit conversations, and authoritative adoption prevents
/// that boundary from moving them. A binding can change here only when its owning
/// workspace changes kind or path, or is removed.
pub fn workspace_rebound_conversations(
    previous: &AppDocument,
    next: &AppDocument,
) -> HashSet<String> {
    let next = workspace_lifecycle_bindings(next);
    previous
        .workspaces
        .iter()
        .filter(|workspace| {
            next.get(&workspace.id)
                != Some(&WorkspaceLifecycleBinding {
                    workspace_kind: workspace.kind,
                    workspace_path: workspace.path.clone(),
                })
        })
        .flat_map(|workspace| {
            workspace
                .conversations
                .iter()
                .map(|conversation| conversation.id.clone())
        })
        .collect()
}

/// Returns whether publishing `next` must exclude every workspace-bound operation.
///
/// Existing work can only own identities already present in `previous`. Adding a
/// conversation to an unchanged workspace therefore cannot invalidate that work:
/// its temporary directory, terminal/MCP owner, and model-run registry key are all
/// conversation-scoped. Adding/removing a workspace or changing its kind/path still
/// needs the fence.
///
/// Compare workspace shape only. Conversation lists are host-authoritative at this
/// boundary; comparing them would create false positives for renderer save payloads.
pub fn requires_exclusive_workspace_lifecycle_fence(
    previous: &AppDocument,
    next: &AppDocument,
) -> bool {
    workspace_lifecycle_bindings(previous) != workspace_lifecycle_bindings(next)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn document(path: &str) -> AppDocument {
        crate::catalog::default_document(Path::new(path))
    }

    #[test]
    fn workspace_path_change_invalidates_the_old_terminal_owner() {
        let previous = document("C:/work/old");
        let mut next = previous.clone();
        next.workspaces[0].path = "C:/work/new".into();

        assert_eq!(
            invalidated_conversations(&previous, &next),
            HashSet::from(["conv_welcome".to_owned()])
        );
        assert!(requires_exclusive_workspace_lifecycle_fence(
            &previous, &next
        ));
    }

    #[test]
    fn moving_a_conversation_invalidates_its_old_terminal_binding() {
        let previous = document("C:/work/first");
        let mut next = previous.clone();
        let conversation = next.workspaces[0].conversations.remove(0);
        let mut second = next.workspaces[0].clone();
        second.id = "ws_second".into();
        second.name = "Second".into();
        second.path = "C:/work/second".into();
        second.conversations = vec![conversation];
        next.workspaces.push(second);

        assert_eq!(
            invalidated_conversations(&previous, &next),
            HashSet::from(["conv_welcome".to_owned()])
        );
        // The fence is caused by the added workspace; it does not inspect
        // conversation lists. Conversation moves are invalidated by `crate::conversations`.
        assert!(requires_exclusive_workspace_lifecycle_fence(
            &previous, &next
        ));
    }

    #[test]
    fn presentation_only_workspace_edits_do_not_close_a_terminal() {
        let previous = document("C:/work/stable");
        let mut next = previous.clone();
        next.workspaces[0].name = "Renamed".into();
        next.workspaces[0].conversations[0].title = "Renamed task".into();

        assert!(invalidated_conversations(&previous, &next).is_empty());
        assert!(!requires_exclusive_workspace_lifecycle_fence(
            &previous, &next
        ));
    }

    #[test]
    fn adding_conversations_to_existing_workspaces_needs_no_exclusive_fence() {
        let previous = document("C:/work/stable");
        let template = previous.workspaces[0].conversations[0].clone();
        let mut next = previous.clone();
        for workspace in &mut next.workspaces {
            let mut conversation = template.clone();
            conversation.id = format!("conv_added_{}", workspace.id);
            conversation.title = "Added".into();
            workspace.conversations.push(conversation);
        }

        assert!(invalidated_conversations(&previous, &next).is_empty());
        assert!(!requires_exclusive_workspace_lifecycle_fence(
            &previous, &next
        ));
    }

    #[test]
    fn adding_a_workspace_remains_an_exclusive_lifecycle_change() {
        let previous = document("C:/work/stable");
        let mut next = previous.clone();
        let mut added = next.workspaces[0].clone();
        added.id = "ws_added".into();
        added.path = "C:/work/added".into();
        added.conversations.clear();
        next.workspaces.push(added);

        assert!(requires_exclusive_workspace_lifecycle_fence(
            &previous, &next
        ));
    }

    /// Renderer save payloads omit every workspace's conversations.
    fn renderer_save_payload(document: &AppDocument) -> AppDocument {
        let mut payload = document.clone();
        for workspace in &mut payload.workspaces {
            workspace.conversations.clear();
        }
        payload
    }

    #[test]
    fn a_renderer_save_payload_alone_never_requires_the_exclusive_fence() {
        let previous = document("C:/work/stable");
        let mut next = previous.clone();
        let mut added = next.workspaces[0].conversations[0].clone();
        added.id = "conv_added".into();
        added.title = "Added".into();
        next.workspaces[0].conversations.push(added);
        let payload = renderer_save_payload(&next);

        assert!(workspace_rebound_conversations(&previous, &payload).is_empty());
        assert!(!requires_exclusive_workspace_lifecycle_fence(
            &previous, &payload
        ));
        // Comparing payload conversations would invalidate every previous
        // conversation and require the exclusive fence for all saves.
        assert_eq!(
            invalidated_conversations(&previous, &payload),
            HashSet::from(["conv_welcome".to_owned()])
        );
    }

    #[test]
    fn a_renderer_save_payload_still_reports_workspace_rebinding() {
        let previous = document("C:/work/old");
        let mut next = previous.clone();
        next.workspaces[0].path = "C:/work/new".into();
        let payload = renderer_save_payload(&next);

        assert_eq!(
            workspace_rebound_conversations(&previous, &payload),
            HashSet::from(["conv_welcome".to_owned()])
        );
        assert!(requires_exclusive_workspace_lifecycle_fence(
            &previous, &payload
        ));
    }

    #[test]
    fn removing_a_workspace_rebinds_every_conversation_it_owned() {
        let previous = document("C:/work/stable");
        let owner = previous.workspaces[0].id.clone();
        let mut next = previous.clone();
        next.workspaces.retain(|workspace| workspace.id != owner);
        let payload = renderer_save_payload(&next);

        assert_eq!(
            workspace_rebound_conversations(&previous, &payload),
            HashSet::from(["conv_welcome".to_owned()])
        );
        assert!(requires_exclusive_workspace_lifecycle_fence(
            &previous, &payload
        ));
    }
}
