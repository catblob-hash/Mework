//! The numbered workspaces a conversation can reach, resolved by the host.
//!
//! A conversation works in one or more directories, and each of them lives on a
//! machine: the host, a WSL distribution, or a registered SSH machine. This
//! module turns the persisted record of that — a primary workspace plus the
//! attached ones — into the single list everything else reads.
//!
//! The list is ordered and 1-based, and that number is the whole of the model's
//! addressing scheme. It is what the `workspace` parameter on every path-taking
//! tool carries, what the `# Environment` section enumerates, and what decides
//! which shell a command can run in. Paths are never how the model selects a
//! machine: a path is resolved *inside* the workspace it named, so naming a
//! directory can never reach a machine the conversation was not granted.
//!
//! Resolution is host-only, like [`ShellRunner`] itself. It reads the persisted
//! conversation and the machine catalog; neither renderer input nor a tool
//! argument can introduce a root or a machine that is not already recorded.

use crate::model::{AttachedWorkspace, ExecutionEnvironmentAssets, RunTarget};
use crate::run_environment::{resolve_shell_runner, ShellRunner};

/// The most workspaces one conversation may address.
///
/// The bound exists so the enum of allowed values stays a thing a model can read
/// at a glance, and so a document cannot make a tool schema unboundedly large.
/// It matches the renderer's own limit on attachable directories.
pub const MAX_WORKSPACES: usize = 32;

/// Which family of shell a workspace's machine speaks.
///
/// This is the only thing the workspace list says about an operating system, and
/// it exists for one question: whether `powershell` can run there. Mework's WSL
/// and SSH legs both invoke `bash`, so every remote workspace is POSIX — an SSH
/// endpoint that happens to be Windows is still reached through a POSIX shell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkspaceOs {
    Windows,
    Posix,
}

impl WorkspaceOs {
    /// Whether the `powershell` tool can run in a workspace on this machine.
    pub fn runs_powershell(self) -> bool {
        matches!(self, Self::Windows)
    }
}

/// One workspace, with everything a caller needs to act in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedWorkspace {
    /// 1-based position in the conversation's list — the model's address for it.
    pub index: u32,
    /// Machine binding, `None` for the host machine.
    pub machine: Option<RunTarget>,
    /// Root directory on that machine. Host paths are whatever the host records;
    /// remote paths are POSIX and may begin with `~`.
    pub root: String,
    /// Trusted shell environment for that machine, including its variable table.
    pub runner: ShellRunner,
    /// Shell family, which is what decides `powershell` availability.
    pub os: WorkspaceOs,
    /// Human-readable machine name, used when the list is stated to the model.
    /// Empty for the host machine, which needs no qualifier.
    pub machine_label: String,
}

impl ResolvedWorkspace {
    /// Whether this workspace is on the host machine, where the filesystem tools
    /// act directly rather than through a shell transport.
    pub fn is_local(&self) -> bool {
        self.machine.is_none()
    }
}

/// A conversation's workspaces in address order.
///
/// Never empty: a conversation always has a primary workspace, which is entry 1.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkspaceSet {
    entries: Vec<ResolvedWorkspace>,
}

impl WorkspaceSet {
    /// Resolves the persisted record into the numbered list.
    ///
    /// `primary` is the conversation's effective workspace — its worktree when it
    /// has one, the workspace root otherwise — already carrying the machine that
    /// workspace is registered on. `attached` follows in its recorded order.
    ///
    /// A machine that is no longer in the catalog fails the whole resolution
    /// rather than dropping the entry. Dropping it would renumber everything
    /// after it, and a conversation whose "workspace 3" silently became a
    /// different directory is worse than one that says the machine is gone.
    pub fn resolve(
        assets: &ExecutionEnvironmentAssets,
        primary: &AttachedWorkspace,
        attached: &[AttachedWorkspace],
    ) -> Result<Self, String> {
        let mut entries = Vec::with_capacity(1 + attached.len());
        for (position, workspace) in std::iter::once(primary).chain(attached).enumerate() {
            if position >= MAX_WORKSPACES {
                break;
            }
            let runner = resolve_shell_runner(assets, workspace.machine.as_ref())?;
            entries.push(ResolvedWorkspace {
                index: position as u32 + 1,
                machine: workspace.machine.clone(),
                root: workspace.path.clone(),
                os: workspace_os(&runner),
                machine_label: machine_label(assets, workspace.machine.as_ref()),
                runner,
            });
        }
        Ok(Self { entries })
    }

    /// Builds a single-workspace set on the host machine.
    ///
    /// The shape every caller that predates machine-bound workspaces still wants:
    /// one local root, no catalog to consult.
    pub fn local_root(path: impl Into<String>) -> Self {
        Self {
            entries: vec![ResolvedWorkspace {
                index: 1,
                machine: None,
                root: path.into(),
                runner: ShellRunner::default(),
                os: host_os(),
                machine_label: String::new(),
            }],
        }
    }

