// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Direct native execution for AVX2-capable Windows and Linux x86-64 hosts. The interpreter
//! remains the architectural reference and can be selected explicitly for diagnostics.

pub(crate) fn host_supported() -> bool {
    crate::native_backend_available()
}

pub(crate) mod block;
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
#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
mod unwind;
#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
pub(crate) mod x87_avx2_emit;

/// The CPU's jit state: the Direct block cache plus state owned ABOVE the individual backends.
/// Track C C1a-pre hoists the G1 SMC heat map here so a future backend could share it with the
/// Direct cache through SPLIT BORROWS on this struct (the now-removed clif backend once did).
/// Deliberately no `Arc` and no
/// `Mutex`: guest execution is single-threaded by design, so plain `&mut` discipline is the
/// whole synchronization story. One boxed allocation on `CpuGsw` keeps the inline footprint a
/// single pointer, so the hot interpreter field offsets stay put (the pending_flags pin in
/// cpu_test.rs). `Deref` to the block cache keeps the pervasive `jit_direct.<method>` call
/// surface unchanged.
pub(crate) struct JitState {
    pub(crate) direct: direct::BlockCache,
    /// Whether the Direct backend lowers `OperandSize::Word` operands below I586.
    ///
    /// A FIELD rather than a `OnceLock` env read like `sixteen_bit_admission_level`, and the
    /// difference is testability rather than taste: a process-wide `OnceLock` cannot be flipped
    /// per test, so the lifted arm would ship with no unit coverage at all while the two tests
    /// that exist pin only the default arm. Seeded from env once by `word_at_486_default`, and
    /// settable programmatically, exactly as `direct_barrier_census` is.
    ///
    /// Default FALSE, which is the shipped behaviour this slice does not change.
    pub(crate) word_at_486: bool,
    /// Emitted memory sites load table bases R15-relative from
    /// `CpuGsw::native_table_slots` instead of baking imm64s. Always true in
    /// production — the `IZARRAVM_R15_TABLES` A/B gate retired once the slice
    /// measured (gate run 4227353445f1, PR #716) — and a FIELD so tests flip it
    /// per-CPU and both emission arms keep unit coverage (`word_at_486`'s reason).
    pub(crate) r15_tables: bool,
    /// Emitted stores take the PAGE_WATCHED fast path (watched-page-bit design
    /// D3). Always true in production — the `IZARRAVM_WATCH_PAGE_BIT` gate
    /// retired after #713 soaked — a field for `word_at_486`'s testability
    /// reason, and CARRIED by clone for its lockstep-comparison reason.
    pub(crate) watch_page_bit: bool,
    /// Emitted stores probe the one-lookup store-bias table and route special pages through
    /// the shared stub pad (`dev_docs/2026-08-07-one-lookup-store-design.md` D3/D4/D5).
    /// Seeded from `IZARRAVM_ONE_LOOKUP_STORE` (default ON, `=0` restores the classify/resolve
    /// emission wholesale); a FIELD for `word_at_486`'s testability reason, and CARRIED by
    /// clone for `watch_page_bit`'s lockstep-comparison reason. Requires `r15_tables` — the
    /// compile walk enforces that at the pad-build site.
    pub(crate) one_lookup_store: bool,
    /// Emitted reads probe the one-lookup load-bias table and route special pages through the
    /// shared read-resolve pad (`dev_docs/2026-08-07-one-lookup-load-design.md` D3a/D3b/D5).
    /// Seeded from `IZARRAVM_ONE_LOOKUP_LOAD` (default ON, `=0` restores the classify/resolve
    /// read emission wholesale); a FIELD for `word_at_486`'s testability reason, CARRIED by
    /// clone for the lockstep reason. Requires `r15_tables`; independent of `one_lookup_store`
    /// so either slice A/Bs alone.
    pub(crate) one_lookup_load: bool,
    /// Whether the blocks in this cache were emitted WITH the `NativeBlockTrace` append
    /// preamble — i.e. the answer to `!bus.native_fetches_are_uniform()` that every resident
    /// block was compiled against.
    ///
    /// Not a policy dial: a MIRROR of a bus property, kept here because `direct::compile`
    /// has no bus in scope (`EmitInput::fetch_trace` carries it the rest of the way). Two
    /// sites keep it honest, and neither is optional:
    ///
    ///   * `try_direct_continuation` synchronises it against the live bus BEFORE the probe,
    ///     so a compile always emits the shape the bus that asked for it needs.
    ///   * `run_direct_block` re-checks it against the live bus before entering native code,
    ///     which covers the test seams that drive `run_direct_block` directly.
    ///
    /// A disagreement at either site rewrites the field and CLEARS the block cache: an
    /// elided-trace block entered with a live `trace_ptr` would silently under-report fetch
    /// observations, so the shape and the bus can never be allowed to drift apart.
    ///
    /// Seeded TRUE — the emitting arm, which is the pre-slice behaviour and what the
    /// `CpuBus` trait default (`native_fetches_are_uniform() == false`) asks for. On the
    /// production `MachineBus` the Direct backend only runs at all under an Approximate
    /// persona (`try_direct_continuation`'s `uses_approximate_timing` gate), and that is
    /// exactly when `flat_data_cost` — and therefore `native_fetches_are_uniform` — is true,
    /// so the field flips to false on the first continuation and never moves again.
    /// CARRIED by clone, for `watch_page_bit`'s lockstep-comparison reason.
    pub(crate) native_fetch_trace: bool,
    /// Admission level for 16-bit code segments, seeded from `IZARRAVM_JIT16`.
    ///
    /// A field for the same reason `word_at_486` is one: the `OnceLock` behind it is process-wide,
    /// so a fixture cannot exercise both arms, and the level-0 early-out in `try_direct_continuation`
    /// would have lost its only cover the moment the default moved off 0.
    pub(crate) sixteen_bit_level: u8,
    /// Cached mirror of `direct::native_keys_admitted(cpu.mode())` — the host-capability and
    /// persona screen `key_for_phys` opens with. A hoisted CONSTANT, not a policy dial: nothing
    /// sets it directly, and `CpuGsw::set_native_key_admission_for_test` does not exist.
    ///
    /// Why the cache cannot go stale, rather than a claim that it does not:
    ///
    ///   * The screen reads exactly two things. `jit::host_supported()` is
    ///     `is_x86_feature_detected!("avx2")`, fixed for the life of the process — and the reason
    ///     this hoist is worth anything, since it is an out-of-crate call the compiler cannot
    ///     fold away, measured at 1.19% of gp2's wall executed once per continuation.
    ///     `CpuGsw::mode` is the other, and `CpuGsw::set_mode` is its only writer THAT CAN
    ///     REACH `key_for_phys` — two canonical-state test closures poke the crate-private
    ///     field directly without ever keying a block, which the adversarial review proved
    ///     harmless by forcing a stale cache there and watching nothing fire
    ///     (the field is private to the crate root, and canonical restore goes through
    ///     `set_mode` like everything else). Both writers refresh this field; there is no third
    ///     way to reach the inputs.
    ///   * CARRIED by clone, deliberately unlike `FastMapServeGate` (which resets to `false`).
    ///     The asymmetry follows the invariant: `CpuGsw::clone` copies `mode`, so the cached
    ///     answer is still the right one, and resetting to `false` would silently REFUSE every
    ///     block key on a cloned CPU — an admission-policy change wearing an accelerator's
    ///     clothes. `word_at_486`'s clone comment makes the same argument for the same reason.
    ///   * `key_for_phys` `debug_assert`s the cache against a live recompute on every call, so a
    ///     future writer that forgets the refresh fails the whole debug test suite at its first
    ///     admitted key rather than quietly changing what compiles.
    pub(crate) native_keys_admitted: bool,
    pub(crate) direct_barrier_census: Option<Box<direct::DirectBarrierCensus>>,
    /// Per-window entry-target tally for the v2 windowed IPE trace. `None` is DISARMED and is the
    /// only state a normal build reaches; see `crate::ipe_entry_tally` for the cost statement.
    /// It lives here, next to `direct_barrier_census`, for that field's reason: the direct entry
    /// path already loads and writes `JitState`, so the disarmed null test costs no extra cache
    /// line and the pinned `CpuGsw` field offsets do not move.
    pub(crate) ipe_entry_targets: Option<Box<crate::ipe_entry_tally::IpeEntryTally>>,
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
    /// N5 audit instrument. It lives HERE, behind the `Box<JitState>` the CPU already owns,
    /// rather than as a `CpuGsw` field, for one measured reason: as a by-value `CpuGsw` field the
    /// 80-byte block moved the pinned `pending_flags` offset 4488 -> 4568, and even boxed it still
    /// moved it to 4496 (its own 8-byte pointer). Hanging it off an allocation `CpuGsw` already
    /// has costs zero bytes there and leaves the pin exactly where it was. Same reasoning as
    /// `direct_barrier_census` above, and the same clone behaviour: a clone gets a fresh block.
    /// There is no FastMap to audit without this feature, so a non-jit build reports zeros.
    pub(crate) fast_map_audit: Box<crate::FastMapAuditCounters>,
    /// Strict E2 edge pages from the last `install`/`reject`, awaiting the fast-map sweep
    /// (watched-page-bit design D4). Non-empty only between the acquiring call and its
    /// `CpuGsw::sweep_block_watch_edges` drain; `install`/`reject` assert that.
    pub(crate) pending_watch_edges: Vec<u32>,
    #[cfg(feature = "direct-callout-attribution")]
    pub(crate) direct_callout_attribution: Option<Box<direct::CallOutAttribution>>,
}

