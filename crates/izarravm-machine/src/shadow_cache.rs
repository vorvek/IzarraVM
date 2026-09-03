// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! S1 of `dev_docs/2026-09-05-s2-cache-differentiation-design.md` (predecessor:
//! `dev_docs/2026-09-01-bus-clock-diag.md`): a SHADOW cache-tag probe for the
//! Approximate timing class (486/586), which charges every cacheable-RAM access
//! a flat cost and never consults `CacheModel`'s tag arrays
//! (`cache_tier_lookups` is 0 on every 486/586 run; see `MachineBus::flat_data_cost`).
//!
//! This module CHARGES NOTHING. It is a second, independent tag array that
//! shadows the same access stream the flat-cost path already charges, sized to
//! the REAL part's cache geometry, selected per persona (`CpuPersona`):
//!
//! * **486**: 8 KiB, 4-way set associative, 16-byte lines, 128 sets, 3-bit
//!   tree pseudo-LRU, no-write-allocate, write-through -- Intel i486
//!   Microprocessor Hardware Reference Manual (1990) §4.2, and the i486DX2
//!   Microprocessor Data Book (Jul 1992) §5/§6.1, both under
//!   `dev_docs/reference/i486/`.
//! * **586 (Pentium MMX 166, `GSW_MODE_SPECS[3]`)**: split 16 KiB instruction +
//!   16 KiB data L1, 32-byte lines, 2-way set associative, 256 sets each,
//!   1-bit LRU -- Pentium Processor Family Developer's Manual Volume 1 (Jul95)
//!   §3.5 pins the base part's 8+8 KiB / 2-way / 32-byte-line structure
//!   (`dev_docs/reference/Pentium-K6/241428-004_..._Volume_1_Jul95.txt:3697-3699`,
//!   "Each of the caches are 8 Kbytes in size and each is organized as a 2-way
//!   set associative ... Each cache line is 32 bytes wide"); the P55C (Pentium
//!   MMX) doubles each half to 16 KiB while keeping the 2-way/32-byte
//!   structure, matching `GSW_MODE_SPECS[3]`'s `L1Cache::Split { instruction_kib:
//!   16, data_kib: 16 }` (`crates/izarravm-core/src/gsw.rs:118-131`) and the S2
//!   design note §0/§2b. The data cache is WRITE-BACK with write-allocate on a
//!   write miss (Vol 1 §3.3.3: 3 writeback buffers; a per-line dirty bit tracks
//!   a modified line, and an eviction of a dirty line counts as a
//!   `write_back_victims` write-back). The instruction cache never sees a
//!   write, so it carries no dirty state.
//!   * **L2** (design §2b item 3): 512 KiB, 32-byte lines, 4-way set
//!     associative, 4096 sets, LRU (reuses the 486 arm's 3-bit tree PLRU,
//!     which is exact for 4 ways), write-back, inclusive of L1 in the sense
//!     that it is probed only on an L1 miss (this shadow does not enforce
//!     strict inclusion -- an L2 fill never explicitly invalidates other L1
//!     lines -- because nothing here charges timing and S1's certifier only
//!     asks that L1 misses rise while L2 misses stay flat on an L2-resident
//!     stride, which holds without enforced inclusion).
//!
//! Every counter here is diagnostic-only, exactly like `OplProbe`: never read
//! by any emulation decision, never part of canonical state, and safe to
//! recompute differently across a save/restore round trip. Coverage is
//! necessarily partial for the interpreter/FastMap path's own reasons -- see
//! `ShadowL1Probe`'s doc -- and additionally covers the Direct JIT's per-block
//! code-fetch stream once `IZARRAVM_SHADOW_L1_PROBE=1` forces the
//! `NativeBlockTrace` append preamble (`bus.rs`'s `native_fetches_are_uniform`);
//! see that doc for what the JIT's bulk per-block DATA charge still leaves
//! unprobed and why.
//!
//! ## No-write-allocate (486) / write-allocate (586)
//!
//! The i486 is write-through with no allocation on a write miss ("Cache
//! allocations are not made on write misses", DX2 Data Book Sec 5.3): a write
//! hit updates the line and touches LRU like any other hit, but a write miss
//! installs nothing and touches no LRU bit, unlike a read/fetch miss, which
//! always installs (a real fill). The 586 data array is the opposite: a write
//! miss allocates the line (write-allocate) and installs it dirty, since the
//! shadow only tracks whether a line is unmodified-since-fill, not its data.
//!
//! ## Invalidation
//!
//! The array is flushed (every tag, LRU byte and dirty bit reset) on INVD,
//! WBINVD, and a persona change -- the same events a real part flushes its own
//! cache on ("the on-chip cache to be flushed", Nov89 Sec 5.7; "all 128 sets of
//! three LRU bits are set to 0"). A persona change also re-selects the
//! geometry (a persona change is a different part). INVD/WBINVD reach it
//! through `CpuBus::note_cache_flush`; a persona change reaches it from
//! `Machine::set_mode`, right next to the equivalent `CacheModel::set_mode`
//! flush. Write-back victim counters are cumulative performance data (like the
//! hit/miss counts), not cache state, and are NOT reset by a flush.
//!
//! Guest DMA does **not** flush or invalidate the shadow, and this is a
//! deliberate omission -- see the module's git history / predecessor doc for
//! the exposure bound; unchanged by S1.

