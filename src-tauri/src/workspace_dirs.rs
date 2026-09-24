use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

use sha2::{Digest, Sha256};

use crate::model::{AppDocument, Workspace, WorkspaceKind};

const TEMPORARY_WORKSPACE_ROOT: &str = "temporary-workspaces";
/// Host-side scratch directories for conversations whose primary workspace is
/// on another machine. The host's own subsystems — preview, LSP, the shell's
/// cwd file, image staging — need a real local directory whatever machine the
/// model is addressing, and a remote root is not one.
const REMOTE_ANCHOR_ROOT: &str = "remote-workspace-anchors";

pub(crate) fn ensure_temporary_workspace(
    app_data: &Path,
    conversation_id: &str,
) -> Result<PathBuf, String> {
    ensure_conversation_workspace(
        app_data,
        TEMPORARY_WORKSPACE_ROOT,
        conversation_id,
        "临时工作区",
    )
}

/// The local anchor of a conversation whose primary workspace is remote.
pub(crate) fn ensure_remote_workspace_anchor(
    app_data: &Path,
    conversation_id: &str,
) -> Result<PathBuf, String> {
    ensure_conversation_workspace(app_data, REMOTE_ANCHOR_ROOT, conversation_id, "远端工作区锚点")
}

/// Makes the App-Data-backed temporary workspace tree match the persisted
/// document. Running this after every successful save also retries cleanup that
/// may have been interrupted by a process exit or a transient filesystem lock.
///
/// `drafts` maps each renderer draft that has a shell open to the project it
/// opened it in (see `TerminalManager::bind_draft`). A draft's directory is the
/// one its conversation will use, keyed by the id it will materialize as, so it
/// is kept for as long as the draft's shell may be sitting in it.
pub(crate) fn reconcile_temporary_workspaces(
    app_data: &Path,
    document: &AppDocument,
    drafts: &HashMap<String, String>,
) -> Result<(), String> {
    let expected = owner_ids(document, drafts, |workspace| {
        workspace.kind == WorkspaceKind::Temporary
    })
    .into_iter()
    .map(workspace_directory_name)
    .collect::<Result<HashSet<_>, _>>()?;
    let root = ensure_workspace_root(app_data, TEMPORARY_WORKSPACE_ROOT, "临时工作区")?;

    for directory_name in &expected {
        let directory = root.join(directory_name);
        fs::create_dir_all(&directory)
            .map_err(|error| format!("无法创建临时工作区目录 {}: {error}", directory.display()))?;
        validate_direct_child_directory(&root, &directory, "临时工作区")?;
    }

    remove_orphan_workspace_entries(&root, &expected, "临时工作区")?;
    reconcile_remote_workspace_anchors(app_data, document, drafts)
}

/// The conversations in the workspaces `select` picks, and the drafts aimed at
/// one of them.
fn owner_ids<'a>(
    document: &'a AppDocument,
    drafts: &'a HashMap<String, String>,
    select: impl Fn(&Workspace) -> bool,
) -> Vec<&'a str> {
    let selected = document
        .workspaces
        .iter()
        .filter(|workspace| select(workspace))
        .collect::<Vec<_>>();
    let drafted = drafts
        .iter()
        .filter(|(_, workspace_id)| {
            selected
                .iter()
                .any(|workspace| workspace.id == **workspace_id)
        })
        .map(|(owner, _)| owner.as_str());
    selected
        .iter()
        .flat_map(|workspace| workspace.conversations.iter())
        .map(|conversation| conversation.id.as_str())
        .chain(drafted)
        .collect()
}

