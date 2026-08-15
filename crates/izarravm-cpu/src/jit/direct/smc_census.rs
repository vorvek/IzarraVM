// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Stage A of the SMC census (`dev_docs/smc-census-design.md` §3, §4, §8 layer 1, cut to a
//! stage A by the adversarial review of 2026-08-15).
//!
//! Scope, verbatim from the review's cut: Q1 (top-pages table), Q2 (no-kill versus killing scan
//! split) and Q6 layer 1 (per-phase UNIT counters). `memory.rs` is not touched, there is no
//! per-block row vector and there is no sampled timer, so the store hot path and the install path
//! are outside this instrument's blast radius entirely. Stage A decides R0, R1, R2 and R6.
//!
//! Everything here is compiled out without `--features smc-census` and, when compiled in, is
//! inert until `IZARRAVM_SMC_CENSUS=1` arms it — the `direct-callout-attribution` shape.
//!
//! # What the numbers mean, and what they do not
//!
//! - `keys_killed` is a KEY count, not a distinct-block count (design §9.1): `invalidated += 1`
//!   in `invalidate_physical_range` fires once per key, including `Seen`/`Dormant`/`Rejected`
//!   keys that own no compiled block. It is never relabelled "blocks".
//! - `keys_scanned` is a WINDOW length, not page occupancy (design §9.2, §12.6). `page_keys_len_sum`
//!   is the only counter that answers the occupancy question, and it is new here.
//! - Scan calls and narrow decode kills have different denominators (design §1). The choke 2x2 is
//!   the only licensed way to relate them.
//! - Phases (e) and (f) NEST (`retire_block` -> `unlink_block` -> `remove_waiting_sources`), which
//!   is review finding M8. The unit rows below are therefore disjoint BY CONSTRUCTION: every (f)
//!   row counts work done in `retire_block`/`unlink_block` OUTSIDE `remove_waiting_sources`, and
//!   every (e) row counts work inside it. R6 may add them; it may not add a timed `retire_block`
//!   to a timed `remove_waiting_sources`.
//!
//! # The page table
//!
//! Space-Saving with 64 counters, keyed on `keys_killed` — the quantity R1 ranks (review finding
//! M1). The other six per-page accumulators carry their OWN inherited error fields, so every
//! reported column has an honest `[count - error, count]` bound rather than borrowing the ranked
//! column's bound.
//!
//! The stream Space-Saving observes is "kill events, keyed by page". A page visit that kills
//! nothing therefore never displaces a resident row: it accumulates into the exact totals and, if
//! the page happens to be resident, into that row's context columns. That is what keeps the
//! `k`-counter guarantee sound (a zero-increment item is simply not in the stream) and is also
//! what keeps the table from thrashing on the many pages that are written but never kill.
//!
//! Lookup is an open-addressed `page -> row` side index (review finding M3), not a linear scan
//! per fold: a resident page costs one multiply and one probe. The O(64) minimum search and the
//! index rebuild run only on a displacement, which by construction requires a NON-resident page
//! that killed at least one key.
//!
//! Reported error bound: for the ranked column, `error <= N / 64` where `N` is total
//! `keys_killed` (review finding M2). The snapshot carries `N` and `N / 64` so the reader does
//! not re-derive it.

use crate::{
    DirectSmcCensusPageCounts, DirectSmcCensusPageRow, DirectSmcCensusPhase,
    DirectSmcCensusSnapshot, DirectSmcCensusUnits,
};

/// Space-Saving counter count. See the module note on M2: the worst-case per-row deflation is
/// `N / PAGE_ROWS`, which at 64 leaves R1's 0.60 bar decidable.
const PAGE_ROWS: usize = 64;
/// Open-addressed index slots. Power of two, four times the row count, so a resident lookup is
/// one probe in the overwhelming majority of cases.
const INDEX_SLOTS: usize = 256;
const NO_ROW: u8 = u8::MAX;

