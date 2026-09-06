//! On-disk storage for the skill registry.
//!
//! Installed skill directories are copied to `<app_data>/skills/<folder_name>`
//! so uninstalling a skill cannot be undone by a later external scan.
//!
//! Skill contents are injected verbatim into model context and must be owned by
//! the application so another process cannot change them between runs.
//!
//! A skill may contain at most 100 MiB and 2,000 entries. ZIP path traversal,
//! symlinks, reparse points, absolute entries, and `..` segments are rejected.

use std::{
    collections::BTreeSet,
    fs::{self, File},
    io,
    path::{Component, Path, PathBuf},
};

use sha2::{Digest, Sha256};

use crate::{
    capabilities::{skill_metadata_from_source, SKILL_MANIFEST},
    model::{SkillRecord, SkillSource},
};

/// Maximum total bytes for one skill.
const MAX_SKILL_BYTES: u64 = 100 * 1024 * 1024;
/// Maximum number of entries for one skill.
const MAX_SKILL_ENTRIES: usize = 2_000;
/// Maximum bytes read from a skill manifest; matches `capabilities.rs`
/// `SKILL_READ_LIMIT`.
const MAX_MANIFEST_BYTES: usize = 256 * 1024;
const MAX_FOLDER_NAME_CHARS: usize = 128;

/// Root directory for installed skills.
pub fn skills_root(app_data: &Path) -> PathBuf {
    app_data.join("skills")
}

/// A candidate skill found by scanning known external directories. It requires
/// explicit user import.
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemSkillCandidate {
    /// Display name of the source directory.
    pub source_name: String,
    /// Skill directory name.
    pub folder_name: String,
    pub name: String,
    pub description: String,
    /// Absolute path to the skill directory.
    pub directory_path: String,
    /// Whether an installed skill already uses this directory name. Conflicting
    /// candidates remain visible but cannot be imported.
    pub conflict: bool,
}

/// External skill roots to scan, relative to home.
const SYSTEM_SKILL_ROOTS: &[(&str, &str)] = &[
    ("Mework", ".mework/skills"),
    ("Claude Code", ".claude/skills"),
    ("Codex", ".codex/skills"),
    ("Agent Skills", ".config/skills"),
];

/// Scans known skill directories without copying files or modifying the document.
pub fn scan_system(
    workspace_paths: &[String],
    installed: &[SkillRecord],
) -> Vec<SystemSkillCandidate> {
    let taken = installed
        .iter()
        .map(|skill| skill.folder_name.to_lowercase())
        .collect::<BTreeSet<_>>();
    let mut roots: Vec<(String, PathBuf)> = Vec::new();
    if let Some(home) = dirs::home_dir() {
        for (label, relative) in SYSTEM_SKILL_ROOTS {
            roots.push(((*label).to_owned(), home.join(relative)));
        }
    }
    for workspace in workspace_paths {
        if workspace.trim().is_empty() {
            continue;
        }
        let base = PathBuf::from(workspace);
        roots.push(("工作区".to_owned(), base.join(".mework").join("skills")));
        roots.push((
            "工作区 Claude".to_owned(),
            base.join(".claude").join("skills"),
        ));
    }

    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut candidates = Vec::new();
    for (source_name, root) in roots {
        let Ok(entries) = fs::read_dir(&root) else {
            continue;
        };
        for entry in entries.flatten() {
            let directory = entry.path();
            let Some(manifest) = manifest_in(&directory) else {
                continue;
            };
            // The same directory may be discovered through multiple roots.
            // Deduplicate by absolute path, not directory name.
            if !seen.insert(directory.to_string_lossy().to_lowercase()) {
                continue;
            }
            let folder_name = entry.file_name().to_string_lossy().into_owned();
            let (name, description) =
                metadata_of(&manifest).unwrap_or_else(|_| (folder_name.clone(), String::new()));
            candidates.push(SystemSkillCandidate {
                source_name: source_name.clone(),
                conflict: taken.contains(&folder_name.to_lowercase()),
                folder_name,
                name,
                description,
                directory_path: directory.to_string_lossy().into_owned(),
            });
        }
    }
    candidates.sort_by(|left, right| {
        left.name
            .to_lowercase()
            .cmp(&right.name.to_lowercase())
            .then_with(|| left.directory_path.cmp(&right.directory_path))
    });
    candidates
}

