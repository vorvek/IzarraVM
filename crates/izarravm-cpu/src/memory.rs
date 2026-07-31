// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
use izarravm_bus::DirectPage;

impl CpuGsw {
    #[cfg(all(
        feature = "jit",
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    fn fast_map_permissions(
        &self,
        linear: u32,
        physical: u32,
        write: bool,
    ) -> Option<jit::fast_map::PagePermissions> {
        if !self.is_paging_enabled() {
            return Some(jit::fast_map::PagePermissions::UNPAGED);
        }
        let entry = self.tlb.lookup(linear >> 12)?;
        (entry.phys == physical & !0x0fff && (!write || entry.dirty)).then_some(
            jit::fast_map::PagePermissions {
                writable: entry.writable,
                user: entry.user,
            },
        )
    }

    #[cfg(all(
        feature = "jit",
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    #[inline(always)]
    fn fast_map_population_enabled(&self) -> bool {
        #[cfg(test)]
        let admission_active =
            self.direct_runtime.admission_active || self.jit_direct.fast_map_enabled();
        #[cfg(not(test))]
        let admission_active = self.direct_runtime.admission_active;

        admission_active && self.mode().uses_approximate_timing()
    }

    /// Recompute the cached `fast_map_serve_enabled` mirror of `fast_map_population_enabled()`.
    /// This must be called from EVERY site that can change that predicate's inputs: `set_mode`
    /// (persona), `finish_direct_execution_transition` (`direct_runtime.admission_active`, via
    /// `jit_direct.execution_enabled()`). A missed call
    /// site desyncs the cache from the real condition; `fast_map_data_slot`'s `debug_assert`
    /// checks this cheaply in debug/test builds.
    ///
    /// Named "refresh", not "recompute-and-return", because callers outside this module (core.rs,
    /// run.rs) reach it through `pub(super)` without needing to know the predicate itself, which
    /// stays private -- population and the interpreter's serve gate both anchor on this ONE
    /// computation so they can never disagree about when the FastMap is live.
    #[cfg(all(
        feature = "jit",
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    pub(super) fn refresh_fast_map_serve_gate(&mut self) {
        self.fast_map_serve_enabled.enabled = self.fast_map_population_enabled();
    }

    #[cfg(all(
        feature = "jit",
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    #[inline]
    fn populate_fast_map_active(
        &mut self,
        linear: u32,
        physical: u32,
        page: DirectPage,
        write: bool,
    ) -> bool {
        // Track the free reordering from the lever-1 design doc: both operands here are
        // side-effect free, and `mapped` is by far the more common outcome (this runs on every
        // direct access), so testing it first skips the TLB probe in `fast_map_permissions`
        // entirely on the already-mapped path instead of computing and discarding it.
        let mapped = if write {
            self.jit_fast_map
                .has_write_mapping_at_epoch(linear, physical, page.mapping_epoch)
        } else {
            self.jit_fast_map
                .has_read_mapping_at_epoch(linear, physical, page.mapping_epoch)
        };
        if mapped {
            return true;
        }
        let Some(permissions) = self.fast_map_permissions(linear, physical, write) else {
            return false;
        };
        if write {
            self.jit_fast_map
                .populate_write(linear, physical, page, permissions)
        } else {
            self.jit_fast_map
                .populate_read(linear, physical, page, permissions)
        }
    }

    #[cfg(all(
        feature = "jit",
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    #[inline]
    fn populate_fast_map(
        &mut self,
        linear: u32,
        physical: u32,
        page: DirectPage,
        write: bool,
    ) -> bool {
        self.fast_map_population_enabled()
            && self.populate_fast_map_active(linear, physical, page, write)
    }

    #[cfg(all(
        feature = "jit",
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    #[inline(always)]
    fn populate_fast_map_from_cached(
        &mut self,
        linear: u32,
        physical: u32,
        entry: DirectPageCacheEntry,
        mapping_epoch: u64,
        write: bool,
    ) {
        if !self.fast_map_population_enabled() {
            return;
        }
        self.populate_fast_map_active(
            linear,
            physical,
            DirectPage {
                physical_page: entry.physical_page,
                ptr: entry.ptr,
                len: 0x1000,
                writable: write,
                mapping_epoch,
            },
            write,
        );
    }

    /// Lever 1: the interpreter's FastMap serve path. Applies exactly the hit predicate native
    /// code uses (`FastMap::lookup_access`) against the CURRENT accessor state -- CPL and CR0.WP
    /// can both move since the mapping was published, so they are read fresh on every probe,
    /// exactly as `translate_linear_checked` rechecks a TLB hit's cached bits against the live
    /// accessor. `Some` means address resolution and page lookup are done: physical address, a
    /// host pointer already biased for `linear`, and whether the page is the Mode13h VGA aperture
    /// (so the caller knows whether it may take the flat RAM charge or must defer to the video
    /// charge). `None` means the caller must fall through to the unchanged canonical path -- a
    /// clean-PTE write, an unaligned or page-crossing access, a CPL-3 hit on a supervisor page, a
    /// stale mapping epoch, and a plain unpopulated page (a cold miss, or `fast_map_serve_enabled`
    /// having gone false since the last population -- see below) all reject here by construction
    /// of `lookup_access`, not by any extra check of ours.
    ///
    /// `fast_map_serve_enabled` (the cached mirror of `fast_map_population_enabled()`, kept in
    /// sync by `refresh_fast_map_serve_gate`) is checked FIRST and short-circuits everything else.
    /// This is NOT the same gate an earlier revision of this function used
    /// (`FastMap::has_storage()`, "has any population ever happened"): storage, once allocated,
    /// is never freed, so `has_storage()` stays `true` for the rest of the CPU's life after the
    /// first successful population -- including across a LIVE GSW MODE SWITCH into a persona that
    /// can never repopulate (386-slow/386 Accurate), where it wrongly kept paying this function's
    /// preamble (and, transiently, wrongly kept SERVING from surviving entries) on every access.
    /// `fast_map_serve_enabled` tracks the actual persona/admission condition instead, so it goes
    /// false the instant a mode switch (or an admission toggle) makes population impossible,
    /// closing both the transient and the steady-state cost. See the campaign log for the
    /// measured regression this replaced: a JIT-off control run showed the fixed preamble cost
    /// (mapping-epoch load, CPL derivation, CR0.WP read) at ~2.66 ns per interpreter data access,
    /// a 4.6% wall regression -- almost exactly what the hit path saves elsewhere. The mechanism
    /// was the PREAMBLE running before a guaranteed miss could be rejected, not where the gate
    /// checking `has_storage()` happened to be written (call site vs. here); do not reintroduce
    /// that preamble-before-gate ordering for either gate.
    #[cfg(all(
        feature = "jit",
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    #[inline]
    fn fast_map_data_slot(
        &mut self,
        linear: u32,
        width: BusWidth,
        write: bool,
    ) -> Option<(u32, *mut u8, bool)> {
        if !self.fast_map_serve_enabled.enabled {
            return None;
        }
        debug_assert_eq!(
            self.fast_map_serve_enabled.enabled,
            self.fast_map_population_enabled(),
            "fast_map_serve_enabled cache is stale relative to fast_map_population_enabled(); a \
             state mutator is missing a refresh_fast_map_serve_gate() call"
        );
        let mapping_epoch = if write {
            self.data_write_pages.mapping_epoch()
        } else {
            self.data_read_pages.mapping_epoch()
        };
        let user = self.current_privilege_level() == 3;
        let write_protect = self.control.cr0 & CR0_WP != 0;
        match self.jit_fast_map.lookup_access(
            linear,
            mapping_epoch,
            width,
            write,
            user,
            write_protect,
        ) {
            Some(access) => {
                self.fast_map_probe.hits += 1;
                Some((access.physical(), access.ptr(), access.is_mode13()))
            }
            None => {
                self.fast_map_probe.misses += 1;
                None
            }
        }
    }

