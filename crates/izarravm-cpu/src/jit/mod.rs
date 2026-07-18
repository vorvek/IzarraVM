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
    pub(crate) smc_heat: direct::SmcHeatMap,
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
}

impl JitState {
    pub(crate) fn new(direct: direct::BlockCache) -> Self {
        Self {
            direct,
            smc_heat: direct::SmcHeatMap::default(),
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
            smc_heat: self.smc_heat.clone(),
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
        }
    }
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
