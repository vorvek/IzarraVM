// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! The link graph: publishing a block's edges, refusing the ones that cannot be taken, and tearing
//! them all down again when a block retires.
//!
//! `try_link_inner` is the single admission point for every edge -- static successor, resolved
//! `waiting` entry, and dynamic RET/RETF bind all funnel through it, and its refusal chain is what
//! `stalls.link_refusals` counts. `make_link_visible` is the other half: it stamps a block into
//! `linear_blocks` for the current `link_epoch` and drains the two queues (`resolve_successors`,
//! `resolve_waiting`) that were waiting on it. `unlink_outbound` / `unlink_block` /
//! `remove_waiting_sources` / `retire_block` are the teardown side, and the chain-requirement cut
//! sites they carry are documented in place.
//!
//! `track_physical_key` and `note_page_span` ride along because they sat inside this contiguous
//! run of the `impl` block; they are physical-key bookkeeping rather than link-graph code.
//!
//! This file is a child module of `jit::direct` rather than more text in `direct.rs`, which sat at
//! 4,969 of the 5,000-line source ceiling before the extraction. Being a descendant of the
//! defining module is what lets it reach `BlockCache`'s private fields.

use super::segment_layout::{BAKES_CS_BIT, segment_bit};
use super::{
    BLOCK_PAGE_SHIFT, BlockCache, BlockId, BlockKey, LinkClearCause, LinkRefusal, LinkSource,
    LinkTarget, MAX_BLOCK_IMM_LANES, NO_IMM_LANE, NativeCodeWatch, SegmentIndex,
};

impl BlockCache {
    pub(super) fn try_link(&mut self, source: BlockId, slot: u8, target: BlockId) -> bool {
        self.try_link_inner(source, slot, target, None)
    }

