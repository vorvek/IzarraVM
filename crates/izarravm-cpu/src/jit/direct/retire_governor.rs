// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! The data-segment reject governor: the retire cap, the link decline, and the layout census.
//!
//! Design: `dev_docs/specs/2026-08-23-data-segment-reject-treadmill-design.md` (rev 3).
//!
//! The problem, measured on the tombraid loader phase: 307,798 of 314,318 compile attempts (97.9%)
//! are data-segment rejects, and installs track them one for one. A block bakes six segment
//! descriptors at compile time; the dispatcher entry check refuses it when the live records do not
//! match, and then RETIRES the key so the next encounter recompiles for the then-live records.
//! Under a record that ALTERNATES that retire is a bet the block always loses: it is re-specialized
//! for the value it will not see next, so it re-refuses, re-retires, and re-compiles forever while
//! executing natively never.
//!
//! The refusal is the correctness half and is untouched here. Only the RE-SPECIALIZATION BET is
//! governed, exactly as `retire_key_for_top_mismatch` governs the identical x87 failure.
//!
//! This file is a child module of `jit::direct` rather than more text in `direct.rs`, which is
//! within 130 code lines of the 5,000-line source ceiling. Being a descendant of the defining
//! module is what lets it reach `BlockCache`'s private fields.

use std::collections::hash_map::Entry;

use super::{
    BlockCache, BlockId, BlockKey, BlockState, LinkClearCause, NativeCodeWatch, SEGMENT_ORDER,
    segment_bit, segment_index, segment_retire_governor,
};
use crate::SegmentRegister;

/// The three arms of `IZARRAVM_SEGMENT_RETIRE_GOVERNOR`. Parsed in `env_gates.rs` on
/// `parse_retry_lift_arm`'s conventions; see `segment_retire_governor` there for the spelling
/// table and why unset must read as `Off`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SegmentRetireGovernor {
    /// The shipped default: every data-segment reject retires its key, as `main` does. Nothing in
    /// this file writes a map or reads a set on this arm, so an OFF leg is a reproduction of main
    /// and not merely a close relative of one.
    Off,
    /// Stage 1: the per-key retire cap. After the cap the entry still REFUSES (interprets), and
    /// the block stays installed and specialized for whatever layout it froze on.
    Cap,
    /// Stage 1 + stage 2: a suppressed reject that took the STRICT arm also cuts the key's
    /// outbound edges and marks the key link-declined, so the block drops to the check it would
    /// have had if it had never linked.
    On,
}

impl SegmentRetireGovernor {
    pub(crate) fn cap_armed(self) -> bool {
        !matches!(self, Self::Off)
    }
}

/// How many times one key may be retired for a data-segment mismatch before it goes STICKY.
///
/// Three: one first compile plus two re-specializations, one more than the x87 cap
/// (`X87_TOP_RETIRE_CAP = 2`) allows for its one-time-TOP-shift shape. The extra spare is bought
/// against the possibility that the moving record has three stable values rather than two, which
/// is the go/no-go the `distinct_layouts` census in `DataSegmentRetireRecord` exists to answer;
/// the ceiling it implies for the pre-registered compile-attempt bar is `4 * lookup_misses`
/// (4 x 14,205 = 56,820 against an OFF-leg 314,318), which still clears the 250,000 bar with
/// ~7,500 to spare. Not an env knob: the ARM is the knob, the cap is one line.
pub(crate) const DATA_SEGMENT_RETIRE_CAP: u8 = 3;

/// How many distinct masked layouts one key's census will remember. Bounds the cap map's value.
pub(crate) const DATA_SEGMENT_LAYOUT_CENSUS_CAP: usize = 8;

/// Which arm of `run_direct_block`'s data-segment check produced the reject.
///
/// `Strict` is `has_linked_successor(id)`: the entry compares ALL SIX of the block's own
/// descriptors, because a chained transfer runs successor bodies without ever re-entering the
/// dispatcher. `Masked` is the unlinked entry, which compares only the block's own `used` set.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DataSegmentRejectArm {
    Strict,
    Masked,
}

