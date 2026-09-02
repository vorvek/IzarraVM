// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Paging, TLB, and direct page caches for fast physical memory access.
//!
//! This module holds the microarchitectural caches (TLB, DirectPageCache,
//! CodePageCache, FetchPageCache, PrefetchWindow) and the page translation
//! machinery. These are transparent to CpuGsw equality (always-equal impls)
//! and are used by both the interpreter path and the JIT paged probes.

use izarravm_bus::DirectPage;

use crate::SegmentRegister;

// The types below are pub(crate) so CpuGsw in lib.rs can name the fields.

// --- Constants ---
/// READ THE CONSUMER BEFORE READING THE SIZE. This is not only a translation cache. With paging
/// on, `fast_map_permissions` (memory.rs) refuses to publish a page into the JIT's linear fast map
/// unless `Tlb::lookup` hits, and every insert that displaces a non-same-residency entry unpublishes
/// the victim's fast-map page. So the set of pages the Direct backend can reach natively is bounded
/// by this constant, direct-mapped: `slot` is `page & (TLB_ENTRIES - 1)`, which at 64 entries
/// discriminates only linear address bits 12 through 17, and pages 256 KiB apart collide. A native
/// access to an unpublished page takes a side exit and a full round trip back into Rust.
///
/// A size sweep on Quake/586 (6.2B cycles, one discarded warmup and one observation per size,
/// each self-consistent) measured that coupling directly:
///
///   entries   unavailable-or-kind exits   side exits    direct entries   insns/entry   coverage
///        64                 39,830,978    40,708,403      157,638,200         17.80     80.54%
///       256                  3,671,362     4,548,908      125,604,995         22.71     81.56%
///      1024                    421,795     1,298,782      122,581,197         23.31     81.66%
///      4096                    184,740     1,062,059      122,216,774         23.39     81.67%
///
/// 1024 is the knee: 4096 buys 0.08 instructions per entry and 0.01 coverage points for four times
/// the memory. Removing 39.4M of the 40.7M native side exits is worth more than every opcode
/// lowering merged into this backend so far, which together moved instructions per entry 12.16 to
/// 17.80.
///
/// NOT free, and not purely microarchitectural. Guest ARCHITECTURAL state is untouched for a
/// correctly flushing guest: a hit and a walk resolve the same physical address. Two caveats keep
/// that from being the flat claim it looks like. A hit skips charged `PageWalkRead` bus cycles, so
/// charged clocks, retired instructions inside a fixed cycle budget, and in-guest frame rate all
/// move. And a walk writes the PDE and PTE accessed bit, plus the dirty bit on a write, back into
/// guest RAM, which a hit skips, so those bytes are refreshed on a different schedule at a
/// different size. That only diverges for a guest that clears A or D without an INVLPG or a CR3
/// reload, which is the undefined-behaviour case the `Tlb` caveat below describes.
///
/// Measured: Quake/586 43.2 to 43.5 fps and Doom/586 833 to 828 realtics, both still inside the
/// gate's existing bands, with frames and gametics unchanged. That is the "timing approx" half of
/// the contract, not a correctness change.
///
/// Two costs to know before raising this further. `Tlb` is an inline array inside `CpuGsw`, which is
/// itself inline in `Machine` and threaded by value, so 8192 entries overflows the main thread's
/// stack during construction (4096 was measured working). And the canonical execution payload
/// serializes every live entry, so its maximum grows with this constant.
pub const TLB_ENTRIES: usize = 1024;
// `Tlb::slot` masks with `TLB_ENTRIES - 1`, and jit/block.rs emits that same mask into native code.
// Both are silently wrong for a non-power-of-two, and the sweep table above invites tuning this.
const _: () = assert!(TLB_ENTRIES.is_power_of_two());
pub(crate) const PREFETCH_WINDOW_BYTES: usize = 32;
pub(crate) const TRACKED_WRITE_PAGES: usize = 8;
/// "No page recorded yet this instruction" for `CpuGsw::last_written_page`. A physical page
/// number is `phys >> 12` over a 32-bit physical address, so the largest real value is 0xFFFFF
/// and `u32::MAX` can never collide with one. A sentinel rather than an `Option` so the hot
/// early-out is one compare against a plain word.
pub(crate) const NO_LAST_WRITTEN_PAGE: u32 = u32::MAX;
const _: () = assert!(NO_LAST_WRITTEN_PAGE > u32::MAX >> 12);
pub(crate) const DIRECT_PAGE_CACHE_LINES: usize = 64;
pub(crate) const FETCH_PAGE_CACHE_ENTRIES: usize = 4;

