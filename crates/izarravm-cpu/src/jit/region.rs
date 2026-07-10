//! Compiled loop-regions: the table that owns them and the execution contract between a region
//! and `run_straight_line`.
//!
//! A region is native x86-64 code for ONE guest loop (a straight-line body whose back-edge
//! branches to its own first instruction). It is installed by stamping its 1-based table index
//! into the `DecodeLine` of its entry address (`DecodeLine::jit_region`), so admission rides the
//! decode-cache lookup the continuation loop already performs - zero extra lookups - and every
//! whole-cache invalidation (generation bump) or line re-decode (`put` clears the slot) drops
//! the region for free. The table itself is only GC'd wholesale (`clear`); a dropped slot just
//! goes cold, which is fine at the "a handful of regions" scale the spike targets.
//!
//! EXECUTION CONTRACT (settled invariants from the seed post-mortem):
//! - The region runs INSIDE `run_straight_line`, replacing interpreted continuations. Its side
//!   exits land at exactly the boundaries the interpreter run would have broken at: halted,
//!   `requires_step_break` (port I/O / pending soft-int), an interrupt-enable transition, the
//!   scaled-clock cap, or any fault (rewound + delivered through the normal path).
//! - Register/ALU work is inlined against `gpr[]` and the pending-flags state; memory operands
//!   go through the `extern "C"` trampolines (`callbacks.rs` pattern: fault-rewind-then-negative
//!   convention). The BIOS stub window 0xFF000..0xFF400 is a no-compile zone (the HLE fetch
//!   seam must keep seeing those fetches).
//! - Guest-visible identity is the gate: cyc/aux byte-identical in all four modes, conformance
//!   exact, the Doom/Quake anchors unmoved. The region's only legal observable is wall time.

use super::exec_mem::ExecutableBuffer;
use super::step::{RegionCtx, RegionEntryFn};

/// One compiled loop-region. `entry_lin`/`d` mirror the decode-line key it was installed under:
/// execution re-validates both (plus the live CS limit) before entering, exactly like a cached
/// decode hit - a region can never be entered from a context its decode would not have hit in.
pub(crate) struct CompiledRegion {
    /// Kept alive for `Drop` (frees the W^X memory `entry` points into); never read after
    /// construction.
    #[allow(dead_code)]
    pub buf: ExecutableBuffer,
    /// The emitted code's entry point, into `buf`.
    pub entry: RegionEntryFn,
    /// The region's slot table and per-entry mailbox. Boxed so its address is stable and
    /// disjoint from the `CpuGsw` allocation (the step function reborrows both mutably).
    /// Re-stamping after an SMC patch replaces `ctx.slots` wholesale from fresh decodes AND re-
    /// emits the buffer (v2 bakes the add-imm immediates into the emitted bytes, so a self-patch
    /// that changes an immediate requires a fresh emit; see `try_admit`).
    pub ctx: Box<RegionCtx>,
    pub entry_lin: u32,
    pub d: bool,
    /// The region's physical byte span [phys_lo, phys_hi], captured at admission (single-page
    /// by the matcher's containment rule, so contiguity holds). A narrow SMC kill inside it
    /// stales the slot table; see `CpuGsw::jit_smc_epoch`.
    pub phys_lo: u32,
    pub phys_hi: u32,
    /// `CpuGsw::jit_smc_epoch` at the last builder validation of `ctx.slots`. Entry requires
    /// equality with the live epoch.
    pub valid_epoch: u32,
    /// Whether the block is a self-loop (native back-edge) or linear. Copied into `ctx.is_loop` on
    /// every entry; `region_step` reads it at the terminal slot.
    pub is_loop: bool,
    /// The CPU mode/size bitmask (`CpuGsw::jit_mode_key`) the block was compiled for. Entry
    /// requires equality with the live mode key, so a block compiled for one mode is never reused
    /// in another at the same phys/d (spec §2.2). A mismatch is a miss: unstamp and re-admit.
    pub mode_key: u32,
    /// Whether this region emitted any native cost-fold LOAD slot (`IZARRAVM_JIT_FOLD` on + fold-eligible
    /// at admission). Those slots assume a FLAT DS (base 0, limit max) so EA == linear, but DS is a
    /// runtime value NOT in `mode_key`; `run_region` re-checks DS flatness per entry when this is set and
    /// bails to the interpreter if DS is no longer flat. Regions without native fold slots skip the check.
    pub has_native_fold: bool,
    /// Whether this region emitted a native cost-fold STORE slot. Those additionally assume DS is
    /// WRITABLE (a `data_write_pages` HIT only proves the physical page was writable via some segment,
    /// not that the current DS permits writes), which is also a runtime value not in `mode_key`; when set
    /// `run_region` re-checks DS writability per entry and bails if DS is now read-only (else the native
    /// store would silently write where the interpreter #GPs).
    pub has_native_store: bool,
}

