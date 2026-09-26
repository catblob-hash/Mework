//! The skills, MCP servers and hooks a fresh installation starts with.
//!
//! These are ordinary user files under `~/.mework`, identical in kind to
//! anything the user writes there by hand: the catalog page lists them, the
//! conversation settings select them, and deleting one deletes it for good.
//! Nothing here mints a `ResourceSource::Builtin` row — `capabilities.rs`'s
//! `discovery_ships_no_builtin_skills` pins that there is no such thing, and a
//! built-in the user cannot delete would be a worse deal than no built-in.
//!
//! Writing is gated by a marker file in the application data directory rather
//! than by whether the files are on disk. "Absent" and "never seeded" are not
//! the same state: the user who deletes a skill directory has said something,
//! and re-creating it on the next launch would be the app arguing back. Wiping
//! the data directory (`npm run reset:data`, a fresh profile) does re-seed,
//! which is the factory-reset reading and the one people expect.
//!
//! The marker also carries the ids, because they are not derivable later
//! without re-deriving the paths: [`crate::capabilities::stable_id`] hashes the
//! absolute location, so an id minted here is specific to this machine's home
//! directory and cannot be written into either seed document as a literal.
//! Every start therefore asks for them where the built-in preset is installed
//! — see `storage::install_builtin_preset` — and hands them straight to it.
//! After the first run that is one read of the marker.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::capabilities::{stable_id, CapabilityKind};

/// Bumped when the shipped set changes, which re-runs seeding once for every
/// installation whose marker predates the bump. Re-runs only ever add: an entry
/// the user deleted stays deleted, because writing skips anything already on
/// disk and the merges below only touch keys nobody else owns.
const SEED_VERSION: u32 = 1;
const MARKER_FILE: &str = "capability-builtin-seed.json";

/// Matches `capabilities::SKILL_READ_LIMIT`; a manifest above it reads as
/// unavailable, so refusing to write one is better than shipping a dead row.
const SKILL_SIZE_LIMIT: usize = 256 * 1024;
const CONFIG_SIZE_LIMIT: usize = 1024 * 1024;

/// Namespace for the handlers this module owns inside `hooks.json`, which the
/// user also edits. A handler outside it is theirs and is never rewritten or
/// removed. The MCP side needs no such constant — merging there is by exact
/// server name — but the same `builtin_` convention names those entries, and a
/// test holds them to it.
const HOOK_NAME_PREFIX: &str = "Mework built-in";

/// The vendored skill manifests, compiled in.
///
/// `include_str!` rather than a bundled resource: it works identically in the
/// packaged binary, in `cargo test` and in the `dev:browser` host, and needs no
/// bundle configuration. Provenance and re-sync steps are in
/// `src-tauri/resources/builtin-skills/README.md`.
const BUILTIN_SKILLS: &[(&str, &str)] = &[
    (
        "systematic-debugging",
        include_str!("../resources/builtin-skills/systematic-debugging/SKILL.md"),
    ),
    (
        "verification-before-completion",
        include_str!("../resources/builtin-skills/verification-before-completion/SKILL.md"),
    ),
    (
        "code-review-and-quality",
        include_str!("../resources/builtin-skills/code-review-and-quality/SKILL.md"),
    ),
    (
        "git-workflow-and-versioning",
        include_str!("../resources/builtin-skills/git-workflow-and-versioning/SKILL.md"),
    ),
    (
        "resolving-merge-conflicts",
        include_str!("../resources/builtin-skills/resolving-merge-conflicts/SKILL.md"),
    ),
];

