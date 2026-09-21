//! The `lsp` tool on a workspace that lives on another machine.
//!
//! The host leg in [`crate::tool_executor`] guards the path on this disk and
//! starts the language server as a child of this process. Neither is possible
//! for a workspace on a WSL distribution or an SSH machine: the file is not
//! here, and a server started here could not open it. So this leg does what
//! Claude Code does when it runs on a remote — it puts the language server
//! *where the code is*. The server is started through that machine's own
//! shell transport ([`crate::lsp_servers::ServerHost::Remote`]) and its
//! configuration is read there too: the workspace's `.mework/lsp.json`, the
//! remote user's, and whichever built-in presets are installed on that
//! machine's PATH.
//!
//! One probe round trip answers everything the call needs before the server is
//! consulted — the canonical root and file (confinement is tested on the
//! canonical path, exactly as the remote file tools do), the two configuration
//! files, the installed presets, the remote home, and the file's text. The
//! server then answers over its own pipes, and a `git check-ignore` for the
//! results runs on that machine when there is anything to filter.

use std::{
    collections::BTreeSet,
    path::Path,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

use crate::{
    lsp::{self, LspCall, LspFiles, MAX_FILE_BYTES},
    lsp_config::{self, LspServerConfig},
    lsp_servers::{LspRegistry, ServerHost},
    model::{JsonObject, ResolvedLanguage, ResourceSource},
    remote_files::{self, Confinement, ExitWording, RemoteShell, RemoteWorkspace, TargetMode},
    run_environment::{self, ShellRunner},
    tool_executor::{optional_string, optional_u64_value, required_string, MAX_PATH_CHARS},
};

/// One probe reads the file and both configuration files; a language server
/// that needs longer than this to be told about a file is not reachable.
const PROBE_TIMEOUT: Duration = Duration::from_secs(60);

/// Largest `lsp.json` pulled over. A configuration file is a few kilobytes;
/// past this it is not one.
const MAX_CONFIG_BYTES: u64 = 1024 * 1024;

/// How long the classifier's existence check may take. It runs before the
/// approval card is decided, on the run loop, so it is bounded tighter than a
/// tool call.
const DECLARES_TIMEOUT: Duration = Duration::from_secs(15);

/// How long one existence answer is reused. One tool call is classified two or
/// three times on its way to execution — for the authorization decision, the
/// card and the execution scope — and each of those would otherwise be a round
/// trip to the machine.
const DECLARES_CACHE_TTL: Duration = Duration::from_secs(10);

/// What the probe brings back.
#[derive(Debug)]
pub(crate) struct Probe {
    /// Canonical workspace root on the machine.
    pub root: String,
    /// Canonical path of the file the call named.
    pub canonical: String,
    /// The remote user's home, which is where the user-level `lsp.json` lives
    /// and what `${HOME}` in an entry expands to there.
    pub home: String,
    /// The workspace's `lsp.json` on the machine — its path there (whichever
    /// spelling was found) and its bytes — when it exists.
    pub project_config: Option<(String, Vec<u8>)>,
    /// The remote user's `lsp.json`, likewise.
    pub user_config: Option<(String, Vec<u8>)>,
    /// Built-in preset commands `command -v` found on that machine's PATH.
    pub installed: BTreeSet<String>,
    /// The file's text.
    pub text: String,
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Runs one `lsp` call in a workspace on another machine.
pub(crate) fn run(
    target: &RemoteWorkspace<'_>,
    registry: &LspRegistry,
    input: &JsonObject,
    language: ResolvedLanguage,
    conversation_id: &str,
) -> Result<String, String> {
    run_with(&target.workspace.runner, target, registry, input, language, conversation_id)
}

pub(crate) fn run_with(
    shell: &dyn RemoteShell,
    target: &RemoteWorkspace<'_>,
    registry: &LspRegistry,
    input: &JsonObject,
    language: ResolvedLanguage,
    conversation_id: &str,
) -> Result<String, String> {
    let call = parse_input(input)?;
    let probe = probe_file(shell, target, &call.requested_path)?;
    // The project's own configuration names the command to launch, which is
    // why the classifier raises the unbounded card when it sees the file. For
    // a remote `lsp` call an unrestricted scope means exactly that card (or
    // full access) — the classifier folds an out-of-workspace path into the
    // same card — so a call still confined to the workspace was told the file
    // is not there and must not act on it now that it is. The classifier's
    // cached answer is dropped so the next call asks the machine again.
    if target.confinement == Confinement::Workspace && probe.project_config.is_some() {
        forget_declares(&target.workspace.runner, &target.workspace.root);
        return Err(format!(
            "Workspace {} on {} ships its own language-server configuration ({}), which names the language server to start there. Starting a project-configured language server needs approval, and this call was not approved for it; call the tool again so the approval can be asked for.",
            target.workspace.index,
            remote_files::machine_name(target),
            probe
                .project_config
                .as_ref()
                .map(|(path, _)| path.as_str())
                .unwrap_or_default()
        ));
    }
    let configs = configs_for(&probe, &target.workspace.runner, language);
    let host = ServerHost::Remote {
        runner: target.workspace.runner.clone(),
        machine_key: target.machine_key.clone(),
        machine_label: target.workspace.machine_label.clone(),
    };
    let files = RemoteFiles {
        shell,
        target,
        text: &probe.text,
    };
    lsp::execute(
        registry,
        &host,
        &configs,
        Path::new(&probe.root),
        Path::new(&probe.canonical),
        &call,
        conversation_id,
        &files,
    )
}

/// The tool arguments, validated the way the host leg validates them.
fn parse_input(input: &JsonObject) -> Result<LspCall, String> {
    let operation = required_string(input, "operation", 64, false)?;
    let requested = required_string(input, "filePath", MAX_PATH_CHARS, false)?;
    let line = optional_u64_value(input, "line")?
        .ok_or_else(|| "line is required; it is 1-based, as shown in editors".to_owned())?;
    let character = optional_u64_value(input, "character")?
        .ok_or_else(|| "character is required; it is 1-based, as shown in editors".to_owned())?;
    let query = match input.get("query") {
        None | Some(serde_json::Value::Null) => None,
        Some(_) => Some(optional_string(input, "query", "", 1024, true)?),
    };
    lsp::parse_call(&operation, requested, line, character, query)
}

// ---------------------------------------------------------------------------
// The probe
// ---------------------------------------------------------------------------

fn lsp_config_relative_paths() -> [String; 2] {
    crate::capabilities::relative_config_paths(crate::capabilities::CapabilityKind::Lsp)
}

/// The probe script: the remote file tools' prologue (enter the root, resolve
/// and canonicalize the path, test confinement, announce root and target),
/// then everything else the call needs, in one answer.
///
/// After the two header lines: the remote home; the project and user
/// configuration files, each as the path that was found, a byte count line and
/// exactly that many bytes — or a lone `-` when neither spelling is there; the
/// installed preset commands one per line, ended by an empty line; and the
/// file, which is whatever remains. Counts rather than delimiters for the
/// configuration files, because they may hold any bytes at all; no count for
/// the file, because it is last, and a count taken before the `cat` would only
/// turn a file rewritten in between into a spurious error.
fn probe_script(target: &RemoteWorkspace<'_>, path: &str) -> Result<String, String> {
    let mut script = remote_files::prologue(target, path, TargetMode::Existing)?;
    script.push_str(&format!(
        "[ -f \"$C\" ] || exit {}\n",
        remote_files::EXIT_WRONG_KIND
    ));
    // `wc -c`, the helper's last resort, pads its answer on some systems;
    // `digits` would read the padded number as 0 and wave a huge file through.
    script.push_str("S=$(digits \"$(fsize \"$C\" | tr -d ' ')\")\n");
    script.push_str(&format!(
        "[ \"$S\" -le {MAX_FILE_BYTES} ] || exit {}\n",
        remote_files::EXIT_TOO_LARGE
    ));
    script.push_str("printf '%s\\n' \"$HOME\"\n");
    // The preferred spelling wins when both exist, as `config_path_for` has
    // it on this host.
    let [preferred, legacy] = lsp_config_relative_paths();
    script.push_str(&format!(
        "emit() {{ if [ -f \"$1/{preferred}\" ]; then f=\"$1/{preferred}\"; elif [ -f \"$1/{legacy}\" ]; then f=\"$1/{legacy}\"; else printf '%s\\n' -; return 0; fi; n=$(digits \"$(fsize \"$f\" | tr -d ' ')\"); if [ \"$n\" -le {MAX_CONFIG_BYTES} ]; then printf '%s\\n' \"$f\"; printf '%s\\n' \"$n\"; cat -- \"$f\"; else printf '%s\\n' -; fi; }}\n"
    ));
    script.push_str("emit \"$ROOT\"\n");
    script.push_str("emit \"$HOME\"\n");
    script.push_str("for c in");
    for command in lsp_config::preset_commands() {
        script.push(' ');
        script.push_str(&run_environment::sh_single_quote(command));
    }
    script.push_str("; do command -v \"$c\" >/dev/null 2>&1 && printf '%s\\n' \"$c\"; done\n");
    script.push_str("printf '\\n'\n");
    script.push_str("cat -- \"$C\"\n");
    Ok(script)
}

fn probe_file(
    shell: &dyn RemoteShell,
    target: &RemoteWorkspace<'_>,
    path: &str,
) -> Result<Probe, String> {
    let script = probe_script(target, path)?;
    let wording = ExitWording::new(path)
        .wrong_kind(format!("Cannot access file: {path}. It is not a regular file"))
        .too_large("File too large for LSP analysis (exceeds 10MB limit)".to_owned());
    let output = remote_files::run_script(shell, target, &script, None, PROBE_TIMEOUT, &wording)?;
    parse_probe(&output.stdout)
}

/// Reads the probe's answer back, byte-wise, in the order the script wrote it.
fn parse_probe(bytes: &[u8]) -> Result<Probe, String> {
    let (header, rest) = remote_files::take_header(bytes)?;
    let (lines, rest) = remote_files::take_lines(rest, 1)?;
    let home = lines.into_iter().next().unwrap_or_default();
    if home.is_empty() {
        return Err("The remote machine did not report the user's home directory".into());
    }
    let (project_config, rest) = take_block(rest)?;
    let (user_config, rest) = take_block(rest)?;
    let mut installed = BTreeSet::new();
    let mut rest = rest;
    loop {
        let (lines, after) = remote_files::take_lines(rest, 1)?;
        rest = after;
        let line = lines.into_iter().next().unwrap_or_default();
        if line.is_empty() {
            break;
        }
        installed.insert(line);
    }
    let text = String::from_utf8(rest.to_vec())
        .map_err(|error| format!("Cannot read file as UTF-8 text: {error}"))?;
    Ok(Probe {
        root: header.root,
        canonical: header.canonical,
        home,
        project_config,
        user_config,
        installed,
        text,
    })
}

/// One counted block: the file's path, a byte-count line and that many bytes —
/// or `-` alone for an absent file.
fn take_block(bytes: &[u8]) -> Result<(Option<(String, Vec<u8>)>, &[u8]), String> {
    let (lines, rest) = remote_files::take_lines(bytes, 1)?;
    let path = lines.into_iter().next().unwrap_or_default();
    if path.trim() == "-" {
        return Ok((None, rest));
    }
    if path.is_empty() {
        return Err("The remote machine returned a malformed configuration block".into());
    }
    let (lines, rest) = remote_files::take_lines(rest, 1)?;
    let count: usize = lines
        .into_iter()
        .next()
        .unwrap_or_default()
        .trim()
        .parse()
        .map_err(|_| "The remote machine returned a malformed configuration block".to_owned())?;
    if rest.len() < count {
        return Err("The remote machine returned an incomplete result".into());
    }
    Ok((Some((path, rest[..count].to_vec())), &rest[count..]))
}

// ---------------------------------------------------------------------------
// Configuration on the other machine
// ---------------------------------------------------------------------------

/// The servers this call may route on, in the host leg's order: the
/// workspace's file, the remote user's file, then the presets found on that
/// machine's PATH.
///
/// `${VAR}` in an entry expands against what is known of that machine: its
/// home, and the variable table the workspace's machine was configured with.
/// The remote user's wider environment is not consulted — the value would be
/// whatever a login shell there happens to export, which this host cannot see.
fn configs_for(
    probe: &Probe,
    runner: &ShellRunner,
    language: ResolvedLanguage,
) -> Vec<LspServerConfig> {
    let env = |name: &str| -> Option<String> {
        if name == "HOME" {
            return Some(probe.home.clone());
        }
        runner.env().get(name).cloned()
    };
    let mut levels: Vec<Vec<lsp_config::LspEntry>> = Vec::with_capacity(3);
    if let Some((path, bytes)) = &probe.project_config {
        levels.push(lsp_config::parse_contents(
            bytes,
            Path::new(path),
            ResourceSource::Workspace,
            None,
            &env,
        ));
    }
    if let Some((path, bytes)) = &probe.user_config {
        levels.push(lsp_config::parse_contents(
            bytes,
            Path::new(path),
            ResourceSource::User,
            None,
            &env,
        ));
    }
    levels.push(lsp_config::builtin_entries_with_resolver(language, &|command| {
        probe.installed.contains(command)
    }));
    lsp_config::merge_servers(levels)
}

// ---------------------------------------------------------------------------
// The file seam
// ---------------------------------------------------------------------------

/// The remote leg's [`LspFiles`]: the text arrived with the probe, and
/// `git check-ignore` runs on the machine.
struct RemoteFiles<'a> {
    shell: &'a dyn RemoteShell,
    target: &'a RemoteWorkspace<'a>,
    text: &'a str,
}

impl LspFiles for RemoteFiles<'_> {
    fn read_text(&self) -> Result<String, String> {
        Ok(self.text.to_owned())
    }

    fn check_ignore(&self, root: &Path, paths: &[String]) -> Option<String> {
        check_ignore_with(self.shell, self.target.cancel, root, paths)
    }
}

