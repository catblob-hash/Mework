//! Editable on-disk copies of the two built-in prompt profiles.
//!
//! The registry in [`crate::prompt_profile`] is compiled into the binary, but
//! the texts a model actually reads have to be changeable without a rebuild: at
//! startup the host materializes both built-ins as JSON files under the
//! application data directory, and every conversation that resolves to a
//! built-in id renders from the file rather than from the compiled table. A key
//! a newer build adds is filled in on the next start; the texts already in the
//! file are the user's and are never rewritten.
//!
//! The files are application-owned but user-edited, so reads go through the same
//! bounded no-follow discipline as the tool-description files beside them, and a
//! write publishes atomically through a temporary file in the same directory. A
//! file that cannot be parsed is left exactly as it is — overwriting it would
//! destroy the only copy of what the user wrote.

use std::{
    collections::HashMap,
    fs, io,
    path::{Path, PathBuf},
};

use serde_json::{Map, Value};

use crate::{
    capabilities, memory_archive_file,
    model::{ResolvedLanguage, ToolDescriptionEntry},
    prompt_profile::{self, PromptKey, PromptProfile},
};

/// Directory under the application data directory holding both profiles.
const PROFILE_DIRECTORY: &str = "prompt-profiles";
/// Size bound for one profile file. The compiled documents are around 45 KiB,
/// so this leaves room for longer user texts without admitting an arbitrary
/// file that happens to sit at the path.
const PROFILE_SIZE_LIMIT: usize = 256 * 1024;
const PROFILE_LABEL: &str = "内置提示词档案";

/// Where the built-in profile for `language` is edited.
pub fn builtin_profile_path(app_data: &Path, language: ResolvedLanguage) -> PathBuf {
    app_data.join(PROFILE_DIRECTORY).join(match language {
        ResolvedLanguage::EnUs => "en-US.json",
        ResolvedLanguage::ZhCn => "zh-CN.json",
    })
}

/// Creates both profile files and fills in the keys this build declares but the
/// file does not. Returns the files this call created or rewrote.
///
/// A file that already spells out every key is left untouched, byte for byte,
/// so materialization is invisible to a user who has edited one. One language
/// failing never stops the other: both are attempted and the failures are
/// reported together.
pub fn materialize_builtin_profiles(app_data: &Path) -> Result<Vec<PathBuf>, String> {
    let mut written = Vec::new();
    let mut failures = Vec::new();
    for language in [ResolvedLanguage::EnUs, ResolvedLanguage::ZhCn] {
        match materialize_one(app_data, language) {
            Ok(Some(path)) => written.push(path),
            Ok(None) => {}
            Err(error) => failures.push(error),
        }
    }
    if failures.is_empty() {
        Ok(written)
    } else {
        Err(failures.join("；"))
    }
}

/// The profile a run renders with for a built-in id: the compiled texts of
/// `language` with the on-disk file's `prompts` and `tools` applied on top.
///
/// Every failure — no file yet, a symlink at the path, an oversized or
/// unparseable file — resolves to the compiled built-in. A damaged file changes
/// the wording back to what shipped; it never fails a run.
pub fn load_builtin_profile(app_data: &Path, language: ResolvedLanguage) -> PromptProfile {
    read_profile_overrides(&builtin_profile_path(app_data, language))
        .map(|(overrides, tools)| PromptProfile::builtin_with_overrides(language, overrides, tools))
        .unwrap_or_else(|_| PromptProfile::builtin_for_language(language))
}

fn materialize_one(app_data: &Path, language: ResolvedLanguage) -> Result<Option<PathBuf>, String> {
    let path = builtin_profile_path(app_data, language);
    let directory = path
        .parent()
        .ok_or_else(|| format!("{PROFILE_LABEL}目标缺少父目录"))?;
    fs::create_dir_all(directory)
        .map_err(|_| format!("无法创建{PROFILE_LABEL}目录 {}", directory.display()))?;
    let builtin = PromptProfile::builtin_for_language(language);

    // `symlink_metadata` rather than `exists`: a link at the path is not a
    // missing file, and the write path rejects it instead of following it.
    let existing = match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(_) => return Err(format!("无法检查{PROFILE_LABEL} {}", path.display())),
        Ok(_) => Some(read_profile_object(&path)?),
    };
    let Some(existing) = existing else {
        let (prompts, _) = merged_prompts(&Map::new(), &builtin);
        write_profile(
            &path,
            &document_text(&builtin.name, &prompts, &Value::Array(Vec::new())),
        )?;
        return Ok(Some(path));
    };

    let (prompts, changed) = merged_prompts(&existing, &builtin);
    if !changed {
        return Ok(None);
    }
    let name = existing
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or(&builtin.name);
    let tools = existing
        .get("tools")
        .cloned()
        .unwrap_or_else(|| Value::Array(Vec::new()));
    write_profile(&path, &document_text(name, &prompts, &tools))?;
    Ok(Some(path))
}

