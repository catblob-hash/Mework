//! The `# Environment` section of the system prompt.
//!
//! One block naming the facts a model cannot discover without spending a tool
//! call: where it is running, whether that directory is a Git checkout, which
//! other directories it may touch, and what host it is on. Claude Code publishes
//! the same block, and the wording follows it so a model trained against that
//! phrasing reads ours the same way.
//!
//! What is deliberately absent: the shell, the model's own name and id, and the
//! knowledge cutoff. Mework exposes `bash` and `powershell` as separate tools, so
//! naming a shell here would contradict the catalog; the model identity and the
//! cutoff are the provider's to know, and repeating a guess at them in the prompt
//! only creates a second, staler source of truth.
//!
//! Every line is a profile key, so a translated profile translates the block.
//! Paths arrive from the host's own records rather than from the model, but they
//! are still user-supplied text: [`sanitize_path`] strips the control and
//! bidirectional characters that could make a rendered line read as something
//! other than what it is.

use std::path::Path;
use std::sync::OnceLock;

use crate::prompt_profile::{PromptKey, PromptProfile};

/// The facts the section reports, already resolved.
///
/// Collected once per run by the trusted request builder. The renderer never
/// supplies any of it: `working_directory` and `additional_directories` come from
/// the host's own conversation record, and the rest is read from this machine.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EnvironmentFacts {
    /// The directory the conversation's tools resolve relative paths against.
    pub working_directory: String,
    /// Whether that directory is an isolated worktree this conversation owns.
    pub is_worktree: bool,
    /// Whether that directory is inside a Git checkout.
    pub is_git_repository: bool,
    /// Directories outside the workspace that the user granted this conversation.
    pub additional_directories: Vec<String>,
    /// Host operating system, as `std::env::consts::OS` names it.
    pub platform: String,
    /// Host operating system version, or empty when it could not be read.
    pub os_version: String,
    /// Today's date on this machine, `YYYY-MM-DD`.
    pub date: String,
}

impl EnvironmentFacts {
    /// Reads every fact for one run.
    ///
    /// `workspace` is the conversation's effective directory — the worktree when
    /// it has one, the workspace root otherwise — so `is_git_repository` answers
    /// for the directory the tools will actually run in.
    pub fn collect(workspace: &Path, is_worktree: bool, additional_directories: &[String]) -> Self {
        Self {
            working_directory: display_path(&workspace.to_string_lossy()),
            is_worktree,
            is_git_repository: is_git_repository(workspace),
            additional_directories: additional_directories
                .iter()
                .map(|directory| display_path(directory))
                .collect(),
            platform: std::env::consts::OS.to_owned(),
            os_version: os_version().to_owned(),
            date: chrono::Local::now().format("%Y-%m-%d").to_string(),
        }
    }
}

/// Drops the Windows extended-length prefix from a path before it is stated.
///
/// `fs::canonicalize` returns `\\?\C:\…`, and a recorded workspace path can carry
/// that form for the rest of its life. The host resolves either spelling, but the
/// model does not only read this path — it types it back into shell commands,
/// where `\\?\` is accepted by almost nothing. Stating the ordinary form is the
/// difference between a path the model can reuse and one it cannot.
fn display_path(path: &str) -> String {
    if let Some(unc) = path.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{unc}");
    }
    path.strip_prefix(r"\\?\").unwrap_or(path).to_owned()
}