// --- TLB ---

// The JIT's paged memory probe emits the entry stride and field offsets directly.
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct TlbEntry {
    /// Linear page number (linear >> 12). Meaningful only when `generation` is current.
    pub(crate) tag: u32,
    /// Physical page base (pte & 0xffff_f000).
    pub(crate) phys: u32,
    /// Live only while equal to the owning Tlb's `generation`.
    pub(crate) generation: u32,
    /// Cached combined PDE&PTE R/W bit (page is writable).
    pub(crate) writable: bool,
    /// Cached combined PDE&PTE U/S bit (page is user-accessible).
    pub(crate) user: bool,
    /// PTE dirty bit already set, so a write hit needs no page-table update.
    pub(crate) dirty: bool,
}

impl TlbEntry {
    pub(crate) const EMPTY: Self = Self {
        tag: 0,
        phys: 0,
        generation: 0,
        writable: false,
        user: false,
        dirty: false,
    };
}

/// Direct-mapped linear-to-physical translation cache. `generation` bumps to flush
/// in O(1); an entry is live only while its tag and `generation` match. Contents are
/// microarchitectural, so the TLB is transparent to CpuGsw equality and prints
/// terse. Non-snooping, which matches real x86: a guest must INVLPG or reload CR3
/// after editing a page-table entry, and IzarraVM flushes on exactly those events.
///
/// Size caveat on that last sentence. The POLICY matches real x86; the CAPACITY does not. A 386 or
/// 486 has on the order of 32 entries and a Pentium 64, set-associative, while this models 1024
/// direct-mapped for the reason in `TLB_ENTRIES` above. A guest that edits a page-table entry and
/// omits the required INVLPG is already relying on undefined behaviour, but on real hardware it
/// often survives because the small TLB evicts the stale entry within a few hundred pages. Here it
/// may not. If a paged title regresses in a way that looks like stale translations, this is the
/// first place to look; the corpus pass for this constant covered Quake under CWSDPMI and Doom
/// under JEMMEX.
///
/// One half of that hazard is closed at the consumer: `translate_linear_checked` never raises a
/// page fault from a hit, only from the walk, so an entry that is stale in the RESTRICTIVE
/// direction (the guest relaxed a PTE and skipped its flush) costs a walk instead of a spurious
/// #PF. The permissive direction -- the guest tightened a PTE and skipped its flush -- is still
/// live, and is the one this capacity note is about.
///
/// Per-slot generation, mirroring `DecodeCache`'s two-slot ring (design
/// `2026-09-02-cr3-data-side-design.md` T2). `generation` stays the HOT field every `lookup` and
/// `insert` compares -- unchanged from the single-slot form -- and is kept in sync with
/// `generations[live]` by every method below; nothing on the hot path reads `generations` or
/// `live` directly. Capacity stays shared (1024 entries for both directories together); only
/// LIVENESS is partitioned, exactly as design section (a) A4 argues.
#[derive(Clone)]
pub(crate) struct Tlb {
    pub(crate) entries: [TlbEntry; TLB_ENTRIES],
    pub(crate) generation: u32,
    /// The generation each ring slot last held. Restored verbatim on an R1 reselect
    /// (`select_generation`) and re-minted on an R2/R3 allocation (`allocate_generation`).
    generations: [u32; 2],
    /// Which slot `generation` currently mirrors, 0 or 1. Used only by `retire_dormant_slot`
    /// (INVLPG's second obligation) to name the OTHER slot; every other method is told its
    /// slot explicitly by the caller, which already knows it from `DecodeCache::select_context`.
    live: u8,
    /// The sole source of fresh generation values (mirrors `BlockCache::next_link_epoch`, design
    /// review J2/D3): every mint, whether a single-slot flush, a ring allocation, or a wholesale
    /// retire-all, draws from this ONE counter, so a value handed to one slot can never collide
    /// with the other's. Starts at 3 because `generations` is seeded `[1, 2]`; NEVER mints 0,
    /// which is `TlbEntry::EMPTY`'s sentinel (design review D3) -- a slot holding it would make
    /// every empty entry read as live the moment that slot is selected.
    next_generation: u32,
}

impl Default for Tlb {
    fn default() -> Self {
        Self {
            entries: [TlbEntry::EMPTY; TLB_ENTRIES],
            generation: 1,
            generations: [1, 2],
            live: 0,
            next_generation: 3,
        }
    }
}

