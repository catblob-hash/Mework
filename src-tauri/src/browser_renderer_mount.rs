use std::{
    error::Error,
    fmt,
    sync::{Mutex, MutexGuard},
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use uuid::{Uuid, Variant, Version};

/// Largest integer that can make a lossless Rust -> JSON -> JavaScript round trip.
pub(crate) const MAX_RENDERER_MOUNT_GENERATION: u64 = (1_u64 << 53) - 1;

/// Native-issued authority held by exactly one main renderer mount.
///
/// The pair is deliberately free of session, page, URL, title, profile, and
/// credential metadata. `mount_id` is an unguessable capability identifier;
/// `generation` is a native monotonic fence within the current app process.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct BrowserRendererMountLease {
    pub mount_id: String,
    pub generation: u64,
}

/// A native presentation may still belong to any invalidated renderer
/// generation at or below this fence.
///
/// The presentation owner must compare its own renderer generation before
/// hiding anything. It must never interpret this as permission to hide a newer
/// presentation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct BrowserRendererPresentationOrphanAction {
    pub hide_through_generation: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BrowserRendererMountError {
    InvalidMountId,
    InvalidGeneration,
    UnknownMount,
    StaleMount,
    MountCollision,
    GenerationExhausted,
    RegistryPoisoned,
}

impl fmt::Display for BrowserRendererMountError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::GenerationExhausted => {
                formatter.write_str("浏览器页面租约 generation 已耗尽，必须重启应用")
            }
            Self::RegistryPoisoned => formatter.write_str("浏览器页面租约注册表不可用"),
            Self::InvalidMountId
            | Self::InvalidGeneration
            | Self::UnknownMount
            | Self::StaleMount
            | Self::MountCollision => formatter.write_str("浏览器页面租约无效或已失效"),
        }
    }
}

impl Error for BrowserRendererMountError {}

#[derive(Clone)]
struct ActiveMount {
    lease: BrowserRendererMountLease,
    last_heartbeat: Instant,
}

struct BrowserRendererMountState {
    last_generation: u64,
    active: Option<ActiveMount>,
    /// Native-only one-time page-load proof. It is injected into the newly finished main
    /// document and never returned by a renderer command.
    bootstrap_challenge: Option<String>,
    /// Retained only so a lost registration response can be retried with the exact challenge
    /// without invalidating the same renderer a second time.
    bootstrap_lease: Option<BrowserRendererMountLease>,
    pending_hide_through_generation: Option<u64>,
    acknowledged_hide_through_generation: u64,
    /// Renderer-origin mutations that passed validation and have not settled yet.
    in_flight_mutations: u64,
    /// A mount was invalidated while it still had authorized mutations running.
    /// Those mutations may bind a native presentation after the invalidating
    /// fail-safe already ran, so the fail-safe must run again once they settle.
    presentation_fence_dirty: bool,
}

impl Default for BrowserRendererMountState {
    fn default() -> Self {
        Self {
            last_generation: 0,
            active: None,
            bootstrap_challenge: None,
            bootstrap_lease: None,
            pending_hide_through_generation: None,
            acknowledged_hide_through_generation: 0,
            in_flight_mutations: 0,
            presentation_fence_dirty: false,
        }
    }
}

/// Process-local renderer mount authority.
///
/// A fresh app process starts a fresh generation sequence. Old process leases
/// still cannot become valid because every mount also carries a native random
/// UUID v4. No lease is persisted across a process restart.
pub(crate) struct BrowserRendererMountRegistry {
    state: Mutex<BrowserRendererMountState>,
}

impl Default for BrowserRendererMountRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl BrowserRendererMountRegistry {
    pub(crate) fn new() -> Self {
        Self {
            state: Mutex::new(BrowserRendererMountState::default()),
        }
    }

    #[cfg(any(test, feature = "browser-dev"))]
    /// Registers a genuinely new renderer mount.
    ///
    /// This is not an idempotent operation: every successful call advances the
    /// native generation and atomically invalidates the previous mount. If an
    /// IPC response is lost and the caller has no returned lease to retry, it
    /// is safe to call this again; the lost lease becomes stale.
    pub(crate) fn register_new_mount(
        &self,
    ) -> Result<BrowserRendererMountLease, BrowserRendererMountError> {
        self.register_new_mount_at(Instant::now())
    }

    /// Begins a new main-document load and returns its native-only bootstrap challenge.
    ///
    /// Invalidating the previous mount happens before the challenge is exposed to the next
    /// document. A delayed command from the old JavaScript context cannot register again because
    /// it never learns this new random value.
    pub(crate) fn begin_main_document_load(&self) -> Result<String, BrowserRendererMountError> {
        let mut state = self.lock_state()?;
        invalidate_active_mount(&mut state);
        let challenge = generate_distinct_mount_id(state.bootstrap_challenge.as_deref());
        state.bootstrap_challenge = Some(challenge.clone());
        state.bootstrap_lease = None;
        Ok(challenge)
    }

    /// Returns the current challenge to the native page-load hook only.
    pub(crate) fn current_bootstrap_challenge(
        &self,
    ) -> Result<Option<String>, BrowserRendererMountError> {
        Ok(self.lock_state()?.bootstrap_challenge.clone())
    }

    /// Returns the newest generation ever issued in this process.
    ///
    /// The main-document load hook uses this only as a native presentation
    /// fence. It is never sent to an unregistered renderer.
    pub(crate) fn latest_generation(&self) -> Result<u64, BrowserRendererMountError> {
        Ok(self.lock_state()?.last_generation)
    }

