// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! S1 of `dev_docs/2026-09-01-bus-clock-diag.md`: a SHADOW L1 tag probe for the
//! Approximate timing class (486/586), which charges every cacheable-RAM access
//! a flat cost and never consults `CacheModel`'s tag arrays
//! (`cache_tier_lookups` is 0 on every 486/586 run; see `MachineBus::flat_data_cost`).
//!
//! This module CHARGES NOTHING. It is a second, independent tag array that
//! shadows the same access stream the flat-cost path already charges, sized to
//! the REAL 486's cache geometry rather than `CacheModel`'s direct-mapped
//! 64-byte-line arrays (which price the guest and are untouched by this file):
//! 8 KiB, 4-way set associative, 16-byte lines, 128 sets, pseudo-LRU
//! replacement -- Intel i486 Microprocessor Hardware Reference Manual (1990)
//! §4.2, and the i486DX2 Microprocessor Data Book (Jul 1992) §5/§6.1, both under
//! `dev_docs/reference/i486/`.
//!
//! Every counter here is diagnostic-only, exactly like `OplProbe`: never read
//! by any emulation decision, never part of canonical state, and safe to
//! recompute differently across a save/restore round trip. Coverage is
//! necessarily partial -- see the module doc on `probe` for what it does and
//! does not see.
//!
//! ## Persona
//!
//! The geometry is the 486's specifically. `flat_data_cost` is also true for
//! the 586 persona, which has a real 8K+8K split code/data cache with 32-byte
//! lines and 2-way associativity, not this module's unified 8K/16-byte/4-way
//! array. Enabling the probe on a 586 run reports 486 numbers against 586
//! traffic; S1 only asked for the 486 figure, so this is accepted for now, not
//! silently fixed.
//!
//! ## No-write-allocate
//!
//! The i486 is write-through with no allocation on a write miss ("Cache
//! allocations are not made on write misses", DX2 Data Book Sec 5.3): a write
//! hit updates the line and touches LRU like any other hit, but a write miss
//! installs nothing and touches no LRU bit, unlike a read/fetch miss, which
//! always installs (a real fill). `ShadowTags::probe` takes `is_write` and
//! implements exactly this asymmetry.
//!
//! ## Invalidation
//!
//! The array is flushed (every tag and every LRU byte reset) on INVD, WBINVD,
//! and a persona change -- the same events a real i486 flushes its own cache
//! on ("the on-chip cache to be flushed", Nov89 Sec 5.7; "all 128 sets of
//! three LRU bits are set to 0"). INVD/WBINVD reach it through
//! `CpuBus::note_cache_flush`, called from `izarravm_cpu`'s two-byte-opcode
//! handler; a persona change reaches it from `Machine::set_mode`, right next
//! to the equivalent `CacheModel::set_mode` flush. There is no separate
//! in-place "machine reset" to hook: a reset in this codebase tears down and
//! reconstructs the whole `Machine` (see the GUI session's generation
//! teardown), and a fresh `Machine` already gets a cold array from
//! `ShadowL1Probe::from_env`.
//!
//! Guest DMA does **not** flush or invalidate the shadow, and this is a
//! deliberate omission, not a hole: the i486 does not autonomously snoop bus
//! traffic. Coherency requires external logic to drive an address onto the
//! part with EADS# (normally gated by AHOLD), and whether that happens is a
//! system/chipset design choice the Hardware Reference Manual explicitly
//! leaves to the board (Nov89 Sec 7.2.8). Modelling "the shadow does not see
//! DMA" is therefore modelling a real PC whose chipset does not drive EADS#,
//! which is the common case. The measured exposure is bounded and small: on
//! doom-486 the SB16 DMA ring is a few KB refilled a few million bytes over a
//! run, at most a few hundred distinct 16-byte lines out of ~90M probed
//! accesses in that run, well under 0.01%.

use izarravm_bus::BusAccessKind;

/// 16-byte lines: `phys >> LINE_SHIFT` is the line number.
const LINE_SHIFT: u32 = 4;
/// 128 sets, so the set index is the low 7 bits of the line number.
const SETS: usize = 128;
const SET_MASK: usize = SETS - 1;
/// 4-way set associative: `SETS * WAYS * 16 bytes = 8 KiB`.
const WAYS: usize = 4;

/// Sentinel tag: `phys` is 32 bits, so the largest real line number is
/// `0xFFFF_FFFF >> 4`, strictly less than `u32::MAX`.
const EMPTY_TAG: u32 = u32::MAX;

/// Which real access this probe saw. Matches the diagnosis's three classes;
/// `BusAccessKind::PageWalkRead`/`PageWalkWrite` fold into the data classes
/// (a page-table walk is a data read/write of RAM like any other), and
/// `IoRead`/`IoWrite`/`InterruptAcknowledge` never reach a cacheable-RAM tag
/// decision, so they have no class here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShadowAccessClass {
    CodeFetch,
    DataRead,
    DataWrite,
}

const CLASS_COUNT: usize = 3;

