//! Which inherited environment variables must never reach a user's shell.
//!
//! Both spawn paths — `std::process::Command` for the shell tool and hooks, and
//! `portable_pty::CommandBuilder` for the interactive terminal — start from this
//! process's own environment. Under `npm run dev:browser` that environment
//! carries the browser-dev bridge address and its bearer token, and an E2E run
//! adds report URLs and their tokens. The backend genuinely needs those values;
//! a command the user or the model runs does not, and anything that dumps its
//! environment would print them.
//!
//! Scrubbing belongs at the spawn boundary rather than at the launcher, which
//! still has to hand the values to the backend, and rather than inside the
//! PowerShell bootstrap, which only runs after `CreateProcess` has already
//! published the whole block to the child.

use std::ffi::OsStr;
use std::ffi::OsString;

/// The variable a development launcher uses to hand this process the `PATH` it
/// should actually run with.
///
/// `scripts/windows-native-build-tools.mjs` has to put `<msys2>\mingw64\bin` and
/// `<msys2>\usr\bin` *ahead* of `System32` so `cargo` finds the native `gcc`. The
/// application inherits that order and gives it to every shell it opens, where a
/// bare `cmd` then resolves to `<msys2>\usr\bin\cmd` — a bash script — instead of
/// `System32\cmd.exe`; `usr\bin` shadows `find`, `sort`, `more`, `link` and `tar`
/// the same way. Splitting the two `PATH`s has to happen here rather than in the
/// launcher, because `cargo run` compiles and executes under one environment.
///
/// The value is the application `PATH` with the toolchain directories appended
/// rather than removed, so a GNU-target binary can still find its runtime DLLs.
pub(crate) const DEV_APPLICATION_PATH_ENVIRONMENT_NAME: &str = "MEWORK_DEV_APPLICATION_PATH";

/// What `restore_dev_application_path` should write and delete, decided without
/// touching the process environment so it can be tested directly.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct DevApplicationPathHandoff {
    /// The `PATH` spellings to overwrite. Empty means leave `PATH` alone.
    pub names: Vec<OsString>,
    pub value: OsString,
    /// Every spelling of the handoff marker, which must be deleted whether or not
    /// a `PATH` is written, so it cannot reach a user's shell.
    pub markers: Vec<OsString>,
}

/// Reads the handoff out of an environment listing. Returns `None` when no
/// launcher marker is present, which is the packaged and user-shell case.
pub(crate) fn dev_application_path_handoff<I>(variables: I) -> Option<DevApplicationPathHandoff>
where
    I: IntoIterator<Item = (OsString, OsString)>,
{
    let mut names = Vec::new();
    let mut markers = Vec::new();
    let mut value: Option<OsString> = None;
    for (name, variable) in variables {
        // Windows environment names are case-insensitive, so the marker and the
        // `PATH` key have to be matched that way or a different spelling escapes.
        let Some(text) = name.to_str() else { continue };
        let upper = text.to_ascii_uppercase();
        if upper == DEV_APPLICATION_PATH_ENVIRONMENT_NAME {
            markers.push(name);
            value = Some(variable);
        } else if upper == "PATH" {
            names.push(name);
        }
    }
    let value = value?;
    // An empty handoff would blank `PATH` for the whole process. Consume the
    // marker anyway; refusing the write is the safe half of the decision.
    if value.is_empty() {
        return Some(DevApplicationPathHandoff {
            names: Vec::new(),
            value,
            markers,
        });
    }
    if names.is_empty() {
        names.push(OsString::from("PATH"));
    }
    Some(DevApplicationPathHandoff {
        names,
        value,
        markers,
    })
}

/// Applies the launcher's handoff to this process. Must be called from the entry
/// point before any thread or child process exists, because mutating the
/// environment is only sound while the process is single-threaded.
pub(crate) fn restore_dev_application_path() {
    let Some(handoff) = dev_application_path_handoff(std::env::vars_os()) else {
        return;
    };
    // SAFETY: called as the first statement of `run`/`run_browser_dev`, before the
    // Tauri builder, the runtime, or any spawn.
    unsafe {
        for name in &handoff.names {
            std::env::set_var(name, &handoff.value);
        }
        for name in &handoff.markers {
            std::env::remove_var(name);
        }
    }
}