    /// Revokes a bootstrap challenge when the native presentation fail-safe
    /// could not complete.
    ///
    /// The exact comparison prevents a delayed cleanup failure from clearing a
    /// newer document's challenge.
    pub(crate) fn abort_main_document_load(
        &self,
        challenge: &str,
    ) -> Result<bool, BrowserRendererMountError> {
        if !is_canonical_uuid_v4(challenge) {
            return Err(BrowserRendererMountError::InvalidMountId);
        }
        let mut state = self.lock_state()?;
        if state.bootstrap_challenge.as_deref() != Some(challenge) {
            return Ok(false);
        }
        state.bootstrap_challenge = None;
        state.bootstrap_lease = None;
        Ok(true)
    }

    /// Registers (or idempotently retries) the renderer belonging to the current native page load.
    pub(crate) fn register_bootstrapped_mount(
        &self,
        challenge: &str,
    ) -> Result<BrowserRendererMountLease, BrowserRendererMountError> {
        if !is_canonical_uuid_v4(challenge) {
            return Err(BrowserRendererMountError::InvalidMountId);
        }
        let now = Instant::now();
        let mut state = self.lock_state()?;
        if state.bootstrap_challenge.as_deref() != Some(challenge) {
            return Err(BrowserRendererMountError::UnknownMount);
        }
        if let Some(lease) = state.bootstrap_lease.clone() {
            validate_exact_mount(&state, &lease.mount_id, lease.generation)?;
            if let Some(active) = state.active.as_mut() {
                active.last_heartbeat = now;
            }
            return Ok(lease);
        }
        let lease = register_new_mount_locked(&mut state, now)?;
        state.bootstrap_lease = Some(lease.clone());
        Ok(lease)
    }

    #[cfg(test)]
    /// Idempotently retries registration of the exact currently active mount.
    ///
    /// An exact retry returns the same lease, renews its heartbeat, does not
    /// advance generation, and does not create an orphan action. A stale,
    /// unknown, or colliding pair is rejected and can never resurrect a mount.
    pub(crate) fn retry_mount_registration(
        &self,
        mount_id: &str,
        generation: u64,
    ) -> Result<BrowserRendererMountLease, BrowserRendererMountError> {
        self.retry_mount_registration_at(mount_id, generation, Instant::now())
    }

    /// Runs a renderer-origin mutation authorized by the exact current mount lease.
    ///
    /// The registry lock is released before the closure runs and is only
    /// re-taken to settle the in-flight count. This is mandatory: native
    /// browser mutations dispatch Win32 and WebView work to the main thread and
    /// wait for it, and several trusted commands take this same lock on the
    /// main thread. Holding it across the closure deadlocks the whole UI until
    /// (or past) the native timeout.
    ///
    /// The check/use race is closed on the other side instead. An invalidation
    /// that lands while a mutation authorized by that mount is still running
    /// marks the presentation fence dirty, so
    /// [`Self::take_settled_presentation_fence`] reports that the
    /// presentation fail-safe must run again once the mutation settles.
    pub(crate) fn with_validated_mutation<T>(
        &self,
        mount_id: &str,
        generation: u64,
        mutation: impl FnOnce() -> T,
    ) -> Result<T, BrowserRendererMountError> {
        let in_flight = self.begin_validated_mutation(mount_id, generation)?;
        let value = mutation();
        drop(in_flight);
        Ok(value)
    }