/// Upper bound on live compiled regions. `find()` reuses a line's entry on re-admission, so the
/// table grows only with DISTINCT hot loops, not with re-decode churn; this caps that growth. When
/// it is hit the table is dropped wholesale and the decode generation is bumped (so no stale stamp
/// survives) - a coarse GC that also adapts to workload phase shifts (the post-clear table refills
/// with whatever is hot now). The single-phase anchors have far fewer hot loops than this, so it
/// never fires there.
// ponytail: whole-table clear-on-full, not per-entry LRU. Upgrade to LRU eviction only if a
// phase-shifting workload is measured re-warming often (jit_table_clears climbing).
pub(crate) const JIT_REGION_TABLE_CAP: usize = 1024;

/// The region table. Index 0 is reserved (DecodeLine stores 1-based `NonZeroU32` indices so the
/// no-region case stays a niche-optimized `None`).
#[derive(Default)]
pub(crate) struct RegionTable {
    regions: Vec<CompiledRegion>,
    /// Whether the continuation seam admits hot loops automatically. Direct CPU users start with
    /// it off; `Machine` applies the production environment policy. Lives here (not on `CpuGsw`)
    /// so it is excluded from CPU equality via this table's always-equal `PartialEq`. Setting it
    /// never makes two otherwise identical CPUs compare unequal, which the differential suite
    /// relies on.
    auto_admit: bool,
}

impl RegionTable {
    /// Whether hotness-driven admission is enabled (see `auto_admit`).
    pub(crate) fn auto_admit(&self) -> bool {
        self.auto_admit
    }

    /// Enable or disable hotness-driven admission.
    pub(crate) fn set_auto_admit(&mut self, on: bool) {
        self.auto_admit = on;
    }

    /// Install a region and return the 1-based index to stamp into the entry's decode line.
    pub(crate) fn install(&mut self, region: CompiledRegion) -> std::num::NonZeroU32 {
        self.regions.push(region);
        std::num::NonZeroU32::new(self.regions.len() as u32).expect("len >= 1 after push")
    }

    /// Shared-borrow lookup; the dispatch itself uses `get_mut` (it resets the ctx), so this is
    /// exercised by tests only today.
    #[cfg(test)]
    pub(crate) fn get(&self, index: std::num::NonZeroU32) -> Option<&CompiledRegion> {
        self.regions.get(index.get() as usize - 1)
    }

    pub(crate) fn get_mut(&mut self, index: std::num::NonZeroU32) -> Option<&mut CompiledRegion> {
        self.regions.get_mut(index.get() as usize - 1)
    }

    /// Whether the written physical span intersects any installed region's span. Called from
    /// the narrow-SMC path; the table holds a handful of regions by design.
    pub(crate) fn covers_physical(&self, physical: u32, width: u32) -> bool {
        let last = physical.wrapping_add(width.saturating_sub(1));
        self.regions
            .iter()
            .any(|r| physical <= r.phys_hi && last >= r.phys_lo)
    }

    /// The already-installed region for this decode-line key, if any: the re-stamp path (an SMC
    /// patch killed the line's stamp, the loop re-warmed) refreshes it instead of re-emitting.
    /// A linear scan; the table holds a handful of regions by design.
    pub(crate) fn find(&self, entry_lin: u32, d: bool) -> Option<std::num::NonZeroU32> {
        self.regions
            .iter()
            .position(|r| r.entry_lin == entry_lin && r.d == d)
            .map(|i| std::num::NonZeroU32::new(i as u32 + 1).expect("positions are 0-based"))
    }

    /// Drop every region (W^X-alloc failure, mode/level change, or capacity GC in `try_admit`).
    /// The caller MUST bump the decode generation afterward so no `DecodeLine` stamp still points
    /// into the now-empty table.
    pub(crate) fn clear(&mut self) {
        self.regions.clear();
    }

    pub(crate) fn len(&self) -> usize {
        self.regions.len()
    }
}

// Transparent accelerator, not architectural state (the DecodeCache pattern): excluded from
// `CpuGsw` equality, cloned as empty (a cloned CPU re-compiles), Debug is a summary.
impl PartialEq for RegionTable {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}
impl Eq for RegionTable {}

impl Clone for RegionTable {
    fn clone(&self) -> Self {
        Self::default()
    }
}

impl std::fmt::Debug for RegionTable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "RegionTable {{ {} regions }}", self.len())
    }
}

#[cfg(test)]
#[path = "region_test.rs"]
mod tests;
