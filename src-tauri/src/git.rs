//! Git for the host: the shared implementation in `git_core`, which the remote
//! agent also runs for checkouts on other machines (see `remote_git`).

pub use git_core::*;

/// Releases a conversation's isolated worktree on this host, with the same
/// retain-on-change semantics as [`release_isolated_worktree`].
pub(crate) fn release_conversation_worktree(
    workspace: &std::path::Path,
    worktree: &crate::model::ConversationWorktree,
) -> Result<bool, String> {
    release_isolated_worktree(
        workspace,
        &IsolatedWorktree {
            path: std::path::PathBuf::from(&worktree.path),
            branch: worktree.branch.clone(),
            base_oid: worktree.base_oid.clone(),
        },
    )
}