    /// Validates the exact mount and registers one in-flight mutation.
    fn begin_validated_mutation(
        &self,
        mount_id: &str,
        generation: u64,
    ) -> Result<InFlightMutation<'_>, BrowserRendererMountError> {
        let mut state = self.lock_state()?;
        validate_exact_mount(&state, mount_id, generation)?;
        state.in_flight_mutations = state.in_flight_mutations.saturating_add(1);
        Ok(InFlightMutation { registry: self })
    }

    /// Records that the presentation fail-safe still owes a run.
    ///
    /// Used when a caller that must not block (the main thread) could not run
    /// it now. [`Self::take_settled_presentation_fence`] then hands it to a
    /// thread that may block.
    pub(crate) fn mark_presentation_fence_dirty(&self) -> Result<(), BrowserRendererMountError> {
        self.lock_state()?.presentation_fence_dirty = true;
        Ok(())
    }

    /// Reports (once) that every mutation authorized before an invalidation has
    /// settled and the presentation fail-safe still has to run for it.
    ///
    /// Taking the fence clears it, so the caller owns the run: if the fail-safe
    /// then fails it must call [`Self::mark_presentation_fence_dirty`] again.
    pub(crate) fn take_settled_presentation_fence(
        &self,
    ) -> Result<bool, BrowserRendererMountError> {
        let mut state = self.lock_state()?;
        if !state.presentation_fence_dirty || state.in_flight_mutations > 0 {
            return Ok(false);
        }
        state.presentation_fence_dirty = false;
        Ok(true)
    }

    /// Renews liveness only for the exact active renderer mount.
    ///
    /// Heartbeats never create or consume presentation orphan actions.
    pub(crate) fn heartbeat(
        &self,
        mount_id: &str,
        generation: u64,
    ) -> Result<(), BrowserRendererMountError> {
        self.heartbeat_at(mount_id, generation, Instant::now())
    }

    #[cfg(test)]
    /// Explicitly invalidates the exact active mount.
    ///
    /// The result only says whether state changed. The presentation action is
    /// intentionally obtained and acknowledged through the separate methods
    /// below.
    pub(crate) fn invalidate_mount(
        &self,
        mount_id: &str,
        generation: u64,
    ) -> Result<bool, BrowserRendererMountError> {
        validate_mount_credential_format(mount_id, generation)?;
        let mut state = self.lock_state()?;
        validate_exact_mount(&state, mount_id, generation)?;
        invalidate_active_mount(&mut state);
        Ok(true)
    }

    #[cfg(test)]
    /// Native-only invalidation for renderer teardown/crash signals.
    pub(crate) fn invalidate_current_mount(&self) -> Result<bool, BrowserRendererMountError> {
        let mut state = self.lock_state()?;
        Ok(invalidate_active_mount(&mut state))
    }

    /// Invalidates the current mount after it misses its heartbeat deadline.
    pub(crate) fn invalidate_stale_mount(
        &self,
        maximum_idle: Duration,
    ) -> Result<bool, BrowserRendererMountError> {
        self.invalidate_stale_mount_at(Instant::now(), maximum_idle)
    }

    /// Returns the coalesced presentation fence without consuming it.
    ///
    /// Repeated calls are idempotent until the native presentation owner
    /// confirms that it hid (or verified the absence of) every presentation at
    /// or below `hide_through_generation`.
    pub(crate) fn pending_presentation_orphan_action(
        &self,
    ) -> Result<Option<BrowserRendererPresentationOrphanAction>, BrowserRendererMountError> {
        let state = self.lock_state()?;
        Ok(state
            .pending_hide_through_generation
            .map(
                |hide_through_generation| BrowserRendererPresentationOrphanAction {
                    hide_through_generation,
                },
            ))
    }

    /// Acknowledges a previously observed presentation orphan fence.
    ///
    /// The acknowledgement is idempotent. If a newer invalidation arrived
    /// while native presentation cleanup was running, its higher fence remains
    /// pending and must be handled separately.
    pub(crate) fn acknowledge_presentation_orphan_action(
        &self,
        hide_through_generation: u64,
    ) -> Result<bool, BrowserRendererMountError> {
        validate_generation(hide_through_generation)?;
        let mut state = self.lock_state()?;

        if hide_through_generation <= state.acknowledged_hide_through_generation {
            return Ok(false);
        }

        let Some(pending_generation) = state.pending_hide_through_generation else {
            return Err(BrowserRendererMountError::UnknownMount);
        };

        if hide_through_generation > pending_generation {
            return Err(BrowserRendererMountError::MountCollision);
        }

        state.acknowledged_hide_through_generation = hide_through_generation;
        if hide_through_generation == pending_generation {
            state.pending_hide_through_generation = None;
        }
        Ok(true)
    }

    #[cfg(any(test, feature = "browser-dev"))]
    fn register_new_mount_at(
        &self,
        now: Instant,
    ) -> Result<BrowserRendererMountLease, BrowserRendererMountError> {
        let mut state = self.lock_state()?;
        register_new_mount_locked(&mut state, now)
    }

    #[cfg(test)]
    fn retry_mount_registration_at(
        &self,
        mount_id: &str,
        generation: u64,
        now: Instant,
    ) -> Result<BrowserRendererMountLease, BrowserRendererMountError> {
        validate_mount_credential_format(mount_id, generation)?;
        let mut state = self.lock_state()?;
        validate_exact_mount(&state, mount_id, generation)?;
        let active = state
            .active
            .as_mut()
            .ok_or(BrowserRendererMountError::UnknownMount)?;
        active.last_heartbeat = now;
        Ok(active.lease.clone())
    }

    fn heartbeat_at(
        &self,
        mount_id: &str,
        generation: u64,
        now: Instant,
    ) -> Result<(), BrowserRendererMountError> {
        validate_mount_credential_format(mount_id, generation)?;
        let mut state = self.lock_state()?;
        validate_exact_mount(&state, mount_id, generation)?;
        let active = state
            .active
            .as_mut()
            .ok_or(BrowserRendererMountError::UnknownMount)?;
        active.last_heartbeat = now;
        Ok(())
    }

    fn invalidate_stale_mount_at(
        &self,
        now: Instant,
        maximum_idle: Duration,
    ) -> Result<bool, BrowserRendererMountError> {
        let mut state = self.lock_state()?;
        let stale = state.active.as_ref().is_some_and(|active| {
            now.saturating_duration_since(active.last_heartbeat) >= maximum_idle
        });
        if !stale {
            return Ok(false);
        }
        Ok(invalidate_active_mount(&mut state))
    }

    fn lock_state(
        &self,
    ) -> Result<MutexGuard<'_, BrowserRendererMountState>, BrowserRendererMountError> {
        self.state
            .lock()
            .map_err(|_| BrowserRendererMountError::RegistryPoisoned)
    }
}

fn register_new_mount_locked(
    state: &mut BrowserRendererMountState,
    now: Instant,
) -> Result<BrowserRendererMountLease, BrowserRendererMountError> {
    let generation = state
        .last_generation
        .checked_add(1)
        .filter(|generation| *generation <= MAX_RENDERER_MOUNT_GENERATION)
        .ok_or(BrowserRendererMountError::GenerationExhausted)?;

    let previous_mount_id = state
        .active
        .as_ref()
        .map(|active| active.lease.mount_id.as_str());
    let mount_id = generate_distinct_mount_id(previous_mount_id);
    let lease = BrowserRendererMountLease {
        mount_id,
        generation,
    };

    invalidate_active_mount(state);
    state.last_generation = generation;
    state.active = Some(ActiveMount {
        lease: lease.clone(),
        last_heartbeat: now,
    });
    Ok(lease)
}

