// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! The segment snapshot a direct block bakes in, and the two index helpers that address it.
//!
//! Extracted from `jit/direct.rs` as pure code motion for the source-line ceiling. Nothing here
//! changed in the move: the const, the struct, its impl, the free predicate and the two index
//! helpers are the text that file carried, and `direct.rs` re-exports every name, so no caller's
//! path moved with the text.
//!
//! It is a coherent unit rather than an arbitrary slice: `SegmentLayout` is the whole of what a
//! block remembers about the six segment registers, `segment_access_supported` is the one
//! predicate that decides whether a descriptor may be reached at compile time, and `segment_bit`
//! and `segment_index` are the only two ways anything addresses the layout's parallel arrays.
//! A future segment-shaped compile-time question belongs here; a run-time one does not, because
//! nothing in this file is read while emitted code is running.

use crate::{CpuGsw, SegmentIndex, SegmentRegister};

pub(super) const SEGMENT_ORDER: [SegmentIndex; 6] = [
    SegmentIndex::Es,
    SegmentIndex::Cs,
    SegmentIndex::Ss,
    SegmentIndex::Ds,
    SegmentIndex::Fs,
    SegmentIndex::Gs,
];

/// Segment state baked into one direct translation, frozen at compile time. `used` is the PINNED
/// set: the segments this block's emitted code depends on, whether through a baked base or a
/// baked selector.
///
/// A linked target does NOT have to carry an identical snapshot -- it used to, and that rule cost
/// prince-586 its whole wall. What it must do is AGREE on every segment that some block in the
/// chain pins. `BlockCache::chain_layouts` carries that transitive requirement per block (own
/// `used`, plus the union of the masks of everything reachable through live links), and
/// `link_merge` below is the edge predicate over it. Because the merge is NON-ADOPTING -- a bit
/// set on either side demands descriptor EQUALITY -- a chain requirement never names a descriptor
/// its holder's own snapshot does not have, so validating the root's six descriptors
/// (`all_data_matches`, run.rs) still validates every body reached through its successor cells.
/// dev_docs/plans/2026-08-18-chain-used-link-mask.md has the invariant and its proof.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SegmentLayout {
    pub(super) cs: SegmentRegister,
    pub(super) data: [SegmentRegister; 6],
    pub(super) used: u8,
}

impl SegmentLayout {
    /// `pinned_segments` is the PINNED set — every segment `data_matches` will compare on entry —
    /// while the accessibility check below runs over the ACCESSED set only. The two used to be the
    /// same mask; they came apart when `MovSegToReg` arrived, which needs a segment's selector
    /// pinned without asserting anything about whether memory can be reached through it.
    ///
    /// The caller accumulates the pinned set through `DirectKind::pinned_segments`, which is the
    /// single definition of the question. This used to take the selector mask and OR the three
    /// together here, which put the union in one place and the question in three.
    pub(crate) fn capture(
        cpu: &CpuGsw,
        read_segments: u8,
        write_segments: u8,
        pinned_segments: u8,
    ) -> Option<Self> {
        debug_assert_eq!(
            pinned_segments & (read_segments | write_segments),
            read_segments | write_segments,
            "every accessed segment must also be pinned",
        );
        let data = SEGMENT_ORDER.map(|segment| cpu.registers.segment(segment));
        let used = pinned_segments;
        for segment in SEGMENT_ORDER {
            let bit = segment_bit(segment);
            if (read_segments | write_segments) & bit == 0 {
                continue;
            }
            let descriptor = data[segment_index(segment)];
            if !segment_access_supported(
                cpu,
                descriptor,
                read_segments & bit != 0,
                write_segments & bit != 0,
            ) {
                return None;
            }
        }
        Some(Self {
            cs: cpu.registers.cs(),
            data,
            used,
        })
    }

    pub(crate) fn cs_matches(self, cpu: &CpuGsw) -> bool {
        self.cs == cpu.registers.cs()
    }

    pub(crate) fn data_matches(self, cpu: &CpuGsw) -> bool {
        SEGMENT_ORDER.into_iter().all(|segment| {
            self.used & segment_bit(segment) == 0
                || self.data[segment_index(segment)] == cpu.registers.segment(segment)
        })
    }

    pub(crate) fn all_data_matches(self, cpu: &CpuGsw) -> bool {
        SEGMENT_ORDER
            .into_iter()
            .all(|segment| self.data[segment_index(segment)] == cpu.registers.segment(segment))
    }

