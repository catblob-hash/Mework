//! Process-local coordination for operations that touch one or more workspaces.
//!
//! The coordinator intentionally does not canonicalize paths. Callers must pass
//! the canonical identity they already use for their workspace boundary. This
//! keeps filesystem policy at the trust boundary while still accepting either a
//! string or a `Path`/`PathBuf`.
//!
//! Acquisitions are try-only: an incompatible active lease is reported
//! immediately. There is no wait queue or condition variable. A single mutex is
//! held only while checking and updating the small in-memory state. The returned
//! RAII lease owns no mutex guard and releases its slots when dropped.

use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
};

/// A caller-normalized identity for one workspace.
///
/// `WorkspaceKey` deliberately preserves the supplied path identity. In
/// particular, it does not perform I/O, resolve symlinks, or change case.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct WorkspaceKey(PathBuf);

impl WorkspaceKey {
    pub fn new(identity: impl Into<PathBuf>) -> Self {
        Self(identity.into())
    }

    #[cfg(test)]
    pub fn as_path(&self) -> &Path {
        &self.0
    }

    fn validate(&self) -> Result<(), CoordinationError> {
        if self.0.as_os_str().is_empty() {
            return Err(CoordinationError::EmptyWorkspaceKey);
        }
        Ok(())
    }
}

impl fmt::Display for WorkspaceKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.display().fmt(formatter)
    }
}

impl From<String> for WorkspaceKey {
    fn from(value: String) -> Self {
        Self(PathBuf::from(value))
    }
}

impl From<&str> for WorkspaceKey {
    fn from(value: &str) -> Self {
        Self(PathBuf::from(value))
    }
}

impl From<PathBuf> for WorkspaceKey {
    fn from(value: PathBuf) -> Self {
        Self(value)
    }
}

impl From<&Path> for WorkspaceKey {
    fn from(value: &Path) -> Self {
        Self(value.to_path_buf())
    }
}

/// The compatibility class of a lease.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OperationMode {
    GlobalShared,
    WorkspaceShared,
    WorkspaceExclusive,
    GlobalExclusive,
}

impl fmt::Display for OperationMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::GlobalShared => "global shared",
            Self::WorkspaceShared => "workspace shared",
            Self::WorkspaceExclusive => "workspace exclusive",
            Self::GlobalExclusive => "global exclusive",
        };
        formatter.write_str(name)
    }
}

/// Optional diagnostic ownership attached to an active lease.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeaseAttribution {
    pub conversation_id: Option<String>,
}

impl LeaseAttribution {
    fn new(conversation_id: Option<String>) -> Self {
        Self { conversation_id }
    }
}

/// A count of shared leases with the same attribution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AttributedLeaseCount {
    pub attribution: LeaseAttribution,
    pub lease_count: usize,
}

/// The active holders for a workspace.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WorkspaceHolders {
    pub shared: Vec<AttributedLeaseCount>,
    pub exclusive: Option<LeaseAttribution>,
}

impl WorkspaceHolders {
    pub fn lease_count(&self) -> usize {
        self.shared
            .iter()
            .map(|entry| entry.lease_count)
            .sum::<usize>()
            + usize::from(self.exclusive.is_some())
    }
}

/// A stable diagnostic view of one active workspace slot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceActivity {
    pub workspace: WorkspaceKey,
    pub holders: WorkspaceHolders,
}

/// A point-in-time diagnostic view of the coordinator. Nothing in the running application
/// inspects coordinator internals: operations acquire leases and act on the result, so this
/// view exists for the coordination tests.
#[cfg(test)]
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CoordinatorSnapshot {
    pub global_shared: Vec<AttributedLeaseCount>,
    pub global_exclusive: Option<LeaseAttribution>,
    pub workspaces: Vec<WorkspaceActivity>,
}

#[cfg(test)]
impl CoordinatorSnapshot {
    pub fn is_idle(&self) -> bool {
        self.global_shared.is_empty()
            && self.global_exclusive.is_none()
            && self.workspaces.is_empty()
    }

    pub fn active_lease_count(&self) -> usize {
        self.global_shared
            .iter()
            .map(|entry| entry.lease_count)
            .sum::<usize>()
            + usize::from(self.global_exclusive.is_some())
            + self
                .workspaces
                .iter()
                .map(|workspace| workspace.holders.lease_count())
                .sum::<usize>()
    }
}