fn generate_distinct_mount_id(previous_mount_id: Option<&str>) -> String {
    loop {
        let candidate = Uuid::new_v4().hyphenated().to_string();
        if previous_mount_id != Some(candidate.as_str()) {
            return candidate;
        }
    }
}

fn validate_mount_credential_format(
    mount_id: &str,
    generation: u64,
) -> Result<(), BrowserRendererMountError> {
    if !is_canonical_uuid_v4(mount_id) {
        return Err(BrowserRendererMountError::InvalidMountId);
    }
    validate_generation(generation)
}

fn validate_generation(generation: u64) -> Result<(), BrowserRendererMountError> {
    if generation == 0 || generation > MAX_RENDERER_MOUNT_GENERATION {
        return Err(BrowserRendererMountError::InvalidGeneration);
    }
    Ok(())
}

fn validate_exact_mount(
    state: &BrowserRendererMountState,
    mount_id: &str,
    generation: u64,
) -> Result<(), BrowserRendererMountError> {
    validate_mount_credential_format(mount_id, generation)?;
    let Some(active) = state.active.as_ref() else {
        return Err(BrowserRendererMountError::UnknownMount);
    };

    if active.lease.mount_id == mount_id && active.lease.generation == generation {
        return Ok(());
    }
    if active.lease.mount_id == mount_id || active.lease.generation == generation {
        return Err(BrowserRendererMountError::MountCollision);
    }
    if generation < active.lease.generation {
        return Err(BrowserRendererMountError::StaleMount);
    }
    Err(BrowserRendererMountError::UnknownMount)
}

/// Keeps one validated renderer-origin mutation counted while it runs.
struct InFlightMutation<'registry> {
    registry: &'registry BrowserRendererMountRegistry,
}

impl Drop for InFlightMutation<'_> {
    fn drop(&mut self) {
        // A poisoned registry is already fatal for every other path; losing the
        // decrement here would only pin `presentation_fence_dirty` forever.
        let Ok(mut state) = self.registry.state.lock() else {
            return;
        };
        state.in_flight_mutations = state.in_flight_mutations.saturating_sub(1);
    }
}

fn invalidate_active_mount(state: &mut BrowserRendererMountState) -> bool {
    let Some(active) = state.active.take() else {
        return false;
    };
    state.pending_hide_through_generation = Some(
        state
            .pending_hide_through_generation
            .map_or(active.lease.generation, |pending| {
                pending.max(active.lease.generation)
            }),
    );
    // A mutation this mount already authorized can still bind a native
    // presentation after the fail-safe that follows this invalidation runs.
    if state.in_flight_mutations > 0 {
        state.presentation_fence_dirty = true;
    }
    true
}

fn is_canonical_uuid_v4(value: &str) -> bool {
    Uuid::parse_str(value).ok().is_some_and(|uuid| {
        uuid.get_variant() == Variant::RFC4122
            && uuid.get_version() == Some(Version::Random)
            && uuid.hyphenated().to_string() == value
    })
}

#[cfg(test)]
mod tests {
    use std::{
        panic::{catch_unwind, AssertUnwindSafe},
        sync::{mpsc, Arc, Barrier},
        thread,
        time::Duration,
    };

    use super::*;

    fn assert_valid_lease(lease: &BrowserRendererMountLease) {
        assert!(is_canonical_uuid_v4(&lease.mount_id));
        assert!((1..=MAX_RENDERER_MOUNT_GENERATION).contains(&lease.generation));
    }

    #[test]
    fn native_mount_ids_are_unique_canonical_uuid_v4_values() {
        let registry = BrowserRendererMountRegistry::new();
        let mut seen = std::collections::HashSet::new();

        for expected_generation in 1..=256 {
            let lease = registry.register_new_mount().unwrap();
            assert_valid_lease(&lease);
            assert_eq!(lease.generation, expected_generation);
            assert!(seen.insert(lease.mount_id));
        }
    }

    #[test]
    fn new_mount_atomically_invalidates_the_old_mount() {
        let registry = BrowserRendererMountRegistry::new();
        let first = registry.register_new_mount().unwrap();
        let second = registry.register_new_mount().unwrap();
        let mut old_mutation_ran = false;

        assert_eq!(second.generation, first.generation + 1);
        assert_eq!(
            registry
                .with_validated_mutation(&first.mount_id, first.generation, || {
                    old_mutation_ran = true;
                })
                .unwrap_err(),
            BrowserRendererMountError::StaleMount
        );
        assert!(!old_mutation_ran);
        assert_eq!(
            registry.pending_presentation_orphan_action().unwrap(),
            Some(BrowserRendererPresentationOrphanAction {
                hide_through_generation: first.generation,
            })
        );
        assert_eq!(
            registry
                .with_validated_mutation(&second.mount_id, second.generation, || 42)
                .unwrap(),
            42
        );
    }