/// Per-key data-segment governor state: the retire budget, and slice (a)'s missing census.
///
/// The census is folded in HERE rather than kept in a map of its own so that it inherits, for
/// free, the containment in `entries` that bounds the budget map — one key set, one set of
/// lifetime hooks, one thing to get wrong.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct DataSegmentRetireRecord {
    /// Retires taken, capped at `DATA_SEGMENT_RETIRE_CAP`.
    spent: u8,
    /// How many of `layouts` are live. Saturates at `DATA_SEGMENT_LAYOUT_CENSUS_CAP`.
    layout_count: u8,
    /// Whether the census stopped recording because it filled. A saturated key reports "at least
    /// 8" and must never be read as exactly 8.
    layouts_saturated: bool,
    /// Fingerprints of the distinct live descriptor tuples this key has been rejected against,
    /// MASKED BY THE REJECTING ARM'S OWN MASK.
    ///
    /// A 32-bit FNV-1a fold of the masked descriptors rather than the raw 24-byte tuple the
    /// design named. The reason is the memory bound: this map's key set is contained in
    /// `entries`, whose cap is `DEFAULT_ENTRY_CAP` (131,072), and eight raw tuples per key would
    /// be 25 MB of worst case for a census. Eight `u32` is 4.7 MB of the same worst case.
    ///
    /// A collision UNDER-reports the distinct count, and the rate is stated per RUN rather than
    /// per key, because per-run is the number a reader of the histogram actually needs. Eight
    /// samples in a 2^32 space is 28 pairs, so ~6.5e-9 per key -- but the loader carries ~14,205
    /// keys, which is **~1e-4 that some key in a run under-reports by one**, not the ~1e-8 the
    /// per-key figure invites. Small enough to grade a go/no-go on, large enough that this is a
    /// fold and not a tuple count.
    layouts: [u32; DATA_SEGMENT_LAYOUT_CENSUS_CAP],
}

impl DataSegmentRetireRecord {
    fn note_layout(&mut self, fingerprint: u32) {
        let live = usize::from(self.layout_count);
        if self.layouts[..live].contains(&fingerprint) {
            return;
        }
        if live == DATA_SEGMENT_LAYOUT_CENSUS_CAP {
            self.layouts_saturated = true;
            return;
        }
        self.layouts[live] = fingerprint;
        self.layout_count += 1;
    }

    pub(crate) fn distinct_layouts(self) -> u8 {
        self.layout_count
    }

    pub(crate) fn layouts_saturated(self) -> bool {
        self.layouts_saturated
    }
}

/// Fold the live descriptors of the segments `mask` names into one 32-bit value.
///
/// The MASK is part of the fold, not just a filter over it: two rejects taken on different arms
/// carry different masks, and a tuple that agrees on the intersection while the masks differ is
/// genuinely a different requirement. Segments outside the mask contribute nothing, which is the
/// whole point — an unmasked census on this loader saturates within a few hundred rejects on
/// records nothing in the chain pins, producing a false no-go for the variant slice.
fn layout_fingerprint(mask: u8, live: &[SegmentRegister; 6]) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    let mut fold = |byte: u8| {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(0x0100_0193);
    };
    fold(mask);
    for segment in SEGMENT_ORDER {
        let bit = segment_bit(segment);
        if mask & bit == 0 {
            continue;
        }
        let descriptor = live[segment_index(segment)];
        for byte in descriptor.selector.to_le_bytes() {
            fold(byte);
        }
        for byte in descriptor.base.to_le_bytes() {
            fold(byte);
        }
        for byte in descriptor.limit.to_le_bytes() {
            fold(byte);
        }
        fold(descriptor.access);
        fold(u8::from(descriptor.default_size_32));
    }
    hash
}