    fn try_link_inner(
        &mut self,
        source: BlockId,
        slot: u8,
        target: BlockId,
        target_eip: Option<u32>,
    ) -> bool {
        let Some(source_index) = self.active_index(source) else {
            self.stalls.link_refusals[LinkRefusal::Inactive as usize] += 1;
            return false;
        };
        let slot_index = usize::from(slot);
        let Some(target_index) = self.active_index(target) else {
            self.stalls.link_refusals[LinkRefusal::Inactive as usize] += 1;
            #[cfg(feature = "direct-link-refusal-census")]
            self.note_direct_link_refused(source_index, slot_index, LinkRefusal::Inactive, target);
            return false;
        };
        let source_block = self.blocks[source_index];
        let target_block = self.blocks[target_index];
        // Split out of the single `||` chain the six conditions used to share, so each refusal
        // names itself. The ORDER is the original chain's order and the short-circuit is
        // preserved, which matters: a stale epoch must be reported before the layout compare,
        // because a stale index's `segment_layouts` entry is not meaningful.
        //
        // The segment arm compares the two ends' CHAIN requirements, not their own snapshots, and
        // the merge it computes here IS the decision: on conflict this refuses with nothing
        // written, and on success the merged requirement is handed to `widen_chain_requirement`
        // AFTER the link is published. The propagation never re-decides this edge.
        //
        // Computed BEHIND the epoch test, not beside it. That is the same short-circuit the split
        // if-chain preserves, and it is load-bearing twice over: a stale index's layout entry is
        // not meaningful, and the epoch arm is a high-frequency refusal that must not start paying
        // for a six-segment merge it never consults.
        let stale_epoch = self.block_link_epochs.get(source_index).copied()
            != Some(self.link_epoch)
            || self.block_link_epochs.get(target_index).copied() != Some(self.link_epoch);
        // A FAR source merges through `far_merge` instead: its snapshot holds the PRE-RETF CS
        // and the target's holds the POST-RETF one, so `link_merge`'s plain `cs` compare refuses
        // every far edge. `far_merge` replaces that compare with INV-FAR-CS, which is a refusal on
        // the TARGET's side and is what makes the linear-keyed cell sound.
        let far = source_block.far_dynamic();
        let chain_merge = (!stale_epoch)
            .then(|| {
                let source_layout = self.chain_layouts[source_index];
                let target_layout = self.chain_layouts[target_index];
                if far {
                    source_layout.far_merge(target_layout)
                } else {
                    source_layout.link_merge(target_layout)
                }
            })
            .flatten();
        // WHICH HALF of `far_merge` refused, computed here so the counter names the invariant
        // rather than the enum variant. A far edge can also lose on an ordinary data-segment
        // conflict, which is the same event `LinkRefusal::SegmentLayout` has always counted; only
        // the CS half is INV-FAR-CS biting, and bar 7 reads exactly that number.
        let far_cs_refusal = far
            && self.chain_layouts[target_index].used
                & (segment_bit(SegmentIndex::Cs) | BAKES_CS_BIT)
                != 0;
        let refusal = if stale_epoch {
            Some(LinkRefusal::StaleEpoch)
        }
        // ABOVE `SegmentLayout` -- and therefore above the outbound fast path below, so even a
        // re-assert of an existing edge is caught. It needs a valid `source_index`, which is why
        // it cannot sit above `Inactive`. `link_source_declined` returns on an emptiness test
        // whenever the governor is not on its `on` arm, so the OFF and `cap` arms pay one
        // compare here and nothing else.
        //
        // The placement is DEFENCE IN DEPTH and is recorded as such rather than claimed to be
        // load-bearing: a mutant that moves this check below the fast path SURVIVES the fixtures,
        // and provably has to, because the decline's own `unlink_outbound` clears BOTH outbound
        // cells, so `outbound[source][slot] == Some(target)` cannot hold for a declined source
        // while it is declined. Kept up here anyway -- the fast path's precondition is a fact
        // about today's decline implementation, and a refusal that depends on one is a refusal
        // waiting to be silently deleted.
        else if self.link_source_declined(source) {
            Some(LinkRefusal::Declined)
        } else if chain_merge.is_none() {
            Some(LinkRefusal::SegmentLayout)
        } else if !source_block.link_compatible(&target_block) {
            Some(LinkRefusal::BlockShape)
        }
        // The dynamic RET PIC path used to layer a strict `has_x87` equality on top of the relaxed
        // rule, in both directions, because it resolves its target at runtime from an arbitrary
        // return address rather than from a compile-time successor shape. Both halves are gone:
        // `emit_completed_dynamic_path` now emits the boundary spill for a float source and
        // selects `integer_entry` for an integer one, which is the whole of what the static path
        // does. `target_eip` no longer changes which edges link, only how the cell is written.
        //
        // An integer source reaching a float target goes through the shared pad. Without one there
        // is no correct address to publish: `body` would enter the target with an unloaded x87
        // register cache. Refusing here leaves the cell on the zero portal, so the exit reports
        // `StaticUnbound` exactly as it did before the pad existed.
        else if !source_block.has_x87 && target_block.has_x87 && self.x87_pad_address().is_none()
        {
            Some(LinkRefusal::MissingX87Pad)
        } else {
            None
        };
        if let Some(refusal) = refusal {
            self.stalls.link_refusals[refusal as usize] += 1;
            // Beside the increment it rides, and NOT inside `SegmentLayout::far_merge`, which is a
            // pure method on a `Copy` struct with access to neither the tally nor `PerfCounters`.
            // No new `LinkRefusal` variant either, so the refusal-census indices stay stable.
            //
            // IT UNDER-REPORTS, DELIBERATELY, AND BAR 7 MUST BE READ AS A LOWER BOUND (code
            // review L-1). The credit lands only when `SegmentLayout` is the BINDING refusal, so a
            // far edge that also trips an earlier arm of the chain above -- `StaleEpoch` most
            // plausibly, since the chain short-circuits in its original order -- is
            // INV-FAR-CS-refused and silent here. Moving the counter above the chain would cost
            // the short-circuit the ordering comment protects and would start pricing a
            // six-segment merge on a high-frequency refusal. The honest denominator for the
            // static half is `blocks_installed_baking_cs`.
            if far_cs_refusal && matches!(refusal, LinkRefusal::SegmentLayout) {
                self.stalls.far_link_refused_cs += 1;
            }
            #[cfg(feature = "direct-link-refusal-census")]
            self.note_direct_link_refused(source_index, slot_index, refusal, target);
            return false;
        }
        if self.outbound[source_index][slot_index] == Some(target) {
            if let Some(target_eip) = target_eip {
                self.link_cells[source_index][slot_index]
                    .set_dynamic(target_eip, self.block_portals[target_index].as_ref());
            }
            #[cfg(feature = "direct-link-refusal-census")]
            self.note_direct_link_linked(source_index, slot_index, target);
            return true;
        }
        self.unlink_outbound(source, slot, LinkClearCause::Replaced);
        // AFTER `unlink_outbound`, which routes through `LinkCell::clear` and resets this to the
        // never-set sentinel. Setting it earlier leaves every cell at `NO_ENTRY_TOP`, the shared
        // x87 pad then bails on every crossing, and the mechanism is inert while every counter
        // gate still passes. Placed beside `mark_spilling`, which has the same ordering
        // requirement for the same reason.
        if let Some(top) = target_block.x87_entry_top() {
            self.link_cells[source_index][slot_index].set_entry_top(top);
        }
        if source_block.has_x87 && !target_block.has_x87 {
            self.link_cells[source_index][slot_index].mark_spilling();
        }
        if let Some(target_eip) = target_eip {
            self.link_cells[source_index][slot_index]
                .set_dynamic(target_eip, self.block_portals[target_index].as_ref());
        } else {
            self.link_cells[source_index][slot_index]
                .set(self.block_portals[target_index].as_ref());
        }
        self.outbound[source_index][slot_index] = Some(target);
        self.inbound.entry(target).or_default().push(LinkSource {
            block: source,
            slot,
        });
        self.stats.links += 1;
        #[cfg(feature = "direct-link-refusal-census")]
        self.note_direct_link_linked(source_index, slot_index, target);
        // AFTER the edge is visible: the propagation walks `inbound`, and this edge's own source
        // may itself be someone's target. `chain_merge` was proved `Some` by the refusal chain
        // above, and that is ALL it is used for here -- the merged VALUE is recomputed inside,
        // from the source's post-cut requirement. See `absorb_published_edge`.
        if chain_merge.is_some() {
            self.absorb_published_edge(source, target, far);
        }
        true
    }

