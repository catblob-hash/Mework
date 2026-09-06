//! Ownership and revocation primitives for Mework's WebView2 browser panel.
//!
//! The panel must claim its host-owned profile directory before startup, and every control handle
//! minted from that claim is revoked when its runtime is torn down.
//!
//! This module deliberately knows nothing about Tauri, WebView2, HTTP endpoints, or bearer tokens.
//! Those remain adapter-owned capabilities and therefore cannot leak across the boundary through a
//! generic runtime registry.

use std::{
    fmt,
    fs::File,
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock},
};

/// Finite, path-free failures suitable for surfacing through adapter-owned error messages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CapabilityError {
    Inactive,
    Unattested,
    PathAlreadyClaimed,
    PathOverlap,
    InvalidOwnedDirectory,
    AttestationMismatch,
    StaleController,
    TeardownInProgress,
    GenerationExhausted,
}

impl fmt::Display for CapabilityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Inactive => "Chromium 运行时能力已撤销",
            Self::Unattested => "Chromium 运行时尚未完成配置目录验证",
            Self::PathAlreadyClaimed => "Chromium 运行时目录已被占用",
            Self::PathOverlap => "Chromium 运行时目录与现有能力边界重叠",
            Self::InvalidOwnedDirectory => "Chromium 运行时目录不属于预期宿主根目录",
            Self::AttestationMismatch => "Chromium 实际配置目录与宿主租约不一致",
            Self::StaleController => "Chromium 原生控制器启动代已过期",
            Self::TeardownInProgress => "Chromium 原生控制器仍在销毁",
            Self::GenerationExhausted => "Chromium 原生控制器启动代已耗尽",
        })
    }
}

impl std::error::Error for CapabilityError {}

/// A canonical, host-owned WebView2 user-data folder.
pub(crate) struct WebView2Profile(OwnedDirectory);

impl WebView2Profile {
    pub(crate) fn within_root(
        directory: &Path,
        expected_root: &Path,
    ) -> Result<Self, CapabilityError> {
        canonical_owned_child(directory, expected_root, true).map(Self)
    }
}

struct ClaimRecord {
    generation: u64,
    directory: PathBuf,
}

#[derive(Default)]
struct ClaimRegistry {
    next_generation: u64,
    claims: Vec<ClaimRecord>,
}

static CLAIMS: OnceLock<Mutex<ClaimRegistry>> = OnceLock::new();

