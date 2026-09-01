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

    /// Probe physical address `phys`, installing the line on a miss via the
    /// documented pseudo-LRU victim choice. Returns `true` on a hit.
    fn probe(&mut self, phys: u32) -> bool {
        let line = phys >> LINE_SHIFT;
        let set = (line as usize) & SET_MASK;
        let base = set * WAYS;
        for way in 0..WAYS {
            if self.tags[base + way] == line {
                self.plru[set] = plru_touch(self.plru[set], way);
                return true;
            }
        }
        let victim = plru_victim(self.plru[set]);
        self.tags[base + victim] = line;
        self.plru[set] = plru_touch(self.plru[set], victim);
        false
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
    /// bool test) when the probe is disabled.
    #[inline]
    pub(crate) fn probe(&mut self, class: ShadowAccessClass, phys: u32) {
        if !self.enabled {
            return;
        }
        let hit = self.tags.probe(phys);
        let counts = &mut self.counts[class_index(class)];
        if hit {
            counts.hits += 1;
        } else {
            counts.misses += 1;
        }
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
mod tests {
    use super::*;

    fn line_addr(line: u32) -> u32 {
        line << LINE_SHIFT
    }

    /// Known-pattern proof of the 4-way pseudo-LRU replacement: fill one set,
    /// evict its pseudo-LRU victim, and confirm the victim -- not just "a"
    /// miss -- is exactly the one Intel's 3-bit tree algorithm predicts.
    ///
    /// Four lines alias into set 0 (line numbers 128 apart, since `SETS` =
    /// 128), then a fifth forces an eviction. Every step below is hand-traced
    /// against `plru_victim`/`plru_touch`, not just re-derived from the code
    /// under test:
    ///
    /// 1. probe(L0)   MISS  bits 0b000 -> victim way0        -> bits 0b011
    /// 2. probe(L128) MISS  bits 0b011 -> victim way2        -> bits 0b110
    /// 3. probe(L256) MISS  bits 0b110 -> victim way1        -> bits 0b101
    /// 4. probe(L384) MISS  bits 0b101 -> victim way3        -> bits 0b000
    /// 5. probe(L0)   HIT   (way0)                           -> bits 0b011
    /// 6. probe(L512) MISS  bits 0b011 -> victim way2 (L128 evicted) -> bits 0b110
    /// 7. probe(L256) HIT   (way1, untouched by the eviction) -> bits 0b101
    /// 8. probe(L128) MISS  (evicted at step 6, so this proves the eviction
    ///                       target -- not merely that some slot missed)
    #[test]
    fn four_way_pseudo_lru_matches_hand_traced_sequence() {
        let mut tags = ShadowTags::new();
        let sequence = [
            (0u32, false),
            (128, false),
            (256, false),
            (384, false),
            (0, true),
            (512, false),
            (256, true),
            (128, false),
        ];
        for (step, (line, expect_hit)) in sequence.into_iter().enumerate() {
            let hit = tags.probe(line_addr(line));
            assert_eq!(
                hit, expect_hit,
                "step {step}: probing line {line} expected hit={expect_hit}, got {hit}"
            );
        }
    }

    #[test]
    fn disabled_probe_counts_nothing() {
        let mut probe = ShadowL1Probe {
            tags: ShadowTags::new(),
            counts: [ShadowClassCounts::default(); CLASS_COUNT],
            enabled: false,
        };
        probe.probe(ShadowAccessClass::DataRead, 0);
        probe.probe(ShadowAccessClass::DataRead, 0x1_0000);
        let diag = probe.diagnostics();
        assert_eq!(diag.data_read, ShadowClassCounts::default());
    }

    #[test]
    fn enabled_probe_splits_by_class() {
        let mut probe = ShadowL1Probe {
            tags: ShadowTags::new(),
            counts: [ShadowClassCounts::default(); CLASS_COUNT],
            enabled: true,
        };
        probe.probe(ShadowAccessClass::CodeFetch, 0);
        probe.probe(ShadowAccessClass::CodeFetch, 0); // same line: hit
        probe.probe(ShadowAccessClass::DataRead, 0x2000);
        probe.probe(ShadowAccessClass::DataWrite, 0x4000);
        let diag = probe.diagnostics();
        assert_eq!(diag.code_fetch, ShadowClassCounts { hits: 1, misses: 1 });
        assert_eq!(diag.data_read, ShadowClassCounts { hits: 0, misses: 1 });
        assert_eq!(diag.data_write, ShadowClassCounts { hits: 0, misses: 1 });
    }

    #[test]
    fn shadow_class_for_maps_page_walks_into_data_classes() {
        assert_eq!(
            shadow_class_for(BusAccessKind::PageWalkRead),
            Some(ShadowAccessClass::DataRead)
        );
        assert_eq!(
            shadow_class_for(BusAccessKind::PageWalkWrite),
            Some(ShadowAccessClass::DataWrite)
        );
        assert_eq!(shadow_class_for(BusAccessKind::IoRead), None);
        assert_eq!(shadow_class_for(BusAccessKind::IoWrite), None);
        assert_eq!(shadow_class_for(BusAccessKind::InterruptAcknowledge), None);
    }
}