/// The MCP servers a fresh installation starts with.
///
/// Every one is stdio, launched through `npx`/`uvx`, and needs no credential.
/// That last part is load-bearing rather than a preference: an entry carrying a
/// `${VAR}` reference that does not resolve is parsed as unavailable, and a
/// *selected* unavailable server fails every run of that conversation closed.
/// A missing `npx` or `uvx` is the benign failure by comparison — the entry
/// still parses, the spawn fails fast, discovery isolates it, and the run
/// proceeds without that server's tools.
const BUILTIN_MCP_SERVERS: &[(&str, &str, &[&str], &str)] = &[
    (
        "builtin_sequential_thinking",
        "npx",
        &["-y", "@modelcontextprotocol/server-sequential-thinking"],
        "Structured, revisable chains of thought for problems that need to be worked through step by step. Requires Node.js.",
    ),
    (
        "builtin_context7",
        "npx",
        &["-y", "@upstash/context7-mcp"],
        "Up-to-date, version-specific documentation and code examples for public libraries. Works without an API key at an anonymous rate limit. Requires Node.js.",
    ),
    (
        "builtin_fetch",
        "uvx",
        &["mcp-server-fetch"],
        "Fetches a URL and returns it as markdown, in chunks. Read-only. Requires uv.",
    ),
    (
        "builtin_time",
        "uvx",
        &["mcp-server-time"],
        "Current time and time-zone conversion, so the model does not have to guess today's date. Requires uv.",
    ),
];

/// One hook handler this module owns, as it is written into `hooks.json`.
struct BuiltinHook {
    event: &'static str,
    matcher: Option<&'static str>,
    name: &'static str,
    status_message: &'static str,
    /// Runs under `bash -lc`.
    command: &'static str,
    /// Runs under `pwsh`/`powershell -NoLogo -NoProfile -NonInteractive`.
    command_windows: &'static str,
}

/// The hooks a fresh installation starts with — seeded, listed, and selected by
/// nothing.
///
/// The built-in preset selects no hook, and that is deliberate. A hook id is a
/// hash of its *position* in `hooks.json` (`#/hooks/<event>/<group>/<handler>`),
/// so deleting or reordering any handler renumbers the rest, and a selected id
/// that no longer resolves fails every run of that conversation closed — by
/// design, since a guard that silently stops guarding is worse than one that
/// refuses to start. Pre-selecting a hook would therefore contradict the whole
/// point of shipping these as files the user is free to delete.
///
/// Every command is written twice, POSIX and PowerShell, and every one is inert
/// on failure: none can block, none writes outside the workspace, and a non-zero
/// exit or a missing shell only paints the hook card red.
const BUILTIN_HOOKS: &[BuiltinHook] = &[
    BuiltinHook {
        event: "SessionStart",
        matcher: None,
        name: "Mework built-in · repository state",
        status_message: "Reading repository state",
        // Non-JSON stdout on SessionStart becomes additional model context, so
        // this is how a hook hands the model a fact. `|| true` keeps a
        // non-repository workspace from painting the card red.
        command: "git rev-parse --abbrev-ref HEAD 2>/dev/null | sed 's/^/Current git branch: /' || true",
        command_windows: "$branch = git rev-parse --abbrev-ref HEAD 2>$null; if ($LASTEXITCODE -eq 0 -and $branch) { Write-Output \"Current git branch: $branch\" }; exit 0",
    },
    BuiltinHook {
        event: "PreToolUse",
        matcher: Some("^(write|edit)$"),
        name: "Mework built-in · protect secret files",
        status_message: "Checking the edit target",
        // Exit 2 is the block, and stderr is the reason the model is shown.
        // The match is anchored on the file name so `.env.example` and
        // `envelope.ts` do not trip it.
        command: "python3 -c \"import json,sys,os,re;d=json.load(sys.stdin);p=(d.get('tool_input') or {}).get('path') or '';n=os.path.basename(p);sys.exit(2) if re.fullmatch(r'\\\\.env(\\\\.(local|development|production|test))?', n) else sys.exit(0)\" 2>/dev/null || exit 0",
        command_windows: "try { $d = [Console]::In.ReadToEnd() | ConvertFrom-Json; $n = Split-Path -Leaf ([string]$d.tool_input.path); if ($n -match '^\\.env(\\.(local|development|production|test))?$') { [Console]::Error.Write(\"Refusing to edit $n: it is a secret file. Ask the user to change it by hand, or turn this hook off.\"); exit 2 } } catch { }; exit 0",
    },
    BuiltinHook {
        event: "PostToolUse",
        matcher: Some("^(bash|zsh|sh|powershell)$"),
        name: "Mework built-in · shell command log",
        status_message: "Recording the command",
        // Append-only, inside the workspace, and silent: stdout stays empty so
        // nothing is added to the model's context and nothing can be parsed as
        // a decision.
        command: "mkdir -p .mework && date '+%Y-%m-%dT%H:%M:%S%z shell tool finished' >> .mework/shell-hook.log 2>/dev/null || true",
        command_windows: "try { New-Item -ItemType Directory -Force -Path .mework | Out-Null; Add-Content -Path .mework/shell-hook.log -Value \"$(Get-Date -Format o) shell tool finished\" } catch { }; exit 0",
    },
];