    #[test]
    fn page_load_challenge_is_exact_idempotent_and_invalidates_the_old_document() {
        let registry = BrowserRendererMountRegistry::new();
        let first_challenge = registry.begin_main_document_load().unwrap();
        let first = registry
            .register_bootstrapped_mount(&first_challenge)
            .unwrap();
        assert_eq!(
            registry
                .register_bootstrapped_mount(&first_challenge)
                .unwrap(),
            first
        );

        let next_challenge = registry.begin_main_document_load().unwrap();
        assert_ne!(next_challenge, first_challenge);
        assert_eq!(
            registry
                .register_bootstrapped_mount(&first_challenge)
                .unwrap_err(),
            BrowserRendererMountError::UnknownMount
        );
        assert_eq!(
            registry
                .with_validated_mutation(&first.mount_id, first.generation, || ())
                .unwrap_err(),
            BrowserRendererMountError::UnknownMount
        );

        let next = registry
            .register_bootstrapped_mount(&next_challenge)
            .unwrap();
        assert_eq!(next.generation, first.generation + 1);
        assert_eq!(
            registry.current_bootstrap_challenge().unwrap(),
            Some(next_challenge)
        );
    }

    #[test]
    fn failed_native_page_load_cleanup_revokes_only_the_exact_challenge() {
        let registry = BrowserRendererMountRegistry::new();
        let first_challenge = registry.begin_main_document_load().unwrap();

        assert_eq!(registry.latest_generation().unwrap(), 0);
        assert!(registry.abort_main_document_load(&first_challenge).unwrap());
        assert_eq!(registry.current_bootstrap_challenge().unwrap(), None);
        assert_eq!(
            registry
                .register_bootstrapped_mount(&first_challenge)
                .unwrap_err(),
            BrowserRendererMountError::UnknownMount
        );
        assert!(!registry.abort_main_document_load(&first_challenge).unwrap());

        let next_challenge = registry.begin_main_document_load().unwrap();
        let lease = registry
            .register_bootstrapped_mount(&next_challenge)
            .unwrap();
        assert_eq!(registry.latest_generation().unwrap(), lease.generation);
        assert!(!registry.abort_main_document_load(&first_challenge).unwrap());
        assert_eq!(
            registry.current_bootstrap_challenge().unwrap(),
            Some(next_challenge)
        );
    }

    #[test]
    fn bootstrap_rejects_unrelated_or_malformed_challenges_without_registering() {
        let registry = BrowserRendererMountRegistry::new();
        let challenge = registry.begin_main_document_load().unwrap();
        let unrelated = Uuid::new_v4().hyphenated().to_string();

        assert_eq!(
            registry
                .register_bootstrapped_mount(&unrelated)
                .unwrap_err(),
            BrowserRendererMountError::UnknownMount
        );
        assert_eq!(
            registry
                .register_bootstrapped_mount("not-a-uuid")
                .unwrap_err(),
            BrowserRendererMountError::InvalidMountId
        );
        let lease = registry.register_bootstrapped_mount(&challenge).unwrap();
        assert_eq!(lease.generation, 1);
        assert_eq!(registry.pending_presentation_orphan_action().unwrap(), None);
    }

    #[test]
    fn exact_registration_retry_is_idempotent_and_every_other_retry_fails_closed() {
        let registry = BrowserRendererMountRegistry::new();
        let lease = registry.register_new_mount().unwrap();
        let retried = registry
            .retry_mount_registration(&lease.mount_id, lease.generation)
            .unwrap();

        assert_eq!(retried, lease);
        assert_eq!(registry.pending_presentation_orphan_action().unwrap(), None);
        assert_eq!(
            registry
                .retry_mount_registration(&lease.mount_id, lease.generation + 1)
                .unwrap_err(),
            BrowserRendererMountError::MountCollision
        );
        assert_eq!(
            registry
                .retry_mount_registration(
                    &Uuid::new_v4().hyphenated().to_string(),
                    lease.generation
                )
                .unwrap_err(),
            BrowserRendererMountError::MountCollision
        );
        assert_eq!(
            registry
                .retry_mount_registration("not-a-uuid", lease.generation)
                .unwrap_err(),
            BrowserRendererMountError::InvalidMountId
        );
    }

    #[test]
    fn stale_unknown_and_colliding_credentials_never_run_mutations() {
        let registry = BrowserRendererMountRegistry::new();
        let old = registry.register_new_mount().unwrap();
        let active = registry.register_new_mount().unwrap();
        let unknown_id = Uuid::new_v4().hyphenated().to_string();
        let mut calls = 0;

        let cases = [
            (
                old.mount_id.as_str(),
                old.generation,
                BrowserRendererMountError::StaleMount,
            ),
            (
                active.mount_id.as_str(),
                active.generation - 1,
                BrowserRendererMountError::MountCollision,
            ),
            (
                unknown_id.as_str(),
                active.generation,
                BrowserRendererMountError::MountCollision,
            ),
            (
                unknown_id.as_str(),
                active.generation + 1,
                BrowserRendererMountError::UnknownMount,
            ),
        ];

        for (mount_id, generation, expected) in cases {
            assert_eq!(
                registry
                    .with_validated_mutation(mount_id, generation, || calls += 1)
                    .unwrap_err(),
                expected
            );
        }
        assert_eq!(calls, 0);
    }

    #[test]
    fn in_flight_mutation_never_blocks_a_concurrent_registration() {
        // The registry lock must stay available while a mutation runs. Native
        // browser mutations wait on the main thread, and the main thread takes
        // this same lock, so blocking here would deadlock the whole UI.
        let registry = Arc::new(BrowserRendererMountRegistry::new());
        let lease = registry.register_new_mount().unwrap();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let mutation_registry = Arc::clone(&registry);
        let mutation_lease = lease.clone();
        let mutation = thread::spawn(move || {
            mutation_registry
                .with_validated_mutation(
                    &mutation_lease.mount_id,
                    mutation_lease.generation,
                    || {
                        entered_tx.send(()).unwrap();
                        release_rx.recv().unwrap();
                        "mutated"
                    },
                )
                .unwrap()
        });

        entered_rx.recv().unwrap();
        let next = registry.register_new_mount().unwrap();
        assert_eq!(next.generation, lease.generation + 1);

        // The fence is owed but not yet reportable: the mutation authorized by
        // the now-invalidated mount is still running.
        assert!(!registry.take_settled_presentation_fence().unwrap());

        release_tx.send(()).unwrap();
        assert_eq!(mutation.join().unwrap(), "mutated");

        // Once it settles the fail-safe is reported exactly once.
        assert!(registry.take_settled_presentation_fence().unwrap());
        assert!(!registry.take_settled_presentation_fence().unwrap());
    }

