use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

use sha2::{Digest, Sha256};

use crate::model::{AppDocument, WorkspaceKind};

const TEMPORARY_WORKSPACE_ROOT: &str = "temporary-workspaces";

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

/// Makes the App-Data-backed temporary workspace tree match the persisted
/// document. Running this after every successful save also retries cleanup that
/// may have been interrupted by a process exit or a transient filesystem lock.
pub(crate) fn reconcile_temporary_workspaces(
    app_data: &Path,
    document: &AppDocument,
) -> Result<(), String> {
    let expected = document
        .workspaces
        .iter()
        .filter(|workspace| workspace.kind == WorkspaceKind::Temporary)
        .flat_map(|workspace| workspace.conversations.iter())
        .map(|conversation| workspace_directory_name(&conversation.id))
        .collect::<Result<HashSet<_>, _>>()?;
    let root = ensure_workspace_root(app_data, TEMPORARY_WORKSPACE_ROOT, "临时工作区")?;

    for directory_name in &expected {
        let directory = root.join(directory_name);
        fs::create_dir_all(&directory)
            .map_err(|error| format!("无法创建临时工作区目录 {}: {error}", directory.display()))?;
        validate_direct_child_directory(&root, &directory, "临时工作区")?;
    }

    remove_orphan_workspace_entries(&root, &expected, "临时工作区")
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
}
