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
    pub(crate) direct_barrier_census: Option<Box<direct::DirectBarrierCensus>>,
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
}

impl JitState {
    pub(crate) fn new(direct: direct::BlockCache) -> Self {
        Self {
            direct,
            direct_barrier_census: direct::barrier_census_default(),
            smc_heat: direct::SmcHeatMap::default(),
            code_watch: Box::default(),
        }
    }
}

// Manual Clone/Debug (replacing the prior derive): a clone gets a FRESH code watch (exactly how
// `BlockCache::clone` drops its compiled blocks), never a deep copy of installed native code.
impl Clone for JitState {
    fn clone(&self) -> Self {
        Self {
            direct: self.direct.clone(),
            direct_barrier_census: None,
            smc_heat: self.smc_heat.clone(),
            // A clone gets a fresh, empty watch, exactly as the pre-hoist BlockCache clone
            // produced (its clone built a new cache with a new watch).
            code_watch: Box::default(),
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

    pub(crate) fn install(&mut self, compilation: &direct::Compilation) -> Option<direct::BlockId> {
        self.direct.install(&mut self.code_watch, compilation)
    }

    pub(crate) fn reject(&mut self, span: direct::RejectedSpan) {
        self.direct.reject(&mut self.code_watch, span);
    }

    pub(crate) fn retire_key_for_recompile(&mut self, key: direct::BlockKey) -> bool {
        self.direct
            .retire_key_for_recompile(&mut self.code_watch, key)
    }

    pub(crate) fn clear(&mut self) {
        self.direct.clear(&mut self.code_watch);
    }

    pub(crate) fn invalidate_physical_range(&mut self, physical: u32, width: u32) -> usize {
        self.direct
            .invalidate_physical_range(&mut self.code_watch, physical, width)
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