/// Installs a skill from a directory.
pub fn install_from_directory(
    app_data: &Path,
    source: &Path,
    installed: &[SkillRecord],
    origin: SkillSource,
) -> Result<SkillRecord, String> {
    let metadata = fs::symlink_metadata(source)
        .map_err(|error| format!("无法读取技能目录：{error}"))?;
    if !metadata.is_dir() {
        return Err("技能来源必须是一个目录".into());
    }
    let manifest = manifest_in(source).ok_or_else(|| {
        format!("目录里没有 {SKILL_MANIFEST}：一个技能必须以它作为正文入口")
    })?;
    let folder_name = folder_name_for(source, installed)?;
    let destination = skills_root(app_data).join(&folder_name);
    // Create the destination before copying. A failed copy must remove its
    // partial directory so it does not block a later installation.
    fs::create_dir_all(skills_root(app_data))
        .map_err(|error| format!("无法创建技能目录：{error}"))?;
    if destination.exists() {
        return Err(format!("技能目录名已被占用：{folder_name}"));
    }
    if let Err(error) = copy_skill_tree(source, &destination) {
        let _ = fs::remove_dir_all(&destination);
        return Err(error);
    }
    finish_install(&destination, &manifest, source, folder_name, origin)
}

/// Installs a skill from a ZIP archive.
pub fn install_from_zip(
    app_data: &Path,
    archive_path: &Path,
    installed: &[SkillRecord],
    origin: SkillSource,
) -> Result<SkillRecord, String> {
    let file = File::open(archive_path).map_err(|error| format!("无法打开压缩包：{error}"))?;
    let mut archive =
        zip::ZipArchive::new(file).map_err(|error| format!("压缩包无法解析：{error}"))?;
    if archive.len() > MAX_SKILL_ENTRIES {
        return Err(format!("压缩包条目超过 {MAX_SKILL_ENTRIES} 个"));
    }
    // Determine the wrapper depth before choosing the directory name so
    // extraction does not create a redundant enclosing directory.
    let (strip_prefix, archive_folder) = zip_layout(&mut archive)?;
    let folder_name = sanitize_folder_name(&archive_folder.unwrap_or_else(|| {
        archive_path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| "skill".to_owned())
    }))?;
    ensure_folder_available(&folder_name, installed)?;
    let destination = skills_root(app_data).join(&folder_name);
    fs::create_dir_all(skills_root(app_data))
        .map_err(|error| format!("无法创建技能目录：{error}"))?;
    if destination.exists() {
        return Err(format!("技能目录名已被占用：{folder_name}"));
    }
    if let Err(error) = extract_zip(&mut archive, &strip_prefix, &destination) {
        let _ = fs::remove_dir_all(&destination);
        return Err(error);
    }
    let manifest = match manifest_in(&destination) {
        Some(manifest) => manifest,
        None => {
            let _ = fs::remove_dir_all(&destination);
            return Err(format!("压缩包里没有 {SKILL_MANIFEST}"));
        }
    };
    finish_install(&destination, &manifest, archive_path, folder_name, origin)
}

/// Removes a skill directory; the caller removes its document index.
pub fn remove_skill(app_data: &Path, folder_name: &str) -> Result<(), String> {
    let folder = sanitize_folder_name(folder_name)?;
    let target = skills_root(app_data).join(&folder);
    match fs::symlink_metadata(&target) {
        Ok(metadata) if metadata.is_dir() => fs::remove_dir_all(&target)
            .map_err(|error| format!("无法删除技能目录：{error}")),
        // Absence is a successful uninstall because the required outcome is
        // that the skill no longer exists.
        Ok(_) | Err(_) => Ok(()),
    }
}