/// The ids the built-in preset selects, handed back so the caller can write them
/// into its settings.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct BuiltinCapabilitySelection {
    /// Ids of every seeded skill, in the order of [`BUILTIN_SKILLS`].
    #[serde(default)]
    pub skill_ids: Vec<String>,
    /// Ids of every seeded MCP server, in the order of [`BUILTIN_MCP_SERVERS`].
    #[serde(default)]
    pub mcp_ids: Vec<String>,
}

#[derive(Serialize, Deserialize)]
struct SeedMarker {
    version: u32,
    seeded_at: String,
    #[serde(flatten)]
    selection: BuiltinCapabilitySelection,
}

/// Writes the built-in capability files once and returns what they are called.
///
/// Never fails the caller: a home directory this process cannot write is a
/// reason to start without built-ins, not a reason not to start. Every problem
/// is reported to stderr and the selection comes back with whatever did land.
#[cfg(not(test))]
pub fn seed_builtin_capabilities(app_data: &Path) -> BuiltinCapabilitySelection {
    let Some(level) = crate::capabilities::ConfigLevel::user() else {
        eprintln!("内置技能与 MCP 未播种：当前平台没有主目录");
        return BuiltinCapabilitySelection::default();
    };
    seed_into(app_data, &level.config_directory())
}

/// Inert under test, because the real one resolves `dirs::home_dir()` and every
/// `storage::load_or_initialize` test would otherwise write built-ins into the
/// developer's own `~/.mework`. What it would have done is covered directly:
/// [`seed_into`] takes both directories and the tests below drive it against
/// temporary ones, and `storage::put_builtin_preset` is tested against a
/// selection built by hand.
#[cfg(test)]
pub fn seed_builtin_capabilities(_app_data: &Path) -> BuiltinCapabilitySelection {
    BuiltinCapabilitySelection::default()
}

/// The whole of the work, with both directories passed in.
///
/// Separated from [`seed_builtin_capabilities`] purely so the tests can drive
/// it: resolving the home directory internally would make every test run write
/// built-ins into the developer's own `~/.mework`.
fn seed_into(app_data: &Path, config_directory: &Path) -> BuiltinCapabilitySelection {
    let marker_path = app_data.join(MARKER_FILE);
    if let Some(marker) = read_marker(&marker_path) {
        if marker.version >= SEED_VERSION {
            return marker.selection;
        }
    }
    let mut selection = BuiltinCapabilitySelection::default();
    match seed_skills(config_directory) {
        Ok(ids) => selection.skill_ids = ids,
        Err(error) => eprintln!("内置技能未能落盘：{error}"),
    }
    match seed_mcp_servers(config_directory) {
        Ok(ids) => selection.mcp_ids = ids,
        Err(error) => eprintln!("内置 MCP 未能落盘：{error}"),
    }
    if let Err(error) = seed_hooks(config_directory) {
        eprintln!("内置钩子未能落盘：{error}");
    }
    write_marker(&marker_path, &selection);
    selection
}