impl BlockCache {
    /// The data-segment half of `retire_key_for_recompile`, governed per key.
    ///
    /// The entry REFUSAL is the caller's and is unconditional on every arm — this decides only
    /// whether the block is also demoted so the next encounter re-specializes it at the then-live
    /// records, and (on `on`) whether the block's outbound edges are cut so it drops to its own
    /// masked check. A key that has spent its budget keeps its block and keeps refusing, so the
    /// worst case for it is permanent interpretation: slow, never wrong.
    ///
    /// `own_mask_matches` is the caller's `data_matches`, computed on this cold path only and
    /// only when the arm is `Strict`. It is what separates "this block rejected BECAUSE it is
    /// linked" from "this block rejected on a record it uses itself" — cutting the edges of the
    /// second kind would buy nothing, because the masked check it would fall back to refuses too.
    ///
    /// ORDERED pre-check first, deliberately, and for two reasons: it mirrors
    /// `retire_key_for_top_mismatch`, so a key that is not `Compiled` never gets a row written for
    /// it and the map's key set stays contained in `entries`; and stage 2 needs the `BlockId`
    /// that lookup produces.
    pub(crate) fn retire_key_for_data_segment(
        &mut self,
        watch: &mut NativeCodeWatch,
        key: BlockKey,
        arm: DataSegmentRejectArm,
        own_mask_matches: bool,
        live: &[SegmentRegister; 6],
    ) -> bool {
        let governor = segment_retire_governor();
        // The caller has already tested this and taken `retire_key_for_recompile` directly on the
        // OFF arm, so that it never builds the two inputs this function needs. The test is
        // repeated here because a second production caller must not be able to acquire the
        // governor by accident, and the fallback is kept rather than made a hard panic because
        // being wrong here costs a re-specialization, not correctness.
        debug_assert!(
            governor.cap_armed(),
            "the OFF arm reaches `retire_key_for_recompile` directly, without paying for the \
             governor's inputs",
        );
        if !governor.cap_armed() {
            return self.retire_key_for_recompile(watch, key);
        }
        let Some(BlockState::Compiled(id)) = self.entries.get(&key).copied() else {
            return false;
        };
        let Some(index) = self.active_index(id) else {
            return false;
        };
        // The arm's OWN mask, per review H3. The strict arm rejected against the CHAIN's
        // requirement, so that is the mask its census must be taken under; the masked arm
        // rejected against the block's own pinned set.
        let mask = match arm {
            DataSegmentRejectArm::Strict => self.chain_layouts[index].used,
            DataSegmentRejectArm::Masked => self.segment_layouts[index].used,
        };
        let fingerprint = layout_fingerprint(mask, live);
        let spent = {
            let record = match self.data_segment_retires.entry(key) {
                Entry::Occupied(occupied) => occupied.into_mut(),
                Entry::Vacant(vacant) => vacant.insert(DataSegmentRetireRecord::default()),
            };
            record.note_layout(fingerprint);
            record.spent
        };
        if spent >= DATA_SEGMENT_RETIRE_CAP {
            self.stalls.data_segment_retires_suppressed += 1;
            // Stage 2 fires on EVERY suppressed strict-arm reject, not once at the crossing: the
            // crossing entry may well have been a masked-arm reject, and a key whose crossing
            // happened to land there would otherwise never decline at all. Idempotent by
            // construction — the cells are already clear and the set already holds the key.
            if matches!(governor, SegmentRetireGovernor::On)
                && matches!(arm, DataSegmentRejectArm::Strict)
                && own_mask_matches
            {
                self.decline_links_for_data_segment(key, id);
            }
            return false;
        }
        if let Some(record) = self.data_segment_retires.get_mut(&key) {
            record.spent = spent + 1;
        }
        if spent + 1 == DATA_SEGMENT_RETIRE_CAP {
            self.stalls.data_segment_sticky_crossings += 1;
        }
        self.retire_key_for_recompile(watch, key)
    }

    /// Stage 2: cut the key's outbound edges and bar them from re-forming.
    ///
    /// With both cells cleared, `LinkCell::linked` is false for both, so `has_linked_successor`
    /// is false and the next entry takes `data_matches` — the exact check this block would have
    /// had if it had never linked. No new predicate, and no body ever runs under a record its
    /// snapshot does not match: the strict arm exists only because a chained transfer skips the
    /// successor's own entry check, and with no live outbound edge there is no chained transfer.
    /// The declined head exits `StaticUnbound` to the dispatcher and the successor is entered
    /// under its OWN check.
    ///
    /// INBOUND edges are untouched. A predecessor P of this block is itself entered on the strict
    /// arm (it has a live outbound cell) and its entry check validates all six of its own
    /// descriptors, which — by the non-adopting merge — are the ones this block and its cone
    /// baked. Cutting P's edge would only cost chaining.
    ///
    /// `chain_layouts` is left alone: it is MONOTONE and is not read by the entry check at all,
    /// so a declined block keeps a stale-too-WIDE chain requirement, which is the direction its
    /// own field comment names as safe.
    fn decline_links_for_data_segment(&mut self, key: BlockKey, id: BlockId) {
        for slot in 0..2 {
            self.unlink_outbound(id, slot, LinkClearCause::DataSegmentDecline);
        }
        // The do-not-park contract's other half: an entry parked before the decline would be
        // retried by every later install at that `LinkTarget` and refused every time.
        self.remove_waiting_sources(id);
        self.data_segment_link_declined.insert(key);
        // TRIPS, not crossings. `data_segment_sticky_crossings` is the one-shot counter; this one
        // says how often the decline actually had to be re-asserted.
        self.stalls.data_segment_link_declines += 1;
    }

    /// Whether `source`'s key is link-declined. The emptiness test is not an optimisation of the
    /// hash lookup so much as a statement about the OFF and `cap` arms, where the set can never
    /// be non-empty and `try_link_inner` must not start paying for a governor that is not armed.
    pub(crate) fn link_source_declined(&self, source: BlockId) -> bool {
        if self.data_segment_link_declined.is_empty() {
            return false;
        }
        self.active_index(source).is_some_and(|index| {
            self.data_segment_link_declined
                .contains(&self.blocks[index].span.key)
        })
    }