fn claims() -> MutexGuard<'static, ClaimRegistry> {
    CLAIMS
        .get_or_init(|| Mutex::new(ClaimRegistry::default()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

struct LeaseCore {
    generation: u64,
    directory: Arc<PathBuf>,
    directory_guards: Option<DirectoryGuards>,
    gate: Arc<CapabilityGate>,
}

struct CapabilityGateState {
    active: bool,
    in_flight: usize,
    controller_generation: u64,
    callback_generation: u64,
    attested_generation: u64,
    teardown_generation: u64,
}

struct CapabilityGate {
    state: Mutex<CapabilityGateState>,
    drained: Condvar,
}

impl CapabilityGate {
    fn new() -> Self {
        Self {
            state: Mutex::new(CapabilityGateState {
                active: true,
                in_flight: 0,
                controller_generation: 0,
                callback_generation: 0,
                attested_generation: 0,
                teardown_generation: 0,
            }),
            drained: Condvar::new(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, CapabilityGateState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn wait_until_drained<'a>(
        &self,
        mut state: MutexGuard<'a, CapabilityGateState>,
    ) -> MutexGuard<'a, CapabilityGateState> {
        while state.in_flight != 0 {
            state = self
                .drained
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        state
    }

    fn ensure_active(&self) -> Result<(), CapabilityError> {
        if self.lock().active {
            Ok(())
        } else {
            Err(CapabilityError::Inactive)
        }
    }

    fn begin_controller(&self) -> Result<u64, CapabilityError> {
        let mut state = self.lock();
        if !state.active {
            return Err(CapabilityError::Inactive);
        }
        if state.teardown_generation != 0 {
            return Err(CapabilityError::TeardownInProgress);
        }
        let generation = state
            .controller_generation
            .checked_add(1)
            .ok_or(CapabilityError::GenerationExhausted)?;
        let callback_generation = state
            .callback_generation
            .checked_add(1)
            .ok_or(CapabilityError::GenerationExhausted)?;
        // Invalidate the old generation before waiting for its already-issued operations. No new
        // old-generation permit can enter while the transition drains.
        state.controller_generation = generation;
        state.callback_generation = callback_generation;
        state.attested_generation = 0;
        state.teardown_generation = 0;
        let state = self.wait_until_drained(state);
        if !state.active {
            return Err(CapabilityError::Inactive);
        }
        if state.controller_generation != generation {
            return Err(CapabilityError::StaleController);
        }
        Ok(generation)
    }

    fn begin_attestation(
        self: &Arc<Self>,
        generation: u64,
    ) -> Result<RuntimePermit, CapabilityError> {
        let mut state = self.lock();
        if !state.active {
            return Err(CapabilityError::Inactive);
        }
        if state.controller_generation != generation {
            return Err(CapabilityError::StaleController);
        }
        state.callback_generation = state
            .callback_generation
            .checked_add(1)
            .ok_or(CapabilityError::GenerationExhausted)?;
        // A failed re-attestation must never inherit authority from this controller's earlier
        // successful check.
        state.attested_generation = 0;
        let mut state = self.wait_until_drained(state);
        if !state.active {
            return Err(CapabilityError::Inactive);
        }
        if state.controller_generation != generation {
            return Err(CapabilityError::StaleController);
        }
        // A concurrent attestation may have completed while this caller waited for its permit to
        // drain. Clear that result again before publishing this independently fenced attempt.
        state.attested_generation = 0;
        state.callback_generation = state
            .callback_generation
            .checked_add(1)
            .ok_or(CapabilityError::GenerationExhausted)?;
        state.in_flight += 1;
        drop(state);
        Ok(RuntimePermit {
            gate: Arc::clone(self),
        })
    }

    fn complete_attestation(&self, generation: u64) -> Result<(), CapabilityError> {
        let mut state = self.lock();
        if !state.active {
            return Err(CapabilityError::Inactive);
        }
        if state.controller_generation != generation {
            return Err(CapabilityError::StaleController);
        }
        state.attested_generation = generation;
        Ok(())
    }

    fn acquire_controller(
        self: &Arc<Self>,
        generation: u64,
        expected_callback_generation: Option<u64>,
    ) -> Result<(RuntimePermit, u64), CapabilityError> {
        let mut state = self.lock();
        if !state.active {
            return Err(CapabilityError::Inactive);
        }
        if state.controller_generation != generation {
            return Err(CapabilityError::StaleController);
        }
        if expected_callback_generation
            .is_some_and(|expected| expected != state.callback_generation)
        {
            return Err(CapabilityError::StaleController);
        }
        if state.attested_generation != generation {
            return Err(CapabilityError::Unattested);
        }
        state.in_flight += 1;
        let callback_generation = state.callback_generation;
        drop(state);
        Ok((
            RuntimePermit {
                gate: Arc::clone(self),
            },
            callback_generation,
        ))
    }

    fn mint_release_observer(
        self: &Arc<Self>,
        generation: u64,
    ) -> Result<WebView2ReleaseObserverPermit, CapabilityError> {
        let state = self.lock();
        if !state.active {
            return Err(CapabilityError::Inactive);
        }
        if state.controller_generation != generation {
            return Err(CapabilityError::StaleController);
        }
        if state.attested_generation != generation {
            return Err(CapabilityError::Unattested);
        }
        Ok(WebView2ReleaseObserverPermit {
            _gate: Arc::clone(self),
            _generation: generation,
        })
    }

    fn invalidate_controller(
        self: &Arc<Self>,
        generation: u64,
    ) -> Result<WebView2TeardownPermit, CapabilityError> {
        let mut state = self.lock();
        if !state.active {
            return Err(CapabilityError::Inactive);
        }
        if state.controller_generation != generation {
            // A stale control is already unable to authorize operations and must never receive a
            // teardown token that could target whichever newer generation replaced it.
            return Err(CapabilityError::StaleController);
        }
        let controller_generation = state
            .controller_generation
            .checked_add(1)
            .ok_or(CapabilityError::GenerationExhausted)?;
        let callback_generation = state
            .callback_generation
            .checked_add(1)
            .ok_or(CapabilityError::GenerationExhausted)?;
        state.controller_generation = controller_generation;
        state.callback_generation = callback_generation;
        state.attested_generation = 0;
        state.teardown_generation = generation;
        let state = self.wait_until_drained(state);
        if !state.active {
            return Err(CapabilityError::Inactive);
        }
        if state.teardown_generation != generation {
            return Err(CapabilityError::StaleController);
        }
        Ok(WebView2TeardownPermit {
            inner: Arc::new(WebView2TeardownPermitCore {
                gate: Arc::clone(self),
                generation,
            }),
        })
    }

    fn ensure_teardown(&self, generation: u64) -> Result<(), CapabilityError> {
        let state = self.lock();
        if !state.active {
            return Err(CapabilityError::Inactive);
        }
        if state.teardown_generation != generation {
            return Err(CapabilityError::StaleController);
        }
        Ok(())
    }

    fn finish_teardown(&self, generation: u64) {
        let mut state = self.lock();
        if state.teardown_generation == generation {
            state.teardown_generation = 0;
            self.drained.notify_all();
        }
    }

    fn revoke_and_wait(&self) -> bool {
        let mut state = self.lock();
        if !state.active {
            return false;
        }
        state.active = false;
        state.attested_generation = 0;
        state.teardown_generation = 0;
        let _state = self.wait_until_drained(state);
        true
    }
}

struct RuntimePermit {
    gate: Arc<CapabilityGate>,
}

impl Drop for RuntimePermit {
    fn drop(&mut self) {
        let mut state = self.gate.lock();
        debug_assert!(state.in_flight > 0);
        state.in_flight = state.in_flight.saturating_sub(1);
        if state.in_flight == 0 {
            self.gate.drained.notify_all();
        }
    }
}

impl LeaseCore {
    fn claim(owned: OwnedDirectory) -> Result<Self, CapabilityError> {
        let OwnedDirectory {
            canonical: directory,
            guards,
        } = owned;
        let mut registry = claims();
        for existing in &registry.claims {
            if canonical_paths_equal(&existing.directory, &directory) {
                return Err(CapabilityError::PathAlreadyClaimed);
            }
            if canonical_path_starts_with(&existing.directory, &directory)
                || canonical_path_starts_with(&directory, &existing.directory)
            {
                return Err(CapabilityError::PathOverlap);
            }
        }
        registry.next_generation = registry.next_generation.wrapping_add(1).max(1);
        let generation = registry.next_generation;
        registry.claims.push(ClaimRecord {
            generation,
            directory: directory.clone(),
        });
        drop(registry);
        Ok(Self {
            generation,
            directory: Arc::new(directory),
            directory_guards: Some(guards),
            gate: Arc::new(CapabilityGate::new()),
        })
    }

    fn ensure_active(&self) -> Result<(), CapabilityError> {
        self.gate.ensure_active()
    }

    fn deactivate(&self) {
        self.gate.revoke_and_wait();
    }

    fn revoke(&mut self) {
        self.deactivate();
        if self.directory_guards.take().is_none() {
            return;
        }
        // Windows guards always deny deletion/rename; profile/temp guards additionally deny
        // write/reparse mutation. Release them only after adapter-owned cleanup, while the
        // registry entry still prevents another in-process claimant from entering.
        let mut registry = claims();
        registry.claims.retain(|claim| {
            claim.generation != self.generation
                || !canonical_paths_equal(&claim.directory, self.directory.as_path())
        });
    }
}

impl Drop for LeaseCore {
    fn drop(&mut self) {
        self.revoke();
    }
}

/// Exclusive ownership of one tab's WebView2 profile directory.
pub(crate) struct WebView2RuntimeLease {
    core: LeaseCore,
}

impl WebView2RuntimeLease {
    pub(crate) fn claim(profile: WebView2Profile) -> Result<Self, CapabilityError> {
        Ok(Self {
            core: LeaseCore::claim(profile.0)?,
        })
    }

    pub(crate) fn directory(&self) -> &Path {
        self.core.directory.as_path()
    }

    pub(crate) fn ensure_active(&self) -> Result<(), CapabilityError> {
        self.core.ensure_active()
    }

    pub(crate) fn controller_issuer(&self) -> WebView2ControllerIssuer {
        WebView2ControllerIssuer {
            gate: Arc::clone(&self.core.gate),
            expected_directory: Arc::clone(&self.core.directory),
        }
    }

    pub(crate) fn revoke(&mut self) {
        self.core.revoke();
    }
}

/// Adapter-owned authority that mints exactly one new, independently attested controller
/// generation at a time. It is intentionally distinct from operation controls so an old control
/// clone cannot mint or reactivate itself.
#[derive(Clone)]
pub(crate) struct WebView2ControllerIssuer {
    gate: Arc<CapabilityGate>,
    expected_directory: Arc<PathBuf>,
}

impl WebView2ControllerIssuer {
    pub(crate) fn begin_controller(&self) -> Result<WebView2Control, CapabilityError> {
        let generation = self.gate.begin_controller()?;
        Ok(WebView2Control {
            gate: Arc::clone(&self.gate),
            generation,
            expected_directory: Arc::clone(&self.expected_directory),
        })
    }
}

/// Cloneable authority for WebView2-native operations. A control remains bound to the exact claim
/// generation and becomes unusable as soon as its owning lease is revoked.
#[derive(Clone)]
pub(crate) struct WebView2Control {
    gate: Arc<CapabilityGate>,
    generation: u64,
    expected_directory: Arc<PathBuf>,
}

impl WebView2Control {
    pub(crate) fn begin_attestation(&self) -> Result<WebView2AttestationPermit, CapabilityError> {
        Ok(WebView2AttestationPermit {
            _permit: self.gate.begin_attestation(self.generation)?,
            gate: Arc::clone(&self.gate),
            generation: self.generation,
            expected_directory: Arc::clone(&self.expected_directory),
        })
    }

    #[cfg_attr(
        all(windows, not(test)),
        expect(
            dead_code,
            reason = "Windows production attestation owns a pre-dispatch permit; this shorthand remains for tests and non-Windows binding"
        )
    )]
    pub(crate) fn verify_attested_user_data_folder(
        &self,
        actual: &Path,
    ) -> Result<(), CapabilityError> {
        self.begin_attestation()?
            .verify_attested_user_data_folder(actual)
    }

    pub(crate) fn permit(&self) -> Result<WebView2Permit, CapabilityError> {
        let (permit, callback_generation) = self.gate.acquire_controller(self.generation, None)?;
        Ok(WebView2Permit {
            _permit: Arc::new(permit),
            gate: Arc::clone(&self.gate),
            generation: self.generation,
            callback_generation,
        })
    }

    /// Mints a sealed observer authority for the browser-dev release barrier. It intentionally
    /// does not count as in-flight work: the process-exit event can only arrive after controller
    /// teardown, so making it block invalidation would deadlock the release it observes.
    #[cfg_attr(
        not(all(windows, feature = "browser-dev")),
        allow(
            dead_code,
            reason = "used only by the Windows browser-dev release barrier"
        )
    )]
    pub(crate) fn release_observer(
        &self,
    ) -> Result<WebView2ReleaseObserverPermit, CapabilityError> {
        self.gate.mint_release_observer(self.generation)
    }

    pub(crate) fn invalidate(&self) -> Result<WebView2TeardownPermit, CapabilityError> {
        self.gate.invalidate_controller(self.generation)
    }

    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "test-only shorthand; production holds an owned page operation permit"
        )
    )]
    pub(crate) fn ensure_active(&self) -> Result<(), CapabilityError> {
        drop(self.permit()?);
        Ok(())
    }
}