/// True for a variable that exists to wire up development and end-to-end
/// harnesses, and that a user command has no reason to inherit.
///
/// The rule is deliberately a rule and not a list: every one of these families
/// grows a new member whenever a harness gains an option, and a list silently
/// stops covering them. It stays narrow by requiring the project's own prefix
/// *and* a harness marker, so `MEWORK_TERMINAL_*` (which the terminal injects
/// immediately after this scrub), `MEWORK_HOOK_EVENT`, `PATH`, the user's API
/// keys, and ordinary `VITE_*` project settings are all kept.
pub(crate) fn is_private_child_environment_name(name: &OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    // Windows environment names are case-insensitive, so a child could otherwise
    // reintroduce the value under a different spelling.
    let name = name.to_ascii_uppercase();
    let owned_by_project = name.starts_with("MEWORK_") || name.starts_with("VITE_");
    owned_by_project && (name.contains("BROWSER_DEV") || name.contains("E2E"))
}

/// Every name in this process's environment that `is_private_child_environment_name`
/// rejects, in the spelling the environment actually uses — which is what a
/// removal has to be keyed on.
pub(crate) fn private_child_environment_names() -> Vec<std::ffi::OsString> {
    std::env::vars_os()
        .map(|(name, _)| name)
        .filter(|name| is_private_child_environment_name(name))
        .collect()
}