/// Seed for `JitState::one_lookup_store`, read once per process from
/// `IZARRAVM_ONE_LOOKUP_STORE` (the retired `IZARRAVM_R15_TABLES` pattern: env knob for the
/// single-binary A/B, deleted after soak).
fn one_lookup_store_default() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        !matches!(
            std::env::var("IZARRAVM_ONE_LOOKUP_STORE").as_deref(),
            Ok("0")
        )
    })
}

/// Seed for `JitState::one_lookup_load`, read once per process from
/// `IZARRAVM_ONE_LOOKUP_LOAD` — the store knob's pattern, on its own retirement clock.
fn one_lookup_load_default() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        !matches!(
            std::env::var("IZARRAVM_ONE_LOOKUP_LOAD").as_deref(),
            Ok("0")
        )
    })
}

impl JitState {
    pub(crate) fn new(direct: direct::BlockCache) -> Self {
        Self {
            direct,
            word_at_486: direct::word_at_486_default(),
            r15_tables: true,
            watch_page_bit: true,
            one_lookup_store: one_lookup_store_default(),
            one_lookup_load: one_lookup_load_default(),
            native_fetch_trace: true,
            sixteen_bit_level: direct::sixteen_bit_admission_level(),
            // A `JitState` does not know the CPU's mode; `CpuGsw::default` refreshes this to the
            // real answer for `GswMode::Gsw586` before it hands the CPU out, and `set_mode` owns
            // it from then on. Seeding `false` here rather than guessing a mode keeps the one
            // computation in one place.
            native_keys_admitted: false,
            direct_barrier_census: direct::barrier_census_default(),
            // No env-var default arm, unlike the census: this instrument is armed only by
            // `Machine::arm_ipe_window_trace`, so a plain `CpuGsw` never pays for it.
            ipe_entry_targets: None,
            smc_heat: direct::SmcHeatMap::default(),
            code_watch: Box::default(),
            fast_map_audit: Box::default(),
            pending_watch_edges: Vec::new(),
            #[cfg(feature = "direct-callout-attribution")]
            direct_callout_attribution: direct::direct_callout_attribution_default(),
        }
    }
}