/// The sole pre-attestation authority for one controller generation. It is acquired before a raw
/// `with_webview` dispatch and remains in-flight until that queued closure either verifies the
/// actual UserDataFolder or is dropped. Normal page permits cannot be minted while it exists.
pub(crate) struct WebView2AttestationPermit {
    _permit: RuntimePermit,
    gate: Arc<CapabilityGate>,
    generation: u64,
    expected_directory: Arc<PathBuf>,
}

impl WebView2AttestationPermit {
    pub(crate) fn verify_attested_user_data_folder(
        &self,
        actual: &Path,
    ) -> Result<(), CapabilityError> {
        let actual =
            std::fs::canonicalize(actual).map_err(|_| CapabilityError::AttestationMismatch)?;
        if !canonical_paths_equal(&actual, self.expected_directory.as_path()) {
            return Err(CapabilityError::AttestationMismatch);
        }
        self.gate.complete_attestation(self.generation)
    }
}

#[derive(Clone)]
pub(crate) struct WebView2Permit {
    _permit: Arc<RuntimePermit>,
    gate: Arc<CapabilityGate>,
    generation: u64,
    callback_generation: u64,
}

impl WebView2Permit {
    /// Creates a non-blocking continuation token for a native completion callback. Merely
    /// registering or retaining the callback must not keep controller teardown waiting forever:
    /// WebView2 and page JavaScript are allowed to never invoke it. When the callback does run it
    /// atomically reacquires a normal permit for this exact generation, or fails closed after
    /// invalidation.
    pub(crate) fn callback_token(&self) -> WebView2CallbackToken {
        WebView2CallbackToken {
            gate: Arc::clone(&self.gate),
            generation: self.generation,
            callback_generation: self.callback_generation,
        }
    }
}

