// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

impl CpuGsw {
    pub fn reset(&mut self) {
        #[cfg(feature = "jit")]
        let native_backend_enabled = self.jit_direct.backend_enabled();
        *self = Self::default();
        #[cfg(feature = "jit")]
        self.jit_direct.set_backend_enabled(native_backend_enabled);
    }

    /// Compute the six arithmetic flags `pending_flags` represents, returning the full eflags value with
    /// them applied (control flags untouched). Pure: does not mutate. Returns current eflags if no pending.
    pub(super) fn materialized_eflags(&self) -> u32 {
        if self.pending_flags.is_none() {
            return self.registers.eflags;
        }
        let p = &self.pending_flags;
        let w = p.width();
        let mask = width_mask(w);
        let sign = width_sign(w);
        let (cf, of, af, clear) = match p.op() {
            LazyFlagOp::Sub => (
                u64::from(p.a) < u64::from(p.b),
                ((p.a ^ p.b) & (p.a ^ p.result) & sign) != 0,
                ((p.a ^ p.b ^ p.result) & 0x10) != 0,
                FLAG_CF | FLAG_OF | FLAG_AF | FLAG_ZF | FLAG_SF | FLAG_PF,
            ),
            LazyFlagOp::Add => (
                u64::from(p.a) + u64::from(p.b) > u64::from(mask),
                ((p.a ^ p.result) & (p.b ^ p.result) & sign) != 0,
                ((p.a ^ p.b ^ p.result) & 0x10) != 0,
                FLAG_CF | FLAG_OF | FLAG_AF | FLAG_ZF | FLAG_SF | FLAG_PF,
            ),
            LazyFlagOp::Logic => (
                false,
                false,
                self.registers.eflags & FLAG_AF != 0,
                FLAG_CF | FLAG_OF | FLAG_ZF | FLAG_SF | FLAG_PF,
            ),
        };
        let cf = p.cf_override().unwrap_or(cf);
        let zf = p.result & mask == 0;
        let sf = p.result & sign != 0;
        let pf = parity(p.result as u8);
        let mut e = self.registers.eflags & !clear;
        if cf {
            e |= FLAG_CF;
        }
        if of {
            e |= FLAG_OF;
        }
        if af {
            e |= FLAG_AF;
        }
        if zf {
            e |= FLAG_ZF;
        }
        if sf {
            e |= FLAG_SF;
        }
        if pf {
            e |= FLAG_PF;
        }
        e | 0x2
    }

    /// Settle any pending arithmetic flags into `registers.eflags` and clear `pending_flags`.
    pub(super) fn materialize_flags(&mut self) {
        if !self.pending_flags.is_none() {
            self.registers.eflags = self.materialized_eflags();
            self.pending_flags = PendingFlags::default();
            self.perf.flag_materializations += 1;
        }
    }

    /// The architectural EFLAGS value, with any deferred arithmetic flags applied. The public,
    /// non-mutating accessor for ANY reader that needs the whole eflags word (tests, the conformance
    /// harness, external callers).
    pub fn eflags(&self) -> u32 {
        self.materialized_eflags()
    }

    pub(super) fn invalidate_code_caches(&mut self) {
        self.perf.decode_inval_other += 1;
        self.perf.code_invalidations += 1;
        self.invalidate_code_caches_uncounted();
    }

    /// The CS-load hook: a CS load never flushes the decode cache. The cache is keyed by LINEAR
    /// address, and every other decode input a CS load could change is re-checked on each hit
    /// instead of being flushed away here: the D bit is part of the line (`DecodeLine::d`,
    /// compared in `get`), and the fetch limit is re-checked at both hit sites (a violation misses
    /// to `decode`, which raises the exact fault). This matters because pmode workloads
    /// load CS at every interrupt edge and V86 monitor round-trip: the Doom 586 census measured
    /// 326M whole-cache CS-load flushes in a 12.4G-instruction timedemo (one per ~38
    /// instructions), pinning decode_hit at 21% regardless of cache size.
    ///
    /// The eip-window prefetch is still dropped: not every far-transfer path routes its eip
    /// write through `set_eip`, and the historical blanket flush covered those. O(1), refills
    /// on the next fetch. The fetch page and code-page translation are linear/physical-keyed
    /// and stay live.
    pub(super) fn invalidate_code_caches_for_cs_load(&mut self) {
        self.perf.decode_inval_cs_load += 1;
        self.rep_resume_active = false;
        self.rep_execution.resume = None;
        self.prefetch.invalidate();
    }

    fn invalidate_code_caches_uncounted(&mut self) {
        self.invalidate_decode_frontend();
        #[cfg(feature = "jit")]
        self.jit_direct.clear();
    }

    fn invalidate_decode_frontend(&mut self) {
        self.code_page.valid = false;
        self.prefetch.invalidate();
        self.fetch_page.invalidate();
        // Paging, mode, A20, and physical-map changes route through here. Any can make the same
        // linear address decode from different bytes, so invalidate the lines and their SMC marks.
        self.decode_cache.invalidate_and_clear_code_marks();
    }

    pub(super) fn invalidate_translation_code_caches(&mut self) {
        self.perf.decode_inval_other += 1;
        self.perf.code_invalidations += 1;
        self.invalidate_decode_frontend();
        #[cfg(feature = "jit")]
        self.jit_direct.invalidate_translation();
    }