/// A structured reason why a try-acquire operation was rejected.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CoordinationError {
    EmptyWorkspaceKey,
    EmptyWorkspaceSet,
    GlobalExclusiveActive {
        requested: OperationMode,
        holder: LeaseAttribution,
    },
    GlobalSharedActive {
        requested: OperationMode,
        holders: Vec<AttributedLeaseCount>,
    },
    WorkspaceBusy {
        requested: OperationMode,
        workspace: WorkspaceKey,
        holders: WorkspaceHolders,
    },
    WorkspaceOperationsActive {
        requested: OperationMode,
        workspaces: Vec<WorkspaceActivity>,
    },
}

impl fmt::Display for CoordinationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyWorkspaceKey => formatter.write_str("workspace identity must not be empty"),
            Self::EmptyWorkspaceSet => {
                formatter.write_str("multi-workspace acquisition requires at least one workspace")
            }
            Self::GlobalExclusiveActive { requested, holder } => {
                write!(
                    formatter,
                    "cannot acquire {requested}: a global exclusive operation is active"
                )?;
                if let Some(conversation_id) = holder.conversation_id.as_deref() {
                    write!(formatter, " for conversation {conversation_id}")?;
                }
                Ok(())
            }
            Self::GlobalSharedActive { requested, holders } => write!(
                formatter,
                "cannot acquire {requested}: {} global shared lease(s) are active",
                holders.iter().map(|entry| entry.lease_count).sum::<usize>()
            ),
            Self::WorkspaceBusy {
                requested,
                workspace,
                holders,
            } => write!(
                formatter,
                "cannot acquire {requested} for {workspace}: the workspace has {} active lease(s)",
                holders.lease_count()
            ),
            Self::WorkspaceOperationsActive {
                requested,
                workspaces,
            } => write!(
                formatter,
                "cannot acquire {requested}: {} workspace(s) have active operations",
                workspaces.len()
            ),
        }
    }
}

impl Error for CoordinationError {}

#[derive(Clone, Debug)]
struct WorkspaceSlot {
    shared: BTreeMap<Option<String>, usize>,
    exclusive: Option<LeaseAttribution>,
}

impl Default for WorkspaceSlot {
    fn default() -> Self {
        Self {
            shared: BTreeMap::new(),
            exclusive: None,
        }
    }
}

impl WorkspaceSlot {
    fn is_idle(&self) -> bool {
        self.shared.is_empty() && self.exclusive.is_none()
    }

    fn holders(&self) -> WorkspaceHolders {
        WorkspaceHolders {
            shared: self
                .shared
                .iter()
                .map(|(conversation_id, lease_count)| AttributedLeaseCount {
                    attribution: LeaseAttribution::new(conversation_id.clone()),
                    lease_count: *lease_count,
                })
                .collect(),
            exclusive: self.exclusive.clone(),
        }
    }
}

#[derive(Default)]
struct CoordinatorState {
    global_shared: BTreeMap<Option<String>, usize>,
    global_exclusive: Option<LeaseAttribution>,
    workspaces: BTreeMap<WorkspaceKey, WorkspaceSlot>,
}

impl CoordinatorState {
    fn global_shared_holders(&self) -> Vec<AttributedLeaseCount> {
        self.global_shared
            .iter()
            .map(|(conversation_id, lease_count)| AttributedLeaseCount {
                attribution: LeaseAttribution::new(conversation_id.clone()),
                lease_count: *lease_count,
            })
            .collect()
    }

    fn workspace_activities(&self) -> Vec<WorkspaceActivity> {
        self.workspaces
            .iter()
            .filter(|(_, slot)| !slot.is_idle())
            .map(|(workspace, slot)| WorkspaceActivity {
                workspace: workspace.clone(),
                holders: slot.holders(),
            })
            .collect()
    }
}

/// Coordinates process-local operations without retaining mutex guards in the
/// returned leases.
#[derive(Clone, Default)]
pub struct OperationCoordinator {
    state: Arc<Mutex<CoordinatorState>>,
}