const fn class_index(class: ShadowAccessClass) -> usize {
    match class {
        ShadowAccessClass::CodeFetch => 0,
        ShadowAccessClass::DataRead => 1,
        ShadowAccessClass::DataWrite => 2,
    }
}

/// Fold a bus access kind to the shadow class it belongs to, or `None` for a
/// kind that never resolves a cacheable-RAM tag (port I/O, interrupt ack).
pub(crate) const fn shadow_class_for(kind: BusAccessKind) -> Option<ShadowAccessClass> {
    match kind {
        BusAccessKind::InstructionPrefetch => Some(ShadowAccessClass::CodeFetch),
        BusAccessKind::DataRead | BusAccessKind::PageWalkRead => Some(ShadowAccessClass::DataRead),
        BusAccessKind::DataWrite | BusAccessKind::PageWalkWrite => {
            Some(ShadowAccessClass::DataWrite)
        }
        BusAccessKind::IoRead | BusAccessKind::IoWrite | BusAccessKind::InterruptAcknowledge => {
            None
        }
    }
}

/// Hit/miss tally for one access class.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ShadowClassCounts {
    pub hits: u64,
    pub misses: u64,
}

impl ShadowClassCounts {
    pub fn accesses(&self) -> u64 {
        self.hits + self.misses
    }

    /// Hit ratio in `[0, 1]`, or `0.0` on an empty class (never divides by 0).
    pub fn hit_ratio(&self) -> f64 {
        let total = self.accesses();
        if total == 0 {
            0.0
        } else {
            self.hits as f64 / total as f64
        }
    }
}

/// A snapshot of every class's counts, for `--profile-json` and the census.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ShadowL1Diagnostics {
    pub enabled: bool,
    pub code_fetch: ShadowClassCounts,
    pub data_read: ShadowClassCounts,
    pub data_write: ShadowClassCounts,
}

/// 3 bits of tree pseudo-LRU state per set, packed as `b0 | b1<<1 | b2<<2`:
/// `b0` picks the half (0 => victim in {way0,way1}, 1 => victim in {way2,way3});
/// `b1` picks way0-vs-way1 when `b0=0`; `b2` picks way2-vs-way3 when `b0=1`.
/// This is the tree-PLRU Intel documents for the i486's 4-way set: a real LRU
/// stack would need `log2(4!) = 5` bits per set, but the part uses this 3-bit
/// approximation (HW Reference Manual §4.2.1, "pseudo-LRU replacement
/// algorithm").
const fn plru_victim(bits: u8) -> usize {
    if bits & 0b001 == 0 {
        if bits & 0b010 == 0 { 0 } else { 1 }
    } else if bits & 0b100 == 0 {
        2
    } else {
        3
    }
}

/// Update the 3 PLRU bits so every path away from `way` still reads as
/// "not recently used", leaving the untouched subtree's bit alone.
const fn plru_touch(bits: u8, way: usize) -> u8 {
    match way {
        0 => (bits & 0b100) | 0b011,
        1 => (bits & 0b100) | 0b001,
        2 => (bits & 0b010) | 0b100,
        3 => bits & 0b010,
        _ => bits,
    }
}

/// The shadow tag array itself: 128 sets x 4 ways of line-number tags, plus one
/// PLRU byte per set. Kept in its own type (rather than folded into
/// `ShadowL1Probe`) so the unit tests can drive `probe` directly without the
/// env-gated wrapper.
///
/// Deviation from Intel, numerically nil: on a miss, real hardware checks the
/// four valid bits first and prefers an invalid (never-filled) way over the
/// PLRU victim (DX2 Sec 5.5). This array always asks `plru_victim` and never
/// looks for an `EMPTY_TAG` way. From the cold state (`plru` all zero, every
/// tag `EMPTY_TAG`) the tree's own fill order happens to visit ways
/// 0, 2, 1, 3 -- all four, no repeats -- before it can repeat any way, and a
/// hit only ever re-marks an already-filled way. So no valid line is ever
/// evicted while an empty way still exists; the shortcut is unobservable here,
/// not because the code checks for it, but because the tree shape guarantees
/// it for this replacement algorithm.
#[derive(Debug)]
struct ShadowTags {
    tags: Box<[u32]>, // [set * WAYS + way]
    plru: Box<[u8]>,  // [set]
}

impl ShadowTags {
    fn new() -> Self {
        Self {
            tags: vec![EMPTY_TAG; SETS * WAYS].into_boxed_slice(),
            plru: vec![0u8; SETS].into_boxed_slice(),
        }
    }

    /// Probe physical address `phys`. Returns `true` on a hit.
    ///
    /// No-write-allocate (DX2 Data Book Sec 5.3): a HIT (read or write)
    /// touches PLRU like any real access. A READ MISS installs the line (a
    /// real fill) and touches PLRU for the new line. A WRITE MISS installs
    /// NOTHING and touches no PLRU bit -- the i486 never allocates a line for
    /// a write that misses, so there is nothing on real silicon for this
    /// probe to mark as used.
    fn probe(&mut self, phys: u32, is_write: bool) -> bool {
        let line = phys >> LINE_SHIFT;
        let set = (line as usize) & SET_MASK;
        let base = set * WAYS;
        for way in 0..WAYS {
            if self.tags[base + way] == line {
                self.plru[set] = plru_touch(self.plru[set], way);
                return true;
            }
        }
        if is_write {
            return false;
        }
        let victim = plru_victim(self.plru[set]);
        self.tags[base + victim] = line;
        self.plru[set] = plru_touch(self.plru[set], victim);
        false
    }

