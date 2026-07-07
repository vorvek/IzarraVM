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
    /// disjoint from the `Cpu386` allocation (the step function reborrows both mutably).
    /// Re-stamping after an SMC patch replaces `ctx.slots` wholesale from fresh decodes AND re-
    /// emits the buffer (v2 bakes the add-imm immediates into the emitted bytes, so a self-patch
    /// that changes an immediate requires a fresh emit; see `try_admit`).
    pub ctx: Box<RegionCtx>,
    pub entry_lin: u32,
    pub d: bool,
    /// The region's physical byte span [phys_lo, phys_hi], captured at admission (single-page
    /// by the matcher's containment rule, so contiguity holds). A narrow SMC kill inside it
    /// stales the slot table; see `Cpu386::jit_smc_epoch`.
    pub phys_lo: u32,
    pub phys_hi: u32,
    /// `Cpu386::jit_smc_epoch` at the last matcher validation of `ctx.slots`. Entry requires
    /// equality with the live epoch.
    pub valid_epoch: u32,
}

/// The region table. Index 0 is reserved (DecodeLine stores 1-based `NonZeroU32` indices so the
/// no-region case stays a niche-optimized `None`).
#[derive(Default)]
pub(crate) struct RegionTable {
    regions: Vec<CompiledRegion>,
}

impl RegionTable {
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

    /// Drop every region (e.g. on a mode/level change so stale native code cannot linger even
    /// as unreachable table entries).
    #[allow(dead_code)] // ponytail: wired up with the compile driver
    pub(crate) fn clear(&mut self) {
        self.regions.clear();
    }

    pub(crate) fn len(&self) -> usize {
        self.regions.len()
    }
}

// Transparent accelerator, not architectural state (the DecodeCache pattern): excluded from
// `Cpu386` equality, cloned as empty (a cloned CPU re-compiles), Debug is a summary.
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
mod tests {
    use super::*;

    unsafe extern "C" fn stub_entry(
        _cpu: *mut crate::Cpu386,
        _bus: *mut std::ffi::c_void,
        _ctx: *mut RegionCtx,
    ) -> i64 {
        0
    }

    fn stub_region(entry_lin: u32, d: bool) -> CompiledRegion {
        CompiledRegion {
            // A trivial valid buffer: a single RET (never called by these tests).
            buf: ExecutableBuffer::new(&[0xc3]).expect("W^X alloc on a supported host"),
            entry: stub_entry,
            ctx: Box::new(RegionCtx {
                step_fn: None,
                inline_step_fn: None,
                set_pending_add_fn: None,
                set_shift_flags_fn: None,
                charge_fetch_fn: None,
                bus_clocks_fn: None,
                line_live_fn: None,
                slots: Vec::new(),
                jnz_slot: 0,
                entry_eip: 0,
                raw_clocks: 0,
                insn_count: 0,
                run_total_at_entry: 0,
                bus_at_run_start: 0,
                cap: 0,
                rem0: 0,
                scale_num: 1,
                scale_den: 1,
                d: true,
                exit: Default::default(),
                fault: None,
                halted: false,
            }),
            entry_lin,
            d,
            phys_lo: entry_lin,
            phys_hi: entry_lin + 0x32,
            valid_epoch: 0,
        }
    }

    #[test]
    fn install_returns_one_based_indices_and_get_round_trips() {
        let mut table = RegionTable::default();
        assert_eq!(table.len(), 0);
        let idx = table.install(stub_region(0x0011_0920, true));
        assert_eq!(idx.get(), 1);
        let region = table.get(idx).expect("installed region is retrievable");
        assert_eq!(region.entry_lin, 0x0011_0920);
        assert!(region.d);
        assert!(table.get(std::num::NonZeroU32::new(2).unwrap()).is_none());
        assert!(table.get_mut(idx).is_some());
    }

    #[test]
    fn find_locates_a_region_by_its_decode_line_key() {
        let mut table = RegionTable::default();
        let idx = table.install(stub_region(0x0047_3DF8, true));
        assert_eq!(table.find(0x0047_3DF8, true), Some(idx));
        // Same address under the other D bit is a different key.
        assert_eq!(table.find(0x0047_3DF8, false), None);
        assert_eq!(table.find(0x0011_0920, true), None);
    }
}