/// Revocable, non-blocking authority retained by an asynchronous WebView2 callback. It holds no
/// in-flight count while dormant; `permit` fences only the callback body that is actually running.
#[derive(Clone)]
pub(crate) struct WebView2CallbackToken {
    gate: Arc<CapabilityGate>,
    generation: u64,
    callback_generation: u64,
}

impl WebView2CallbackToken {
    pub(crate) fn permit(&self) -> Result<WebView2Permit, CapabilityError> {
        let (permit, callback_generation) = self
            .gate
            .acquire_controller(self.generation, Some(self.callback_generation))?;
        Ok(WebView2Permit {
            _permit: Arc::new(permit),
            gate: Arc::clone(&self.gate),
            generation: self.generation,
            callback_generation,
        })
    }
}

/// One controller generation's release-only authority. It is minted only after normal authority
/// has been invalidated and every in-flight operation has drained. Holding it cannot authorize a
/// page operation; it only proves that an adapter may close/destroy and await this exact surface.
#[derive(Clone)]
pub(crate) struct WebView2TeardownPermit {
    inner: Arc<WebView2TeardownPermitCore>,
}

struct WebView2TeardownPermitCore {
    gate: Arc<CapabilityGate>,
    generation: u64,
}

impl WebView2TeardownPermit {
    pub(crate) fn ensure_active(&self) -> Result<(), CapabilityError> {
        self.inner.gate.ensure_teardown(self.inner.generation)
    }
}

impl Drop for WebView2TeardownPermitCore {
    fn drop(&mut self) {
        self.gate.finish_teardown(self.generation);
    }
}

/// Sealed, release-only authority for the browser-dev WebView2 process-exit event. The event owns
/// this token from registration through native observation/unregistration, but the token exposes
/// no general controller operation and intentionally does not block teardown.
#[cfg_attr(
    not(all(windows, feature = "browser-dev")),
    allow(
        dead_code,
        reason = "used only by the Windows browser-dev release barrier"
    )
)]
pub(crate) struct WebView2ReleaseObserverPermit {
    _gate: Arc<CapabilityGate>,
    _generation: u64,
}

struct OwnedDirectory {
    canonical: PathBuf,
    guards: DirectoryGuards,
}

struct DirectoryGuards {
    _root: File,
    _directory: File,
}

fn canonical_owned_child(
    directory: &Path,
    expected_root: &Path,
    deny_directory_mutation: bool,
) -> Result<OwnedDirectory, CapabilityError> {
    // Open the identity guards before inspecting path metadata. On Windows the first successful
    // open already denies rename/delete and, for profile/temp objects, write/reparse mutation,
    // eliminating the inspect-then-open window. A second handle proves the path resolves to the exact
    // retained object rather than merely another ordinary directory.
    let root_guard = open_directory_guard(expected_root, false)?;
    let directory_guard = open_directory_guard(directory, deny_directory_mutation)?;
    let root_verifier = open_directory_guard(expected_root, false)?;
    let directory_verifier = open_directory_guard(directory, deny_directory_mutation)?;
    if !directory_guard_identities_equal(&root_guard, &root_verifier)
        || !directory_guard_identities_equal(&directory_guard, &directory_verifier)
    {
        return Err(CapabilityError::InvalidOwnedDirectory);
    }

    let raw_root_metadata = std::fs::symlink_metadata(expected_root)
        .map_err(|_| CapabilityError::InvalidOwnedDirectory)?;
    if !raw_root_metadata.is_dir() || metadata_is_link_or_reparse(&raw_root_metadata) {
        return Err(CapabilityError::InvalidOwnedDirectory);
    }
    let raw_directory_metadata =
        std::fs::symlink_metadata(directory).map_err(|_| CapabilityError::InvalidOwnedDirectory)?;
    if !raw_directory_metadata.is_dir() || metadata_is_link_or_reparse(&raw_directory_metadata) {
        return Err(CapabilityError::InvalidOwnedDirectory);
    }

    let guarded_root_metadata = root_guard
        .metadata()
        .map_err(|_| CapabilityError::InvalidOwnedDirectory)?;
    let guarded_directory_metadata = directory_guard
        .metadata()
        .map_err(|_| CapabilityError::InvalidOwnedDirectory)?;
    if !guarded_root_metadata.is_dir()
        || metadata_is_link_or_reparse(&guarded_root_metadata)
        || !guarded_directory_metadata.is_dir()
        || metadata_is_link_or_reparse(&guarded_directory_metadata)
    {
        return Err(CapabilityError::InvalidOwnedDirectory);
    }

    let root =
        std::fs::canonicalize(expected_root).map_err(|_| CapabilityError::InvalidOwnedDirectory)?;

    let directory =
        std::fs::canonicalize(directory).map_err(|_| CapabilityError::InvalidOwnedDirectory)?;
    if !directory
        .parent()
        .is_some_and(|parent| canonical_paths_equal(parent, &root))
    {
        return Err(CapabilityError::InvalidOwnedDirectory);
    }
    Ok(OwnedDirectory {
        canonical: directory,
        guards: DirectoryGuards {
            _root: root_guard,
            _directory: directory_guard,
        },
    })
}