/// Drops the anchors of conversations that no longer exist or whose workspace
/// is back on this machine. Anchors are created on demand, so nothing is made
/// here; a missing one is recreated by the next run that needs it.
fn reconcile_remote_workspace_anchors(
    app_data: &Path,
    document: &AppDocument,
    drafts: &HashMap<String, String>,
) -> Result<(), String> {
    let expected = owner_ids(document, drafts, |workspace| {
        workspace.kind == WorkspaceKind::Directory && workspace.machine.is_some()
    })
    .into_iter()
    .map(workspace_directory_name)
    .collect::<Result<HashSet<_>, _>>()?;
    let root = ensure_workspace_root(app_data, REMOTE_ANCHOR_ROOT, "远端工作区锚点")?;
    remove_orphan_workspace_entries(&root, &expected, "远端工作区锚点")
}

fn remove_orphan_workspace_entries(
    root: &Path,
    expected: &HashSet<String>,
    label: &str,
) -> Result<(), String> {
    for entry in fs::read_dir(root).map_err(|error| format!("无法读取{label}根目录: {error}"))?
    {
        let entry = entry.map_err(|error| format!("无法读取{label}目录项: {error}"))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !is_workspace_directory_name(&name) || expected.contains(&name) {
            continue;
        }
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| format!("无法检查待清理的{label} {}: {error}", path.display()))?;
        if is_link_like(&metadata) {
            remove_link_entry(&path, &metadata, label)?;
            continue;
        }
        if metadata.is_dir() {
            validate_direct_child_directory(root, &path, &format!("待清理的{label}"))?;
            fs::remove_dir_all(&path)
                .map_err(|error| format!("无法删除{label} {}: {error}", path.display()))?;
        } else {
            fs::remove_file(&path)
                .map_err(|error| format!("无法删除无效{label}文件 {}: {error}", path.display()))?;
        }
    }
    Ok(())
}

fn ensure_conversation_workspace(
    app_data: &Path,
    root_name: &str,
    conversation_id: &str,
    label: &str,
) -> Result<PathBuf, String> {
    let directory_name = workspace_directory_name(conversation_id)?;
    let root = ensure_workspace_root(app_data, root_name, label)?;
    let workspace = root.join(directory_name);
    fs::create_dir_all(&workspace).map_err(|error| format!("无法创建{label}目录: {error}"))?;
    validate_direct_child_directory(&root, &workspace, label)
}

