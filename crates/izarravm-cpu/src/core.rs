// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::paging::{VGA_APERTURE_END, VGA_APERTURE_START};

/// Which control-register write drove a `flush_tlb_and_code_caches`. Diagnostic only: the arms
/// select a counter and nothing else. See `CpuGsw::flush_tlb_and_code_caches`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TranslationFlushReason {
    /// `MOV CR3, r32` -- the page-directory base was reloaded. No PRODUCTION caller constructs
    /// this any more: a real reload executes through
    /// `CpuGsw::flush_tlb_and_code_caches_for_cr3_write`, the ring-gated entry point that needs
    /// the register's old value and so cannot go through this generic, reason-only function. The
    /// variant stays live for test scaffolding that wants the pre-gate always-full-flush behavior
    /// without simulating a real reload's old/new pair (`allow(dead_code)` outside `cfg(test)`,
    /// where nothing in the lib-only compilation unit constructs it).
    #[cfg_attr(not(test), allow(dead_code))]
    Cr3,
    /// `MOV CR0, r32` or `LMSW`, on the arm where `cr0_write_moves_code_translation` is true.
    Cr0,
    /// `load_task_state` reloading CR3 out of the incoming TSS while PG is set.
    TaskSwitch,
}

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

    /// A CLONE of this CPU with its lazy flags SETTLED — the base carrying the architectural value
    /// and the descriptor torn down. The public form of what `cpu_test.rs`'s `settled_state` does
    /// in-crate, exported for the cross-crate fixtures that cannot reach `materialize_flags`.
    ///
    /// # Why a cross-role comparison needs this
    ///
    /// `registers.eflags` together with `pending_flags` is a REPRESENTATION of the flags, not the
    /// architectural value. Two roles at the same architectural state are free to carry different
    /// (base, descriptor) pairs for it, and since `run_direct_block` settles on the way INTO
    /// emitted code while the interpreter keeps its lazy flags, they routinely do. `CpuGsw` derives
    /// `PartialEq` over every field, so comparing two roles directly compares that split.
    ///
    /// **Settling the base alone is NOT this rule and must not be mistaken for it.** Overwriting
    /// `registers.eflags` with `eflags()` and leaving `pending_flags` standing still byte-compares
    /// the descriptor through the derived `PartialEq`, and additionally constructs a state no code
    /// path produces: a settled base with a live descriptor over it.
    ///
    /// **What this does NOT weaken:** a WRONG flag value still fails, because materialising is
    /// exactly what turns a descriptor into flags. Every other field is compared byte for byte.
    pub fn settled(&self) -> Self {
        let mut settled = self.clone();
        settled.materialize_flags();
        settled
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
        // None of this function's callers (A20 toggle, direct-map change, device DMA, aperture
        // remap) flush the whole TLB in the same operation, so `translation_pages` must not clear
        // here (F11): a stale TLB entry could still serve a code translation with no fresh walk
        // to re-mark it. Keeping the marks is the conservative direction -- more retires, never
        // fewer -- exactly like retaining `code_bytes`.
        let retired = self.invalidate_decode_frontend(false);
        // None of these causes moves a linear->physical PAGE TABLE mapping (T1's own A1
        // analysis: A20/direct-map/aperture/device-DMA are bus-level), so no `Tlb` entry is made
        // stale by one on its OWN account. But `retire_ring` (inside `invalidate_decode_frontend`
        // above) still renumbers ring slots regardless of cause (design review D2), and unlike
        // INVLPG this caller has no narrower, targeted fix for that -- so retire fully. These
        // causes are all rare (A20 toggles once at boot; aperture/direct-map track video mode
        // and PCI decode changes, not per-instruction traffic), so the cost is negligible, and it
        // closes the same slot-reoccupation gap D2 names rather than leaving it open.
        self.tlb.retire_all_slots(retired);
        #[cfg(feature = "jit")]
        self.jit_direct.clear();
    }

    /// The O(1) fetch-window drop, extracted so `flush_tlb_keep_code_caches` can run it without
    /// the decode-line teardown below it.
    ///
    /// These three are dropped for a reason unrelated to INV-T (the translation invariant the
    /// decode-line bump protects): the eip-window prefetch may hold bytes fetched under the old
    /// segmentation, and the code page / fetch page are per-fetch translation caches. A mode
    /// change has to drop them whether or not the linear->physical map moved.
    fn invalidate_fetch_frontend(&mut self) {
        self.code_page.valid = false;
        self.prefetch.invalidate();
        self.fetch_page.invalidate();
    }

    /// `clear_translation_marks` is threaded straight through to
    /// `DecodeCache::invalidate_and_clear_code_marks`: `true` only when the caller flushed the
    /// whole TLB in this same operation (F11).
    ///
    /// The SOLE choke point for `invalidate_code_caches_uncounted` (A20, direct-map, aperture)
    /// and `invalidate_translation_code_caches` (CR0/task-switch full flush, INVLPG): the decode
    /// ring is always fully retired here, which renumbers its slots (design review D2). Returns
    /// the `RingRetired` token instead of consuming it, because what that renumbering means for
    /// the `Tlb` is NOT the same at every caller -- INVLPG has its OWN narrower, targeted fix
    /// (`retire_dormant_slot`, called in `execute_extended.rs` before this function even runs),
    /// and retiring the whole `Tlb` here too would silently defeat it (a real INVLPG invalidates
    /// exactly one page, not the whole TLB, and `invlpg_invalidates_only_the_addressed_page_at_cpl0`
    /// pins that). Every caller must therefore decide, explicitly, at its own call site.
    fn invalidate_decode_frontend(&mut self, clear_translation_marks: bool) -> RingRetired {
        self.invalidate_fetch_frontend();
        // Paging, mode, A20, and physical-map changes route through here. Any can make the same
        // linear address decode from different bytes, so invalidate the lines and their SMC marks.
        let retired = self
            .decode_cache
            .invalidate_and_clear_code_marks(clear_translation_marks);
        self.perf.translation_pages_marked = self.decode_cache.translation_pages_marked;
        // No line survives the bump, so no aperture line does either. Clearing here is what
        // makes the aperture-remap flush self-limiting: the flush it triggers lands in this
        // function and disarms it until aperture code is genuinely decoded again.
        self.has_aperture_code.0 = false;
        retired
    }

    /// `clear_translation_marks`: see `invalidate_decode_frontend`. The two production callers
    /// disagree: `flush_tlb_and_code_caches`'s CR0/task-switch arms flush the whole TLB first
    /// (`true`); `INVLPG` invalidates a single TLB entry (`false`).
    ///
    /// Returns the `RingRetired` token `invalidate_decode_frontend` produced, for the SAME reason
    /// that function does not consume it: `flush_tlb_and_code_caches` needs the whole `Tlb`
    /// retired here (it has no narrower fix of its own), while the INVLPG handler in
    /// `execute_extended.rs` does (`retire_dormant_slot`, already run) and must discard this one
    /// explicitly rather than have it silently retire the live slot too.
    pub(super) fn invalidate_translation_code_caches(
        &mut self,
        clear_translation_marks: bool,
    ) -> RingRetired {
        self.perf.decode_inval_other += 1;
        self.perf.code_invalidations += 1;
        let retired = self.invalidate_decode_frontend(clear_translation_marks);
        #[cfg(feature = "jit")]
        self.jit_direct.invalidate_translation();
        retired
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
        self.record_fast_map_wipe_extent();
    }

    /// Wipe the FastMap and charge what it discarded to the audit counters. The extent is the only
    /// honest price of a wipe: the entry count, not the call count.
    #[cfg(all(
        feature = "jit",
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    pub(super) fn record_fast_map_wipe_extent(&mut self) {
        let extent = self.jit_fast_map.invalidate_all();
        self.jit_direct.fast_map_audit.wipe_pages_cleared += extent.pages;
        self.jit_direct.fast_map_audit.wipe_vga_pages_cleared += extent.vga_pages;
    }

    #[cfg(feature = "jit")]
    fn bump_wipes_tlb_flush_counter(&mut self) {
        self.jit_direct.fast_map_audit.wipes_tlb_flush += 1;
    }

    #[cfg(not(feature = "jit"))]
    fn bump_wipes_tlb_flush_counter(&mut self) {}

    /// The whole-cache code-translation teardown, with the CAUSE named so the teardown that
    /// `decode_inval_other` aggregates can be attributed to the control-register write that
    /// caused it.
    ///
    /// The three named reasons are the three production callers: `MOV CR3`
    /// (`execute_extended.rs`), `MOV CR0` when `cr0_write_moves_code_translation` says the map
    /// moved (`flush_tlb_for_cr0_write` below), and `load_task_state` under paging
    /// (`control.rs`). `Unattributed` is for test harnesses and any future caller that has not
    /// been given a reason yet; it counts in `decode_inval_other` like every other reason and in
    /// no split counter, so the split can only ever UNDER-count, never over-count.
    ///
    /// The split is diagnostic, not behavioural: every arm does exactly the same work. Nothing
    /// here gates anything. `dev_docs/2026-09-02-tyrian-586-specs-diag.md` section 4.1 reached
    /// "it is CR3" by eliminating the other two from unrelated counters; this makes the same
    /// claim measurable, which is the precondition the diagnosis set for writing any gate.
    ///
    /// **`Cr3` here is the pre-gate, always-full-flush entry point**, kept for callers that are
    /// not simulating a real `MOV CR3` reload and so have no old/new register pair to give the
    /// ring (test scaffolding, mainly). A real `MOV CR3` executes through
    /// `flush_tlb_and_code_caches_for_cr3_write` below, which is the only caller that can seed and
    /// consult the two-slot ring. Both bump `decode_inval_cr3`, so the ring's own identity
    /// (`cr3_code_flush_taken + cr3_code_flush_skipped == decode_inval_cr3`) holds for every ring
    /// write; this generic arm is simply outside that count's denominator on the taken/skipped
    /// side, exactly as an "under-attributed" `Unattributed` reason would be.
    pub(super) fn flush_tlb_and_code_caches(&mut self, reason: TranslationFlushReason) {
        match reason {
            TranslationFlushReason::Cr3 => self.perf.decode_inval_cr3 += 1,
            TranslationFlushReason::Cr0 => self.perf.decode_inval_cr0 += 1,
            TranslationFlushReason::TaskSwitch => self.perf.decode_inval_task_switch += 1,
        }
        self.bump_wipes_tlb_flush_counter();
        // T1 (design `2026-09-02-cr3-data-side-design.md`): the physical data-page caches
        // (`data_read_pages`/`data_write_pages`) are keyed by physical page and a bus mapping
        // epoch, never by CR3, so they are no longer touched by any control-register flush --
        // only their three bus causes (`note_a20_changed`, `note_direct_map_changed`,
        // `note_direct_data_map_changed`) invalidate them.
        #[cfg(all(
            feature = "jit",
            target_arch = "x86_64",
            any(target_os = "windows", target_os = "linux")
        ))]
        self.record_fast_map_wipe_extent();
        // The decode ring retires below, unconditionally, which renumbers ring slots (design
        // review D2) -- this caller (unlike INVLPG) has no narrower `Tlb` fix of its own, so the
        // whole `Tlb` retires too, consuming the token `invalidate_translation_code_caches`
        // returns. `translation_pages` may clear here (F11): both the TLB and the decode lines it
        // could back are gone by the time this returns.
        let retired = self.invalidate_translation_code_caches(true);
        self.tlb.retire_all_slots(retired);
    }

    /// `MOV CR3, r32`, the ring-gated code half (design `2026-09-02-cr3-code-cache-gate-design.md`
    /// part 2). Unlike `flush_tlb_and_code_caches(Cr3)` above, this is what a REAL reload executes:
    /// it needs the register's OLD value to seed the ring (R8) and to tell a same-directory
    /// reselect (R1) from a genuine third value (R3), so it owns the assignment to
    /// `self.control.cr3` itself. Callers must pass the value about to be written and must NOT
    /// assign the register first.
    ///
    /// The data side is now split three ways (design `2026-09-02-cr3-data-side-design.md`, T1+T2,
    /// amending the paragraph this replaces). `data_read_pages`/`data_write_pages` are OUT of this
    /// function entirely (T1): they are keyed by physical page and a bus mapping epoch, never by
    /// CR3, so no arm here touches them any more -- only their three bus causes do. The FastMap
    /// wipe stays unconditional on BOTH arms below, exactly as before (T3, retaining it across an
    /// R1 reselect, is a later slice). The TLB is now GATED, the same shape as the decode and JIT
    /// halves (T2): `Reselected` restores the reselected slot's generation with `select_generation`,
    /// no walk, no entry lost; `Allocated` mints a fresh generation for the newly occupied slot
    /// with `allocate_generation`, trusting nothing about who held the slot before (review finding
    /// P1, PR #826: merging this with `Reselected` let a directory that had just vacated a slot
    /// via INVLPG's narrower retire be silently restored for a completely different directory);
    /// `Taken` retires both generations with `retire_all_slots` (consuming the `RingRetired` token
    /// `select_context`'s R3 hands back) and then mints slot 0's fresh one.
    /// The eip-window fetch frontend stays unconditional on every write, both arms, unchanged. The
    /// JIT half is gated TOO, by slice S-B (design doc `2026-09-02-cr3-jit-half-design.md`): a link
    /// graph built under one directory is retained across an R1 reselect back to it, keyed apart
    /// from a second directory's graph by a per-slot epoch and a `slot`-tagged rendezvous key
    /// rather than being torn down on every write.
    ///
    /// | mechanism | what it buys, and why it is safe |
    /// |---|---|
    /// | L1, `link_epochs: [u64; 2]` plus a live `link_slot` | an R1 reselect restores that slot's own epoch instead of minting a new one, so every block made link-visible under it is visible again with no work; R2 allocate mints a fresh epoch for the newly occupied slot, invalidating nothing because nothing carries it yet |
    /// | L2, `LinkTarget` carries a `context: u8` | partitions `linear_blocks` AND `waiting` by ring slot at zero new comparison sites, so an install under slot B cannot resolve a source parked under slot A |
    /// | L3, `make_link_visible`'s cross-context arm | a block re-touched under the OTHER slot's live epoch has its outbound cells cleared before this context's admission can rebind them -- **a retained link is a direct jump inside the arena that bypasses every key check**, so without this a stale cell would chain into the wrong context's target with no fault, no counter, wrong answers |
    /// | L4, root dispatch is the base case | entry always builds its `BlockKey` from the LIVE physical, so no cell can ever be armed FROM a block that was never entered under this context, and `try_link_inner`'s `stale_epoch` refusal (compared against the live slot's epoch alone) is a second, independent barrier behind the key partition |
    /// | L5, every wholesale cause retires both `link_epochs` | `invalidate_translation` (this function's `Taken` arm, the translation-page store arm, the SMC wholesale arm) keeps tearing the WHOLE graph down exactly as before this slice; only the R1 reselect path is new |
    ///
    /// `ContextSelect::Reselected(slot)` is R1 and `ContextSelect::Allocated(slot)` is R2
    /// (review finding P1, PR #826, split them: `DecodeCache::select_context` used to return one
    /// `Skipped(slot)` for both, and a caller that could not tell them apart could only stay safe
    /// by leaning on an invariant a narrower retire elsewhere could break). `Reselected` calls
    /// `select_link_context`, restoring that slot's saved epoch; `Allocated` calls
    /// `allocate_link_context`, minting a fresh one, exactly mirroring what the `Tlb` arm above
    /// does for the same two cases and for the same reason. `ContextSelect::Taken(slot)` is R3,
    /// and only that arm calls `invalidate_translation()` (and, after it, `allocate_link_context`,
    /// which mints a FRESH epoch for slot 0 after the wholesale retire).
    pub(super) fn flush_tlb_and_code_caches_for_cr3_write(&mut self, new_cr3: u32) {
        self.perf.decode_inval_cr3 += 1;
        // `wipes_tlb_flush` stays first and unconditional (design section (d) point 1): it counts
        // the EVENT, not the outcome, and is the divergence check against `cr3_code_flush_taken +
        // _skipped`.
        self.bump_wipes_tlb_flush_counter();
        // T1: the FastMap wipe stays unconditional on both arms (design section (d) point 5); T3,
        // retaining it too, is a later slice. `data_read_pages`/`data_write_pages` are not touched
        // here at all any more.
        #[cfg(all(
            feature = "jit",
            target_arch = "x86_64",
            any(target_os = "windows", target_os = "linux")
        ))]
        self.record_fast_map_wipe_extent();
        let old_cr3_masked = self.control.cr3 & 0xffff_f000;
        self.control.cr3 = new_cr3;
        let new_cr3_masked = new_cr3 & 0xffff_f000;
        self.invalidate_fetch_frontend();
        // T2: the TLB is now gated on the SAME ring decision as the decode and JIT halves, so it
        // has to run AFTER `select_context`, not before -- the reverse of the pre-T2 ordering, in
        // which the whole data side wiped unconditionally above this match. Write any future
        // reordering's rule here; do not leave a stale comment standing next to it.
        match self
            .decode_cache
            .select_context(old_cr3_masked, new_cr3_masked)
        {
            ContextSelect::Reselected(slot) => {
                self.perf.cr3_code_flush_skipped += 1;
                // R1: the requested directory already owns this slot. RESTORE its saved
                // generation and epoch; no entry is lost, and the generation/epoch compare in
                // `lookup`/`insert`/`is_link_visible` IS the restore. Never mint here: see the
                // `ContextSelect` doc comment (review finding P1, PR #826).
                self.tlb.select_generation(slot);
                #[cfg(feature = "jit")]
                {
                    self.jit_direct.select_link_context(slot);
                    self.perf.cr3_link_context_selects += 1;
                }
            }
            ContextSelect::Allocated(slot) => {
                self.perf.cr3_code_flush_skipped += 1;
                // R2: a miss took a free slot. MINT a fresh generation and epoch for it; nothing
                // about that slot's previous occupant, if any, may be trusted. This is the P1
                // fix (review, PR #826): before this split, this arm called `select_generation`,
                // which could restore a directory that had just vacated the slot via a narrow
                // retire (INVLPG's `retire_dormant_slot`) that never re-minted THIS slot's
                // generation, silently serving one directory's cached translations to another.
                // Minting costs nothing here -- the slot is, by construction, either virgin
                // (seeded distinct at `Tlb::default`/`BlockCache::new`) or was already fully
                // retired -- so this is not a narrower fix, it removes the dependency on every
                // OTHER caller keeping its own retire paired correctly.
                self.tlb.allocate_generation(slot);
                #[cfg(feature = "jit")]
                {
                    self.jit_direct.allocate_link_context(slot);
                    self.perf.cr3_link_context_selects += 1;
                }
            }
            ContextSelect::Taken(slot, token) => {
                self.perf.cr3_code_flush_taken += 1;
                self.perf.decode_inval_other += 1;
                self.perf.code_invalidations += 1;
                self.has_aperture_code.0 = false;
                // R3: both TLB generations retire together with the decode ring (design review
                // D2) -- `retire_ring` already renumbered the slots this token came from, so
                // `slot`'s old occupant, if any, must not be served under the new one. Mint slot
                // 0's fresh generation only after the retire, exactly as the decode and JIT
                // halves mint their own fresh state only after their own retire.
                self.tlb.retire_all_slots(token);
                self.tlb.allocate_generation(slot);
                self.perf.translation_pages_marked = self.decode_cache.translation_pages_marked;
                #[cfg(feature = "jit")]
                {
                    self.jit_direct.invalidate_translation();
                    self.jit_direct.allocate_link_context(slot);
                    self.perf.cr3_link_graph_retires += 1;
                }
            }
        }
    }

    /// `flush_tlb_and_code_caches` without the code-translation teardown: the decode lines, their
    /// SMC marks, the aperture flag and the Direct link graph all stay. `new_cr0` is the value CR0
    /// holds AFTER the write that reached here; see `flush_tlb_for_cr0_write` for why the DATA-side
    /// wipe below only needs the new value, not the old one.
    ///
    /// What this gives up, and why each one is safe to retain (design section 4.1):
    ///
    /// | skipped | why retaining it is safe |
    /// |---|---|
    /// | decode-line generation bump | the line key is `(linear, d)` plus a stored `phys_start`; the map did not move, `d` is compared at every hit site, and nothing mode-dependent is baked into `DecodedInsn` |
    /// | dirty-word wipe of `code_bytes` / `code_pages` | sticky SMC marks retained means MORE trapping, never less; a stale mark costs one future narrow attempt |
    /// | `dirty_byte_words` / `dirty_page_words` clear | bookkeeping for the wipe above, bounded by the bitmap size |
    /// | `native_code_watch.clear()` | an armed host write-watch retained is the conservative direction; skipping a clear creates no new watch edges |
    /// | `code_page_lin.clear()` | it maps physical page to linear page; the map did not move, so every entry stays true |
    /// | `has_aperture_code.0 = false` | the lines survive, so the flag describing them should too -- clearing it while they stay live would be the bug |
    /// | `jit_direct.invalidate_translation()` | links carry `mode_key`, and no static edge can cross a `mode_key` boundary, so a retained link is unreachable under the new mode |
    ///
    /// The last row also gives up a periodic link-policy amnesty (the chain-layout reset and the
    /// data-segment decline clear that ride inside `invalidate_translation`). Losing an amnesty can
    /// only make link ADMISSION stricter, never admit an edge that should be refused, so it is not
    /// a soundness question -- but it is why the ladder measures `link_refusals` alongside the win.
    ///
    /// The DATA half (TLB, FastMap) is now ALSO gated, on a narrower condition than the code half
    /// above: it wipes only when `new_cr0 & CR0_PG != 0`. This branch is reached only when
    /// `cr0_write_moves_code_translation` is false, which forces
    /// `old_cr0 & CR0_PG == new_cr0 & CR0_PG` (the predicate's first disjunct is exactly
    /// `delta & CR0_PG != 0`) -- so PG is unchanged here, and checking `new_cr0` alone tells us
    /// whether it was already 0 or already 1 on both sides. Two cases:
    ///
    /// - PG stays 1 (paging was already on, e.g. only TS/MP/EM/NE/AM/CD/NW moved): still wipe.
    ///   CORRECTING THE PRIOR TEXT HERE (design `2026-09-02-cr3-data-side-design.md` section (c)):
    ///   `data_read_pages`/`data_write_pages` are NOT touched by this function at all any more
    ///   (T1 -- they are physical-keyed with no permission bits, so no control-register write of
    ///   any kind bears on them), and what the FastMap caches is never the filling access's
    ///   privilege, only the PAGE's U/S and R/W bits, re-checked against the LIVE accessor at
    ///   every probe (`memory.rs::lookup_access`). The FastMap still wipes here, but for the
    ///   ordinary reason every unconditional flush wipes it, not because it is "privilege-tagged".
    ///   The TLB (T2) is gated the SAME way the CR3 ring gates it: this path never renumbers ring
    ///   slots (`select_context` is not called here), so only the LIVE slot's generation may
    ///   retire -- `Tlb::flush_live_slot()`, not `retire_all_slots()`, because there is no
    ///   `RingRetired` token to consume and the dormant slot's entries are still valid under their
    ///   own directory. The `memory.rs` WP invariant ("WP changes flush, so `wp` is consistent
    ///   within a generation") stays exactly as conservative as before for this case --
    ///   unconditional.
    /// - PG stays 0 (paging was already off -- the ONLY way a bare PE toggle reaches this branch,
    ///   since PG=1 with PE=0 is `#GP(0)` at the MOV CR0 site): skip the wipe. With paging off,
    ///   `translate_linear_checked`'s early return makes the TLB dead weight (never populated,
    ///   never consulted); `data_read_pages`/`data_write_pages` are keyed by PHYSICAL address alone
    ///   with no permission bits in the entry (`memory.rs::read_direct_page_cached`) and, since T1,
    ///   are not reached from this function regardless; and `memory.rs::fast_map_permissions`
    ///   returns the fixed, fully-permissive `PagePermissions::UNPAGED` for every population made
    ///   while paging is off, so no FastMap entry populated under this condition carries a
    ///   CPL-dependent tag to begin with. A PE toggle "changes CPL", but there is no
    ///   privilege-tagged data here for a changed CPL to stale. The TLB wipe is skipped along with
    ///   the rest rather than kept alone: it is free either way (nothing is in it, paging being
    ///   off), so keeping it unconditional would buy nothing.
    ///
    /// `wipes_tlb_flush` still counts the EVENT on the skipped arm (`bump_wipes_tlb_flush_counter`
    /// runs unconditionally, same as the ungated function above) -- only `wipe_pages_cleared` and
    /// the rest of the wipe's cost collapse. See `dev_docs/2026-09-01-tyrian-transition-diag.md`
    /// section 7 S2 for the measured motivation (34,112 whole-map wipes per guest second on a
    /// workload that never enables paging) and `cpu_cr0_flush_test.rs`'s `data_caches` module for
    /// the red/anti-regression/correctness rows this rests on.
    fn flush_tlb_keep_code_caches(&mut self, new_cr0: u32) {
        self.bump_wipes_tlb_flush_counter();
        if new_cr0 & CR0_PG != 0 {
            self.tlb.flush_live_slot();
            #[cfg(all(
                feature = "jit",
                target_arch = "x86_64",
                any(target_os = "windows", target_os = "linux")
            ))]
            self.record_fast_map_wipe_extent();
        }
        self.invalidate_fetch_frontend();
    }

    /// Whether a CR0 write from `old` to `new` can move the linear->physical map.
    ///
    /// PG is the ONLY bit of CR0 that participates in address translation. Every other bit (PE,
    /// MP, EM, TS, NE, AM, NW, CD) leaves the map identical: PE is `jit_mode_key` bit 1, so it is
    /// inside `BlockKey` and inside `LinkTarget` and no static edge crosses a `mode_key` boundary;
    /// MP/EM/TS/NE are re-read from this struct by emitted x87 code at runtime (`emit_gate` in
    /// `jit/x87_avx2_emit.rs`, mirrored on the admission side in `jit/native_x87.rs`); AM is
    /// re-read at every dispatcher entry through the `alignment_armed` mirror, which both call
    /// sites recompute before they flush; CD/NW are inert storage. `CLTS` writes CR0 with no flush
    /// at all, and a task switch sets `CR0_TS` with no code flush either -- this rule is existing
    /// policy, stated by existing code, and the two CR0 writers were the inconsistency.
    ///
    /// WP IS NOT HERE FOR CODE TRANSLATION. Code fetch is a read; WP is supervisor *write*
    /// permission, so it cannot stale a decode line or a link. It is in this predicate for the
    /// DATA-SIDE slice (`flush_tlb_keep_code_caches`'s `new_cr0 & CR0_PG` gate), whose invariant is
    /// stated in `memory.rs`: the protection check is redone from the cached page bits against the
    /// current accessor (CPL can change without a flush); WP changes flush, so `wp` is consistent
    /// within a generation. That invariant still holds with the data-side gate in place: a write
    /// where `new & CR0_PG != 0 && delta & CR0_WP != 0` makes this predicate TRUE, so it takes the
    /// `flush_tlb_and_code_caches` arm, which retires the TLB unconditionally via
    /// `invalidate_translation_code_caches` -- not the gated `flush_tlb_keep_code_caches` at all.
    /// A WP change can only reach the gated function when
    /// `new & CR0_PG == 0`, at which point the WP bit governs nothing (WP is a paging-only
    /// protection check) and the DATA-side gate's own `new_cr0 & CR0_PG != 0` test is already false,
    /// so it wipes only for the unrelated "PG stays 1" case, never skips it. Keeping the term costs
    /// nothing and pre-pays the follow-on. Do not "simplify" it away.
    fn cr0_write_moves_code_translation(old: u32, new: u32) -> bool {
        let delta = old ^ new;
        delta & CR0_PG != 0 || (new & CR0_PG != 0 && delta & CR0_WP != 0)
    }

    /// The gated flush the two CR0 writers take. `old` is the value CR0 held BEFORE the write;
    /// `new` is the value it holds after. Both call sites flush AFTER assigning `self.control.cr0`,
    /// so the old value has to be captured before that assignment; capturing it after collapses
    /// both arguments to the new value and the predicate then never fires. The PG rows in
    /// `cpu_cr0_flush_test.rs` catch exactly that.
    ///
    /// SWAPPING the two arguments, by contrast, is provably INERT and no test catches it:
    /// `delta` is symmetric, and the asymmetric `new & CR0_PG` term is only reached when
    /// `delta & CR0_PG == 0`, at which point `old & CR0_PG == new & CR0_PG`. The mutation ledger
    /// at the foot of `cpu_cr0_flush_test.rs` records this as a non-gate rather than as coverage.
    pub(super) fn flush_tlb_for_cr0_write(&mut self, old_cr0: u32, new_cr0: u32) {
        if Self::cr0_write_moves_code_translation(old_cr0, new_cr0) {
            self.flush_tlb_and_code_caches(TranslationFlushReason::Cr0);
        } else {
            self.flush_tlb_keep_code_caches(new_cr0);
        }
    }

    pub(super) fn set_eip(&mut self, eip: u32) {
        self.rep_resume_active = false;
        self.rep_execution.resume = None;
        self.registers.eip = eip;
        self.prefetch.invalidate();
    }

    pub(super) fn record_write_page(&mut self, physical: u32) {
        let page = physical >> 12;
        // One compare in front of the two linear scans below. A repeat store to the page the
        // last one hit is by far the common shape (a string move, a stack sequence, any loop
        // walking a buffer), and this instruction already recorded that page, so both scans
        // would run to their conclusion and change nothing. See `CpuGsw::last_written_page` for
        // why a match is exact rather than a guess.
        if self.last_written_page == page {
            return;
        }
        self.last_written_page = page;
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
        #[cfg(feature = "jit")]
        {
            self.jit_direct.fast_map_audit.wipes_a20 += 1;
        }
    }

    pub fn note_direct_map_changed(&mut self) {
        self.invalidate_code_caches();
        self.invalidate_direct_pages();
        self.perf.direct_map_invalidations += 1;
        #[cfg(feature = "jit")]
        {
            self.jit_direct.fast_map_audit.wipes_direct_map += 1;
        }
    }

    /// Drop cached data pointers after the VGA direct-write aperture re-points, without discarding
    /// decoded or compiled guest code -- and without discarding the RAM mappings, which is what
    /// makes this worth having separately from `note_direct_map_changed`.
    ///
    /// SCOPE, and why it is exactly this. There are exactly two callers, and BOTH are gated on a
    /// change in `Vega::direct_write_token`: the port-write path in `bus.rs`, and the INT 10h HLE
    /// seam in `video.rs`. The second is the one a reader should worry about, since it wraps a
    /// whole mode set -- but an INT 10h mode set routes separately through
    /// `Machine::set_vga_mode_with_clear`, which raises the COARSE `mark_direct_map_changed`, and
    /// the run loop tests that first. The token check there is a backstop, not the mode-set path.
    ///
    /// The only thing the token describes is which host bytes back physical pages
    /// `0xA0000..0xAFFFF` for a data access. `Bus::direct_page` hands out a video pointer for that
    /// range and for no other; every other page resolves through `direct_ram_bytes`, whose answer
    /// is a function of the RAM lookup table, and that table is rebuilt only on a PCI
    /// memory-decode change -- which routes through the COARSE `note_direct_map_changed` instead.
    /// So an aperture-scoped invalidation is not an approximation of the global one for this cause.
    /// It is equivalent to it.
    ///
    /// Because it is scoped, the machine must NOT advance the global direct-mapping epoch for this
    /// cause, and `Machine::mark_direct_data_map_changed` does not. Advancing it would make every
    /// surviving RAM entry stop matching on the next interpreter probe and would empty the
    /// direct-page caches on their next insert -- exactly the coarse behaviour this replaces. The
    /// two halves are one change; neither works alone.
    ///
    /// Aperture pages are reached through the FastMap's own VGA registry and through a
    /// physical-range sweep of the direct-page caches, so a linear alias of the aperture is covered
    /// however the guest got to it. Emitted native code never reads the epoch table -- its only
    /// guard against a stale aperture mapping is the entry itself being cleared -- and the registry
    /// sweep is what clears it.
    ///
    /// HISTORY: doom moves this token 3,425,430 times in one timedemo, all of them the Mode X map
    /// mask (port 0x3C5, sequencer index 2) cycling planes 0-3. The global form threw away 43.3M
    /// live entries doing it, 88.1% of them RAM whose host pointers had not moved.
    /// Evidence: `.bench/results/fastmap-wipe-20260803/README.md`.
    /// The APERTURE REMAP edge: what physical 0xA0000-0xBFFFF CONTAINS changed without any
    /// memory write, so the SMC marks cannot have seen it. Read-side register moves (GC read map
    /// select, read mode, odd/even), mode-set arms that never touch the direct-write identity,
    /// and anything else that re-points the window all land here via the machine's batch
    /// boundary.
    ///
    /// Gated on the decode cache actually holding aperture code, which no fixture on the board
    /// ever does: for everything else this is one bool load. When it IS set, the full
    /// invalidation runs, and clears the flag on the way through, so a guest cycling the
    /// aperture pays one flush per aperture line inserted, not one per cycle.
    pub fn note_aperture_content_changed(&mut self) {
        if self.has_aperture_code.0 {
            self.invalidate_code_caches();
        }
    }

    pub fn note_direct_data_map_changed(&mut self) {
        self.data_read_pages
            .invalidate_physical_range(VGA_APERTURE_START, VGA_APERTURE_END);
        self.data_write_pages
            .invalidate_physical_range(VGA_APERTURE_START, VGA_APERTURE_END);
        #[cfg(all(
            feature = "jit",
            target_arch = "x86_64",
            any(target_os = "windows", target_os = "linux")
        ))]
        {
            let extent = self.jit_fast_map.invalidate_vga_pages();
            self.jit_direct.fast_map_audit.wipe_aperture_pages_cleared += extent.vga_pages;
        }
        self.perf.direct_map_invalidations += 1;
        #[cfg(feature = "jit")]
        {
            self.jit_direct.fast_map_audit.wipes_direct_data_map += 1;
        }
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
        // Value-less callers, and the one place mutable imm32 lanes are refused outright: a device
        // or HLE range, a page-walk A/D-bit store, and the string-op translate-time invalidation
        // all arrive here. None of them is the guest patching its own instruction stream through
        // its own store path, so none of them may keep a block alive.
        self.note_code_write_inner(physical, width, false)
    }

    /// Host address of `len` guest bytes at `physical`, from a page this CPU has already FETCHED
    /// code out of. `None` whenever the fetch-page cache does not currently cover them, which is a
    /// REFUSAL rather than a failure: the one caller (`imm_lane_for`) then keeps its baked
    /// immediate and the block is correct as ever.
    ///
    /// The fetch cache, and ONLY the fetch cache, is the page-kind guard. `Bus::direct_page`
    /// produces a VIDEO pointer (the mode-13h plane window) for `BusAccessKind::DataRead` and
    /// `DataWrite` and for nothing else, so a fetch-cache entry can never be a device aperture —
    /// while `data_read_pages` / `data_write_pages` can be, and are not cleared by
    /// `note_direct_data_map_changed` in a way compiled blocks observe. An earlier revision fell
    /// back to those two caches; a lane baked against a plane buffer could then have outlived a
    /// write-token change and read an immediate the interpreter would not. Do not reintroduce the
    /// fallback: the win from it is a lane that would have been created one recompile later
    /// anyway, and the loss is the only structural argument that keeps device memory out.
    ///
    /// The pointer's lifetime is the lifetime of the mapping: a real direct-map change routes
    /// through `note_direct_map_changed`, which drops every compiled block, and RAM host pointers
    /// do not move for a given physical page.
    ///
    /// Keyed by linear address, so the physical page is matched by scanning the cache's handful of
    /// entries — compile time only.
    #[cfg(feature = "jit")]
    pub(crate) fn direct_host_bytes(&self, physical: u32, len: u32) -> Option<usize> {
        let page = physical & !0x0fff;
        let offset = (physical & 0x0fff) as usize;
        let last = physical.checked_add(len.checked_sub(1)?)?;
        if last & !0x0fff != page {
            return None;
        }
        let end = offset + len as usize;
        self.fetch_page.entries.iter().find_map(|entry| {
            (entry.valid && entry.physical_page == page && end <= entry.len && !entry.ptr.is_null())
                .then(|| entry.ptr as usize + offset)
        })
    }

    /// Cheap, side-effect-free probe hoisted out of `note_code_write_hit`: does the store range
    /// touch any watched code (a compiled block's physical span or a decoded instruction line) OR
    /// a page the CR3 code-cache gate's ring depends on (a page-directory or page-table page a
    /// walk has read, `translation_pages`)? Value-aware callers (the sized-store path) gate the
    /// read-old-bytes comparison that drives G2 same-value elision on this, paying nothing extra
    /// when the store misses all code.
    ///
    /// `translation_pages` belongs HERE and not in `note_code_write_inner` (finding F1): this is
    /// the predicate BOTH FastMap store gates already compute unconditionally
    /// (`memory.rs`'s `finish_fast_map_write` and `write_linear_fragment_after_probe`), so a page
    /// table's writes reach the invalidation door at all only because this test sees them. Placed
    /// below those gates, a `MOV [pte], eax` would never open the door and the watch would be dead
    /// on the hot path.
    #[inline]
    pub(super) fn code_write_watched(&self, physical: u32, width: u32) -> bool {
        #[cfg(feature = "jit")]
        if self.jit_direct.range_hits_compiled_code(physical, width) {
            return true;
        }
        if self.decode_cache.range_hits_code(physical, width) {
            return true;
        }
        self.decode_cache
            .range_hits_translation_page(physical, width)
    }

    /// The invalidation body of a code write, entered from the guest's own COMMITTED data stores.
    /// Only reached once the store is known to have changed a watched code byte (G2 elision skips
    /// it for same-value sized stores). These are the only writes allowed to take the mutable-lane
    /// exemption; `note_code_write` is the value-less door and refuses it.
    ///
    /// The unit-sim feed lives in the shared body below, behind the elision choke, so the
    /// diagnostic mirrors the post-elision production invalidation path exactly.
    #[inline]
    pub(super) fn note_code_write_hit(&mut self, physical: u32, width: u32) -> bool {
        self.note_code_write_inner(physical, width, true)
    }

    /// A CHANGED one-byte guest store onto a direct RAM page, routed to whichever of the two doors
    /// the one-byte lane arm selects. The slow byte path's entry point (`write_linear_u8`).
    ///
    /// **The arm test is the whole point of this function existing.** A one-byte lane can only be
    /// absorbed through the value-aware door, so `IZARRAVM_IMM8_LANES=1` needs it; but the door
    /// also decides whether a byte write that lands on a DWORD lane is counted in
    /// `smc_lane_reject_width` / `smc_lane_reject_address`, and it makes the choke walk the lane
    /// arrays for every block a byte store touches. Taking it unconditionally would move both
    /// counters and add that walk on the SHIPPED arm, which
    /// `dev_docs/duke-reprofile-2026-08-19.md` reads as a baseline (`smc_lane_reject_width` "reads
    /// 0 today") and compares against the 08-16 census. The off arm must be the pre-slice world
    /// bit for bit, counters included, so it keeps the value-less door it always had.
    /// `the_off_arm_moves_no_rejection_counter_on_a_byte_write` is that pin.
    ///
    /// **TWO ARMS OPEN THE SAME DOOR SINCE L2 ARM 2.** `IZARRAVM_COUNT_LANES=1` puts one-byte
    /// GROUP-2 COUNT lanes in blocks (`count_lane_for`), which need the value-aware door for
    /// exactly the reason the `0x80` immediate lanes do, so the test is an OR rather than a second
    /// knob replacing the first. The shipped world — both knobs off — is unchanged, and each arm
    /// alone still opens the door on its own: the two lane classes are independent levers and
    /// neither may require the other to be measurable.
    ///
    /// The knobs are process-wide `OnceLock` reads (thread-local `Cell`s under `cfg(test)`), so
    /// this is a predictable load and branch, not an env lookup.
    #[inline]
    pub(super) fn note_code_byte_write_hit(&mut self, physical: u32) -> bool {
        #[cfg(feature = "jit")]
        if crate::jit::direct::value_aware_byte_door_enabled() {
            return self.note_code_write_hit(physical, 1);
        }
        self.note_code_write(physical, 1)
    }

    /// In-flight SMC needs no check here, and that is a proof rather than an omission. A store
    /// from native code into watched code never commits inside the block: the emitted store's
    /// code-watch guard side-exits (`SideExitReason::CodeWatch`) before the write, and the
    /// interpreter then replays the instruction. So no compiled block is mid-execution when this
    /// runs, and a block cannot patch its own lane from under itself.
    #[inline]
    fn note_code_write_inner(&mut self, physical: u32, width: u32, lanes: bool) -> bool {
        // THE CALL-OUT WINDOW. `InterpretOne` runs one interpreter instruction with a native block
        // live on the host stack, and that instruction is allowed to STORE. The proof this
        // function's doc comment rests on -- "no compiled block is mid-execution when this runs" --
        // is exactly what stops holding there, and the consequence is not academic: the block's
        // own page is code-watched, so a store into it reaches `invalidate_physical_range`, which
        // retires the running block, frees its arena bytes and can hand them to the next
        // compilation while the helper still has to RETURN through them.
        //
        // So while the window is open the write is RECORDED and reported as a hit, and nothing is
        // invalidated. Reporting the hit is what makes the helper's R5 clause fire, which ends the
        // native run at this instruction; `run_direct_block` then drains the list through this
        // same function with the flag clear, so the invalidation happens for real one step later,
        // before any guest instruction can observe the stale code.
        //
        // The window branch asks `code_write_watched` FOR ITSELF, and the reason is that the
        // sentence this comment used to end with was wrong: "`note_code_write_hit` reaches here
        // only behind `code_write_watched`" is true of the SIZED store path and false of the BYTE
        // one. `write_linear_u8` routes through `note_code_byte_write_hit` on `changed` alone, by
        // design, so that a one-byte immediate patch can be absorbed as a lane -- see the two-doors
        // note in `write_linear_fragment`. Without the probe here, EVERY changed byte store made
        // from inside a call-out would be recorded, R5 would read a non-empty list, and every
        // byte-storing row on the `InterpretOne` allowlist would RESYNC on every execution and be
        // demoted by the governor for traffic that touches no code at all. It cost S3's XCHG r/m8
        // and INC/DEC r/m8 rows their whole value until it was found.
        //
        // An unwatched write FALLS THROUGH to the body, and the `&&` below rather than a nested
        // early return is what makes that true. It shipped as a nested `return false`, which the
        // paragraph after it already claimed was a fall-through: the two disagreed and the code
        // won, so a store made inside a window skipped the unit-sim feed, the SMC trace and the
        // smc-census choke for as long as the window was open. None of those three invalidates
        // anything, so nothing was unsound; they are DIAGNOSTICS, and a diagnostic that silently
        // stops observing during exactly the mechanism under measurement is worse than one that
        // never ran.
        //
        // Falling through is SOUND for an unwatched write: the body below invalidates only what
        // `range_hits_compiled_code` and `decode_cache::range_hits_code` name, and
        // `code_write_watched` IS the disjunction of those two, so a write that fails it reaches
        // no invalidation door and cannot retire the running block. It returns `invalidated`,
        // which is `false` on that path, having invalidated nothing.
        //
        // THE SIZED PATH PROBES TWICE while a window is open, and that is accepted rather than
        // threaded. `write_linear_fragment` and `finish_fast_map_write` both compute `watched`
        // for their own same-value elision and then call in here, which asks again. Threading
        // the answer through would mean a second entry point: the BYTE door
        // (`note_code_byte_write_hit`) genuinely does not know -- that is the whole reason this
        // probe exists -- so a single `watched: bool` parameter would force the byte path to
        // compute one on every changed store, including the vast majority that never see a
        // window. That trades a cost confined to one probe per InterpretOne store for a cost on
        // the shipped byte-store path, which is the wrong direction. Revisit only if the loader
        // ladder puts `code_write_watched` in the profile.
        // `jit`-gated with the field it reads (`lib.rs:2414-2415`). Without the backend there are
        // no native blocks, so no call-out window can be open and this branch is dead -- but it
        // was not gated, and `cargo check --no-default-features` did not compile because of it.
        #[cfg(feature = "jit")]
        if self.deferred_code_writes.is_open() && self.code_write_watched(physical, width) {
            self.record_deferred_code_write(physical, width, lanes);
            return true;
        }
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
        // SMC trace (diagnostic, off by default). The gate is HERE, at the call site: with the
        // trace disabled the slot is `None` and neither the decode-line probe nor the action
        // record is built, so the invalidation path is byte-identical to an untraced build. See
        // `smc_trace` and the trace-off probe pinned by the campaign protocol.
        let traced = self.smc_trace.0.is_some().then(|| {
            crate::smc_trace::SmcTracePre::new(
                self.decode_cache.covering_line(physical),
                self.perf.instructions,
            )
        });
        let mut action = crate::smc_trace::SmcTraceAction::default();
        let mut invalidated = false;
        // G1: heat is incremented only on a byte-precise ACTUAL invalidation (a killed compiled
        // block or a narrow decode kill), never on the coarse global-flush fallback and never when
        // the write hit no code. That precision dissolves the 16-byte false-demotion concern: a
        // data byte sharing a chunk with cold code kills nothing, so it never heats the chunk.
        let mut heat_hit = false;
        // Whether this write was a pure immediate patch: some live block claimed it as a lane and
        // no block died. That is the case whose heat contribution is dropped below.
        #[cfg(feature = "jit")]
        let mut lane_only = false;
        // The census window clock (design §7's plumbing rule): `BlockCache` cannot reach
        // `PerfCounters`, so the choke stashes the retired-instruction count here. One
        // `cfg`-gated statement, no signature change anywhere below it.
        #[cfg(feature = "smc-census")]
        self.jit_direct.smc_census_set_clock(self.perf.instructions);
        #[cfg(feature = "smc-census")]
        let mut census_block_scan = false;
        #[cfg(feature = "jit")]
        if self.jit_direct.range_hits_compiled_code(physical, width) {
            #[cfg(feature = "smc-census")]
            {
                census_block_scan = true;
            }
            // The demoted-call-out map. See `BlockCache::forget_demoted_sites_in`, and the note in
            // the decode branch below for why it is called from BOTH doors and from neither the
            // top of this function nor `invalidate_physical_range`.
            self.jit_direct.forget_demoted_sites_in(physical, width);
            let outcome = self
                .jit_direct
                .invalidate_physical_range(physical, width, lanes);
            action.blocks_killed = outcome.blocks as u32;
            invalidated = outcome.blocks != 0;
            heat_hit |= outcome.blocks != 0;
            lane_only = outcome.lane_accepts != 0 && outcome.blocks == 0;
            self.perf.smc_lane_accepts += u64::from(outcome.lane_accepts);
            self.perf.smc_scan_calls += 1;
            self.perf.smc_scan_keys += u64::from(outcome.keys_scanned);
            self.perf.smc_lane_reject_width += u64::from(outcome.lane_reject_width);
            self.perf.smc_lane_reject_address += u64::from(outcome.lane_reject_address);
        }
        #[cfg(not(feature = "jit"))]
        let _ = lanes;
        // The CR3 code-cache gate's write watch (finding F1): a store into a page-directory or
        // page-table page a walk has read forces the same wholesale retire an SMC store into
        // unmarked code never gets, because the ring's soundness rests on the invariant that NO
        // live decode line survives an edit to the structures its translation depended on. A page
        // table holds no code, so this can never overlap the `range_hits_code` branch below on a
        // real guest. `clear_translation_marks: false` -- this write did not flush the TLB before
        // T2, so clearing the bitmap here would leave a future TLB-hit translation's line
        // unprotected (F11); the marks stay set, which only costs a future retire that would have
        // fired anyway. `retire_ring`'s call inside DOES retire both `Tlb` generations now (design
        // T2, review D2): this arm renumbers ring slots exactly like every other wholesale cause,
        // and the TLB's per-slot generations must retire with it or a stale generation would
        // survive under the wrong directory.
        if self
            .decode_cache
            .range_hits_translation_page(physical, width)
        {
            invalidated = true;
            action.wholesale = true;
            self.perf.translation_page_writes += 1;
            self.perf.code_invalidations += 1;
            let retired = self.decode_cache.invalidate_and_clear_code_marks(false);
            self.tlb.retire_all_slots(retired);
            self.has_aperture_code.0 = false;
            #[cfg(feature = "jit")]
            self.jit_direct.invalidate_translation();
            self.fetch_page.invalidate();
        }
        if self.decode_cache.range_hits_code(physical, width) {
            invalidated = true;
            // The demoted-call-out map, at the second of the two doors. It has to be BOTH because
            // it outlives blocks: a demotion retires its block, so an overwrite that arrives later
            // often reaches only this one -- the interpreter is still running that instruction, so
            // its decode line is live even though its block is gone.
            //
            // And it has to be HERE and not at the top of this function, which is where it was
            // written first: that is the door EVERY changed byte store takes, watched or not, and
            // a `retain` over the map's sixty entries on each of several million stores is not a
            // probe, it is a scan. MEASURED on the tombraid loader, four interleaved pairs of the
            // two placements, min wall 8.075 s against 7.007 s -- 1.15x, on a host loaded enough
            // that both arms ran a second above their quiet-host figures, which interleaving is
            // what controls for. Both branches it now sits in have already established that the
            // write touches code, which is a tiny fraction of stores.
            //
            // What that trades away, said plainly: a write that hits neither a compiled block nor
            // a decode line leaves the site stale. That is the same population the block cache
            // itself never hears about, so the map is exactly as current as `entries` is, and the
            // residue is one missed lowering at one address until the next wipe.
            #[cfg(feature = "jit")]
            self.jit_direct.forget_demoted_sites_in(physical, width);
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
                    action.narrow_kills = kills;
                    self.perf.smc_narrow_kills += u64::from(kills);
                    if kills > 0 {
                        self.perf.code_invalidations += 1;
                        heat_hit = true;
                    }
                }
                None => {
                    action.wholesale = true;
                    self.perf.decode_inval_smc += 1;
                    self.perf.code_invalidations += 1;
                    // `false`: an SMC store does not flush the TLB, so `translation_pages` must
                    // stay set (F11) -- the ring's slots are still retired below via this same
                    // call, only the bitmap survives. SMC has no bearing on any linear->physical
                    // map, but `retire_ring` renumbers ring slots regardless of cause (design T2,
                    // review D2), so the `Tlb`'s per-slot generations must retire here too or a
                    // stale generation would survive under the wrong directory after the next
                    // `MOV CR3` reoccupies this slot.
                    let retired = self.decode_cache.invalidate_and_clear_code_marks(false);
                    self.tlb.retire_all_slots(retired);
                    // The second of the two generation-bump call sites; same clearing rule as
                    // invalidate_decode_frontend, or an SMC wholesale flush would leave the
                    // aperture flag armed with no line behind it.
                    self.has_aperture_code.0 = false;
                    #[cfg(feature = "jit")]
                    self.jit_direct.invalidate_translation();
                }
            }
            // The fetch-page snapshot may hold the written bytes under either outcome.
            self.fetch_page.invalidate();
        }
        // The demotion channel is the campaign's actual payoff. A lane patch still kills the
        // narrow decode line (the interpreter's cached decode of that instruction carries a stale
        // immediate, so killing the line keeps the interpreter trivially correct), and that kill
        // is what sets `heat_hit`. Charging heat for it would leave the four Doom patch sites
        // driving the same chunk-hot crossings and the same `smc_heat_demotions` that refuse
        // Direct admission for the renderer loops, and the lane would buy nothing.
        #[cfg(feature = "jit")]
        if heat_hit && !lane_only {
            let epoch = self.smc_heat_epoch();
            self.sync_smc_heat();
            let newly_hot = self.jit_direct.smc_heat.bump(physical, width, epoch);
            action.newly_hot = newly_hot;
            self.perf.smc_heat_chunks_hot += u64::from(newly_hot);
        }
        #[cfg(not(feature = "jit"))]
        let _ = heat_hit;
        // The 2x2 of design §4, placed where `action` is readable and UNCONDITIONAL (the heat
        // block above it is inside `heat_hit && !lane_only`). Duke's scan calls and its narrow
        // kills do not share a denominator; this is the only licensed way to relate them.
        #[cfg(feature = "smc-census")]
        self.jit_direct.note_smc_census_choke(
            census_block_scan,
            action.narrow_kills != 0,
            action.wholesale,
        );
        if let Some(pre) = traced
            && let Some(trace) = self.smc_trace.0.as_mut()
        {
            trace.record(physical, width, pre, action);
        }
        invalidated
    }

    /// Record one code write taken while an `InterpretOne` call-out held a native block live.
    /// Separate from the branch above and `#[cold]` so the open-window case costs the choke one
    /// not-taken branch and no register pressure.
    #[cfg(feature = "jit")]
    #[cold]
    #[inline(never)]
    fn record_deferred_code_write(&mut self, physical: u32, width: u32, lanes: bool) {
        self.jit_direct.note_deferred_code_write();
        self.deferred_code_writes.push(crate::DeferredCodeWrite {
            physical,
            width,
            lanes,
        });
    }

    /// Replay every write the call-out window deferred, with the window CLOSED, and clear the
    /// list. Called by `run_direct_block` once per native entry, after the fetch accounting and
    /// before it returns.
    ///
    /// Exact for the recorded entries: each replays the same `(physical, width)` through the same
    /// door (`lanes` picks the value-aware one) that the interpreter's store path chose, so the
    /// lane-absorption decision and every SMC counter land where they would have landed.
    ///
    /// The overflow arm is coarse and unreachable in practice. `MAX_DEFERRED_CODE_WRITES` is
    /// sized for one instruction's stores plus a whole exception delivery's pushes and page-walk
    /// accessed-bit writes; past that the only sound answer is to drop every compiled block and
    /// every decode line, which is what it does. It deliberately does NOT consult the write-page
    /// record: that record is settled at the end of the step (`settle_write_record`), so by the
    /// time this runs it names nothing, and making the drain depend on it would have coupled two
    /// mechanisms for a path that cannot be reached.
    #[cfg(feature = "jit")]
    pub(super) fn drain_deferred_code_writes(&mut self) {
        if self.deferred_code_writes.is_empty() {
            return;
        }
        let deferred = core::mem::take(&mut self.deferred_code_writes);
        for index in 0..usize::from(deferred.count) {
            let write = deferred.entries[index];
            if write.lanes {
                self.note_code_write_hit(write.physical, write.width);
            } else {
                self.note_code_write(write.physical, write.width);
            }
        }
        if deferred.overflow {
            self.jit_direct.clear();
            self.invalidate_code_caches();
        }
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

    /// Reset coupling for the hoisted heat map: heat drops exactly when the ACTIVE backend's
    /// cache resets its storage. The cache signals resets through a counter (it cannot reach the
    /// map itself; internal resets fire from inside `probe`/`install`), and every heat access
    /// synchronizes here first, so the observable lifetime is identical to the map living inside
    /// the cache. Single-threaded by design: plain fields and split borrows, no Arc, no Mutex.
    #[cfg(feature = "jit")]
    pub(crate) fn sync_smc_heat(&mut self) {
        let jit = &mut *self.jit_direct;
        let resets = jit.direct.heat_resets();
        jit.smc_heat.sync_resets(resets);
    }

    pub(super) fn begin_instruction(&mut self) {
        #[cfg(feature = "int-trace")]
        if crate::int_trace::armed() {
            crate::int_trace::on_instruction(
                self.registers.cs().selector,
                self.registers.eip,
                self.pushad_image(),
                self.flag(FLAG_CF),
            );
        }
        self.settle_write_record();
    }

    /// Act on the write record the instruction just finished left, then clear it.
    ///
    /// TWO CALLERS, and the second is why this is a function. The interpreter reaches it through
    /// `begin_instruction`, once per instruction, which is where it has always lived. An
    /// `InterpretOne` call-out reaches it directly, at the END of its step, because the slot after
    /// it is NATIVE and will never call `begin_instruction` -- so without this the record would
    /// accumulate across the rest of the block and the prefetch invalidation below would run at
    /// the first interpreted instruction AFTER the block instead of at the next slot.
    ///
    /// The prefetch half is not bookkeeping. A 486 prefetch queue is a SNAPSHOT: writes to bytes
    /// already fetched are not observed until control flow or a refill drops the queue, so a
    /// call-out that patches the bytes ahead of it must invalidate here or the interpreter fetches
    /// what the guest just overwrote.
    #[inline]
    pub(super) fn settle_write_record(&mut self) {
        if self.written_pages_overflow {
            self.prefetch.invalidate();
        } else if self.written_count > 0
            && let Some(code_page) = self.prefetch.physical_page()
            && self.written_pages.contains(&Some(code_page))
        {
            self.prefetch.invalidate();
        }
        // Clear only the slots that were actually written this instruction (most
        // instructions are register-only and written_count is 0, so this is a no-op
        // instead of an unconditional 64-byte memset).
        for i in 0..self.written_count as usize {
            self.written_pages[i] = None;
        }
        self.written_count = 0;
        self.written_pages_overflow = false;
        self.last_written_page = NO_LAST_WRITTEN_PAGE;
    }

    pub fn is_protected_mode(&self) -> bool {
        self.control.cr0 & CR0_PE != 0
    }

    /// Whether implicit stack references (PUSH/POP/CALL/RET/interrupts) use the
    /// full 32-bit ESP or wrap within the 16-bit SP, per the loaded SS descriptor's
    /// B bit (386 PRM 16.2: "the size of stack pointer ... used by the processor
    /// for implicit stack references" is controlled by SS.B, independent of
    /// operand size, gate size, or CS's D bit). Real mode and V86 always load SS
    /// through `load_segment_real`/`load_segment_real_mode`, both of which stamp
    /// `default_size_32: false` -- the real-mode load preserves the cached LIMIT
    /// (unreal mode) but deliberately not the B bit -- so THIS query reads 16-bit
    /// in every mode without a mode test.
    ///
    /// That is a statement about SS here, not about the field generally:
    /// `default_size_32` is also read off CS for the decode default size
    /// (`emit_input.rs`'s address/operand-size derivation, the JIT mode key, the
    /// decode-cache key), and off any segment for the protected-mode expand-down
    /// ceiling. Those readers are unaffected because CS re-canonicalizes on every
    /// real-mode load and the expand-down arm is gated on protected-and-not-V86.
    ///
    /// Backed by the cached `SegmentRegister.default_size_32` field
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
    ///
    /// Written as three INDEPENDENT tests rather than `is_protected_mode() && !is_v86_mode()
    /// && ...`, because `is_v86_mode()` re-tests CR0.PE itself: that form asks
    /// `PE && !(PE && VM) && cpl == 0`, which is the same predicate with one redundant CR0 load
    /// and branch. `finish_instruction` asks this on every retired instruction (V86-monitor
    /// residency attribution), so the redundancy is per-instruction. Identical by boolean
    /// algebra: `PE && (!PE || !VM) == PE && !VM`.
    #[inline]
    pub fn is_ring0_protected(&self) -> bool {
        self.is_protected_mode()
            && self.registers.eflags & FLAG_VM == 0
            && self.current_privilege_level() == 0
    }

    /// The CPU mode/size bitmask a compiled JIT block is keyed by (spec §2.2): a block compiled
    /// for one mode must never be reused in another at the same phys/d. Packs the CS operand-size
    /// default (D bit), protected mode (CR0.PE), V86, the SS stack big bit (B), and the GSW mode.
    /// A mode change already invalidates the decode cache (`set_mode`), but it is folded in here
    /// too so the key is self-contained.
    ///
    /// Validated at the PROBE, not at the entry. `BlockKey` carries this value and
    /// `BlockCache::probe` matches the whole key, so a block only ever reaches an entry that
    /// already agreed on it. `run_direct_block` used to re-derive the key and compare a second
    /// time; audit item 2.4 deleted that compare and left a `debug_assert_eq!` in its place,
    /// because nothing between the probe and the entry executes a guest instruction and so none
    /// of the bits above can move in between. `jit_direct_reject_mode_key` survives as a
    /// permanently zero counter.
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
        self.invalidate_code_caches();
        // `fast_map_population_enabled()` (memory.rs) depends on `mode()`: a live switch INTO an
        // Accurate persona must stop the interpreter's FastMap serve path from probing at all, not
        // merely stop it from populating further. Without this refresh, `fast_map_serve_enabled`
        // stays stuck at whatever it was on the PREVIOUS persona -- on a switch away from an
        // Approximate persona this both reinstates the guaranteed-miss preamble cost the serve
        // gate exists to avoid, and lets already-live FastMap entries keep serving under a persona
        // that must never take this path (both found by adversarial review of the lever-1 slice).
        #[cfg(all(
            feature = "jit",
            target_arch = "x86_64",
            any(target_os = "windows", target_os = "linux")
        ))]
        self.refresh_fast_map_serve_gate();
        // The persona half of `key_for_phys`'s host/persona screen just moved. This is the only
        // writer of `mode` outside `Default`, so this one call is the whole invalidation
        // contract for `JitState::native_keys_admitted`.
        #[cfg(feature = "jit")]
        self.refresh_native_key_admission();
    }

    /// Recompute the cached `native_keys_admitted` mirror of
    /// `jit::direct::native_keys_admitted(self.mode)`. Call it from every site that can change
    /// that predicate's inputs; since `host_supported()` is process-constant, that inventory is
    /// exactly `Default` (initial seed) and `set_mode` (persona). `key_for_phys`'s
    /// `debug_assert` catches a site that grows later and forgets.
    #[cfg(feature = "jit")]
    pub(crate) fn refresh_native_key_admission(&mut self) {
        self.jit_direct.native_keys_admitted = crate::jit::direct::native_keys_admitted(self.mode);
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
        let persona = self.persona();
        let (num, den) = level_timing(persona);
        let scaled = clocks * u64::from(num) + self.timing_rem;
        // Specialized per persona for the same reason `scale_clocks` above is: a runtime `den`
        // from the `level_timing` match emits a real hardware div, while a compile-time constant
        // strength-reduces to a magic-multiplier multiply-shift. Quotient first, remainder as
        // `scaled - quot * den`, so this is one div plus a mul and a sub rather than two divs.
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

    /// `scale_clocks_batch` WITHOUT the side effect: the same long division over the same
    /// `timing_rem` carry, but the new remainder is discarded instead of stored.
    ///
    /// Exists for exactly one caller, the JIT's interpreter call-out slot
    /// (`jit/direct/callout.rs`), which needs the scaled value of a block prefix that the block
    /// has NOT retired yet. The prefix is charged once, later, by `run_direct_block`'s single
    /// batch call; consuming the carry here would move that charge and make the batch inexact.
    /// Reading it without consuming it is what lets a mid-block port read hand the device the
    /// same guest-time offset an interpreted continuation would.
    #[cfg(feature = "jit")]
    pub(super) fn preview_scale_clocks(&self, clocks: u64) -> u64 {
        let persona = self.persona();
        let scaled = clocks.saturating_mul(u64::from(level_timing(persona).0)) + self.timing_rem;
        match persona {
            CpuPersona::I386 => scaled / 5u64,
            CpuPersona::I486 | CpuPersona::I586 => scaled / 12u64,
        }
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

    /// Arm or disarm the per-window direct-entry target tally that backs the v2 windowed IPE
    /// trace. Armed by `Machine::arm_ipe_window_trace` and by nothing else.
    ///
    /// Read `crate::ipe_entry_tally`'s module comment before arming this from anywhere new: the
    /// armed leg pays a hash-map probe on the backend's hottest path, so an armed run maps the
    /// workload and must never be used as a wall measurement of it. Disarming frees the map.
    ///
    /// The signature is UNCONDITIONAL and the body is `jit`-gated, the shape
    /// `note_direct_map_changed` uses: `jit_direct` does not exist in a `--no-default-features`
    /// build, and the machine calls this from code that is not itself gated. Without the JIT
    /// there are no direct entries to tally, so the jit-off leg is correctly a no-op rather than
    /// a missing feature.
    pub fn set_ipe_entry_targets_armed(&mut self, armed: bool) {
        #[cfg(feature = "jit")]
        {
            self.jit_direct.ipe_entry_targets = if armed { Some(Box::default()) } else { None };
        }
        let _ = armed;
    }

    /// The current window's entry targets, or `None` when disarmed. Does NOT clear the tally, so
    /// the still-open trailing window can be read after the run returns. Always `None` without
    /// the `jit` feature, which is the same answer "disarmed" gives.
    pub fn ipe_entry_targets(&self, top_n: usize) -> Option<crate::IpeEntryTargets> {
        self.ipe_entry_targets_inner(top_n)
    }

    /// The `jit` arm of `ipe_entry_targets`. Split into a gated pair of private functions rather
    /// than a gated block INSIDE one function so that neither leg needs an early `return`, which
    /// `clippy::needless_return` rejects on the arm where it is the tail.
    #[cfg(feature = "jit")]
    fn ipe_entry_targets_inner(&self, top_n: usize) -> Option<crate::IpeEntryTargets> {
        self.jit_direct
            .ipe_entry_targets
            .as_ref()
            .map(|tally| tally.snapshot(top_n))
    }

    #[cfg(not(feature = "jit"))]
    fn ipe_entry_targets_inner(&self, _top_n: usize) -> Option<crate::IpeEntryTargets> {
        None
    }

    /// Start the next entry-target window. No-op when disarmed, and no-op without the JIT.
    pub fn reset_ipe_entry_targets(&mut self) {
        #[cfg(feature = "jit")]
        if let Some(tally) = self.jit_direct.ipe_entry_targets.as_mut() {
            tally.reset();
        }
    }

    /// Where the last fatal `CpuError` was raised. Read this ONLY when the run
    /// actually stopped on one: nothing clears it, and a fatal error leaves the
    /// machine resumable, so on any other stop it describes an older fault.
    pub fn fault_site(&self) -> Option<FaultSiteRecord> {
        self.fault_site.0
    }

    /// Record the raise site of a fatal `CpuError`. `start_eip` is the faulting
    /// instruction's first byte; CS is taken live because `finish_instruction`
    /// only receives a bare selector and widening that `#[inline]` signature to
    /// carry a 16-byte descriptor would cost the retire path for a cold
    /// diagnostic.
    ///
    /// `cs_moved` is passed in rather than derived here. Deriving it by
    /// comparing selectors is wrong on the exception arm, where the rewind has
    /// already reloaded CS and made them match while leaving a fabricated
    /// real-mode base behind: the caller is the only place that still knows.
    ///
    /// Cold and never inlined. `finish_instruction` is `#[inline]` with six call
    /// sites, one of them the retire path of every straight-line run, and this
    /// codebase has a documented layout and code-growth sensitivity there.
    #[cold]
    #[inline(never)]
    pub(crate) fn record_fault_site(&mut self, start_eip: u32, cs_moved: bool) {
        self.fault_site = FaultSite(Some(FaultSiteRecord {
            cs: self.registers.cs(),
            eip: start_eip,
            cs_moved,
        }));
    }

    /// Lever 1 (interpreter FastMap serve path) hit/miss counters, stored outside
    /// `PerfCounters` at the `CpuGsw` tail (see `FastMapProbeCounters` for why). Reset
    /// alongside the other counters by `reset_perf_counters`.
    pub fn fast_map_probe_counters(&self) -> FastMapProbeCounters {
        self.fast_map_probe
    }

    /// N5 audit instrument: whole-map wipe causes plus the env-gated read/write/RMW shape
    /// census. The counters live behind `JitState` rather than on `CpuGsw` so the instrument
    /// costs the interpreter's pinned hot-field layout nothing; see `JitState::fast_map_audit`
    /// for the measurement that forced that. Reset by `reset_perf_counters` like the others.
    pub fn fast_map_audit_counters(&self) -> FastMapAuditCounters {
        #[cfg(feature = "jit")]
        let counters = *self.jit_direct.fast_map_audit;
        #[cfg(not(feature = "jit"))]
        let counters = FastMapAuditCounters::default();
        FastMapAuditCounters {
            // The live gate is the bare `CpuGsw` byte, not the stored copy; report that one so a
            // reader of the JSON cannot mistake an unarmed run for an armed one that saw nothing.
            census_enabled: self.rmw_census_enabled,
            // Same rule for the slot-reject half: report the LIVE gate byte, so five zero reject
            // counters can be read as "the instrument was off", never as "nothing was refused".
            slot_reject_enabled: self.slot_census_enabled,
            ..counters
        }
    }

    /// Whether EITHER code-watch table holds an entry for this physical address's page — the
    /// value of the fast-map `PAGE_WATCHED` bit at fill time (watched-page-bit design D2).
    /// Exists under every cfg (the interpreter fast map fills on all builds); reads false where
    /// a watch type is compiled out.
    pub(crate) fn physical_page_watched(&self, physical: u32) -> bool {
        let page = physical >> 12;
        #[allow(unused_variables)]
        let watched = false;
        #[cfg(feature = "jit")]
        let watched = watched || self.jit_direct.code_watch_page_is_watched(page);
        #[cfg(all(
            feature = "jit",
            target_arch = "x86_64",
            any(target_os = "windows", target_os = "linux")
        ))]
        let watched = watched || self.decode_cache.code_watch_page_is_watched(page);
        // Finding F2: without this, a native emitted store bypasses the invalidation door
        // entirely (it goes straight to the host pointer unless the FastMap entry's PAGE_WATCHED
        // bit, stamped from this function, says otherwise), so a page that just became a page
        // table would keep taking native stores unobserved.
        watched || self.decode_cache.translation_page_is_watched(page)
    }

    /// Mark `physical`'s page as page-directory/page-table structure a walk just read (design
    /// part 2(b)/finding F11), called unconditionally from `translate_linear_checked` on EVERY
    /// walk. When this is the page's unwatched -> watched edge, sweep it through the FastMap
    /// synchronously, in the shape of `sweep_sticky_watch_edges` (finding F2): a native block's
    /// interpreter call-out can decode mid-block, so a bit-clear FastMap entry for a page that
    /// just became watched must not survive to the next native store.
    #[inline]
    pub(super) fn mark_translation_page(&mut self, physical: u32) {
        if !self.decode_cache.mark_translation_page(physical) {
            return;
        }
        self.perf.translation_pages_marked = self.decode_cache.translation_pages_marked;
        #[cfg(all(
            feature = "jit",
            target_arch = "x86_64",
            any(target_os = "windows", target_os = "linux")
        ))]
        self.jit_fast_map
            .clear_unwatched_entries_of_physical_page(physical & 0xffff_f000);
    }

    /// Drain and apply the sticky watch's strict E1 edges (watched-page-bit design D4): every
    /// physical page that just crossed unwatched -> watched has its bit-clear fast-map entries
    /// invalidated BEFORE native code can run again. Called immediately after the decode-cache
    /// insert choke; a native block's interpreter callout can decode mid-block, so this must be
    /// synchronous with the mark, not batched to a dispatch boundary.
    #[cfg(all(
        feature = "jit",
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    pub(crate) fn sweep_sticky_watch_edges(&mut self) {
        // Asked BEFORE the take. This runs at every native entry and is a no-op read there, and
        // `mem::take` writes three words into the live `Vec` whichever way the answer goes.
        if !self.decode_cache.has_pending_watch_edges() {
            return;
        }
        let pages = self.decode_cache.take_watch_edge_pages();
        let mut cleared = 0;
        for page in pages {
            cleared += self
                .jit_fast_map
                .clear_unwatched_entries_of_physical_page(page << 12);
        }
        self.decode_cache.note_watch_sweep_cleared(cleared);
    }

    /// The block-watch twin (E2), drained after `JitState::install` / `reject`.
    #[cfg(feature = "jit")]
    pub(crate) fn sweep_block_watch_edges(&mut self) {
        // See `sweep_sticky_watch_edges`: the emptiness question is asked before the take.
        if !self.jit_direct.has_pending_watch_edges() {
            return;
        }
        let pages = self.jit_direct.take_watch_edge_pages();
        let mut cleared = 0;
        for page in pages {
            cleared += self
                .jit_fast_map
                .clear_unwatched_entries_of_physical_page(page << 12);
        }
        self.jit_direct.note_watch_sweep_cleared(cleared);
    }

    /// Slice 0 of the watched-page-bit design (`dev_docs/2026-08-06-watched-page-bit-design.md`
    /// D6): unwatched <-> watched page-edge rates for both code-watch tables — the design's
    /// go/no-go instrument. Counters live on the watch types themselves so nothing on the store
    /// or interpreter hot paths moves; collection here is a cold per-run read.
    pub fn code_watch_edge_counters(&self) -> CodeWatchEdgeCounters {
        #[allow(unused_mut)]
        let mut counters = CodeWatchEdgeCounters::default();
        #[cfg(all(
            feature = "jit",
            target_arch = "x86_64",
            any(target_os = "windows", target_os = "linux")
        ))]
        {
            counters.sticky_page_edges = self.decode_cache.code_watch_page_edges();
            counters.sweep_cleared_entries += self.decode_cache.code_watch_sweep_cleared();
        }
        #[cfg(feature = "jit")]
        {
            counters.block_page_edges = self.jit_direct.code_watch_page_edges();
            counters.block_page_releases = self.jit_direct.code_watch_page_releases();
            counters.sweep_cleared_entries += self.jit_direct.code_watch_sweep_cleared();
        }
        counters
    }

    /// Test-only sticky mark that routes through the SAME edge choke production uses — mark
    /// plus synchronous E1 sweep (design H7). Fixtures must use this, never
    /// `decode_cache.mark_code_range` directly: a bare mark leaves the edge pending, which the
    /// next mark's debug assertion catches, and a fixture that never swept would exercise a
    /// coherence path production does not have.
    #[cfg(all(
        test,
        feature = "jit",
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    pub(crate) fn mark_decode_code_for_test(&mut self, physical: u32, len: u8) {
        self.decode_cache.mark_code_range(physical, len);
        self.sweep_sticky_watch_edges();
    }

    /// The block-watch twin of `mark_decode_code_for_test` (design H7).
    #[cfg(all(test, feature = "jit"))]
    pub(crate) fn mark_block_code_for_test(&mut self, physical: u32, len: u8) {
        self.jit_direct.mark_code_range(physical, len);
        self.sweep_block_watch_edges();
    }

    /// Zero the host-side performance counters, including the memory-poll
    /// subset stored outside `PerfCounters` (see `PollSkipMemoryCounters`).
    pub fn reset_perf_counters(&mut self) {
        self.perf = PerfCounters::default();
        self.poll_skip_memory = PollSkipMemoryCounters::default();
        self.fast_map_probe = FastMapProbeCounters::default();
        #[cfg(feature = "jit")]
        {
            *self.jit_direct.fast_map_audit = FastMapAuditCounters::default();
            self.jit_direct.reset_code_watch_edge_counters();
        }
        #[cfg(all(
            feature = "jit",
            target_arch = "x86_64",
            any(target_os = "windows", target_os = "linux")
        ))]
        {
            self.decode_cache.reset_code_watch_edge_counter();
        }
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

    /// `timing_rem`, ungated by `feature = "jit"` (unlike
    /// `poll_skip_timing_remainder` above): the reflected-call memo's raw-clock
    /// recovery (slice1 plan Revision 2, R2.2/R2.15) is production code behind a
    /// runtime knob, not a diagnostic feature, and needs this carry sampled at a
    /// trip's open and close regardless of which cargo features are compiled in.
    pub(crate) fn reflected_call_timing_rem(&self) -> u64 {
        self.timing_rem
    }

    /// `core_clocks_so_far`, exposed for the GP2 poll-skip seam's `izarravm-machine` fixtures,
    /// which build the same `CalloutPollSkipRequest` the Direct call-out builds and need this
    /// exact term (`core_clocks_at_block_entry`) to reproduce `now_at(0)`.
    #[cfg(feature = "jit")]
    pub fn core_clocks_so_far_for_test(&self) -> u64 {
        self.core_clocks_so_far
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

    /// Whether the sampled bucket profiler is armed. The boot profiler asks
    /// before snapshotting at a phase boundary, so an unprofiled run neither
    /// pays for the clone nor reports empty per-phase tables.
    pub fn profiling_enabled(&self) -> bool {
        self.profile.is_enabled()
    }

    /// Enable or disable the host-only Direct structural-stop census.
    pub fn enable_direct_barrier_census(&mut self, enabled: bool) {
        #[cfg(feature = "jit")]
        {
            self.jit_direct.set_barrier_census_enabled(enabled);
            // The retire tail reads the mirror, not the boxed state, so this is the invalidation
            // half of `RetireGates`. Re-read rather than assign `enabled`, so the mirror is
            // whatever the state actually reports.
            self.retire_gates.barrier_census = self.jit_direct.barrier_census_active();
        }
        #[cfg(not(feature = "jit"))]
        let _ = enabled;
    }

    #[cfg(feature = "direct-link-refusal-census")]
    pub fn enable_direct_link_refusal_census(&mut self, enabled: bool) {
        self.jit_direct
            .set_direct_link_refusal_census_enabled(enabled);
    }

    #[cfg(all(test, feature = "direct-callout-attribution"))]
    pub(crate) fn enable_direct_callout_attribution_for_test(&mut self) {
        self.jit_direct.enable_direct_callout_attribution_for_test();
    }

    /// Admit `OperandSize::Word` operands to the Direct backend below I586.
    ///
    /// Seeded from `IZARRAVM_JIT16_486` at construction; this is the programmatic form the A/B
    /// and the compile-outcome tests drive, so the lifted arm has coverage rather than shipping
    /// as a path only an env var can reach.
    pub fn set_word_operands_at_486(&mut self, enabled: bool) {
        #[cfg(feature = "jit")]
        {
            self.jit_direct.word_at_486 = enabled;
        }
        #[cfg(not(feature = "jit"))]
        let _ = enabled;
    }

    /// Set the 16-bit code-segment admission level (see `sixteen_bit_admission_level`).
    /// Seeded from `IZARRAVM_JIT16`; this is the programmatic form fixtures drive.
    pub fn set_sixteen_bit_admission_level(&mut self, level: u8) {
        #[cfg(feature = "jit")]
        {
            self.jit_direct.sixteen_bit_level = level;
        }
        #[cfg(not(feature = "jit"))]
        let _ = level;
    }

    /// Always available, unlike the census: see `DirectStallSnapshot`.
    /// See `DirectPageCache::set_mapping_epoch_for_test`.
    #[cfg(test)]
    pub(crate) fn set_data_write_mapping_epoch_for_test(&mut self, epoch: u64) {
        self.data_write_pages.set_mapping_epoch_for_test(epoch);
    }

    /// Whether the SS-load shape measurement is being taken at all.
    ///
    /// GATED with the barrier census since the S4 review round. The pair of counters behind it
    /// sits in two INTERPRETER arms and is read on census legs only, which is the same bargain
    /// every other diagnostic on a hot path takes here; before the gate, `0x8e` paid a segment
    /// compare and `0x17` an unconditional record read on every execution, in every build.
    ///
    /// The measurement itself is answered: the tombraid loader phase split 484,385 same-record
    /// against 488,498 record-moving, which is what design review 10.1 M5 asked for and what the
    /// two SS call-out rows were built on. It stays because the ladder still reads it, not
    /// because anything is waiting on it.
    #[inline]
    pub(crate) fn ss_load_census_active(&self) -> bool {
        #[cfg(feature = "jit")]
        {
            self.jit_direct.barrier_census_active()
        }
        #[cfg(not(feature = "jit"))]
        {
            false
        }
    }

    /// One interpreted SS load, classified for the S4d M5 measurement by whether the record
    /// moved. `before` is the SS record as it stood at the top of the arm.
    ///
    /// Here rather than at the two call sites so the comparison is written once: the question is
    /// the same one R2 asks of the six records, and two copies of it could drift apart.
    pub(crate) fn note_ss_load_record(&mut self, before: crate::SegmentRegister) {
        #[cfg(feature = "jit")]
        {
            let same = self.registers.segment(crate::SegmentIndex::Ss) == before;
            self.jit_direct.note_ss_load(same);
        }
        #[cfg(not(feature = "jit"))]
        {
            let _ = before;
        }
    }

    pub fn direct_stall_snapshot(&self) -> crate::DirectStallSnapshot {
        #[cfg(feature = "jit")]
        {
            let mut snapshot = self.jit_direct.stall_snapshot();
            // L8's far-CALL ledger is a `CpuGsw` cell written by emitted code through R15, not a
            // JIT-frame lane folded in per exit -- see `jit::direct::far_call_ledger_offset`. It
            // is monotonic, so reading it here IS the ledger; nothing drains it.
            snapshot.far_call_native = self.far_call_ledger.0;
            snapshot
        }
        #[cfg(not(feature = "jit"))]
        {
            crate::DirectStallSnapshot::default()
        }
    }

    /// Drive one `dormant_heat` exit into the census from OUTSIDE this crate.
    ///
    /// Exists for the `izarravm` crate's JSON-schema fixtures, which own the reporting surface and
    /// cannot reach `JitState::note_unbound_target` (it is `pub(crate)`). Without it the only
    /// dormant-heat JSON a downstream test can produce is the zero snapshot, and a zero snapshot
    /// cannot tell a carried site list from a hard-coded empty one.
    #[cfg(all(feature = "jit", feature = "barrier-census-closure"))]
    pub fn note_dormant_heat_exit_for_test(&mut self, linear: u32, dynamic: bool) {
        self.note_unbound_exit_for_test(
            crate::jit::direct::UnboundTarget::DormantHeat,
            linear,
            dynamic,
        );
    }

    /// The `Rejected` twin, for the same reason and the same downstream fixtures.
    #[cfg(all(feature = "jit", feature = "barrier-census-closure"))]
    pub fn note_rejected_exit_for_test(&mut self, linear: u32, dynamic: bool) {
        self.note_unbound_exit_for_test(
            crate::jit::direct::UnboundTarget::Rejected,
            linear,
            dynamic,
        );
    }

    #[cfg(all(feature = "jit", feature = "barrier-census-closure"))]
    fn note_unbound_exit_for_test(
        &mut self,
        kind: crate::jit::direct::UnboundTarget,
        linear: u32,
        dynamic: bool,
    ) {
        if dynamic {
            self.jit_direct.note_dynamic_miss_target(kind, linear);
        } else {
            // No key: this seam names a class and an address, and the per-cause split of
            // `dormant_other` is about a key the caller does not have. It reads as zero here,
            // which is the honest answer for a synthesised exit.
            self.jit_direct.note_unbound_target(kind, linear, None);
        }
    }

    /// The entry-attribution observer's snapshot, or `None` when it was never armed.
    ///
    /// The tally is THREAD-LOCAL, so this must be called on the thread that ran the guest. The
    /// headless runner the design's protocol uses drives the machine and writes the profile JSON
    /// on one thread, which is the only configuration this instrument is claimed for; a caller on
    /// another thread gets an all-zero snapshot rather than a merged one, and `marks` reading zero
    /// against a non-zero `jit_direct_entries` is what makes that visible rather than silent.
    ///
    /// It hangs off `CpuGsw` for the same reason the census snapshot does: this is the seam that
    /// owns both the instrument and `PerfCounters`, and the exporter receives a snapshot and no
    /// CPU.
    #[cfg(all(feature = "jit", feature = "direct-entry-attribution"))]
    pub fn direct_entry_attribution_snapshot(
        &self,
    ) -> Option<crate::DirectEntryAttributionSnapshot> {
        crate::jit::direct::snapshot()
    }

    /// The census snapshot, JOINED with the two perf counters its classes are designed to close
    /// on. This is the only seam where that join can happen: the census lives on `JitState` and
    /// cannot see `PerfCounters`, and `direct_barrier_census_json` receives a snapshot and no CPU.
    /// `CpuGsw` owns both.
    pub fn direct_barrier_census_snapshot(&self) -> Option<DirectBarrierCensusSnapshot> {
        #[cfg(feature = "jit")]
        {
            #[allow(unused_mut)]
            let mut snapshot = self.jit_direct.barrier_census_snapshot()?;
            #[cfg(feature = "barrier-census-closure")]
            {
                snapshot.static_unbound_exits = self.perf.jit_direct_unresolved_static_unbound;
                snapshot.dynamic_miss_exits =
                    self.perf.jit_direct_unresolved_dynamic_miss_or_unbound;
            }
            Some(snapshot)
        }
        #[cfg(not(feature = "jit"))]
        {
            None
        }
    }

    #[cfg(feature = "direct-link-refusal-census")]
    pub fn direct_link_refusal_census_snapshot(&self) -> Option<DirectLinkRefusalCensusSnapshot> {
        self.jit_direct.direct_link_refusal_census_snapshot()
    }

    #[cfg(feature = "direct-callout-attribution")]
    pub fn direct_callout_attribution_snapshot(&self) -> Option<DirectCallOutAttributionSnapshot> {
        self.jit_direct.direct_callout_attribution_snapshot()
    }

    #[cfg(feature = "smc-census")]
    pub fn direct_smc_census_snapshot(&self) -> Option<crate::DirectSmcCensusSnapshot> {
        self.jit_direct.smc_census_snapshot()
    }

    /// Slice 0 of the reflected-call HLE design's trip-shape instrument
    /// (dev_docs/2026-09-03-reflected-call-hle-design.md /
    /// dev_docs/2026-09-03-reflected-call-hle-review.md). `None` when the
    /// feature is not built in or the run never armed
    /// `IZARRAVM_REFLECTED_CALL_DIAGNOSTIC`.
    #[cfg(feature = "reflected-call-diagnostic")]
    pub fn reflected_call_diagnostic_snapshot(
        &self,
    ) -> Option<crate::ReflectedCallDiagnosticSnapshot> {
        crate::reflected_call_diag::snapshot()
    }

    /// `(execution-weighted arity histogram, distinct sites)` from the THROWAWAY stage-0 RETF
    /// census, or `None` when it is disarmed. See `jit::direct::retf_census`.
    #[cfg(feature = "retf-arity-census")]
    pub fn retf_arity_snapshot(
        &self,
    ) -> Option<([u64; crate::jit::direct::RETF_TARGET_CENSUS_CAP + 2], u64)> {
        self.jit_direct.retf_arity_snapshot()
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

    /// EAX, ECX, EDX, EBX, ESP, EBP, ESI, EDI in the encoder's register order, so
    /// an index into this array is the same index the ModRM fields use.
    #[cfg(feature = "int-trace")]
    pub(super) fn pushad_image(&self) -> [u32; 8] {
        [
            self.read_gpr32(0),
            self.read_gpr32(1),
            self.read_gpr32(2),
            self.read_gpr32(3),
            self.read_gpr32(4),
            self.read_gpr32(5),
            self.read_gpr32(6),
            self.read_gpr32(7),
        ]
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

    /// CF and the width mask are read INSIDE the arms that use them, not once at the top.
    ///
    /// Only ADC (2) and SBB (3) consume the carry in, and `flag(FLAG_CF)` is not a bit test: with
    /// a pending descriptor live it materialises CF through `arith_flag`, which loads the
    /// descriptor, decodes its op and does a 64-bit compare. Hoisting that above the match made
    /// every ADD, SUB, CMP, AND, OR and XOR pay for it and throw the answer away. The mask is the
    /// same story one order of magnitude down: only the three logic arms mask here (the add and
    /// sub helpers mask for themselves).
    ///
    /// Nothing between the old read point and an arm can move CF, so the sunk read sees exactly
    /// the value the hoisted one did.
    pub(super) fn alu(&mut self, op: u8, a: u32, b: u32, width: BusWidth) -> u32 {
        match op {
            0 => self.alu_add(a, b, 0, width),
            2 => {
                let cf_in = u32::from(self.flag(FLAG_CF));
                self.alu_add(a, b, cf_in, width)
            }
            3 => {
                let cf_in = u32::from(self.flag(FLAG_CF));
                self.alu_sub(a, b, cf_in, width)
            }
            5 | 7 => self.alu_sub(a, b, 0, width),
            1 => {
                let result = (a | b) & width_mask(width);
                self.alu_logic(result, width)
            }
            4 => {
                let result = (a & b) & width_mask(width);
                self.alu_logic(result, width)
            }
            6 => {
                let result = (a ^ b) & width_mask(width);
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
        // Seeded ONLY for RCL/RCR, the two ops that read the carry IN. Every other arm assigns
        // `cf` on its first iteration and `count` is at least 1 here, so the seed those ops see is
        // dead -- and `flag(FLAG_CF)` is a full materialisation through the pending descriptor,
        // not a bit test, which is what made paying for it on every SHL/SHR/SAR/ROL/ROR worth
        // removing.
        let mut cf = matches!(op, 2 | 3) && self.flag(FLAG_CF);
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
        // AF rides live `eflags` under a Logic descriptor (see `arith_flag`'s Logic arm), so the
        // OUTGOING descriptor's AF has to be materialised into `eflags` before the Logic
        // descriptor below replaces it. That is only true of an ADD or SUB descriptor: with no
        // pending descriptor, or a Logic one already, `flag(FLAG_AF)` reads `eflags & FLAG_AF`
        // and this writes the same bit straight back, so the read-back and the read-modify-write
        // were a no-op on every logic op that followed a logic op.
        if !self.pending_flags.is_none() && self.pending_flags.op() != LazyFlagOp::Logic {
            if self.arith_flag(FLAG_AF) {
                self.registers.eflags |= FLAG_AF;
            } else {
                self.registers.eflags &= !FLAG_AF;
            }
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
}