/// Normalize only proxy bypass variables. Configuration overrides inheritance, including
/// an explicit empty value. POSIX prefers `no_proxy`; Windows prefers `NO_PROXY`.
/// Other spellings are a deterministic fallback, never dependent on insertion order.
/// This normalizes list syntax, not client-specific matching semantics.
pub(crate) fn normalized_proxy_bypass(
    inherited: &std::collections::BTreeMap<String, String>,
    configured: &std::collections::BTreeMap<String, String>,
    windows: bool,
) -> Result<std::collections::BTreeMap<String, String>, String> {
    let select = |values: &std::collections::BTreeMap<String, String>| {
        let preferred = if windows { "NO_PROXY" } else { "no_proxy" };
        values.get(preferred).or_else(|| values.iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("NO_PROXY"))
            .map(|(_, value)| value)).cloned()
    };
    let Some(value) = select(configured).or_else(|| select(inherited)) else {
        return Ok(Default::default());
    };
    let mut entries = Vec::new();
    for entry in value.split(|c: char| c == ',' || c == ';' || c.is_ascii_whitespace()) {
        if entry.is_empty() { continue; }
        if entry.contains("://") || entry.contains(['?', '#', '@'])
            || (entry.contains('*') && entry != "*") || entry.chars().any(char::is_control) {
            return Err("Invalid NO_PROXY entry: use host, IP, CIDR or * entries, not URLs or host globs".into());
        }
        if !entries.contains(&entry) { entries.push(entry); }
    }
    let value = entries.join(",");
    let mut output = std::collections::BTreeMap::new();
    output.insert("NO_PROXY".into(), value.clone());
    if !windows { output.insert("no_proxy".into(), value); }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    fn private(name: &str) -> bool {
        is_private_child_environment_name(OsStr::new(name))
    }

    #[test]
    fn proxy_bypass_normalization_preserves_scope_and_explicit_precedence() {
        let map = |pairs: &[(&str, &str)]| pairs.iter()
            .map(|(key, value)| (key.to_string(), value.to_string())).collect();
        for windows in [false, true] {
            for name in ["NO_PROXY", "no_proxy", "No_Proxy"] {
                let output = normalized_proxy_bypass(&map(&[(name, "a;b a, c")]), &map(&[]), windows).unwrap();
                assert_eq!(output["NO_PROXY"], "a,b,c");
                assert_eq!(output.len(), if windows { 1 } else { 2 });
                if !windows { assert_eq!(output["no_proxy"], "a,b,c"); }
                let empty = normalized_proxy_bypass(&map(&[(name, "inherited")]),
                    &map(&[("no_proxy", "")]), windows).unwrap();
                assert_eq!(empty["NO_PROXY"], "");
            }
            let both = map(&[("NO_PROXY", "upper"), ("no_proxy", "lower")]);
            let output = normalized_proxy_bypass(&map(&[]), &both, windows).unwrap();
            assert_eq!(output["NO_PROXY"], if windows { "upper" } else { "lower" });
            let output = normalized_proxy_bypass(&both, &map(&[("No_Proxy", "configured")]), windows).unwrap();
            assert_eq!(output["NO_PROXY"], "configured");
        }
        for value in ["*", "10.0.0.0/8", "::1", "[::1]:8080", "example.com:443", ".example.com"] {
            let result = normalized_proxy_bypass(&map(&[]), &map(&[("NO_PROXY", value)]), false).unwrap();
            assert_eq!(result["no_proxy"], value);
        }
        assert!(normalized_proxy_bypass(&map(&[]), &map(&[]), false).unwrap().is_empty());
        for value in ["http://localhost", "*.example.com", "user@host", "host?query"] {
            assert!(normalized_proxy_bypass(&map(&[]), &map(&[("NO_PROXY", value)]), false).is_err());
        }
    }

    #[test]
    fn every_harness_variable_is_private_and_nothing_else_is() {
        for name in [
            // The bridge address and the bearer token that authenticates to it.
            "MEWORK_BROWSER_DEV_TOKEN",
            "MEWORK_BROWSER_DEV_SUPPLIED_TOKEN",
            "MEWORK_BROWSER_DEV_ADDRESS",
            "MEWORK_BROWSER_DEV_ORIGIN",
            "MEWORK_BROWSER_DEV_INSTANCE_ID",
            "MEWORK_BROWSER_DEV_DATA_IDENTIFIER",
            "VITE_BROWSER_DEV_TOKEN",
            "VITE_BROWSER_DEV_BACKEND_URL",
            // E2E harness wiring, including its own report tokens.
            "MEWORK_MEMORY_E2E_RUN_ID",
            "MEWORK_IMAGE_INPUT_E2E_REPORT_PORT",
            "MEWORK_WEB_SEARCH_E2E",
            "VITE_IMAGE_INPUT_E2E_REPORT_TOKEN",
            "VITE_WEB_SEARCH_E2E_REPORT_TOKEN",
            "VITE_MEMORY_E2E_ENABLED",
            // Case-insensitive: Windows would treat these as the same variable.
            "mework_browser_dev_token",
            "Vite_Browser_Dev_Token",
        ] {
            assert!(private(name), "{name} must not reach a user command");
        }

        for name in [
            // Injected immediately after the scrub; removing it would break the
            // control handshake the terminal depends on.
            "MEWORK_TERMINAL_CONTROL_NONCE",
            "MEWORK_TERMINAL_ACK_EVENT",
            "MEWORK_TERMINAL_REJECT_EVENT",
            // Product variables a command is entitled to see.
            "MEWORK_HOOK_EVENT",
            "MEWORK_DIR",
            "MEWORK_KERNEL_SHADOW",
            "VITE_POLICY_EXPECTATION",
            // Nothing outside the project's own namespace is ever touched.
            "PATH",
            "Path",
            "USERPROFILE",
            "TERM",
            "OPENAI_API_KEY",
            "E2E_SOMETHING_ELSE",
            "BROWSER_DEV_TOKEN",
        ] {
            assert!(!private(name), "{name} must still be inherited");
        }
    }

    #[test]
    fn names_are_collected_in_the_spelling_the_environment_uses() {
        // A removal keyed on the canonical upper-case spelling would miss the
        // entry on a case-sensitive platform.
        let name = OsString::from("Mework_Browser_Dev_Probe_Name");
        // SAFETY: single-threaded assertion over a name no other test uses.
        unsafe { std::env::set_var(&name, "value") };
        let collected = private_child_environment_names();
        unsafe { std::env::remove_var(&name) };
        assert!(collected.contains(&name), "collected: {collected:?}");
    }

    fn variables(pairs: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
        pairs
            .iter()
            .map(|(name, value)| (OsString::from(*name), OsString::from(*value)))
            .collect()
    }

    const BUILD_PATH: &str =
        "C:\\msys64\\mingw64\\bin;C:\\msys64\\usr\\bin;C:\\WINDOWS\\system32";
    const APPLICATION_PATH: &str =
        "C:\\WINDOWS\\system32;C:\\msys64\\mingw64\\bin;C:\\msys64\\usr\\bin";

    #[test]
    fn the_launcher_handoff_restores_the_application_path_and_is_consumed() {
        let handoff = dev_application_path_handoff(variables(&[
            ("Path", BUILD_PATH),
            (DEV_APPLICATION_PATH_ENVIRONMENT_NAME, APPLICATION_PATH),
            ("MEWORK_DIR", "C:\\app"),
        ]))
        .expect("a marked environment must produce a handoff");
        assert_eq!(handoff.names, vec![OsString::from("Path")]);
        assert_eq!(handoff.value, OsString::from(APPLICATION_PATH));
        assert_eq!(
            handoff.markers,
            vec![OsString::from(DEV_APPLICATION_PATH_ENVIRONMENT_NAME)]
        );

        // The marker is matched case-insensitively, and the write lands on the
        // spelling the environment actually uses.
        let handoff = dev_application_path_handoff(variables(&[
            ("PATH", BUILD_PATH),
            ("Mework_Dev_Application_Path", APPLICATION_PATH),
        ]))
        .expect("a lower-case marker is the same variable on Windows");
        assert_eq!(handoff.names, vec![OsString::from("PATH")]);
        assert_eq!(
            handoff.markers,
            vec![OsString::from("Mework_Dev_Application_Path")]
        );

        // No PATH of its own: the restore still has to publish one.
        let handoff = dev_application_path_handoff(variables(&[(
            DEV_APPLICATION_PATH_ENVIRONMENT_NAME,
            APPLICATION_PATH,
        )]))
        .expect("the handoff does not depend on an inherited PATH");
        assert_eq!(handoff.names, vec![OsString::from("PATH")]);
    }

    #[test]
    fn an_unmarked_or_empty_handoff_never_rewrites_path() {
        // The packaged application and every user shell: nothing to restore, and
        // the inherited PATH must be left exactly as it is.
        assert_eq!(
            dev_application_path_handoff(variables(&[
                ("Path", APPLICATION_PATH),
                ("MEWORK_DIR", "C:\\app"),
            ])),
            None
        );
        assert_eq!(dev_application_path_handoff(variables(&[])), None);

        // An empty marker would blank PATH for the whole process; consume it, but
        // never write it.
        let handoff = dev_application_path_handoff(variables(&[
            ("Path", BUILD_PATH),
            (DEV_APPLICATION_PATH_ENVIRONMENT_NAME, ""),
        ]))
        .expect("an empty marker still has to be deleted");
        assert!(handoff.names.is_empty());
        assert_eq!(
            handoff.markers,
            vec![OsString::from(DEV_APPLICATION_PATH_ENVIRONMENT_NAME)]
        );
    }

    #[test]
    fn the_restored_path_puts_system32_ahead_of_the_msys_shadows() {
        // The whole point of the split: `cmd`, `find`, `sort`, `link` and `tar`
        // exist under `<msys2>\usr\bin` too, so whichever directory comes first
        // decides what a shell the application opens actually runs. The handoff
        // therefore has to win over the inherited build `PATH`, not merge with it.
        let handoff = dev_application_path_handoff(variables(&[
            ("Path", BUILD_PATH),
            (DEV_APPLICATION_PATH_ENVIRONMENT_NAME, APPLICATION_PATH),
        ]))
        .expect("a marked environment must produce a handoff");
        let restored = handoff.value.to_str().expect("the handoff is UTF-8");
        assert_ne!(restored, BUILD_PATH, "恢复必须换掉构建 PATH 而不是沿用它");
        let entries: Vec<&str> = restored.split(';').collect();
        let system32 = entries
            .iter()
            .position(|entry| entry.eq_ignore_ascii_case("C:\\WINDOWS\\system32"))
            .expect("System32 must survive the restore");
        let usr_bin = entries
            .iter()
            .position(|entry| entry.eq_ignore_ascii_case("C:\\msys64\\usr\\bin"))
            .expect("the toolchain is appended, not dropped, so DLLs stay findable");
        assert!(system32 < usr_bin, "{restored}");

        let build: Vec<&str> = BUILD_PATH.split(';').collect();
        assert!(
            build.iter().position(|entry| *entry == "C:\\msys64\\mingw64\\bin")
                < build
                    .iter()
                    .position(|entry| entry.eq_ignore_ascii_case("C:\\WINDOWS\\system32")),
            "构建 PATH 必须继续把 mingw64 排在前面，否则 gcc 会解析错"
        );
    }
}