/// Renders the section, or an empty string when the profile cleared its frame.
///
/// The frame carries the heading and the sentence introducing the list; a profile
/// that empties it means "do not announce an environment", so the facts are
/// dropped with it rather than emitted bare.
pub fn environment_section(profile: &PromptProfile, facts: &EnvironmentFacts) -> String {
    if profile.text(PromptKey::SystemEnvironmentSection).is_empty() {
        return String::new();
    }
    let mut lines = Vec::new();
    push_fact(
        &mut lines,
        profile.render(
            PromptKey::SystemEnvironmentWorkingDirectory,
            &[("path", &sanitize_path(&facts.working_directory))],
        ),
    );
    if facts.is_worktree {
        push_fact(
            &mut lines,
            profile
                .text(PromptKey::SystemEnvironmentWorktree)
                .to_owned(),
        );
        push_fact(
            &mut lines,
            profile
                .text(PromptKey::SystemEnvironmentWorktreeStash)
                .to_owned(),
        );
    }
    push_fact(
        &mut lines,
        profile.render(
            PromptKey::SystemEnvironmentGitRepository,
            &[(
                "value",
                if facts.is_git_repository {
                    "true"
                } else {
                    "false"
                },
            )],
        ),
    );
    if !facts.additional_directories.is_empty() {
        let heading = lines.len();
        push_fact(
            &mut lines,
            profile
                .text(PromptKey::SystemEnvironmentAdditionalDirectories)
                .to_owned(),
        );
        // A profile that emptied the heading removed the list with it: nested
        // items under no heading would read as belonging to the line above.
        if lines.len() > heading {
            for directory in &facts.additional_directories {
                let directory = sanitize_path(directory);
                if !directory.is_empty() {
                    lines.push(format!("  - {directory}"));
                }
            }
        }
    }
    push_fact(
        &mut lines,
        profile.render(
            PromptKey::SystemEnvironmentPlatform,
            &[("platform", &sanitize_path(&facts.platform))],
        ),
    );
    if !facts.os_version.is_empty() {
        push_fact(
            &mut lines,
            profile.render(
                PromptKey::SystemEnvironmentOsVersion,
                &[("version", &sanitize_path(&facts.os_version))],
            ),
        );
    }
    push_fact(
        &mut lines,
        profile.render(PromptKey::SystemEnvironmentDate, &[("date", &facts.date)]),
    );
    if lines.is_empty() {
        return String::new();
    }
    profile.render(
        PromptKey::SystemEnvironmentSection,
        &[("facts", &lines.join("\n"))],
    )
}

/// Appends one top-level fact as a list item, skipping a line the profile emptied.
fn push_fact(lines: &mut Vec<String>, text: String) {
    if text.trim().is_empty() {
        return;
    }
    lines.push(format!(" - {text}"));
}

/// Whether `path` or any ancestor holds a `.git` entry.
///
/// A linked worktree records `.git` as a file rather than a directory, so both
/// count. This deliberately does not shell out to `git`: the section is rendered
/// on every turn, and the answer only has to be as good as "there is a checkout
/// here", which the presence of the entry already settles.
fn is_git_repository(path: &Path) -> bool {
    path.ancestors()
        .any(|ancestor| ancestor.join(".git").exists())
}

/// Host operating-system version, read once per process.
fn os_version() -> &'static str {
    static VERSION: OnceLock<String> = OnceLock::new();
    VERSION.get_or_init(detect_os_version)
}

/// Reads the Windows version from the kernel rather than from `GetVersionEx`,
/// which reports 6.2 for an application without a compatibility manifest.
#[cfg(windows)]
fn detect_os_version() -> String {
    use windows_sys::Wdk::System::SystemServices::RtlGetVersion;
    use windows_sys::Win32::System::SystemInformation::OSVERSIONINFOW;

    let mut info: OSVERSIONINFOW = unsafe { std::mem::zeroed() };
    info.dwOSVersionInfoSize = std::mem::size_of::<OSVERSIONINFOW>() as u32;
    // SAFETY: `info` is a zeroed, correctly sized `OSVERSIONINFOW`, and
    // `RtlGetVersion` only writes into the structure it is handed.
    let status = unsafe { RtlGetVersion(&mut info) };
    if status != 0 {
        return String::new();
    }
    format!(
        "Windows {}.{}.{}",
        info.dwMajorVersion, info.dwMinorVersion, info.dwBuildNumber
    )
}