impl Tlb {
    #[inline]
    pub(crate) fn slot(page: u32) -> usize {
        (page as usize) & (TLB_ENTRIES - 1)
    }

    #[inline]
    pub(crate) fn lookup(&self, page: u32) -> Option<TlbEntry> {
        let e = self.entries[Self::slot(page)];
        (e.generation == self.generation && e.tag == page).then_some(e)
    }

    /// Returns the entry this insert displaced from its array slot, if that slot held one.
    ///
    /// Reports `previous` whenever `previous.generation != 0` (D3: 0 is `TlbEntry::EMPTY`'s own
    /// sentinel and is never a live generation), NOT only when `previous.generation ==
    /// self.generation`. The narrower, generation-matching check was sound before T2 (design
    /// `2026-09-02-cr3-data-side-design.md`): every generation bump was ALSO a wholesale FastMap
    /// wipe (`record_fast_map_wipe_extent`, tied to the same CR3/CR0 flush), so a displaced entry
    /// from an OLDER generation was already covered by that wipe and reporting it again would
    /// have been redundant work, not a correctness gap. T2 breaks that pairing: the translation-
    /// page and SMC-wholesale retires now bump the generation WITHOUT touching the FastMap at
    /// all, so a displaced entry from a generation this same walk just retired (a live entry one
    /// statement ago) would silently stop being reported, and the caller's FastMap-invalidate-on-
    /// eviction (`memory.rs`'s `same_residency` check) would never run for it -- measured on
    /// `active_fast_map_tracks_tlb_collision_and_rewalks_canonically` (`cpu_test.rs`): a second
    /// linear page colliding into the same array slot, walked immediately after a translation-
    /// page retire fired on the SAME walk, stopped invalidating the evicted page's FastMap entry.
    /// Reporting on any non-empty slot costs nothing extra on the common path (the caller's own
    /// `same_residency` comparison already runs whenever `previous` is `Some`) and costs at most
    /// one redundant, harmless invalidate on the pre-T2 already-wiped-FastMap path.
    #[inline]
    pub(crate) fn insert(
        &mut self,
        page: u32,
        phys: u32,
        writable: bool,
        user: bool,
        dirty: bool,
    ) -> Option<TlbEntry> {
        let slot = Self::slot(page);
        let previous = self.entries[slot];
        self.entries[slot] = TlbEntry {
            tag: page,
            phys,
            generation: self.generation,
            writable,
            user,
            dirty,
        };
        (previous.generation != 0).then_some(previous)
    }

    #[inline]
    pub(crate) fn invalidate(&mut self, page: u32) {
        let slot = Self::slot(page);
        let entry = self.entries[slot];
        if entry.generation == self.generation && entry.tag == page {
            self.entries[slot] = TlbEntry::EMPTY;
        }
    }

    /// The sole minter (design review D3/J2): never returns 0, `TlbEntry::EMPTY`'s sentinel. On
    /// wrap the whole table is cleared before the counter restarts at 1, so no entry carrying a
    /// pre-wrap generation can alias a freshly-minted one.
    fn mint_generation(&mut self) -> u32 {
        if self.next_generation == 0 {
            self.entries = [TlbEntry::EMPTY; TLB_ENTRIES];
            self.next_generation = 1;
        }
        let generation = self.next_generation;
        self.next_generation = self.next_generation.wrapping_add(1);
        generation
    }

    /// R1: reselect a ring slot the caller already knows is occupied. Restores that slot's
    /// stored generation with no other work -- every entry inserted while it was last live is
    /// live again for free, because the generation comparison in `lookup`/`insert` IS the
    /// restore. Never walks or re-checks an entry at this point; the whole soundness argument
    /// for that is design section (d).
    pub(crate) fn select_generation(&mut self, slot: u8) {
        debug_assert!(
            usize::from(slot) < self.generations.len(),
            "select_generation given a slot the ring did not report as occupied"
        );
        self.live = slot;
        self.generation = self.generations[usize::from(slot)];
    }

    /// R2 (a fresh directory took a free slot) or the post-retire mint in R3 (a third distinct
    /// value re-occupies slot 0 after `retire_all_slots`). Mints a generation nothing has been
    /// stamped with yet, so nothing already cached can match it: no entry is invalidated by this
    /// call on its own.
    pub(crate) fn allocate_generation(&mut self, slot: u8) {
        let generation = self.mint_generation();
        self.generations[usize::from(slot)] = generation;
        self.live = slot;
        self.generation = generation;
    }

