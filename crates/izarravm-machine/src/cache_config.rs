// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Cache geometry, tier costs, and level configuration.
//! This module contains the constant data and functions used by `CacheModel`.

use izarravm_core::{CpuPersona, GswMode};

/// Retained lookup convention for the 386 modes, which have no internal L1.
pub(crate) const CACHE_LINE_BYTES: u32 = 64;

/// The minimum internal lookup line sizes the L1 tag arrays. External L2 uses
/// its own fixed 32-byte line geometry.
pub(crate) const CACHE_MIN_LINE_BYTES: u32 = 16;
pub(crate) const CACHE_L1_MAX_LINES: usize = (32 * 1024) / CACHE_MIN_LINE_BYTES as usize;
pub(crate) const CACHE_L2_MAX_LINES: usize = (512 * 1024) / 32;

/// Internal cache lookup line size: 16 bytes for 486 and 32 for 586.
/// The cacheless 386 modes retain their 64-byte lookup convention.
pub(crate) const fn cache_line_bytes(mode: GswMode) -> u32 {
    match mode.persona() {
        CpuPersona::I386 => CACHE_LINE_BYTES,
        CpuPersona::I486 => 16,
        CpuPersona::I586 => 32,
    }
}

/// `log2(cache_line_bytes)`: the shift that turns a physical address into a line
/// number. Written as a `const fn` loop rather than `trailing_zeros` so the whole
/// family stays `const`.
pub(crate) const fn cache_line_shift(mode: GswMode) -> u32 {
    let mut bytes = cache_line_bytes(mode);
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

/// Fixed board latency tariffs in reference clocks, not literal FSB cycles.
/// Including the two-clock transfer, L2 costs 14/166 MHz (84.3 ns), derived
/// from the 66 MHz external-cache bus; RAM costs 32/166 MHz (192.8 ns).
/// Runtime L1 hits are folded into fast-persona instruction charges.
pub(crate) const fn tier_cost(_mode: GswMode) -> TierCost {
    TierCost {
        l1: 0,
        l2: 12,
        ram: 30,
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct CacheLevelConfig {
    pub(crate) l1_mask: u32,
    pub(crate) l2_mask: u32,
    /// Internal lookup shift. External L2 always uses its separate 32-byte shift.
    pub(crate) line_shift: u32,
}

pub(crate) const CACHE_TIER_DISABLED_MASK: u32 = u32::MAX;

pub(crate) const fn build_cache_level_config(mode: GswMode) -> CacheLevelConfig {
    let g = cache_geometry(mode);
    let line_bytes = cache_line_bytes(mode);
    let l1_lines = if g.l1_bytes == 0 {
        0
    } else {
        g.l1_bytes / line_bytes
    };
    let l2_lines = if g.l2_bytes == 0 { 0 } else { g.l2_bytes / 32 };
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
        line_shift: cache_line_shift(mode),
    }
}

#[inline(always)]
pub(crate) const fn cache_level_config(mode: GswMode) -> CacheLevelConfig {
    build_cache_level_config(mode)
}

pub(crate) const fn code_fetch_ws(_mode: GswMode) -> u8 {
    0
}
