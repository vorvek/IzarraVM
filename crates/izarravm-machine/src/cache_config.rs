//! Cache geometry, tier costs, and level configuration.
//! This module contains the constant data and functions used by `CacheModel`.

use izarravm_core::{CpuPersona, GswMode};

pub(crate) const CACHE_LINE_BYTES: u32 = 64;
pub(crate) const CACHE_L1_MAX_LINES: usize = (32 * 1024) / CACHE_LINE_BYTES as usize;
pub(crate) const CACHE_L2_MAX_LINES: usize = (512 * 1024) / CACHE_LINE_BYTES as usize;

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

pub(crate) const fn tier_cost(mode: GswMode) -> TierCost {
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
}

pub(crate) const CACHE_TIER_DISABLED_MASK: u32 = u32::MAX;

pub(crate) const fn build_cache_level_config(mode: GswMode) -> CacheLevelConfig {
    let g = cache_geometry(mode);
    let l1_lines = if g.l1_bytes == 0 {
        0
    } else {
        g.l1_bytes / CACHE_LINE_BYTES
    };
    let l2_lines = if g.l2_bytes == 0 {
        0
    } else {
        g.l2_bytes / CACHE_LINE_BYTES
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
    }
}

#[inline(always)]
pub(crate) const fn cache_level_config(mode: GswMode) -> CacheLevelConfig {
    build_cache_level_config(mode)
}

pub(crate) const fn code_fetch_ws(_mode: GswMode) -> u8 {
    0
}