    /// The `flush_tlb_keep_code_caches` PG-stays-1 CR0 path (design review D4): that caller does
    /// NOT retire the decode ring (`DecodeCache::contexts` is untouched, so no slot renumbering
    /// happens), so only the CURRENTLY LIVE slot's generation may be retired -- the dormant
    /// slot's entries are still valid under their own directory and must survive. Mirrors the
    /// pre-T2 single-slot `flush()` exactly, scoped to the live slot's bookkeeping entry too.
    pub(crate) fn flush_live_slot(&mut self) {
        let generation = self.mint_generation();
        self.generations[usize::from(self.live)] = generation;
        self.generation = generation;
    }

    /// Every site that retires `DecodeCache`'s ring (design review D2): `retire_ring` RENUMBERS
    /// slots (`contexts = [None, None]`, so the next `select_context` allocates into slot 0
    /// regardless of who held it before), and slot INDEX is the only thing tying a `Tlb`
    /// generation to a directory. A retire therefore invalidates the correspondence, not merely
    /// the contents, independent of what caused it -- A20, a direct-map change, an SMC wholesale
    /// kill, a translation-page store, or a real ring retire (R3/R5) all renumber the same way.
    /// Takes the `RingRetired` token `retire_ring` returns so an implementer who forgets to call
    /// this where a ring retire happened gets an `unused_must_use` warning, promoted to a build
    /// failure under this crate's `-D warnings` gate, instead of a silent stale-generation bug.
    /// Unconditional at every call site: a translation-page write or an SMC kill can fire on a
    /// ring that was never seeded at all (no `MOV CR3` has run yet), and the retire is still
    /// needed there for CONTENT reasons -- the edited PTE makes an existing cached translation
    /// wrong regardless of ring bookkeeping (`pte_edit_under_a_live_cr3_forces_a_redecode` and
    /// its siblings exercise exactly that shape). See `Tlb::insert`'s doc comment for the OTHER
    /// half of this: an eviction whose victim carries a generation this call already retired must
    /// still be reported to the FastMap-invalidate caller, which is why `insert` no longer gates
    /// on an exact generation match.
    pub(crate) fn retire_all_slots(&mut self, _token: crate::RingRetired) {
        self.generations[0] = self.mint_generation();
        self.generations[1] = self.mint_generation();
        self.live = 0;
        self.generation = self.generations[0];
    }

    /// INVLPG's second obligation under two slots (design section (d)). `invalidate` above
    /// clears only the LIVE generation's entry for this page; a dormant slot's entry for the
    /// same linear page survives untouched. Retire the whole non-live generation rather than
    /// reaching for one entry under it: one store, the conservative direction (more re-walks for
    /// the other context, never a wrong answer), and INVLPG is rare enough on this workload that
    /// the extra re-walks it costs never show up.
    pub(crate) fn retire_dormant_slot(&mut self) {
        let dormant = 1 - usize::from(self.live);
        self.generations[dormant] = self.mint_generation();
    }

    /// Force the allocator to the wrap boundary without four billion inserts, so a wrap test can
    /// run in microseconds. Mirrors `DecodeCache::next_generation`'s equivalent test-only path
    /// (`cache.next_generation = u32::MAX` in `cpu_persona_system_test.rs`).
    #[cfg(test)]
    pub(crate) fn set_next_generation_for_test(&mut self, value: u32) {
        self.next_generation = value;
    }

    /// The stored per-slot generations, for a test that wants to assert VALUES (e.g. that a wrap
    /// never leaves two slots reading the same one) rather than inferring them through `lookup`,
    /// which a wrap's own entries-clear would satisfy regardless of whether the values collided.
    #[cfg(test)]
    pub(crate) fn generations_for_test(&self) -> [u32; 2] {
        self.generations
    }
}

impl PartialEq for Tlb {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}
impl Eq for Tlb {}

impl std::fmt::Debug for Tlb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Tlb {{ {TLB_ENTRIES} entries }}")
    }
}

// --- Direct Page Cache ---

/// First physical byte of the legacy VGA graphics aperture, and one past its last. The single
/// source of truth for the range: `FastMap` classifies a page as `PageKind::Mode13` by it, and
/// `note_direct_data_map_changed` scopes its invalidation to it. `Bus::direct_page` hands out a
/// video pointer for this range and for no other, which is what makes the scoping sound.
pub(crate) const VGA_APERTURE_START: u32 = 0x000a_0000;
pub(crate) const VGA_APERTURE_END: u32 = 0x000b_0000;

#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct DirectPageCacheEntry {
    pub(crate) physical_page: u32,
    pub(crate) ptr: *mut u8,
}