/// Finalizes installation by parsing metadata, canonicalizing the manifest name,
/// and calculating the content hash.
fn finish_install(
    destination: &Path,
    manifest: &Path,
    origin_path: &Path,
    folder_name: String,
    origin: SkillSource,
) -> Result<SkillRecord, String> {
    // Canonicalize lowercase `skill.md` to `SKILL.md`; reads recognize one name
    // to avoid case-insensitive filesystem conflicts.
    let canonical_manifest = destination.join(SKILL_MANIFEST);
    if manifest != canonical_manifest {
        fs::rename(manifest, &canonical_manifest)
            .map_err(|error| format!("无法归一化 {SKILL_MANIFEST}：{error}"))?;
    }
    let bytes = crate::memory_archive_file::read_bounded_nofollow_labeled(
        &canonical_manifest,
        MAX_MANIFEST_BYTES,
        "技能",
    )?;
    let source_text = String::from_utf8(bytes.clone())
        .map_err(|_| format!("{SKILL_MANIFEST} 不是有效的 UTF-8 文本"))?;
    let parsed = skill_metadata_from_source(&source_text, &folder_name);
    let now = chrono::Utc::now().to_rfc3339();
    Ok(SkillRecord {
        id: format!("skill_{}", short_hash(&folder_name, &parsed.name)),
        name: parsed.name,
        description: parsed.description,
        folder_name,
        source: origin,
        source_location: origin_path.to_string_lossy().into_owned(),
        source_url: String::new(),
        author: parsed.author,
        version: parsed.version,
        tags: parsed.tags,
        content_hash: hex_digest(&bytes),
        enabled: true,
        installed_at: now.clone(),
        updated_at: now,
    })
}

/// Finds a skill manifest in a directory. Both filename casings are accepted,
/// but the file must not be a link.
fn manifest_in(directory: &Path) -> Option<PathBuf> {
    for candidate in [SKILL_MANIFEST, "skill.md"] {
        let path = directory.join(candidate);
        if matches!(fs::symlink_metadata(&path), Ok(metadata) if metadata.is_file()) {
            return Some(path);
        }
    }
    None
}

fn metadata_of(manifest: &Path) -> io::Result<(String, String)> {
    let mut content = String::new();
    use std::io::Read as _;
    File::open(manifest)?
        .take(64 * 1024)
        .read_to_string(&mut content)?;
    let fallback = manifest
        .parent()
        .and_then(|parent| parent.file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "skill".to_owned());
    let parsed = skill_metadata_from_source(&content, &fallback);
    Ok((parsed.name, parsed.description))
}

fn folder_name_for(source: &Path, installed: &[SkillRecord]) -> Result<String, String> {
    let raw = source
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let folder = sanitize_folder_name(&raw)?;
    ensure_folder_available(&folder, installed)?;
    Ok(folder)
}

fn ensure_folder_available(folder: &str, installed: &[SkillRecord]) -> Result<(), String> {
    // Windows treats `Foo` and `foo` as the same directory. Two records pointing
    // to it would let removing either record delete the other's contents.
    let lowered = folder.to_lowercase();
    if installed
        .iter()
        .any(|skill| skill.folder_name.to_lowercase() == lowered)
    {
        return Err(format!("已经安装过同名技能：{folder}"));
    }
    Ok(())
}

fn sanitize_folder_name(raw: &str) -> Result<String, String> {
    let mut cleaned = String::new();
    for character in raw.trim().chars() {
        if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
            cleaned.push(character);
        } else if !cleaned.ends_with('-') {
            cleaned.push('-');
        }
    }
    let cleaned = cleaned.trim_matches(['-', '.']).to_owned();
    if cleaned.is_empty() || cleaned.chars().count() > MAX_FOLDER_NAME_CHARS {
        return Err("技能目录名必须是 1–128 个可用字符".into());
    }
    Ok(cleaned)
}

