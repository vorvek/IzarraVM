// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Direct native execution for AVX2-capable Windows and Linux x86-64 hosts. The interpreter
//! remains the architectural reference and can be selected explicitly for diagnostics.

pub(crate) fn host_supported() -> bool {
    crate::native_backend_available()
}

pub(crate) mod block;
// dead_code until C1 wires the dispatcher: in C0 the backend is exercised only by its
// proof battery and the register-unit differential test.
#[cfg(all(
    feature = "clif-backend",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[allow(dead_code)]
pub(crate) mod clif;
#[cfg_attr(
    not(all(
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    )),
    allow(dead_code)
)]
pub(crate) mod code_watch;
#[cfg_attr(
    not(all(
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    )),
    allow(dead_code)
)]
pub(crate) mod direct;
pub(crate) mod encoder;
pub(crate) mod exec_mem;
#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
pub(crate) mod fast_map;
#[cfg_attr(
    not(all(
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    )),
    allow(dead_code)
)]
pub(crate) mod links;
#[allow(dead_code)]
pub(crate) mod native_x87;
mod region;
#[cfg_attr(
    not(all(
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    )),
    allow(dead_code)
)]
pub(crate) mod smc_heat;
pub(crate) mod step;
pub(crate) mod unit_sim;
#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
pub(crate) mod x87_avx2_emit;

pub(crate) use region::RegionTable;

/// The CPU's jit state: the Direct block cache plus state owned ABOVE the individual backends.
/// Track C C1a-pre hoists the G1 SMC heat map here so the Direct cache and the future clif
/// cache share one map through SPLIT BORROWS on this struct. Deliberately no `Arc` and no
/// `Mutex`: guest execution is single-threaded by design, so plain `&mut` discipline is the
/// whole synchronization story. One boxed allocation on `CpuGsw` keeps the inline footprint a
/// single pointer, so the hot interpreter field offsets stay put (the pending_flags pin in
/// cpu_test.rs). `Deref` to the block cache keeps the pervasive `jit_direct.<method>` call
/// surface unchanged.
pub(crate) struct JitState {
    pub(crate) direct: direct::BlockCache,
    pub(crate) direct_barrier_census: Option<Box<direct::DirectBarrierCensus>>,
    direct_helpers_enabled: bool,
    direct_generation: u64,
    pub(crate) direct_native_frame_depth: u32,
    direct_reset_pending: bool,
    #[cfg(test)]
    direct_helper_force: DirectHelperTestForce,
    #[cfg(test)]
    direct_helper_edges_for_test: bool,
    pub(crate) smc_heat: direct::SmcHeatMap,
    /// The native code watch, HOISTED out of `BlockCache` (Track C C1c-pre, design decision
    /// D-C1c.1, mirroring the C1a-pre `SmcHeatMap` hoist): "watched" is a property of what
    /// physical code is currently resident and executable, not which backend put it there,
    /// so both backends register into ONE instance and the baked table-1 base stays a
    /// single shared pointer. Plain field reached by split borrow, no `Arc`/`Mutex` (guest
    /// execution is single-threaded). The `Box` moves as a whole across the hoist, so the
    /// published `table_base()` and per-page addresses Direct's emitted code bakes stay
    /// stable (the inner table/page allocations never reallocate on a field move).
    pub(crate) code_watch: Box<code_watch::NativeCodeWatch>,
    /// The per-instance clif policy flag (Track C C1a, plan decision D-C1.4 seam 2): the
    /// clif analogue of the Direct backend's enabled bit, settable per CpuGsw so
    /// differential tests route one instance through clif and another through Direct in one
    /// process. Present in every jit build (not only clif-backend) so policy gates such as
    /// poll_skip_eligible read one condition everywhere; without the feature no admission
    /// path ever consults it beyond that gate.
    pub(crate) clif_enabled: bool,
    /// The clif unit cache (Track C C1a, decision D-C1.1): key-based entries and per-unit
    /// descriptors, parallel to `direct` and sharing `smc_heat` through split borrows.
    #[cfg(all(
        feature = "clif-backend",
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    pub(crate) clif_units: clif::cache::ClifUnitCache,
    /// The pinned-ISA compile-and-install backend, built lazily on the first clif admission
    /// attempt so a run that never enables the clif policy never pays for the ISA pin or the
    /// arena reservation. `None` both before first use and after an unsupported-host failure.
    #[cfg(all(
        feature = "clif-backend",
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    pub(crate) clif_backend: Option<clif::ClifBackend>,
    /// Per-entry call-out scratch (Track C C1b): the hard-stop error relay (design finding
    /// B2), the N1 key-material snapshot for `callout_exit_latched`, and the exit-point/
    /// clock bookkeeping the dispatcher reads after a unit returns. Host bookkeeping only,
    /// excluded from CpuGsw equality through this type's always-true PartialEq.
    #[cfg(all(
        feature = "clif-backend",
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    pub(crate) clif_run: clif::callout::ClifRunScratch,
    /// Track C A2 (`dev_docs/plans/2026-07-19-clif-arena-reset-design.md` section 3.1): set
    /// by the `clif_clear` wrapper below on every wholesale clear -- pure heap bookkeeping,
    /// touching no arena byte, so it is safe to set even when the clear fired from inside a
    /// live native clif frame (a call-out's SMC-triggered clear, design section 5.4).
    /// Consumed by `apply_deferred_clif_arena_reset`, called from the top of
    /// `try_clif_continuation` (`run.rs`), the one point design section 5 proves is provably
    /// frame-free.
    #[cfg(all(
        feature = "clif-backend",
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    pub(crate) backend_needs_reset: bool,
    /// Track C A2 (design section 6): how many clif native frames are live on the host call
    /// stack right now (0 or 1 today; design section 5's frame-free proof). Tracked with a
    /// drop guard (`NativeFrameGuard`, `run.rs`) around the sole native-entry call site so a
    /// future fallible Rust inserted between entry and exit can never leak the increment
    /// (design section 6, MINOR-4). `apply_deferred_clif_arena_reset` treats a nonzero depth
    /// as a release-safe skip -- a real branch in every build profile, not a debug-only
    /// assert (design section 6, MAJOR-1).
    #[cfg(all(
        feature = "clif-backend",
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    pub(crate) native_frame_depth: u32,
}