    /// Raw load through a FastMap-resolved pointer. Mirrors `read_direct_entry`, but the FastMap
    /// bias already accounts for the exact linear offset, so there is no separate page offset to
    /// add.
    #[cfg(all(
        feature = "jit",
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    #[inline]
    fn read_fast_map_ptr(ptr: *mut u8, width: BusWidth) -> u32 {
        match width {
            BusWidth::Byte => unsafe { u32::from(*ptr) },
            BusWidth::Word => unsafe {
                u32::from(u16::from_le(std::ptr::read_unaligned(ptr.cast::<u16>())))
            },
            BusWidth::Dword => unsafe { u32::from_le(std::ptr::read_unaligned(ptr.cast::<u32>())) },
        }
    }

    /// Raw store through a FastMap-resolved pointer. Mirrors `write_direct_entry`; see
    /// `read_fast_map_ptr`.
    #[cfg(all(
        feature = "jit",
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    #[inline]
    fn write_fast_map_ptr(ptr: *mut u8, width: BusWidth, value: u32) {
        match width {
            BusWidth::Byte => unsafe {
                *ptr = value as u8;
            },
            BusWidth::Word => unsafe {
                std::ptr::write_unaligned(ptr.cast::<u16>(), (value as u16).to_le());
            },
            BusWidth::Dword => unsafe {
                std::ptr::write_unaligned(ptr.cast::<u32>(), value.to_le());
            },
        }
    }

    /// The joined tail for a FastMap read hit: the SAME charge, counter increments as the slow
    /// (DirectPageCache) read path, just fed by the FastMap's pointer instead of re-deriving one.
    /// A Mode13 hit defers to the full `charge_direct_memory` so the VGA aperture keeps its
    /// `note_direct_write` and persona wait states (invariant 1); a Ram hit takes the equivalent
    /// flat charge through `charge_direct_ram_memory`, which skips only the redundant aperture
    /// range compare `charge_direct_memory` would otherwise redo (see `bus.rs::charge_ram_only`).
    #[cfg(all(
        feature = "jit",
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    #[inline]
    fn finish_fast_map_read<B: CpuBus>(
        &mut self,
        bus: &mut B,
        physical: u32,
        ptr: *mut u8,
        mode13: bool,
        width: BusWidth,
        kind: BusAccessKind,
    ) -> ExecResult<u32> {
        if mode13 {
            bus.charge_direct_memory(physical, width, kind)?;
        } else {
            bus.charge_direct_ram_memory(physical, width, kind)?;
        }
        self.record_data_read(kind, true);
        self.perf.direct_data_pointer_reads += 1;
        Ok(Self::read_fast_map_ptr(ptr, width))
    }

    /// The joined tail for a FastMap write hit. Runs, in order: `record_write_page` (invariant 2 --
    /// a FastMap write bias only exists after the PTE dirty bit is committed, or the page is
    /// unpaged, exactly like the two `record_write_page` call sites inside
    /// `translate_linear_checked` that this replaces); the cheap `code_write_watched` probe;
    /// the charge (same split as `finish_fast_map_read`); the old-bytes compare that drives G2
    /// same-value elision; the store itself; then `note_code_write_hit` on change (invariant 3),
    /// matching the compare-then-write-then-invalidate ordering `write_linear_fragment` already
    /// documents for the slow sized path.
    ///
    /// The invalidation gate is width-sensitive, because the two slow paths this replaces are NOT
    /// symmetric: `write_linear_fragment` (sized) pre-gates on `code_write_watched` before calling
    /// `note_code_write_hit` at all, but `write_linear_u8` (byte) calls `note_code_write` on every
    /// `changed` write, with no watched pre-check. Those two are separate DOORS onto the same body
    /// (`note_code_write_inner`) and are no longer interchangeable: `note_code_write_hit` allows
    /// the mutable imm32 lane exemption, `note_code_write` refuses it. Nothing here depends on the
    /// difference — a lane accepts width 4 only, so no byte write is ever accepted through either
    /// door, and this function calls `note_code_write_hit` for both widths. The one visible
    /// consequence is diagnostic and lives on the SLOW byte path: a byte write landing inside a
    /// lane goes through `write_linear_u8`'s value-less door, so it retires the block exactly as
    /// it should but is not counted in `smc_lane_reject_width`, while the same write served here
    /// would be. `note_code_write_hit`'s FIRST action, before any invalidation logic, is an
    /// unconditional unit-sim feed (`core.rs`) -- diagnostic only, but the one place
    /// `IZARRAVM_UNIT_SIM` observes SMC. An earlier version of this function used `watched &&
    /// changed` for every width, which silently dropped that sim feed for a changed byte write
    /// that hits no watched code, exactly the persona (486/586, where the FastMap is armed) the
    /// simulator exists to model. So: byte writes call `note_code_write_hit` on `changed` alone,
    /// matching the slow byte path; sized writes keep the `watched && changed` pre-gate, matching
    /// the slow sized path. `note_code_write_hit` re-derives whether anything is actually watched
    /// on its own regardless of the caller's gate, so calling it for an unwatched byte write is
    /// safe -- the invalidation half is then a harmless no-op and only the sim feed fires.
    #[cfg(all(
        feature = "jit",
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    #[inline]
    #[allow(clippy::too_many_arguments)]
    fn finish_fast_map_write<B: CpuBus>(
        &mut self,
        bus: &mut B,
        physical: u32,
        ptr: *mut u8,
        mode13: bool,
        width: BusWidth,
        value: u32,
        kind: BusAccessKind,
    ) -> ExecResult<()> {
        self.record_write_page(physical);
        let watched = self.code_write_watched(physical, width.bytes());
        if mode13 {
            bus.charge_direct_memory(physical, width, kind)?;
        } else {
            bus.charge_direct_ram_memory(physical, width, kind)?;
        }
        let changed = Self::read_fast_map_ptr(ptr, width) != value;
        Self::write_fast_map_ptr(ptr, width, value);
        // `note_code_write_hit` is a total no-op when `watched` is false and no unit sim is
        // attached: `code_write_watched` being false means both `range_hits_compiled_code` and
        // `decode_cache.range_hits_code` are false, so every branch inside is skipped and nothing
        // is incremented. The `width == BusWidth::Byte` half of the gate calls it anyway on every
        // unwatched byte write, only to feed the diagnostic unit sim; narrowing that half to
        // "only when a unit sim is actually attached" keeps the diagnostic feed working while
        // dropping the call entirely on the default build's unwatched byte-write path.
        if changed && (watched || (width == BusWidth::Byte && self.unit_sim.0.is_some())) {
            self.note_code_write_hit(physical, width.bytes());
        }
        self.record_data_write(kind, true);
        self.perf.direct_data_pointer_writes += 1;
        Ok(())
    }

