// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! G1 self-modifying-code heat map. A separate structure from the refcounted watch pages: the
//! review proved a counter living in a refcounted page ping-pongs (demotion zeroes refcounts, the
//! page resets, the heat is lost, the code re-admits). See
//! dev_docs/specs/2026-07-15-smc-hardening-design.md.
//!
//! Ownership (Track C C1a-pre hoist): the map is a PLAIN FIELD on `CpuGsw`, reached through SPLIT
//! BORROWS so a future backend could share it the way the (now-removed) clif backend once did.
//! Deliberately no `Arc` and no `Mutex`:
//! guest execution is single-threaded by design, so plain `&mut` discipline is the whole
//! synchronization story. Heat is shared history at a physical address; it is cleared ONLY when
//! the ACTIVE backend's cache resets its storage (the reset-coupling counter on `BlockCache`,
//! observed by `CpuGsw::sync_smc_heat`), never by an inactive backend's reset.

use std::collections::HashMap;

use crate::U32BuildHasher;

/// A 16-byte code chunk that took this many *actual* invalidations within one heat epoch is treated
/// as self-modifying and its admission is refused (routed to the interpreter). DOSBox-X uses 4
/// (invalidation_map, cache.h:414).
pub(crate) const SMC_HEAT_THRESHOLD: u8 = 4;

/// Heat-epoch cadence, chosen as the cheapest existing coarse clock: the retired-instruction
/// megacount `perf.instructions >> SMC_HEAT_EPOCH_SHIFT`. A tight churn loop retires far fewer than
/// 2^20 instructions between its rewrites, so its 4 invalidations land in one epoch and demote; a
/// game that rewrites a region once per level load spreads those writes across many epochs, whose
/// counts read as zero (older epoch), so it never demotes. This is the two-epoch aging.
pub(crate) const SMC_HEAT_EPOCH_SHIFT: u32 = 20;

/// 4096-byte page split into 256 sixteen-byte chunks. Each chunk carries `(epoch, count)`; a count
/// is only live while its stamp equals the current epoch (older stamps read as zero), which is the
/// two-epoch aging that keeps normal level-load rewrites from ever accumulating to the threshold.
/// The demotion threshold travels with the map so every consumer applies one policy.
pub(crate) struct SmcHeatMap {
    pages: HashMap<u32, Box<[(u32, u8); 256]>, U32BuildHasher>,
    threshold: u8,
    /// The owning cache's `heat_resets` value this map last synchronized against
    /// (`CpuGsw::sync_smc_heat`). Lives inside the map so it shares the map's
    /// transparent-accelerator equality and clone conventions.
    synced_resets: u64,
}

impl Default for SmcHeatMap {
    fn default() -> Self {
        Self {
            pages: HashMap::default(),
            threshold: SMC_HEAT_THRESHOLD,
            synced_resets: 0,
        }
    }
}

impl SmcHeatMap {
    #[inline]
    pub(crate) fn effective(&self, physical: u32, epoch: u32) -> u8 {
        let chunk = ((physical & 0x0fff) >> 4) as usize;
        self.pages.get(&(physical >> 12)).map_or(0, |chunks| {
            let (stamp, count) = chunks[chunk];
            if stamp == epoch { count } else { 0 }
        })
    }

    /// Bump every 16-byte chunk touched by `[physical, physical+width)` for the current epoch and
    /// return how many of them crossed the threshold on this bump (a diagnostic of distinct hot
    /// chunks). A store touches at most two chunks here; a block-span check never bumps.
    pub(crate) fn bump(&mut self, physical: u32, width: u32, epoch: u32) -> u32 {
        if width == 0 {
            return 0;
        }
        let first = physical >> 4;
        let last = physical.wrapping_add(width - 1) >> 4;
        let mut newly_hot = 0;
        let mut global_chunk = first;
        while global_chunk <= last {
            let byte = global_chunk << 4;
            let chunks = self
                .pages
                .entry(byte >> 12)
                .or_insert_with(|| Box::new([(0u32, 0u8); 256]));
            let slot = &mut chunks[((byte & 0x0fff) >> 4) as usize];
            let before = if slot.0 == epoch { slot.1 } else { 0 };
            let after = before.saturating_add(1);
            *slot = (epoch, after);
            if before < self.threshold && after >= self.threshold {
                newly_hot += 1;
            }
            global_chunk += 1;
        }
        newly_hot
    }