#[cfg(not(windows))]
fn is_link_like(metadata: &fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

#[cfg(windows)]
fn is_link_like(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
    metadata.file_type().is_symlink()
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn remove_link_entry(path: &Path, _metadata: &fs::Metadata, label: &str) -> Result<(), String> {
    fs::remove_file(path)
        .map_err(|error| format!("无法删除孤儿{label}链接 {}: {error}", path.display()))
}

#[cfg(windows)]
fn remove_link_entry(path: &Path, metadata: &fs::Metadata, label: &str) -> Result<(), String> {
    use std::os::windows::fs::MetadataExt;

    const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x0010;
    let result = if metadata.file_attributes() & FILE_ATTRIBUTE_DIRECTORY != 0 {
        fs::remove_dir(path)
    } else {
        fs::remove_file(path)
    };
    result.map_err(|error| format!("无法删除孤儿{label}链接 {}: {error}", path.display()))
}

fn ensure_workspace_root(app_data: &Path, root_name: &str, label: &str) -> Result<PathBuf, String> {
    fs::create_dir_all(app_data).map_err(|error| format!("无法创建应用数据目录: {error}"))?;
    let canonical_app_data = fs::canonicalize(app_data)
        .map_err(|error| format!("无法访问应用数据目录 {}: {error}", app_data.display()))?;
    if !canonical_app_data.is_dir() {
        return Err("应用数据路径不是目录".into());
    }
    let root = canonical_app_data.join(root_name);
    fs::create_dir_all(&root).map_err(|error| format!("无法创建{label}根目录: {error}"))?;
    let root =
        fs::canonicalize(&root).map_err(|error| format!("无法验证{label}根目录: {error}"))?;
    if !root.is_dir() || !root.starts_with(&canonical_app_data) {
        return Err(format!("{label}根目录越出应用数据目录"));
    }
    Ok(root)
}

fn validate_direct_child_directory(
    root: &Path,
    directory: &Path,
    label: &str,
) -> Result<PathBuf, String> {
    let metadata =
        fs::symlink_metadata(directory).map_err(|error| format!("无法检查{label}目录: {error}"))?;
    if is_link_like(&metadata) {
        return Err(format!("拒绝使用符号链接形式的{label}目录"));
    }
    let canonical =
        fs::canonicalize(directory).map_err(|error| format!("无法验证{label}目录: {error}"))?;
    if !canonical.is_dir() || canonical.parent() != Some(root) {
        return Err(format!("{label}目录越出可信根目录"));
    }
    Ok(canonical)
}

pub(crate) fn workspace_directory_name(workspace_key: &str) -> Result<String, String> {
    if workspace_key.trim().is_empty() {
        return Err("工作区标识不能为空".into());
    }
    let digest = Sha256::digest(workspace_key.as_bytes());
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn is_workspace_directory_name(name: &str) -> bool {
    name.len() == 64
        && name
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temporary_workspaces_remain_conversation_scoped() {
        let app_data = tempfile::tempdir().unwrap();
        let first = ensure_temporary_workspace(app_data.path(), "conversation-one").unwrap();
        let second = ensure_temporary_workspace(app_data.path(), "conversation-two").unwrap();
        let canonical_app_data = fs::canonicalize(app_data.path()).unwrap();
        assert_ne!(first, second);
        assert!(first.starts_with(&canonical_app_data));
        assert!(second.starts_with(&canonical_app_data));
    }

    #[test]
    fn a_drafts_scratch_directory_lasts_while_the_draft_is_bound_to_it() {
        let app_data = tempfile::tempdir().unwrap();
        let document = crate::catalog::default_document();
        let temporary = document
            .workspaces
            .iter()
            .find(|workspace| workspace.kind == WorkspaceKind::Temporary)
            .expect("the default document has the temporary project")
            .id
            .clone();
        let directory = ensure_temporary_workspace(app_data.path(), "conv_draft").unwrap();

        let drafts = HashMap::from([("conv_draft".to_owned(), temporary)]);
        reconcile_temporary_workspaces(app_data.path(), &document, &drafts).unwrap();
        assert!(directory.is_dir(), "the draft's shell may be sitting in it");

        // Aimed at a project directory instead, the draft owns no scratch directory.
        let drafts = HashMap::from([("conv_draft".to_owned(), document.workspaces[0].id.clone())]);
        reconcile_temporary_workspaces(app_data.path(), &document, &drafts).unwrap();
        assert!(!directory.exists());
    }

    #[test]
    fn remote_anchors_are_dropped_once_their_workspace_is_back_on_this_machine() {
        let app_data = tempfile::tempdir().unwrap();
        let mut document = crate::catalog::default_document();
        let workspace = &mut document.workspaces[0];
        workspace.machine = Some(crate::model::RunTarget::Ssh {
            machine_id: "m1".into(),
        });
        let conversation_id = workspace.conversations[0].id.clone();
        let anchor = ensure_remote_workspace_anchor(app_data.path(), &conversation_id).unwrap();
        assert!(anchor.is_dir());
        assert_ne!(
            anchor,
            ensure_temporary_workspace(app_data.path(), &conversation_id).unwrap(),
            "the anchor is not the temporary workspace: that tree is reconciled by other rules"
        );

        reconcile_temporary_workspaces(app_data.path(), &document, &HashMap::new()).unwrap();
        assert!(anchor.is_dir(), "a remote workspace keeps its anchor");

        document.workspaces[0].machine = None;
        reconcile_temporary_workspaces(app_data.path(), &document, &HashMap::new()).unwrap();
        assert!(!anchor.exists(), "a workspace back on this machine needs no anchor");
    }
}
