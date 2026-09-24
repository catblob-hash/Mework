//! The numbered workspaces a conversation can reach, resolved by the host.
//!
//! A conversation works in one or more directories, and each of them lives on a
//! machine: the host, a WSL distribution, or a registered SSH machine. This
//! module turns the persisted record of that — the project's workspace 1, the
//! project's further workspaces, then the conversation's own attached ones —
//! into the single list everything else reads.
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

use crate::machine_shells::DetectedShell;
use crate::model::{AttachedWorkspace, ExecutionEnvironmentAssets, RunTarget};
use crate::run_environment::{resolve_shell_runner, ShellRunner};
use crate::shell_backend::{MachineOs, ShellBackend};

/// The most workspaces one conversation may address: every workspace its
/// project may hold, then every directory it may attach.
///
/// The bound exists so a document cannot make a tool schema unboundedly large.
/// It is the sum of the two limits the host enforces on save rather than a
/// number of its own, because a smaller cap here would drop the last attached
/// workspaces without a word — the list is truncated, not refused — and the
/// model would never learn they were granted.
pub const MAX_WORKSPACES: usize =
    crate::storage::MAX_PROJECT_WORKSPACES + crate::storage::MAX_ADDITIONAL_DIRECTORIES;

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
    /// The machine's operating system, or `None` for an SSH machine that has
    /// not been probed yet.
    pub os: Option<MachineOs>,
    /// The shell backends a command can run in here, from the machine's last
    /// probe ([`crate::machine_shells::known`]). This is what decides which
    /// shell tools may name this workspace.
    pub shells: Vec<DetectedShell>,
    /// Human-readable machine name, used when the list is stated to the model.
    /// Empty for the host machine, which needs no qualifier.
    pub machine_label: String,
    /// The sandbox this workspace's commands run in, when sandboxing is on:
    /// the conversation's cell on this workspace's machine (see
    /// [`WorkspaceSet::sandboxed`]).
    pub sandbox: Option<remote_agent::protocol::SandboxSpec>,
}

impl ResolvedWorkspace {
    /// Whether this workspace is on the host machine, where the filesystem tools
    /// act directly rather than through a shell transport.
    pub fn is_local(&self) -> bool {
        self.machine.is_none()
    }

    /// Where `backend` is on this workspace's machine, if it is there.
    pub fn shell(&self, backend: ShellBackend) -> Option<&DetectedShell> {
        self.shells.iter().find(|shell| shell.backend == backend)
    }

