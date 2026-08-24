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
    /// The layout the DISPATCHER ENTRY CHECK compares against, and the one place the
    /// `IZARRAVM_CHAIN_ENTRY_CHECK` arm selects an array.
    ///
    /// * ARMED: `chain_layouts[i]` -- this block's own pinned set plus the union of the masks of
    ///   everything reachable through live links. The caller compares it MASKED (`data_matches`),
    ///   which is exactly INV-ENTRY's statement: every segment some block in the live cone pins
    ///   holds the descriptor that cone baked.
    /// * OFF: `segment_layouts[i]`, and the caller keeps `main`'s two-arm expression verbatim.
    ///
    /// **ONE indexed 116-byte copy either way.** It REPLACES `segment_layout`'s fetch on the entry
    /// path rather than adding to it; the 2026-08-18 plan pinned that requirement and a second
    /// copy is mutant M14, which the `#[cfg(test)]` fetch counter below exists to kill. The
    /// counter is bumped by the ONE accessor both arms go through, so a mutant that reads a `Vec`
    /// directly instead of calling it is not caught here -- that shape is a review kill, and the
    /// mutant sweep records it as one.
    ///
    /// `cs_matches` shares this fetch and is unaffected: `chain.cs == own.cs` always, because
    /// every merge constructs `cs: self.cs` and `link_merge` refuses a near edge whose ends
    /// disagree on `cs` before the merge runs.
    pub(crate) fn entry_layout(&self, id: BlockId) -> Option<SegmentLayout> {
        let index = id.index();
        if self.chain_entry_check_armed {
            self.fetch_entry_layout(&self.chain_layouts, index)
        } else {
            self.fetch_entry_layout(&self.segment_layouts, index)
        }
    }

    /// The single counted read behind `entry_layout`. See its doc comment for what the counter is
    /// for; in a non-test build this is `Vec::get(..).copied()` and nothing else.
    fn fetch_entry_layout(&self, from: &[SegmentLayout], index: usize) -> Option<SegmentLayout> {
        #[cfg(test)]
        self.entry_layout_fetches
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        from.get(index).copied()
    }

    /// Whether the entry check reads the chain requirement. Read once per cache, in
    /// `with_entry_cap_and_decode_slots`; see the field and `env_gates::chain_entry_check_armed`.
    pub(crate) fn chain_entry_check_armed(&self) -> bool {
        self.chain_entry_check_armed
    }

    /// Reset `index`'s chain requirement to its own layout when the cut that just ran took its
    /// LAST live outbound edge.
    ///
    /// A requirement is a statement about a LIVE link graph. With `outbound` empty the cone is
    /// this block alone, and `segment_layouts[index].used` is exactly what this block pins --
    /// the same argument `invalidate_translation` makes wholesale, applied per block. Without it
    /// a block that has ever linked keeps a stale-too-WIDE requirement forever, which under the
    /// chain entry check is a REJECT the block's own mask would have passed: up to 2.80% of all
    /// entries on wolf3d-486, measured.
    ///
    /// # KEYED ON `outbound`, NEVER ON `has_linked_successor`
    ///
    /// `LinkCell::linked()` is a VISIBILITY predicate (`links.rs`): it loads the cell's portal and
    /// defers to `BlockPortal::visible`. `suspend_decode_slot` and `compact_arena` clear and
    /// REPUBLISH portals without touching `outbound` and without re-running `try_link_inner`, so
    /// `has_linked_successor` reverts to true with no merge and no widen behind it. Narrowing on
    /// that state gives: hide the successor -> narrow the root -> re-show the successor -> enter
    /// the root under the narrowed mask -> chain into a body against a base nobody validated. A
    /// wrong-base MISCOMPILE, and the census says the hidden state is reached on 12 of 14
    /// row-arms, so it is not theoretical.
    ///
    /// `outbound[..] = Some(..)` has exactly ONE writer, in `try_link_inner`, two statements above
    /// the widen. An edge cannot come back without re-running the merge and the propagation. That
    /// is what makes this predicate safe and the visibility one unsafe.
    ///
    /// # AND NO `debug_assert!(!cell.linked())` HERE, FOR THE SAME REASON
    ///
    /// It would read false for a live-but-hidden edge and true for a cleared-but-republished one,
    /// so it is unsound as a check and a standing invitation to re-key the predicate on it. The
    /// assert below reads `LinkCell::is_cleared`, which is the cell's OWN state and says nothing
    /// about visibility.
    pub(super) fn narrow_chain_requirement_if_leaf(&mut self, index: usize, cause: LinkClearCause) {
        if self.outbound[index] != [None, None] {
            return;
        }
        // THE ORDERING PIN. The two cut sites disagree about the order of their two writes --
        // `unlink_outbound` empties `outbound` (its `.take()`) BEFORE clearing the cell, and
        // `unlink_block`'s inbound walk clears the cell BEFORE emptying `outbound` -- so only
        // TAIL placement is correct at both. Move this call up beside either site's `outbound`
        // write and the narrowing can run while that slot's own cell is still armed, which is a
        // wrong-base miscompile and not merely a lying counter. This assert is what makes that
        // edit fail instead of shipping. `outbound[j] == None` implies cell `j` is cleared at
        // every site that writes the pair, which is why checking BOTH slots is meaningful.
        debug_assert!(
            self.link_cells[index].iter().all(|cell| cell.is_cleared()),
            "the chain requirement may only be narrowed after every outbound cell is cleared",
        );
        self.chain_layouts[index] = self.segment_layouts[index];
        self.stalls.chain_requirement_narrowed[cause as usize] += 1;
    }

    /// The precondition of a WHOLESALE requirement reset: no edge is left to be violated.
    ///
    /// `invalidate_translation` drops every edge and then resets every requirement, and the ORDER
    /// is load-bearing. Reset first and a still-armed cell could reach a body under a requirement
    /// that no longer names the segment that body's cone pins -- stale-too-NARROW, which is the
    /// miscompile direction, and which `main` was insulated from only because its entry check
    /// never read this array. The quantifier can range over ALL slots because an inactive one's
    /// `outbound` is already empty: `retire_block` runs `unlink_block` before it clears
    /// `block_active`.
    pub(super) fn assert_no_live_edge_before_wholesale_reset(&self) {
        debug_assert!(
            self.outbound.iter().all(|slots| *slots == [None, None]),
            "every edge must be dropped before a wholesale chain-requirement reset",
        );
    }

    /// Absorb a freshly published edge `source -> target` into `source`'s chain requirement.
    ///
    /// **The merge is recomputed HERE rather than reused from the refusal decision, and that is
    /// the whole point of this function** (review round 2, R2-1). `try_link_inner` snapshots
    /// `chain_layouts[source]` before its refusal chain and, on the relink-replace path, calls
    /// `unlink_outbound` afterwards -- which may narrow that very entry. Writing the pre-cut
    /// snapshot back would silently undo the narrowing and hand `source` a requirement carrying
    /// bits contributed by the edge that was just cut. Nothing asserts against it: the
    /// monotonicity test below passes, because the pre-cut value contains the narrowed one.
    ///
    /// Recomputing changes NO edge admission. The refusal decision was already taken, above, from
    /// the pre-cut value; this runs only on the admitted path. And the post-cut source can differ
    /// from the pre-cut one only by having a NARROWER `used` -- non-adoption keeps `data` and `cs`
    /// identical to `segment_layouts[source]`'s at all times -- so the recomputed merge is `Some`
    /// wherever the original was, and the `expect` below cannot fire.
    ///
    /// It is not behind `IZARRAVM_CHAIN_ENTRY_CHECK`: it is the other half of the un-gated
    /// narrowing, and gating one without the other would leave the two arms with different link
    /// graphs. See that knob's doc comment for the OFF-arm consequence, which is one-directional.
    pub(super) fn absorb_published_edge(&mut self, source: BlockId, target: BlockId, far: bool) {
        let (Some(source_index), Some(target_index)) =
            (self.active_index(source), self.active_index(target))
        else {
            return;
        };
        let source_layout = self.chain_layouts[source_index];
        let target_layout = self.chain_layouts[target_index];
        let merged = if far {
            source_layout.far_merge(target_layout)
        } else {
            source_layout.link_merge(target_layout)
        };
        let merged = merged.expect(
            "the pre-cut merge was admitted and narrowing the source can only remove mask bits",
        );
        self.widen_chain_requirement(source, merged);
    }

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