/// `git check-ignore` over `paths` in `root`, on the machine `shell` reaches.
/// Stdout when it reported matches; `None` otherwise, which keeps every result.
fn check_ignore_with(
    shell: &dyn RemoteShell,
    cancel: &crate::cancel::CancelSignal,
    root: &Path,
    paths: &[String],
) -> Option<String> {
    let root = root.to_string_lossy();
    if root.trim().is_empty() || root.chars().any(char::is_control) {
        return None;
    }
    let mut script = format!(
        "cd -- {} || exit 64\ngit check-ignore --",
        run_environment::quote_remote_path(root.trim())
    );
    for path in paths {
        if path.chars().any(char::is_control) {
            return None;
        }
        script.push(' ');
        script.push_str(&run_environment::sh_single_quote(path));
    }
    script.push('\n');
    let output = shell
        .run(&script, None, lsp::CHECK_IGNORE_TIMEOUT, cancel)
        .ok()?;
    if output.status != Some(0) {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

// ---------------------------------------------------------------------------
// The edit hook
// ---------------------------------------------------------------------------

/// The current text of `canonical` on the machine, for re-syncing a document a
/// live server there holds after the model wrote it. `None` when the file is
/// gone, too large, or the machine did not answer — a miss the next navigation
/// call cannot repair on its own, but silent, because an edit that succeeded
/// must not be reported as anything else.
pub(crate) fn read_text(runner: &ShellRunner, canonical: &str) -> Option<String> {
    read_text_with(runner, canonical, &crate::cancel::CancelSignal::default())
}

pub(crate) fn read_text_with(
    shell: &dyn RemoteShell,
    canonical: &str,
    cancel: &crate::cancel::CancelSignal,
) -> Option<String> {
    if canonical.trim().is_empty() || canonical.chars().any(char::is_control) {
        return None;
    }
    let script = format!(
        "f={}\n[ -f \"$f\" ] || exit 66\ns=$(wc -c < \"$f\" | tr -d ' ')\n[ \"$s\" -le {MAX_FILE_BYTES} ] || exit 68\ncat -- \"$f\"\n",
        run_environment::sh_single_quote(canonical)
    );
    let output = shell.run(&script, None, PROBE_TIMEOUT, cancel).ok()?;
    if output.status != Some(0) {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

// ---------------------------------------------------------------------------
// The classifier's question
// ---------------------------------------------------------------------------

/// Whether the workspace at `root` on the machine `runner` reaches ships its
/// own language-server configuration; `None` when the machine did not answer.
///
/// The host leg answers this with a `stat`; here it is a round trip, bounded
/// and cached briefly so the two or three classifications one call goes
/// through do not each pay it. What "did not answer" means is the caller's
/// decision — the classifier treats it as "yes", the safe default.
pub(crate) fn workspace_declares_language_servers(
    runner: &ShellRunner,
    root: &str,
) -> Option<bool> {
    declares_with(runner, root, &|script| {
        run_environment::run_remote_script(
            runner,
            script,
            None,
            DECLARES_TIMEOUT,
            &crate::cancel::CancelSignal::default(),
        )
        .ok()
        .and_then(|output| output.status)
    })
}

/// Drops the cached answer for one workspace, so the next classification asks
/// the machine again. Called when the executor found a project file the
/// classifier had been told was not there.
pub(crate) fn forget_declares(runner: &ShellRunner, root: &str) {
    let fingerprint = runner.fingerprint();
    let mut entries = declares_cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    entries.retain(|(machine, path, _, _)| !(*machine == fingerprint && path == root));
}

/// The script's exit status when it ran: `0` when a configuration file exists,
/// `1` when neither spelling does, anything else (or `None`) when the machine
/// could not say.
type DeclaresProbe<'a> = dyn Fn(&str) -> Option<i32> + 'a;

type DeclaresCache = Mutex<Vec<(String, String, Option<bool>, Instant)>>;

fn declares_cache() -> &'static DeclaresCache {
    static CACHE: OnceLock<DeclaresCache> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(Vec::new()))
}

fn declares_with(runner: &ShellRunner, root: &str, probe: &DeclaresProbe<'_>) -> Option<bool> {
    let cache = declares_cache();
    let fingerprint = runner.fingerprint();
    let now = Instant::now();
    {
        let mut entries = cache.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        entries.retain(|(_, _, _, taken)| now.duration_since(*taken) < DECLARES_CACHE_TTL);
        if let Some((_, _, answer, _)) = entries
            .iter()
            .find(|(machine, path, _, _)| *machine == fingerprint && path == root)
        {
            return *answer;
        }
    }
    let answer = match declares_script(root).ok().and_then(|script| probe(&script)) {
        Some(0) => Some(true),
        Some(1) => Some(false),
        _ => None,
    };
    // Only an answer is remembered. A machine that did not answer is asked
    // again next time; caching the silence would keep the card up for the
    // whole window after the link recovered.
    if answer.is_some() {
        let mut entries = cache.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        entries.push((fingerprint, root.to_owned(), answer, now));
    }
    answer
}

/// Exit 0 when either spelling of the file exists under the root, 1 when
/// neither does, 64 when the root cannot be entered.
fn declares_script(root: &str) -> Result<String, String> {
    if root.trim().is_empty() || root.chars().any(char::is_control) {
        return Err("The workspace root cannot be tested".into());
    }
    let [preferred, legacy] = lsp_config_relative_paths();
    Ok(format!(
        "cd -- {} || exit 64\nif [ -f {} ] || [ -f {} ]; then exit 0; else exit 1; fi\n",
        run_environment::quote_remote_path(root.trim()),
        run_environment::sh_single_quote(&preferred),
        run_environment::sh_single_quote(&legacy),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote_files::tests::{fixture, write_fixture_file, Harness};

    const PROJECT_CONFIG: &str =
        r#"{"lspServers":{"probe":{"command":"probe-ls","extensionToLanguage":{".rs":"rust"}}}}"#;

    #[test]
    fn the_probe_brings_back_root_file_configs_and_presets() {
        let Some(fixture) = fixture() else { return };
        write_fixture_file(&fixture, "src/main.rs", b"fn main() {}\n");
        write_fixture_file(&fixture, ".mework/lsp.json", PROJECT_CONFIG.as_bytes());
        let harness = Harness::new(&fixture);
        let target = harness.target(Confinement::Workspace);
        let probe = probe_file(&fixture.shell, &target, "src/main.rs").expect("the probe runs");
        assert_eq!(probe.text, "fn main() {}\n");
        assert_eq!(probe.root, fixture.posix_root);
        assert_eq!(probe.canonical, format!("{}/src/main.rs", fixture.posix_root));
        assert!(!probe.home.is_empty());
        let (path, project) = probe.project_config.as_ref().expect("the project file was read");
        assert_eq!(path, &format!("{}/.mework/lsp.json", fixture.posix_root));
        assert_eq!(std::str::from_utf8(project).unwrap(), PROJECT_CONFIG);
        // Only preset commands are ever reported, and only when installed.
        for command in &probe.installed {
            assert!(lsp_config::preset_commands().any(|preset| preset == command));
        }

        // The legacy spelling is found too, and an empty file is a file.
        std::fs::remove_file(Path::new(&fixture.workspace).join(".mework/lsp.json")).unwrap();
        write_fixture_file(&fixture, ".naiword/lsp.json", b"");
        let probe = probe_file(&fixture.shell, &target, "src/main.rs").expect("the probe runs");
        let (path, project) = probe.project_config.as_ref().expect("the legacy file was read");
        assert_eq!(path, &format!("{}/.naiword/lsp.json", fixture.posix_root));
        assert!(project.is_empty());
    }

    #[test]
    fn the_probe_refuses_directories_and_paths_outside_the_root() {
        let Some(fixture) = fixture() else { return };
        write_fixture_file(&fixture, "src/main.rs", b"fn main() {}\n");
        let harness = Harness::new(&fixture);
        let target = harness.target(Confinement::Workspace);
        let error = probe_file(&fixture.shell, &target, "src").unwrap_err();
        assert!(error.contains("not a regular file"), "{error}");
        write_fixture_file(&fixture, "../outside.rs", b"fn outside() {}\n");
        let error = probe_file(&fixture.shell, &target, "../outside.rs").unwrap_err();
        assert!(error.contains("outside workspace"), "{error}");
        let error = probe_file(&fixture.shell, &target, "src/missing.rs").unwrap_err();
        assert!(error.contains("No such file"), "{error}");
        // An unconfined call — full access, or an approved one — may reach it.
        let free = harness.target(Confinement::Machine);
        let probe = probe_file(&fixture.shell, &free, "../outside.rs").expect("reachable");
        assert_eq!(probe.text, "fn outside() {}\n");
    }

    #[test]
    fn a_project_file_under_a_confined_scope_is_refused_before_any_server_starts() {
        let Some(fixture) = fixture() else { return };
        write_fixture_file(&fixture, "src/main.rs", b"fn main() {}\n");
        write_fixture_file(&fixture, ".mework/lsp.json", PROJECT_CONFIG.as_bytes());
        let harness = Harness::new(&fixture);
        let target = harness.target(Confinement::Workspace);
        let registry = LspRegistry::default();
        // The classifier had been told (by a stale answer) that nothing is
        // declared here; the refusal drops that answer.
        let runner = &target.workspace.runner;
        let root = target.workspace.root.clone();
        assert_eq!(declares_with(runner, &root, &|_| Some(1)), Some(false));
        let input: JsonObject = serde_json::from_value(serde_json::json!({
            "operation": "hover",
            "filePath": "src/main.rs",
            "line": 1,
            "character": 4,
        }))
        .unwrap();
        let error = run_with(
            &fixture.shell,
            &target,
            &registry,
            &input,
            ResolvedLanguage::EnUs,
            "c1",
        )
        .unwrap_err();
        assert!(error.contains("needs approval"), "{error}");
        assert!(error.contains(".mework/lsp.json"), "{error}");
        assert!(registry.snapshot().is_empty(), "nothing was started");
        assert_eq!(
            declares_with(runner, &root, &|_| Some(0)),
            Some(true),
            "the next classification asks the machine again"
        );
    }

    #[test]
    fn the_probe_answer_is_parsed_byte_wise() {
        let payload =
            b"/root\n/root/a.rs\n/home/u\n/root/.mework/lsp.json\n5\n{\"a\"}-\nrust-analyzer\ngopls\n\nabc"
                .to_vec();
        let probe = parse_probe(&payload).unwrap();
        assert_eq!(probe.root, "/root");
        assert_eq!(probe.canonical, "/root/a.rs");
        assert_eq!(probe.home, "/home/u");
        let (path, bytes) = probe.project_config.as_ref().unwrap();
        assert_eq!(path, "/root/.mework/lsp.json");
        assert_eq!(bytes, b"{\"a\"}");
        assert!(probe.user_config.is_none());
        assert_eq!(
            probe.installed,
            ["rust-analyzer".to_owned(), "gopls".to_owned()].into_iter().collect()
        );
        assert_eq!(probe.text, "abc");

        // The file is whatever remains, so an empty file is simply nothing.
        let empty = b"/root\n/root/a.rs\n/home/u\n-\n-\n\n".to_vec();
        assert_eq!(parse_probe(&empty).unwrap().text, "");
        // A configuration block cut short is an incomplete answer.
        let short = b"/root\n/root/a.rs\n/home/u\n/root/.mework/lsp.json\n9\n{}-\n\nabc".to_vec();
        assert!(parse_probe(&short).unwrap_err().contains("incomplete"));
    }

    #[test]
    fn configuration_levels_merge_in_the_host_legs_order_with_remote_expansion() {
        let mut runner_env = std::collections::BTreeMap::new();
        runner_env.insert("TOOLS".to_owned(), "/opt/tools".to_owned());
        let runner = ShellRunner::Wsl {
            distro: "Ubuntu".into(),
            env: runner_env,
        };
        let probe = Probe {
            root: "/srv/app".into(),
            canonical: "/srv/app/main.rs".into(),
            home: "/home/dev".into(),
            project_config: Some((
                "/srv/app/.mework/lsp.json".into(),
                br#"{"lspServers":{"rust-analyzer":{"command":"${TOOLS}/ra","extensionToLanguage":{".rs":"rust"}}}}"#
                    .to_vec(),
            )),
            user_config: Some((
                "/home/dev/.naiword/lsp.json".into(),
                br#"{"lspServers":{"rust-analyzer":{"command":"${HOME}/.cargo/bin/rust-analyzer","extensionToLanguage":{".rs":"rust"}},"pyright":{"command":"pyright-langserver","args":["--stdio"],"extensionToLanguage":{".py":"python"}}}}"#
                    .to_vec(),
            )),
            installed: ["gopls".to_owned()].into_iter().collect(),
            text: String::new(),
        };
        let configs = configs_for(&probe, &runner, ResolvedLanguage::EnUs);
        let command = |name: &str| {
            configs
                .iter()
                .find(|config| config.name == name)
                .map(|config| config.command.clone())
        };
        assert_eq!(configs.len(), 3, "one rust-analyzer, pyright, gopls");
        assert_eq!(
            command("rust-analyzer").as_deref(),
            Some("/opt/tools/ra"),
            "the project file wins and expands the machine's table"
        );
        assert_eq!(command("pyright").as_deref(), Some("pyright-langserver"));
        assert_eq!(
            command("gopls").as_deref(),
            Some("gopls"),
            "only installed presets are offered"
        );
        assert!(command("clangd").is_none());

        let user_only = Probe {
            project_config: None,
            ..probe
        };
        let configs = configs_for(&user_only, &runner, ResolvedLanguage::EnUs);
        let rust_analyzer = configs
            .iter()
            .find(|config| config.name == "rust-analyzer")
            .expect("the user file's entry");
        assert_eq!(
            rust_analyzer.command, "/home/dev/.cargo/bin/rust-analyzer",
            "${{HOME}} is the remote home"
        );
    }

    #[test]
    fn the_declares_check_is_cached_per_machine_and_root() {
        let runner = ShellRunner::Ssh {
            host: "cache-test".into(),
            port: 0,
            identity_file: String::new(),
            env: Default::default(),
        };
        let calls = std::cell::Cell::new(0);
        let probe = |_: &str| {
            calls.set(calls.get() + 1);
            Some(0)
        };
        assert_eq!(declares_with(&runner, "/srv/cached", &probe), Some(true));
        assert_eq!(declares_with(&runner, "/srv/cached", &probe), Some(true));
        assert_eq!(calls.get(), 1, "the second classification reuses the answer");
        // Forgetting makes the next classification ask again.
        forget_declares(&runner, "/srv/cached");
        assert_eq!(declares_with(&runner, "/srv/cached", &probe), Some(true));
        assert_eq!(calls.get(), 2);

        // Silence is not an answer and is not remembered.
        let silent_calls = std::cell::Cell::new(0);
        let silent = |_: &str| {
            silent_calls.set(silent_calls.get() + 1);
            None::<i32>
        };
        assert_eq!(declares_with(&runner, "/srv/unreachable", &silent), None);
        assert_eq!(declares_with(&runner, "/srv/unreachable", &silent), None);
        assert_eq!(silent_calls.get(), 2, "a machine that did not answer is asked again");
        let missing_root = |_: &str| Some(64);
        assert_eq!(declares_with(&runner, "/srv/gone", &missing_root), None);

        let absent = |_: &str| Some(1);
        assert_eq!(declares_with(&runner, "/srv/plain", &absent), Some(false));
    }

    #[test]
    fn the_declares_check_reads_the_file_on_the_machine() {
        let Some(fixture) = fixture() else { return };
        let cancel = crate::cancel::CancelSignal::default();
        let probe = |script: &str| {
            fixture
                .shell
                .run(script, None, DECLARES_TIMEOUT, &cancel)
                .ok()
                .and_then(|output| output.status)
        };
        let runner = ShellRunner::Wsl {
            distro: "declares-test".into(),
            env: Default::default(),
        };
        let plain = format!("{}/plain", fixture.workspace);
        write_fixture_file(&fixture, "plain/README", b"");
        assert_eq!(declares_with(&runner, &plain, &probe), Some(false));
        write_fixture_file(&fixture, "x/.mework/lsp.json", b"{}");
        write_fixture_file(&fixture, "y/.naiword/lsp.json", b"{}");
        assert_eq!(
            declares_with(&runner, &format!("{}/x", fixture.workspace), &probe),
            Some(true)
        );
        assert_eq!(
            declares_with(&runner, &format!("{}/y", fixture.workspace), &probe),
            Some(true)
        );
        assert_eq!(
            declares_with(&runner, &format!("{}/nowhere", fixture.workspace), &probe),
            None,
            "a root that cannot be entered is not an answer"
        );
    }

    #[test]
    fn read_text_and_check_ignore_run_on_the_machine() {
        let Some(fixture) = fixture() else { return };
        write_fixture_file(&fixture, "src/lib.rs", b"pub fn f() {}\n");
        let cancel = crate::cancel::CancelSignal::default();
        let root = &fixture.posix_root;
        assert_eq!(
            read_text_with(&fixture.shell, &format!("{root}/src/lib.rs"), &cancel).as_deref(),
            Some("pub fn f() {}\n")
        );
        assert!(read_text_with(&fixture.shell, &format!("{root}/src/none.rs"), &cancel).is_none());
        // Not a repository: no filtering, every result kept.
        assert!(check_ignore_with(
            &fixture.shell,
            &cancel,
            Path::new(root),
            &[format!("{root}/src/lib.rs")]
        )
        .is_none());
    }
}
