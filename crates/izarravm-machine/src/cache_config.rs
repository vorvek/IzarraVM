// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Cache geometry, tier costs, and level configuration.
//! This module contains the constant data and functions used by `CacheModel`.

use izarravm_core::{CpuPersona, GswMode};

/// The modeled cache line, epoch 1: one 64-byte line for every persona.
///
/// Census row 4 (`dev_docs/2026-09-05-timing-constant-census.md`): no era part
/// this project models has a 64-byte line. A P55C is 32 bytes and a 486DX2 is
/// 16, so epoch 2 gives each persona its own (`cache_line_bytes`). The constant
/// stays because it IS the epoch-1 value and every epoch-1 mask is derived from
/// it; moving it would move epoch 1.
pub(crate) const CACHE_LINE_BYTES: u32 = 64;

/// The SMALLEST line any persona models in any epoch (the 486's 16 bytes), which
/// is what sizes the tag arrays: a smaller line means more lines for the same
/// geometry, so this is the worst case. A live geometry with a larger line uses a
/// smaller mask and simply leaves the tail of the array unused -- the full line
/// number is stored as the tag, so an unused entry can never alias a used one.
pub(crate) const CACHE_MIN_LINE_BYTES: u32 = 16;
pub(crate) const CACHE_L1_MAX_LINES: usize = (32 * 1024) / CACHE_MIN_LINE_BYTES as usize;
pub(crate) const CACHE_L2_MAX_LINES: usize = (512 * 1024) / CACHE_MIN_LINE_BYTES as usize;

/// The modeled line size for `mode` under `epoch`, in bytes. Always a power of
/// two, so `cache_line_shift` is exact.
///
/// Epoch 2 (census row 4): **32 bytes on the I586** (the P55C's L1 line) and
/// **16 on the I486** (the 486DX2's unified-cache line, and `#842`'s 486 arm
/// geometry). The I386 is out of the recalibration's scope and keeps 64.
pub(crate) const fn cache_line_bytes(mode: GswMode, epoch: u32) -> u32 {
    if epoch < 2 {
        return CACHE_LINE_BYTES;
    }
    match mode.persona() {
        CpuPersona::I386 => CACHE_LINE_BYTES,
        CpuPersona::I486 => 16,
        CpuPersona::I586 => 32,
    }
}

/// `log2(cache_line_bytes)`: the shift that turns a physical address into a line
/// number. Written as a `const fn` loop rather than `trailing_zeros` so the whole
/// family stays `const`.
pub(crate) const fn cache_line_shift(mode: GswMode, epoch: u32) -> u32 {
    let mut bytes = cache_line_bytes(mode, epoch);
    let mut shift = 0;
    while bytes > 1 {
        bytes >>= 1;
        shift += 1;
    }
    shift
}

#[derive(Clone, Copy)]
pub(crate) struct CacheGeometry {
    pub(crate) l1_bytes: u32,
    pub(crate) l2_bytes: u32,
}