    /// G1 cheap pre-compile gate: is the entry chunk at `physical` hot this epoch?
    pub(crate) fn chunk_hot(&self, physical: u32, epoch: u32) -> bool {
        self.effective(physical, epoch) >= self.threshold
    }

    /// G1 full-span gate: any 16-byte chunk overlapping `[physical, physical+len)` at or above the
    /// threshold for `epoch`.
    pub(crate) fn span_hot(&self, physical: u32, len: u32, epoch: u32) -> bool {
        if len == 0 {
            return false;
        }
        let first = physical >> 4;
        let last = physical.wrapping_add(len - 1) >> 4;
        (first..=last).any(|global_chunk| self.chunk_hot(global_chunk << 4, epoch))
    }

    /// Whether any 16-byte chunk overlapping `[physical, physical + len)` carries a record from
    /// ANY epoch (count > 0). Read-only and non-consuming, unlike `take_stale_stamp`: this is
    /// the disp-lane admission probe, and consuming here would fight `lift_cold_smc_dormant`'s
    /// recovery contract. A record means the chunk took at least one heat-charged kill (a
    /// killed block or a narrow decode kill — `note_code_write_inner` bumps on nothing else),
    /// i.e. the bytes have measured patch history; lane-absorbed patches deliberately stop
    /// refreshing it, so "no record" stays the steady state of code that never needed lanes.
    pub(crate) fn has_record_range(&self, physical: u32, len: u32) -> bool {
        if len == 0 {
            return false;
        }
        let first = physical >> 4;
        let last = physical.wrapping_add(len - 1) >> 4;
        (first..=last).any(|global_chunk| {
            let byte = global_chunk << 4;
            self.pages
                .get(&(byte >> 12))
                .is_some_and(|chunks| chunks[((byte & 0x0fff) >> 4) as usize].1 > 0)
        })
    }

    /// One-shot recovery probe: true when the chunk at `physical` carries a recorded stamp
    /// (count > 0) from an OLDER epoch, meaning its heat has aged out. Consumes the stamp so a
    /// later non-heat Dormant at the same chunk is never spuriously lifted by ancient history.
    pub(crate) fn take_stale_stamp(&mut self, physical: u32, epoch: u32) -> bool {
        let Some(chunks) = self.pages.get_mut(&(physical >> 12)) else {
            return false;
        };
        let slot = &mut chunks[((physical & 0x0fff) >> 4) as usize];
        if slot.1 > 0 && slot.0 != epoch {
            *slot = (0, 0);
            true
        } else {
            false
        }
    }

    pub(crate) fn clear(&mut self) {
        self.pages.clear();
    }

    /// Reset coupling: drop the heat when the owning cache's reset counter has moved since the
    /// last synchronization. See `CpuGsw::sync_smc_heat` for the contract.
    pub(crate) fn sync_resets(&mut self, resets: u64) {
        if self.synced_resets != resets {
            self.clear();
            self.synced_resets = resets;
        }
    }

    #[cfg(test)]
    pub(crate) fn set_threshold(&mut self, threshold: u8) {
        self.threshold = threshold;
    }
}

// The map is a host-only accelerator field on CpuGsw, so it follows the decode-cache
// conventions: clones drop the accumulated heat (the threshold policy is kept), equality always
// holds (transparent to CpuGsw comparisons), and Debug prints terse.
impl Clone for SmcHeatMap {
    fn clone(&self) -> Self {
        Self {
            pages: HashMap::default(),
            threshold: self.threshold,
            synced_resets: 0,
        }
    }
}

impl PartialEq for SmcHeatMap {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}
impl Eq for SmcHeatMap {}

impl std::fmt::Debug for SmcHeatMap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SmcHeatMap {{ {} pages }}", self.pages.len())
    }
}