use izarravm_bus::BusAccessKind;
use izarravm_core::CpuPersona;

/// Sentinel tag: `phys` is 32 bits, so the largest real line number is
/// `0xFFFF_FFFF >> LINE_SHIFT` for any of this module's line sizes, strictly
/// less than `u32::MAX`.
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

    /// Misses per access -- `1.0 - hit_ratio()` restated without the
    /// subtraction, for callers that want the miss side directly (S1's `mpi`
    /// is this figure folded across classes and normalized by
    /// `perf.instructions`, computed by the caller from raw counts).
    pub fn miss_ratio(&self) -> f64 {
        let total = self.accesses();
        if total == 0 {
            0.0
        } else {
            self.misses as f64 / total as f64
        }
    }
}

/// A snapshot of one L1 array's three classes (a unified 486 array uses all
/// three; the 586 split arrays each populate only the classes they can see --
/// `code_fetch` on the instruction array, `data_read`/`data_write` on the
/// data array -- leaving the other classes at their default zero).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ShadowLevelDiagnostics {
    pub code_fetch: ShadowClassCounts,
    pub data_read: ShadowClassCounts,
    pub data_write: ShadowClassCounts,
    pub write_back_victims: u64,
}

/// A snapshot of every class's counts, for `--profile-json` and the census.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ShadowL1Diagnostics {
    pub enabled: bool,
    pub persona: Option<CpuPersona>,
    /// The 486's unified array, or the 586's data array folded with its
    /// instruction array's `code_fetch` class -- kept for backward
    /// compatibility with existing `--profile-json` consumers that read
    /// `code_fetch`/`data_read`/`data_write` directly off the top level.
    pub code_fetch: ShadowClassCounts,
    pub data_read: ShadowClassCounts,
    pub data_write: ShadowClassCounts,
    /// L1 write-back victims (dirty-line evictions), 586 data array only;
    /// always 0 on the write-through 486 arm.
    pub write_back_victims: u64,
    /// The 586 L2 shadow (512 KiB / 32 B / 4-way / 4096 sets), probed only on
    /// an L1 miss. All-zero (and `None` would be equally honest, but a
    /// zeroed struct keeps the JSON shape stable across personas) on 486.
    pub l2: ShadowLevelDiagnostics,
}