    /// Flush every tag and every PLRU byte, as INVD/WBINVD/reset do on real
    /// silicon ("all 128 sets of three LRU bits are set to 0", Nov89 Sec 5.7).
    fn flush(&mut self) {
        self.tags.fill(EMPTY_TAG);
        self.plru.fill(0);
    }
}

/// The bus-owned shadow probe: `ShadowTags` plus per-class counters and the
/// env-resolved enable bit. Constructed once per `Machine` (`from_env`,
/// mirroring `OplProbe`) and reused across every batch; it is NOT part of
/// canonical state (see the module doc).
///
/// ## Coverage
///
/// This probes every commit site that charges the flat Approximate-class RAM
/// cost with a KNOWN PHYSICAL ADDRESS: the interpreter's data read/write path
/// (`MachineBus::charge_ram_only`, `data_access_wait_states`,
/// `record_direct_ram_accesses`, the misaligned split in
/// `charge_direct_ram_split`) and the interpreter's per-instruction code-fetch
/// path (`charge_physical_instruction_fetch_run`'s conventional/extended-RAM
/// fast arm, and `charge_classified_instruction_fetch_run`'s cacheable-RAM arm
/// for the rarer byte-straddling and device-adjacent runs it handles).
///
/// It does NOT probe the JIT's bulk per-block charges
/// (`charge_native_cached_fetches`, `charge_bus_clocks_bulk`,
/// `finish_compiled_window`): those commit one aggregate cost for a whole
/// block times an iteration count and never carry the individual addresses a
/// tag lookup needs (`CompiledBusDelta` counts accesses by WIDTH only, not by
/// address). Giving the JIT per-address addresses would mean a callback per
/// real memory access from compiled code, which is exactly the hot-path cost
/// S2's design note weighs and rejects for this diagnostic slice. So these
/// counters describe the address-classified (interpreter / FastMap-direct)
/// share of the access stream, not the whole one; see
/// `dev_docs/2026-09-01-bus-clock-diag.md` §1 for the bucket split this
/// corresponds to (`data_read_flat`, `data_write_flat`, `fetch_icache`,
/// `fetch_other`, versus `bulk_native_fetch`/`bulk_native_data`).
#[derive(Debug)]
pub struct ShadowL1Probe {
    tags: ShadowTags,
    counts: [ShadowClassCounts; CLASS_COUNT],
    enabled: bool,
}

/// `IZARRAVM_SHADOW_L1_PROBE`: unset or any value other than exactly `"1"` is
/// OFF (the repo's off-spelling convention -- `""` is not a distinct signal
/// from unset, see the env-null-empty trap). `"1"` turns the probe on.
fn shadow_l1_probe_enabled() -> bool {
    std::env::var("IZARRAVM_SHADOW_L1_PROBE").as_deref() == Ok("1")
}

impl ShadowL1Probe {
    pub(crate) fn from_env() -> Self {
        Self {
            tags: ShadowTags::new(),
            counts: [ShadowClassCounts::default(); CLASS_COUNT],
            enabled: shadow_l1_probe_enabled(),
        }
    }

    /// Probe one access of `class` at physical address `phys`. A no-op (one
    /// bool test) when the probe is disabled. `class == DataWrite` takes the
    /// no-write-allocate path in `ShadowTags::probe`.
    #[inline]
    pub(crate) fn probe(&mut self, class: ShadowAccessClass, phys: u32) {
        if !self.enabled {
            return;
        }
        let is_write = class == ShadowAccessClass::DataWrite;
        let hit = self.tags.probe(phys, is_write);
        let counts = &mut self.counts[class_index(class)];
        if hit {
            counts.hits += 1;
        } else {
            counts.misses += 1;
        }
    }

    /// Flush the tag array (INVD/WBINVD/reset/persona change). Counters are
    /// cumulative performance data, not cache state, and are NOT reset by a
    /// flush -- a real flush invalidates lines, it does not erase a
    /// performance counter.
    pub(crate) fn flush(&mut self) {
        self.tags.flush();
    }

    pub(crate) fn diagnostics(&self) -> ShadowL1Diagnostics {
        ShadowL1Diagnostics {
            enabled: self.enabled,
            code_fetch: self.counts[class_index(ShadowAccessClass::CodeFetch)],
            data_read: self.counts[class_index(ShadowAccessClass::DataRead)],
            data_write: self.counts[class_index(ShadowAccessClass::DataWrite)],
        }
    }
}

#[cfg(test)]
#[path = "shadow_cache_test.rs"]
mod tests;