    #[inline]
    pub(super) fn record_data_read(&mut self, kind: BusAccessKind, direct: bool) {
        if kind == BusAccessKind::DataRead {
            if direct {
                self.perf.data_direct_reads += 1;
            } else {
                self.perf.data_slow_reads += 1;
            }
        }
    }

    #[inline]
    pub(super) fn record_data_write(&mut self, kind: BusAccessKind, direct: bool) {
        if kind == BusAccessKind::DataWrite {
            if direct {
                self.perf.data_direct_writes += 1;
            } else {
                self.perf.data_slow_writes += 1;
            }
        }
    }

    #[inline]
    fn read_direct_entry(entry: DirectPageCacheEntry, physical: u32, width: BusWidth) -> u32 {
        let offset = (physical & 0x0fff) as usize;
        let ptr = unsafe { entry.ptr.add(offset) };
        match width {
            BusWidth::Byte => unsafe { u32::from(*ptr) },
            BusWidth::Word => unsafe {
                u32::from(u16::from_le(std::ptr::read_unaligned(ptr.cast::<u16>())))
            },
            BusWidth::Dword => unsafe { u32::from_le(std::ptr::read_unaligned(ptr.cast::<u32>())) },
        }
    }

    #[inline]
    fn write_direct_entry(entry: DirectPageCacheEntry, physical: u32, width: BusWidth, value: u32) {
        let offset = (physical & 0x0fff) as usize;
        let ptr = unsafe { entry.ptr.add(offset) };
        match width {
            BusWidth::Byte => unsafe {
                *ptr = value as u8;
            },
            BusWidth::Word => unsafe {
                std::ptr::write_unaligned(ptr.cast::<u16>(), (value as u16).to_le());
            },
            BusWidth::Dword => unsafe {
                std::ptr::write_unaligned(ptr.cast::<u32>(), value.to_le());
            },
        }
    }

    #[inline]
    fn direct_access_page_local(physical: u32, width: BusWidth) -> bool {
        let offset = (physical & 0x0fff) as usize;
        if offset + width.bytes() as usize > 0x1000 {
            return false;
        }
        match width {
            BusWidth::Byte => true,
            BusWidth::Word => physical & 1 == 0,
            BusWidth::Dword => physical & 3 == 0,
        }
    }