    #[test]
    fn a_mutation_waiting_on_the_main_thread_does_not_deadlock_the_main_thread() {
        // Exact shape of the sidebar-collapse freeze: a native browser mutation
        // runs off the main thread and waits for the main thread to service a
        // Win32/WebView dispatch, while the main thread services a trusted
        // command that needs this registry. If the registry lock spans the
        // mutation, neither side can ever advance.
        let registry = Arc::new(BrowserRendererMountRegistry::new());
        let lease = registry.register_new_mount().unwrap();
        let (dispatch_tx, dispatch_rx) = mpsc::channel();
        let (answer_tx, answer_rx) = mpsc::channel();

        let mutation_registry = Arc::clone(&registry);
        let mutation_lease = lease.clone();
        let mutation = thread::spawn(move || {
            mutation_registry
                .with_validated_mutation(
                    &mutation_lease.mount_id,
                    mutation_lease.generation,
                    || {
                        dispatch_tx.send(()).unwrap();
                        // The real code waits here without a timeout.
                        answer_rx.recv().unwrap()
                    },
                )
                .unwrap()
        });

        // "Main thread": it must stay able to take the registry lock, then
        // service the dispatch the mutation is blocked on.
        dispatch_rx.recv().unwrap();
        registry
            .heartbeat(&lease.mount_id, lease.generation)
            .unwrap();
        answer_tx.send("serviced").unwrap();

        assert_eq!(mutation.join().unwrap(), "serviced");
    }

    #[test]
    fn settled_presentation_fence_is_not_raised_without_an_invalidation() {
        let registry = BrowserRendererMountRegistry::new();
        let lease = registry.register_new_mount().unwrap();

        assert!(!registry.take_settled_presentation_fence().unwrap());
        registry
            .with_validated_mutation(&lease.mount_id, lease.generation, || ())
            .unwrap();
        assert!(!registry.take_settled_presentation_fence().unwrap());
    }

    #[test]
    fn stale_and_heartbeat_invalidation_also_raise_the_settled_fence() {
        let registry = Arc::new(BrowserRendererMountRegistry::new());
        let base = Instant::now();
        let lease = registry.register_new_mount_at(base).unwrap();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let mutation_registry = Arc::clone(&registry);
        let mutation_lease = lease.clone();
        let mutation = thread::spawn(move || {
            mutation_registry
                .with_validated_mutation(
                    &mutation_lease.mount_id,
                    mutation_lease.generation,
                    || {
                        entered_tx.send(()).unwrap();
                        release_rx.recv().unwrap();
                    },
                )
                .unwrap()
        });

        entered_rx.recv().unwrap();
        assert!(registry
            .invalidate_stale_mount_at(base + Duration::from_secs(30), Duration::from_secs(5))
            .unwrap());
        assert!(!registry.take_settled_presentation_fence().unwrap());

        release_tx.send(()).unwrap();
        mutation.join().unwrap();
        assert!(registry.take_settled_presentation_fence().unwrap());
    }

    #[test]
    fn heartbeat_renews_only_the_exact_mount_and_stale_expiry_is_separate() {
        let registry = BrowserRendererMountRegistry::new();
        let base = Instant::now();
        let lease = registry.register_new_mount_at(base).unwrap();

        assert!(!registry
            .invalidate_stale_mount_at(base + Duration::from_secs(4), Duration::from_secs(5))
            .unwrap());
        registry
            .heartbeat_at(
                &lease.mount_id,
                lease.generation,
                base + Duration::from_secs(4),
            )
            .unwrap();
        assert!(!registry
            .invalidate_stale_mount_at(base + Duration::from_secs(8), Duration::from_secs(5))
            .unwrap());
        assert!(registry
            .invalidate_stale_mount_at(base + Duration::from_secs(9), Duration::from_secs(5))
            .unwrap());
        assert_eq!(
            registry
                .heartbeat(&lease.mount_id, lease.generation)
                .unwrap_err(),
            BrowserRendererMountError::UnknownMount
        );
        assert_eq!(
            registry.pending_presentation_orphan_action().unwrap(),
            Some(BrowserRendererPresentationOrphanAction {
                hide_through_generation: lease.generation,
            })
        );
    }

    #[test]
    fn explicit_invalidation_and_presentation_acknowledgement_are_separate() {
        let registry = BrowserRendererMountRegistry::new();
        let lease = registry.register_new_mount().unwrap();

        assert!(registry
            .invalidate_mount(&lease.mount_id, lease.generation)
            .unwrap());
        let action = BrowserRendererPresentationOrphanAction {
            hide_through_generation: lease.generation,
        };
        assert_eq!(
            registry.pending_presentation_orphan_action().unwrap(),
            Some(action)
        );
        assert_eq!(
            registry.pending_presentation_orphan_action().unwrap(),
            Some(action)
        );
        assert!(registry
            .acknowledge_presentation_orphan_action(action.hide_through_generation)
            .unwrap());
        assert_eq!(registry.pending_presentation_orphan_action().unwrap(), None);
        assert!(!registry
            .acknowledge_presentation_orphan_action(action.hide_through_generation)
            .unwrap());
    }