fn read_marker(path: &Path) -> Option<SeedMarker> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// A marker that cannot be written is only reported, never fatal. The cost of
/// losing it is one repeated seeding pass, and that pass skips everything
/// already on disk.
fn write_marker(path: &Path, selection: &BuiltinCapabilitySelection) {
    let marker = SeedMarker {
        version: SEED_VERSION,
        seeded_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        selection: selection.clone(),
    };
    let Ok(text) = serde_json::to_string_pretty(&marker) else {
        return;
    };
    if let Some(parent) = path.parent() {
        if let Err(error) = std::fs::create_dir_all(parent) {
            eprintln!("内置能力播种标记未能落盘：{error}");
            return;
        }
    }
    if let Err(error) = std::fs::write(path, format!("{text}\n")) {
        eprintln!("内置能力播种标记未能落盘：{error}");
    }
}

/// Writes each manifest to `<config>/skills/<dir>/SKILL.md`, skipping any
/// directory that already holds one.
///
/// The id is minted from the path whether or not this call wrote the file, so
/// a user who already has a directory by one of these names keeps their own
/// manifest and the preset points at it. That is the right outcome: the name is
/// what the model sees, and two manifests under one name is exactly the
/// collision that fails a run.
fn seed_skills(config_directory: &Path) -> Result<Vec<String>, String> {
    let root = config_directory.join(CapabilityKind::Skills.relative_path());
    let mut ids = Vec::new();
    let mut failures = Vec::new();
    for (directory, manifest) in BUILTIN_SKILLS {
        let path = root.join(directory).join("SKILL.md");
        if !path.exists() {
            if let Some(parent) = path.parent() {
                if let Err(error) = std::fs::create_dir_all(parent) {
                    failures.push(format!("{directory}：{error}"));
                    continue;
                }
            }
            if let Err(error) = crate::memory_archive_file::write_all_nofollow_labeled(
                &path,
                manifest.as_bytes(),
                SKILL_SIZE_LIMIT,
                "技能说明",
            ) {
                failures.push(format!("{directory}：{error}"));
                continue;
            }
        }
        ids.push(stable_id(
            "skill_user",
            directory,
            &path.to_string_lossy(),
        ));
    }
    if failures.is_empty() {
        Ok(ids)
    } else {
        Err(failures.join("；"))
    }
}

/// Merges the built-in servers into `<config>/mcp.json`, leaving every other
/// key byte-identical.
///
/// The file is rewritten through `serde_json` rather than spliced, which is safe
/// here only because this runs before the user has one: a pre-existing file
/// belongs to the user, so any `builtin_` name already present is left exactly
/// as they wrote it and only genuinely new names are added.
fn seed_mcp_servers(config_directory: &Path) -> Result<Vec<String>, String> {
    let path = config_directory.join(CapabilityKind::Mcp.relative_path());
    let mut document = read_json_object(&path)?;
    let mut servers = match document.remove("mcpServers") {
        Some(Value::Object(servers)) => servers,
        // A file whose `mcpServers` is not an object is skipped wholesale by the
        // scanner, and replacing it would destroy whatever the user meant. Add
        // nothing and report it.
        Some(_) => {
            return Err(format!(
                "{} 的 mcpServers 不是对象，未写入内置条目",
                path.display()
            ))
        }
        None => Map::new(),
    };
    let mut ids = Vec::new();
    let mut added = false;
    for (name, command, arguments, description) in BUILTIN_MCP_SERVERS {
        if !servers.contains_key(*name) {
            servers.insert(
                (*name).to_owned(),
                json!({
                    "type": "stdio",
                    "command": command,
                    "args": arguments,
                    "description": description,
                }),
            );
            added = true;
        }
        ids.push(stable_id(
            "mcp_user",
            name,
            &format!("{}#/mcpServers/{name}", path.to_string_lossy()),
        ));
    }
    document.insert("mcpServers".into(), Value::Object(servers));
    if added {
        write_json_object(&path, &document, "MCP 配置")?;
    }
    Ok(ids)
}