/// The seven per-page accumulators. Every one of them carries an inherited error twin on
/// displacement, so `count - error` is a lower bound for each independently.
#[derive(Clone, Copy, Default)]
pub(crate) struct PageCounts {
    /// Per-page VISITS, not per-call: one `invalidate_physical_range` call spanning a page
    /// boundary visits two pages.
    pub(crate) page_visits: u64,
    pub(crate) keys_scanned: u64,
    pub(crate) keys_killed: u64,
    pub(crate) keys_surviving: u64,
    pub(crate) lane_accepts: u64,
    pub(crate) no_kill_visits: u64,
    /// Sum of `page_keys.keys.len()` at scan time — the occupancy figure design §12.6 says
    /// nothing records today.
    pub(crate) page_keys_len_sum: u64,
}

impl PageCounts {
    fn add(&mut self, other: &Self) {
        self.page_visits += other.page_visits;
        self.keys_scanned += other.keys_scanned;
        self.keys_killed += other.keys_killed;
        self.keys_surviving += other.keys_surviving;
        self.lane_accepts += other.lane_accepts;
        self.no_kill_visits += other.no_kill_visits;
        self.page_keys_len_sum += other.page_keys_len_sum;
    }

    fn export(&self) -> DirectSmcCensusPageCounts {
        DirectSmcCensusPageCounts {
            page_visits: self.page_visits,
            keys_scanned: self.keys_scanned,
            keys_killed: self.keys_killed,
            keys_surviving: self.keys_surviving,
            lane_accepts: self.lane_accepts,
            no_kill_visits: self.no_kill_visits,
            page_keys_len_sum: self.page_keys_len_sum,
        }
    }
}

#[derive(Clone, Copy, Default)]
struct PageRow {
    page: u32,
    counts: PageCounts,
    error: PageCounts,
}

struct PageTable {
    rows: [PageRow; PAGE_ROWS],
    used: usize,
    index_row: [u8; INDEX_SLOTS],
    index_page: [u32; INDEX_SLOTS],
    /// Exact, unaffected by displacement. The closure asserts run against THIS, never against the
    /// sum of the rows (Space-Saving counters sum to at least the true total, so a row sum is not
    /// a closure).
    totals: PageCounts,
    slot_claims: u64,
    displacements: u64,
}

impl Default for PageTable {
    fn default() -> Self {
        Self {
            rows: [PageRow::default(); PAGE_ROWS],
            used: 0,
            index_row: [NO_ROW; INDEX_SLOTS],
            index_page: [0; INDEX_SLOTS],
            totals: PageCounts::default(),
            slot_claims: 0,
            displacements: 0,
        }
    }
}

impl PageTable {
    #[inline]
    fn slot_of(page: u32) -> usize {
        let mut hash = page.wrapping_mul(0x9E37_79B1);
        hash ^= hash >> 16;
        (hash as usize) & (INDEX_SLOTS - 1)
    }

    #[inline]
    fn find(&self, page: u32) -> Option<usize> {
        let mut slot = Self::slot_of(page);
        loop {
            let row = self.index_row[slot];
            if row == NO_ROW {
                return None;
            }
            if self.index_page[slot] == page {
                return Some(usize::from(row));
            }
            slot = (slot + 1) & (INDEX_SLOTS - 1);
        }
    }

    fn index_insert(&mut self, page: u32, row: usize) {
        let mut slot = Self::slot_of(page);
        while self.index_row[slot] != NO_ROW {
            slot = (slot + 1) & (INDEX_SLOTS - 1);
        }
        self.index_row[slot] = u8::try_from(row).expect("Space-Saving row index must fit u8");
        self.index_page[slot] = page;
    }

    /// Rebuilt whole rather than tombstoned: a displacement removes one page and adds another,
    /// and open addressing cannot delete in place without breaking probe chains. Displacements
    /// are rare by construction (they need a non-resident page that killed a key), and the
    /// rebuild is 256 stores plus at most 64 inserts.
    fn index_rebuild(&mut self) {
        self.index_row = [NO_ROW; INDEX_SLOTS];
        for row in 0..self.used {
            let page = self.rows[row].page;
            self.index_insert(page, row);
        }
    }

