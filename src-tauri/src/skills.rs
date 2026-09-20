//! Skill discovery.
//!
//! A skill is a directory with a `SKILL.md` in it, and the two places Mework
//! looks are the same two levels Claude Code uses: `~/.mework/skills/<dir>/`
//! for every workspace and `<workspace>/.mework/skills/<dir>/` for one. Nothing
//! is copied or registered: the folder on disk is the skill, its directory name
//! is the name the model addresses it by, and a conversation picks the ones it
//! wants by the id discovery mints from the folder's location.

use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::{
    capabilities::{
        normalized_location_for_id, skill_metadata_from_source, stable_id, SKILL_MANIFEST,
        SKILL_READ_LIMIT,
    },
    memory_archive_file,
    model::{ResourceDescriptor, ResourceSource},
};

/// Directory names Claude Code refuses for a skill: they cannot be typed as a
/// slash command there, and here they would make one enum value ambiguous.
pub fn directory_name_is_valid(name: &str) -> bool {
    !name.is_empty()
        && name.trim() == name
        && !name
            .chars()
            .any(|character| character.is_control() || matches!(character, '(' | ')' | ','))
}

fn id_prefix(source: ResourceSource) -> &'static str {
    match source {
        ResourceSource::User => "skill_user",
        ResourceSource::Workspace => "skill_workspace",
        ResourceSource::Builtin => "skill_builtin",
    }
}

/// Every skill directly under `root` (a `skills/` directory), as catalog entries.
///
/// Only direct children that are real directories holding a regular file named
/// exactly `SKILL.md` count; symlinked directories or manifests are skipped so a
/// link cannot delegate what enters the model context to its target's owner. A
/// manifest that cannot be read or is too large is still listed, unavailable,
/// with the reason as its description — the user sees why rather than nothing.
pub fn discover_in_root(
    root: &Path,
    source: ResourceSource,
    workspace_id: Option<&str>,
) -> Vec<ResourceDescriptor> {
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };
    let mut skills = Vec::new();
    for entry in entries.flatten() {
        let directory = entry.path();
        let Ok(metadata) = fs::symlink_metadata(&directory) else {
            continue;
        };
        if !metadata.is_dir() {
            continue;
        }
        let Some(directory_name) = directory.file_name().map(|name| name.to_string_lossy()) else {
            continue;
        };
        if !directory_name_is_valid(&directory_name) {
            continue;
        }
        let manifest = directory.join(SKILL_MANIFEST);
        if !matches!(fs::symlink_metadata(&manifest), Ok(metadata) if metadata.is_file()) {
            continue;
        }
        skills.push(descriptor_for(
            &manifest,
            &directory_name,
            source,
            workspace_id,
        ));
    }
    skills
}

fn descriptor_for(
    manifest: &Path,
    directory_name: &str,
    source: ResourceSource,
    workspace_id: Option<&str>,
) -> ResourceDescriptor {
    let location = manifest.to_string_lossy().into_owned();
    let id = stable_id(id_prefix(source), directory_name, &location);
    match read_manifest(manifest) {
        Ok(text) => {
            let metadata = skill_metadata_from_source(&text, directory_name);
            ResourceDescriptor {
                id,
                name: metadata.name,
                description: metadata.description,
                location,
                source,
                available: true,
                workspace_id: workspace_id.map(str::to_owned),
            }
        }
        Err(reason) => ResourceDescriptor {
            id,
            name: directory_name.to_owned(),
            description: reason,
            location,
            source,
            available: false,
            workspace_id: workspace_id.map(str::to_owned),
        },
    }
}

/// Reads a `SKILL.md` with the same no-follow and size rules a run applies, so
/// an entry the catalog calls available is one a run can actually read.
pub fn read_manifest(manifest: &Path) -> Result<String, String> {
    let bytes = memory_archive_file::read_bounded_nofollow_labeled(
        manifest,
        SKILL_READ_LIMIT as usize,
        "skill",
    )?;
    String::from_utf8(bytes).map_err(|_| format!("{SKILL_MANIFEST} is not valid UTF-8 text"))
}