    /// Merge two CHAIN requirements, NON-ADOPTING: the result requires the UNION of the two
    /// masks, and every segment in that union must carry the SAME descriptor on both sides.
    /// `None` is a conflict -- no requirement satisfies both ends at once.
    ///
    /// Non-adoption is what makes the result's descriptors identical to `self`'s: the two agree
    /// wherever the result's mask can ever be read, so the merge only ever moves `used`. The
    /// adopting variant (take the pinning side's descriptor for a bit only one side sets) admits
    /// another 25.9% of prince's refusals but is unsound until the dispatcher entry check reads
    /// the chain requirement instead of the block's own; it is a separate slice.
    ///
    /// Why the mask has to be TRANSITIVE, and not just this edge's two ends: a chained transfer
    /// jumps into the successor's body without returning to the dispatcher, so no block except
    /// the chain root ever runs a segment check. With `R -> S` admitted on a differing ES that
    /// neither pins, and `S -> T` admitted because `S` and `T` agree on the ES that `T` pins,
    /// entering at `R` validates `R`'s ES and then runs `T`'s body against `S`'s ES base. That
    /// is why `BlockCache` propagates the requirement backwards on every widen and cuts the edges
    /// that cannot follow.
    pub(crate) fn merge_chain(self, target: Self) -> Option<Self> {
        let used = self.used | target.used;
        for segment in SEGMENT_ORDER {
            let index = segment_index(segment);
            if used & segment_bit(segment) != 0 && self.data[index] != target.data[index] {
                return None;
            }
        }
        Some(Self {
            cs: self.cs,
            data: self.data,
            used,
        })
    }

    /// The static-edge predicate and its product in one call: `None` refuses the edge, `Some` is
    /// the source's widened chain requirement. `cs` is compared plain and stays out of the merge
    /// (0 of prince-586's 107,088 refusals differed on CS, and `cs_matches` at the entry check is
    /// a separate gate).
    pub(crate) fn link_merge(self, target: Self) -> Option<Self> {
        if self.cs != target.cs {
            return None;
        }
        self.merge_chain(target)
    }

    /// The pinned selector for `segment`, from whichever of the two snapshots holds it. CS lives
    /// in its own field and is pinned for every block; the other five must be in `used`, which
    /// `DirectKind::selector_segment` is what guarantees for a `MovSegToReg` slot.
    pub(crate) fn selector(self, segment: SegmentIndex) -> u16 {
        if segment == SegmentIndex::Cs {
            return self.cs.selector;
        }
        debug_assert_ne!(self.used & segment_bit(segment), 0);
        self.data[segment_index(segment)].selector
    }

    pub(crate) fn descriptor(self, segment: SegmentIndex) -> SegmentRegister {
        debug_assert_ne!(self.used & segment_bit(segment), 0);
        self.data[segment_index(segment)]
    }
}

pub(super) fn segment_access_supported(
    cpu: &CpuGsw,
    descriptor: SegmentRegister,
    read: bool,
    write: bool,
) -> bool {
    if !cpu.is_protected_mode() || cpu.is_v86_mode() {
        return true;
    }
    let access = descriptor.access;
    if access & 0x80 == 0 || access & 0x10 == 0 {
        return false;
    }
    let code = access & 0x08 != 0;
    let expand_down = !code && access & 0x04 != 0;
    if expand_down || (read && code && access & 0x02 == 0) {
        return false;
    }
    !write || (!code && access & 0x02 != 0)
}

pub(super) const fn segment_bit(segment: SegmentIndex) -> u8 {
    match segment {
        SegmentIndex::Es => 1 << 0,
        SegmentIndex::Cs => 1 << 1,
        SegmentIndex::Ss => 1 << 2,
        SegmentIndex::Ds => 1 << 3,
        SegmentIndex::Fs => 1 << 4,
        SegmentIndex::Gs => 1 << 5,
    }
}

pub(super) const fn segment_index(segment: SegmentIndex) -> usize {
    match segment {
        SegmentIndex::Es => 0,
        SegmentIndex::Cs => 1,
        SegmentIndex::Ss => 2,
        SegmentIndex::Ds => 3,
        SegmentIndex::Fs => 4,
        SegmentIndex::Gs => 5,
    }
}
