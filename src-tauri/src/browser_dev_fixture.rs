use std::{
    env, fs,
    path::{Path, PathBuf},
};

const MEMORY_E2E_ENABLE_ENV: &str = "MEWORK_MEMORY_E2E";
const MEMORY_E2E_RUN_ID_ENV: &str = "MEWORK_MEMORY_E2E_RUN_ID";
const MEMORY_E2E_WORKSPACE_ENV: &str = "MEWORK_MEMORY_E2E_WORKSPACE";
const MEMORY_E2E_WORKSPACE_MARKER_ENV: &str = "MEWORK_MEMORY_E2E_WORKSPACE_MARKER";
const MEMORY_E2E_DATA_IDENTIFIER_PREFIX: &str = "com.mework.app.e2e.memory-";
const MEMORY_E2E_MARKER_FILE: &str = ".mework-memory-e2e-workspace";

#[derive(Clone, Debug)]
pub(crate) struct BrowserDevMemoryWorkspaceFixture {
    workspace_path: PathBuf,
    temporary_root: PathBuf,
    run_id: String,
    marker: String,
}

impl BrowserDevMemoryWorkspaceFixture {
    pub(crate) fn authorized_workspace_path(&self) -> Result<PathBuf, String> {
        validate_memory_workspace_path(
            &self.workspace_path,
            &self.temporary_root,
            &self.run_id,
            &self.marker,
        )?;
        Ok(self.workspace_path.clone())
    }
}

pub(crate) fn browser_dev_memory_workspace_fixture(
) -> Result<Option<BrowserDevMemoryWorkspaceFixture>, String> {
    let enabled = optional_unicode_environment(MEMORY_E2E_ENABLE_ENV)?;
    let run_id = optional_unicode_environment(MEMORY_E2E_RUN_ID_ENV)?;
    let workspace = optional_unicode_environment(MEMORY_E2E_WORKSPACE_ENV)?;
    let marker = optional_unicode_environment(MEMORY_E2E_WORKSPACE_MARKER_ENV)?;
    if enabled.is_none() && run_id.is_none() && workspace.is_none() && marker.is_none() {
        return Ok(None);
    }
    if enabled.as_deref() != Some("1") {
        return Err(format!("{MEMORY_E2E_ENABLE_ENV} 只接受精确值 1"));
    }
    let run_id = run_id.ok_or_else(|| format!("缺少 {MEMORY_E2E_RUN_ID_ENV}"))?;
    let workspace = workspace.ok_or_else(|| format!("缺少 {MEMORY_E2E_WORKSPACE_ENV}"))?;
    let marker = marker.ok_or_else(|| format!("缺少 {MEMORY_E2E_WORKSPACE_MARKER_ENV}"))?;
    let data_identifier = env::var("MEWORK_BROWSER_DEV_DATA_IDENTIFIER")
        .map_err(|_| "记忆 E2E 缺少有效应用数据标识".to_owned())?;
    let bridge_token = env::var("MEWORK_BROWSER_DEV_TOKEN")
        .map_err(|_| "记忆 E2E 缺少有效 browser-dev 桥令牌".to_owned())?;
    validate_memory_e2e_fixture(
        &run_id,
        &marker,
        &workspace,
        &data_identifier,
        &bridge_token,
        &env::temp_dir(),
    )
    .map(Some)
}

fn optional_unicode_environment(name: &str) -> Result<Option<String>, String> {
    match env::var(name) {
        Ok(value) => Ok(Some(value)),
        Err(env::VarError::NotPresent) => Ok(None),
        Err(env::VarError::NotUnicode(_)) => Err(format!("{name} 必须是 Unicode 文本")),
    }
}

fn validate_memory_e2e_fixture(
    run_id: &str,
    marker: &str,
    workspace: &str,
    data_identifier: &str,
    bridge_token: &str,
    temporary_root: &Path,
) -> Result<BrowserDevMemoryWorkspaceFixture, String> {
    if !is_lower_hex(run_id, 24) {
        return Err(format!(
            "{MEMORY_E2E_RUN_ID_ENV} 必须是 12 字节随机值的小写十六进制编码"
        ));
    }
    if !is_lower_hex(marker, 64) {
        return Err(format!(
            "{MEMORY_E2E_WORKSPACE_MARKER_ENV} 必须是 32 字节随机值的小写十六进制编码"
        ));
    }
    if data_identifier != format!("{MEMORY_E2E_DATA_IDENTIFIER_PREFIX}{run_id}") {
        return Err("记忆 E2E 必须使用与 run ID 精确绑定的独占应用数据标识".into());
    }
    if !is_lower_hex(bridge_token, 64) {
        return Err("记忆 E2E 只接受 browser-dev 生成的 32 字节随机桥令牌".into());
    }
    let workspace = PathBuf::from(workspace);
    if !workspace.is_absolute() {
        return Err("记忆 E2E 工作区必须是绝对路径".into());
    }
    let temporary_root = fs::canonicalize(temporary_root)
        .map_err(|error| format!("无法解析系统临时目录: {error}"))?;
    let workspace_path = fs::canonicalize(&workspace)
        .map_err(|error| format!("无法解析记忆 E2E 工作区: {error}"))?;
    validate_memory_workspace_path(&workspace_path, &temporary_root, run_id, marker)?;
    Ok(BrowserDevMemoryWorkspaceFixture {
        workspace_path,
        temporary_root,
        run_id: run_id.to_owned(),
        marker: marker.to_owned(),
    })
}