/// Merges the built-in handlers into `<config>/hooks.json`.
///
/// Ownership is by handler `name`: a rerun replaces the handlers this module
/// wrote and never touches the rest. Positions shift as a result, which changes
/// the ids of the handlers after them — acceptable precisely because no preset
/// selects a hook, so there is no stored id to invalidate.
fn seed_hooks(config_directory: &Path) -> Result<(), String> {
    let path = config_directory.join(CapabilityKind::Hooks.relative_path());
    let mut document = read_json_object(&path)?;
    let mut events = match document.remove("hooks") {
        Some(Value::Object(events)) => events,
        Some(_) => {
            return Err(format!(
                "{} 的 hooks 不是对象，未写入内置条目",
                path.display()
            ))
        }
        None => Map::new(),
    };
    // One group per event per matcher, so a matcher the user also uses stays
    // their group and ours stays ours.
    let mut grouped: BTreeMap<(&str, Option<&str>), Vec<Value>> = BTreeMap::new();
    for hook in BUILTIN_HOOKS {
        grouped
            .entry((hook.event, hook.matcher))
            .or_default()
            .push(json!({
                "type": "command",
                "name": hook.name,
                "statusMessage": hook.status_message,
                "command": hook.command,
                "commandWindows": hook.command_windows,
                "timeout": 30,
            }));
    }
    for ((event, matcher), handlers) in grouped {
        let mut groups = match events.remove(event) {
            Some(Value::Array(groups)) => groups,
            Some(_) => Vec::new(),
            None => Vec::new(),
        };
        groups.retain(|group| !group_is_ours(group));
        let mut group = Map::new();
        if let Some(matcher) = matcher {
            group.insert("matcher".into(), Value::String(matcher.into()));
        }
        group.insert("hooks".into(), Value::Array(handlers));
        groups.push(Value::Object(group));
        events.insert(event.to_owned(), Value::Array(groups));
    }
    document.insert("hooks".into(), Value::Object(events));
    write_json_object(&path, &document, "钩子配置")
}

/// Whether every handler in a group carries this module's name prefix. A group
/// the user has added a handler to is theirs from then on.
fn group_is_ours(group: &Value) -> bool {
    let Some(handlers) = group.get("hooks").and_then(Value::as_array) else {
        return false;
    };
    !handlers.is_empty()
        && handlers.iter().all(|handler| {
            handler
                .get("name")
                .and_then(Value::as_str)
                .is_some_and(|name| name.starts_with(HOOK_NAME_PREFIX))
        })
}

/// Reads a JSON object, treating "no file" as "empty object".
///
/// A file that exists but does not parse is an error rather than something to
/// overwrite: it is the only copy of whatever the user wrote.
fn read_json_object(path: &Path) -> Result<Map<String, Value>, String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Ok(Map::new());
    };
    if text.trim().is_empty() {
        return Ok(Map::new());
    }
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(object)) => Ok(object),
        Ok(_) => Err(format!("{} 的顶层不是对象，未写入内置条目", path.display())),
        Err(error) => Err(format!("{} 无法解析（{error}），未写入内置条目", path.display())),
    }
}