    #[test]
    fn orphan_fences_coalesce_and_a_newer_fence_survives_an_old_ack() {
        let registry = BrowserRendererMountRegistry::new();
        let first = registry.register_new_mount().unwrap();
        let second = registry.register_new_mount().unwrap();
        let first_action = registry
            .pending_presentation_orphan_action()
            .unwrap()
            .unwrap();
        assert_eq!(first_action.hide_through_generation, first.generation);

        let third = registry.register_new_mount().unwrap();
        let newest_action = registry
            .pending_presentation_orphan_action()
            .unwrap()
            .unwrap();
        assert_eq!(newest_action.hide_through_generation, second.generation);
        assert!(registry
            .acknowledge_presentation_orphan_action(first_action.hide_through_generation)
            .unwrap());
        assert_eq!(
            registry.pending_presentation_orphan_action().unwrap(),
            Some(newest_action)
        );
        assert!(registry
            .acknowledge_presentation_orphan_action(newest_action.hide_through_generation)
            .unwrap());
        assert_eq!(
            registry
                .with_validated_mutation(&third.mount_id, third.generation, || true)
                .unwrap(),
            true
        );
    }

    #[test]
    fn native_invalidation_is_idempotent_but_old_renderer_invalidation_is_not() {
        let registry = BrowserRendererMountRegistry::new();
        let stale = registry.register_new_mount().unwrap();
        let lease = registry.register_new_mount().unwrap();

        assert_eq!(
            registry
                .invalidate_mount(&stale.mount_id, stale.generation)
                .unwrap_err(),
            BrowserRendererMountError::StaleMount
        );
        assert_eq!(
            registry
                .with_validated_mutation(&lease.mount_id, lease.generation, || "still-current")
                .unwrap(),
            "still-current"
        );

        assert!(registry.invalidate_current_mount().unwrap());
        assert!(!registry.invalidate_current_mount().unwrap());
        assert_eq!(
            registry
                .invalidate_mount(&lease.mount_id, lease.generation)
                .unwrap_err(),
            BrowserRendererMountError::UnknownMount
        );
    }

    #[test]
    fn process_restart_rejects_every_lease_from_the_previous_registry() {
        let previous_process = BrowserRendererMountRegistry::new();
        let old_lease = previous_process.register_new_mount().unwrap();
        let restarted_process = BrowserRendererMountRegistry::new();

        assert_eq!(
            restarted_process
                .retry_mount_registration(&old_lease.mount_id, old_lease.generation)
                .unwrap_err(),
            BrowserRendererMountError::UnknownMount
        );

        let new_lease = restarted_process.register_new_mount().unwrap();
        assert_eq!(new_lease.generation, 1);
        assert_ne!(new_lease.mount_id, old_lease.mount_id);
        assert_eq!(
            restarted_process
                .with_validated_mutation(&old_lease.mount_id, old_lease.generation, || ())
                .unwrap_err(),
            BrowserRendererMountError::MountCollision
        );
    }

    #[test]
    fn js_safe_generation_limit_is_transactional_and_fail_closed() {
        let registry = BrowserRendererMountRegistry {
            state: Mutex::new(BrowserRendererMountState {
                last_generation: MAX_RENDERER_MOUNT_GENERATION - 1,
                ..BrowserRendererMountState::default()
            }),
        };
        let last = registry.register_new_mount().unwrap();

        assert_eq!(last.generation, MAX_RENDERER_MOUNT_GENERATION);
        assert_eq!(
            registry.register_new_mount().unwrap_err(),
            BrowserRendererMountError::GenerationExhausted
        );
        assert_eq!(
            registry
                .with_validated_mutation(&last.mount_id, last.generation, || "still-current")
                .unwrap(),
            "still-current"
        );
        assert_eq!(
            registry
                .with_validated_mutation(&last.mount_id, MAX_RENDERER_MOUNT_GENERATION + 1, || ())
                .unwrap_err(),
            BrowserRendererMountError::InvalidGeneration
        );
        assert_eq!(
            registry
                .with_validated_mutation(&last.mount_id, u64::MAX, || ())
                .unwrap_err(),
            BrowserRendererMountError::InvalidGeneration
        );
    }

    #[test]
    fn concurrent_registration_is_unique_monotonic_and_leaves_one_authority() {
        const THREADS: usize = 24;
        let registry = Arc::new(BrowserRendererMountRegistry::new());
        let barrier = Arc::new(Barrier::new(THREADS));
        let mut handles = Vec::new();

        for _ in 0..THREADS {
            let registry = Arc::clone(&registry);
            let barrier = Arc::clone(&barrier);
            handles.push(thread::spawn(move || {
                barrier.wait();
                registry.register_new_mount().unwrap()
            }));
        }

        let mut leases: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect();
        leases.sort_by_key(|lease| lease.generation);
        assert_eq!(
            leases
                .iter()
                .map(|lease| lease.generation)
                .collect::<Vec<_>>(),
            (1..=THREADS as u64).collect::<Vec<_>>()
        );
        assert_eq!(
            leases
                .iter()
                .map(|lease| lease.mount_id.as_str())
                .collect::<std::collections::HashSet<_>>()
                .len(),
            THREADS
        );

        let active = leases.last().unwrap();
        assert_eq!(
            registry
                .with_validated_mutation(&active.mount_id, active.generation, || "current")
                .unwrap(),
            "current"
        );
        for stale in &leases[..leases.len() - 1] {
            assert_eq!(
                registry
                    .with_validated_mutation(&stale.mount_id, stale.generation, || ())
                    .unwrap_err(),
                BrowserRendererMountError::StaleMount
            );
        }
        assert_eq!(
            registry.pending_presentation_orphan_action().unwrap(),
            Some(BrowserRendererPresentationOrphanAction {
                hide_through_generation: THREADS as u64 - 1,
            })
        );
    }