    fn note(&mut self, page: u32, counts: &PageCounts) {
        self.totals.add(counts);
        if let Some(row) = self.find(page) {
            self.rows[row].counts.add(counts);
            return;
        }
        if counts.keys_killed == 0 {
            // Not an event in the ranked stream. See the module note.
            return;
        }
        if self.used < PAGE_ROWS {
            let row = self.used;
            self.rows[row] = PageRow {
                page,
                counts: *counts,
                error: PageCounts::default(),
            };
            self.used += 1;
            self.index_insert(page, row);
            self.slot_claims += 1;
            return;
        }
        let victim = (0..PAGE_ROWS)
            .min_by_key(|row| self.rows[*row].counts.keys_killed)
            .expect("the Space-Saving table is never empty once full");
        let inherited = self.rows[victim].counts;
        let mut promoted = inherited;
        promoted.add(counts);
        self.rows[victim] = PageRow {
            page,
            counts: promoted,
            error: inherited,
        };
        self.slot_claims += 1;
        self.displacements += 1;
        self.index_rebuild();
    }

    fn export(&self) -> Vec<DirectSmcCensusPageRow> {
        let mut rows = self.rows[..self.used]
            .iter()
            .map(|row| {
                assert!(
                    row.error.keys_killed <= row.counts.keys_killed,
                    "Space-Saving error exceeded its own counter"
                );
                DirectSmcCensusPageRow {
                    page: row.page,
                    counts: row.counts.export(),
                    error: row.error.export(),
                }
            })
            .collect::<Vec<_>>();
        rows.sort_by(|left, right| {
            right
                .counts
                .keys_killed
                .cmp(&left.counts.keys_killed)
                .then(left.page.cmp(&right.page))
        });
        rows
    }
}

/// One reporting phase: the whole run, or the pinned instruction window (design §9.6 — both are
/// reported, and the decision rules apply to the pinned window).
#[derive(Default)]
struct Phase {
    units: DirectSmcCensusUnits,
    pages: PageTable,
}

/// Per-page scratch, accumulated inside the existing per-page body and folded once (design §3's
/// "one add per page, not per key" discipline, which `keys_scanned` already follows).
#[derive(Default)]
pub(crate) struct PageAccum {
    pub(crate) counts: PageCounts,
    pub(crate) entries_get_misses: u64,
    pub(crate) survivors_moved: u64,
    pub(crate) drain_calls: u64,
    pub(crate) drain_elements: u64,
    pub(crate) reinserted: bool,
}

/// Per-call scratch, folded once at the end of `invalidate_physical_range`.
#[derive(Default)]
pub(crate) struct CallAccum {
    pub(crate) pages_present: u64,
    pub(crate) keys_surviving: u64,
}

pub(crate) struct SmcCensus {
    /// Retired-instruction clock, stashed by the write choke. `retire_block` and its callees read
    /// it through `in_window`; nothing inside `BlockCache` can reach `PerfCounters` directly, and
    /// this keeps `invalidate_physical_range`'s signature unchanged.
    window: Option<(u64, u64)>,
    in_window: bool,
    clock: u64,
    whole: Phase,
    windowed: Phase,
}

impl SmcCensus {
    fn new(window: Option<(u64, u64)>) -> Self {
        Self {
            window,
            // A window that starts at 0 is live from the first write; anything else waits for the
            // choke's first clock stash.
            in_window: window.is_none_or(|(start, _)| start == 0),
            clock: 0,
            whole: Phase::default(),
            windowed: Phase::default(),
        }
    }

    #[inline]
    fn each(&mut self, mut apply: impl FnMut(&mut Phase)) {
        apply(&mut self.whole);
        if self.in_window {
            apply(&mut self.windowed);
        }
    }

    fn set_clock(&mut self, instructions: u64) {
        self.clock = instructions;
        self.in_window = match self.window {
            None => false,
            Some((start, end)) => instructions >= start && instructions < end,
        };
    }

    fn note_choke(&mut self, block_scan: bool, narrow_kill: bool, wholesale: bool) {
        self.each(|phase| {
            phase.units.choke_calls += 1;
            match (block_scan, narrow_kill) {
                (true, false) => phase.units.choke_block_only += 1,
                (false, true) => phase.units.choke_narrow_only += 1,
                (true, true) => phase.units.choke_both += 1,
                (false, false) => phase.units.choke_neither += 1,
            }
            if wholesale {
                phase.units.choke_wholesale += 1;
            }
        });
    }