/// The file's `prompts` completed with this build's registry: every declared key
/// in registry order — the file's own text where it has one, the compiled text
/// where it does not — followed by the ids this build does not declare.
///
/// An unknown id is kept rather than dropped: it may belong to a build the user
/// downgraded from, and materialization is not the place to decide that a text
/// is obsolete. The boolean says whether anything had to be filled in; when it
/// is false the caller leaves the file alone.
fn merged_prompts(
    existing: &Map<String, Value>,
    builtin: &PromptProfile,
) -> (Vec<(String, Value)>, bool) {
    let declared = existing.get("prompts").and_then(Value::as_object);
    let mut prompts = Vec::with_capacity(PromptKey::ALL.len());
    let mut changed = false;
    for key in PromptKey::ALL {
        // Only a string is a text; anything else is treated as absent, which is
        // also how the profile parser reads it.
        match declared
            .and_then(|prompts| prompts.get(key.id()))
            .and_then(Value::as_str)
        {
            Some(text) => prompts.push((key.id().to_owned(), Value::String(text.to_owned()))),
            None => {
                changed = true;
                prompts.push((
                    key.id().to_owned(),
                    Value::String(builtin.text(*key).to_owned()),
                ));
            }
        }
    }
    if let Some(declared) = declared {
        for (id, value) in declared {
            if PromptKey::parse(id).is_none() {
                prompts.push((id.clone(), value.clone()));
            }
        }
    }
    (prompts, changed)
}

/// One profile file: `name`, `prompts` in the order given, `tools`.
///
/// Written by hand because the key order carries meaning here — a `serde_json`
/// map sorts its keys and would scatter the registry, leaving a user who
/// compares two builds no way to see what a new key belongs to.
fn document_text(name: &str, prompts: &[(String, Value)], tools: &Value) -> String {
    let mut text = String::from("{\n  \"name\": ");
    text.push_str(&Value::String(name.to_owned()).to_string());
    text.push_str(",\n  \"prompts\": ");
    if prompts.is_empty() {
        text.push_str("{}");
    } else {
        text.push('{');
        for (index, (id, value)) in prompts.iter().enumerate() {
            if index > 0 {
                text.push(',');
            }
            text.push_str("\n    ");
            text.push_str(&Value::String(id.clone()).to_string());
            text.push_str(": ");
            text.push_str(&indented(value, 4));
        }
        text.push_str("\n  }");
    }
    text.push_str(",\n  \"tools\": ");
    text.push_str(&indented(tools, 2));
    text.push_str("\n}\n");
    text
}

/// `value` as pretty JSON whose continuation lines sit at `spaces`.
fn indented(value: &Value, spaces: usize) -> String {
    let rendered = serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string());
    let padding = " ".repeat(spaces);
    rendered.replace('\n', &format!("\n{padding}"))
}

fn write_profile(path: &Path, text: &str) -> Result<(), String> {
    memory_archive_file::write_all_nofollow_labeled(
        path,
        text.as_bytes(),
        PROFILE_SIZE_LIMIT,
        PROFILE_LABEL,
    )
    .map_err(|error| format!("{}：{error}", path.display()))
}

fn read_profile_overrides(
    path: &Path,
) -> Result<(HashMap<PromptKey, String>, Vec<ToolDescriptionEntry>), String> {
    let value = Value::Object(read_profile_object(path)?);
    Ok((
        prompt_profile::parse_prompt_overrides(&value),
        capabilities::parse_tool_description_entries(&value),
    ))
}