impl JitState {
    pub(crate) fn new(direct: direct::BlockCache) -> Self {
        Self {
            direct,
            direct_barrier_census: direct::barrier_census_default(),
            direct_helpers_enabled: matches!(
                std::env::var("IZARRAVM_DIRECT_HELPERS").as_deref(),
                Ok("1")
            ),
            direct_generation: 1,
            direct_native_frame_depth: 0,
            direct_reset_pending: false,
            #[cfg(test)]
            direct_helper_force: DirectHelperTestForce::None,
            #[cfg(test)]
            direct_helper_edges_for_test: false,
            smc_heat: direct::SmcHeatMap::default(),
            code_watch: Box::default(),
            clif_enabled: false,
            #[cfg(all(
                feature = "clif-backend",
                target_arch = "x86_64",
                any(target_os = "windows", target_os = "linux")
            ))]
            clif_units: clif::cache::ClifUnitCache::default(),
            #[cfg(all(
                feature = "clif-backend",
                target_arch = "x86_64",
                any(target_os = "windows", target_os = "linux")
            ))]
            clif_backend: None,
            #[cfg(all(
                feature = "clif-backend",
                target_arch = "x86_64",
                any(target_os = "windows", target_os = "linux")
            ))]
            clif_run: clif::callout::ClifRunScratch::default(),
            #[cfg(all(
                feature = "clif-backend",
                target_arch = "x86_64",
                any(target_os = "windows", target_os = "linux")
            ))]
            backend_needs_reset: false,
            #[cfg(all(
                feature = "clif-backend",
                target_arch = "x86_64",
                any(target_os = "windows", target_os = "linux")
            ))]
            native_frame_depth: 0,
        }
    }
}