/// Recursively copies a skill directory. Symlinks and reparse points are
/// rejected because their targets may escape the skill directory and change
/// independently.
fn copy_skill_tree(source: &Path, destination: &Path) -> Result<(), String> {
    let mut entries = 0usize;
    let mut bytes = 0u64;
    fs::create_dir_all(destination).map_err(|error| format!("无法创建技能目录：{error}"))?;
    copy_directory(source, destination, &mut entries, &mut bytes)
}

fn copy_directory(
    source: &Path,
    destination: &Path,
    entries: &mut usize,
    bytes: &mut u64,
) -> Result<(), String> {
    let listing = fs::read_dir(source).map_err(|error| format!("无法读取技能目录：{error}"))?;
    for entry in listing {
        let entry = entry.map_err(|error| format!("无法读取技能目录项：{error}"))?;
        let metadata = entry
            .metadata()
            .map_err(|error| format!("无法读取技能目录项属性：{error}"))?;
        let file_type = entry.file_type().map_err(|error| format!("无法判定条目类型：{error}"))?;
        if file_type.is_symlink() {
            return Err("技能目录里不能包含符号链接".into());
        }
        *entries += 1;
        if *entries > MAX_SKILL_ENTRIES {
            return Err(format!("技能条目超过 {MAX_SKILL_ENTRIES} 个"));
        }
        let target = destination.join(entry.file_name());
        if file_type.is_dir() {
            fs::create_dir_all(&target).map_err(|error| format!("无法创建子目录：{error}"))?;
            copy_directory(&entry.path(), &target, entries, bytes)?;
            continue;
        }
        *bytes += metadata.len();
        if *bytes > MAX_SKILL_BYTES {
            return Err("技能总大小超过 100 MiB".into());
        }
        fs::copy(entry.path(), &target).map_err(|error| format!("无法复制技能文件：{error}"))?;
    }
    Ok(())
}

/// Determines whether a ZIP has a skill at its root or inside one enclosing
/// directory, returning the prefix to remove and its directory name.
fn zip_layout(
    archive: &mut zip::ZipArchive<File>,
) -> Result<(String, Option<String>), String> {
    let mut top_level: BTreeSet<String> = BTreeSet::new();
    let mut manifest_at_root = false;
    for index in 0..archive.len() {
        let entry = archive
            .by_index(index)
            .map_err(|error| format!("压缩包条目无法读取：{error}"))?;
        let Some(path) = entry.enclosed_name() else {
            // A missing enclosed name indicates a path escaping the extraction
            // root. Reject the entire archive because it is untrusted.
            return Err("压缩包里有越界的路径条目".into());
        };
        let mut components = path.components();
        let Some(Component::Normal(first)) = components.next() else {
            continue;
        };
        let first = first.to_string_lossy().into_owned();
        if components.next().is_none() {
            if first.eq_ignore_ascii_case(SKILL_MANIFEST) {
                manifest_at_root = true;
            }
        }
        top_level.insert(first);
    }
    if manifest_at_root {
        return Ok((String::new(), None));
    }
    if top_level.len() == 1 {
        let folder = top_level.into_iter().next().expect("恰好一个顶层条目");
        return Ok((folder.clone(), Some(folder)));
    }
    Err(format!("压缩包根目录里没有 {SKILL_MANIFEST}"))
}

fn extract_zip(
    archive: &mut zip::ZipArchive<File>,
    strip_prefix: &str,
    destination: &Path,
) -> Result<(), String> {
    fs::create_dir_all(destination).map_err(|error| format!("无法创建技能目录：{error}"))?;
    let mut bytes = 0u64;
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|error| format!("压缩包条目无法读取：{error}"))?;
        let Some(path) = entry.enclosed_name() else {
            return Err("压缩包里有越界的路径条目".into());
        };
        let relative = if strip_prefix.is_empty() {
            path.to_path_buf()
        } else {
            match path.strip_prefix(strip_prefix) {
                Ok(stripped) => stripped.to_path_buf(),
                Err(_) => continue,
            }
        };
        if relative.as_os_str().is_empty() {
            continue;
        }
        let target = destination.join(&relative);
        if entry.is_dir() {
            fs::create_dir_all(&target).map_err(|error| format!("无法创建子目录：{error}"))?;
            continue;
        }
        bytes += entry.size();
        if bytes > MAX_SKILL_BYTES {
            return Err("技能总大小超过 100 MiB".into());
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|error| format!("无法创建子目录：{error}"))?;
        }
        let mut output =
            File::create(&target).map_err(|error| format!("无法写入技能文件：{error}"))?;
        io::copy(&mut entry, &mut output)
            .map_err(|error| format!("无法解压技能文件：{error}"))?;
    }
    Ok(())
}