/// Reads the kernel name and release, which is what `uname -sr` prints.
#[cfg(unix)]
fn detect_os_version() -> String {
    // SAFETY: `info` is a zeroed `utsname` and `uname` only writes into it.
    let mut info: libc::utsname = unsafe { std::mem::zeroed() };
    if unsafe { libc::uname(&mut info) } != 0 {
        return String::new();
    }
    let system = c_string_field(&info.sysname);
    let release = c_string_field(&info.release);
    match (system.is_empty(), release.is_empty()) {
        (true, true) => String::new(),
        (true, false) => release,
        (false, true) => system,
        (false, false) => format!("{system} {release}"),
    }
}

#[cfg(not(any(windows, unix)))]
fn detect_os_version() -> String {
    String::new()
}

/// Reads one NUL-terminated `utsname` field.
#[cfg(unix)]
fn c_string_field(field: &[libc::c_char]) -> String {
    field
        .iter()
        .take_while(|byte| **byte != 0)
        .map(|byte| *byte as u8 as char)
        .collect()
}

/// Removes the characters that would let a path rewrite the line it appears on.
///
/// Controls could close the list item early and bidirectional or zero-width marks
/// could make the rendered path read as a different one. Both are dropped rather
/// than escaped: the section is a statement of fact, and a path that needs
/// escaping is more usefully shown as the printable characters it really has.
fn sanitize_path(value: &str) -> String {
    value
        .chars()
        .filter(|character| {
            !character.is_control()
                && !matches!(
                    *character,
                    '\u{061c}'
                        | '\u{200b}'
                        | '\u{200e}'
                        | '\u{200f}'
                        | '\u{202a}'..='\u{202e}'
                        | '\u{2066}'..='\u{2069}'
                        | '\u{feff}'
                        | '\u{e0000}'..='\u{e007f}'
                )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ResolvedLanguage;

    fn facts() -> EnvironmentFacts {
        EnvironmentFacts {
            working_directory: "C:/work/app".into(),
            is_worktree: false,
            is_git_repository: true,
            additional_directories: Vec::new(),
            platform: "windows".into(),
            os_version: "Windows 10.0.26100".into(),
            date: "2026-01-02".into(),
        }
    }

    #[test]
    fn section_lists_every_fact_as_one_item() {
        let profile = PromptProfile::builtin_english();
        assert_eq!(
            environment_section(&profile, &facts()),
            "# Environment\nYou have been invoked in the following environment:\n\
             \x20- Primary working directory: C:/work/app\n\
             \x20- Is a git repository: true\n\
             \x20- Platform: windows\n\
             \x20- OS Version: Windows 10.0.26100\n\
             \x20- Today's date: 2026-01-02"
        );
    }

    #[test]
    fn additional_directories_are_nested_under_their_own_heading() {
        let profile = PromptProfile::builtin_english();
        let section = environment_section(
            &profile,
            &EnvironmentFacts {
                additional_directories: vec!["D:/shared/lib".into(), "D:/docs".into()],
                ..facts()
            },
        );
        assert!(
            section.contains(
                " - Additional working directories:\n  - D:/shared/lib\n  - D:/docs\n - Platform:"
            ),
            "{section}"
        );
    }

    #[test]
    fn a_worktree_says_so_and_warns_about_the_shared_stash() {
        let profile = PromptProfile::builtin_english();
        let section = environment_section(
            &profile,
            &EnvironmentFacts {
                is_worktree: true,
                ..facts()
            },
        );
        assert!(section.contains(" - This is a git worktree"), "{section}");
        assert!(section.contains("git stash push -u -m"), "{section}");
        assert!(
            !environment_section(&profile, &facts()).contains("worktree"),
            "a workspace root must not claim to be a worktree"
        );
    }

    #[test]
    fn an_unreadable_os_version_drops_its_line_rather_than_stating_nothing() {
        let profile = PromptProfile::builtin_english();
        let section = environment_section(
            &profile,
            &EnvironmentFacts {
                os_version: String::new(),
                ..facts()
            },
        );
        assert!(!section.contains("OS Version"), "{section}");
        assert!(section.contains(" - Platform: windows"), "{section}");
    }

    #[test]
    fn bidirectional_and_control_characters_never_reach_the_prompt() {
        let profile = PromptProfile::builtin_english();
        let section = environment_section(
            &profile,
            &EnvironmentFacts {
                working_directory: "C:/work\u{202e}/app\u{0007}".into(),
                additional_directories: vec!["D:/lib\n - Platform: linux".into()],
                ..facts()
            },
        );
        assert!(
            section.contains(" - Primary working directory: C:/work/app"),
            "{section}"
        );
        assert!(!section.contains('\u{202e}'), "{section}");
        // The newline is gone, so the forged text stays inside the item it was
        // written into; only one line can claim to be the platform fact.
        assert_eq!(
            section
                .lines()
                .filter(|line| line.starts_with(" - Platform:"))
                .count(),
            1,
            "a path must not be able to forge a second fact: {section}"
        );
    }

    #[test]
    fn an_emptied_frame_suppresses_the_whole_section() {
        let profile = PromptProfile::from_file(
            "test".into(),
            "Test".into(),
            ResolvedLanguage::EnUs,
            [(PromptKey::SystemEnvironmentSection, String::new())]
                .into_iter()
                .collect(),
            Vec::new(),
        );
        assert_eq!(environment_section(&profile, &facts()), "");
    }

    #[test]
    fn the_chinese_profile_reports_the_same_facts() {
        let profile = PromptProfile::builtin_chinese();
        let section = environment_section(&profile, &facts());
        assert!(section.contains("C:/work/app"), "{section}");
        assert!(section.contains("Windows 10.0.26100"), "{section}");
        assert!(section.contains("2026-01-02"), "{section}");
        assert_eq!(section.lines().count(), 7, "{section}");
    }

    #[test]
    fn collecting_reads_the_workspace_and_this_machine() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("checkout");
        let extra = root.path().join("library");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&extra).unwrap();
        let extra = extra.to_string_lossy().into_owned();

        let facts = EnvironmentFacts::collect(&workspace, false, std::slice::from_ref(&extra));
        assert_eq!(facts.working_directory, workspace.to_string_lossy());
        assert!(!facts.is_worktree);
        assert!(!facts.is_git_repository);
        assert_eq!(facts.additional_directories, vec![extra.clone()]);
        assert_eq!(facts.platform, std::env::consts::OS);
        assert_eq!(facts.date.len(), "2026-01-02".len());

        std::fs::create_dir(workspace.join(".git")).unwrap();
        let facts = EnvironmentFacts::collect(&workspace, true, &[]);
        assert!(facts.is_worktree);
        assert!(facts.is_git_repository);
        assert!(facts.additional_directories.is_empty());
    }

    #[test]
    fn the_windows_verbatim_prefix_never_reaches_the_model() {
        // A recorded workspace path keeps whatever `canonicalize` gave it, and the
        // model types this path back into shell commands.
        assert_eq!(display_path(r"\\?\C:\work\app"), r"C:\work\app");
        assert_eq!(
            display_path(r"\\?\UNC\server\share\app"),
            r"\\server\share\app"
        );
        assert_eq!(display_path(r"C:\work\app"), r"C:\work\app");
        assert_eq!(display_path("/home/u/app"), "/home/u/app");
    }

    #[test]
    fn a_checkout_is_recognized_by_its_git_entry_at_any_depth() {
        let root = tempfile::tempdir().unwrap();
        let nested = root.path().join("crates").join("inner");
        std::fs::create_dir_all(&nested).unwrap();
        assert!(!is_git_repository(&nested));
        std::fs::create_dir(root.path().join(".git")).unwrap();
        assert!(is_git_repository(&nested));
        assert!(is_git_repository(root.path()));
    }

    #[test]
    fn this_machine_reports_its_own_version() {
        // The value differs per host, so the invariant is that the reader either
        // answers or fails closed — never that it returns a placeholder string.
        let version = os_version();
        assert_eq!(
            version,
            os_version(),
            "the read must be cached, not repeated"
        );
        if cfg!(any(windows, unix)) {
            assert!(
                !version.is_empty(),
                "expected a version on a supported host"
            );
        }
    }
}