#[cfg(windows)]
fn open_directory_guard(
    path: &Path,
    deny_directory_mutation: bool,
) -> Result<File, CapabilityError> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };

    std::fs::OpenOptions::new()
        .read(true)
        // The profile itself is immutable while Chromium may traverse it. Parent roots remain
        // writable for child lifecycle operations, but still refuse delete sharing so their
        // identity cannot be replaced.
        .share_mode(if deny_directory_mutation {
            FILE_SHARE_READ
        } else {
            FILE_SHARE_READ | FILE_SHARE_WRITE
        })
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .map_err(|_| CapabilityError::InvalidOwnedDirectory)
}

#[cfg(not(windows))]
fn open_directory_guard(
    path: &Path,
    _deny_directory_mutation: bool,
) -> Result<File, CapabilityError> {
    File::open(path).map_err(|_| CapabilityError::InvalidOwnedDirectory)
}

#[cfg(windows)]
fn canonical_paths_equal(left: &Path, right: &Path) -> bool {
    left.to_string_lossy()
        .eq_ignore_ascii_case(&right.to_string_lossy())
}

#[cfg(not(windows))]
fn canonical_paths_equal(left: &Path, right: &Path) -> bool {
    left == right
}

#[cfg(windows)]
fn canonical_path_starts_with(path: &Path, base: &Path) -> bool {
    let mut path_components = path.components();
    base.components().all(|base_component| {
        path_components.next().is_some_and(|path_component| {
            path_component
                .as_os_str()
                .to_string_lossy()
                .eq_ignore_ascii_case(&base_component.as_os_str().to_string_lossy())
        })
    })
}

#[cfg(not(windows))]
fn canonical_path_starts_with(path: &Path, base: &Path) -> bool {
    path.starts_with(base)
}