    /// `cause` is passed by the caller rather than inferred: the same helper serves the
    /// relink-replace path in `try_link_inner` and the retirement walk in `unlink_block`, and
    /// nothing inside the helper can tell those apart.
    ///
    /// CUT SITE A of the two that narrow the chain requirement. The narrowing call is at the TAIL
    /// of this function and the placement is load-bearing: this site empties `outbound` FIRST
    /// (the `.take()` below) and clears the cell four lines later, while cut site B in
    /// `unlink_block` does the two in the opposite order. Only tail placement is correct at both.
    /// See `narrow_chain_requirement_if_leaf`, which asserts the order rather than trusting it.
    ///
    /// The `else` arm above the tail returns EARLY and deliberately does not narrow: `outbound`
    /// did not change on that path, so there is nothing for the narrowing to be a consequence of
    /// and a counter bumped there would be reporting an event that did not happen.
    pub(super) fn unlink_outbound(&mut self, source: BlockId, slot: u8, cause: LinkClearCause) {
        let Some(source_index) = self.active_index(source) else {
            return;
        };
        let slot_index = usize::from(slot);
        let Some(target) = self.outbound[source_index][slot_index].take() else {
            self.link_cells[source_index][slot_index].clear();
            return;
        };
        self.link_cells[source_index][slot_index].clear();
        if let Some(inbound) = self.inbound.get_mut(&target) {
            inbound.retain(|link| !(link.block == source && link.slot == slot));
            if inbound.is_empty() {
                self.inbound.remove(&target);
            }
        }
        self.stats.unlinks += 1;
        self.stalls.links_cleared[cause as usize] += 1;
        #[cfg(feature = "direct-link-refusal-census")]
        self.note_direct_link_cleared(source_index, slot_index, cause, target);
        self.narrow_chain_requirement_if_leaf(source_index, cause);
    }