    #[test]
    fn concurrent_exact_retries_do_not_advance_generation() {
        const THREADS: usize = 16;
        let registry = Arc::new(BrowserRendererMountRegistry::new());
        let lease = registry.register_new_mount().unwrap();
        let barrier = Arc::new(Barrier::new(THREADS));
        let mut handles = Vec::new();

        for _ in 0..THREADS {
            let registry = Arc::clone(&registry);
            let barrier = Arc::clone(&barrier);
            let lease = lease.clone();
            handles.push(thread::spawn(move || {
                barrier.wait();
                registry
                    .retry_mount_registration(&lease.mount_id, lease.generation)
                    .unwrap()
            }));
        }

        for handle in handles {
            assert_eq!(handle.join().unwrap(), lease);
        }
        let replacement = registry.register_new_mount().unwrap();
        assert_eq!(replacement.generation, lease.generation + 1);
    }

    #[test]
    fn a_panicking_mutation_settles_its_fence_without_poisoning_the_registry() {
        // The registry lock is not held across the mutation, so an unrelated
        // native panic must not revoke renderer authority for the whole
        // process. The in-flight count still has to unwind correctly.
        let registry = Arc::new(BrowserRendererMountRegistry::new());
        let lease = registry.register_new_mount().unwrap();

        let panic = catch_unwind(AssertUnwindSafe(|| {
            let _ =
                registry.with_validated_mutation(&lease.mount_id, lease.generation, || -> () {
                    panic!("native browser mutation panicked")
                });
        }));
        assert!(panic.is_err());

        registry
            .heartbeat(&lease.mount_id, lease.generation)
            .unwrap();
        let next = registry.register_new_mount().unwrap();
        assert_eq!(next.generation, lease.generation + 1);
        // The invalidation above landed after the panicking mutation settled,
        // so no deferred fail-safe is owed.
        assert!(!registry.take_settled_presentation_fence().unwrap());
    }

    #[test]
    fn poisoned_registry_rejects_every_later_operation() {
        let registry = Arc::new(BrowserRendererMountRegistry::new());
        let lease = registry.register_new_mount().unwrap();

        let poisoner = Arc::clone(&registry);
        assert!(thread::spawn(move || {
            let _guard = poisoner.state.lock().unwrap();
            panic!("poison registry while its lock is held")
        })
        .join()
        .is_err());

        assert_eq!(
            registry.register_new_mount().unwrap_err(),
            BrowserRendererMountError::RegistryPoisoned
        );
        assert_eq!(
            registry
                .with_validated_mutation(&lease.mount_id, lease.generation, || ())
                .unwrap_err(),
            BrowserRendererMountError::RegistryPoisoned
        );
        assert_eq!(
            registry.take_settled_presentation_fence().unwrap_err(),
            BrowserRendererMountError::RegistryPoisoned
        );
        assert_eq!(
            registry
                .heartbeat(&lease.mount_id, lease.generation)
                .unwrap_err(),
            BrowserRendererMountError::RegistryPoisoned
        );
        assert_eq!(
            registry.pending_presentation_orphan_action().unwrap_err(),
            BrowserRendererMountError::RegistryPoisoned
        );
    }

    #[test]
    fn serialized_contract_contains_only_capability_and_generation_fields() {
        let lease = BrowserRendererMountRegistry::new()
            .register_new_mount()
            .unwrap();
        let lease_json = serde_json::to_value(&lease).unwrap();
        let action_json = serde_json::to_value(BrowserRendererPresentationOrphanAction {
            hide_through_generation: lease.generation,
        })
        .unwrap();

        assert_eq!(
            lease_json
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect::<Vec<_>>(),
            vec!["generation".to_owned(), "mountId".to_owned()]
        );
        assert_eq!(
            action_json
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect::<Vec<_>>(),
            vec!["hideThroughGeneration".to_owned()]
        );
    }

    #[test]
    fn noncanonical_or_non_v4_mount_ids_and_invalid_generations_are_rejected() {
        let registry = BrowserRendererMountRegistry::new();
        let lease = registry.register_new_mount().unwrap();
        let uppercase = lease.mount_id.to_ascii_uppercase();
        let nil = Uuid::nil().hyphenated().to_string();

        for invalid_id in ["", "not-a-uuid", uppercase.as_str(), nil.as_str()] {
            assert_eq!(
                registry
                    .with_validated_mutation(invalid_id, lease.generation, || ())
                    .unwrap_err(),
                BrowserRendererMountError::InvalidMountId
            );
        }
        for invalid_generation in [0, MAX_RENDERER_MOUNT_GENERATION + 1, u64::MAX] {
            assert_eq!(
                registry
                    .with_validated_mutation(&lease.mount_id, invalid_generation, || ())
                    .unwrap_err(),
                BrowserRendererMountError::InvalidGeneration
            );
        }
    }
}