/// The directory name of a discovered skill — the model-facing name — read
/// back from its manifest location.
pub fn directory_name_of(descriptor: &ResourceDescriptor) -> String {
    Path::new(&descriptor.location)
        .parent()
        .and_then(Path::file_name)
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Deletes a discovered skill's directory.
///
/// The descriptor is re-derived by the caller from a fresh scan, and the
/// directory is deleted only when it is a direct, non-link child of one of
/// `roots` — the two `skills/` directories the scan read from — so a stale or
/// forged location can never reach anything else. Deletion is `remove_dir_all`:
/// a skill's scripts and references live in the same folder and go with it,
/// which is what removing the skill means when the folder is the skill.
pub fn delete_directory(roots: &[PathBuf], descriptor: &ResourceDescriptor) -> Result<(), String> {
    let directory = Path::new(&descriptor.location)
        .parent()
        .ok_or_else(|| "The skill has no directory to delete".to_owned())?;
    let parent_key = directory
        .parent()
        .map(|parent| normalized_location_for_id(&parent.to_string_lossy()));
    let inside_known_root = roots
        .iter()
        .any(|root| Some(normalized_location_for_id(&root.to_string_lossy())) == parent_key);
    if !inside_known_root {
        return Err("The skill is not inside a skills directory Mework reads".into());
    }
    let metadata = fs::symlink_metadata(directory)
        .map_err(|error| format!("Could not inspect the skill directory: {error}"))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("The skill directory is not a plain directory".into());
    }
    fs::remove_dir_all(directory)
        .map_err(|error| format!("Could not delete the skill directory: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_skill(root: &Path, directory: &str, manifest: &str) -> PathBuf {
        let folder = root.join(directory);
        fs::create_dir_all(&folder).unwrap();
        let path = folder.join(SKILL_MANIFEST);
        fs::write(&path, manifest).unwrap();
        path
    }

    #[test]
    fn a_skill_is_a_direct_child_directory_with_a_manifest() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("skills");
        write_skill(
            &root,
            "commit-helper",
            "---\nname: Commit helper\ndescription: Prepare a commit\n---\n\nBody\n",
        );
        write_skill(&root, "bare", "# Bare skill\n\nFirst paragraph.\n");
        // Neither a file at the top level nor a directory without a manifest counts.
        fs::write(root.join("README.md"), "not a skill").unwrap();
        fs::create_dir_all(root.join("empty")).unwrap();
        // Nested skills are not discovered: only direct children are.
        write_skill(&root.join("nested"), "inner", "# Inner\n");

        let mut skills = discover_in_root(&root, ResourceSource::User, None);
        skills.sort_by(|left, right| left.name.cmp(&right.name));

        assert_eq!(skills.len(), 2);
        assert_eq!(skills[0].name, "Bare skill");
        assert_eq!(skills[0].description, "First paragraph.");
        assert_eq!(directory_name_of(&skills[0]), "bare");
        assert!(skills[0].id.starts_with("skill_user_bare_"));
        assert_eq!(skills[1].name, "Commit helper");
        assert_eq!(directory_name_of(&skills[1]), "commit-helper");
        assert!(skills.iter().all(|skill| skill.available));
        assert!(skills.iter().all(|skill| skill.workspace_id.is_none()));
    }

    #[test]
    fn workspace_skills_carry_their_workspace_and_a_distinct_prefix() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("skills");
        write_skill(&root, "deploy", "# Deploy\n");

        let skills = discover_in_root(&root, ResourceSource::Workspace, Some("ws_1"));

        assert_eq!(skills.len(), 1);
        assert!(skills[0].id.starts_with("skill_workspace_deploy_"));
        assert_eq!(skills[0].workspace_id.as_deref(), Some("ws_1"));
        assert_eq!(skills[0].source, ResourceSource::Workspace);
    }

    #[test]
    fn an_unreadable_manifest_is_listed_but_unavailable() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("skills");
        let manifest = write_skill(&root, "huge", "x");
        let oversized = vec![b'x'; SKILL_READ_LIMIT as usize + 1];
        fs::write(&manifest, oversized).unwrap();

        let skills = discover_in_root(&root, ResourceSource::User, None);

        assert_eq!(skills.len(), 1);
        assert!(!skills[0].available);
        assert_eq!(skills[0].name, "huge");
        assert!(
            skills[0].description.contains("skill"),
            "{}",
            skills[0].description
        );
    }

    #[test]
    fn directory_names_follow_claude_code_rules() {
        assert!(directory_name_is_valid("commit-helper"));
        assert!(directory_name_is_valid("代码评审"));
        assert!(!directory_name_is_valid(""));
        assert!(!directory_name_is_valid(" padded"));
        assert!(!directory_name_is_valid("a,b"));
        assert!(!directory_name_is_valid("call(me)"));
        assert!(!directory_name_is_valid("tab\there"));
    }

    #[test]
    fn deleting_a_skill_removes_only_a_direct_child_of_a_known_root() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("skills");
        let manifest = write_skill(&root, "gone", "# Gone\n");
        fs::write(root.join("gone").join("helper.py"), "print(1)").unwrap();
        let elsewhere = write_skill(&temp.path().join("other"), "kept", "# Kept\n");
        let known = discover_in_root(&root, ResourceSource::User, None);
        let foreign = ResourceDescriptor {
            id: "forged".into(),
            name: "kept".into(),
            description: String::new(),
            location: elsewhere.to_string_lossy().into_owned(),
            source: ResourceSource::User,
            available: true,
            workspace_id: None,
        };

        let roots = vec![root.clone()];
        let error = delete_directory(&roots, &foreign).unwrap_err();
        assert!(error.contains("not inside"), "{error}");
        assert!(elsewhere.exists());

        delete_directory(&roots, &known[0]).unwrap();
        assert!(!manifest.exists());
        assert!(!root.join("gone").exists());
    }
}