    fn note_page(&mut self, page: u32, accum: &PageAccum) {
        let counts = accum.counts;
        self.each(|phase| {
            phase.pages.note(page, &counts);
            phase.units.page_visits += 1;
            phase.units.page_removes += 1;
            phase.units.window_searches += 1;
            phase.units.page_keys_len_sum += counts.page_keys_len_sum;
            phase.units.window_len_sum += counts.keys_scanned;
            phase.units.entries_get_misses += accum.entries_get_misses;
            phase.units.survivors_moved += accum.survivors_moved;
            phase.units.drain_calls += accum.drain_calls;
            phase.units.drain_elements += accum.drain_elements;
            if accum.reinserted {
                phase.units.page_reinserts += 1;
            } else {
                phase.units.page_dropped_empty += 1;
            }
        });
    }

    fn note_absent_page(&mut self) {
        self.each(|phase| {
            phase.units.page_visits += 1;
            phase.units.page_absent += 1;
        });
    }

    fn note_call(&mut self, killed: u64, lane_accepts: u64, keys_scanned: u64, accum: &CallAccum) {
        let surviving = accum.keys_surviving;
        let pages_present = accum.pages_present;
        self.each(|phase| {
            phase.units.scan_calls += 1;
            phase.units.keys_scanned += keys_scanned;
            phase.units.keys_killed += killed;
            phase.units.keys_surviving += surviving;
            phase.units.lane_accept_keys += lane_accepts;
            if pages_present == 0 {
                phase.units.scan_calls_absent_page += 1;
            }
            if killed != 0 {
                phase.units.scan_calls_kill += 1;
                phase.units.keys_kill += keys_scanned;
                // Review finding M7: the waste a presence filter would elide lives here too, not
                // only in calls that killed nothing.
                phase.units.keys_surviving_in_kill_calls += surviving;
            } else if lane_accepts != 0 {
                phase.units.scan_calls_lane_only += 1;
                phase.units.keys_lane_only += keys_scanned;
            } else {
                phase.units.scan_calls_no_kill += 1;
                phase.units.keys_no_kill += keys_scanned;
            }
        });
    }

    fn note_waiting_retain(&mut self, map_len: u64, sources_visited: u64, entries_dropped: u64) {
        self.each(|phase| {
            phase.units.waiting_retain_calls += 1;
            phase.units.waiting_map_len_sum += map_len;
            phase.units.waiting_sources_visited += sources_visited;
            phase.units.waiting_entries_dropped += entries_dropped;
        });
    }

    fn note_retire(&mut self, effective: bool, release_bytes: u64, decode_slots: u64) {
        self.each(|phase| {
            phase.units.retire_calls += 1;
            if effective {
                phase.units.retire_calls_effective += 1;
                phase.units.release_range_bytes += release_bytes;
                phase.units.decode_dependency_slots += decode_slots;
            }
        });
    }

    fn note_unlink(&mut self, effective: bool, inbound_walked: u64, inbound_reparked: u64) {
        self.each(|phase| {
            phase.units.unlink_calls += 1;
            if effective {
                phase.units.unlink_calls_effective += 1;
                phase.units.inbound_links_walked += inbound_walked;
                phase.units.inbound_links_reparked += inbound_reparked;
            }
        });
    }

    #[cold]
    #[inline(never)]
    fn snapshot(&self) -> DirectSmcCensusSnapshot {
        DirectSmcCensusSnapshot {
            window: self.window,
            clock: self.clock,
            whole_run: Self::export_phase("whole_run", &self.whole),
            windowed: Self::export_phase("window", &self.windowed),
        }
    }

    fn export_phase(label: &'static str, phase: &Phase) -> DirectSmcCensusPhase {
        let rows = phase.pages.export();
        let totals = phase.pages.totals;
        let lower_sum: u64 = rows
            .iter()
            .map(|row| row.counts.keys_killed - row.error.keys_killed)
            .sum();
        assert!(
            lower_sum <= totals.keys_killed,
            "Space-Saving lower bounds exceeded the exact kill total"
        );
        DirectSmcCensusPhase {
            label,
            units: phase.units,
            page_totals: totals.export(),
            pages: rows,
            page_slot_claims: phase.pages.slot_claims,
            page_displacements: phase.pages.displacements,
            page_rows_capacity: u32::try_from(PAGE_ROWS).expect("64 fits u32"),
        }
    }
}

