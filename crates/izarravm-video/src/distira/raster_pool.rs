// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! The raster thread pool, the shared frame store, and the thread-safe
//! diagnostic counters.
//!
//! Distira splits a large triangle across 2 or 4 lanes by row, the way
//! 86Box splits its Voodoo render work across render threads. Lane `i`
//! rasterises the rows where `(y - min_y) % lanes == i`, so the lanes
//! never touch the same pixel. The lanes run on a dedicated rayon pool;
//! the calling thread blocks until the join.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};

/// How many raster threads to use on a host with `cores` logical CPUs.
/// Two by default; four only when six or more cores are available, so
/// the rasteriser leaves room for the main emulation thread and the
/// GUI/audio threads.
pub(super) fn lanes_for_cores(cores: usize) -> usize {
    if cores >= 6 { 4 } else { 2 }
}

/// The upper bound both the pool size and `Distira::set_raster_lanes`
/// clamp to. Not a hardware limit -- `render_band`'s `y % lanes`
/// partition works for any lane count -- just a sanity ceiling on the
/// number of OS threads a single Distira instance will ask the process
/// for.
pub(super) const MAX_LANES: usize = 32;

/// `IZARRAVM_DISTIRA_LANES`, read and clamped ONCE per process and cached:
/// the A/B knob for the raised-lane-cap lever
/// (`dev_docs/2026-09-05-tombraid-glide-foyer-profile.md` section 5, lever
/// B). Unset (or unparsable, or zero) keeps today's `lanes_for_cores`
/// policy. This is the single read site -- both the pool's thread count
/// and `Distira::new`'s default `raster_lanes` go through it, so the pool
/// always has at least as many OS threads as any instance's default asks
/// for.
fn lane_override() -> Option<usize> {
    static OVERRIDE: OnceLock<Option<usize>> = OnceLock::new();
    *OVERRIDE.get_or_init(|| {
        std::env::var("IZARRAVM_DISTIRA_LANES")
            .ok()
            .and_then(|value| value.trim().parse::<usize>().ok())
            .filter(|&lanes| lanes >= 1)
            .map(|lanes| lanes.min(MAX_LANES))
    })
}

pub(super) fn host_lanes() -> usize {
    lane_override().unwrap_or_else(|| {
        lanes_for_cores(
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1),
        )
    })
}

/// The dedicated raster pool, sized by [`host_lanes`] and started on
/// first use. One pool serves every Distira instance in the process.
pub(super) fn raster_pool() -> &'static rayon::ThreadPool {
    static POOL: OnceLock<rayon::ThreadPool> = OnceLock::new();
    POOL.get_or_init(|| {
        rayon::ThreadPoolBuilder::new()
            .num_threads(host_lanes())
            .thread_name(|index| format!("distira-raster-{index}"))
            .build()
            .expect("build the Distira raster pool")
    })
}

/// The frame store, held as relaxed byte atomics so raster lanes can
/// share it through `&self`. Relaxed `AtomicU8` loads and stores compile
/// to plain byte moves on x86, so the serial paths pay nothing.
///
/// Row partitioning is what keeps parallel writes deterministic: each
/// lane owns a disjoint set of rows, and a pixel's colour and depth
/// bytes are a function of its row. A guest CAN program overlapping
/// colour and aux bases; two lanes then write the same byte and the
/// byte value depends on timing (as on the real board), but the access
/// stays defined behavior because every access is atomic.
pub struct FrameStore {
    bytes: Vec<AtomicU8>,
}

impl FrameStore {
    pub(super) fn new(len: usize) -> Self {
        let mut bytes = Vec::new();
        bytes.resize_with(len, AtomicU8::default);
        Self { bytes }
    }

    pub(super) fn len(&self) -> usize {
        self.bytes.len()
    }

    pub(super) fn get(&self, offset: usize) -> Option<u8> {
        Some(self.bytes.get(offset)?.load(Ordering::Relaxed))
    }

    pub(super) fn set(&self, offset: usize, value: u8) -> bool {
        let Some(slot) = self.bytes.get(offset) else {
            return false;
        };
        slot.store(value, Ordering::Relaxed);
        true
    }

    pub(super) fn read_u16_le(&self, offset: usize) -> Option<u16> {
        let low = self.get(offset)?;
        let high = self.get(offset + 1)?;
        Some(u16::from_le_bytes([low, high]))
    }

    /// A masked offset of exactly `len - 1` still drops the whole write
    /// instead of wrapping its high byte to offset 0 -- a residual
    /// one-byte hole in the FBI write path's otherwise unconditional
    /// "wrap, never drop" contract (`dev_docs/2026-09-01-fb4mb-review.md`
    /// section 4). Every caller in this codebase computes an even offset
    /// (`color_start`/`aux_base` are multiples of 8192, `pitch` a multiple
    /// of 128, and the column term is always `x * 2`), and `len` (aliased
    /// to `DISTIRA_FB_SIZE`) is a power of two and therefore even, so
    /// `len - 1` is odd and this arm is unreachable today. Not fixed here:
    /// 86Box has the mirror-image bug, a one-byte overread at
    /// `addr == fb_mask`, so there is no reference to copy either way.
    pub(super) fn write_u16_le(&self, offset: usize, value: u16) -> bool {
        if offset + 1 >= self.bytes.len() {
            return false;
        }
        let [low, high] = value.to_le_bytes();
        self.bytes[offset].store(low, Ordering::Relaxed);
        self.bytes[offset + 1].store(high, Ordering::Relaxed);
        true
    }

    /// Fill `start..end` (clamped to the store) with one byte.
    pub(super) fn fill(&self, start: usize, end: usize, value: u8) {
        let end = end.min(self.bytes.len());
        for slot in self.bytes.get(start..end).unwrap_or(&[]) {
            slot.store(value, Ordering::Relaxed);
        }
    }

    /// How many bytes in `start..end` (clamped) are not zero.
    pub(super) fn count_nonzero(&self, start: usize, end: usize) -> usize {
        let end = end.min(self.bytes.len());
        self.bytes
            .get(start..end)
            .unwrap_or(&[])
            .iter()
            .filter(|slot| slot.load(Ordering::Relaxed) != 0)
            .count()
    }
}

impl Clone for FrameStore {
    fn clone(&self) -> Self {
        Self {
            bytes: self
                .bytes
                .iter()
                .map(|slot| AtomicU8::new(slot.load(Ordering::Relaxed)))
                .collect(),
        }
    }
}

impl PartialEq for FrameStore {
    fn eq(&self, other: &Self) -> bool {
        self.bytes.len() == other.bytes.len()
            && self
                .bytes
                .iter()
                .zip(&other.bytes)
                .all(|(a, b)| a.load(Ordering::Relaxed) == b.load(Ordering::Relaxed))
    }
}

impl Eq for FrameStore {}

impl std::fmt::Debug for FrameStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "FrameStore({} bytes)", self.bytes.len())
    }
}

/// A read-side diagnostic counter that can be bumped through `&self`
/// from any thread. Replaces the former `Cell<u64>` counters, which
/// kept `Distira` from being `Sync`.
#[derive(Debug, Default)]
pub struct DiagCounter(AtomicU64);

impl DiagCounter {
    pub(super) fn get(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }

    pub(super) fn increment(&self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

impl Clone for DiagCounter {
    fn clone(&self) -> Self {
        Self(AtomicU64::new(self.get()))
    }
}

impl PartialEq for DiagCounter {
    fn eq(&self, other: &Self) -> bool {
        self.get() == other.get()
    }
}

impl Eq for DiagCounter {}