// Manual Clone/Debug (replacing the prior derive) because the clif backend and unit cache are
// host-only accelerators, not architectural state: a clone gets a FRESH backend and an empty
// unit cache (exactly how `BlockCache::clone` drops its compiled blocks), never a deep copy of
// installed native code or the pinned ISA handle.
impl Clone for JitState {
    fn clone(&self) -> Self {
        Self {
            direct: self.direct.clone(),
            direct_barrier_census: None,
            direct_helpers_enabled: self.direct_helpers_enabled,
            direct_generation: 1,
            direct_native_frame_depth: 0,
            direct_reset_pending: false,
            #[cfg(test)]
            direct_helper_force: DirectHelperTestForce::None,
            #[cfg(test)]
            direct_helper_edges_for_test: false,
            smc_heat: self.smc_heat.clone(),
            // A clone gets a fresh, empty watch, exactly as the pre-hoist BlockCache clone
            // produced (its clone built a new cache with a new watch).
            code_watch: Box::default(),
            clif_enabled: self.clif_enabled,
            #[cfg(all(
                feature = "clif-backend",
                target_arch = "x86_64",
                any(target_os = "windows", target_os = "linux")
            ))]
            clif_units: clif::cache::ClifUnitCache::default(),
            #[cfg(all(
                feature = "clif-backend",
                target_arch = "x86_64",
                any(target_os = "windows", target_os = "linux")
            ))]
            clif_backend: None,
            #[cfg(all(
                feature = "clif-backend",
                target_arch = "x86_64",
                any(target_os = "windows", target_os = "linux")
            ))]
            clif_run: clif::callout::ClifRunScratch::default(),
            // A fresh backend and empty cache mean nothing is pending: a clone never carries
            // over a stale reset flag or an in-flight-frame count from its source.
            #[cfg(all(
                feature = "clif-backend",
                target_arch = "x86_64",
                any(target_os = "windows", target_os = "linux")
            ))]
            backend_needs_reset: false,
            #[cfg(all(
                feature = "clif-backend",
                target_arch = "x86_64",
                any(target_os = "windows", target_os = "linux")
            ))]
            native_frame_depth: 0,
        }
    }
}

// Pre-hoist call-site compatibility: the watch-consuming cache operations keep their
// original names and signatures HERE, splitting the borrow between the cache and the
// hoisted watch internally. Inherent methods win over the `Deref` to `BlockCache`, so
// every existing `jit_direct.<method>(..)` call site, production and test, compiles
// unchanged against the hoisted ownership.
impl JitState {
    pub(crate) fn probe(&mut self, key: direct::BlockKey) -> direct::BlockProbe {
        self.apply_deferred_direct_reset();
        if self.direct_native_frame_depth != 0 {
            return direct::BlockProbe::Interpret;
        }
        let resets = self.direct.heat_resets();
        let probe = self.direct.probe(&mut self.code_watch, key);
        if self.direct.heat_resets() != resets {
            self.bump_direct_generation();
        }
        probe
    }

    pub(crate) fn install(&mut self, compilation: &direct::Compilation) -> Option<direct::BlockId> {
        self.apply_deferred_direct_reset();
        if self.direct_native_frame_depth != 0 {
            return None;
        }
        let resets = self.direct.heat_resets();
        let installed = self.direct.install(&mut self.code_watch, compilation);
        if self.direct.heat_resets() != resets {
            self.bump_direct_generation();
        }
        installed
    }

    pub(crate) fn reject(&mut self, span: direct::RejectedSpan) {
        self.direct.reject(&mut self.code_watch, span);
    }

    pub(crate) fn retire_key_for_recompile(&mut self, key: direct::BlockKey) -> bool {
        let retired = self
            .direct
            .retire_key_for_recompile(&mut self.code_watch, key);
        if retired {
            self.bump_direct_generation();
        }
        retired
    }

    pub(crate) fn clear(&mut self) {
        self.bump_direct_generation();
        if self.direct_native_frame_depth != 0 {
            self.direct_reset_pending = true;
            return;
        }
        self.direct.clear(&mut self.code_watch);
        self.direct_reset_pending = false;
    }

    pub(crate) fn invalidate_physical_range(&mut self, physical: u32, width: u32) -> usize {
        let invalidated =
            self.direct
                .invalidate_physical_range(&mut self.code_watch, physical, width);
        if invalidated != 0 {
            self.bump_direct_generation();
        }
        invalidated
    }

    pub(crate) fn invalidate_translation(&mut self) {
        self.bump_direct_generation();
        self.direct.invalidate_translation();
    }

    pub(crate) fn suspend_decode_slot(&mut self, slot: usize) -> usize {
        self.bump_direct_generation();
        self.direct.suspend_decode_slot(slot)
    }