impl Default for DirectPageCacheEntry {
    fn default() -> Self {
        Self {
            physical_page: u32::MAX,
            ptr: std::ptr::null_mut(),
        }
    }
}

pub(crate) struct DirectPageCache {
    pub(crate) entries: [DirectPageCacheEntry; DIRECT_PAGE_CACHE_LINES],
    mapping_epoch: u64,
}

impl Default for DirectPageCache {
    fn default() -> Self {
        Self {
            entries: [DirectPageCacheEntry::default(); DIRECT_PAGE_CACHE_LINES],
            mapping_epoch: 0,
        }
    }
}

impl DirectPageCache {
    #[inline]
    pub(crate) fn slot(page: u32) -> usize {
        ((page >> 12) as usize) & (DIRECT_PAGE_CACHE_LINES - 1)
    }

    #[inline]
    pub(crate) fn get(&self, physical: u32) -> Option<DirectPageCacheEntry> {
        let page = physical & !0x0fff;
        let entry = self.entries[Self::slot(page)];
        (entry.physical_page == page).then_some(entry)
    }

    #[inline]
    pub(crate) fn insert(&mut self, page: DirectPage) {
        if self.mapping_epoch != page.mapping_epoch {
            self.entries.fill(DirectPageCacheEntry::default());
            self.mapping_epoch = page.mapping_epoch;
        }
        self.entries[Self::slot(page.physical_page)] = DirectPageCacheEntry {
            physical_page: page.physical_page,
            ptr: page.ptr,
        };
    }

    #[inline]
    pub(crate) fn mapping_epoch(&self) -> u64 {
        self.mapping_epoch
    }

    /// Move the epoch without a mapping to insert, so a test can stage what a paging change looks
    /// like to the `InterpretOne` resume predicate. The predicate is the only reader that treats
    /// this value as a generation rather than as a cache key, and no admitted opcode can move it,
    /// so there is no fixture that reaches the clause any other way.
    #[cfg(test)]
    pub(crate) fn set_mapping_epoch_for_test(&mut self, epoch: u64) {
        self.mapping_epoch = epoch;
    }

    #[inline]
    pub(crate) fn invalidate(&mut self) {
        self.entries.fill(DirectPageCacheEntry::default());
        self.mapping_epoch = 0;
    }

    /// Drop only the entries backed by `start..end`, keeping the cache's mapping epoch and every
    /// other entry. Used for a device aperture that re-points without disturbing RAM: a full
    /// `invalidate` would also zero the epoch, which is what makes every surviving FastMap entry
    /// stop matching.
    ///
    /// RECORDED, NOT BUILT: this scans all 64 entries, and the VGA aperture is only 16 pages, which
    /// can occupy only slots `0x20..=0x2F` (`slot` keeps bits 12..17 of the page). Walking those 16
    /// slots instead would be a 4x cut on a path that runs twice per direct-write-token move --
    /// 6.8M times in a doom timedemo. It is left unbuilt because it is worth roughly 0.1% of wall,
    /// which is below this rig's layout confound and therefore cannot be validated on a ladder; it
    /// belongs with whatever next change to this path can be. The physical-page equality test must
    /// stay either way, because pages 256 KiB apart share a line.
    #[inline]
    pub(crate) fn invalidate_physical_range(&mut self, start: u32, end: u32) {
        for entry in &mut self.entries {
            if (start..end).contains(&entry.physical_page) {
                *entry = DirectPageCacheEntry::default();
            }
        }
    }
}

impl Clone for DirectPageCache {
    fn clone(&self) -> Self {
        Self::default()
    }
}

impl PartialEq for DirectPageCache {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}
impl Eq for DirectPageCache {}

impl std::fmt::Debug for DirectPageCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "DirectPageCache")
    }
}

#[cfg(test)]
#[path = "paging_test.rs"]
mod page_map_tests;

// --- Code Page Cache (for fetch) ---

#[derive(Clone, Default)]
pub(crate) struct CodePageCache {
    pub(crate) valid: bool,
    pub(crate) cs: SegmentRegister,
    pub(crate) linear_page: u32,
    pub(crate) physical_page: u32,
}

impl PartialEq for CodePageCache {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}
impl Eq for CodePageCache {}

impl std::fmt::Debug for CodePageCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "CodePageCache")
    }
}

// --- Prefetch Window ---

