// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Per-window tally of DISTINCT direct-JIT entry targets, the v2 addition to the windowed IPE
//! trace. MEASUREMENT ONLY, and off unless the trace is armed.
//!
//! Why it exists: a window's IPE says how long the average stint was, and nothing about how many
//! DIFFERENT places the dispatcher kept re-entering. Two windows with identical IPE are different
//! problems if one re-enters four blocks ten million times and the other re-enters four hundred
//! thousand blocks twenty times each. The wolf3d-586 lead needs the second question answered by
//! guest ADDRESS, not by aggregate, so the tally keys on the block's entry LINEAR.
//!
//! COST, stated honestly because this hangs off the hottest path in the backend
//! (`CpuGsw::run_direct_block`, at the site that increments `jit_direct_entries`):
//!
//!   * DISARMED — the default, and every benchmark run — is one `Option<Box<..>>` null test on a
//!     `JitState` field. `JitState` is already loaded and written at that site
//!     (`note_segment_write_block_entry` two lines down), so the test adds no cache line and no
//!     allocation, and the body is `#[inline(never)]` so the hot path grows by a load, a compare
//!     and a not-taken branch. Nothing else changes: no counter moves, no run boundary moves,
//!     and a disarmed run executes the same guest instruction stream it did before v2.
//!   * ARMED costs a `HashMap<u32, u64>` probe per direct entry — a hash, a bucket load, and on
//!     the common repeat-entry path one increment. That is REAL: expect a wall regression on the
//!     order of a few percent on an entry-heavy fixture. An armed run is therefore a MAP of the
//!     workload, never a timing measurement of it, and no wall number may be taken from one.

/// The drained shape of one window's entry-target tally.
///
/// `distinct` counts every target the window saw. `top` carries at most `top_n` of them, so
/// `top.len() < distinct` is the normal case and NOT a discrepancy: the truncated tail is still
/// counted in `distinct` and still summed into `top_total`'s complement. A consumer that wants
/// "how concentrated is this window" reads `top` against `entries`; a consumer that wants "how
/// many places is it re-entering" reads `distinct`, which truncation never touches.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IpeEntryTargets {
    /// Number of DISTINCT entry linears seen in the window. Never affected by top-N truncation.
    pub distinct: u64,
    /// Total entries the tally observed in the window. Equals the sum of every target's count,
    /// including the ones truncated out of `top`.
    pub total: u64,
    /// The `top_n` heaviest targets, `(entry_linear, count)`, descending by count and then by
    /// linear so the order is deterministic across runs.
    pub top: Vec<(u32, u64)>,
}

/// Live per-window accounting. One instance lives in `JitState` behind an `Option<Box<..>>`;
/// `None` is disarmed and is the only state a normal build ever reaches.
///
/// `jit`-gated, unlike `IpeEntryTargets` above: its home is a `JitState` field, and without the
/// JIT there is no direct entry path to tally. The RESULT type stays unconditional because
/// `izarravm-machine` names it in a signature that is not itself gated.
#[cfg(feature = "jit")]
#[derive(Debug, Default)]
pub(crate) struct IpeEntryTally {
    counts: std::collections::HashMap<u32, u64>,
    total: u64,
}

#[cfg(feature = "jit")]
impl IpeEntryTally {
    /// Record one direct entry at `linear`. `#[inline(never)]` deliberately: the caller's hot
    /// path should carry the null test and nothing else, so an armed build does not push the
    /// backend's entry sequence out of its cache lines for a disarmed one's benefit.
    #[inline(never)]
    pub(crate) fn note_entry(&mut self, linear: u32) {
        *self.counts.entry(linear).or_insert(0) += 1;
        self.total += 1;
    }

    /// Snapshot the window WITHOUT clearing it, so the still-open trailing window can be read
    /// from a `&self` borrow after the run returns.
    pub(crate) fn snapshot(&self, top_n: usize) -> IpeEntryTargets {
        let mut top: Vec<(u32, u64)> = self.counts.iter().map(|(&k, &v)| (k, v)).collect();
        // Descending by count, then ASCENDING by linear: a HashMap iterates in an unspecified
        // order, so without the tie-break two runs of the same workload could print different
        // top-N lists and a diff would read as a workload change.
        top.sort_unstable_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        top.truncate(top_n);
        IpeEntryTargets {
            distinct: self.counts.len() as u64,
            total: self.total,
            top,
        }
    }

    /// Start the next window. Keeps the map's capacity: the next window re-enters largely the
    /// same code, so reallocating from empty every window would be the tally's own cost, not the
    /// workload's.
    pub(crate) fn reset(&mut self) {
        self.counts.clear();
        self.total = 0;
    }
}

#[cfg(all(test, feature = "jit"))]
#[path = "ipe_entry_tally_test.rs"]
mod tests;