#[cfg(windows)]
fn metadata_is_link_or_reparse(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    metadata.file_type().is_symlink()
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn metadata_is_link_or_reparse(metadata: &std::fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

#[cfg(windows)]
fn directory_guard_identities_equal(left: &File, right: &File) -> bool {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    };

    fn identity(file: &File) -> Option<(u32, u64)> {
        let mut information = std::mem::MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::zeroed();
        let succeeded =
            unsafe { GetFileInformationByHandle(file.as_raw_handle(), information.as_mut_ptr()) };
        if succeeded == 0 {
            return None;
        }
        let information = unsafe { information.assume_init() };
        Some((
            information.dwVolumeSerialNumber,
            ((information.nFileIndexHigh as u64) << 32) | information.nFileIndexLow as u64,
        ))
    }

    identity(left).is_some_and(|left| Some(left) == identity(right))
}

#[cfg(unix)]
fn directory_guard_identities_equal(left: &File, right: &File) -> bool {
    use std::os::unix::fs::MetadataExt;

    match (left.metadata(), right.metadata()) {
        (Ok(left), Ok(right)) => left.dev() == right.dev() && left.ino() == right.ino(),
        _ => false,
    }
}

#[cfg(not(any(windows, unix)))]
fn directory_guard_identities_equal(_left: &File, _right: &File) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn child(root: &Path, name: &str) -> PathBuf {
        let path = root.join(name);
        std::fs::create_dir_all(&path).expect("owned directory");
        path
    }

    #[test]
    fn distinct_leases_claim_sibling_directories_under_one_owned_root() {
        let tree = tempfile::tempdir().expect("temp tree");
        let shared_root = child(tree.path(), "chromium-root");
        let browser = WebView2RuntimeLease::claim(
            WebView2Profile::within_root(&child(&shared_root, "tab"), &shared_root).unwrap(),
        )
        .unwrap();
        let other_browser = WebView2RuntimeLease::claim(
            WebView2Profile::within_root(&child(&shared_root, "other-tab"), &shared_root).unwrap(),
        )
        .unwrap();

        browser.ensure_active().unwrap();
        other_browser.ensure_active().unwrap();
        assert_ne!(browser.directory(), other_browser.directory());
    }

    #[test]
    fn duplicate_and_overlapping_claims_fail_closed() {
        let tree = tempfile::tempdir().expect("temp tree");
        let root = child(tree.path(), "root");
        let claimed = child(&root, "claimed");
        let browser =
            WebView2RuntimeLease::claim(WebView2Profile::within_root(&claimed, &root).unwrap())
                .unwrap();

        let duplicate = WebView2RuntimeLease::claim(
            WebView2Profile::within_root(&claimed, &root).unwrap(),
        )
        .err()
        .expect("same canonical path must not be shared");
        assert_eq!(duplicate, CapabilityError::PathAlreadyClaimed);

        let nested = child(&claimed, "nested");
        let overlap = WebView2RuntimeLease::claim(
            WebView2Profile::within_root(&nested, &claimed).unwrap(),
        )
        .err()
        .expect("ancestor and descendant claims must not coexist");
        assert_eq!(overlap, CapabilityError::PathOverlap);
        drop(browser);
    }

    #[test]
    fn explicit_revoke_and_drop_invalidate_old_controls_and_release_claims() {
        let tree = tempfile::tempdir().expect("temp tree");
        let root = child(tree.path(), "root");
        let directory = child(&root, "profile");
        let mut first = WebView2RuntimeLease::claim(
            WebView2Profile::within_root(&directory, &root).unwrap(),
        )
        .unwrap();
        let stale = first
            .controller_issuer()
            .begin_controller()
            .expect("first controller");
        stale
            .verify_attested_user_data_folder(&directory)
            .expect("first attestation");
        first.revoke();
        assert_eq!(stale.ensure_active(), Err(CapabilityError::Inactive));

        let second = WebView2RuntimeLease::claim(
            WebView2Profile::within_root(&directory, &root).unwrap(),
        )
        .expect("explicit revoke releases the claim");
        let second_control = second
            .controller_issuer()
            .begin_controller()
            .expect("second controller");
        second_control
            .verify_attested_user_data_folder(&directory)
            .expect("second attestation");
        drop(second);
        assert_eq!(
            second_control.ensure_active(),
            Err(CapabilityError::Inactive)
        );
    }

    #[test]
    fn webview2_controller_generations_require_independent_attestation_and_never_reactivate() {
        let tree = tempfile::tempdir().expect("temp tree");
        let root = child(tree.path(), "root");
        let expected = child(&root, "expected");
        let other = child(&root, "other");
        let mut lease =
            WebView2RuntimeLease::claim(WebView2Profile::within_root(&expected, &root).unwrap())
                .unwrap();
        let issuer = lease.controller_issuer();
        let first = issuer.begin_controller().expect("first controller");
        assert_eq!(first.ensure_active(), Err(CapabilityError::Unattested));
        assert_eq!(
            first.verify_attested_user_data_folder(&other),
            Err(CapabilityError::AttestationMismatch)
        );
        first
            .verify_attested_user_data_folder(&expected)
            .expect("exact actual folder attests");
        first.ensure_active().unwrap();

        let replacement = issuer
            .begin_controller()
            .expect("replacement gets a new generation");
        assert_eq!(
            first.ensure_active(),
            Err(CapabilityError::StaleController),
            "an old control must not revive with a replacement controller"
        );
        assert_eq!(
            replacement.ensure_active(),
            Err(CapabilityError::Unattested)
        );
        replacement
            .verify_attested_user_data_folder(&expected)
            .expect("the replacement controller re-attests independently");
        assert_eq!(first.ensure_active(), Err(CapabilityError::StaleController));
        replacement.ensure_active().unwrap();
        lease.revoke();
        assert_eq!(replacement.ensure_active(), Err(CapabilityError::Inactive));
    }

    #[test]
    fn teardown_authority_is_release_only_and_bound_to_one_invalidated_generation() {
        let tree = tempfile::tempdir().expect("temp tree");
        let root = child(tree.path(), "root");
        let directory = child(&root, "profile");
        let mut lease = WebView2RuntimeLease::claim(
            WebView2Profile::within_root(&directory, &root).expect("owned profile"),
        )
        .expect("profile lease");
        let issuer = lease.controller_issuer();
        let control = issuer.begin_controller().expect("first controller");
        control
            .verify_attested_user_data_folder(&directory)
            .expect("first attestation");
        let observer = control
            .release_observer()
            .expect("attested generation may mint a release observer");

        let teardown = control
            .invalidate()
            .expect("invalidation mints release-only authority");
        teardown.ensure_active().unwrap();
        assert_eq!(
            control.ensure_active(),
            Err(CapabilityError::StaleController)
        );
        drop(observer);

        assert!(matches!(
            issuer.begin_controller(),
            Err(CapabilityError::TeardownInProgress)
        ));
        drop(teardown);
        let replacement = issuer.begin_controller().expect("replacement controller");
        replacement
            .verify_attested_user_data_folder(&directory)
            .expect("replacement attestation");
        let replacement_teardown = replacement.invalidate().expect("replacement teardown");
        replacement_teardown.ensure_active().unwrap();
        lease.revoke();
        assert_eq!(
            replacement_teardown.ensure_active(),
            Err(CapabilityError::Inactive)
        );
    }

    #[test]
    fn pre_attestation_dispatch_blocks_teardown_and_cannot_attest_after_invalidation() {
        let tree = tempfile::tempdir().expect("temp tree");
        let root = child(tree.path(), "root");
        let directory = child(&root, "profile");
        let lease = WebView2RuntimeLease::claim(
            WebView2Profile::within_root(&directory, &root).expect("owned profile"),
        )
        .expect("profile lease");
        let control = lease
            .controller_issuer()
            .begin_controller()
            .expect("controller");
        let attestation = control.begin_attestation().expect("attestation dispatch");
        let (result_tx, result_rx) = std::sync::mpsc::channel();
        let invalidating_control = control.clone();
        let invalidator = std::thread::spawn(move || {
            result_tx.send(invalidating_control.invalidate()).unwrap();
        });

        while control.gate.lock().controller_generation == control.generation {
            std::thread::yield_now();
        }
        assert_eq!(
            attestation.verify_attested_user_data_folder(&directory),
            Err(CapabilityError::StaleController),
            "a late queued attestation must not revive an invalidated generation"
        );
        assert!(
            result_rx
                .recv_timeout(std::time::Duration::from_millis(50))
                .is_err(),
            "teardown crossed the still-live pre-attestation dispatch"
        );
        drop(attestation);
        let teardown = result_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("teardown did not resume after attestation dispatch drained")
            .expect("teardown authority");
        invalidator.join().unwrap();
        teardown.ensure_active().unwrap();
    }

    #[test]
    fn cloned_page_permit_keeps_async_tail_inside_generation_drain() {
        let tree = tempfile::tempdir().expect("temp tree");
        let root = child(tree.path(), "root");
        let directory = child(&root, "profile");
        let lease = WebView2RuntimeLease::claim(
            WebView2Profile::within_root(&directory, &root).expect("owned profile"),
        )
        .expect("profile lease");
        let control = lease
            .controller_issuer()
            .begin_controller()
            .expect("controller");
        control
            .verify_attested_user_data_folder(&directory)
            .expect("attestation");
        let caller_permit = control.permit().expect("caller permit");
        let async_tail = caller_permit.clone();
        let (result_tx, result_rx) = std::sync::mpsc::channel();
        let invalidating_control = control.clone();
        let invalidator = std::thread::spawn(move || {
            result_tx.send(invalidating_control.invalidate()).unwrap();
        });

        while control.gate.lock().controller_generation == control.generation {
            std::thread::yield_now();
        }
        drop(caller_permit);
        assert!(
            result_rx
                .recv_timeout(std::time::Duration::from_millis(50))
                .is_err(),
            "teardown crossed an async tail after its caller returned"
        );
        drop(async_tail);
        let teardown = result_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("teardown did not resume after async tail drained")
            .expect("teardown authority");
        invalidator.join().unwrap();
        teardown.ensure_active().unwrap();
    }

    #[test]
    fn dormant_callback_token_never_blocks_teardown_and_cannot_publish_after_invalidation() {
        let tree = tempfile::tempdir().expect("temp tree");
        let root = child(tree.path(), "root");
        let directory = child(&root, "profile");
        let lease = WebView2RuntimeLease::claim(
            WebView2Profile::within_root(&directory, &root).expect("owned profile"),
        )
        .expect("profile lease");
        let control = lease
            .controller_issuer()
            .begin_controller()
            .expect("controller");
        control
            .verify_attested_user_data_folder(&directory)
            .expect("attestation");
        let callback = control
            .permit()
            .expect("registration permit")
            .callback_token();

        let teardown = control
            .invalidate()
            .expect("a never-fired callback does not block invalidation");
        teardown.ensure_active().unwrap();
        assert!(matches!(
            callback.permit(),
            Err(CapabilityError::StaleController)
        ));
    }

    #[test]
    fn running_callback_reacquires_only_a_short_generation_fence() {
        let tree = tempfile::tempdir().expect("temp tree");
        let root = child(tree.path(), "root");
        let directory = child(&root, "profile");
        let lease = WebView2RuntimeLease::claim(
            WebView2Profile::within_root(&directory, &root).expect("owned profile"),
        )
        .expect("profile lease");
        let control = lease
            .controller_issuer()
            .begin_controller()
            .expect("controller");
        control
            .verify_attested_user_data_folder(&directory)
            .expect("attestation");
        let registration = control.permit().expect("registration permit");
        let callback = registration.callback_token();
        drop(registration);
        let running_callback = callback
            .permit()
            .expect("callback starts before invalidation");

        let (result_tx, result_rx) = std::sync::mpsc::channel();
        let invalidating_control = control.clone();
        let invalidator = std::thread::spawn(move || {
            result_tx.send(invalidating_control.invalidate()).unwrap();
        });
        while control.gate.lock().controller_generation == control.generation {
            std::thread::yield_now();
        }
        assert!(
            result_rx
                .recv_timeout(std::time::Duration::from_millis(50))
                .is_err(),
            "teardown crossed a callback body that had already reacquired authority"
        );
        drop(running_callback);
        let teardown = result_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("teardown did not resume after the callback body finished")
            .expect("teardown authority");
        invalidator.join().unwrap();
        teardown.ensure_active().unwrap();
        assert!(matches!(
            callback.permit(),
            Err(CapabilityError::StaleController)
        ));
    }

    #[test]
    fn callback_tokens_are_bound_to_one_attestation_epoch() {
        let tree = tempfile::tempdir().expect("temp tree");
        let root = child(tree.path(), "root");
        let directory = child(&root, "profile");
        let other = child(&root, "other");
        let lease = WebView2RuntimeLease::claim(
            WebView2Profile::within_root(&directory, &root).expect("owned profile"),
        )
        .expect("profile lease");
        let control = lease
            .controller_issuer()
            .begin_controller()
            .expect("controller");
        control
            .verify_attested_user_data_folder(&directory)
            .expect("first attestation");
        let old_callback = control.permit().expect("first permit").callback_token();

        let reattestation = control.begin_attestation().expect("reattest dispatch");
        assert!(matches!(
            old_callback.permit(),
            Err(CapabilityError::StaleController)
        ));
        reattestation
            .verify_attested_user_data_folder(&directory)
            .expect("second attestation");
        assert!(matches!(
            old_callback.permit(),
            Err(CapabilityError::StaleController)
        ));
        drop(reattestation);

        let current_callback = control
            .permit()
            .expect("new attestation permit")
            .callback_token();
        drop(current_callback.permit().expect("current callback works"));

        let failed = control
            .begin_attestation()
            .expect("failed reattest dispatch");
        assert_eq!(
            failed.verify_attested_user_data_folder(&other),
            Err(CapabilityError::AttestationMismatch)
        );
        assert!(matches!(
            current_callback.permit(),
            Err(CapabilityError::StaleController)
        ));
        drop(failed);
        assert_eq!(control.ensure_active(), Err(CapabilityError::Unattested));
        assert!(matches!(
            current_callback.permit(),
            Err(CapabilityError::StaleController)
        ));
    }

    #[test]
    fn replacement_controller_waits_for_in_flight_old_generation_operations() {
        let tree = tempfile::tempdir().expect("temp tree");
        let root = child(tree.path(), "root");
        let directory = child(&root, "profile");
        let lease = WebView2RuntimeLease::claim(
            WebView2Profile::within_root(&directory, &root).expect("owned profile"),
        )
        .expect("profile lease");
        let issuer = lease.controller_issuer();
        let first = issuer.begin_controller().expect("first controller");
        first
            .verify_attested_user_data_folder(&directory)
            .expect("first attestation");
        let permit = first.permit().expect("in-flight page operation");
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (finished_tx, finished_rx) = std::sync::mpsc::channel();
        let replacer = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            let replacement = issuer.begin_controller().expect("replacement controller");
            finished_tx.send(replacement).unwrap();
        });

        started_rx.recv().unwrap();
        assert!(
            finished_rx
                .recv_timeout(std::time::Duration::from_millis(50))
                .is_err(),
            "a replacement controller crossed an old-generation page operation"
        );
        drop(permit);
        let replacement = finished_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("replacement did not resume after the old operation drained");
        replacer.join().unwrap();
        assert_eq!(first.ensure_active(), Err(CapabilityError::StaleController));
        assert_eq!(
            replacement.ensure_active(),
            Err(CapabilityError::Unattested)
        );
    }

    #[test]
    fn concurrent_controller_issuers_never_return_an_already_stale_generation_as_success() {
        let tree = tempfile::tempdir().expect("temp tree");
        let root = child(tree.path(), "root");
        let directory = child(&root, "profile");
        let lease = WebView2RuntimeLease::claim(
            WebView2Profile::within_root(&directory, &root).expect("owned profile"),
        )
        .expect("profile lease");
        let issuer = lease.controller_issuer();
        let first = issuer.begin_controller().expect("first controller");
        first
            .verify_attested_user_data_folder(&directory)
            .expect("first attestation");
        let old_operation = first.permit().expect("old-generation operation");

        let (result_tx, result_rx) = std::sync::mpsc::channel();
        let first_issuer = issuer.clone();
        let first_tx = result_tx.clone();
        let first_replacement = std::thread::spawn(move || {
            first_tx.send(first_issuer.begin_controller()).unwrap();
        });
        while issuer.gate.lock().controller_generation < 2 {
            std::thread::yield_now();
        }

        let second_issuer = issuer.clone();
        let second_replacement = std::thread::spawn(move || {
            result_tx.send(second_issuer.begin_controller()).unwrap();
        });
        while issuer.gate.lock().controller_generation < 3 {
            std::thread::yield_now();
        }

        drop(old_operation);
        let outcomes = [result_rx.recv().unwrap(), result_rx.recv().unwrap()];
        first_replacement.join().unwrap();
        second_replacement.join().unwrap();

        assert_eq!(
            outcomes.iter().filter(|result| result.is_ok()).count(),
            1,
            "exactly the newest issuer may return a usable controller"
        );
        assert_eq!(
            outcomes
                .iter()
                .filter(|result| matches!(result, Err(CapabilityError::StaleController)))
                .count(),
            1,
            "the superseded issuer must fail instead of returning stale authority"
        );
    }

    #[test]
    fn invalid_directory_errors_never_echo_paths() {
        let tree = tempfile::tempdir().expect("temp tree");
        let secret_name = "secret-profile-name";
        let error = WebView2Profile::within_root(&tree.path().join(secret_name), tree.path())
            .err()
            .expect("missing directory is rejected");
        assert!(!error.to_string().contains(secret_name));
    }

    #[cfg(windows)]
    #[test]
    fn windows_claim_comparisons_cannot_be_bypassed_by_path_case() {
        let claimed = Path::new(r"C:\Owned\Chromium\Profile");
        let same = Path::new(r"c:\owned\chromium\profile");
        let descendant = Path::new(r"c:\OWNED\CHROMIUM\PROFILE\Cache");

        assert!(canonical_paths_equal(claimed, same));
        assert!(canonical_path_starts_with(descendant, claimed));
    }

    #[cfg(windows)]
    #[test]
    fn windows_lease_prevents_claimed_directory_replacement_until_revoke() {
        let tree = tempfile::tempdir().expect("temp tree");
        let root = child(tree.path(), "root");
        let directory = child(&root, "profile");
        let moved = root.join("moved");
        let mut lease = WebView2RuntimeLease::claim(
            WebView2Profile::within_root(&directory, &root).expect("owned profile"),
        )
        .expect("profile lease");

        assert!(
            std::fs::rename(&directory, &moved).is_err(),
            "a live claim allowed its directory identity to be replaced"
        );
        lease.revoke();
        std::fs::rename(&directory, &moved)
            .expect("revoke releases the directory replacement guard");
    }

    #[cfg(unix)]
    #[test]
    fn symlink_candidates_are_rejected() {
        use std::os::unix::fs::symlink;

        let tree = tempfile::tempdir().expect("temp tree");
        let root = child(tree.path(), "root");
        let target = child(&root, "target");
        let link = root.join("link");
        symlink(&target, &link).expect("symlink");
        assert!(matches!(
            WebView2Profile::within_root(&link, &root),
            Err(CapabilityError::InvalidOwnedDirectory)
        ));

        let linked_root = tree.path().join("linked-root");
        symlink(&root, &linked_root).expect("root symlink");
        assert!(matches!(
            WebView2Profile::within_root(&target, &linked_root),
            Err(CapabilityError::InvalidOwnedDirectory)
        ));
    }
}