impl OperationCoordinator {
    /// Tries to acquire a compatibility lease for an operation whose workspace identity has not
    /// yet been projected. Global shared leases coexist with workspace readers, but conservatively
    /// block every workspace/global writer.
    pub fn try_global_shared(
        &self,
        conversation_id: Option<String>,
    ) -> Result<OperationLease, CoordinationError> {
        let attribution = LeaseAttribution::new(conversation_id);
        let mut state = self.lock_state();
        if let Some(holder) = state.global_exclusive.clone() {
            return Err(CoordinationError::GlobalExclusiveActive {
                requested: OperationMode::GlobalShared,
                holder,
            });
        }
        let exclusive_workspaces = state
            .workspaces
            .iter()
            .filter(|(_, slot)| slot.exclusive.is_some())
            .map(|(workspace, slot)| WorkspaceActivity {
                workspace: workspace.clone(),
                holders: slot.holders(),
            })
            .collect::<Vec<_>>();
        if !exclusive_workspaces.is_empty() {
            return Err(CoordinationError::WorkspaceOperationsActive {
                requested: OperationMode::GlobalShared,
                workspaces: exclusive_workspaces,
            });
        }
        *state
            .global_shared
            .entry(attribution.conversation_id.clone())
            .or_insert(0) += 1;
        drop(state);
        Ok(self.new_lease(OperationMode::GlobalShared, Vec::new(), attribution))
    }

    /// Tries to acquire a shared lease for one workspace.
    ///
    /// Shared leases may coexist on the same workspace. They conflict with an
    /// exclusive lease for that workspace and with the global exclusive lease.
    pub fn try_workspace_shared(
        &self,
        workspace: impl Into<WorkspaceKey>,
        conversation_id: Option<String>,
    ) -> Result<OperationLease, CoordinationError> {
        let workspace = workspace.into();
        workspace.validate()?;
        let attribution = LeaseAttribution::new(conversation_id);
        let mut state = self.lock_state();

        if let Some(holder) = state.global_exclusive.clone() {
            return Err(CoordinationError::GlobalExclusiveActive {
                requested: OperationMode::WorkspaceShared,
                holder,
            });
        }
        if let Some(slot) = state.workspaces.get(&workspace) {
            if slot.exclusive.is_some() {
                return Err(CoordinationError::WorkspaceBusy {
                    requested: OperationMode::WorkspaceShared,
                    workspace,
                    holders: slot.holders(),
                });
            }
        }

        *state
            .workspaces
            .entry(workspace.clone())
            .or_default()
            .shared
            .entry(attribution.conversation_id.clone())
            .or_insert(0) += 1;
        drop(state);

        Ok(self.new_lease(OperationMode::WorkspaceShared, vec![workspace], attribution))
    }

    /// Tries to acquire an exclusive lease for one workspace.
    pub fn try_workspace_exclusive(
        &self,
        workspace: impl Into<WorkspaceKey>,
        conversation_id: Option<String>,
    ) -> Result<OperationLease, CoordinationError> {
        self.try_workspaces_exclusive([workspace.into()], conversation_id)
    }

    /// Atomically tries to acquire exclusive leases for all supplied workspaces.
    ///
    /// Keys are validated, deduplicated, and sorted before the state is locked.
    /// Every slot is checked before any slot is changed, so an error never leaves
    /// a partial acquisition behind.
    pub fn try_workspaces_exclusive<I, K>(
        &self,
        workspaces: I,
        conversation_id: Option<String>,
    ) -> Result<OperationLease, CoordinationError>
    where
        I: IntoIterator<Item = K>,
        K: Into<WorkspaceKey>,
    {
        let workspaces = workspaces
            .into_iter()
            .map(Into::into)
            .collect::<BTreeSet<WorkspaceKey>>();
        if workspaces.is_empty() {
            return Err(CoordinationError::EmptyWorkspaceSet);
        }
        for workspace in &workspaces {
            workspace.validate()?;
        }
        let workspaces = workspaces.into_iter().collect::<Vec<_>>();
        let attribution = LeaseAttribution::new(conversation_id);
        let mut state = self.lock_state();

        if let Some(holder) = state.global_exclusive.clone() {
            return Err(CoordinationError::GlobalExclusiveActive {
                requested: OperationMode::WorkspaceExclusive,
                holder,
            });
        }
        if !state.global_shared.is_empty() {
            return Err(CoordinationError::GlobalSharedActive {
                requested: OperationMode::WorkspaceExclusive,
                holders: state.global_shared_holders(),
            });
        }

        for workspace in &workspaces {
            if let Some(slot) = state.workspaces.get(workspace) {
                if !slot.is_idle() {
                    return Err(CoordinationError::WorkspaceBusy {
                        requested: OperationMode::WorkspaceExclusive,
                        workspace: workspace.clone(),
                        holders: slot.holders(),
                    });
                }
            }
        }

        for workspace in &workspaces {
            state
                .workspaces
                .entry(workspace.clone())
                .or_default()
                .exclusive = Some(attribution.clone());
        }
        drop(state);

        Ok(self.new_lease(OperationMode::WorkspaceExclusive, workspaces, attribution))
    }