    fn invalidate_direct_pages(&mut self) {
        self.data_read_pages.invalidate();
        self.data_write_pages.invalidate();
        self.fetch_page.invalidate();
        #[cfg(all(
            feature = "jit",
            target_arch = "x86_64",
            any(target_os = "windows", target_os = "linux")
        ))]
        self.jit_fast_map.invalidate_all();
    }

    pub(super) fn flush_tlb_and_code_caches(&mut self) {
        self.tlb.flush();
        // Clear the physical caches with the linear FastMap so alias-verification tags and
        // mappings are rebuilt from the current translation and permissions.
        self.data_read_pages.invalidate();
        self.data_write_pages.invalidate();
        #[cfg(all(
            feature = "jit",
            target_arch = "x86_64",
            any(target_os = "windows", target_os = "linux")
        ))]
        self.jit_fast_map.invalidate_all();
        self.invalidate_translation_code_caches();
    }

    pub(super) fn set_eip(&mut self, eip: u32) {
        self.rep_resume_active = false;
        self.rep_execution.resume = None;
        self.registers.eip = eip;
        self.prefetch.invalidate();
    }

    pub(super) fn record_write_page(&mut self, physical: u32) {
        let page = physical >> 12;
        if self.written_pages.contains(&Some(page)) {
            return;
        }
        if let Some(slot) = self.written_pages.iter_mut().find(|slot| slot.is_none()) {
            *slot = Some(page);
            self.written_count += 1;
        } else {
            self.written_pages_overflow = true;
        }
    }

    /// The machine calls this when the A20 gate toggles. A20 is masked at the bus on every access,
    /// so the CPU never sees it directly, yet it changes which physical bytes back a linear address
    /// near the 1 MB wrap. The prefetch (cached bytes) and the decode cache (cached decode of those
    /// bytes) would otherwise replay stale content, so invalidate both. Rare; a coarse flush is fine.
    pub fn note_a20_changed(&mut self) {
        self.invalidate_code_caches();
        self.invalidate_direct_pages();
    }

    pub fn note_direct_map_changed(&mut self) {
        self.invalidate_code_caches();
        self.invalidate_direct_pages();
        self.perf.direct_map_invalidations += 1;
    }

    /// Drop cached data pointers after a device aperture changes without
    /// discarding decoded or compiled guest code. The bus mapping epoch is
    /// global, so RAM entries from the previous epoch must leave with the VGA
    /// entries even though their host pointers did not change.
    pub fn note_direct_data_map_changed(&mut self) {
        self.data_read_pages.invalidate();
        self.data_write_pages.invalidate();
        #[cfg(all(
            feature = "jit",
            target_arch = "x86_64",
            any(target_os = "windows", target_os = "linux")
        ))]
        self.jit_fast_map.invalidate_all();
        self.perf.direct_map_invalidations += 1;
    }

    /// The decode cache's current generation. Advances on every cache invalidation (CS/paging/mode
    /// change, ISA-level change, A20 toggle, self-modifying write). Exposed for observability and so
    /// the machine can verify the A20 seam end-to-end.
    pub fn decode_cache_generation(&self) -> u32 {
        self.decode_cache.generation
    }

    /// The machine calls this only when a device wrote guest RAM but cannot report the physical
    /// range. Known ranges must use `note_device_memory_write_range` so unrelated native blocks and
    /// decode lines survive.
    pub fn note_device_memory_write(&mut self) {
        self.perf.device_write_coarse_resets += 1;
        self.invalidate_code_caches();
    }

    /// Report an exact physical range written by a device or HLE service. Compiled blocks and
    /// decoded lines are invalidated only when their physical spans overlap the write. A prefetched
    /// instruction snapshot is dropped only when the write touches bytes already in that snapshot.
    pub fn note_device_memory_write_range(&mut self, physical: u32, width: u32) {
        if width == 0 {
            return;
        }
        self.perf.device_write_ranges += 1;
        self.perf.device_write_bytes += u64::from(width);
        let prefetch_hit = self.prefetch.overlaps_physical_range(physical, width);
        // G2 out of scope: a device/HLE write range invalidates unconditionally. The device wrote
        // the bytes through its own path, so the CPU has no pre-write old-byte snapshot to compare.
        let code_hit = self.note_code_write(physical, width);
        if prefetch_hit {
            self.prefetch.invalidate();
        }
        if prefetch_hit || code_hit {
            self.perf.device_write_code_hits += 1;
        }
    }

    /// A guest data write of `width` bytes to `physical`. If any written byte was decoded as part of
    /// a cached instruction it is self-modifying code, so advance the decode-cache generation to
    /// re-decode those lines. Byte-exact: a 16-bit stack push just below the code (the flat
    /// tiny-model layout the benchmarks use) writes only its own two bytes, so it never disturbs the
    /// adjacent code.
    pub(super) fn note_code_write(&mut self, physical: u32, width: u32) -> bool {
        self.note_code_write_hit(physical, width)
    }

    /// Cheap, side-effect-free probe hoisted out of `note_code_write_hit`: does the store range
    /// touch any watched code (a compiled block's physical span or a decoded instruction line)?
    /// Value-aware callers (the sized-store path) gate the read-old-bytes comparison that drives
    /// G2 same-value elision on this, paying nothing extra when the store misses all code.
    #[inline]
    pub(super) fn code_write_watched(&self, physical: u32, width: u32) -> bool {
        #[cfg(feature = "jit")]
        if self.jit_direct.range_hits_compiled_code(physical, width) {
            return true;
        }
        self.decode_cache.range_hits_code(physical, width)
    }

    /// The invalidation body of a code write. Only reached once the store is known to have changed
    /// a watched code byte (G2 elision skips it for same-value sized stores) or from a value-less
    /// caller through `note_code_write`. The unit-sim feed lives here, behind the elision choke, so
    /// the diagnostic mirrors the post-elision production invalidation path exactly.
    #[inline]
    pub(super) fn note_code_write_hit(&mut self, physical: u32, width: u32) -> bool {
        // Diagnostic: mirror the guest store into the unit simulator so a write into a simulated
        // unit's page invalidates it, exactly as an SMC store retires the real region. The sim's
        // own map ignores pages it does not own, so this is a cheap no-op off the measured path.
        // The sim takes the whole store range and internally visits the first byte's page and, when
        // the store spans a page boundary, the last byte's page too (a store touches at most two
        // pages here); the two-page visit lives inside the sim so it can classify the store's byte
        // range against unit members (L3 restamp) rather than kill unconditionally.
        #[cfg(feature = "jit")]
        if let Some(sim) = self.unit_sim.0.as_mut() {
            sim.note_code_write(physical, width);
        }
        let mut invalidated = false;
        // G1: heat is incremented only on a byte-precise ACTUAL invalidation (a killed compiled
        // block or a narrow decode kill), never on the coarse global-flush fallback and never when
        // the write hit no code. That precision dissolves the 16-byte false-demotion concern: a
        // data byte sharing a chunk with cold code kills nothing, so it never heats the chunk.
        let mut heat_hit = false;
        #[cfg(feature = "jit")]
        if self.jit_direct.range_hits_compiled_code(physical, width) {
            let killed = self.jit_direct.invalidate_physical_range(physical, width);
            invalidated = killed != 0;
            heat_hit |= killed != 0;
        }
        if self.decode_cache.range_hits_code(physical, width) {
            invalidated = true;
            if self.profile.enabled {
                // Flush-source census (64-byte physical blocks): locates the code/data byte
                // sharing behind a residual SMC flush storm. Off the common path (flushes only).
                *self
                    .profile
                    .smc_flush_blocks
                    .entry(physical & !63)
                    .or_insert(0) += 1;
            }
            // Narrow path: kill only the lines covering the written bytes, when the
            // physical-to-linear reconstruction is unambiguous for EVERY written byte (per-byte
            // decision; a multi-byte write crossing into an aliased/straddled/unknown page falls
            // back for the whole write). The global generation is untouched on the narrow path,
            // so every other cached line survives the self-patch.
            let narrow = (0..width).try_fold(0u32, |acc, i| {
                let byte = physical.wrapping_add(i);
                if !self.decode_cache.is_code_byte(byte) {
                    return Some(acc);
                }
                self.decode_cache.narrow_invalidate(byte).map(|k| acc + k)
            });
            match narrow {
                Some(kills) => {
                    self.perf.smc_narrow_kills += u64::from(kills);
                    if kills > 0 {
                        self.perf.code_invalidations += 1;
                        heat_hit = true;
                    }
                    // A kill inside an installed region's physical span stales its slot table
                    // even though the entry line's stamp may survive; bump the epoch so entry
                    // re-validates through the matcher.
                    #[cfg(feature = "jit")]
                    if kills > 0 && self.jit_regions.covers_physical(physical, width) {
                        self.decode_cache.jit_smc_epoch =
                            self.decode_cache.jit_smc_epoch.wrapping_add(1);
                    }
                }
                None => {
                    self.perf.decode_inval_smc += 1;
                    self.perf.code_invalidations += 1;
                    self.decode_cache.invalidate_and_clear_code_marks();
                    #[cfg(feature = "jit")]
                    self.jit_direct.invalidate_translation();
                }
            }
            // The fetch-page snapshot may hold the written bytes under either outcome.
            self.fetch_page.invalidate();
        }
        #[cfg(feature = "jit")]
        if heat_hit {
            let epoch = self.smc_heat_epoch();
            let newly_hot = self.jit_direct.smc_heat_bump(physical, width, epoch);
            self.perf.smc_heat_chunks_hot += u64::from(newly_hot);
        }
        #[cfg(not(feature = "jit"))]
        let _ = heat_hit;
        invalidated
    }

    /// G1 heat epoch: the retired-instruction megacount. Both the invalidation choke (which bumps
    /// heat) and the admission gate (which reads it) derive the epoch from this one clock, so a
    /// chunk's churn count is only live within the ~1M-instruction window it accrued in.
    /// Corner: `reset_perf_counters` restarts the instruction count, so epoch numbers repeat and a
    /// stale epoch-0 stamp can briefly read as current again (one epoch of over-conservative
    /// demotion at worst; correctness is unaffected, demotion only routes to the interpreter).
    #[cfg(feature = "jit")]
    #[inline]
    pub(super) fn smc_heat_epoch(&self) -> u32 {
        (self.perf.instructions >> jit::direct::SMC_HEAT_EPOCH_SHIFT) as u32
    }

    pub(super) fn begin_instruction(&mut self) {
        // A 486 prefetch queue is a snapshot: writes to already fetched bytes are
        // not observed until control flow or the next refill invalidates the queue.
        if self.written_pages_overflow {
            self.prefetch.invalidate();
        } else if self.written_count > 0 {
            if let Some(code_page) = self.prefetch.physical_page() {
                if self.written_pages.contains(&Some(code_page)) {
                    self.prefetch.invalidate();
                }
            }
        }
        // Clear only the slots that were actually written this instruction (most
        // instructions are register-only and written_count is 0, so this is a no-op
        // instead of an unconditional 64-byte memset).
        for i in 0..self.written_count as usize {
            self.written_pages[i] = None;
        }
        self.written_count = 0;
        self.written_pages_overflow = false;
    }

    pub fn is_protected_mode(&self) -> bool {
        self.control.cr0 & CR0_PE != 0
    }

    /// Whether implicit stack references (PUSH/POP/CALL/RET/interrupts) use the
    /// full 32-bit ESP or wrap within the 16-bit SP, per the loaded SS descriptor's
    /// B bit (386 PRM 16.2: "the size of stack pointer ... used by the processor
    /// for implicit stack references" is controlled by SS.B, independent of
    /// operand size, gate size, or CS's D bit). Real mode and V86 always load SS
    /// through `load_segment_real`/`SegmentRegister::real`, which sets
    /// `default_size_32: false`, so this is 16-bit there too -- one rule covers
    /// every mode. Backed by the cached `SegmentRegister.default_size_32` field
    /// (populated from descriptor bit 22 in `descriptor_to_segment`), so this is a
    /// field read, not a descriptor decode: safe in the push/pop hot path.
    #[inline]
    pub(super) fn stack_is_32bit(&self) -> bool {
        self.registers.segment(SegmentIndex::Ss).default_size_32
    }

    /// True while executing ring-0 protected-mode code that is not a V86 task —
    /// i.e., inside a V86 monitor (TOKAEMM). The machine defers deferred HLE
    /// interrupt servicing (`handle_int10`/`handle_int13`/…, which assume a
    /// real-mode INT frame at SS:SP) while this holds, so the HLE runs only once
    /// the monitor has reflected the INT back into the V86 guest.
    ///
    /// ASSUMPTION: today "ring-0 PM" is *only* the TOKAEMM monitor — every stock
    /// HLE-served BIOS INT (10h–1Ah) is issued from real mode, so this reads false
    /// there. A future protected-mode guest that legitimately issued an HLE INT at
    /// CPL 0 would have it deferred until it next left ring-0 PM; revisit this gate
    /// when PM DOS-extender / DPMI support lands.
    pub fn is_ring0_protected(&self) -> bool {
        self.is_protected_mode() && !self.is_v86_mode() && self.current_privilege_level() == 0
    }

    /// The CPU mode/size bitmask a compiled JIT block is keyed by (spec §2.2): a block compiled
    /// for one mode must never be reused in another at the same phys/d. Packs the CS operand-size
    /// default (D bit), protected mode (CR0.PE), V86, the SS stack big bit (B), and the GSW mode.
    /// A mode change already invalidates the decode cache (`set_mode`), but it is folded in here
    /// too so the key is self-contained. Validated at every region entry (`run_region`).
    #[cfg(feature = "jit")]
    pub(super) fn jit_mode_key(&self) -> u32 {
        let mut key = 0u32;
        if self.registers.cs().default_size_32 {
            key |= 1 << 0;
        }
        if self.control.cr0 & CR0_PE != 0 {
            key |= 1 << 1;
        }
        if self.is_v86_mode() {
            key |= 1 << 2;
        }
        if self.registers.segment(SegmentIndex::Ss).default_size_32 {
            key |= 1 << 3;
        }
        // The native memory probe emits a TLB translation only for a paged block. A paging change
        // therefore needs a mode-key miss and a fresh emission.
        if self.control.cr0 & CR0_PG != 0 {
            key |= 1 << 4;
        }
        key | (u32::from(self.mode.rank()) << 8)
    }

    /// Whether `seg` is FLAT: base 0 and limit `u32::MAX`. This is exactly the interpreter's
    /// `segment_linear_byte` fast path (base 0 + limit max → return the offset as the linear address),
    /// so under it EA == linear and no segment-limit fault is ever skipped (with limit max no offset can
    /// exceed it, and the flat fast path returns before the expand-down logic). The native memory
    /// probe needs this for DS because it computes the linear offset directly. DS is not part of the
    /// mode key, so every region entry rechecks it.
    #[cfg(feature = "jit")]
    pub(super) fn jit_segment_flat(&self, seg: SegmentIndex) -> bool {
        let d = self.registers.segment(seg);
        d.base == 0 && d.limit == u32::MAX
    }

    /// Whether a data read through `seg` is permitted. This deliberately uses the interpreter's
    /// descriptor check so execute-only code loaded into DS cannot enter a native memory region.
    #[cfg(feature = "jit")]
    pub(super) fn jit_segment_readable(&self, seg: SegmentIndex) -> bool {
        let access = self.registers.segment(seg).access;
        self.check_segment_access_kind(seg, access, false).is_ok()
    }

    /// Whether a data write through `seg` is permitted. A direct-page cache hit proves that the
    /// physical page is writable, but it says nothing about the current segment descriptor.
    #[cfg(feature = "jit")]
    pub(super) fn jit_segment_writable(&self, seg: SegmentIndex) -> bool {
        let access = self.registers.segment(seg).access;
        self.check_segment_access_kind(seg, access, true).is_ok()
    }

    /// The active GSW compatibility mode.
    pub fn mode(&self) -> GswMode {
        self.mode
    }

    /// The guest-facing ISA persona selected by the active mode.
    pub fn persona(&self) -> CpuPersona {
        self.mode.persona()
    }

    /// Compatibility readout for callers that still use the old name.
    pub fn level(&self) -> CpuPersona {
        self.persona()
    }

    /// Install one core-table mode atomically. Every call resets fractional timing
    /// state and execution accelerators, including switches between the two modes
    /// that share the 386 persona.
    pub fn set_mode(&mut self, mode: GswMode) {
        self.mode = mode;
        self.timing_rem = 0;
        self.fp_rem = 0;
        self.rep_resume_active = false;
        *self.rep_execution = RepExecution::default();
        #[cfg(feature = "jit")]
        self.jit_regions.clear();
        self.invalidate_code_caches();
    }

    /// Advance the architectural TSC for machine time not represented by
    /// retired-instruction clocks. This leaves instruction timing unchanged
    /// and preserves a guest WRMSR rebase.
    pub fn advance_tsc(&mut self, clocks: u64) {
        self.msr.tsc_offset = self.msr.tsc_offset.wrapping_add(clocks);
    }

    /// Scale a retired instruction's clocks by the active level's timing factor,
    /// carrying the fractional remainder so a run of cheap ops is not rounded to
    /// zero. This is the single per-mode timing dial; it feeds both the CPU's
    /// own clock counter and, through the returned CycleOutcome, the machine's
    /// device timing.
    ///
    /// May return 0 for a cheap op in a faster mode; that is safe because the
    /// remainder carry guarantees a clock tick within a few instructions and the
    /// machine's batch loop advances on instruction progress, not on clocks alone.
    pub(super) fn scale_clocks(&mut self, clocks: u32) -> u64 {
        // Specialized per-level so the compiler sees the denominator as a compile-time
        // constant and strength-reduces the divide to a magic-multiplier multiply-shift
        // (~4 cycles of imul) instead of a hardware div (~20-40 cycles). The generic form
        // `scaled / u64::from(den)` with a runtime `den` from the level_timing match
        // emitted a real div instruction on every call; this specialization avoids it.
        // The remainder carry (`timing_rem`) is shared across all levels (reset on level
        // change), so switching levels mid-run is safe (the carry is < the old den and
        // the new den is always >= 5, so the first scaled result is exact).
        let persona = self.persona();
        let (num, den) = level_timing(persona);
        let scaled = u64::from(clocks) * u64::from(num) + self.timing_rem;
        // The `match` on the persona gives the compiler a compile-time constant for `den`
        // in each arm, enabling magic-multiplier strength reduction. The generic
        // `scaled % den` + `scaled / den` would be two divs; computing the quotient first
        // then the remainder as `scaled - quot * den` is one div + one mul + one sub.
        let (quot, rem) = match persona {
            CpuPersona::I386 => {
                let q = scaled / 5u64;
                (q, scaled - q * 5)
            }
            CpuPersona::I486 | CpuPersona::I586 => {
                let q = scaled / 12u64;
                (q, scaled - q * 12)
            }
        };
        let _ = den; // den is unused; the match arms carry the constant directly
        self.timing_rem = rem;
        quot
    }

    /// `scale_clocks` for a whole compiled-region run in one call: same exact long division,
    /// u64 raw input because a region batch can exceed u32. Equal to summing per-instruction
    /// `scale_clocks` results over the same charges (the remainder-carry identity pinned by
    /// `scale_clocks_batches_exactly`).
    #[cfg(feature = "jit")]
    pub(super) fn scale_clocks_batch(&mut self, clocks: u64) -> u64 {
        let (num, den) = level_timing(self.persona());
        let scaled = clocks * u64::from(num) + self.timing_rem;
        self.timing_rem = scaled % u64::from(den);
        scaled / u64::from(den)
    }

    /// Scale an x87 op's raw core clocks by the active level's FP-timing factor for the
    /// op's class, carrying the fractional remainder in `fp_rem` so a cheap FP op is not
    /// rounded to zero. Mirrors `scale_clocks` but uses `fp_timing_class` and `fp_rem`;
    /// every class ratio shares the same denominator (FP_TIMING_DEN), so the carried
    /// remainder stays exact across ops of different classes. With an identity factor
    /// this returns `clocks` unchanged.
    pub(super) fn scale_fp_clocks(&mut self, clocks: u32, class: FpOpClass) -> u32 {
        let num = fp_timing_class(self.persona(), class);
        let scaled = u64::from(clocks) * u64::from(num) + self.fp_rem;
        self.fp_rem = scaled % u64::from(FP_TIMING_DEN);
        (scaled / u64::from(FP_TIMING_DEN)).min(u64::from(u32::MAX)) as u32
    }

    /// Reported (L1 KB, L2 KB) cache for the live mode. The same geometry drives
    /// per-mode data-access timing through the machine's `CacheModel`, so this is no
    /// longer a no-timing readout.
    pub fn cache_kb(&self) -> (u16, u16) {
        self.mode.cache_kb()
    }

    /// Host-side performance counters accumulated since construction or the last
    /// `reset_perf_counters`. Diagnostics for `--headless-bench`; not architectural state.
    /// The memory-poll skip counter subset (stored outside `PerfCounters`;
    /// see `PollSkipMemoryCounters` for why). Reset alongside the other
    /// counters by `reset_perf_counters`.
    #[inline(never)]
    pub fn poll_skip_memory(&self) -> PollSkipMemoryCounters {
        self.poll_skip_memory
    }

    pub fn perf_counters(&self) -> &PerfCounters {
        &self.perf
    }

    /// Zero the host-side performance counters, including the memory-poll
    /// subset stored outside `PerfCounters` (see `PollSkipMemoryCounters`).
    pub fn reset_perf_counters(&mut self) {
        self.perf = PerfCounters::default();
        self.poll_skip_memory = PollSkipMemoryCounters::default();
    }

    #[cfg(feature = "jit")]
    fn poll_skip_core_projection(&self, poll: PollLoop, iterations: u64) -> Option<(u64, u64)> {
        let raw = poll.raw_core_clocks().checked_mul(iterations)?;
        let (num, den) = level_timing(self.persona());
        let scaled = raw
            .checked_mul(u64::from(num))?
            .checked_add(self.timing_rem)?;
        Some((scaled / u64::from(den), scaled % u64::from(den)))
    }

    /// Exact non-mutating core-clock projection for complete poll-loop iterations.
    #[cfg(feature = "jit")]
    pub fn project_poll_skip_core(&self, poll: PollLoop, iterations: u64) -> Option<u64> {
        let (charged, _) = self.poll_skip_core_projection(poll, iterations)?;
        self.elapsed_clocks.checked_add(charged)?;
        Some(charged)
    }

    /// Fractional core-timing carry, exposed with the poll-skip diagnostics so
    /// machine boundary tests and measurement reports can verify exact scaling.
    #[cfg(feature = "jit")]
    pub fn poll_skip_timing_remainder(&self) -> u64 {
        self.timing_rem
    }

    /// Commit complete poll-loop iterations through the same remainder-carry scaler
    /// used by normal execution. Retired-instruction and unit-simulator counts stay
    /// unchanged because these instructions did not execute.
    #[cfg(feature = "jit")]
    pub fn commit_poll_skip_core(&mut self, poll: PollLoop, iterations: u64) -> Option<u64> {
        let (charged, remainder) = self.poll_skip_core_projection(poll, iterations)?;
        self.elapsed_clocks = self.elapsed_clocks.checked_add(charged)?;
        self.timing_rem = remainder;
        self.perf.poll_skip_spans = self.perf.poll_skip_spans.saturating_add(1);
        self.perf.poll_skip_iterations = self.perf.poll_skip_iterations.saturating_add(iterations);
        if poll.family() == crate::PollFamily::Memory {
            self.poll_skip_memory.spans = self.poll_skip_memory.spans.saturating_add(1);
            self.poll_skip_memory.iterations =
                self.poll_skip_memory.iterations.saturating_add(iterations);
        }
        Some(charged)
    }

    /// TLB-hit-only, non-mutating linear-to-physical PROBE for a data READ
    /// (the poll-skip memory certification seam, R2). Unpaged mode returns the
    /// linear identity. Paged mode consults ONLY the cached TLB entry's
    /// read-protection bit (never the dirty bit: this is a read, never a
    /// write); DECLINES (`None`) on a TLB miss or a would-fault protection
    /// mismatch. Deliberately does NOT call `translate_linear`/
    /// `translate_linear_checked`: the full walk sets CR2 on a fault, issues
    /// charged PageWalkRead bus reads, and can write the PTE accessed bit into
    /// guest RAM on a fill, none of which a pure certification probe may do.
    /// A decline costs nothing beyond one ordinary interpreted iteration
    /// before the next batch's classifier retries: `try_poll_skip` requires
    /// `at_head`, so the previous iteration's CMP just executed and warmed
    /// this exact page's TLB entry.
    #[cfg(feature = "jit")]
    #[cold]
    #[inline(never)]
    pub fn probe_linear_read_physical(&self, linear: u32) -> Option<u32> {
        if !self.is_paging_enabled() {
            return Some(linear);
        }
        let page = linear >> 12;
        let entry = self.tlb.lookup(page)?;
        let user = self.current_privilege_level() == 3;
        if user && !entry.user {
            return None;
        }
        Some(entry.phys | (linear & 0x0000_0fff))
    }

    /// Apply the non-architectural housekeeping a taken backedge performs while
    /// leaving the architectural loop-head EIP unchanged.
    #[cfg(feature = "jit")]
    pub fn poll_skip_backedge_housekeeping(&mut self) {
        self.set_eip(self.registers.eip);
    }

    /// Enable host-side CPU bucket profiling. Guest-visible state and timing are unchanged.
    pub fn enable_profiling(&mut self, sample_stride: u64) {
        self.profile.enable(sample_stride);
    }

    /// Disable and clear host-side CPU bucket profiling.
    pub fn disable_profiling(&mut self) {
        self.profile.disable();
    }

    pub fn profile_snapshot(&self) -> CpuProfileSnapshot {
        self.profile.snapshot()
    }
    pub fn is_paging_enabled(&self) -> bool {
        self.control.cr0 & CR0_PG != 0
    }

    /// Virtual-8086 mode: the VM flag set while in protected mode. A V86 task runs
    /// 8086 code with real-mode segment addressing but always at CPL 3. Public for
    /// the machine's `in_v86()` so default-boot tests can assert that the guest
    /// runs virtualized. This is the visibility counterpart of `is_ring0_protected`.
    pub fn is_v86_mode(&self) -> bool {
        self.is_protected_mode() && self.registers.eflags & FLAG_VM != 0
    }

    /// The I/O privilege level (EFLAGS bits 12-13).
    pub(super) fn iopl(&self) -> u8 {
        ((self.registers.eflags >> 12) & 3) as u8
    }

    /// IOPL-sensitive instructions (CLI, STI, PUSHF/POPF, INT n) fault to the monitor
    /// inside a V86 task when IOPL is below 3.
    pub(super) fn check_v86_iopl(&self) -> ExecResult<()> {
        if self.is_v86_mode() && self.iopl() < 3 {
            return Err(InternalFault::Exception {
                vector: 13,
                error_code: Some(0),
            });
        }
        Ok(())
    }

    /// Current privilege level. Per the 386 PRM this is a *cached* quantity (`self.cpl`),
    /// not a live read of `CS.selector & 3`: during exception delivery out of a V86 source,
    /// the ring-0 stack pushes happen before the handler's own CS is loaded, so a live
    /// formula would derive "user" from whatever RPL bits the *source* V86 CS happened to
    /// carry (arbitrary, since V86 CS is a real-mode-style segment) instead of the level
    /// the pushes actually execute at. See `deliver_exception` for the write-before-push
    /// ordering that makes this correct.
    ///
    /// The debug assert cross-checks the cache against the historical live formula in the
    /// two cases where they must always agree (real mode and V86, both fixed points); it is
    /// intentionally NOT checked in protected non-V86 mode, where mid-instruction sequences
    /// (like `deliver_exception`'s push sequence) transiently hold `self.cpl` at the target
    /// level while `CS.selector` still names the source segment.
    pub(super) fn current_privilege_level(&self) -> u8 {
        if !self.is_protected_mode() {
            debug_assert_eq!(self.cpl, 0, "real mode is always CPL 0");
        } else if self.is_v86_mode() {
            debug_assert_eq!(self.cpl, 3, "a V86 task is always CPL 3");
        }
        self.cpl
    }

    pub fn linear_eip(&self) -> u32 {
        self.registers.cs().base.wrapping_add(self.registers.eip)
    }

    pub fn read_reg16(&self, reg: Reg16) -> u16 {
        self.read_gpr16(reg.index() as u8)
    }

    pub fn write_reg16(&mut self, reg: Reg16, value: u16) {
        self.write_gpr16(reg.index() as u8, value);
    }
    /// Recompute the cached `alignment_armed` bit (`CR0.AM && EFLAGS.AC`). Called at every
    /// writer that can change either bit:
    /// - CR0.AM: `MOV CR0, reg` and (defensively) LMSW. LMSW only loads MP/EM/TS/PE,
    ///   CLTS and the task-switch `CR0.TS |=` only touch TS, so none of those can flip
    ///   AM, but the two explicit CR0 image writers both recompute for uniformity.
    /// - EFLAGS.AC: `load_flags` (POPF/POPFD and every IRET form route through it), the
    ///   task-switch EFLAGS load, and `set_flag_live` when the mask includes AC (the
    ///   check const-folds away at the arithmetic-flag call sites). SAHF, the lazy-flag
    ///   materialization, and the VM/NT/AF read-modify-writes never reach bit 18.
    ///
    /// `registers`/`control` are pub, so a direct field poke bypasses this; the only such
    /// non-test writer in the tree (`boot_sector_cpu`) pokes a fresh `CpuGsw::default()`
    /// whose reset image has both bits clear, matching the default `false`.
    pub(super) fn recompute_alignment_armed(&mut self) {
        self.alignment_armed =
            self.control.cr0 & CR0_AM != 0 && self.registers.eflags & FLAG_AC != 0;
    }
    pub(super) fn segment_from_reg_field(&self, reg: u8) -> SegmentRegister {
        match reg {
            0 => self.registers.segment(SegmentIndex::Es),
            1 => self.registers.segment(SegmentIndex::Cs),
            2 => self.registers.segment(SegmentIndex::Ss),
            3 => self.registers.segment(SegmentIndex::Ds),
            4 => self.registers.segment(SegmentIndex::Fs),
            _ => self.registers.segment(SegmentIndex::Gs),
        }
    }

    pub(super) fn read_gpr32(&self, index: u8) -> u32 {
        self.registers.gpr[usize::from(index & 7)]
    }

    pub(super) fn write_gpr32(&mut self, index: u8, value: u32) {
        self.registers.gpr[usize::from(index & 7)] = value;
    }

    /// The EDX:EAX register pair as one 64-bit value (EDX is the high dword). Used by the
    /// 64-bit MSR and time-stamp instructions.
    pub(super) fn read_edx_eax(&self) -> u64 {
        (u64::from(self.read_gpr32(2)) << 32) | u64::from(self.read_gpr32(0))
    }

    /// Split a 64-bit value into EDX:EAX (EDX high, EAX low).
    pub(super) fn set_edx_eax(&mut self, value: u64) {
        self.write_gpr32(0, value as u32);
        self.write_gpr32(2, (value >> 32) as u32);
    }

    /// The time-stamp counter: retired-instruction clocks plus machine-time and
    /// guest WRMSR adjustments.
    pub(super) fn time_stamp_counter(&self) -> u64 {
        self.elapsed_clocks.wrapping_add(self.msr.tsc_offset)
    }

    pub(super) fn read_gpr16(&self, index: u8) -> u16 {
        self.registers.gpr[usize::from(index & 7)] as u16
    }

    pub(super) fn write_gpr16(&mut self, index: u8, value: u16) {
        let slot = &mut self.registers.gpr[usize::from(index & 7)];
        *slot = (*slot & 0xffff_0000) | u32::from(value);
    }

    pub(super) fn read_gpr8(&self, index: u8) -> u8 {
        let reg = usize::from(index & 3);
        if index < 4 {
            self.registers.gpr[reg] as u8
        } else {
            (self.registers.gpr[reg] >> 8) as u8
        }
    }

    pub(super) fn write_gpr8(&mut self, index: u8, value: u8) {
        let reg = usize::from(index & 3);
        if index < 4 {
            self.registers.gpr[reg] = (self.registers.gpr[reg] & !0xff) | u32::from(value);
        } else {
            self.registers.gpr[reg] = (self.registers.gpr[reg] & !0xff00) | (u32::from(value) << 8);
        }
    }

    pub(super) fn read_gpr_sized(&self, index: u8, operand_size: OperandSize) -> u32 {
        match operand_size {
            OperandSize::Word => u32::from(self.read_gpr16(index)),
            OperandSize::Dword => self.read_gpr32(index),
        }
    }

    pub(super) fn write_gpr_sized(&mut self, index: u8, operand_size: OperandSize, value: u32) {
        match operand_size {
            OperandSize::Word => self.write_gpr16(index, value as u16),
            OperandSize::Dword => self.write_gpr32(index, value),
        }
    }
    /// True when the interrupt flag is set, so a maskable interrupt can be taken.
    pub fn interrupts_enabled(&self) -> bool {
        self.flag(FLAG_IF)
    }

    /// True when a maskable interrupt could be serviced at this instruction
    /// boundary: IF is set AND the STI one-instruction shadow is not pending. The
    /// machine batch loop watches this transition so it can end a batch whenever an
    /// instruction makes an interrupt newly serviceable (POPF/IRET enabling IF, or
    /// the instruction after STI consuming the shadow). That keeps the per-batch
    /// interrupt check equivalent to the old per-instruction one without re-querying
    /// the PIC inside the batch.
    pub fn can_take_interrupt(&self) -> bool {
        self.flag(FLAG_IF) && !self.interrupt_shadow
    }

    pub(super) fn flag(&self, flag: u32) -> bool {
        const ARITH: u32 = FLAG_CF | FLAG_PF | FLAG_AF | FLAG_ZF | FLAG_SF | FLAG_OF;
        // Route only a single, purely-arithmetic flag bit through the pending descriptor; a control
        // bit or multi-bit query falls through to the live eflags.
        if !self.pending_flags.is_none() && (flag & ARITH) != 0 && (flag & !ARITH) == 0 {
            return self.arith_flag(flag);
        }
        self.registers.eflags & flag != 0
    }

    /// True iff `flag` (one of the six arithmetic bits) is set, computed from `pending_flags` when present
    /// so reads never force materialization. For any non-arithmetic (control) flag, or no pending, reads
    /// `registers.eflags` directly.
    fn arith_flag(&self, flag: u32) -> bool {
        if self.pending_flags.is_none() {
            return self.registers.eflags & flag != 0;
        }
        let p = &self.pending_flags;
        let mask = width_mask(p.width());
        let sign = width_sign(p.width());
        match flag {
            FLAG_ZF => p.result & mask == 0,
            FLAG_SF => p.result & sign != 0,
            FLAG_PF => parity(p.result as u8),
            FLAG_AF if p.op() == LazyFlagOp::Logic => self.registers.eflags & FLAG_AF != 0,
            FLAG_AF => ((p.a ^ p.b ^ p.result) & 0x10) != 0,
            FLAG_CF => {
                if let Some(cf) = p.cf_override() {
                    cf
                } else if p.op() == LazyFlagOp::Logic {
                    false
                } else if p.op() == LazyFlagOp::Sub {
                    u64::from(p.a) < u64::from(p.b)
                } else {
                    u64::from(p.a) + u64::from(p.b) > u64::from(mask)
                }
            }
            FLAG_OF if p.op() == LazyFlagOp::Logic => false,
            FLAG_OF if p.op() == LazyFlagOp::Sub => ((p.a ^ p.b) & (p.a ^ p.result) & sign) != 0,
            FLAG_OF => ((p.a ^ p.result) & (p.b ^ p.result) & sign) != 0,
            _ => self.registers.eflags & flag != 0, // unreachable under flag()'s guard; defensive
        }
    }

    #[inline]
    fn set_flag_live(&mut self, flag: u32, enabled: bool) {
        if enabled {
            self.registers.eflags |= flag;
        } else {
            self.registers.eflags &= !flag;
        }
        self.registers.eflags |= 0x2;
        // `flag` is a compile-time-constant mask at every call site; expected to fold once
        // set_flag inlines (set_flag_live is #[inline]). Measured: FORCING set_flag inline
        // cost 1-3% wall via code bloat, so do not re-add #[inline] there for this check.
        if flag & FLAG_AC != 0 {
            self.recompute_alignment_armed();
        }
    }

    pub(super) fn set_flag(&mut self, flag: u32, enabled: bool) {
        const ARITH: u32 = FLAG_CF | FLAG_PF | FLAG_AF | FLAG_ZF | FLAG_SF | FLAG_OF;
        if !self.pending_flags.is_none() {
            let arith = flag & ARITH;
            if flag == FLAG_CF {
                if !self.pending_flags.is_none() {
                    let p = self.pending_flags; // copy
                    self.pending_flags = p.with_cf_override(enabled);
                }
                self.registers.eflags |= 0x2;
                return;
            }
            if arith != 0 {
                self.materialize_flags();
            }
        }
        self.set_flag_live(flag, enabled);
    }

    pub(super) fn alu(&mut self, op: u8, a: u32, b: u32, width: BusWidth) -> u32 {
        let mask = width_mask(width);
        let cf_in = u32::from(self.flag(FLAG_CF));
        match op {
            0 => self.alu_add(a, b, 0, width),
            2 => self.alu_add(a, b, cf_in, width),
            3 => self.alu_sub(a, b, cf_in, width),
            5 | 7 => self.alu_sub(a, b, 0, width),
            1 => {
                let result = (a | b) & mask;
                self.alu_logic(result, width)
            }
            4 => {
                let result = (a & b) & mask;
                self.alu_logic(result, width)
            }
            6 => {
                let result = (a ^ b) & mask;
                self.alu_logic(result, width)
            }
            _ => unreachable!("alu op {op}"),
        }
    }

    pub(super) fn double_shift(
        &mut self,
        left: bool,
        dest: u32,
        src: u32,
        raw_count: u8,
        operand_size: OperandSize,
    ) -> u32 {
        // The 386 masks the count to 5 bits. A count of 0 is a no-op that touches no flags.
        let count = u32::from(raw_count) & 0x1f;
        if count == 0 {
            return dest;
        }
        let bits = operand_size.bytes() * 8;
        let mask = operand_size.mask();
        let msb = 1u32 << (bits - 1);
        let mut d = dest & mask;
        let mut s = src & mask;
        // A masked count past the operand width is undefined per Intel, but the 386
        // leaves the destination as the source rotated by the count modulo the width:
        // SHLD rotates left, SHRD rotates right. A 5-bit count never exceeds a 32-bit
        // width, so this only applies to the 16-bit forms. The flags are undefined here
        // and the conformance harness masks them. Derived from the SingleStepTests
        // vectors.
        if count > bits {
            let n = count % bits;
            let result = if left {
                ((s << n) | (s >> (bits - n))) & mask
            } else {
                ((s >> n) | (s << (bits - n))) & mask
            };
            self.set_shift_result_flags(None, None, result, operand_size.bus_width());
            return result;
        }
        // A nonzero count always overwrites cf on the first iteration; unlike the
        // rotate-through-carry shifts there is no carry-in to seed.
        let mut cf = false;
        for _ in 0..count {
            if left {
                // SHLD: dest shifts left, the vacated low bit takes src's high bit.
                cf = d & msb != 0;
                d = ((d << 1) | ((s & msb) >> (bits - 1))) & mask;
                s = (s << 1) & mask;
            } else {
                // SHRD: dest shifts right, the vacated high bit takes src's low bit.
                cf = d & 1 != 0;
                d = (d >> 1) | ((s & 1) << (bits - 1));
                s >>= 1;
            }
        }
        let of = if count == 1 {
            // OF is defined only for a single-bit count: set when the sign bit changed.
            Some((dest ^ d) & msb != 0)
        } else {
            None
        };
        self.set_shift_result_flags(Some(cf), of, d, operand_size.bus_width());
        d & mask
    }

    pub(super) fn shift_rotate(
        &mut self,
        op: u8,
        value: u32,
        raw_count: u8,
        width: BusWidth,
    ) -> u32 {
        // The 386 masks the count to 5 bits, then performs that many
        // single-bit steps. A single-bit loop (<=31 iterations) matches silicon
        // step for step and avoids every closed-form edge case (a `>> bits` shift
        // at a full rotation, the RCL/RCR rotate-through-carry modulus). Switch to
        // a closed form only if this ever shows up on a profile.
        let count = u32::from(raw_count) & 0x1f;
        if count == 0 {
            return value; // a zero count affects no flags at all
        }
        let mask = width_mask(width);
        let msb = width_sign(width);
        let bits = width.bytes() * 8;
        let mut v = value & mask;
        let mut cf = self.flag(FLAG_CF); // seed for RCL/RCR
        for _ in 0..count {
            match op {
                0 => {
                    // ROL
                    let bit = (v & msb) != 0;
                    v = ((v << 1) | u32::from(bit)) & mask;
                    cf = bit;
                }
                1 => {
                    // ROR
                    let bit = (v & 1) != 0;
                    v = (v >> 1) | (u32::from(bit) << (bits - 1));
                    cf = bit;
                }
                2 => {
                    // RCL (rotate left through carry)
                    let bit = (v & msb) != 0;
                    v = ((v << 1) | u32::from(cf)) & mask;
                    cf = bit;
                }
                3 => {
                    // RCR (rotate right through carry)
                    let bit = (v & 1) != 0;
                    v = (v >> 1) | (u32::from(cf) << (bits - 1));
                    cf = bit;
                }
                4 | 6 => {
                    // SHL (/6 aliases SHL)
                    cf = (v & msb) != 0;
                    v = (v << 1) & mask;
                }
                5 => {
                    // SHR (logical)
                    cf = (v & 1) != 0;
                    v >>= 1;
                }
                7 => {
                    // SAR (arithmetic, sign preserved)
                    cf = (v & 1) != 0;
                    v = (v >> 1) | (v & msb);
                }
                _ => unreachable!("shift/rotate op {op}"),
            }
        }
        if matches!(op, 4..=7) {
            let of = if count == 1 {
                // OF is defined only for a single-bit count.
                let top = (v & msb) != 0;
                Some(match op {
                    4 | 6 => top ^ cf,       // SHL: top bit of result XOR carry out
                    5 => (value & msb) != 0, // SHR: most-significant bit of the original
                    7 => false,              // SAR never overflows
                    _ => unreachable!("shift op {op}"),
                })
            } else {
                None
            };
            self.set_shift_result_flags(Some(cf), of, v, width);
        } else {
            self.set_flag(FLAG_CF, cf);
            if count == 1 {
                // OF is defined only for a single-bit count.
                let top = (v & msb) != 0;
                let of = match op {
                    0 | 2 => top ^ cf,                      // ROL, RCL: top bit XOR carry out
                    1 | 3 => top ^ ((v & (msb >> 1)) != 0), // ROR, RCR: top two bits XORed
                    _ => unreachable!("rotate op {op}"),
                };
                self.set_flag(FLAG_OF, of);
            }
        }
        v & mask
    }

    pub(super) fn inc_dec(&mut self, value: u32, is_dec: bool, width: BusWidth) -> u32 {
        // INC/DEC affect OF/SF/ZF/AF/PF exactly like ADD/SUB by 1, but leave CF.
        let carry = self.flag(FLAG_CF);
        let mask = width_mask(width);
        let value = value & mask;
        let result = if is_dec {
            value.wrapping_sub(1) & mask
        } else {
            value.wrapping_add(1) & mask
        };
        let lf = LazyFlags {
            a: value,
            b: 1,
            result,
            width,
            op: if is_dec {
                LazyFlagOp::Sub
            } else {
                LazyFlagOp::Add
            },
            cf_override: Some(carry),
        };
        self.pending_flags = PendingFlags::from_legacy(&lf);
        result
    }

    pub(super) fn alu_add_eager(&mut self, a: u32, b: u32, carry: u32, width: BusWidth) -> u32 {
        let mask = width_mask(width);
        let sign = width_sign(width);
        let a = a & mask;
        let b = b & mask;
        let full = u64::from(a) + u64::from(b) + u64::from(carry);
        let result = (full as u32) & mask;
        self.set_flag(FLAG_CF, full > u64::from(mask));
        self.set_flag(FLAG_OF, ((a ^ result) & (b ^ result) & sign) != 0);
        self.set_flag(FLAG_AF, ((a ^ b ^ result) & 0x10) != 0);
        self.set_szp(result, width);
        result
    }

    pub(super) fn alu_add(&mut self, a: u32, b: u32, carry: u32, width: BusWidth) -> u32 {
        if carry != 0 {
            return self.alu_add_eager(a, b, carry, width);
        }
        let mask = width_mask(width);
        let a = a & mask;
        let b = b & mask;
        let result = ((u64::from(a) + u64::from(b)) as u32) & mask;
        let lf = LazyFlags {
            a,
            b,
            result,
            width,
            op: LazyFlagOp::Add,
            cf_override: None,
        };
        self.pending_flags = PendingFlags::from_legacy(&lf);
        result
    }

    pub(super) fn alu_sub_eager(&mut self, a: u32, b: u32, borrow: u32, width: BusWidth) -> u32 {
        let mask = width_mask(width);
        let sign = width_sign(width);
        let a = a & mask;
        let b = b & mask;
        let rhs = u64::from(b) + u64::from(borrow);
        let result = (u64::from(a).wrapping_sub(rhs) as u32) & mask;
        self.set_flag(FLAG_CF, u64::from(a) < rhs);
        self.set_flag(FLAG_OF, ((a ^ b) & (a ^ result) & sign) != 0);
        self.set_flag(FLAG_AF, ((a ^ b ^ result) & 0x10) != 0);
        self.set_szp(result, width);
        result
    }

    pub(super) fn alu_sub(&mut self, a: u32, b: u32, borrow: u32, width: BusWidth) -> u32 {
        if borrow != 0 {
            return self.alu_sub_eager(a, b, borrow, width);
        }
        let mask = width_mask(width);
        let a = a & mask;
        let b = b & mask;
        let result = (u64::from(a).wrapping_sub(u64::from(b)) as u32) & mask;
        let lf = LazyFlags {
            a,
            b,
            result,
            width,
            op: LazyFlagOp::Sub,
            cf_override: None,
        };
        self.pending_flags = PendingFlags::from_legacy(&lf);
        result
    }

    pub(super) fn alu_logic(&mut self, result: u32, width: BusWidth) -> u32 {
        let result = result & width_mask(width);
        let af = self.flag(FLAG_AF);
        if af {
            self.registers.eflags |= FLAG_AF;
        } else {
            self.registers.eflags &= !FLAG_AF;
        }
        self.registers.eflags |= 0x2;
        let lf = LazyFlags {
            a: 0,
            b: 0,
            result,
            width,
            op: LazyFlagOp::Logic,
            cf_override: None,
        };
        self.pending_flags = PendingFlags::from_legacy(&lf);
        result
    }

    pub(super) fn mul(&mut self, operand: u32, signed: bool, width: BusWidth) {
        // Multiply the implicit accumulator (AL/AX/EAX) by the operand and store the
        // wide product split across AH:AL / DX:AX / EDX:EAX. CF and OF are set when the
        // high half is significant (unsigned: nonzero; signed: not the sign extension
        // of the low half); SF/ZF/AF/PF are left untouched (undefined on the 386).
        let significant = match (width, signed) {
            (BusWidth::Byte, false) => {
                let product = u16::from(self.read_gpr8(0)) * u16::from(operand as u8);
                self.write_gpr16(0, product);
                product & 0xff00 != 0
            }
            (BusWidth::Byte, true) => {
                let product = i16::from(self.read_gpr8(0) as i8) * i16::from(operand as u8 as i8);
                self.write_gpr16(0, product as u16);
                product != i16::from(product as u8 as i8)
            }
            (BusWidth::Word, false) => {
                let product = u32::from(self.read_gpr16(0)) * u32::from(operand as u16);
                self.write_gpr16(0, product as u16);
                self.write_gpr16(2, (product >> 16) as u16);
                product >> 16 != 0
            }
            (BusWidth::Word, true) => {
                let product =
                    i32::from(self.read_gpr16(0) as i16) * i32::from(operand as u16 as i16);
                self.write_gpr16(0, product as u16);
                self.write_gpr16(2, (product >> 16) as u16);
                product != i32::from(product as u16 as i16)
            }
            (BusWidth::Dword, false) => {
                let product = u64::from(self.read_gpr32(0)) * u64::from(operand);
                self.write_gpr32(0, product as u32);
                self.write_gpr32(2, (product >> 32) as u32);
                product >> 32 != 0
            }
            (BusWidth::Dword, true) => {
                let product = i64::from(self.read_gpr32(0) as i32) * i64::from(operand as i32);
                self.write_gpr32(0, product as u32);
                self.write_gpr32(2, (product >> 32) as u32);
                product != i64::from(product as u32 as i32)
            }
        };
        self.set_flag(FLAG_CF | FLAG_OF, significant);
    }

    pub(super) fn imul_truncated(&mut self, a: u32, b: u32, operand_size: OperandSize) -> u32 {
        // Two-operand signed multiply: the low-half product truncated to the operand size.
        // CF/OF are set when the full product does not sign-extend back from the truncation
        // (the result does not fit). SF/ZF/AF/PF are left undefined, matching the 386.
        let (result, significant) = match operand_size {
            OperandSize::Word => {
                let p = i32::from(a as u16 as i16) * i32::from(b as u16 as i16);
                (p as u16 as u32, p != i32::from(p as u16 as i16))
            }
            OperandSize::Dword => {
                let p = i64::from(a as i32) * i64::from(b as i32);
                (p as u32, p != i64::from(p as u32 as i32))
            }
        };
        self.set_flag(FLAG_CF | FLAG_OF, significant);
        result
    }

    pub(super) fn div(&mut self, operand: u32, signed: bool, width: BusWidth) -> ExecResult<()> {
        // Divide the implicit dividend (AX / DX:AX / EDX:EAX) by the operand, writing the
        // quotient to AL/AX/EAX and the remainder to AH/DX/EDX. Divide-by-zero and
        // quotient overflow are checked BEFORE any register write and return DivideError
        // (real-mode #DE delivery is deferred). Arithmetic flags are left undefined.
        if operand & width_mask(width) == 0 {
            return Err(divide_error());
        }
        match (width, signed) {
            (BusWidth::Byte, false) => {
                let dividend = u32::from(self.read_gpr16(0));
                let divisor = u32::from(operand as u8);
                let quotient = dividend / divisor;
                if quotient > 0xff {
                    return Err(divide_error());
                }
                self.write_gpr8(0, quotient as u8);
                self.write_gpr8(4, (dividend % divisor) as u8);
            }
            (BusWidth::Byte, true) => {
                let dividend = i32::from(self.read_gpr16(0) as i16);
                let divisor = i32::from(operand as u8 as i8);
                let (Some(quotient), Some(remainder)) =
                    (dividend.checked_div(divisor), dividend.checked_rem(divisor))
                else {
                    return Err(divide_error());
                };
                if !(i32::from(i8::MIN)..=i32::from(i8::MAX)).contains(&quotient) {
                    return Err(divide_error());
                }
                self.write_gpr8(0, quotient as u8);
                self.write_gpr8(4, remainder as u8);
            }
            (BusWidth::Word, false) => {
                let dividend =
                    (u32::from(self.read_gpr16(2)) << 16) | u32::from(self.read_gpr16(0));
                let divisor = u32::from(operand as u16);
                let quotient = dividend / divisor;
                if quotient > 0xffff {
                    return Err(divide_error());
                }
                self.write_gpr16(0, quotient as u16);
                self.write_gpr16(2, (dividend % divisor) as u16);
            }
            (BusWidth::Word, true) => {
                let dividend =
                    ((u32::from(self.read_gpr16(2)) << 16) | u32::from(self.read_gpr16(0))) as i32;
                let divisor = i32::from(operand as u16 as i16);
                let (Some(quotient), Some(remainder)) =
                    (dividend.checked_div(divisor), dividend.checked_rem(divisor))
                else {
                    return Err(divide_error());
                };
                if !(i32::from(i16::MIN)..=i32::from(i16::MAX)).contains(&quotient) {
                    return Err(divide_error());
                }
                self.write_gpr16(0, quotient as u16);
                self.write_gpr16(2, remainder as u16);
            }
            (BusWidth::Dword, false) => {
                let dividend =
                    (u64::from(self.read_gpr32(2)) << 32) | u64::from(self.read_gpr32(0));
                let divisor = u64::from(operand);
                let quotient = dividend / divisor;
                if quotient > 0xffff_ffff {
                    return Err(divide_error());
                }
                self.write_gpr32(0, quotient as u32);
                self.write_gpr32(2, (dividend % divisor) as u32);
            }
            (BusWidth::Dword, true) => {
                let dividend =
                    ((u64::from(self.read_gpr32(2)) << 32) | u64::from(self.read_gpr32(0))) as i64;
                let divisor = i64::from(operand as i32);
                let (Some(quotient), Some(remainder)) =
                    (dividend.checked_div(divisor), dividend.checked_rem(divisor))
                else {
                    return Err(divide_error());
                };
                if !(i64::from(i32::MIN)..=i64::from(i32::MAX)).contains(&quotient) {
                    return Err(divide_error());
                }
                self.write_gpr32(0, quotient as u32);
                self.write_gpr32(2, remainder as u32);
            }
        }
        Ok(())
    }

    pub(super) fn set_szp(&mut self, result: u32, width: BusWidth) {
        if !self.pending_flags.is_none() {
            self.materialize_flags();
        }
        self.set_szp_live(result, width);
    }

    fn set_szp_live(&mut self, result: u32, width: BusWidth) {
        let mask = width_mask(width);
        let sign = width_sign(width);
        self.set_flag_live(FLAG_ZF, result & mask == 0);
        self.set_flag_live(FLAG_SF, result & sign != 0);
        self.set_flag_live(FLAG_PF, parity(result as u8));
    }

    fn set_shift_result_flags(
        &mut self,
        cf: Option<bool>,
        of: Option<bool>,
        result: u32,
        width: BusWidth,
    ) {
        if self.pending_flags.is_none() {
            if let Some(cf) = cf {
                self.set_flag_live(FLAG_CF, cf);
            }
            if let Some(of) = of {
                self.set_flag_live(FLAG_OF, of);
            }
            self.set_szp_live(result, width);
            return;
        }

        let cf = match cf {
            Some(cf) => cf,
            None => self.flag(FLAG_CF),
        };
        let of = match of {
            Some(of) => of,
            None => self.flag(FLAG_OF),
        };
        let af = self.flag(FLAG_AF);
        self.pending_flags = PendingFlags::default();
        self.set_flag_live(FLAG_CF, cf);
        self.set_flag_live(FLAG_OF, of);
        self.set_flag_live(FLAG_AF, af);
        self.set_szp_live(result, width);
    }

    pub(super) fn condition(&self, condition: u8) -> bool {
        match condition {
            0x0 => self.flag(FLAG_OF),
            0x1 => !self.flag(FLAG_OF),
            0x2 => self.flag(FLAG_CF),
            0x3 => !self.flag(FLAG_CF),
            0x4 => self.flag(FLAG_ZF),
            0x5 => !self.flag(FLAG_ZF),
            0x6 => self.flag(FLAG_CF) || self.flag(FLAG_ZF),
            0x7 => !self.flag(FLAG_CF) && !self.flag(FLAG_ZF),
            0x8 => self.flag(FLAG_SF),
            0x9 => !self.flag(FLAG_SF),
            0xa => self.flag(FLAG_PF),
            0xb => !self.flag(FLAG_PF),
            0xc => self.flag(FLAG_SF) != self.flag(FLAG_OF),
            0xd => self.flag(FLAG_SF) == self.flag(FLAG_OF),
            0xe => self.flag(FLAG_ZF) || (self.flag(FLAG_SF) != self.flag(FLAG_OF)),
            _ => !self.flag(FLAG_ZF) && (self.flag(FLAG_SF) == self.flag(FLAG_OF)),
        }
    }

    // -------- JIT inline-slot flag helpers (feature `jit`) --------
    //
    // The v2 region inlines the register-only slots (mov r32,r32 / add r32,imm32 / shr r32,imm8)
    // natively against gpr[] (addressed by offset, see the repr(C) Registers guard test). The mov
    // slots touch no flags; the add and shr slots must update the lazy/eager flag state exactly as
    // the interpreter's `alu_add` and `shift_rotate` would. These helpers are that exact update,
    // factored so the emitted native code can call them with the operands it already has in host
    // registers, skipping the interpreter's full decode/dispatch for the flag side effect.
    //
    // Each helper takes the SAME arguments the interpreter handler used (the operand values, not gpr
    // indices) so the descriptor it constructs is byte-identical to what `alu_add`/`shift_rotate`
    // would have produced. The native code has already written the result gpr by offset store; the
    // helper only updates flags. A mid-region fault on a LATER memory slot therefore sees the same
    // pending/live flag state the interpreter would have at that eip (the property the differential
    // suite's flag-equality-after-every-exit test pins).

    /// Set `pending_flags` as a deferred 32-bit ADD of `b` to `a`, exactly as `alu_add(a, b, 0,
    /// BusWidth::Dword)` does. The caller (native inline code) has already computed and written the
    /// result to gpr; this only records the flag descriptor. `a` and `b` are the width-masked (here
    /// full 32-bit) operands, matching the lazy-flags contract: `b` is the raw second operand, not
    /// b+carry (plain ADD, carry 0, so the lazy Add form applies).
    #[cfg(feature = "jit")]
    pub(crate) fn jit_set_pending_add(&mut self, a: u32, b: u32) {
        let result = a.wrapping_add(b);
        let lf = LazyFlags {
            a,
            b,
            result,
            width: BusWidth::Dword,
            op: LazyFlagOp::Add,
            cf_override: None,
        };
        self.pending_flags = PendingFlags::from_legacy(&lf);
    }

    /// Update flags for a 32-bit SHR of `value` by `raw_count`, exactly as
    /// `shift_rotate(5, value, raw_count, BusWidth::Dword)` followed by `set_shift_result_flags`
    /// does. The caller has already written `value >> (count & 0x1f)` to gpr.
    ///
    /// Replicates the single-bit-shift loop's flag semantics precisely: CF is the last bit shifted
    /// out (bit `count-1` of the original value, i.e. bit 24 for count 25). For count != 1, OF is
    /// undefined and falls back to the live OF; AF is always preserved (read live before the
    /// descriptor is dropped). Shifts never stay lazy, so this materializes: it folds any pending
    /// descriptor's CF/OF/AF out, clears pending, and writes CF/OF/AF/SZP live.
    #[cfg(feature = "jit")]
    pub(crate) fn jit_set_shift_flags_shr(&mut self, value: u32, raw_count: u8) {
        let count = u32::from(raw_count) & 0x1f;
        // A zero count affects no flags at all (shift_rotate returns early); the inline path never
        // calls this for count 0 (the matcher's SHR slots pin count to 25), but the guard keeps the
        // helper a faithful replica of shift_rotate for any caller.
        if count == 0 {
            return;
        }
        let result = value >> count;
        // CF = the last bit shifted out by the single-bit loop: after `count` right shifts, that is
        // bit (count-1) of the original value.
        let cf = (value >> (count - 1)) & 1 != 0;
        // OF is defined only for count == 1: for SHR it is the MSB of the original value. For any
        // other count, set_shift_result_flags receives None and falls back to the live OF.
        let of = if count == 1 {
            Some(value & 0x8000_0000 != 0)
        } else {
            None
        };
        self.set_shift_result_flags(Some(cf), of, result, BusWidth::Dword);
    }
}
