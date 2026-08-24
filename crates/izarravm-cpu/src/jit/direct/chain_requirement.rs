// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! The chain requirement: the transitive segment obligation a linked block carries.
//!
//! `BlockCache::chain_layouts` holds, per block, its own pinned segment set plus the union of the
//! masks of everything reachable through live links. `widen_chain_requirement` is the one writer
//! that GROWS a requirement, and it restores the obligation backwards over `inbound` until the
//! graph settles. The invariant and its proof live in
//! `dev_docs/plans/2026-08-18-chain-used-link-mask.md`.
//!
//! This file is a child module of `jit::direct` rather than more text in `direct.rs`, which sat at
//! 4,998 of the 5,000-line source ceiling before the extraction. Being a descendant of the
//! defining module is what lets it reach `BlockCache`'s private fields.

use super::segment_layout::{BAKES_CS_BIT, segment_bit};
use super::{BlockCache, BlockId, LinkClearCause, SegmentIndex, SegmentLayout};

impl BlockCache {
    /// Absorb `merged` as `source`'s chain requirement and push the widening backwards until it
    /// settles. Called only from `try_link_inner`, only after the edge is published.
    ///
    /// The obligation being restored is: for every live edge `P -> Q`, `chain(Q).used` is a
    /// subset of `chain(P).used` and the two agree on every descriptor in `chain(Q).used`. A
    /// widen at `Q` can break that for `Q`'s PREDECESSORS, so the walk is inbound-only; it cannot
    /// break it for `Q`'s successors, because their obligation ranges over their own (unchanged)
    /// masks, and a bit new to `Q` cannot already be in a successor's mask -- if it were, `Q`
    /// would have had it before this widen.
    ///
    /// A predecessor that cannot absorb the widen -- its own frozen descriptor for one of the new
    /// segments disagrees -- has its edge CUT. That arm is reachable and load-bearing even under
    /// the non-adopting merge: equality is demanded at link time over a mask that later GROWS,
    /// so an edge admitted because nobody pinned ES becomes unsound the moment a block downstream
    /// of the target pins ES with a different descriptor.
    ///
    /// Termination: a requirement bit, once set, is never cleared while the block lives, and
    /// there are seven of them -- the six segments plus `BAKES_CS_BIT` -- so each block is pushed
    /// at most seven times per generation.
    pub(super) fn widen_chain_requirement(&mut self, source: BlockId, merged: SegmentLayout) {
        let Some(source_index) = self.active_index(source) else {
            return;
        };
        // ABOVE the no-change return, deliberately. Non-adoption is a property of every merge this
        // function is ever handed, not only of the ones that widen something -- a merge that
        // rewrote the block's descriptors while leaving the mask alone is exactly the silent
        // failure this asserts against, and behind the return it would never be checked.
        debug_assert_eq!(
            self.chain_layouts[source_index].data, merged.data,
            "the non-adopting merge never rewrites a block's own descriptors",
        );
        if self.chain_layouts[source_index].used == merged.used {
            return;
        }
        debug_assert_eq!(
            self.chain_layouts[source_index].used & merged.used,
            self.chain_layouts[source_index].used,
            "a chain requirement may only ever widen",
        );
        self.chain_layouts[source_index] = merged;
        let mut worklist = vec![source];
        while let Some(widened) = worklist.pop() {
            let Some(widened_index) = self.active_index(widened) else {
                continue;
            };
            let requirement = self.chain_layouts[widened_index];
            // Snapshot: `unlink_outbound` below edits this very vector.
            let Some(inbound) = self.inbound.get(&widened).cloned() else {
                continue;
            };
            for link in inbound {
                // An `inbound` entry can name a block whose slot has since been recycled. Widening
                // or cutting on that index would touch a DIFFERENT block's edges; the retirement
                // walk in `unlink_block` guards the same way.
                let Some(predecessor_index) = self.active_index(link.block) else {
                    continue;
                };
                // THE FAR CUT. A far predecessor whose incoming requirement carries either CS
                // bit is UNLINKED rather than merged through.
                //
                // Merging would compare the predecessor's PRE-RETF `data[1]` against a POST-RETF
                // requirement -- two unrelated CS records -- and the ordinary `push cs; call; retf`
                // shape would satisfy that comparison BY COINCIDENCE and then be wrong on a later
                // rebind of the same cell. INV-FAR-CS is a property of the target's requirement,
                // and a requirement GROWS, so the edge predicate alone is not enough: this is what
                // keeps the invariant true after the edge is published.
                if self.blocks[predecessor_index].far_dynamic()
                    && requirement.used & (segment_bit(SegmentIndex::Cs) | BAKES_CS_BIT) != 0
                {
                    self.stalls.far_link_cut_on_widen += 1;
                    self.unlink_outbound(link.block, link.slot, LinkClearCause::ChainWiden);
                    continue;
                }
                match self.chain_layouts[predecessor_index].merge_chain(requirement) {
                    Some(predecessor_merged) => {
                        if predecessor_merged.used != self.chain_layouts[predecessor_index].used {
                            self.chain_layouts[predecessor_index] = predecessor_merged;
                            worklist.push(link.block);
                        }
                    }
                    None => {
                        self.unlink_outbound(link.block, link.slot, LinkClearCause::ChainWiden);
                    }
                }
            }
        }
    }
}