    /// Tries to acquire the global exclusive lease.
    ///
    /// The global lease conflicts with every workspace lease and with another
    /// global lease.
    pub fn try_global_exclusive(
        &self,
        conversation_id: Option<String>,
    ) -> Result<OperationLease, CoordinationError> {
        let attribution = LeaseAttribution::new(conversation_id);
        let mut state = self.lock_state();

        if let Some(holder) = state.global_exclusive.clone() {
            return Err(CoordinationError::GlobalExclusiveActive {
                requested: OperationMode::GlobalExclusive,
                holder,
            });
        }
        if !state.global_shared.is_empty() {
            return Err(CoordinationError::GlobalSharedActive {
                requested: OperationMode::GlobalExclusive,
                holders: state.global_shared_holders(),
            });
        }
        let active_workspaces = state.workspace_activities();
        if !active_workspaces.is_empty() {
            return Err(CoordinationError::WorkspaceOperationsActive {
                requested: OperationMode::GlobalExclusive,
                workspaces: active_workspaces,
            });
        }

        state.global_exclusive = Some(attribution.clone());
        drop(state);

        Ok(self.new_lease(OperationMode::GlobalExclusive, Vec::new(), attribution))
    }

    #[cfg(test)]
    pub fn snapshot(&self) -> CoordinatorSnapshot {
        let state = self.lock_state();
        CoordinatorSnapshot {
            global_shared: state.global_shared_holders(),
            global_exclusive: state.global_exclusive.clone(),
            workspaces: state.workspace_activities(),
        }
    }

    #[cfg(test)]
    pub fn is_idle(&self) -> bool {
        self.snapshot().is_idle()
    }

    fn lock_state(&self) -> MutexGuard<'_, CoordinatorState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn new_lease(
        &self,
        mode: OperationMode,
        workspaces: Vec<WorkspaceKey>,
        attribution: LeaseAttribution,
    ) -> OperationLease {
        OperationLease {
            state: self.state.clone(),
            descriptor: Some(LeaseDescriptor {
                mode,
                workspaces,
                attribution,
            }),
        }
    }
}

#[derive(Debug)]
struct LeaseDescriptor {
    mode: OperationMode,
    workspaces: Vec<WorkspaceKey>,
    attribution: LeaseAttribution,
}

/// An owned, `Send` RAII lease. Dropping it releases every associated slot.
#[must_use = "the operation lease must remain alive for the complete operation"]
pub struct OperationLease {
    state: Arc<Mutex<CoordinatorState>>,
    descriptor: Option<LeaseDescriptor>,
}

impl OperationLease {
    #[cfg(test)]
    pub fn mode(&self) -> OperationMode {
        self.descriptor
            .as_ref()
            .expect("operation lease descriptor must exist until drop")
            .mode
    }

    #[cfg(test)]
    pub fn workspaces(&self) -> &[WorkspaceKey] {
        &self
            .descriptor
            .as_ref()
            .expect("operation lease descriptor must exist until drop")
            .workspaces
    }

    #[cfg(test)]
    pub fn conversation_id(&self) -> Option<&str> {
        self.descriptor
            .as_ref()
            .expect("operation lease descriptor must exist until drop")
            .attribution
            .conversation_id
            .as_deref()
    }
}

impl fmt::Debug for OperationLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OperationLease")
            .field("descriptor", &self.descriptor)
            .finish_non_exhaustive()
    }
}