pub(crate) fn smc_census_default() -> Option<Box<SmcCensus>> {
    if !matches!(std::env::var("IZARRAVM_SMC_CENSUS").as_deref(), Ok("1")) {
        return None;
    }
    let window = std::env::var("IZARRAVM_SMC_CENSUS_WINDOW").ok().map(|raw| {
        let (start, end) = raw
            .split_once(',')
            .expect("IZARRAVM_SMC_CENSUS_WINDOW is <start>,<end> in retired instructions");
        let start = start
            .trim()
            .parse::<u64>()
            .expect("IZARRAVM_SMC_CENSUS_WINDOW start must parse");
        let end = end
            .trim()
            .parse::<u64>()
            .expect("IZARRAVM_SMC_CENSUS_WINDOW end must parse");
        assert!(
            start < end,
            "IZARRAVM_SMC_CENSUS_WINDOW start must precede its end"
        );
        (start, end)
    });
    Some(Box::new(SmcCensus::new(window)))
}

impl super::BlockCache {
    /// Retired-instruction clock plumbing. One statement in the write choke, inside a `cfg`, so
    /// `invalidate_physical_range` keeps its signature and nothing about the hot path moves when
    /// the feature is off (design §7's clock rule, reused here for the window instead of a
    /// lifetime fold).
    pub(crate) fn smc_census_set_clock(&mut self, instructions: u64) {
        if let Some(census) = self.smc_census.as_mut() {
            census.set_clock(instructions);
        }
    }

    pub(crate) fn note_smc_census_choke(
        &mut self,
        block_scan: bool,
        narrow_kill: bool,
        wholesale: bool,
    ) {
        if let Some(census) = self.smc_census.as_mut() {
            census.note_choke(block_scan, narrow_kill, wholesale);
        }
    }

    pub(crate) fn note_smc_census_page(&mut self, page: u32, accum: &PageAccum) {
        if let Some(census) = self.smc_census.as_mut() {
            census.note_page(page, accum);
        }
    }

    pub(crate) fn note_smc_census_absent_page(&mut self) {
        if let Some(census) = self.smc_census.as_mut() {
            census.note_absent_page();
        }
    }

    pub(crate) fn note_smc_census_call(
        &mut self,
        killed: u64,
        lane_accepts: u64,
        keys_scanned: u64,
        accum: &CallAccum,
    ) {
        if let Some(census) = self.smc_census.as_mut() {
            census.note_call(killed, lane_accepts, keys_scanned, accum);
        }
    }

    pub(crate) fn note_smc_census_waiting_retain(
        &mut self,
        map_len: u64,
        sources_visited: u64,
        entries_dropped: u64,
    ) {
        if let Some(census) = self.smc_census.as_mut() {
            census.note_waiting_retain(map_len, sources_visited, entries_dropped);
        }
    }

    pub(crate) fn note_smc_census_retire(
        &mut self,
        effective: bool,
        release_bytes: u64,
        decode_slots: u64,
    ) {
        if let Some(census) = self.smc_census.as_mut() {
            census.note_retire(effective, release_bytes, decode_slots);
        }
    }

    pub(crate) fn note_smc_census_unlink(
        &mut self,
        effective: bool,
        inbound_walked: u64,
        inbound_reparked: u64,
    ) {
        if let Some(census) = self.smc_census.as_mut() {
            census.note_unlink(effective, inbound_walked, inbound_reparked);
        }
    }

    #[cold]
    #[inline(never)]
    pub(crate) fn smc_census_snapshot(&self) -> Option<DirectSmcCensusSnapshot> {
        Some(self.smc_census.as_ref()?.snapshot())
    }

    #[cfg(test)]
    pub(crate) fn enable_smc_census_for_test(&mut self, window: Option<(u64, u64)>) {
        self.smc_census = Some(Box::new(SmcCensus::new(window)));
    }
}