    /// Drop one key's decline. Called from `retire_key_for_recompile` (the flag is a statement
    /// about a specific block's live edges, and those do not survive a retire — without this the
    /// recompile is born declined and is permanently unchained for its predecessor incarnation's
    /// judgement) and from `invalidate_translation` (which erases every chain requirement a
    /// decline could have been earned against).
    ///
    /// The BUDGET is deliberately not dropped at either site. It is a statement about the guest's
    /// records at this address, which surviving a recompile is the whole point of, and the two
    /// ways of being wrong are not symmetric: a stale count can only make a block interpret an
    /// entry it could have run natively — bounded by the cap, never consulted once the block
    /// stops rejecting — while a stale decline bars re-linking for the rest of the cache's life.
    pub(crate) fn forget_data_segment_decline(&mut self, key: BlockKey) {
        if self.data_segment_link_declined.is_empty() {
            return;
        }
        self.data_segment_link_declined.remove(&key);
    }

    /// Both governor structures die with their key at `invalidate_physical_range`.
    pub(crate) fn forget_data_segment_state_for_key(&mut self, key: BlockKey) {
        self.data_segment_retires.remove(&key);
        self.forget_data_segment_decline(key);
    }

    /// `reset_storage` and the emptiness assert in `clear`.
    pub(crate) fn clear_data_segment_state(&mut self) {
        self.data_segment_retires.clear();
        self.data_segment_link_declined.clear();
    }

    pub(crate) fn data_segment_state_is_empty(&self) -> bool {
        self.data_segment_retires.is_empty() && self.data_segment_link_declined.is_empty()
    }

    /// The distinct-layout histogram over the keys the cap map currently holds.
    ///
    /// A PARTITION, deliberately: cell `n` for `n` in `0..=8` counts keys that saw EXACTLY `n`
    /// distinct masked layouts and stopped there, and the LAST cell counts keys whose census
    /// SATURATED -- eight recorded and at least one more seen. A saturated key is counted ONLY in
    /// the last cell. The first draft counted it in both, which made the cells sum to more than
    /// the map's length and left "how many keys alternate between exactly two layouts" -- the one
    /// question this census exists to answer -- unreadable without knowing that.
    ///
    /// READ IT OFF THE `cap` LEG. The fingerprint folds the REJECTING ARM'S MASK into itself, and
    /// the `on` arm converts strict-arm keys into masked-arm ones part-way through a run, so a
    /// key that rejected under both masks reports the union of two censuses as one number. The
    /// `cap` leg leaves every key on the arm it started on.
    ///
    /// Read off the LIVE map rather than accumulated, so a key dropped by an invalidation takes
    /// its census with it. On a fixture whose `jit_direct_cache_resets` is 0 and whose loader
    /// phase rewrites no compiled code that is the same number either way; where it is not, the
    /// histogram under-reports and says so here rather than in a footnote.
    pub(crate) fn data_segment_layout_histogram(
        &self,
    ) -> [u64; DATA_SEGMENT_LAYOUT_CENSUS_CAP + 2] {
        let mut histogram = [0u64; DATA_SEGMENT_LAYOUT_CENSUS_CAP + 2];
        for record in self.data_segment_retires.values() {
            let cell = if record.layouts_saturated() {
                DATA_SEGMENT_LAYOUT_CENSUS_CAP + 1
            } else {
                usize::from(record.distinct_layouts())
            };
            histogram[cell] += 1;
        }
        histogram
    }

    #[cfg(test)]
    pub(crate) fn data_segment_retire_record_for_test(
        &self,
        key: BlockKey,
    ) -> Option<DataSegmentRetireRecord> {
        self.data_segment_retires.get(&key).copied()
    }

    #[cfg(test)]
    pub(crate) fn data_segment_link_declined_for_test(&self, key: BlockKey) -> bool {
        self.data_segment_link_declined.contains(&key)
    }

    /// The link-graph door the governor fixtures need. `resolve_successors` is the function
    /// `make_link_visible` calls, and the do-not-park contract lives inside it, so a fixture that
    /// wants to prove "refused once, never parked" has to be able to call exactly this.
    #[cfg(test)]
    pub(crate) fn resolve_successors_for_test(&mut self, source: BlockId) {
        self.resolve_successors(source);
    }

    #[cfg(test)]
    pub(crate) fn waiting_holds_source_for_test(&self, source: BlockId) -> bool {
        self.waiting
            .values()
            .any(|sources| sources.iter().any(|entry| entry.block == source))
    }
}