fn write_json_object(path: &Path, document: &Map<String, Value>, label: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let text = serde_json::to_string_pretty(document).map_err(|error| error.to_string())?;
    crate::memory_archive_file::write_all_nofollow_labeled(
        path,
        format!("{text}\n").as_bytes(),
        CONFIG_SIZE_LIMIT,
        label,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Names the `builtin_` namespace so the table below is held to it even
    /// though the merge itself keys on exact server names.
    const SERVER_PREFIX: &str = "builtin_";

    /// Every vendored manifest has to survive the scanner, or the built-in ships
    /// as an unavailable row that fails any run selecting it.
    #[test]
    fn every_vendored_skill_manifest_parses_and_names_its_directory() {
        for (directory, manifest) in BUILTIN_SKILLS {
            assert!(
                manifest.starts_with("---\n"),
                "{directory} 的说明缺少 frontmatter 起始行"
            );
            let body = manifest.strip_prefix("---\n").unwrap();
            let (frontmatter, rest) = body
                .split_once("\n---\n")
                .unwrap_or_else(|| panic!("{directory} 的说明缺少 frontmatter 收尾行"));
            let name = frontmatter
                .lines()
                .find_map(|line| line.strip_prefix("name:"))
                .map(str::trim)
                .unwrap_or_else(|| panic!("{directory} 的说明没有 name"));
            assert_eq!(
                name, *directory,
                "技能的模型可见名是目录名，frontmatter 必须与之一致"
            );
            assert!(
                frontmatter
                    .lines()
                    .any(|line| line.starts_with("description:")),
                "{directory} 的说明没有 description"
            );
            assert!(!rest.trim().is_empty(), "{directory} 的说明没有正文");
            assert!(
                manifest.len() <= SKILL_SIZE_LIMIT,
                "{directory} 的说明超过读取上限"
            );
            assert!(
                crate::skills::directory_name_is_valid(directory),
                "{directory} 不是合法的技能目录名"
            );
        }
    }

    /// A `${VAR}` without a default makes an entry unavailable, and a selected
    /// unavailable server fails every run of that conversation closed. The
    /// built-in preset selects all of these, so none may carry one.
    #[test]
    fn no_seeded_mcp_server_depends_on_an_environment_variable() {
        for (name, command, arguments, _) in BUILTIN_MCP_SERVERS {
            assert!(
                name.starts_with(SERVER_PREFIX),
                "{name} 不在本模块拥有的命名空间里"
            );
            assert!(!command.contains("${"), "{name} 的命令引用了环境变量");
            for argument in *arguments {
                assert!(!argument.contains("${"), "{name} 的参数引用了环境变量");
            }
        }
    }

    /// Both spellings are required: on Windows the POSIX command would be handed
    /// to PowerShell verbatim, and the reverse elsewhere.
    #[test]
    fn every_seeded_hook_is_written_for_both_shells_and_can_be_addressed() {
        for hook in BUILTIN_HOOKS {
            assert!(
                hook.name.starts_with(HOOK_NAME_PREFIX),
                "{} 不在本模块拥有的命名空间里",
                hook.name
            );
            assert!(!hook.command.trim().is_empty(), "{} 缺少命令", hook.name);
            assert!(
                !hook.command_windows.trim().is_empty(),
                "{} 缺少 Windows 命令",
                hook.name
            );
            if let Some(matcher) = hook.matcher {
                regex::Regex::new(matcher)
                    .unwrap_or_else(|_| panic!("{} 的 matcher 不是合法正则", hook.name));
            }
        }
    }

    /// Seeding twice must not write twice, and must report the same ids both
    /// times — the second call is what every relaunch after the first performs.
    #[test]
    fn seeding_is_idempotent_and_replays_its_ids_from_the_marker() {
        let app_data = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let config = home.path().join(".mework");
        let first = seed_into(app_data.path(), &config);
        let marker = app_data.path().join(MARKER_FILE);
        assert!(marker.exists(), "播种后应当留下标记文件");
        assert_eq!(first.skill_ids.len(), BUILTIN_SKILLS.len());
        assert_eq!(first.mcp_ids.len(), BUILTIN_MCP_SERVERS.len());
        let stamp = std::fs::read_to_string(&marker).unwrap();
        let second = seed_into(app_data.path(), &config);
        assert_eq!(first.skill_ids, second.skill_ids);
        assert_eq!(first.mcp_ids, second.mcp_ids);
        assert_eq!(
            stamp,
            std::fs::read_to_string(&marker).unwrap(),
            "第二次播种不应改写标记"
        );
    }

    /// The reading side of the ids has to agree with the writing side, or the
    /// presets point at nothing. This pins the shape of the location strings
    /// rather than their hashes, which are machine-specific by construction.
    #[test]
    fn minted_ids_match_what_discovery_would_produce_for_the_same_paths() {
        let home = tempfile::tempdir().unwrap();
        let config = home.path().join(".mework");
        let skill_ids = seed_skills(&config).unwrap();
        let mcp_ids = seed_mcp_servers(&config).unwrap();
        for (index, (directory, _)) in BUILTIN_SKILLS.iter().enumerate() {
            let location = config.join("skills").join(directory).join("SKILL.md");
            assert_eq!(
                skill_ids[index],
                stable_id("skill_user", directory, &location.to_string_lossy())
            );
        }
        for (index, (name, _, _, _)) in BUILTIN_MCP_SERVERS.iter().enumerate() {
            let location = format!(
                "{}#/mcpServers/{name}",
                config.join("mcp.json").to_string_lossy()
            );
            assert_eq!(mcp_ids[index], stable_id("mcp_user", name, &location));
        }
    }

    /// The user's own entries are what the merge exists to protect.
    #[test]
    fn merging_keeps_entries_the_user_already_wrote() {
        let home = tempfile::tempdir().unwrap();
        let config = home.path().join(".mework");
        std::fs::create_dir_all(&config).unwrap();
        std::fs::write(
            config.join("mcp.json"),
            r#"{"mcpServers":{"mine":{"command":"my-server"}}}"#,
        )
        .unwrap();
        std::fs::write(
            config.join("hooks.json"),
            r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","name":"mine","command":"true"}]}]}}"#,
        )
        .unwrap();
        seed_mcp_servers(&config).unwrap();
        seed_hooks(&config).unwrap();

        let servers: Value =
            serde_json::from_str(&std::fs::read_to_string(config.join("mcp.json")).unwrap())
                .unwrap();
        assert_eq!(
            servers["mcpServers"]["mine"]["command"], "my-server",
            "用户自己的服务器必须原样保留"
        );
        assert!(servers["mcpServers"]["builtin_context7"].is_object());

        let hooks: Value =
            serde_json::from_str(&std::fs::read_to_string(config.join("hooks.json")).unwrap())
                .unwrap();
        assert_eq!(hooks["hooks"]["Stop"][0]["hooks"][0]["name"], "mine");
        assert!(hooks["hooks"]["SessionStart"].is_array());
    }

    /// Deleting a built-in is a decision, and the next launch must not argue
    /// with it.
    #[test]
    fn a_deleted_builtin_is_not_resurrected() {
        let app_data = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let config = home.path().join(".mework");
        seed_into(app_data.path(), &config);
        let manifest = config
            .join("skills")
            .join(BUILTIN_SKILLS[0].0)
            .join("SKILL.md");
        assert!(manifest.exists(), "第一次播种应当写出内置技能");
        std::fs::remove_file(&manifest).unwrap();
        seed_into(app_data.path(), &config);
        assert!(!manifest.exists(), "标记还在时不得重写已删除的内置技能");
    }

    /// The marker is what makes deletion stick, so wiping the data directory —
    /// a factory reset — has to bring the built-ins back.
    #[test]
    fn a_wiped_data_directory_seeds_again() {
        let home = tempfile::tempdir().unwrap();
        let config = home.path().join(".mework");
        let first = tempfile::tempdir().unwrap();
        seed_into(first.path(), &config);
        let manifest = config
            .join("skills")
            .join(BUILTIN_SKILLS[0].0)
            .join("SKILL.md");
        std::fs::remove_file(&manifest).unwrap();
        let second = tempfile::tempdir().unwrap();
        seed_into(second.path(), &config);
        assert!(manifest.exists(), "标记没了就应当重新播种");
    }
}