    fn unlink_block(&mut self, id: BlockId) {
        let Some(index) = self.active_index(id) else {
            #[cfg(feature = "smc-census")]
            self.note_smc_census_unlink(false, 0, 0);
            return;
        };
        #[cfg(feature = "smc-census")]
        let mut census_walked = 0u64;
        #[cfg(feature = "smc-census")]
        let mut census_reparked = 0u64;
        self.remove_waiting_sources(id);
        let target_key = LinkTarget {
            linear: self.blocks[index].span.key.linear,
            mode_key: self.blocks[index].span.key.mode_key,
        };
        if self.linear_blocks.get(&target_key) == Some(&id) {
            self.linear_blocks.remove(&target_key);
        }
        if let Some(inbound) = self.inbound.remove(&id) {
            for link in inbound {
                #[cfg(feature = "smc-census")]
                {
                    census_walked += 1;
                }
                let source_index = link.block.index();
                if self.active_index(link.block) == Some(source_index) {
                    let slot = usize::from(link.slot);
                    self.link_cells[source_index][slot].clear();
                    self.outbound[source_index][slot] = None;
                    if let Some(successor) = self.blocks[source_index].successors[slot] {
                        // Review finding M9: `link.block` is the SOURCE, so a self-linking block
                        // re-parks a waiting entry naming `id` right here. That is why the second
                        // `remove_waiting_sources` below is load-bearing and R6's "drop the second
                        // pass" arm cannot be proved. This counter measures how often it happens.
                        #[cfg(feature = "smc-census")]
                        if link.block == id {
                            census_reparked += 1;
                        }
                        self.waiting.entry(successor).or_default().push(link);
                    }
                    self.stats.unlinks += 1;
                    self.stalls.links_cleared[LinkClearCause::Retired as usize] += 1;
                    #[cfg(feature = "direct-link-refusal-census")]
                    self.note_direct_link_cleared(source_index, slot, LinkClearCause::Retired, id);
                    // CUT SITE B, and the LARGEST narrowing population on every row: this walk
                    // clears each predecessor's cell INLINE and never reaches `unlink_outbound`.
                    // At the tail of the per-edge body for the reason site A's doc comment gives.
                    self.narrow_chain_requirement_if_leaf(source_index, LinkClearCause::Retired);
                }
            }
        }
        for slot in 0..2 {
            self.unlink_outbound(id, slot, LinkClearCause::Retired);
        }
        self.remove_waiting_sources(id);
        #[cfg(feature = "smc-census")]
        self.note_smc_census_unlink(true, census_walked, census_reparked);
    }

    pub(super) fn remove_waiting_sources(&mut self, id: BlockId) {
        #[cfg(feature = "smc-census")]
        let census_map_len = self.waiting.len() as u64;
        #[cfg(feature = "smc-census")]
        let mut census_visited = 0u64;
        self.waiting.retain(|_, sources| {
            #[cfg(feature = "smc-census")]
            {
                census_visited += sources.len() as u64;
            }
            sources.retain(|source| source.block != id);
            !sources.is_empty()
        });
        // Phase (e). `retain` walks the WHOLE waiting map, not this block's sources, and
        // `unlink_block` calls it twice — so `waiting_map_len_sum` is what prices the pass, not
        // the call count.
        #[cfg(feature = "smc-census")]
        self.note_smc_census_waiting_retain(
            census_map_len,
            census_visited,
            census_map_len - self.waiting.len() as u64,
        );
    }