    /// Whether a command can run in `backend` here.
    pub fn runs(&self, backend: ShellBackend) -> bool {
        self.shell(backend).is_some()
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
    /// workspace is registered on. `attached` follows in its recorded order; for
    /// a conversation that is
    /// [`Workspace::conversation_workspaces_after_primary`](crate::model::Workspace::conversation_workspaces_after_primary):
    /// the project's further workspaces, then the conversation's own.
    ///
    /// Each entry's runner carries the variable table recorded for that
    /// workspace — its machine and its path. Workspace 1's is the one recorded
    /// under `primary_env_path`, which is not its root when the conversation
    /// runs in an isolated worktree: the variables belong to the project's
    /// registered directory the worktree was checked out from.
    ///
    /// A machine that is no longer in the catalog fails the whole resolution
    /// rather than dropping the entry. Dropping it would renumber everything
    /// after it, and a conversation whose "workspace 3" silently became a
    /// different directory is worse than one that says the machine is gone.
    pub fn resolve_with_primary_env(
        assets: &ExecutionEnvironmentAssets,
        primary: &AttachedWorkspace,
        primary_env_path: &str,
        attached: &[AttachedWorkspace],
    ) -> Result<Self, String> {
        let mut entries = Vec::with_capacity(1 + attached.len());
        for (position, workspace) in std::iter::once(primary).chain(attached).enumerate() {
            if position >= MAX_WORKSPACES {
                break;
            }
            let env_path = if position == 0 {
                primary_env_path
            } else {
                workspace.path.as_str()
            };
            let runner = resolve_shell_runner(assets, workspace.machine.as_ref(), Some(env_path))?;
            let (os, shells) = crate::machine_shells::known(workspace.machine.as_ref());
            entries.push(ResolvedWorkspace {
                index: position as u32 + 1,
                machine: workspace.machine.clone(),
                root: workspace.path.clone(),
                os,
                shells,
                machine_label: machine_label(assets, workspace.machine.as_ref()),
                runner,
                sandbox: None,
            });
        }
        Ok(Self { entries })
    }

    /// [`resolve_with_primary_env`](Self::resolve_with_primary_env) for a
    /// primary whose variables are recorded under its own root.
    #[cfg(test)]
    pub fn resolve(
        assets: &ExecutionEnvironmentAssets,
        primary: &AttachedWorkspace,
        attached: &[AttachedWorkspace],
    ) -> Result<Self, String> {
        Self::resolve_with_primary_env(assets, primary, &primary.path, attached)
    }

    /// Builds a single-workspace set on the host machine.
    ///
    /// The shape every caller that predates machine-bound workspaces still wants:
    /// one local root, no catalog to consult.
    pub fn local_root(path: impl Into<String>) -> Self {
        let (os, shells) = crate::machine_shells::known(None);
        Self {
            entries: vec![ResolvedWorkspace {
                index: 1,
                machine: None,
                root: path.into(),
                runner: ShellRunner::default(),
                os,
                shells,
                machine_label: String::new(),
                sandbox: None,
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
        let (os, shells) = match &runner {
            // An SSH runner's machine id is unrecoverable, so its last probe is
            // too; it keeps what an unprobed machine is assumed to have.
            ShellRunner::Ssh { .. } => crate::machine_shells::assumed(machine.as_ref()),
            _ => crate::machine_shells::known(machine.as_ref()),
        };
        Self {
            entries: vec![ResolvedWorkspace {
                index: 1,
                machine,
                root: root.into(),
                os,
                shells,
                machine_label: String::new(),
                runner,
                sandbox: None,
            }],
        }
    }

    /// The same set with every workspace's commands confined to the sandbox
    /// `settings` describe, when they are on.
    ///
    /// A conversation has one cell per machine, named for the conversation, and
    /// each cell may write every workspace the conversation has on that
    /// machine: one command may well build in workspace 1 and write its
    /// output to workspace 2. A different conversation — or this one after its
    /// workspaces or the settings changed — is a different cell.
    pub fn sandboxed(mut self, settings: &crate::model::SandboxSettings, conversation_id: &str) -> Self {
        if !settings.enabled {
            return self;
        }
        let machine_of = |workspace: &ResolvedWorkspace| crate::run_environment::env_key(workspace.machine.as_ref());
        let roots: Vec<(String, String)> = self
            .entries
            .iter()
            .map(|workspace| (machine_of(workspace), workspace.root.clone()))
            .collect();
        for workspace in &mut self.entries {
            let machine = machine_of(workspace);
            let mut writable: Vec<String> = roots
                .iter()
                .filter(|(other, _)| *other == machine)
                .map(|(_, root)| root.clone())
                .collect();
            writable.extend(settings.writable.iter().cloned());
            writable.sort();
            writable.dedup();
            workspace.sandbox = Some(remote_agent::protocol::SandboxSpec {
                cell: format!("conversation-{conversation_id}"),
                policy: remote_agent::protocol::SandboxPolicy {
                    writable,
                    deny_read: settings.deny_read.clone(),
                    readable: Vec::new(),
                    deny_write: Vec::new(),
                    network: settings.network_policy(),
                },
            });
        }
        self
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

    /// Workspace 1's shell environment, which is the run's own: what a tool
    /// approval is bound to and what a child agent inherits. The host's for a
    /// set built empty.
    pub fn primary_runner(&self) -> ShellRunner {
        self.primary()
            .map(|workspace| workspace.runner.clone())
            .unwrap_or_default()
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

    /// Addresses whose machine has `backend`.
    ///
    /// Empty means the backend's tools have nowhere to run in this
    /// conversation, which is what withdraws them from the wire entirely.
    pub fn shell_addresses(&self, backend: ShellBackend) -> Vec<u32> {
        self.entries
            .iter()
            .filter(|workspace| workspace.runs(backend))
            .map(|workspace| workspace.index)
            .collect()
    }

    /// Whether any workspace's machine has `backend`.
    pub fn runs(&self, backend: ShellBackend) -> bool {
        self.entries.iter().any(|workspace| workspace.runs(backend))
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

    #[test]
    fn a_sandboxed_set_gives_each_machine_one_cell_writing_all_its_workspaces() {
        let assets = assets();
        let set = WorkspaceSet::resolve(
            &assets,
            &AttachedWorkspace { machine: None, path: "/work/a".into() },
            &[
                AttachedWorkspace { machine: None, path: "/work/b".into() },
                AttachedWorkspace {
                    machine: Some(RunTarget::Ssh { machine_id: "m1".into() }),
                    path: "/home/dev/c".into(),
                },
            ],
        )
        .unwrap();
        assert!(set.clone().sandboxed(&crate::model::SandboxSettings::default(), "c1").entries().iter().all(|w| w.sandbox.is_none()));
        let settings = crate::model::SandboxSettings {
            enabled: true,
            writable: vec!["~/shared".into()],
            ..Default::default()
        };
        let set = set.sandboxed(&settings, "c1");
        let local = set.get(1).unwrap().sandbox.clone().unwrap();
        assert_eq!(local.cell, "conversation-c1");
        assert_eq!(local.policy.writable, vec!["/work/a".to_owned(), "/work/b".into(), "~/shared".into()]);
        assert_eq!(set.get(2).unwrap().sandbox, Some(local.clone()));
        let ssh = set.get(3).unwrap().sandbox.clone().unwrap();
        assert_eq!(ssh.cell, "conversation-c1");
        assert_eq!(ssh.policy.writable, vec!["/home/dev/c".to_owned(), "~/shared".into()]);
        assert_eq!(ssh.policy.network.mode, remote_agent::protocol::NetworkMode::Allowlist);
        assert!(ssh.policy.network.allow.iter().any(|host| host == "registry.npmjs.org"));
    }
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

    /// A machine nobody has probed keeps what every remote leg ran before
    /// machines had backends: bash, and nothing else.
    #[test]
    fn an_unprobed_remote_machine_is_assumed_to_have_bash_only() {
        let assets = ExecutionEnvironmentAssets {
            ssh_machines: vec![SshMachineConfig {
                id: "unprobed".into(),
                name: "unprobed".into(),
                host: "user@unprobed".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let workspace = AttachedWorkspace {
            machine: Some(RunTarget::Ssh {
                machine_id: "unprobed".into(),
            }),
            path: "~/app".into(),
        };
        let set = WorkspaceSet::resolve(&assets, &workspace, &[]).unwrap();
        let entry = set.get(1).unwrap();
        assert_eq!(entry.os, None);
        assert!(entry.runs(ShellBackend::Bash));
        assert!(!entry.runs(ShellBackend::PowerShell));
        assert!(set.shell_addresses(ShellBackend::PowerShell).is_empty());
    }

    /// A probed machine lists exactly what the probe found, and each shell's
    /// addresses are the workspaces on machines that have it.
    #[test]
    fn shell_addresses_follow_each_machines_probe() {
        use crate::machine_shells::{seed_for_test, MachineShells};
        let assets = ExecutionEnvironmentAssets {
            ssh_machines: vec![SshMachineConfig {
                id: "winbox".into(),
                name: "winbox".into(),
                host: "user@winbox".into(),
                agent_shell: Some(ShellBackend::PowerShell),
                ..Default::default()
            }],
            ..Default::default()
        };
        seed_for_test(
            "ssh:winbox",
            MachineShells {
                os: MachineOs::Windows,
                shells: vec![DetectedShell {
                    backend: ShellBackend::PowerShell,
                    path: r"C:\Program Files\PowerShell\7\pwsh.exe".into(),
                }],
                probed_at: String::new(),
            },
        );
        let windows = AttachedWorkspace {
            machine: Some(RunTarget::Ssh {
                machine_id: "winbox".into(),
            }),
            path: "C:/work".into(),
        };
        let set = WorkspaceSet::resolve(&assets, &local("/work/app"), &[windows]).unwrap();
        assert_eq!(set.get(2).unwrap().os, Some(MachineOs::Windows));
        assert_eq!(set.shell_addresses(ShellBackend::PowerShell), {
            let mut expected = Vec::new();
            if set.get(1).unwrap().runs(ShellBackend::PowerShell) {
                expected.push(1);
            }
            expected.push(2);
            expected
        });
        assert!(!set.get(2).unwrap().runs(ShellBackend::Bash));
        // The agent shell follows the machine's settings once the machine has it.
        let shell = set.get(2).unwrap().runner.agent_shell().unwrap().clone();
        assert_eq!(shell.backend, ShellBackend::PowerShell);
        assert!(shell.program.ends_with("pwsh.exe"));
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

    /// A conversation's list is its project's workspaces first — workspace 1,
    /// then the project's further ones in order — and its own attached
    /// workspaces after them.
    #[test]
    fn project_workspaces_are_numbered_before_the_conversations_own() {
        let mut document = crate::catalog::default_document();
        let project = &mut document.workspaces[0];
        project.path = "C:/work/app".into();
        project.additional_workspaces = vec![remote("~/services"), local("D:/shared")];
        project.conversations[0].attached_workspaces = vec![local("E:/notes")];
        let project = &document.workspaces[0];
        let conversation = &project.conversations[0];

        let set = WorkspaceSet::resolve(
            &assets(),
            &local(&project.path),
            &project.conversation_workspaces_after_primary(conversation),
        )
        .unwrap();

        assert_eq!(set.addresses(), vec![1, 2, 3, 4]);
        assert_eq!(set.get(1).unwrap().root, "C:/work/app");
        assert_eq!(set.get(2).unwrap().root, "~/services");
        assert_eq!(set.get(2).unwrap().machine_label, "devbox");
        assert_eq!(set.get(3).unwrap().root, "D:/shared");
        assert_eq!(set.get(4).unwrap().root, "E:/notes");
        // The run's own shell is workspace 1's, whatever the members are on.
        assert!(matches!(set.primary_runner(), ShellRunner::Local { .. }));
    }

    /// A temporary project has no shared root, so an entry recorded on one
    /// (which validation refuses) never reaches its conversations' list.
    #[test]
    fn a_temporary_project_contributes_no_members() {
        let mut document = crate::catalog::default_document();
        let temporary = document
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.kind == crate::model::WorkspaceKind::Temporary)
            .unwrap();
        temporary.additional_workspaces = vec![local("D:/shared")];
        let mut conversation = document.workspaces[0].conversations[0].clone();
        conversation.attached_workspaces = vec![local("E:/notes")];
        let temporary = document
            .workspaces
            .iter()
            .find(|workspace| workspace.kind == crate::model::WorkspaceKind::Temporary)
            .unwrap();
        assert_eq!(
            temporary.conversation_workspaces_after_primary(&conversation),
            vec![local("E:/notes")]
        );
    }

    #[test]
    fn each_workspace_carries_its_own_variables() {
        let mut assets = assets();
        for (key, value) in [
            ("local|C:/work/app", "app"),
            ("ssh:m1|~/services", "services"),
            ("local|C:/work/tools", "tools"),
        ] {
            assets.env_vars.insert(
                key.to_owned(),
                [("WHICH".to_owned(), value.to_owned())].into_iter().collect(),
            );
        }
        let which = |set: &WorkspaceSet, index: u32| {
            set.get(index)
                .and_then(|workspace| workspace.runner.env().get("WHICH").cloned())
        };

        let set = WorkspaceSet::resolve(
            &assets,
            &local("C:/work/app"),
            &[remote("~/services"), local("C:/work/tools"), local("C:/work/bare")],
        )
        .unwrap();
        assert_eq!(which(&set, 1).as_deref(), Some("app"));
        assert_eq!(which(&set, 2).as_deref(), Some("services"));
        assert_eq!(which(&set, 3).as_deref(), Some("tools"));
        assert_eq!(which(&set, 4), None);

        // A worktree stands in for workspace 1 at another path, and still runs
        // with the variables of the directory it was checked out from.
        let worktree = WorkspaceSet::resolve_with_primary_env(
            &assets,
            &local("C:/data/worktrees/app-1"),
            "C:/work/app",
            &[],
        )
        .unwrap();
        assert_eq!(worktree.get(1).unwrap().root, "C:/data/worktrees/app-1");
        assert_eq!(which(&worktree, 1).as_deref(), Some("app"));
    }

    #[test]
    fn the_run_environment_follows_a_remote_primary() {
        let set = WorkspaceSet::resolve(&assets(), &remote("~/app"), &[local("C:/work")]).unwrap();
        assert!(matches!(set.primary_runner(), ShellRunner::Ssh { .. }));
        assert_eq!(
            WorkspaceSet::default().primary_runner(),
            ShellRunner::default()
        );
    }

    #[test]
    fn a_bare_local_root_is_one_host_workspace() {
        let set = WorkspaceSet::local_root("C:/work/app");
        assert_eq!(set.addresses(), vec![1]);
        assert!(set.primary().unwrap().is_local());
        assert_eq!(set.local_roots(), vec!["C:/work/app"]);
    }
}
