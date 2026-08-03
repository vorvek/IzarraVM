// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Attribution for the VGA direct-write-token seam, the single cause of 99.45% of doom's
//! whole-map FastMap wipes.
//!
//! `.bench/results/n5-fastmap-20260802/README.md` established WHERE the wipes come from — the
//! port-write path in `bus.rs`, where a change in `Vega::direct_write_token` marks the direct data
//! map dirty — but left WHICH register moves the token as an inference. This census measures it:
//! every token change is attributed to the port, the index register in force at the time, and the
//! byte written, and every token transition is counted in a `before x after` matrix.
//!
//! It also measures the two properties a fix would exploit:
//!
//! - **Coalescing headroom.** The bus only sets a flag; `run.rs` applies the wipe once at a batch
//!   boundary. `applies_same_token` counts the applications whose token equals the one in force at
//!   the PREVIOUS application — a wipe that restored a mapping regime the map was already built
//!   for, and therefore pure loss.
//! - **Burst shape.** `gap_buckets` is a log2 histogram of retired instructions between
//!   consecutive applications, which says whether the map ever gets time to warm.
//!
//! Gated at the call site on `enabled`, read once from `IZARRAVM_VGA_WIPE_CENSUS`, following the
//! `barrier_census_active` pattern: a disabled instrument costs one bool test on a path that is
//! already a device port write.

/// Distinct `(port, selector, value)` keys the histogram can hold before it stops learning new
/// ones. Sized for the whole VGA register file several times over; the seam has at most a handful
/// of live keys in practice, and `key_overflow` reports any that did not fit.
const KEY_SLOTS: usize = 128;

/// Log2 buckets for the instruction gap between consecutive applications: bucket `i` holds gaps in
/// `[2^i, 2^(i+1))`, with bucket 0 also holding a gap of zero.
const GAP_BUCKETS: usize = 32;

/// The token is 0 (no direct mapping), 1 (chained Mode 13h) or 2..=5 (a Mode X plane). One extra
/// row guards against a future token value rather than panicking inside an instrument.
const TOKEN_VALUES: usize = 8;

/// Never a real token, so it cannot collide with a first application whose token is genuinely 0.
const NO_TOKEN: u8 = u8::MAX;

fn census_default() -> bool {
    std::env::var("IZARRAVM_VGA_WIPE_CENSUS").is_ok_and(|value| value != "0")
}

/// One `(port, selector, value)` row of the attribution histogram.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VgaWipeKeyRow {
    /// The I/O port written.
    pub port: u16,
    /// The index register in force for that port when the write happened, or 0 for a port that is
    /// not an indexed data port.
    pub selector: u8,
    /// The byte the guest wrote.
    pub value: u8,
    /// How many token changes this exact write produced.
    pub count: u64,
}

/// A read-only copy of the census, taken after the run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VgaWipeCensusSnapshot {
    pub events: u64,
    pub key_overflow: u64,
    pub applies: u64,
    pub applies_same_token: u64,
    pub rows: Vec<VgaWipeKeyRow>,
    /// `transitions[before][after]`, indexed by token value.
    pub transitions: [[u64; TOKEN_VALUES]; TOKEN_VALUES],
    /// Log2 histogram of retired instructions between consecutive applications.
    pub gap_buckets: [u64; GAP_BUCKETS],
}

#[derive(Debug, Clone)]
pub(crate) struct VgaWipeCensus {
    pub(crate) enabled: bool,
    events: u64,
    keys: [u32; KEY_SLOTS],
    counts: [u64; KEY_SLOTS],
    used: usize,
    key_overflow: u64,
    transitions: [[u64; TOKEN_VALUES]; TOKEN_VALUES],
    applies: u64,
    applies_same_token: u64,
    last_applied_token: u8,
    last_applied_instructions: u64,
    gap_buckets: [u64; GAP_BUCKETS],
}

impl Default for VgaWipeCensus {
    fn default() -> Self {
        Self {
            enabled: census_default(),
            events: 0,
            keys: [0; KEY_SLOTS],
            counts: [0; KEY_SLOTS],
            used: 0,
            key_overflow: 0,
            transitions: [[0; TOKEN_VALUES]; TOKEN_VALUES],
            applies: 0,
            applies_same_token: 0,
            last_applied_token: NO_TOKEN,
            last_applied_instructions: 0,
            gap_buckets: [0; GAP_BUCKETS],
        }
    }
}

// Diagnostic-only, exactly like `PerfCounters` and `FastMapAuditCounters`: an instrument must never
// make two machines compare unequal, or arming it would change canonical-state test outcomes.
impl PartialEq for VgaWipeCensus {
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}
impl Eq for VgaWipeCensus {}

impl VgaWipeCensus {
    /// Record one token change at the port-write seam. `selector` must be sampled BEFORE the write,
    /// since writing an index port changes it.
    pub(crate) fn record_token_change(
        &mut self,
        port: u16,
        selector: u8,
        value: u8,
        before: u8,
        after: u8,
    ) {
        self.events += 1;
        let before_index = usize::from(before).min(TOKEN_VALUES - 1);
        let after_index = usize::from(after).min(TOKEN_VALUES - 1);
        self.transitions[before_index][after_index] += 1;
        let key = (u32::from(port) << 16) | (u32::from(selector) << 8) | u32::from(value);
        if let Some(slot) = self.keys[..self.used].iter().position(|&k| k == key) {
            self.counts[slot] += 1;
        } else if self.used < KEY_SLOTS {
            self.keys[self.used] = key;
            self.counts[self.used] = 1;
            self.used += 1;
        } else {
            self.key_overflow += 1;
        }
    }

    /// Record one batch-boundary application of the wipe. `token` is the token in force at that
    /// moment and `instructions` the retired-instruction count, both read only when armed.
    pub(crate) fn record_apply(&mut self, token: u8, instructions: u64) {
        self.applies += 1;
        if self.last_applied_token == token {
            self.applies_same_token += 1;
        }
        if self.last_applied_token != NO_TOKEN {
            let gap = instructions.saturating_sub(self.last_applied_instructions);
            let bucket = if gap < 2 {
                0
            } else {
                (u64::BITS - 1 - gap.leading_zeros()) as usize
            };
            self.gap_buckets[bucket.min(GAP_BUCKETS - 1)] += 1;
        }
        self.last_applied_token = token;
        self.last_applied_instructions = instructions;
    }

    pub(crate) fn snapshot(&self) -> Option<VgaWipeCensusSnapshot> {
        if !self.enabled {
            return None;
        }
        let mut rows: Vec<VgaWipeKeyRow> = (0..self.used)
            .map(|slot| {
                let key = self.keys[slot];
                VgaWipeKeyRow {
                    port: (key >> 16) as u16,
                    selector: (key >> 8) as u8,
                    value: key as u8,
                    count: self.counts[slot],
                }
            })
            .collect();
        rows.sort_by(|a, b| b.count.cmp(&a.count).then(a.port.cmp(&b.port)));
        Some(VgaWipeCensusSnapshot {
            events: self.events,
            key_overflow: self.key_overflow,
            applies: self.applies,
            applies_same_token: self.applies_same_token,
            rows,
            transitions: self.transitions,
            gap_buckets: self.gap_buckets,
        })
    }
}

#[cfg(test)]
#[path = "vga_wipe_census_test.rs"]
mod vga_wipe_census_test;
