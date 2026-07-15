// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! G1 self-modifying-code heat map. A separate structure from the refcounted watch pages: the
//! review proved a counter living in a refcounted page ping-pongs (demotion zeroes refcounts, the
//! page resets, the heat is lost, the code re-admits). Its lifetime is tied to the block cache and
//! it is cleared ONLY in the cache's reset_storage/clear. See
//! dev_docs/specs/2026-07-15-smc-hardening-design.md.

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
#[derive(Default)]
pub(crate) struct SmcHeatMap {
    pages: HashMap<u32, Box<[(u32, u8); 256]>, U32BuildHasher>,
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
    /// return how many of them crossed `threshold` on this bump (a diagnostic of distinct hot
    /// chunks). A store touches at most two chunks here; a block-span check never bumps.
    pub(crate) fn bump(&mut self, physical: u32, width: u32, epoch: u32, threshold: u8) -> u32 {
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
            if before < threshold && after >= threshold {
                newly_hot += 1;
            }
            global_chunk += 1;
        }
        newly_hot
    }

    /// Any 16-byte chunk overlapping `[physical, physical+len)` at or above `threshold` for `epoch`.
    pub(crate) fn span_hot(&self, physical: u32, len: u32, epoch: u32, threshold: u8) -> bool {
        if len == 0 {
            return false;
        }
        let first = physical >> 4;
        let last = physical.wrapping_add(len - 1) >> 4;
        (first..=last).any(|global_chunk| self.effective(global_chunk << 4, epoch) >= threshold)
    }

    pub(crate) fn clear(&mut self) {
        self.pages.clear();
    }
}