fn read_profile_object(path: &Path) -> Result<Map<String, Value>, String> {
    let bytes =
        memory_archive_file::read_bounded_nofollow_labeled(path, PROFILE_SIZE_LIMIT, PROFILE_LABEL)
            .map_err(|error| format!("{}：{error}", path.display()))?;
    match serde_json::from_slice::<Value>(&bytes) {
        Ok(Value::Object(object)) => Ok(object),
        Ok(_) => Err(format!("{} 的顶层必须是 JSON 对象", path.display())),
        Err(_) => Err(format!("{} 不是有效的 JSON", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ids of the `prompts` object in file order.
    fn prompt_ids_in_file(text: &str) -> Vec<String> {
        text.lines()
            .filter_map(|line| line.strip_prefix("    \""))
            .filter_map(|line| line.split_once("\": "))
            .map(|(id, _)| id.to_owned())
            .collect()
    }

    #[test]
    fn a_fresh_directory_gets_both_profiles_with_every_key() {
        let directory = tempfile::tempdir().unwrap();
        let written = materialize_builtin_profiles(directory.path()).unwrap();

        assert_eq!(
            written,
            vec![
                builtin_profile_path(directory.path(), ResolvedLanguage::EnUs),
                builtin_profile_path(directory.path(), ResolvedLanguage::ZhCn),
            ]
        );
        for language in [ResolvedLanguage::EnUs, ResolvedLanguage::ZhCn] {
            let text =
                fs::read_to_string(builtin_profile_path(directory.path(), language)).unwrap();
            assert_eq!(
                prompt_ids_in_file(&text),
                PromptKey::ALL
                    .iter()
                    .map(|key| key.id().to_owned())
                    .collect::<Vec<_>>()
            );
            let compiled = PromptProfile::builtin_for_language(language);
            let loaded = load_builtin_profile(directory.path(), language);
            assert_eq!(loaded.id, compiled.id);
            assert_eq!(loaded.language, language);
            for key in PromptKey::ALL {
                assert_eq!(loaded.text(*key), compiled.text(*key), "{}", key.id());
            }
        }

        // Nothing left to fill in, so a second start rewrites nothing.
        assert_eq!(
            materialize_builtin_profiles(directory.path()).unwrap(),
            Vec::<PathBuf>::new()
        );
    }

    #[test]
    fn a_user_edit_survives_materialization_and_reaches_the_run() {
        let directory = tempfile::tempdir().unwrap();
        materialize_builtin_profiles(directory.path()).unwrap();
        let path = builtin_profile_path(directory.path(), ResolvedLanguage::EnUs);
        let edited = fs::read_to_string(&path).unwrap().replace(
            &format!(
                "\"{}\": {}",
                PromptKey::SystemMcpServerDefaultDescription.id(),
                Value::String(
                    PromptKey::SystemMcpServerDefaultDescription
                        .builtin_en()
                        .to_owned()
                )
            ),
            &format!(
                "\"{}\": \"MEWORK_PROFILE_FILE_PROBE\"",
                PromptKey::SystemMcpServerDefaultDescription.id()
            ),
        );
        fs::write(&path, &edited).unwrap();

        assert_eq!(
            materialize_builtin_profiles(directory.path()).unwrap(),
            Vec::<PathBuf>::new()
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), edited);
        let profile = load_builtin_profile(directory.path(), ResolvedLanguage::EnUs);
        assert_eq!(
            profile.text(PromptKey::SystemMcpServerDefaultDescription),
            "MEWORK_PROFILE_FILE_PROBE"
        );
        // Keys the file did not touch still come from the compiled built-in.
        assert_eq!(
            profile.text(PromptKey::SystemCapabilityRow),
            PromptKey::SystemCapabilityRow.builtin_en()
        );
    }

    #[test]
    fn a_key_a_newer_build_added_is_filled_in_registry_order() {
        let directory = tempfile::tempdir().unwrap();
        let path = builtin_profile_path(directory.path(), ResolvedLanguage::EnUs);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            r#"{
              "name": "renamed",
              "prompts": {
                "system.capability_row": "- {name} :: {description}",
                "prompt.from.another.build": "kept"
              },
              "tools": [{"toolName": "ls", "schemaNotes": "Absolute paths only.", "usageGuidance": ""}]
            }"#,
        )
        .unwrap();

        assert_eq!(
            materialize_builtin_profiles(directory.path()).unwrap(),
            vec![
                path.clone(),
                builtin_profile_path(directory.path(), ResolvedLanguage::ZhCn),
            ]
        );

        let text = fs::read_to_string(&path).unwrap();
        let ids = prompt_ids_in_file(&text);
        let mut expected = PromptKey::ALL
            .iter()
            .map(|key| key.id().to_owned())
            .collect::<Vec<_>>();
        // An id this build does not declare keeps its text and moves to the end.
        expected.push("prompt.from.another.build".to_owned());
        assert_eq!(ids, expected);

        let profile = load_builtin_profile(directory.path(), ResolvedLanguage::EnUs);
        assert_eq!(
            profile.text(PromptKey::SystemCapabilityRow),
            "- {name} :: {description}"
        );
        assert_eq!(
            profile.text(PromptKey::SystemMcpSection),
            PromptKey::SystemMcpSection.builtin_en()
        );
        // `name` and `tools` are the user's; filling keys in does not touch them.
        assert!(text.contains("\"name\": \"renamed\""));
        assert_eq!(profile.tools.len(), 1);
        assert_eq!(profile.tools[0].tool_name, "ls");
        assert_eq!(
            profile.text(PromptKey::ToolLsDescription),
            "Absolute paths only."
        );
    }

    #[test]
    fn an_unparseable_file_is_left_alone_and_the_run_falls_back() {
        let directory = tempfile::tempdir().unwrap();
        let path = builtin_profile_path(directory.path(), ResolvedLanguage::EnUs);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "{ this is not JSON").unwrap();

        let error = materialize_builtin_profiles(directory.path()).unwrap_err();

        assert!(error.contains("不是有效的 JSON"), "{error}");
        assert_eq!(fs::read_to_string(&path).unwrap(), "{ this is not JSON");
        // The other language is still materialized.
        assert!(builtin_profile_path(directory.path(), ResolvedLanguage::ZhCn).is_file());
        assert_eq!(
            load_builtin_profile(directory.path(), ResolvedLanguage::EnUs),
            PromptProfile::builtin_english()
        );
    }

    #[test]
    fn a_missing_file_resolves_to_the_compiled_profile() {
        let directory = tempfile::tempdir().unwrap();
        assert_eq!(
            load_builtin_profile(directory.path(), ResolvedLanguage::ZhCn),
            PromptProfile::builtin_chinese()
        );
    }
}