    /// Builds a single-workspace set around a runner the caller already holds.
    ///
    /// Tests and legacy callers carry a resolved `ShellRunner` rather than a
    /// machine record, and re-reading the catalog to reconstruct what the runner
    /// already encodes would only introduce a second way to disagree with it.
    /// The machine binding is therefore derived from the runner itself: a local
    /// runner stays host-local, a WSL runner names its distro, and an SSH runner
    /// cannot recover its machine id — its fingerprint is the host string, not
    /// the catalog row — so it gets a placeholder id and the guard-level facts
    /// (POSIX, no label) that do not depend on the catalog.
    pub fn single(root: impl Into<String>, runner: ShellRunner) -> Self {
        let machine = match &runner {
            ShellRunner::Local { .. } => None,
            ShellRunner::Wsl { distro, .. } => Some(RunTarget::Wsl {
                distro: distro.clone(),
            }),
            ShellRunner::Ssh { .. } => Some(RunTarget::Ssh {
                machine_id: String::new(),
            }),
        };
        Self {
            entries: vec![ResolvedWorkspace {
                index: 1,
                machine,
                root: root.into(),
                os: workspace_os(&runner),
                machine_label: String::new(),
                runner,
            }],
        }
    }

    pub fn entries(&self) -> &[ResolvedWorkspace] {
        &self.entries
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The primary workspace — entry 1 — or `None` for a set built empty.
    pub fn primary(&self) -> Option<&ResolvedWorkspace> {
        self.entries.first()
    }

    /// Looks a workspace up by the number the model used.
    pub fn get(&self, index: u32) -> Option<&ResolvedWorkspace> {
        self.entries
            .iter()
            .find(|workspace| workspace.index == index)
    }

    /// Resolves the `workspace` argument of a tool call.
    ///
    /// Absent means workspace 1: a conversation with one workspace never sees the
    /// parameter, and a model that omits it in a multi-workspace conversation
    /// means the one it was told is primary. An out-of-range number is an error
    /// rather than a fallback — silently acting in the wrong directory is the one
    /// outcome no caller can recover from.
    pub fn select(&self, index: Option<u32>) -> Result<&ResolvedWorkspace, String> {
        let index = index.unwrap_or(1);
        self.get(index).ok_or_else(|| {
            format!(
                "There is no workspace {index}. This conversation has {}.",
                self.address_list()
            )
        })
    }

    /// The addresses in this set, as prose for an error message.
    fn address_list(&self) -> String {
        match self.entries.len() {
            0 => "none".to_owned(),
            1 => "only workspace 1".to_owned(),
            _ => format!(
                "workspaces {}",
                self.entries
                    .iter()
                    .map(|workspace| workspace.index.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }

    /// Every address in the set, for a schema enum.
    pub fn addresses(&self) -> Vec<u32> {
        self.entries
            .iter()
            .map(|workspace| workspace.index)
            .collect()
    }

    /// Addresses whose machine can run `powershell`.
    ///
    /// Empty means the tool has nowhere to run in this conversation, which is
    /// what withdraws it from the wire entirely.
    pub fn powershell_addresses(&self) -> Vec<u32> {
        self.entries
            .iter()
            .filter(|workspace| workspace.os.runs_powershell())
            .map(|workspace| workspace.index)
            .collect()
    }

    /// Whether any workspace can run `powershell`.
    pub fn runs_powershell(&self) -> bool {
        self.entries
            .iter()
            .any(|workspace| workspace.os.runs_powershell())
    }

    /// Roots on the host machine, which is the set the local path guard trusts.
    ///
    /// Remote roots are deliberately absent: they are not paths in this
    /// filesystem, and admitting one would let a remote root's spelling widen
    /// local reach if the two ever collided.
    pub fn local_roots(&self) -> Vec<String> {
        self.entries
            .iter()
            .filter(|workspace| workspace.is_local())
            .map(|workspace| workspace.root.clone())
            .collect()
    }
}

/// The shell family of the machine a runner dispatches to.
fn workspace_os(runner: &ShellRunner) -> WorkspaceOs {
    match runner {
        ShellRunner::Local { .. } => host_os(),
        ShellRunner::Wsl { .. } | ShellRunner::Ssh { .. } => WorkspaceOs::Posix,
    }
}

/// This machine's shell family.
fn host_os() -> WorkspaceOs {
    if cfg!(windows) {
        WorkspaceOs::Windows
    } else {
        WorkspaceOs::Posix
    }
}

/// The name to show for a machine, or empty for the host.
///
/// A deleted SSH machine cannot reach here — [`resolve_shell_runner`] has already
/// failed the resolution — so the catalog lookup is a read, not a fallback.
fn machine_label(assets: &ExecutionEnvironmentAssets, machine: Option<&RunTarget>) -> String {
    match machine {
        None => String::new(),
        Some(RunTarget::Wsl { distro }) => distro.clone(),
        Some(RunTarget::Ssh { machine_id }) => assets
            .ssh_machines
            .iter()
            .find(|candidate| candidate.id == *machine_id)
            .map(|candidate| candidate.name.clone())
            .unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::SshMachineConfig;

    fn assets() -> ExecutionEnvironmentAssets {
        ExecutionEnvironmentAssets {
            ssh_machines: vec![SshMachineConfig {
                id: "m1".into(),
                name: "devbox".into(),
                host: "user@devbox".into(),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    fn local(path: &str) -> AttachedWorkspace {
        AttachedWorkspace {
            machine: None,
            path: path.into(),
        }
    }

    fn remote(path: &str) -> AttachedWorkspace {
        AttachedWorkspace {
            machine: Some(RunTarget::Ssh {
                machine_id: "m1".into(),
            }),
            path: path.into(),
        }
    }

    #[test]
    fn addresses_are_one_based_and_follow_the_recorded_order() {
        let set = WorkspaceSet::resolve(
            &assets(),
            &local("C:/work/app"),
            &[remote("~/services"), local("D:/shared")],
        )
        .unwrap();

        assert_eq!(set.addresses(), vec![1, 2, 3]);
        assert_eq!(set.get(1).unwrap().root, "C:/work/app");
        assert_eq!(set.get(2).unwrap().root, "~/services");
        assert_eq!(set.get(2).unwrap().machine_label, "devbox");
        assert_eq!(set.get(3).unwrap().root, "D:/shared");
        assert!(set.get(4).is_none());
    }

    #[test]
    fn an_absent_argument_selects_the_primary_workspace() {
        let set = WorkspaceSet::resolve(&assets(), &local("C:/work/app"), &[remote("~/services")])
            .unwrap();
        assert_eq!(set.select(None).unwrap().index, 1);
        assert_eq!(set.select(Some(2)).unwrap().index, 2);
    }

    #[test]
    fn an_out_of_range_address_names_what_does_exist() {
        let set = WorkspaceSet::resolve(&assets(), &local("C:/work/app"), &[remote("~/services")])
            .unwrap();
        let error = set.select(Some(7)).unwrap_err();
        assert!(error.contains("no workspace 7"), "{error}");
        assert!(error.contains("workspaces 1, 2"), "{error}");

        let single = WorkspaceSet::resolve(&assets(), &local("C:/work/app"), &[]).unwrap();
        assert!(
            single.select(Some(2)).unwrap_err().contains("only workspace 1"),
            "a one-workspace conversation should say so"
        );
    }

    #[test]
    fn remote_workspaces_are_posix_and_never_run_powershell() {
        let set =
            WorkspaceSet::resolve(&assets(), &remote("~/app"), &[remote("~/services")]).unwrap();
        assert_eq!(set.get(1).unwrap().os, WorkspaceOs::Posix);
        assert!(set.powershell_addresses().is_empty());
        assert!(!set.runs_powershell());
    }

    #[test]
    fn powershell_addresses_name_only_the_windows_workspaces() {
        let set =
            WorkspaceSet::resolve(&assets(), &local("C:/work/app"), &[remote("~/services")])
                .unwrap();
        if cfg!(windows) {
            assert_eq!(set.powershell_addresses(), vec![1]);
            assert!(set.runs_powershell());
        } else {
            // The host itself is POSIX, so no workspace in this set can run it.
            assert!(set.powershell_addresses().is_empty());
        }
    }

    #[test]
    fn only_host_roots_reach_the_local_path_guard() {
        let set = WorkspaceSet::resolve(
            &assets(),
            &local("C:/work/app"),
            &[remote("~/services"), local("D:/shared")],
        )
        .unwrap();
        assert_eq!(set.local_roots(), vec!["C:/work/app", "D:/shared"]);
    }

    #[test]
    fn a_deleted_machine_fails_the_whole_set_rather_than_renumbering_it() {
        let dangling = AttachedWorkspace {
            machine: Some(RunTarget::Ssh {
                machine_id: "gone".into(),
            }),
            path: "~/app".into(),
        };
        let error = WorkspaceSet::resolve(&assets(), &local("C:/work/app"), &[dangling]).unwrap_err();
        assert!(error.contains("gone"), "{error}");
    }

    #[test]
    fn the_address_space_is_bounded() {
        let attached: Vec<AttachedWorkspace> = (0..MAX_WORKSPACES + 10)
            .map(|index| local(&format!("C:/extra/{index}")))
            .collect();
        let set = WorkspaceSet::resolve(&assets(), &local("C:/work/app"), &attached).unwrap();
        assert_eq!(set.len(), MAX_WORKSPACES);
        assert_eq!(*set.addresses().last().unwrap(), MAX_WORKSPACES as u32);
    }

    #[test]
    fn a_bare_local_root_is_one_host_workspace() {
        let set = WorkspaceSet::local_root("C:/work/app");
        assert_eq!(set.addresses(), vec![1]);
        assert!(set.primary().unwrap().is_local());
        assert_eq!(set.local_roots(), vec!["C:/work/app"]);
    }
}