// Manual Clone/Debug (replacing the prior derive): a clone gets a FRESH code watch (exactly how
// `BlockCache::clone` drops its compiled blocks), never a deep copy of installed native code.
impl Clone for JitState {
    fn clone(&self) -> Self {
        Self {
            direct: self.direct.clone(),
            // CARRIED, unlike the census below, and the asymmetry is deliberate. This is a
            // COMPILE POLICY, not a diagnostic: `CpuGsw::clone` is what the lockstep
            // interpreter-versus-native comparisons build their second role from, so a clone that
            // silently reverted to the default arm would compare a lifted CPU against an unlifted
            // one and report the disagreement as agreement.
            word_at_486: self.word_at_486,
            r15_tables: self.r15_tables,
            watch_page_bit: self.watch_page_bit,
            one_lookup_store: self.one_lookup_store,
            one_lookup_load: self.one_lookup_load,
            native_fetch_trace: self.native_fetch_trace,
            sixteen_bit_level: self.sixteen_bit_level,
            // CARRIED, for the reason spelled out on the field: the clone copies `mode` too, so
            // the cached answer stays correct, and a reset would refuse every key.
            native_keys_admitted: self.native_keys_admitted,
            direct_barrier_census: None,
            // Dropped by clone, exactly as the census is: a diagnostic tally belongs to the run
            // that armed it, and a lockstep clone must not double-count its parent's entries.
            ipe_entry_targets: None,
            smc_heat: self.smc_heat.clone(),
            // A clone gets a fresh, empty watch, exactly as the pre-hoist BlockCache clone
            // produced (its clone built a new cache with a new watch).
            code_watch: Box::default(),
            fast_map_audit: Box::default(),
            pending_watch_edges: Vec::new(),
            #[cfg(feature = "direct-callout-attribution")]
            direct_callout_attribution: None,
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
        self.direct.probe(&mut self.code_watch, key)
    }

    // No emptiness assertion here: consecutive installs/rejects WITHOUT native execution between
    // them are harmless (edges just accumulate) and common in cache-level fixtures. The invariant
    // that matters — INV-W, "no native code runs while edges are pending" — is enforced by the
    // backstop DRAIN at the dispatch boundary (`run_direct_block` sweeps both watches on entry).
    pub(crate) fn install(&mut self, compilation: &direct::Compilation) -> Option<direct::BlockId> {
        self.direct.install(
            &mut self.code_watch,
            &mut self.pending_watch_edges,
            compilation,
        )
    }

    pub(crate) fn reject(&mut self, span: direct::RejectedSpan) {
        self.direct
            .reject(&mut self.code_watch, &mut self.pending_watch_edges, span);
    }

    pub(crate) fn retire_key_for_recompile(&mut self, key: direct::BlockKey) -> bool {
        self.direct
            .retire_key_for_recompile(&mut self.code_watch, key)
    }

    pub(crate) fn clear(&mut self) {
        self.direct.clear(&mut self.code_watch);
    }

    pub(crate) fn invalidate_physical_range(
        &mut self,
        physical: u32,
        width: u32,
        lanes: bool,
    ) -> direct::RangeInvalidation {
        self.direct
            .invalidate_physical_range(&mut self.code_watch, physical, width, lanes)
    }

    /// The shared table-1 base every backend's emitted store checks consult.
    pub(crate) fn native_code_watch_table(&mut self) -> usize {
        self.code_watch.table_base()
    }

    pub(crate) fn code_watch_page_edges(&self) -> u64 {
        self.code_watch.page_edges()
    }

    pub(crate) fn code_watch_page_releases(&self) -> u64 {
        self.code_watch.page_releases()
    }

    pub(crate) fn code_watch_sweep_cleared(&self) -> u64 {
        self.code_watch.sweep_cleared()
    }

    pub(crate) fn code_watch_page_is_watched(&self, page: u32) -> bool {
        self.code_watch.page_is_watched(page)
    }

    pub(crate) fn take_watch_edge_pages(&mut self) -> Vec<u32> {
        std::mem::take(&mut self.pending_watch_edges)
    }

    pub(crate) fn note_watch_sweep_cleared(&mut self, cleared: u64) {
        self.code_watch.note_sweep_cleared(cleared);
    }

    pub(crate) fn reset_code_watch_edge_counters(&mut self) {
        self.code_watch.reset_edge_counters();
    }

    pub(crate) fn range_hits_compiled_code(&self, physical: u32, width: u32) -> bool {
        self.code_watch.range_watched(physical, width)
    }

    /// Retire count only, with the mutable-lane exemption OFF: the pre-lane behaviour of
    /// `invalidate_physical_range`, for the fixtures that predate lanes and assert on that count.
    #[cfg(test)]
    pub(crate) fn retire_physical_range_for_test(&mut self, physical: u32, width: u32) -> usize {
        self.invalidate_physical_range(physical, width, false)
            .blocks
    }

    /// Test-only block-watch mark. Routes its strict edges through the SAME pending buffer the
    /// production install/reject chokes use (design H7), so a fixture using this hook and then
    /// `CpuGsw::sweep_block_watch_edges` (or the `mark_block_code_for_test` wrapper) exercises
    /// the identical coherence path — a hook that bypassed the sweep would devacuate every
    /// watched-store fixture built on it.
    #[cfg(test)]
    pub(crate) fn mark_code_range(&mut self, physical: u32, len: u8) {
        let edges = self.code_watch.acquire_range(physical, u32::from(len));
        self.pending_watch_edges.extend(edges.0);
    }

    /// Allocate the barrier census without going through `IZARRAVM_DIRECT_BARRIER_CENSUS`.
    /// Tests must not set that variable: the whole process shares one environment and the
    /// harness runs threaded, so an env flip is visible to every other test's `JitState`.
    #[cfg(test)]
    pub(crate) fn enable_barrier_census_for_test(&mut self) {
        self.direct_barrier_census = Some(Box::new(direct::DirectBarrierCensus::default()));
    }
}

impl std::fmt::Debug for JitState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JitState")
            .field("direct", &self.direct)
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

// Host-only cache state (F-A8): never influences architectural comparisons, so equality always
// holds, exactly like the block cache and heat map it wraps.
impl PartialEq for JitState {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}
impl Eq for JitState {}