    #[inline]
    pub(super) fn read_direct_page_cached<B: CpuBus>(
        &mut self,
        bus: &mut B,
        _linear: u32,
        physical: u32,
        width: BusWidth,
        kind: BusAccessKind,
    ) -> ExecResult<Option<u32>> {
        if !Self::direct_access_page_local(physical, width) {
            return Ok(None);
        }
        if let Some(entry) = self.data_read_pages.get(physical) {
            #[cfg(all(
                feature = "jit",
                target_arch = "x86_64",
                any(target_os = "windows", target_os = "linux")
            ))]
            self.populate_fast_map_from_cached(
                _linear,
                physical,
                entry,
                self.data_read_pages.mapping_epoch(),
                false,
            );
            bus.charge_direct_memory(physical, width, kind)?;
            self.record_data_read(kind, true);
            self.perf.direct_data_pointer_reads += 1;
            return Ok(Some(Self::read_direct_entry(entry, physical, width)));
        }
        let Some(page) = bus.direct_page(physical, kind)? else {
            self.perf.direct_page_misses += 1;
            return Ok(None);
        };
        let offset = (physical & 0x0fff) as usize;
        if page.len < 0x1000 || offset + width.bytes() as usize > page.len {
            self.perf.direct_page_misses += 1;
            return Ok(None);
        }
        self.perf.direct_page_hits += 1;
        self.data_read_pages.insert(page);
        #[cfg(all(
            feature = "jit",
            target_arch = "x86_64",
            any(target_os = "windows", target_os = "linux")
        ))]
        self.populate_fast_map(_linear, physical, page, false);
        bus.charge_direct_memory(physical, width, kind)?;
        self.record_data_read(kind, true);
        self.perf.direct_data_pointer_reads += 1;
        Ok(Some(Self::read_direct_entry(
            DirectPageCacheEntry {
                physical_page: page.physical_page,
                ptr: page.ptr,
            },
            physical,
            width,
        )))
    }

    #[inline]
    pub(super) fn read_direct_byte_page_cached<B: CpuBus>(
        &mut self,
        bus: &mut B,
        linear: u32,
        physical: u32,
        kind: BusAccessKind,
    ) -> ExecResult<Option<u8>> {
        self.read_direct_byte_page_cached_inner(bus, Some(linear), physical, kind)
    }

    #[inline]
    fn read_direct_byte_page_cached_inner<B: CpuBus>(
        &mut self,
        bus: &mut B,
        _linear: Option<u32>,
        physical: u32,
        kind: BusAccessKind,
    ) -> ExecResult<Option<u8>> {
        if let Some(entry) = self.data_read_pages.get(physical) {
            #[cfg(all(
                feature = "jit",
                target_arch = "x86_64",
                any(target_os = "windows", target_os = "linux")
            ))]
            if let Some(linear) = _linear {
                self.populate_fast_map_from_cached(
                    linear,
                    physical,
                    entry,
                    self.data_read_pages.mapping_epoch(),
                    false,
                );
            }
            bus.charge_direct_memory(physical, BusWidth::Byte, kind)?;
            self.record_data_read(kind, true);
            self.perf.direct_data_pointer_reads += 1;
            let offset = (physical & 0x0fff) as usize;
            return Ok(Some(unsafe { *entry.ptr.add(offset) }));
        }
        let Some(page) = bus.direct_page(physical, kind)? else {
            self.perf.direct_page_misses += 1;
            return Ok(None);
        };
        let offset = (physical & 0x0fff) as usize;
        if page.len < 0x1000 || offset >= page.len {
            self.perf.direct_page_misses += 1;
            return Ok(None);
        }
        self.perf.direct_page_hits += 1;
        self.data_read_pages.insert(page);
        #[cfg(all(
            feature = "jit",
            target_arch = "x86_64",
            any(target_os = "windows", target_os = "linux")
        ))]
        if let Some(linear) = _linear {
            self.populate_fast_map(linear, physical, page, false);
        }
        bus.charge_direct_memory(physical, BusWidth::Byte, kind)?;
        self.record_data_read(kind, true);
        self.perf.direct_data_pointer_reads += 1;
        Ok(Some(unsafe { *page.ptr.add(offset) }))
    }

    #[inline]
    /// `Some(changed)` means the direct sized write completed (`changed` = the old bytes differed
    /// from `value`); `None` asks the caller to use the bus path. The `changed` flag drives G2
    /// same-value elision, mirroring the byte variant `write_direct_byte_page_cached`.
    pub(super) fn write_direct_page_cached<B: CpuBus>(
        &mut self,
        bus: &mut B,
        _linear: u32,
        physical: u32,
        width: BusWidth,
        value: u32,
        kind: BusAccessKind,
    ) -> ExecResult<Option<bool>> {
        if !Self::direct_access_page_local(physical, width) {
            return Ok(None);
        }
        if let Some(entry) = self.data_write_pages.get(physical) {
            #[cfg(all(
                feature = "jit",
                target_arch = "x86_64",
                any(target_os = "windows", target_os = "linux")
            ))]
            self.populate_fast_map_from_cached(
                _linear,
                physical,
                entry,
                self.data_write_pages.mapping_epoch(),
                true,
            );
            bus.charge_direct_memory(physical, width, kind)?;
            let changed = Self::read_direct_entry(entry, physical, width) != value;
            Self::write_direct_entry(entry, physical, width, value);
            self.record_data_write(kind, true);
            self.perf.direct_data_pointer_writes += 1;
            return Ok(Some(changed));
        }
        let Some(page) = bus.direct_page(physical, kind)? else {
            self.perf.direct_page_misses += 1;
            return Ok(None);
        };
        let offset = (physical & 0x0fff) as usize;
        if !page.writable || page.len < 0x1000 || offset + width.bytes() as usize > page.len {
            self.perf.direct_page_misses += 1;
            return Ok(None);
        }
        self.perf.direct_page_hits += 1;
        self.data_write_pages.insert(page);
        #[cfg(all(
            feature = "jit",
            target_arch = "x86_64",
            any(target_os = "windows", target_os = "linux")
        ))]
        self.populate_fast_map(_linear, physical, page, true);
        bus.charge_direct_memory(physical, width, kind)?;
        let entry = DirectPageCacheEntry {
            physical_page: page.physical_page,
            ptr: page.ptr,
        };
        let changed = Self::read_direct_entry(entry, physical, width) != value;
        Self::write_direct_entry(entry, physical, width, value);
        self.record_data_write(kind, true);
        self.perf.direct_data_pointer_writes += 1;
        Ok(Some(changed))
    }

    /// `Some(changed)` means the direct write completed; `None` asks the caller to use the bus path.
    #[inline]
    pub(super) fn write_direct_byte_page_cached<B: CpuBus>(
        &mut self,
        bus: &mut B,
        linear: u32,
        physical: u32,
        value: u8,
        kind: BusAccessKind,
    ) -> ExecResult<Option<bool>> {
        self.write_direct_byte_page_cached_inner(bus, Some(linear), physical, value, kind)
    }

    #[inline]
    fn write_direct_byte_page_cached_inner<B: CpuBus>(
        &mut self,
        bus: &mut B,
        _linear: Option<u32>,
        physical: u32,
        value: u8,
        kind: BusAccessKind,
    ) -> ExecResult<Option<bool>> {
        if let Some(entry) = self.data_write_pages.get(physical) {
            #[cfg(all(
                feature = "jit",
                target_arch = "x86_64",
                any(target_os = "windows", target_os = "linux")
            ))]
            if let Some(linear) = _linear {
                self.populate_fast_map_from_cached(
                    linear,
                    physical,
                    entry,
                    self.data_write_pages.mapping_epoch(),
                    true,
                );
            }
            bus.charge_direct_memory(physical, BusWidth::Byte, kind)?;
            let offset = (physical & 0x0fff) as usize;
            let changed = unsafe { *entry.ptr.add(offset) != value };
            unsafe {
                *entry.ptr.add(offset) = value;
            }
            self.record_data_write(kind, true);
            self.perf.direct_data_pointer_writes += 1;
            return Ok(Some(changed));
        }
        let Some(page) = bus.direct_page(physical, kind)? else {
            self.perf.direct_page_misses += 1;
            return Ok(None);
        };
        let offset = (physical & 0x0fff) as usize;
        if !page.writable || page.len < 0x1000 || offset >= page.len {
            self.perf.direct_page_misses += 1;
            return Ok(None);
        }
        self.perf.direct_page_hits += 1;
        self.data_write_pages.insert(page);
        #[cfg(all(
            feature = "jit",
            target_arch = "x86_64",
            any(target_os = "windows", target_os = "linux")
        ))]
        if let Some(linear) = _linear {
            self.populate_fast_map(linear, physical, page, true);
        }
        bus.charge_direct_memory(physical, BusWidth::Byte, kind)?;
        let changed = unsafe { *page.ptr.add(offset) != value };
        unsafe {
            *page.ptr.add(offset) = value;
        }
        self.record_data_write(kind, true);
        self.perf.direct_data_pointer_writes += 1;
        Ok(Some(changed))
    }
    // (`read_rm_u8` was removed with the legacy 0x84 TEST r/m8,reg8 handler — its only remaining
    // caller. The converted flags-misc executor reads the byte r/m via `read_operand_u8` on the
    // pre-decoded operand instead. `write_rm_u8` was removed earlier with the legacy 0x88 MOV
    // r/m8,r8 handler. The sized/read siblings remain in use by the fallback handlers.)

    pub(super) fn read_operand_u8<B: CpuBus>(
        &mut self,
        bus: &mut B,
        operand: RmOperand,
    ) -> ExecResult<u8> {
        match operand {
            RmOperand::Register(index) => Ok(self.read_gpr8(index)),
            RmOperand::Memory(memory) => {
                self.read_memory_u8(bus, memory.segment, memory.offset, BusAccessKind::DataRead)
            }
        }
    }

    pub(super) fn write_operand_u8<B: CpuBus>(
        &mut self,
        bus: &mut B,
        operand: RmOperand,
        value: u8,
    ) -> ExecResult<()> {
        match operand {
            RmOperand::Register(index) => {
                self.write_gpr8(index, value);
                Ok(())
            }
            RmOperand::Memory(memory) => self.write_memory_u8(
                bus,
                memory.segment,
                memory.offset,
                value,
                BusAccessKind::DataWrite,
            ),
        }
    }

    pub(super) fn read_operand_sized<B: CpuBus>(
        &mut self,
        bus: &mut B,
        operand: RmOperand,
        size: OperandSize,
    ) -> ExecResult<u32> {
        match operand {
            RmOperand::Register(index) => Ok(self.read_gpr_sized(index, size)),
            RmOperand::Memory(memory) => self.read_memory_sized(
                bus,
                memory.segment,
                memory.offset,
                size,
                BusAccessKind::DataRead,
            ),
        }
    }

    pub(super) fn write_operand_sized<B: CpuBus>(
        &mut self,
        bus: &mut B,
        operand: RmOperand,
        size: OperandSize,
        value: u32,
    ) -> ExecResult<()> {
        match operand {
            RmOperand::Register(index) => {
                self.write_gpr_sized(index, size, value);
                Ok(())
            }
            RmOperand::Memory(memory) => self.write_memory_sized(
                bus,
                memory.segment,
                memory.offset,
                size,
                value,
                BusAccessKind::DataWrite,
            ),
        }
    }

    pub(super) fn read_memory_u8<B: CpuBus>(
        &mut self,
        bus: &mut B,
        segment: SegmentIndex,
        offset: u32,
        kind: BusAccessKind,
    ) -> ExecResult<u8> {
        let linear = self.segment_linear_byte(segment, offset, false)?;
        self.read_linear_u8(bus, linear, kind)
    }

    fn read_linear_u8<B: CpuBus>(
        &mut self,
        bus: &mut B,
        linear: u32,
        kind: BusAccessKind,
    ) -> ExecResult<u8> {
        #[cfg(all(
            feature = "jit",
            target_arch = "x86_64",
            any(target_os = "windows", target_os = "linux")
        ))]
        if let Some((physical, ptr, mode13)) =
            self.fast_map_data_slot(linear, BusWidth::Byte, false)
        {
            return self
                .finish_fast_map_read(bus, physical, ptr, mode13, BusWidth::Byte, kind)
                .map(|value| value as u8);
        }
        let physical = if self.control.cr0 & CR0_PG == 0 {
            linear
        } else {
            self.translate_linear(bus, linear, false)?
        };
        if let Some(value) = self.read_direct_byte_page_cached(bus, linear, physical, kind)? {
            return Ok(value);
        }
        let read = bus.read_memory_direct(physical, BusWidth::Byte, kind)?;
        self.record_data_read(kind, read.direct);
        Ok(read.value as u8)
    }

    pub(super) fn write_memory_u8<B: CpuBus>(
        &mut self,
        bus: &mut B,
        segment: SegmentIndex,
        offset: u32,
        value: u8,
        kind: BusAccessKind,
    ) -> ExecResult<()> {
        let linear = self.segment_linear_byte(segment, offset, true)?;
        self.write_linear_u8(bus, linear, value, kind)
    }

    fn write_linear_u8<B: CpuBus>(
        &mut self,
        bus: &mut B,
        linear: u32,
        value: u8,
        kind: BusAccessKind,
    ) -> ExecResult<()> {
        #[cfg(all(
            feature = "jit",
            target_arch = "x86_64",
            any(target_os = "windows", target_os = "linux")
        ))]
        if let Some((physical, ptr, mode13)) = self.fast_map_data_slot(linear, BusWidth::Byte, true)
        {
            return self.finish_fast_map_write(
                bus,
                physical,
                ptr,
                mode13,
                BusWidth::Byte,
                u32::from(value),
                kind,
            );
        }
        let physical = if self.control.cr0 & CR0_PG == 0 {
            self.record_write_page(linear);
            linear
        } else {
            self.translate_linear(bus, linear, true)?
        };
        if let Some(changed) =
            self.write_direct_byte_page_cached(bus, linear, physical, value, kind)?
        {
            if changed {
                self.note_code_write(physical, 1);
            }
            return Ok(());
        }
        self.note_code_write(physical, 1);
        let write = bus.write_memory_direct(physical, BusWidth::Byte, u32::from(value), kind)?;
        self.record_data_write(kind, write.direct);
        Ok(())
    }

    /// Validate a data access's *kind* against the segment descriptor's type field: a
    /// write through a read-only data segment, or any access through an execute-only
    /// code segment loaded into a data-segment register, is #GP (386 PRM 5-12, "Data
    /// segments can be read-only or read/write... Code segments can be execute-only or
    /// execute/read"). Real mode and V86 mode always carry the fully-permissive
    /// `access = 0x93` (`SegmentRegister::real`), so this only ever rejects something in
    /// protected mode; the caller gates on that to skip the check entirely otherwise.
    /// Instruction fetch never routes through here (it uses `code_linear_for_offset`),
    /// so CS's own readability never needs checking on this path -- only the case of a
    /// *data* segment register (DS/ES/FS/GS/SS) that happens to hold a code descriptor.
    pub(super) fn check_segment_access_kind(
        &self,
        segment: SegmentIndex,
        access: u8,
        write: bool,
    ) -> ExecResult<()> {
        if !self.is_protected_mode() || self.is_v86_mode() {
            return Ok(());
        }
        let is_code = access & 0x08 != 0; // descriptor type bit 3
        let ok = if is_code {
            // A code descriptor addressed as data: legal only for a read, and only if
            // the code segment's readable bit (type bit 1) is set.
            !write && access & 0x02 != 0
        } else {
            // A data descriptor: legal for a read always; a write needs the writable
            // bit (type bit 1) set.
            !write || access & 0x02 != 0
        };
        if ok {
            Ok(())
        } else {
            Err(segment_limit_fault(segment))
        }
    }

    #[inline]
    pub(super) fn segment_linear_byte(
        &self,
        segment: SegmentIndex,
        offset: u32,
        write: bool,
    ) -> ExecResult<u32> {
        let descriptor = self.registers.segment(segment);
        self.check_segment_access_kind(segment, descriptor.access, write)?;
        if descriptor.base == 0 && descriptor.limit == u32::MAX {
            return Ok(offset);
        }
        let expand_down = self.is_protected_mode()
            && !self.is_v86_mode()
            && descriptor.access & 0x18 == 0x10
            && descriptor.access & 0x04 != 0;
        let in_limit = if expand_down {
            // 386 PRM 5-12: an expand-down segment's valid offsets are those ABOVE the
            // limit (up to 0xffff, or 0xffff_ffff for a 32-bit-default segment), the
            // reverse of the normal sense.
            let ceiling = if descriptor.default_size_32 {
                u32::MAX
            } else {
                0xffff
            };
            offset > descriptor.limit && offset <= ceiling
        } else {
            offset <= descriptor.limit
        };
        if !in_limit {
            return Err(segment_limit_fault(segment));
        }
        Ok(descriptor.base.wrapping_add(offset))
    }

    pub(super) fn read_memory_sized<B: CpuBus>(
        &mut self,
        bus: &mut B,
        segment: SegmentIndex,
        offset: u32,
        size: OperandSize,
        kind: BusAccessKind,
    ) -> ExecResult<u32> {
        self.check_alignment(offset, size.bytes())?;
        self.read_memory_bus_width(bus, segment, offset, size.bus_width(), kind)
    }

    pub(super) fn write_memory_sized<B: CpuBus>(
        &mut self,
        bus: &mut B,
        segment: SegmentIndex,
        offset: u32,
        size: OperandSize,
        value: u32,
        kind: BusAccessKind,
    ) -> ExecResult<()> {
        self.check_alignment(offset, size.bytes())?;
        self.write_memory_bus_width(bus, segment, offset, size.bus_width(), value, kind)
    }

    pub(super) fn read_memory_bus_width<B: CpuBus>(
        &mut self,
        bus: &mut B,
        segment: SegmentIndex,
        offset: u32,
        width: BusWidth,
        kind: BusAccessKind,
    ) -> ExecResult<u32> {
        if width == BusWidth::Byte {
            return self
                .read_memory_u8(bus, segment, offset, kind)
                .map(u32::from);
        }
        let linear = self.segment_linear_range(segment, offset, width.bytes(), false)?;
        if self.is_paging_enabled() && Self::linear_range_crosses_page(linear, width.bytes()) {
            return self.read_paged_cross_page(bus, linear, width.bytes(), kind);
        }
        self.read_linear_fragment(bus, linear, width, kind)
    }

    pub(super) fn write_memory_bus_width<B: CpuBus>(
        &mut self,
        bus: &mut B,
        segment: SegmentIndex,
        offset: u32,
        width: BusWidth,
        value: u32,
        kind: BusAccessKind,
    ) -> ExecResult<()> {
        if width == BusWidth::Byte {
            return self.write_memory_u8(bus, segment, offset, value as u8, kind);
        }
        let linear = self.segment_linear_range(segment, offset, width.bytes(), true)?;
        if self.is_paging_enabled() && Self::linear_range_crosses_page(linear, width.bytes()) {
            return self.write_paged_cross_page(bus, linear, width.bytes(), value, kind);
        }
        self.write_linear_fragment(bus, linear, width, value, kind)
    }

    #[inline]
    fn linear_range_crosses_page(linear: u32, width: u32) -> bool {
        (linear & 0x0fff) + width > 0x1000
    }

    fn read_paged_cross_page<B: CpuBus>(
        &mut self,
        bus: &mut B,
        linear: u32,
        width: u32,
        kind: BusAccessKind,
    ) -> ExecResult<u32> {
        let mut value = 0u32;
        let mut completed = 0u32;
        while completed < width {
            let at = linear.wrapping_add(completed);
            let fragment = Self::page_local_fragment_width(at, width - completed);
            value |= self.read_linear_fragment(bus, at, fragment, kind)? << (completed * 8);
            completed += fragment.bytes();
        }
        Ok(value)
    }

    fn write_paged_cross_page<B: CpuBus>(
        &mut self,
        bus: &mut B,
        linear: u32,
        width: u32,
        value: u32,
        kind: BusAccessKind,
    ) -> ExecResult<()> {
        let mut completed = 0u32;
        while completed < width {
            let at = linear.wrapping_add(completed);
            let fragment = Self::page_local_fragment_width(at, width - completed);
            // G2: mask to the fragment width so the same-value compare in write_linear_fragment
            // sees only the bytes this fragment stores. Unmasked high bits made every cross-page
            // sub-dword fragment read as changed, defeating elision; the store itself writes only
            // `fragment` bytes either way, so masking is behavior-neutral for the write.
            let shifted = value >> (completed * 8);
            let fragment_value = match fragment {
                BusWidth::Byte => shifted & 0xff,
                BusWidth::Word => shifted & 0xffff,
                BusWidth::Dword => shifted,
            };
            self.write_linear_fragment(bus, at, fragment, fragment_value, kind)?;
            completed += fragment.bytes();
        }
        Ok(())
    }

    #[inline]
    fn page_local_fragment_width(linear: u32, remaining: u32) -> BusWidth {
        let page_remaining = 0x1000 - (linear & 0x0fff);
        if remaining >= 4 && page_remaining >= 4 && linear & 3 == 0 {
            BusWidth::Dword
        } else if remaining >= 2 && page_remaining >= 2 && linear & 1 == 0 {
            BusWidth::Word
        } else {
            BusWidth::Byte
        }
    }

    fn read_linear_fragment<B: CpuBus>(
        &mut self,
        bus: &mut B,
        linear: u32,
        width: BusWidth,
        kind: BusAccessKind,
    ) -> ExecResult<u32> {
        if width == BusWidth::Byte {
            return self.read_linear_u8(bus, linear, kind).map(u32::from);
        }
        #[cfg(all(
            feature = "jit",
            target_arch = "x86_64",
            any(target_os = "windows", target_os = "linux")
        ))]
        if let Some((physical, ptr, mode13)) = self.fast_map_data_slot(linear, width, false) {
            return self.finish_fast_map_read(bus, physical, ptr, mode13, width, kind);
        }
        let physical = self.translate_linear(bus, linear, false)?;
        if let Some(value) = self.read_direct_page_cached(bus, linear, physical, width, kind)? {
            return Ok(value);
        }
        let read = bus.read_memory_direct(physical, width, kind)?;
        self.record_data_read(kind, read.direct);
        Ok(read.value)
    }

    fn write_linear_fragment<B: CpuBus>(
        &mut self,
        bus: &mut B,
        linear: u32,
        width: BusWidth,
        value: u32,
        kind: BusAccessKind,
    ) -> ExecResult<()> {
        if width == BusWidth::Byte {
            return self.write_linear_u8(bus, linear, value as u8, kind);
        }
        #[cfg(all(
            feature = "jit",
            target_arch = "x86_64",
            any(target_os = "windows", target_os = "linux")
        ))]
        if let Some((physical, ptr, mode13)) = self.fast_map_data_slot(linear, width, true) {
            return self.finish_fast_map_write(bus, physical, ptr, mode13, width, value, kind);
        }
        let physical = self.translate_linear(bus, linear, true)?;
        // G2: same-value elision for sized stores. Probe the code watch first (side-effect free);
        // when the store misses all watched code this costs exactly what the old unconditional
        // note_code_write cost. On a direct-page hit we read the old bytes, write, and invalidate
        // only when the value actually changed a watched code byte, so a patch-then-restore of
        // identical bytes never triggers a cold re-decode. Ordering is compare-then-write-then-
        // invalidate, matching the shipped byte path in write_linear_u8.
        let watched = self.code_write_watched(physical, width.bytes());
        match self.write_direct_page_cached(bus, linear, physical, width, value, kind)? {
            Some(changed) => {
                if watched && changed {
                    self.note_code_write_hit(physical, width.bytes());
                }
                Ok(())
            }
            None => {
                // Bus fallback (MMIO or a fragment the direct-page cache could not serve). MMIO
                // reads are side-effecting, so the old bytes cannot be pre-read to compare; a
                // watched store here invalidates unconditionally.
                if watched {
                    self.note_code_write_hit(physical, width.bytes());
                }
                let write = bus.write_memory_direct(physical, width, value, kind)?;
                self.record_data_write(kind, write.direct);
                Ok(())
            }
        }
    }

    // #AC alignment check (486). A data access faults vector 17 (no error code) when
    // CR0.AM and EFLAGS.AC are both set and the access runs at CPL 3, and the effective
    // address is not naturally aligned for its width (word on a 2-byte boundary, dword on
    // a 4-byte boundary). Supervisor accesses (CPL < 3) and instruction fetches are exempt;
    // fetches never route through this helper. Byte accesses (width 1) are always aligned.
    fn check_alignment(&self, offset: u32, width: u32) -> ExecResult<()> {
        if width <= 1 || !self.alignment_armed {
            return Ok(());
        }
        if self.current_privilege_level() == 3 && !offset.is_multiple_of(width) {
            // Real 486 #AC pushes a zero error code; this core models it without one,
            // matching the rest of the spec's fault contract. Flagged as a divergence.
            return Err(InternalFault::Exception {
                vector: 17,
                error_code: None,
            });
        }
        Ok(())
    }

    pub(super) fn translate_segmented<B: CpuBus>(
        &mut self,
        bus: &mut B,
        segment: SegmentIndex,
        offset: u32,
        width: u32,
        write: bool,
    ) -> ExecResult<(u32, u32)> {
        let linear = self.segment_linear_range(segment, offset, width, write)?;
        let physical = self.translate_linear(bus, linear, write)?;
        if write {
            // G2 out of scope: this is a translate-time invalidation with no value in hand yet
            // (the store value arrives later, through the operand write path), so there is nothing
            // to compare and the invalidation stays unconditional.
            self.note_code_write(physical, width);
        }
        Ok((linear, physical))
    }

    fn segment_linear_range(
        &self,
        segment: SegmentIndex,
        offset: u32,
        width: u32,
        write: bool,
    ) -> ExecResult<u32> {
        let descriptor = self.registers.segment(segment);
        self.check_segment_access_kind(segment, descriptor.access, write)?;
        let linear = if descriptor.base == 0 && descriptor.limit == u32::MAX {
            offset
        } else {
            let last = offset.saturating_add(width.saturating_sub(1));
            let expand_down = self.is_protected_mode()
                && !self.is_v86_mode()
                && descriptor.access & 0x18 == 0x10
                && descriptor.access & 0x04 != 0;
            let in_limit = if expand_down {
                let ceiling = if descriptor.default_size_32 {
                    u32::MAX
                } else {
                    0xffff
                };
                offset > descriptor.limit && last <= ceiling
            } else {
                offset <= descriptor.limit && last <= descriptor.limit
            };
            if !in_limit {
                return Err(segment_limit_fault(segment));
            }
            descriptor.base.wrapping_add(offset)
        };
        Ok(linear)
    }

    pub(super) fn translate_linear<B: CpuBus>(
        &mut self,
        bus: &mut B,
        linear: u32,
        write: bool,
    ) -> ExecResult<u32> {
        self.translate_linear_checked(bus, linear, write, PagingAccessor::Current)
    }

    /// Like `translate_linear`, but for accesses to descriptor tables (GDT/LDT/IDT)
    /// and TSS fields during exception delivery, segment loads, and task switches.
    /// These are architecturally implicit supervisor accesses (386 PRM 6.2, 7.2):
    /// the processor consults them to set up or validate a privilege transition, so
    /// they must not be checked against the CPL of the code that triggered the
    /// transition. A V86 task (always CPL 3) or a ring-3 CS delivering through an
    /// interrupt gate must be able to read its own TSS/GDT even when those pages
    /// are marked supervisor-only (U/S=0), exactly as real silicon does. Forcing
    /// `user = false` here also means a WP-clear supervisor write (the 386 default)
    /// is never blocked by a read-only system-structure page.
    pub(super) fn translate_linear_system<B: CpuBus>(
        &mut self,
        bus: &mut B,
        linear: u32,
        write: bool,
    ) -> ExecResult<u32> {
        self.translate_linear_checked(bus, linear, write, PagingAccessor::Supervisor)
    }

    fn write_page_walk_entry<B: CpuBus>(
        &mut self,
        bus: &mut B,
        physical: u32,
        value: u32,
    ) -> ExecResult<()> {
        bus.write_memory(
            physical,
            BusWidth::Dword,
            value,
            BusAccessKind::PageWalkWrite,
        )?;
        self.record_write_page(physical);
        // G2 out of scope: a page-walk A/D-bit store invalidates unconditionally. The old PTE was
        // already consumed by the walk and the bus write above committed the new bytes, so there
        // is no old-byte snapshot to compare, and self-modifying page tables are rare enough that
        // the extra pre-read would not earn its cost.
        self.note_code_write(physical, BusWidth::Dword.bytes());
        Ok(())
    }

    fn translate_linear_checked<B: CpuBus>(
        &mut self,
        bus: &mut B,
        linear: u32,
        write: bool,
        accessor: PagingAccessor,
    ) -> ExecResult<u32> {
        if !self.is_paging_enabled() {
            if write {
                self.record_write_page(linear);
            }
            return Ok(linear);
        }

        // Paging privilege: CPL 3 is a user access, CPL 0-2 are supervisor. A
        // system-structure access is forced supervisor regardless of the current
        // CPL (see `translate_linear_system`).
        let user = match accessor {
            // CPL is the cached quantity (`current_privilege_level`/`self.cpl`), not a live
            // read of CS.selector -- see that method for why a live formula misclassifies
            // the monitor's own ring-0 stack pushes as user during V86-source exception
            // delivery (source CS's RPL bits are irrelevant once cpl has already been set
            // to the entered level).
            PagingAccessor::Current => self.current_privilege_level() == 3,
            PagingAccessor::Supervisor => false,
        };
        // CR0.WP (a 486 addition) makes supervisor writes obey the page R/W bit too.
        // With WP clear, supervisor writes to read-only pages succeed (386 behavior).
        let wp = self.control.cr0 & CR0_WP != 0;

        // TLB fast path: a cached entry skips the two page-table reads (and the
        // accessed-bit write the fill already did). The protection check is redone
        // from the cached page bits against the *current* accessor (CPL can change
        // without a flush); WP changes flush, so `wp` is consistent within a
        // generation. A write to a page whose dirty bit is not yet set falls through
        // to the walk so the PTE's D bit is updated.
        //
        // A hit NEVER raises the fault itself -- it either serves the access or falls
        // through to the walk. A cached entry can only be more restrictive than the
        // live tables (a guest that relaxes a PTE and skips the required flush), and
        // this core's TLB is 1024 entries against real silicon's 32-64, so such an
        // entry survives here long after hardware would have evicted it. Deciding the
        // fault from the walk instead costs nothing on the hot path (it runs only when
        // the hit would not have served the access anyway) and keeps the page tables
        // the sole authority on what faults. Repro that found this: TSUMERA (Borland
        // 32RTM under VCPI) at exit -- its ring-0 DPMI host flips a data page from R/O
        // to R/W without reloading CR3, and the ring-3 refcount decrement that follows
        // took a spurious #PF(7) off the stale entry.
        let page = linear >> 12;
        if let Some(e) = self.tlb.lookup(page) {
            let permitted = if user {
                e.user && (!write || e.writable)
            } else {
                !write || !wp || e.writable
            };
            // Serve the hit only when it permits the access and needs no D-bit update.
            // Anything else falls through to the walk, which re-reads the page tables
            // and is the authority on both the permission and the fault it raises.
            if permitted && (!write || e.dirty) {
                let physical = e.phys | (linear & 0x0000_0fff);
                if write {
                    self.record_write_page(physical);
                }
                return Ok(physical);
            }
        }

        let directory = self.control.cr3 & 0xffff_f000;
        let directory_address = directory + (((linear >> 22) & 0x03ff) * 4);
        let mut pde = bus.read_memory(
            directory_address,
            BusWidth::Dword,
            BusAccessKind::PageWalkRead,
        )?;
        if pde & 1 == 0 {
            self.control.cr2 = linear;
            return Err(InternalFault::Exception {
                vector: 14,
                error_code: Some(page_fault_code(false, write, user)),
            });
        }
        if pde & 0x20 == 0 {
            pde |= 0x20;
            self.write_page_walk_entry(bus, directory_address, pde)?;
        }

        let table_address = (pde & 0xffff_f000) + (((linear >> 12) & 0x03ff) * 4);
        let mut pte =
            bus.read_memory(table_address, BusWidth::Dword, BusAccessKind::PageWalkRead)?;
        if pte & 1 == 0 {
            self.control.cr2 = linear;
            return Err(InternalFault::Exception {
                vector: 14,
                error_code: Some(page_fault_code(false, write, user)),
            });
        }

        // Protection check. The combined R/W and U/S come from ANDing the PDE and
        // PTE bits (bit 1 and bit 2). A page is user-accessible only if both U/S
        // bits are set, and writable only if both R/W bits are set.
        //   - A user access faults if it touches a supervisor page, or writes a
        //     read-only page.
        //   - A supervisor write faults only when CR0.WP is set and the page is
        //     read-only (combined R/W = 0). With WP clear, supervisor writes pass.
        // Either way the fault is present=1 and the error-code U/S bit reflects the
        // access (user), not the page. Checked before the dirty bit is set so a
        // faulting write leaves it clear.
        let writable = pde & pte & 0x2 != 0;
        let user_accessible = pde & pte & 0x4 != 0;
        let protection_fault = if user {
            !user_accessible || (write && !writable)
        } else {
            write && wp && !writable
        };
        if protection_fault {
            self.control.cr2 = linear;
            return Err(InternalFault::Exception {
                vector: 14,
                error_code: Some(page_fault_code(true, write, user)),
            });
        }

        let dirty = if write { 0x40 } else { 0 };
        let accessed_dirty = 0x20 | dirty;
        if pte & accessed_dirty != accessed_dirty {
            pte |= accessed_dirty;
            self.write_page_walk_entry(bus, table_address, pte)?;
        }

        // Cache the completed translation. Only reached on the success path, so a
        // page that faulted (not present / protection) is never cached. `dirty`
        // records whether the PTE's D bit is now set, so a later read hits but a
        // first write to a still-clean page re-walks to set it.
        let physical_page = pte & 0xffff_f000;
        let entry_dirty = pte & 0x40 != 0;
        let previous = self
            .tlb
            .insert(page, physical_page, writable, user_accessible, entry_dirty);
        #[cfg(not(all(
            feature = "jit",
            target_arch = "x86_64",
            any(target_os = "windows", target_os = "linux")
        )))]
        let _ = previous;
        #[cfg(all(
            feature = "jit",
            target_arch = "x86_64",
            any(target_os = "windows", target_os = "linux")
        ))]
        if let Some(previous) = previous {
            let same_residency = previous.tag == page
                && previous.phys == physical_page
                && previous.writable == writable
                && previous.user == user_accessible
                && (!previous.dirty || entry_dirty);
            if !same_residency {
                self.jit_fast_map.invalidate_page(previous.tag << 12);
            }
        }

        let physical = physical_page | (linear & 0x0000_0fff);
        if write {
            self.record_write_page(physical);
        }
        Ok(physical)
    }

    pub(super) fn push<B: CpuBus>(
        &mut self,
        bus: &mut B,
        value: u32,
        operand_size: OperandSize,
    ) -> ExecResult<()> {
        let width = operand_size.bytes();
        // The write PRECEDES the (E)SP commit: a push whose stack write faults
        // (#PF on a not-yet-committed stack page under a lazy-commit DPMI host,
        // or a #GP/#SS limit violation) must leave (E)SP at its pre-instruction
        // value so the post-handler restart re-executes cleanly. Committing
        // first left ESP decremented across the fault; CWSDPMI's commit-and-
        // retry stack growth then double-decremented, shifting every later
        // stack slot one down and handing DJGPP code shifted callee-saved
        // registers on the next epilogue (found via Quake's crt1
        // setup_environment crash).
        if self.stack_is_32bit() {
            // SS.B=1: implicit stack references use the full 32-bit ESP, for both
            // 16-bit and 32-bit operand-size pushes (386 PRM 16.2: the B bit picks
            // the stack-pointer width, independent of operand size).
            let esp = self.registers.esp().wrapping_sub(width);
            self.write_memory_sized(
                bus,
                SegmentIndex::Ss,
                esp,
                operand_size,
                value,
                BusAccessKind::DataWrite,
            )?;
            self.registers.set_esp(esp);
        } else {
            // SS.B=0 (real mode, V86, or a 16-bit protected-mode stack): the address
            // comes from SP only, only SP advances, and ESP's high word is preserved
            // (real silicon wraps SP, not ESP, on this stack).
            let sp = self.read_gpr16(4).wrapping_sub(width as u16);
            self.write_memory_sized(
                bus,
                SegmentIndex::Ss,
                u32::from(sp),
                operand_size,
                value,
                BusAccessKind::DataWrite,
            )?;
            self.write_gpr16(4, sp);
        }
        Ok(())
    }

    pub(super) fn pop<B: CpuBus>(
        &mut self,
        bus: &mut B,
        operand_size: OperandSize,
    ) -> ExecResult<u32> {
        let width = operand_size.bytes();
        if self.stack_is_32bit() {
            let esp = self.registers.esp();
            let value = self.read_memory_sized(
                bus,
                SegmentIndex::Ss,
                esp,
                operand_size,
                BusAccessKind::DataRead,
            )?;
            self.registers.set_esp(esp.wrapping_add(width));
            Ok(value)
        } else {
            // SS.B=0: read from SP and advance only SP, preserving ESP's high word.
            let sp = self.read_gpr16(4);
            let value = self.read_memory_sized(
                bus,
                SegmentIndex::Ss,
                u32::from(sp),
                operand_size,
                BusAccessKind::DataRead,
            )?;
            self.write_gpr16(4, sp.wrapping_add(width as u16));
            Ok(value)
        }
    }
}