    pub(crate) fn direct_generation(&self) -> u64 {
        self.direct_generation
    }

    pub(crate) fn direct_helpers_enabled(&self) -> bool {
        self.direct_helpers_enabled
    }

    #[cfg(test)]
    pub(crate) fn set_direct_helpers_enabled_for_test(&mut self, enabled: bool) {
        self.direct_helpers_enabled = enabled;
    }

    #[cfg(test)]
    pub(crate) fn set_direct_helper_force_for_test(&mut self, force: DirectHelperTestForce) {
        self.direct_helper_force = force;
    }

    #[cfg(test)]
    pub(crate) fn direct_helper_force_for_test(&self) -> DirectHelperTestForce {
        self.direct_helper_force
    }

    #[cfg(test)]
    pub(crate) fn set_direct_helper_edges_for_test(&mut self, enabled: bool) {
        self.direct_helper_edges_for_test = enabled;
    }

    #[cfg(test)]
    pub(crate) fn direct_helper_edges_for_test(&self) -> bool {
        self.direct_helper_edges_for_test
    }

    #[cfg(test)]
    pub(crate) fn direct_reset_pending_for_test(&self) -> bool {
        self.direct_reset_pending
    }

    pub(crate) fn apply_deferred_direct_reset(&mut self) {
        if !self.direct_reset_pending || self.direct_native_frame_depth != 0 {
            return;
        }
        self.direct.clear(&mut self.code_watch);
        self.direct_reset_pending = false;
    }

    fn bump_direct_generation(&mut self) {
        self.direct_generation = self
            .direct_generation
            .checked_add(1)
            .expect("Direct execution generation exhausted");
    }

    /// The shared table-1 base every backend's emitted store checks consult.
    pub(crate) fn native_code_watch_table(&mut self) -> usize {
        self.code_watch.table_base()
    }

    pub(crate) fn range_hits_compiled_code(&self, physical: u32, width: u32) -> bool {
        self.code_watch.range_watched(physical, width)
    }

    #[cfg(test)]
    pub(crate) fn mark_code_range(&mut self, physical: u32, len: u8) {
        self.code_watch.acquire_range(physical, u32::from(len));
    }

    /// Install a compiled clif unit descriptor, registering its guest physical range with
    /// the shared watch (design section 2.5/6, the M5 deliverable). Splits the borrow
    /// between `clif_units` and the hoisted `code_watch`, mirroring the `install`/`clear`
    /// wrappers above for Direct's own cache.
    #[cfg(all(
        feature = "clif-backend",
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    pub(crate) fn clif_install(
        &mut self,
        descriptor: clif::cache::ClifUnitDescriptor,
        cells: [std::sync::Arc<links::LinkCell>; 2],
        sentinel_addr: usize,
    ) -> Option<u32> {
        self.clif_units
            .install(&mut self.code_watch, descriptor, cells, sentinel_addr)
    }

    /// Wholesale clif-cache drop, releasing every installed unit's watch registration first.
    /// Track C A2 (design section 3.1): the arena reset itself is DEFERRED -- this wrapper
    /// only sets a flag, since a wholesale clear can fire from inside a live native clif
    /// frame (an x87 call-out's SMC-triggered clear, design section 5.4) where touching arena
    /// bytes would be a use-after-free. Setting a bool is pure heap bookkeeping and safe at
    /// any call site; `apply_deferred_clif_arena_reset` performs the actual reclaim later, at
    /// the one point design section 5 proves is provably frame-free.
    #[cfg(all(
        feature = "clif-backend",
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    pub(crate) fn clif_clear(&mut self) {
        self.clif_units.clear(&mut self.code_watch);
        self.backend_needs_reset = true;
    }