    pub(super) fn retire_block(&mut self, watch: &mut NativeCodeWatch, id: BlockId) {
        let Some(index) = self.active_index(id) else {
            // §12.9: a second key naming the same block finds it already retired. Counted, so the
            // per-key kill count and the per-block death count stay separately visible.
            #[cfg(feature = "smc-census")]
            self.note_smc_census_retire(false, 0, 0);
            return;
        };
        let span = self.blocks[index].span;
        #[cfg(feature = "smc-census")]
        let census_decode_slots = self.block_decode_slots[index].len() as u64;
        self.block_portals[index].clear();
        self.block_link_epochs[index] = 0;
        self.unregister_decode_dependencies(id, index);
        if self.blocks[index].dynamic_successor() {
            self.link_sources
                .remove(&self.link_cells[index][0].address());
        }
        self.unlink_block(id);
        #[cfg(feature = "direct-link-refusal-census")]
        self.close_direct_link_rows(index);
        self.block_active[index] = false;
        self.blocks[index].entry = 0;
        self.blocks[index].body_entry = 0;
        self.block_imm_lanes[index] = [NO_IMM_LANE; MAX_BLOCK_IMM_LANES];
        self.block_imm_lane_widths[index] = [0; MAX_BLOCK_IMM_LANES];
        self.free_block_slots
            .push(u16::try_from(index).expect("block slot index must fit its ID"));
        self.live_blocks -= 1;
        watch.release_range(span.key.physical, u32::from(span.guest_len));
        #[cfg(feature = "smc-census")]
        self.note_smc_census_retire(true, u64::from(span.guest_len), census_decode_slots);
    }

    pub(super) fn track_physical_key(&mut self, key: BlockKey) {
        let page = self
            .physical_keys
            .entry(key.physical >> BLOCK_PAGE_SHIFT)
            .or_default();
        // Sorted insert (ties on `physical` may exist across mode/linear keys;
        // their relative order is irrelevant). Insertion is compile/track-time,
        // orders of magnitude rarer than the store-side window scan this order
        // exists for.
        let at = page
            .keys
            .partition_point(|tracked| tracked.physical <= key.physical);
        page.keys.insert(at, key);
    }

    /// Record that `key`'s page roots a span of `guest_len` bytes, widening the
    /// page's invalidation window bound. Called when a key becomes
    /// Compiled/Rejected; `track_physical_key` has always run first.
    pub(super) fn note_page_span(&mut self, key: BlockKey, guest_len: u32) {
        if let Some(page) = self
            .physical_keys
            .get_mut(&(key.physical >> BLOCK_PAGE_SHIFT))
        {
            page.max_span = page.max_span.max(guest_len);
        }
    }

    pub(super) fn make_link_visible(&mut self, id: BlockId) {
        let Some(index) = self.active_index(id) else {
            return;
        };
        if self.block_link_epochs.get(index).copied() == Some(self.link_epoch) {
            if !self.block_portals[index].visible() {
                self.publish_portal(index);
            }
            return;
        }
        self.block_portals[index].clear();
        self.block_link_epochs[index] = self.link_epoch;
        let span = self.blocks[index].span;
        let target = LinkTarget {
            linear: span.key.linear,
            mode_key: span.key.mode_key,
        };
        self.linear_blocks.insert(target, id);
        self.resolve_successors(id);
        self.resolve_waiting(target, id);
        self.publish_portal(index);
    }

