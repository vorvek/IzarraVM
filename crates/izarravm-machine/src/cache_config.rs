//! Cache geometry, tier costs and level config (Phase 3 carve).
//! Pure const data and const fns extracted from machine/lib.rs monolith.
//! No state, no hot-path logic; used by CacheModel.

use izarravm_cpu::CpuLevel;

pub(crate) const CACHE_LINE_BYTES: u32 = 64;
pub(crate) const CACHE_L1_MAX_LINES: usize = (32 * 1024) / CACHE_LINE_BYTES as usize;
pub(crate) const CACHE_L2_MAX_LINES: usize = (512 * 1024) / CACHE_LINE_BYTES as usize;

#[derive(Clone, Copy)]
pub(crate) struct CacheGeometry {
    pub(crate) l1_bytes: u32,
    pub(crate) l2_bytes: u32,
}

pub(crate) const fn cache_geometry(level: CpuLevel) -> CacheGeometry {
    match level {
        CpuLevel::I286 => CacheGeometry {
            l1_bytes: 0,
            l2_bytes: 0,
        },
        CpuLevel::I386 => CacheGeometry {
            l1_bytes: 0,
            l2_bytes: 64 * 1024,
        },
        CpuLevel::I486 => CacheGeometry {
            l1_bytes: 16 * 1024,
            l2_bytes: 128 * 1024,
        },
        CpuLevel::I586 => CacheGeometry {
            l1_bytes: 32 * 1024,
            l2_bytes: 512 * 1024,
        },
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct TierCost {
    pub(crate) l1: u8,
    pub(crate) l2: u8,
    pub(crate) ram: u8,
}

pub(crate) const fn tier_cost(level: CpuLevel) -> TierCost {
    match level {
        CpuLevel::I286 => TierCost {
            l1: 0,
            l2: 0,
            ram: 0,
        },
        CpuLevel::I386 => TierCost {
            l1: 0,
            l2: 0,
            ram: 3,
        },
        CpuLevel::I486 => TierCost {
            l1: 2,
            l2: 191,
            ram: 250,
        },
        CpuLevel::I586 => TierCost {
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

pub(crate) const fn build_cache_level_config(level: CpuLevel) -> CacheLevelConfig {
    let g = cache_geometry(level);
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

pub(crate) const CACHE_CONFIG_286: CacheLevelConfig = build_cache_level_config(CpuLevel::I286);
pub(crate) const CACHE_CONFIG_386: CacheLevelConfig = build_cache_level_config(CpuLevel::I386);
pub(crate) const CACHE_CONFIG_486: CacheLevelConfig = build_cache_level_config(CpuLevel::I486);
pub(crate) const CACHE_CONFIG_586: CacheLevelConfig = build_cache_level_config(CpuLevel::I586);

#[inline(always)]
pub(crate) const fn cache_level_config(level: CpuLevel) -> CacheLevelConfig {
    match level {
        CpuLevel::I286 => CACHE_CONFIG_286,
        CpuLevel::I386 => CACHE_CONFIG_386,
        CpuLevel::I486 => CACHE_CONFIG_486,
        CpuLevel::I586 => CACHE_CONFIG_586,
    }
}

pub(crate) const fn code_fetch_ws(level: CpuLevel) -> u8 {
    match level {
        CpuLevel::I286 => 0,
        CpuLevel::I386 => 0,
        CpuLevel::I486 => 0,
        CpuLevel::I586 => 0,
    }
}