fn hex_digest(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn short_hash(folder: &str, name: &str) -> String {
    let digest = Sha256::digest(format!("{folder}\u{1f}{name}").as_bytes());
    digest
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_skill(root: &Path, folder: &str, body: &str) -> PathBuf {
        let directory = root.join(folder);
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join(SKILL_MANIFEST), body).unwrap();
        directory
    }

    #[test]
    fn install_copies_the_tree_and_parses_frontmatter() {
        let temp = tempfile::tempdir().unwrap();
        let app_data = temp.path().join("app");
        let source = write_skill(
            &temp.path().join("src"),
            "my-skill",
            "---\nname: My Skill\ndescription: does things\nversion: 1.2.3\n---\n\nbody\n",
        );
        fs::write(source.join("extra.txt"), "hello").unwrap();

        let record = install_from_directory(&app_data, &source, &[], SkillSource::LocalDirectory)
            .expect("install succeeds");

        assert_eq!(record.name, "My Skill");
        assert_eq!(record.description, "does things");
        assert_eq!(record.version, "1.2.3");
        assert_eq!(record.folder_name, "my-skill");
        assert!(record.enabled);
        assert!(skills_root(&app_data)
            .join("my-skill")
            .join(SKILL_MANIFEST)
            .is_file());
        assert!(skills_root(&app_data).join("my-skill").join("extra.txt").is_file());
    }

    #[test]
    fn install_normalizes_a_lowercase_manifest_name() {
        let temp = tempfile::tempdir().unwrap();
        let app_data = temp.path().join("app");
        let source = temp.path().join("src").join("lower");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("skill.md"), "# Lower\n\nbody\n").unwrap();

        let record = install_from_directory(&app_data, &source, &[], SkillSource::LocalDirectory)
            .expect("install succeeds");

        assert_eq!(record.name, "Lower");
        assert!(skills_root(&app_data)
            .join(&record.folder_name)
            .join(SKILL_MANIFEST)
            .is_file());
    }

    #[test]
    fn install_rejects_a_directory_without_a_manifest() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("src").join("empty");
        fs::create_dir_all(&source).unwrap();

        let error = install_from_directory(
            &temp.path().join("app"),
            &source,
            &[],
            SkillSource::LocalDirectory,
        )
        .expect_err("a directory without SKILL.md is not a skill");

        assert!(error.contains(SKILL_MANIFEST), "{error}");
    }

    #[test]
    fn install_rejects_a_folder_name_already_taken_case_insensitively() {
        let temp = tempfile::tempdir().unwrap();
        let source = write_skill(&temp.path().join("src"), "Alpha", "# Alpha\n");
        let installed = vec![SkillRecord {
            folder_name: "alpha".into(),
            ..Default::default()
        }];

        let error = install_from_directory(
            &temp.path().join("app"),
            &source,
            &installed,
            SkillSource::LocalDirectory,
        )
        .expect_err("case-insensitive collision is still a collision");

        assert!(error.contains("同名技能"), "{error}");
    }

    #[test]
    fn removing_an_absent_skill_is_not_an_error() {
        let temp = tempfile::tempdir().unwrap();
        remove_skill(temp.path(), "never-installed").expect("absent removal is a no-op");
    }

    #[test]
    fn sanitize_rejects_names_that_reduce_to_nothing() {
        assert!(sanitize_folder_name("...").is_err());
        assert_eq!(sanitize_folder_name("My Skill!").unwrap(), "My-Skill");
    }
}