/// 3 bits of tree pseudo-LRU state per set, packed as `b0 | b1<<1 | b2<<2`:
/// `b0` picks the half (0 => victim in {way0,way1}, 1 => victim in {way2,way3});
/// `b1` picks way0-vs-way1 when `b0=0`; `b2` picks way2-vs-way3 when `b0=1`.
/// This is the tree-PLRU Intel documents for a 4-way set: a real LRU stack
/// would need `log2(4!) = 5` bits per set, but the part uses this 3-bit
/// approximation (i486 HW Reference Manual §4.2.1, "pseudo-LRU replacement
/// algorithm"; reused verbatim for the 586 L2 shadow, also 4-way).
const fn plru4_victim(bits: u8) -> usize {
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
const fn plru4_touch(bits: u8, way: usize) -> u8 {
    match way {
        0 => (bits & 0b100) | 0b011,
        1 => (bits & 0b100) | 0b001,
        2 => (bits & 0b010) | 0b100,
        3 => bits & 0b010,
        _ => bits,
    }
}

/// 1-bit true-LRU state for a 2-way set (the 586 arm's replacement, design
/// §2b item 1: "1-bit LRU"). `bit == way` means "way `way` is the victim" --
/// i.e. the bit names the LEAST recently used way directly, no tree needed
/// for 2 ways (a real LRU stack for 2 ways needs exactly 1 bit).
const fn lru2_victim(bit: u8) -> usize {
    bit as usize
}

/// After touching `way`, the OTHER way becomes the victim.
const fn lru2_touch(way: usize) -> u8 {
    (1 - way) as u8
}

/// One shadow tag array: `sets * ways` line-number tags, one replacement-state
/// byte per set (3-bit PLRU for a 4-way array, 1-bit LRU for a 2-way array),
/// and -- only meaningful when `write_allocate` is set -- one dirty bit per
/// line and a running write-back-victim count.
///
/// Kept as one generically-sized type (rather than four monomorphized structs)
/// because every array here differs only in these five numbers, and S1c's L2
/// probe reuses the exact same struct as an L1 array at different geometry.
#[derive(Debug)]
struct ShadowTags {
    line_shift: u32,
    set_mask: usize,
    ways: usize,
    /// 586 data / L2: write-allocate + write-back (dirty bit, write-back
    /// victims). 486 unified / 586 instruction: write-through, no-write-
    /// allocate -- `dirty` stays allocated but is never set, so
    /// `write_back_victims` reads 0, byte-for-byte the pre-S1a behavior.
    write_allocate: bool,
    tags: Box<[u32]>,   // [set * ways + way]
    lru: Box<[u8]>,     // [set]; low 3 bits used for a 4-way array, low bit for 2-way
    dirty: Box<[bool]>, // [set * ways + way]
    write_back_victims: u64,
}

impl ShadowTags {
    /// The real 486's unified array: 8 KiB, 16-byte lines, 128 sets, 4-way,
    /// write-through / no-write-allocate. Also `ShadowTags::new()`'s geometry,
    /// preserved for the pre-S1a unit tests below.
    fn new_i486_unified() -> Self {
        Self::with_geometry(4, 128, 4, false)
    }

    /// The 586's split-array geometry: 32-byte lines, 256 sets, 2-way.
    /// `write_allocate = true` selects the write-back data array; the
    /// instruction array (never written) passes `write_allocate = false` so
    /// its `dirty`/`write_back_victims` are trivially inert.
    fn new_i586_split(write_allocate: bool) -> Self {
        Self::with_geometry(5, 256, 2, write_allocate)
    }

    /// The 586's L2 shadow: 512 KiB, 32-byte lines, 4096 sets, 4-way,
    /// write-back (design §2b item 3).
    fn new_i586_l2() -> Self {
        Self::with_geometry(5, 4096, 4, true)
    }

    fn with_geometry(line_shift: u32, sets: usize, ways: usize, write_allocate: bool) -> Self {
        debug_assert!(
            ways == 2 || ways == 4,
            "only 2-way and 4-way arrays are modeled"
        );
        debug_assert!(sets.is_power_of_two());
        Self {
            line_shift,
            set_mask: sets - 1,
            ways,
            write_allocate,
            tags: vec![EMPTY_TAG; sets * ways].into_boxed_slice(),
            lru: vec![0u8; sets].into_boxed_slice(),
            dirty: vec![false; sets * ways].into_boxed_slice(),
            write_back_victims: 0,
        }
    }

    #[inline]
    fn victim(&self, set: usize) -> usize {
        if self.ways == 2 {
            lru2_victim(self.lru[set])
        } else {
            plru4_victim(self.lru[set])
        }
    }

    #[inline]
    fn touch(&self, set: usize, way: usize) -> u8 {
        if self.ways == 2 {
            lru2_touch(way)
        } else {
            plru4_touch(self.lru[set], way)
        }
    }

    /// Probe physical address `phys`. Returns `true` on a hit.
    ///
    /// No-write-allocate arm (`write_allocate == false`, DX2 Data Book Sec
    /// 5.3): a HIT (read or write) touches LRU like any real access. A READ
    /// MISS installs the line (a real fill) and touches LRU for the new
    /// line. A WRITE MISS installs NOTHING and touches no LRU bit.
    ///
    /// Write-allocate / write-back arm (`write_allocate == true`, Pentium Vol
    /// 1 §3.3.3): a write HIT sets the line's dirty bit (in addition to
    /// touching LRU like any hit); ANY miss (read or write) installs the
    /// line, touches LRU, and marks it dirty iff the access that caused the
    /// fill was itself a write (a read miss brings in clean data; a write
    /// miss brings in the line and immediately applies the write). If the
    /// evicted line was dirty, `write_back_victims` counts one write-back.
    fn probe(&mut self, phys: u32, is_write: bool) -> bool {
        let line = phys >> self.line_shift;
        let set = (line as usize) & self.set_mask;
        let base = set * self.ways;
        for way in 0..self.ways {
            if self.tags[base + way] == line {
                self.lru[set] = self.touch(set, way);
                if is_write && self.write_allocate {
                    self.dirty[base + way] = true;
                }
                return true;
            }
        }
        if is_write && !self.write_allocate {
            return false;
        }
        let victim = self.victim(set);
        if self.write_allocate && self.dirty[base + victim] {
            self.write_back_victims += 1;
        }
        self.tags[base + victim] = line;
        self.lru[set] = self.touch(set, victim);
        self.dirty[base + victim] = is_write;
        false
    }

    /// Flush every tag, LRU byte and dirty bit; counters (hits/misses live in
    /// the caller, `write_back_victims` lives here) are cumulative
    /// performance data and are NOT reset by a flush.
    fn flush(&mut self) {
        self.tags.fill(EMPTY_TAG);
        self.lru.fill(0);
        self.dirty.fill(false);
    }
}

/// One L1 array (486 unified, or one half of the 586 split arrays) plus its
/// per-class hit/miss counters -- everything `ShadowLevelDiagnostics` reports.
#[derive(Debug)]
struct ShadowLevel {
    tags: ShadowTags,
    counts: [ShadowClassCounts; CLASS_COUNT],
}

impl ShadowLevel {
    fn new(tags: ShadowTags) -> Self {
        Self {
            tags,
            counts: [ShadowClassCounts::default(); CLASS_COUNT],
        }
    }

    #[inline]
    fn probe(&mut self, class: ShadowAccessClass, phys: u32) -> bool {
        let is_write = class == ShadowAccessClass::DataWrite;
        let hit = self.tags.probe(phys, is_write);
        let counts = &mut self.counts[class_index(class)];
        if hit {
            counts.hits += 1;
        } else {
            counts.misses += 1;
        }
        hit
    }

    fn flush(&mut self) {
        self.tags.flush();
    }

    fn diagnostics(&self) -> ShadowLevelDiagnostics {
        ShadowLevelDiagnostics {
            code_fetch: self.counts[class_index(ShadowAccessClass::CodeFetch)],
            data_read: self.counts[class_index(ShadowAccessClass::DataRead)],
            data_write: self.counts[class_index(ShadowAccessClass::DataWrite)],
            write_back_victims: self.tags.write_back_victims,
        }
    }
}

/// The per-persona array set. The 486 arm is exactly S1's pre-existing single
/// unified array; the 586 arm is S1a's split I/D arrays plus S1c's L2 shadow,
/// probed on an L1 miss.
#[derive(Debug)]
enum ShadowArrays {
    I486Unified(ShadowLevel),
    I586Split {
        instr: ShadowLevel,
        data: ShadowLevel,
        l2: ShadowLevel,
    },
}

impl ShadowArrays {
    fn for_persona(persona: CpuPersona) -> Self {
        match persona {
            // I386 never reaches the Approximate-timing / flat_data_cost arm
            // this probe shadows (`GSW_MODE_SPECS[0..1]` use the Accurate
            // `CacheModel` tag arrays already), but a shadow must still exist
            // for `set_mode` to construct into -- give it the 486's unified
            // geometry rather than special-casing a fourth arm nothing ever
            // arms.
            CpuPersona::I386 | CpuPersona::I486 => {
                ShadowArrays::I486Unified(ShadowLevel::new(ShadowTags::new_i486_unified()))
            }
            CpuPersona::I586 => ShadowArrays::I586Split {
                instr: ShadowLevel::new(ShadowTags::new_i586_split(false)),
                data: ShadowLevel::new(ShadowTags::new_i586_split(true)),
                l2: ShadowLevel::new(ShadowTags::new_i586_l2()),
            },
        }
    }

    /// Probe `class` at `phys`. On the split 586 arrays, an L1 miss also
    /// probes the shared L2 shadow (design §2b item 3: "probed only on an L1
    /// miss").
    #[inline]
    fn probe(&mut self, class: ShadowAccessClass, phys: u32) {
        match self {
            ShadowArrays::I486Unified(level) => {
                level.probe(class, phys);
            }
            ShadowArrays::I586Split { instr, data, l2 } => {
                let level = match class {
                    ShadowAccessClass::CodeFetch => &mut *instr,
                    ShadowAccessClass::DataRead | ShadowAccessClass::DataWrite => &mut *data,
                };
                if !level.probe(class, phys) {
                    l2.probe(class, phys);
                }
            }
        }
    }

    fn flush(&mut self) {
        match self {
            ShadowArrays::I486Unified(level) => level.flush(),
            ShadowArrays::I586Split { instr, data, l2 } => {
                instr.flush();
                data.flush();
                l2.flush();
            }
        }
    }

    fn diagnostics(
        &self,
    ) -> (
        ShadowClassCounts,
        ShadowClassCounts,
        ShadowClassCounts,
        u64,
        ShadowLevelDiagnostics,
    ) {
        match self {
            ShadowArrays::I486Unified(level) => {
                let d = level.diagnostics();
                (
                    d.code_fetch,
                    d.data_read,
                    d.data_write,
                    d.write_back_victims,
                    ShadowLevelDiagnostics::default(),
                )
            }
            ShadowArrays::I586Split { instr, data, l2 } => {
                let i = instr.diagnostics();
                let dd = data.diagnostics();
                (
                    i.code_fetch,
                    dd.data_read,
                    dd.data_write,
                    dd.write_back_victims,
                    l2.diagnostics(),
                )
            }
        }
    }
}

/// The bus-owned shadow probe: a per-persona `ShadowArrays` plus the
/// env-resolved enable bit. Constructed once per `Machine` (`from_env`,
/// mirroring `OplProbe`) and reused across every batch; it is NOT part of
/// canonical state (see the module doc). `Machine::set_mode` re-selects the
/// geometry and flushes on every persona change (`set_persona_and_flush`).
///
/// ## Coverage
///
/// This probes every commit site that charges the flat Approximate-class RAM
/// cost with a KNOWN PHYSICAL ADDRESS: the interpreter's data read/write path
/// (`MachineBus::charge_ram_only`, `data_access_wait_states`,
/// `record_direct_ram_accesses`, the misaligned split in
/// `charge_direct_ram_split`), the interpreter's per-instruction code-fetch
/// path, and -- once armed -- the Direct JIT's compiled-block code-fetch
/// stream: `bus.rs`'s `native_fetches_are_uniform()` returns `false` while
/// this probe is enabled even under `flat_data_cost`, which forces every
/// resident block to carry the `NativeBlockTrace` append preamble (the same
/// mechanism the Accurate timing class already uses for exact fetch
/// accounting) instead of the aggregate bulk-charge shape. That preamble
/// hands `charge_native_cached_fetches` the block's `physical_start` and
/// per-instruction `fetch_lens`, from which every fetch's own physical
/// address is reconstructed and probed -- full, exact JIT code-fetch
/// coverage, not a sample.
///
/// It does NOT probe the JIT's bulk per-block DATA charge
/// (`charge_bus_clocks_bulk` fed by `CompiledBusDelta`): that path counts
/// accesses by WIDTH only, never by address, because the whole point of the
/// aggregate charge is that compiled code touches memory directly with no
/// host call per access. Giving every compiled data access a per-address
/// callback would mean inserting a call at every memory op the Direct
/// backend emits, which is exactly the hot-path cost the S2 design note §4
/// weighs and defers to a per-block SAMPLED scheme (§3c) for the timing
/// charge; S1 measures, it does not charge, but reusing that same emitter
/// surface safely is out of scope for this slice -- see
/// `dev_docs/2026-09-05-s1-shadow-miss-results.md` for the measured coverage
/// fraction this leaves (JIT data accesses are NOT probed; the interpreter/
/// FastMap-direct data stream and the full JIT+interpreter fetch stream are).
#[derive(Debug)]
pub struct ShadowL1Probe {
    persona: CpuPersona,
    arrays: ShadowArrays,
    enabled: bool,
}

/// `IZARRAVM_SHADOW_L1_PROBE`: unset or any value other than exactly `"1"` is
/// OFF (the repo's off-spelling convention -- `""` is not a distinct signal
/// from unset, see the env-null-empty trap). `"1"` turns the probe on.
fn shadow_l1_probe_enabled() -> bool {
    std::env::var("IZARRAVM_SHADOW_L1_PROBE").as_deref() == Ok("1")
}

impl ShadowL1Probe {
    pub(crate) fn from_env(persona: CpuPersona) -> Self {
        Self {
            persona,
            arrays: ShadowArrays::for_persona(persona),
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
        self.arrays.probe(class, phys);
    }

    /// Whether the probe is armed. `bus.rs` reads this to decide whether a
    /// 586/486 (`flat_data_cost`) bus must still force the JIT's
    /// `NativeBlockTrace` append preamble for fetch coverage, exactly the way
    /// an Accurate-class bus already does unconditionally.
    #[inline]
    pub(crate) fn wants_native_fetch_trace(&self) -> bool {
        self.enabled
    }

    /// Flush the tag arrays (INVD/WBINVD/reset). Counters are cumulative
    /// performance data, not cache state, and are NOT reset by a flush.
    pub(crate) fn flush(&mut self) {
        self.arrays.flush();
    }

    /// Re-select the geometry for `persona` (a persona change is a different
    /// part) and flush, exactly as `flush` does for a same-persona flush.
    /// Called from `Machine::set_mode`.
    pub(crate) fn set_persona_and_flush(&mut self, persona: CpuPersona) {
        if persona != self.persona {
            self.persona = persona;
            self.arrays = ShadowArrays::for_persona(persona);
        } else {
            self.arrays.flush();
        }
    }

    pub(crate) fn diagnostics(&self) -> ShadowL1Diagnostics {
        let (code_fetch, data_read, data_write, write_back_victims, l2) = self.arrays.diagnostics();
        ShadowL1Diagnostics {
            enabled: self.enabled,
            persona: self.enabled.then_some(self.persona),
            code_fetch,
            data_read,
            data_write,
            write_back_victims,
            l2,
        }
    }
}

#[cfg(test)]
#[path = "shadow_cache_test.rs"]
mod tests;