fn validate_memory_workspace_path(
    workspace_path: &Path,
    temporary_root: &Path,
    run_id: &str,
    marker: &str,
) -> Result<(), String> {
    let raw_metadata = fs::symlink_metadata(workspace_path)
        .map_err(|error| format!("无法检查记忆 E2E 工作区: {error}"))?;
    if !raw_metadata.is_dir() || metadata_is_link_or_reparse(&raw_metadata) {
        return Err("记忆 E2E 工作区必须是非链接目录".into());
    }
    let canonical_workspace = fs::canonicalize(workspace_path)
        .map_err(|error| format!("无法重新解析记忆 E2E 工作区: {error}"))?;
    if canonical_workspace != workspace_path {
        return Err("记忆 E2E 工作区必须使用规范路径".into());
    }
    if canonical_workspace.parent() != Some(temporary_root) {
        return Err("记忆 E2E 工作区必须是系统临时目录的直属子目录".into());
    }
    let expected_prefix = format!("mework-memory-e2e-{run_id}-");
    let name = canonical_workspace
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("记忆 E2E 工作区目录名无效")?;
    let suffix = name
        .strip_prefix(&expected_prefix)
        .filter(|suffix| {
            !suffix.is_empty()
                && suffix.len() <= 64
                && suffix.bytes().all(|byte| byte.is_ascii_alphanumeric())
        })
        .ok_or("记忆 E2E 工作区目录名未绑定当前 run ID")?;
    debug_assert!(!suffix.is_empty());

    let marker_path = canonical_workspace.join(MEMORY_E2E_MARKER_FILE);
    let marker_metadata = fs::symlink_metadata(&marker_path)
        .map_err(|error| format!("无法检查记忆 E2E 工作区 marker: {error}"))?;
    if !marker_metadata.is_file()
        || metadata_is_link_or_reparse(&marker_metadata)
        || marker_metadata.len() > 256
    {
        return Err("记忆 E2E 工作区 marker 必须是受限的非链接普通文件".into());
    }
    let actual = fs::read(&marker_path)
        .map_err(|error| format!("无法读取记忆 E2E 工作区 marker: {error}"))?;
    let expected = memory_workspace_marker_body(run_id, marker);
    if actual != expected.as_bytes() {
        return Err("记忆 E2E 工作区 marker 与宿主身份不匹配".into());
    }
    Ok(())
}

fn memory_workspace_marker_body(run_id: &str, marker: &str) -> String {
    format!("MEWORK_MEMORY_E2E_WORKSPACE_V1\nrun={run_id}\nmarker={marker}\n")
}

fn is_lower_hex(value: &str, expected_len: usize) -> bool {
    value.len() == expected_len
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

#[cfg(windows)]
fn metadata_is_link_or_reparse(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt as _;
    metadata.file_type().is_symlink() || metadata.file_attributes() & 0x0000_0400 != 0
}

#[cfg(not(windows))]
fn metadata_is_link_or_reparse(metadata: &fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn valid_memory_fixture() -> (tempfile::TempDir, tempfile::TempDir, String, String, String) {
        let root = tempfile::tempdir().unwrap();
        let run_id = "0123456789abcdef01234567".to_owned();
        let marker = "a".repeat(64);
        let workspace = tempfile::Builder::new()
            .prefix(&format!("mework-memory-e2e-{run_id}-"))
            .tempdir_in(root.path())
            .unwrap();
        let mut marker_file =
            fs::File::create(workspace.path().join(MEMORY_E2E_MARKER_FILE)).unwrap();
        marker_file
            .write_all(memory_workspace_marker_body(&run_id, &marker).as_bytes())
            .unwrap();
        let data_identifier = format!("{MEMORY_E2E_DATA_IDENTIFIER_PREFIX}{run_id}");
        (root, workspace, run_id, marker, data_identifier)
    }

    #[test]
    fn validates_memory_workspace_bound_to_run_marker_data_and_temp_root() {
        let (root, workspace, run_id, marker, data_identifier) = valid_memory_fixture();
        let fixture = validate_memory_e2e_fixture(
            &run_id,
            &marker,
            workspace.path().to_str().unwrap(),
            &data_identifier,
            &"b".repeat(64),
            root.path(),
        )
        .unwrap();
        assert_eq!(
            fixture.authorized_workspace_path().unwrap(),
            fs::canonicalize(workspace.path()).unwrap()
        );

        fs::write(workspace.path().join(MEMORY_E2E_MARKER_FILE), b"tampered").unwrap();
        assert!(fixture
            .authorized_workspace_path()
            .unwrap_err()
            .contains("marker"));
    }

    #[test]
    fn rejects_memory_workspace_identity_or_location_substitution() {
        let (root, workspace, run_id, marker, data_identifier) = valid_memory_fixture();
        assert!(validate_memory_e2e_fixture(
            &run_id,
            &marker,
            workspace.path().to_str().unwrap(),
            &format!("{data_identifier}-forged"),
            &"b".repeat(64),
            root.path(),
        )
        .unwrap_err()
        .contains("应用数据标识"));

        let outside = tempfile::tempdir().unwrap();
        fs::write(
            outside.path().join(MEMORY_E2E_MARKER_FILE),
            memory_workspace_marker_body(&run_id, &marker),
        )
        .unwrap();
        assert!(validate_memory_e2e_fixture(
            &run_id,
            &marker,
            outside.path().to_str().unwrap(),
            &data_identifier,
            &"b".repeat(64),
            root.path(),
        )
        .unwrap_err()
        .contains("临时目录"));
    }
}