#[derive(Clone)]
pub(crate) struct PrefetchWindow {
    pub(crate) bytes: [u8; PREFETCH_WINDOW_BYTES],
    pub(crate) cs: SegmentRegister,
    pub(crate) linear_base: u32,
    pub(crate) physical_base: u32,
    pub(crate) len: u8,
}

impl Default for PrefetchWindow {
    fn default() -> Self {
        Self {
            bytes: [0; PREFETCH_WINDOW_BYTES],
            cs: SegmentRegister::default(),
            linear_base: 0,
            physical_base: 0,
            len: 0,
        }
    }
}

impl PrefetchWindow {
    pub(crate) fn invalidate(&mut self) {
        self.len = 0;
    }

    pub(crate) fn get(&self, cs: SegmentRegister, linear: u32) -> Option<(u8, u32)> {
        if self.cs != cs {
            return None;
        }
        let offset = linear.checked_sub(self.linear_base)? as usize;
        if offset < usize::from(self.len) {
            Some((self.bytes[offset], self.physical_base + offset as u32))
        } else {
            None
        }
    }

    pub(crate) fn physical_page(&self) -> Option<u32> {
        (self.len != 0).then_some(self.physical_base >> 12)
    }

    /// Whether a wrapping physical write range touches bytes held in this snapshot. Device writes
    /// use this to preserve an unrelated prefetch window while still observing DMA over code.
    pub(crate) fn overlaps_physical_range(&self, physical: u32, width: u32) -> bool {
        self.len != 0
            && width != 0
            && (0..u32::from(self.len)).any(|offset| {
                self.physical_base
                    .wrapping_add(offset)
                    .wrapping_sub(physical)
                    < width
            })
    }
}

impl PartialEq for PrefetchWindow {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}
impl Eq for PrefetchWindow {}

impl std::fmt::Debug for PrefetchWindow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "PrefetchWindow {{ {} bytes }}", self.len)
    }
}

// --- Fetch Page Cache ---

/// Number of entries in the instruction-fetch page cache (direct-mapped by linear page).
/// A single entry thrashed at every EIP page-boundary crossing (60M misses on Doom demo3);
/// 4 entries cover a function that spans up to 3 page boundaries (common at >4 KiB or when
/// code sits near a page edge) without eviction. Power of two for the slot mask.
#[derive(Clone, Copy)]
pub(crate) struct FetchPageCacheEntry {
    pub(crate) valid: bool,
    pub(crate) cs: SegmentRegister,
    pub(crate) linear_page: u32,
    pub(crate) physical_page: u32,
    pub(crate) ptr: *mut u8,
    pub(crate) len: usize,
}

impl Default for FetchPageCacheEntry {
    fn default() -> Self {
        Self {
            valid: false,
            cs: SegmentRegister::default(),
            linear_page: 0,
            physical_page: 0,
            ptr: std::ptr::null_mut(),
            len: 0,
        }
    }
}

#[derive(Default)]
pub(crate) struct FetchPageCache {
    pub(crate) entries: [FetchPageCacheEntry; FETCH_PAGE_CACHE_ENTRIES],
}

impl FetchPageCache {
    #[inline]
    pub(crate) fn slot(linear: u32) -> usize {
        ((linear >> 12) as usize) & (FETCH_PAGE_CACHE_ENTRIES - 1)
    }

    #[inline]
    pub(crate) fn get(&self, cs: SegmentRegister, linear: u32) -> Option<(u8, u32)> {
        let offset = (linear & 0x0fff) as usize;
        let entry = &self.entries[Self::slot(linear)];
        if entry.valid
            && entry.cs == cs
            && entry.linear_page == (linear & !0x0fff)
            && offset < entry.len
        {
            let value = unsafe { *entry.ptr.add(offset) };
            Some((value, entry.physical_page + offset as u32))
        } else {
            None
        }
    }

    #[inline]
    pub(crate) fn put(&mut self, cs: SegmentRegister, linear: u32, page: DirectPage) {
        self.entries[Self::slot(linear)] = FetchPageCacheEntry {
            valid: true,
            cs,
            linear_page: linear & !0x0fff,
            physical_page: page.physical_page,
            ptr: page.ptr,
            len: page.len,
        };
    }

    pub(crate) fn invalidate(&mut self) {
        for e in &mut self.entries {
            e.valid = false;
        }
    }
}

impl PartialEq for FetchPageCache {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}
impl Eq for FetchPageCache {}

impl std::fmt::Debug for FetchPageCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "FetchPageCache")
    }
}

impl Clone for FetchPageCache {
    fn clone(&self) -> Self {
        Self::default()
    }
}