    /// Track C A2 (design sections 3.2/6): consume a pending arena reset if one is due. The
    /// SOLE call site is the top of `try_clif_continuation` (`run.rs`), before `clif_hot` and
    /// before any admission or adapter call -- the provably frame-free point design section 5
    /// establishes. RELEASE-SAFE guard (design section 6, MAJOR-1): if a native clif frame is
    /// somehow live (impossible today by construction; guarded against a future call-out that
    /// breaks the frame-free invariant), the reset is skipped in EVERY build profile and the
    /// flag stays set so the next frame-free call reclaims instead -- the reclaim is merely
    /// delayed one admission cycle, never lost, and never a release-mode use-after-free.
    #[cfg(all(
        feature = "clif-backend",
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    pub(crate) fn apply_deferred_clif_arena_reset(&mut self) {
        if !self.backend_needs_reset {
            return;
        }
        if self.native_frame_depth != 0 {
            debug_assert!(
                false,
                "clif arena reset attempted with a native frame live on the stack; see \
                 dev_docs/plans/2026-07-19-clif-arena-reset-design.md section 6"
            );
            return; // release: leave the flag set; the next frame-free call retries.
        }
        if let Some(backend) = self.clif_backend.as_mut() {
            // MINOR-5 (design section 7.4): `native_frame_depth` is threaded through so
            // `reset_arena`'s own internal assert has something to check, belt-and-suspenders
            // against a hypothetical second caller -- this call site already proved it is 0.
            if !backend.reset_arena(self.native_frame_depth) {
                // `make_rw` failed (design section 7.3): drop and let the next admission
                // rebuild a fresh backend rather than trust a half-reset arena.
                self.clif_backend = None;
            }
        }
        self.backend_needs_reset = false;
    }

    /// SMC invalidation for the clif cache, releasing the watch registration of any compiled
    /// unit dropped by the write (M5's eviction-side discipline).
    #[cfg(all(
        feature = "clif-backend",
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    pub(crate) fn clif_invalidate_physical_range(
        &mut self,
        physical: u32,
        width: u32,
    ) -> clif::cache::ClifInvalidateOutcome {
        self.clif_units
            .invalidate_physical_range(&mut self.code_watch, physical, width)
    }

    /// Track C1f diagnostic gauge: `ClifUnitCache::entries_len`, exposed through the
    /// hoisted `JitState` for `CpuGsw::jit_clif_counters` to read (mirrors the accessor
    /// pattern of the other `clif_*` wrappers on this impl).
    #[cfg(all(
        feature = "clif-backend",
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    pub(crate) fn clif_entries_len(&self) -> usize {
        self.clif_units.entries_len()
    }
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum DirectHelperTestForce {
    #[default]
    None,
    StaleDecode,
    GenerationAfterRetire,
    InvalidateCurrentAfterRetire,
    ClearAfterRetire,
    EipAfterRetire,
    ModeAfterRetire,
    SegmentAfterRetire,
    InterruptAfterRetire,
    HaltAfterRetire,
    RepAfterRetire,
    HardError,
    Panic,
    ClearThenPanic,
}

impl std::fmt::Debug for JitState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JitState")
            .field("direct", &self.direct)
            .field("clif_enabled", &self.clif_enabled)
            .finish()
    }
}

impl std::ops::Deref for JitState {
    type Target = direct::BlockCache;
    fn deref(&self) -> &Self::Target {
        &self.direct
    }
}

impl std::ops::DerefMut for JitState {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.direct
    }
}

// Host-only policy and cache state (F-A8): never influences architectural comparisons, so
// equality always holds, exactly like the block cache and heat map it wraps. The
// clif_enabled policy flag must not make two otherwise-identical CPUs compare unequal in
// differential tests that route one instance through each backend.
impl PartialEq for JitState {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}
impl Eq for JitState {}

/// Drop guard for a backend's native-frame depth. The raw pointer avoids holding a Rust borrow
/// into `CpuGsw` across a native entry that receives the whole CPU as a mutable pointer.
#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
pub(crate) struct NativeFrameGuard(*mut u32);

#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
impl NativeFrameGuard {
    /// # Safety
    /// `depth` must be valid for reads and writes for the guard's whole lifetime, and nothing
    /// else may mutate the pointee concurrently. Guest execution is single-threaded.
    pub(crate) unsafe fn enter(depth: *mut u32) -> Self {
        // SAFETY: caller contract.
        unsafe {
            *depth += 1;
        }
        Self(depth)
    }
}

#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
impl Drop for NativeFrameGuard {
    fn drop(&mut self) {
        // SAFETY: constructed only by `enter`, whose contract guarantees this pointer is
        // still valid and exclusively ours to decrement.
        unsafe {
            *self.0 -= 1;
        }
    }
}