    /// Bind one observed near-RET target to a target-checked successor cell. Dynamic targets are
    /// deliberately not added to `waiting`: if the target is not link-visible yet, a later RET
    /// observation retries after normal admission has had a chance to compile it.
    /// `cs_limit` is the LIVE CS limit, read by the caller after the native run and therefore
    /// already the POST-RETF one for a far exit. It closes a hole a chained far entry would
    /// otherwise walk into: `run_direct_block`'s dispatcher entry refuses a block whose
    /// `eip + guest_len - 1` exceeds `cs.limit`, and a CHAINED transfer jumps to `body_offset` and
    /// never reaches that check. That gap is pre-existing for the near PIC and this slice does not
    /// open it -- but the far edge multiplies the population by ~274 M on wolf3d and moves it into
    /// the 64 K-wrap region, where a return to `F000:FFxx` is an ordinary BIOS shape. Closed HERE,
    /// at bind time, where it is one compare that never runs again.
    pub(crate) fn bind_dynamic_successor(
        &mut self,
        site_cell: usize,
        target_eip: u32,
        target_linear: u32,
        mode_key: u32,
        cs_limit: u32,
    ) -> bool {
        let Some(source) = self.link_sources.get(&site_cell).copied() else {
            return false;
        };
        if source.slot != 0 {
            return false;
        }
        let Some(source_index) = self.active_index(source.block) else {
            return false;
        };
        if !self.blocks[source_index].dynamic_successor() {
            return false;
        }
        let target_key = LinkTarget {
            linear: target_linear,
            mode_key,
        };
        let Some(target) = self.linear_blocks.get(&target_key).copied() else {
            return false;
        };
        let far = self.blocks[source_index].far_dynamic();
        // FAR EDGES ONLY, and the gate is about ACCOUNTING rather than about outcome (code review
        // M-1). For a NEAR bind the compare is outcome-inert in the normal path -- `link_merge`
        // refuses any near edge whose two chain layouts disagree on `cs`, `merge_chain` preserves
        // `self.cs`, so a linkable near target was compiled under the LIVE CS record and the
        // compile walk's own limit test already bounds `eip + guest_len - 1` by that CS's limit.
        // It is NOT inert in attribution: when the two CS records do differ, an ungated compare
        // returns before `try_link_inner` is reached, and a refusal main counted as
        // `link_refusals[SegmentLayout]` vanishes with no counter at all, because
        // `far_link_refused_limit` is far-only. That is a silent OFF-arm change to a refusal
        // census the campaign ranks and closes on. If the near arm is ever wanted as a FIDELITY
        // improvement it is a separate decision, with its own counter and its own line in the
        // OFF-arm delta list; it must not ride this slice.
        //
        // Written as `+ guest_len > limit + 1` rather than `+ guest_len - 1 > limit` so a
        // `guest_len` of 0 cannot underflow (code review L-3). Unreachable today -- a compiled
        // block has at least one byte -- but the value comes out of a struct field, and the two
        // forms cost the same.
        if far && let Some(target_index) = self.active_index(target) {
            let guest_len = u64::from(self.blocks[target_index].span.guest_len);
            if u64::from(target_eip) + guest_len > u64::from(cs_limit) + 1 {
                self.stalls.far_link_refused_limit += 1;
                return false;
            }
        }
        // A FAR cell holds a LINEAR, not an EIP: the emitted compare adds the live CS base to the
        // popped offset before it reads the cell. `target_linear` is already the post-RETF one --
        // the caller reads `cs().base` AFTER the native run.
        let cell_key = if far { target_linear } else { target_eip };
        if let Some(slot) = self.outbound[source_index]
            .iter()
            .position(|outbound| *outbound == Some(target))
        {
            return self.try_link_inner(source.block, slot as u8, target, Some(cell_key));
        }
        let slot = self.outbound[source_index]
            .iter()
            .position(Option::is_none)
            .unwrap_or_else(|| usize::from(self.dynamic_next_slots[source_index] & 1));
        self.dynamic_next_slots[source_index] = ((slot + 1) & 1) as u8;
        self.try_link_inner(source.block, slot as u8, target, Some(cell_key))
    }
}
