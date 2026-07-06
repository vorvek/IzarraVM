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

/// One compiled loop-region. `entry_lin`/`d` mirror the decode-line key it was installed under:
/// execution re-validates both (plus the live CS limit) before entering, exactly like a cached
/// decode hit - a region can never be entered from a context its decode would not have hit in.
pub(crate) struct CompiledRegion {
    #[allow(dead_code)] // ponytail: read by the dispatch once the emitter lands
    pub buf: ExecutableBuffer,
    pub entry_lin: u32,
    pub d: bool,
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

    pub(crate) fn get(&self, index: std::num::NonZeroU32) -> Option<&CompiledRegion> {
        self.regions.get(index.get() as usize - 1)
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

    #[test]
    fn install_returns_one_based_indices_and_get_round_trips() {
        let mut table = RegionTable::default();
        assert_eq!(table.len(), 0);
        // A trivial valid buffer: a single RET.
        let buf = ExecutableBuffer::new(&[0xc3]).expect("W^X alloc on a supported host");
        let idx = table.install(CompiledRegion {
            buf,
            entry_lin: 0x0011_0920,
            d: true,
        });
        assert_eq!(idx.get(), 1);
        let region = table.get(idx).expect("installed region is retrievable");
        assert_eq!(region.entry_lin, 0x0011_0920);
        assert!(region.d);
        assert!(table.get(std::num::NonZeroU32::new(2).unwrap()).is_none());
    }
}