pub(crate) const fn cache_geometry(mode: GswMode) -> CacheGeometry {
    let geometry = mode.cache_geometry();
    CacheGeometry {
        l1_bytes: geometry.l1.total_kib() as u32 * 1024,
        l2_bytes: geometry.external_kib as u32 * 1024,
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct TierCost {
    pub(crate) l1: u8,
    pub(crate) l2: u8,
    pub(crate) ram: u8,
}

/// Per-tier data wait states. A tier's cost in raw bus clocks is
/// `2 + wait_states` (`BusCycle::clocks_for`), which `scale_bus` then turns into
/// guest clocks.
///
/// EPOCH 2, both fast personas, re-solved against the parts rather than against a
/// benchmark (design section 9.5 / census row 4). `bus_timing` goes to `(1, 1)` in
/// the same commit, so a wait state IS a guest clock: the epoch-1 pairs were
/// solved under `(16,105)` and `(1,3)` and, left alone, would price a 586 L2 hit
/// at 202 clocks (1.22 us) and a 486 L2 hit at 193 (2.9 us) -- absurd for the
/// parts. The L1 entries are 0 because epoch 2 folds the L1-hit data access into
/// the instruction class count (Intel's counts already contain it); see
/// `MachineBus::l1_charges_folded`.
///
/// Pre-registered physical ranges, declared before the slice was measured, with
/// the value chosen inside each:
/// * I586 L2, ws 8..=16 -- a 512 KiB pipelined-burst SRAM on a 66 MHz front-side
///   bus returns a single read in ~4-6 bus clocks = 10-15 CPU clocks at 166 MHz.
///   **12** (a 14-clock hit, 84 ns), the value design section 9.5 names.
/// * I586 RAM, ws 23..=38 -- census row 4's own PC100 reference is a 25-40 clock
///   miss. **30** (a 32-clock access, 193 ns), the value design section 9.5 names.
/// * I486 L2, ws 2..=8 -- a 256 KiB external cache on a 33 MHz bus returns a
///   single read in ~3 bus clocks = 6 CPU clocks at 66 MHz. **5** (7 clocks,
///   106 ns).
/// * I486 RAM, ws 8..=18 -- 70 ns FPM DRAM behind a 33 MHz bus, ~5-7 bus clocks
///   plus the row access = 10-20 CPU clocks. **12** (14 clocks, 212 ns).
///
/// These reach the GUEST bill only through the accurate (non-`flat_data_cost`)
/// class, which neither fast persona uses; on the 486 and 586 they price the
/// Neurketa bandwidth diagnostic, whose bands move with them. The 486's KEPT miss
/// charge (design section 9.9) is **0 and recorded as an under-charge**: slice 0b
/// never ran the `#842` shadow probe on prince-486 / doom-486 / wolf3d-486, and
/// the brief forbids inventing a rate.
pub(crate) const fn tier_cost(mode: GswMode, epoch: u32) -> TierCost {
    if epoch >= 2 {
        match mode.persona() {
            CpuPersona::I386 => {}
            CpuPersona::I486 => {
                return TierCost {
                    l1: 0,
                    l2: 5,
                    ram: 12,
                };
            }
            CpuPersona::I586 => {
                return TierCost {
                    l1: 0,
                    l2: 12,
                    ram: 30,
                };
            }
        }
    }
    match mode.persona() {
        CpuPersona::I386 => TierCost {
            l1: 0,
            l2: 0,
            ram: 3,
        },
        CpuPersona::I486 => TierCost {
            l1: 2,
            l2: 191,
            ram: 250,
        },
        CpuPersona::I586 => TierCost {
            l1: 0,
            l2: 200,
            ram: 255,
        },
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct CacheLevelConfig {
    pub(crate) l1_mask: u32,
    pub(crate) l2_mask: u32,
    /// `log2(cache_line_bytes(mode, epoch))`. The line size is per-persona from
    /// epoch 2 on, so the address-to-line shift travels with the masks it was
    /// derived from rather than sitting as a hardcoded `>> 6` at the lookup.
    pub(crate) line_shift: u32,
}

pub(crate) const CACHE_TIER_DISABLED_MASK: u32 = u32::MAX;

pub(crate) const fn build_cache_level_config(mode: GswMode, epoch: u32) -> CacheLevelConfig {
    let g = cache_geometry(mode);
    let line_bytes = cache_line_bytes(mode, epoch);
    let l1_lines = if g.l1_bytes == 0 {
        0
    } else {
        g.l1_bytes / line_bytes
    };
    let l2_lines = if g.l2_bytes == 0 {
        0
    } else {
        g.l2_bytes / line_bytes
    };
    CacheLevelConfig {
        l1_mask: if l1_lines == 0 {
            CACHE_TIER_DISABLED_MASK
        } else {
            l1_lines - 1
        },
        l2_mask: if l2_lines == 0 {
            CACHE_TIER_DISABLED_MASK
        } else {
            l2_lines - 1
        },
        line_shift: cache_line_shift(mode, epoch),
    }
}

#[inline(always)]
pub(crate) const fn cache_level_config(mode: GswMode, epoch: u32) -> CacheLevelConfig {
    build_cache_level_config(mode, epoch)
}

pub(crate) const fn code_fetch_ws(_mode: GswMode) -> u8 {
    0
}