impl Drop for OperationLease {
    fn drop(&mut self) {
        let Some(descriptor) = self.descriptor.take() else {
            return;
        };
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        match descriptor.mode {
            OperationMode::GlobalShared => {
                let conversation_id = descriptor.attribution.conversation_id;
                if let Some(count) = state.global_shared.get_mut(&conversation_id) {
                    debug_assert!(*count > 0, "global shared lease counter underflow");
                    *count -= 1;
                    if *count == 0 {
                        state.global_shared.remove(&conversation_id);
                    }
                } else {
                    debug_assert!(false, "global shared attribution was not registered");
                }
            }
            OperationMode::WorkspaceShared => {
                let workspace = descriptor
                    .workspaces
                    .first()
                    .expect("shared lease must contain exactly one workspace");
                let remove_slot = if let Some(slot) = state.workspaces.get_mut(workspace) {
                    let conversation_id = descriptor.attribution.conversation_id;
                    if let Some(count) = slot.shared.get_mut(&conversation_id) {
                        debug_assert!(*count > 0, "shared lease counter underflow");
                        *count -= 1;
                        if *count == 0 {
                            slot.shared.remove(&conversation_id);
                        }
                    } else {
                        debug_assert!(false, "shared lease attribution was not registered");
                    }
                    slot.is_idle()
                } else {
                    debug_assert!(false, "shared lease workspace slot was not registered");
                    false
                };
                if remove_slot {
                    state.workspaces.remove(workspace);
                }
            }
            OperationMode::WorkspaceExclusive => {
                for workspace in &descriptor.workspaces {
                    let remove_slot = if let Some(slot) = state.workspaces.get_mut(workspace) {
                        debug_assert_eq!(
                            slot.exclusive.as_ref(),
                            Some(&descriptor.attribution),
                            "exclusive lease attribution changed before drop"
                        );
                        slot.exclusive = None;
                        slot.is_idle()
                    } else {
                        debug_assert!(false, "exclusive lease workspace slot was not registered");
                        false
                    };
                    if remove_slot {
                        state.workspaces.remove(workspace);
                    }
                }
            }
            OperationMode::GlobalExclusive => {
                debug_assert_eq!(
                    state.global_exclusive.as_ref(),
                    Some(&descriptor.attribution),
                    "global lease attribution changed before drop"
                );
                state.global_exclusive = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc, Barrier,
        },
        thread,
        time::{Duration, Instant},
    };

    fn conversation(id: &str) -> Option<String> {
        Some(id.to_owned())
    }

    #[test]
    fn same_workspace_readers_coexist_and_exclude_writers() {
        let coordinator = OperationCoordinator::default();
        let reader_a = coordinator
            .try_workspace_shared("workspace-a", conversation("conversation-a"))
            .unwrap();
        let reader_b = coordinator
            .try_workspace_shared("workspace-a", conversation("conversation-b"))
            .unwrap();

        let error = coordinator
            .try_workspace_exclusive("workspace-a", conversation("writer"))
            .unwrap_err();
        match error {
            CoordinationError::WorkspaceBusy {
                requested,
                workspace,
                holders,
            } => {
                assert_eq!(requested, OperationMode::WorkspaceExclusive);
                assert_eq!(workspace, WorkspaceKey::from("workspace-a"));
                assert_eq!(holders.lease_count(), 2);
                assert!(holders.exclusive.is_none());
            }
            other => panic!("unexpected error: {other:?}"),
        }

        drop(reader_a);
        drop(reader_b);
        let writer = coordinator
            .try_workspace_exclusive("workspace-a", conversation("writer"))
            .unwrap();
        assert!(matches!(
            coordinator.try_workspace_shared("workspace-a", None),
            Err(CoordinationError::WorkspaceBusy {
                requested: OperationMode::WorkspaceShared,
                ..
            })
        ));
        drop(writer);
        assert!(coordinator.is_idle());
    }

    #[test]
    fn different_workspaces_can_be_held_in_parallel() {
        let coordinator = OperationCoordinator::default();
        let workspace_a = coordinator
            .try_workspace_exclusive("workspace-a", None)
            .unwrap();
        let workspace_b = coordinator
            .try_workspace_exclusive("workspace-b", None)
            .unwrap();
        let workspace_c = coordinator
            .try_workspace_shared("workspace-c", None)
            .unwrap();

        assert_eq!(coordinator.snapshot().active_lease_count(), 3);

        let moved_lease = thread::spawn(move || {
            assert_eq!(workspace_b.mode(), OperationMode::WorkspaceExclusive);
            drop(workspace_b);
        });
        moved_lease.join().unwrap();
        drop(workspace_a);
        drop(workspace_c);
        assert!(coordinator.is_idle());
    }

    #[test]
    fn global_exclusive_conflicts_with_every_operation() {
        let coordinator = OperationCoordinator::default();
        let workspace = coordinator
            .try_workspace_shared("workspace-a", conversation("reader"))
            .unwrap();

        assert!(matches!(
            coordinator.try_global_exclusive(conversation("global")),
            Err(CoordinationError::WorkspaceOperationsActive {
                requested: OperationMode::GlobalExclusive,
                ..
            })
        ));
        drop(workspace);

        let global = coordinator
            .try_global_exclusive(conversation("global"))
            .unwrap();
        assert_eq!(global.conversation_id(), Some("global"));
        assert!(matches!(
            coordinator.try_workspace_shared("workspace-a", None),
            Err(CoordinationError::GlobalExclusiveActive {
                requested: OperationMode::WorkspaceShared,
                ..
            })
        ));
        assert!(matches!(
            coordinator.try_workspaces_exclusive(["workspace-a", "workspace-b"], None),
            Err(CoordinationError::GlobalExclusiveActive {
                requested: OperationMode::WorkspaceExclusive,
                ..
            })
        ));
        assert!(matches!(
            coordinator.try_global_exclusive(None),
            Err(CoordinationError::GlobalExclusiveActive {
                requested: OperationMode::GlobalExclusive,
                ..
            })
        ));

        drop(global);
        assert!(coordinator.is_idle());
    }

    #[test]
    fn global_shared_compatibility_matrix_is_symmetric() {
        let coordinator = OperationCoordinator::default();
        let global_shared = coordinator
            .try_global_shared(conversation("legacy"))
            .unwrap();
        let second_global_shared = coordinator.try_global_shared(None).unwrap();
        let workspace_reader = coordinator
            .try_workspace_shared("workspace-a", conversation("reader"))
            .unwrap();

        assert!(matches!(
            coordinator.try_workspace_exclusive("workspace-b", None),
            Err(CoordinationError::GlobalSharedActive {
                requested: OperationMode::WorkspaceExclusive,
                ..
            })
        ));
        assert!(matches!(
            coordinator.try_global_exclusive(None),
            Err(CoordinationError::GlobalSharedActive {
                requested: OperationMode::GlobalExclusive,
                ..
            })
        ));

        drop(global_shared);
        drop(second_global_shared);
        drop(workspace_reader);

        let workspace_writer = coordinator
            .try_workspace_exclusive("workspace-a", None)
            .unwrap();
        assert!(matches!(
            coordinator.try_global_shared(None),
            Err(CoordinationError::WorkspaceOperationsActive {
                requested: OperationMode::GlobalShared,
                ..
            })
        ));
        drop(workspace_writer);

        let global_writer = coordinator.try_global_exclusive(None).unwrap();
        assert!(matches!(
            coordinator.try_global_shared(None),
            Err(CoordinationError::GlobalExclusiveActive {
                requested: OperationMode::GlobalShared,
                ..
            })
        ));
        drop(global_writer);
        assert!(coordinator.is_idle());
    }

    #[test]
    fn global_shared_snapshot_counts_and_drop_cleanup_are_exact() {
        let coordinator = OperationCoordinator::default();
        let first = coordinator
            .try_global_shared(conversation("conversation-a"))
            .unwrap();
        let second = coordinator
            .try_global_shared(conversation("conversation-a"))
            .unwrap();
        let anonymous = coordinator.try_global_shared(None).unwrap();

        let snapshot = coordinator.snapshot();
        assert_eq!(snapshot.active_lease_count(), 3);
        assert_eq!(
            snapshot.global_shared,
            vec![
                AttributedLeaseCount {
                    attribution: LeaseAttribution::new(None),
                    lease_count: 1,
                },
                AttributedLeaseCount {
                    attribution: LeaseAttribution::new(conversation("conversation-a")),
                    lease_count: 2,
                },
            ]
        );

        drop(first);
        assert_eq!(coordinator.snapshot().active_lease_count(), 2);
        thread::spawn(move || drop(second)).join().unwrap();
        assert_eq!(coordinator.snapshot().active_lease_count(), 1);
        drop(anonymous);
        assert_eq!(coordinator.snapshot(), CoordinatorSnapshot::default());
        assert!(coordinator.is_idle());
    }

    #[test]
    fn multi_workspace_acquisition_is_atomic_sorted_and_deduplicated() {
        let coordinator = OperationCoordinator::default();
        let blocker = coordinator
            .try_workspace_shared("workspace-b", conversation("blocker"))
            .unwrap();

        let error = coordinator
            .try_workspaces_exclusive(
                ["workspace-c", "workspace-a", "workspace-b", "workspace-a"],
                conversation("multi"),
            )
            .unwrap_err();
        assert!(matches!(
            error,
            CoordinationError::WorkspaceBusy {
                workspace,
                ..
            } if workspace == WorkspaceKey::from("workspace-b")
        ));

        // A failed multi-acquire must not leave the earlier sorted keys held.
        drop(
            coordinator
                .try_workspace_exclusive("workspace-a", None)
                .unwrap(),
        );
        drop(
            coordinator
                .try_workspace_exclusive("workspace-c", None)
                .unwrap(),
        );
        drop(blocker);

        let multi = coordinator
            .try_workspaces_exclusive(
                ["workspace-c", "workspace-a", "workspace-b", "workspace-a"],
                conversation("multi"),
            )
            .unwrap();
        assert_eq!(
            multi.workspaces(),
            &[
                WorkspaceKey::from("workspace-a"),
                WorkspaceKey::from("workspace-b"),
                WorkspaceKey::from("workspace-c"),
            ]
        );
        assert_eq!(coordinator.snapshot().workspaces.len(), 3);
        drop(multi);
        assert!(coordinator.is_idle());

        assert!(matches!(
            coordinator.try_workspaces_exclusive(Vec::<WorkspaceKey>::new(), None),
            Err(CoordinationError::EmptyWorkspaceSet)
        ));
    }

    #[test]
    fn drop_cleans_up_after_early_return() {
        fn operation_that_returns_early(
            coordinator: &OperationCoordinator,
        ) -> Result<(), &'static str> {
            let _lease = coordinator
                .try_workspace_exclusive("workspace-a", conversation("early"))
                .unwrap();
            Err("stop")
        }

        let coordinator = OperationCoordinator::default();
        assert_eq!(operation_that_returns_early(&coordinator), Err("stop"));
        assert!(coordinator.is_idle());

        let reacquired = coordinator
            .try_workspace_exclusive("workspace-a", conversation("next"))
            .unwrap();
        drop(reacquired);
        assert!(coordinator.is_idle());
    }

    #[test]
    fn invalid_requests_do_not_leave_residual_slots() {
        let coordinator = OperationCoordinator::default();
        assert!(matches!(
            coordinator.try_workspace_shared("", None),
            Err(CoordinationError::EmptyWorkspaceKey)
        ));
        assert!(matches!(
            coordinator.try_workspaces_exclusive(["workspace-a", ""], None),
            Err(CoordinationError::EmptyWorkspaceKey)
        ));
        assert_eq!(coordinator.snapshot(), CoordinatorSnapshot::default());

        for index in 0..100 {
            let lease = coordinator
                .try_workspace_shared(format!("workspace-{}", index % 5), conversation("loop"))
                .unwrap();
            drop(lease);
        }
        assert_eq!(coordinator.snapshot(), CoordinatorSnapshot::default());
    }

    #[test]
    fn exclusive_contention_stress_preserves_mutual_exclusion_and_cleans_up() {
        const THREADS: usize = 8;
        const ITERATIONS: usize = 250;

        let coordinator = Arc::new(OperationCoordinator::default());
        let active = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(Barrier::new(THREADS));
        let mut workers = Vec::new();

        for thread_index in 0..THREADS {
            let coordinator = coordinator.clone();
            let active = active.clone();
            let barrier = barrier.clone();
            workers.push(thread::spawn(move || {
                barrier.wait();
                for iteration in 0..ITERATIONS {
                    let deadline = Instant::now() + Duration::from_secs(10);
                    let lease = loop {
                        match coordinator.try_workspace_exclusive(
                            "shared-workspace",
                            Some(format!("worker-{thread_index}")),
                        ) {
                            Ok(lease) => break lease,
                            Err(CoordinationError::WorkspaceBusy { .. }) => {
                                assert!(
                                    Instant::now() < deadline,
                                    "timed out retrying a try-only acquisition"
                                );
                                thread::yield_now();
                            }
                            Err(other) => panic!("unexpected coordination error: {other}"),
                        }
                    };

                    assert_eq!(
                        active.fetch_add(1, Ordering::SeqCst),
                        0,
                        "two exclusive leases overlapped"
                    );
                    if iteration % 11 == 0 {
                        thread::yield_now();
                    }
                    assert_eq!(active.fetch_sub(1, Ordering::SeqCst), 1);
                    drop(lease);
                }
            }));
        }

        for worker in workers {
            worker.join().unwrap();
        }
        assert_eq!(active.load(Ordering::SeqCst), 0);
        assert_eq!(coordinator.snapshot(), CoordinatorSnapshot::default());
    }
}
