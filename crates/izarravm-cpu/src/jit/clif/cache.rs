// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Track C C1a: the clif unit cache and the unit-boundary growth walker. A parallel type to
//! `jit::direct::BlockCache` (plan decision D-C1.1), reusing Direct's DESIGN: the same key
//! fields (section 2.1), the same static exclusions K1-K5 (section 2.2), and the same
//! classifier for unit growth (F-A5), while keeping clif's compile path decoupled from
//! Direct's emission internals. Since C1b a unit executes its leading run of
//! register/immediate slots natively (jit/clif/lower.rs) with x87 slots as call-outs, and
//! side-exits at the first non-lowered slot, which the interpreter retires.

use std::collections::HashMap;

use super::super::code_watch::NativeCodeWatch;
use super::super::direct::{
    self, DirectKind, MAX_BLOCK_INSTRUCTIONS, SegmentLayout, SmcHeatMap, UnitTerminal,
};
use crate::{CpuGsw, CpuPersona, OperandSize, Prefixes, U32BuildHasher};

/// The clif hotness threshold (P4): an independent per-decode-line counter at Direct's
/// `admission_heat` defaults (plan section 2.4, row P4), so the two backends compile
/// independently under a runtime policy switch (decision D-C1.4) without sharing a counter.
#[cfg(not(test))]
pub(crate) const CLIF_DEFAULT_ADMISSION_HEAT: u8 = 8;
#[cfg(test)]
pub(crate) const CLIF_DEFAULT_ADMISSION_HEAT: u8 = 1;

/// The clif unit key: the same three fields as `jit::direct::BlockKey` with identical
/// semantics (plan section 2.1), as a parallel type per decision D-C1.1.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct ClifUnitKey {
    pub(crate) linear: u32,
    pub(crate) physical: u32,
    pub(crate) mode_key: u32,
}

/// K1-K5 from the entry-guard checklist (plan section 2.2), byte-for-byte the same
/// exclusions as `direct::key_for`: host support, decode variant, persona class, the BIOS
/// F-page window, and the video/ROM physical window (G4's static half).
pub(crate) fn clif_key_for(cpu: &CpuGsw, lin: u32, d: bool) -> Option<ClifUnitKey> {
    if !super::super::host_supported()
        || !d
        || !matches!(cpu.persona(), CpuPersona::I486 | CpuPersona::I586)
    {
        return None;
    }
    if lin.wrapping_sub(0x000f_f000) < 0x400 {
        return None;
    }
    let physical = cpu.decode_cache.line_phys_start(lin, d)?;
    if (0x000a_0000..0x0010_0000).contains(&physical) {
        return None;
    }
    Some(ClifUnitKey {
        linear: lin,
        physical,
        mode_key: cpu.jit_mode_key(),
    })
}

/// Per-unit descriptor: the clif analogue of Direct's `Compilation`/`CompiledBlock`
/// metadata. The guest byte layout here is AUTHORITATIVE (decision D-C1.5), never the
/// arena's page-rounded span. Carries the mode/CS/CPL/data-segment snapshot Direct's
/// `CompiledBlock` carries, plus the three guard/quota fields the review named explicitly:
/// `has_wide_accesses`, `is_self_loop`, and `instructions`.
#[derive(Clone, Debug)]
pub(crate) struct ClifUnitDescriptor {
    pub(crate) key: ClifUnitKey,
    /// Guest bytes covered, from the unit's own layout (D-C1.5).
    pub(crate) guest_len: u16,
    /// Per-instruction guest fetch lengths, `instructions` entries live.
    pub(crate) fetch_lens: [u8; MAX_BLOCK_INSTRUCTIONS],
    pub(crate) instructions: u8,
    /// CS/data-segment descriptor snapshot at admission (guards G6/G8).
    pub(crate) segment_layout: SegmentLayout,
    /// CPL-3 memory model captured at admission (guard G7).
    pub(crate) memory_cpl3: bool,
    /// Any word or dword access in the unit (guard G9).
    pub(crate) has_wide_accesses: bool,
    /// Terminal Jcc whose taken target is the unit entry.
    pub(crate) is_self_loop: bool,
    /// The compiled unit's native entry.
    pub(crate) entry: usize,
    /// The operand table (F4/design section 2.2, widened by C1c): TWO `u32` lanes per
    /// instruction slot, `[2 * i]` the operand immediate (`insn.imm` verbatim; the decoder
    /// already operand-width-extended it, including 0x83's sign extension) and `[2 * i + 1]`
    /// the addressing-mode displacement (design section 1.2's F4 extension). Both lanes are
    /// guest-controlled values, so both load from this table at compile-time-constant
    /// offsets, never bake as constants. The second lane exists because a memory form with
    /// an immediate source (`mov dword [ebx+disp], imm32`, 0xc7) carries BOTH a displacement
    /// AND an operand immediate; the design's single-slot table cannot hold both (a recorded
    /// C1c deviation from design section 1.2's "no new table shape is needed" claim).
    pub(crate) operands: [u32; 2 * MAX_BLOCK_INSTRUCTIONS],
    /// How many leading slots execute natively (lowered instructions plus x87 call-outs);
    /// the unit side-exits at slot `leading` (the stop slot) with EIP materialized there.
    pub(crate) leading: u8,
    /// Bit i set when slot i is an x87 call-out (charged by the interpreter during the
    /// call-out, excluded from the static profile per the no-double-charge invariant).
    pub(crate) x87_mask: u32,
    /// Static profile: cumulative raw core clocks of LOWERED slots with index < i (x87 and
    /// never-executed slots excluded), reusing Direct's per-kind cost table
    /// (`DirectKind::raw_clocks`). Index `leading` (== the full leading run) is carried in
    /// `raw_clocks_total` since the array is indexed by slot.
    pub(crate) cum_raw_before: [u16; MAX_BLOCK_INSTRUCTIONS],
    /// Cumulative count of LOWERED slots with index < i (the design's
    /// `lowered_instructions` fetch-population split: x87 slots excluded).
    pub(crate) cum_lowered_before: [u8; MAX_BLOCK_INSTRUCTIONS],
    /// The full leading run's raw clocks and lowered-slot count (the normal side-exit
    /// charge).
    pub(crate) raw_clocks_total: u16,
    pub(crate) lowered_total: u8,
    /// Static per-width memory access counts (C1c, design section 4/M4): cumulative counts
    /// of LOWERED slots with index < i, computed by `plan_unit` through `DirectKind`'s own
    /// per-width accessors verbatim, so the two backends' counts are equal by shared code.
    /// X87 slots are excluded (their call-out charges its own accesses through the
    /// interpreter, the no-double-charge invariant).
    pub(crate) cum_access_before: [ClifAccessCounts; MAX_BLOCK_INSTRUCTIONS],
    /// The full leading run's access counts (the normal full-run charge).
    pub(crate) access_total: ClifAccessCounts,
}

/// Per-width static memory access counts for one unit prefix, the clif mirror of the six
/// accumulator fields Direct's `Compilation` carries (design section 4's parallel-field
/// decision, D-C1.1). In increment 1 every natively retired access is RAM by construction
/// (the identity check side-exits on the mode13 kind), so these static counts equal the
/// dynamic counts Direct would have accumulated and RAM-lane charging is exact.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ClifAccessCounts {
    pub(crate) byte_reads: u8,
    pub(crate) word_reads: u8,
    pub(crate) dword_reads: u8,
    pub(crate) byte_stores: u8,
    pub(crate) word_stores: u8,
    pub(crate) dword_stores: u8,
}

impl ClifAccessCounts {
    pub(crate) fn is_zero(self) -> bool {
        self == Self::default()
    }

    pub(crate) fn reads(self) -> u64 {
        u64::from(self.byte_reads) + u64::from(self.word_reads) + u64::from(self.dword_reads)
    }

    pub(crate) fn stores(self) -> u64 {
        u64::from(self.byte_stores) + u64::from(self.word_stores) + u64::from(self.dword_stores)
    }
}

impl ClifUnitDescriptor {
    /// G6: full CS descriptor equality, not selector-only (mirrors
    /// `CompiledBlock::cs_descriptor_matches`).
    pub(crate) fn cs_descriptor_matches(&self, cpu: &CpuGsw) -> bool {
        self.segment_layout.cs_matches(cpu)
    }

    /// G8, non-chain form only (C1a has no linking; the chain variant is C1d's job, per the
    /// checklist's deferred-to-C1d note).
    pub(crate) fn data_descriptors_match(&self, cpu: &CpuGsw) -> bool {
        self.segment_layout.data_matches(cpu)
    }
}

/// The result of the unit-boundary growth walk (F-A5).
pub(crate) struct UnitLayout {
    pub(crate) guest_len: u16,
    pub(crate) fetch_lens: [u8; MAX_BLOCK_INSTRUCTIONS],
    pub(crate) instructions: u8,
    pub(crate) has_wide_accesses: bool,
    pub(crate) is_self_loop: bool,
    pub(crate) read_segments: u8,
    pub(crate) write_segments: u8,
    /// The per-slot classifications, in slot order, for the C1b lowering (compile-time
    /// input only; the descriptor stores derived data, never the kinds themselves).
    pub(crate) kinds: Vec<DirectKind>,
    /// Per-slot operand-immediate and addressing-displacement lanes (`[2 * i]` /
    /// `[2 * i + 1]`), populating the descriptor's operand table.
    pub(crate) operands: [u32; 2 * MAX_BLOCK_INSTRUCTIONS],
}

/// Forward-decode a unit from the hot root, terminating on the four terminal kinds
/// (Call/Jmp/Ret/Jcc, INCLUDED as the unit's last instruction), on the first structurally
/// unclassifiable opcode (Q1's stop-growth resolution: the unit ends BEFORE it), and at
/// `MAX_BLOCK_INSTRUCTIONS`, reusing `direct::classify` unchanged through the
/// `unit_growth_classify` seam. Page-local and warm-line-only, like Direct's builder; a
/// unit with zero instructions does not exist (`None`).
pub(crate) fn walk_unit(cpu: &CpuGsw, entry_lin: u32, d: bool) -> Option<UnitLayout> {
    let entry_page = entry_lin & !0xfff;
    let mut lin = entry_lin;
    let mut fetch_lens = [0u8; MAX_BLOCK_INSTRUCTIONS];
    let mut instructions = 0usize;
    let mut guest_len = 0u32;
    let mut has_wide_accesses = false;
    let mut is_self_loop = false;
    let mut read_segments = 0u8;
    let mut write_segments = 0u8;
    let mut kinds = Vec::new();
    let mut operands = [0u32; 2 * MAX_BLOCK_INSTRUCTIONS];
    // The word-form prefix acceptance and 586 gate mirror Direct's compile loop exactly
    // (`compile_with_instruction_limit`'s `prefixes_supported` and persona checks): a word
    // form is exactly one operand-size override, admitted only on the I586 persona.
    let word_prefixes = Prefixes {
        operand_size_override: true,
        ..Prefixes::default()
    };
    while instructions < MAX_BLOCK_INSTRUCTIONS {
        if lin & !0xfff != entry_page {
            break;
        }
        let Some(insn) = cpu.decode_cache.get(lin, d) else {
            break;
        };
        let prefixes_supported = match insn.operand_size {
            OperandSize::Word => {
                insn.prefixes == word_prefixes && cpu.persona() == CpuPersona::I586
            }
            OperandSize::Dword => insn.prefixes == Prefixes::default(),
        };
        if !prefixes_supported {
            break;
        }
        if (lin & 0xfff) + u32::from(insn.len) > 0x1000 {
            break;
        }
        let Some(step) = direct::unit_growth_classify(&insn, lin, entry_lin) else {
            break;
        };
        // C1c section 1.4: the stack-width admission gate, identical to Direct's
        // compile-time reject (`direct.rs`'s uses_stack + !stack_is_32bit Retry). A 16-bit
        // stack's SP wrap is a form this design does not build, so growth stops at the
        // slot exactly as an unclassifiable opcode would (no new stop tag, per the
        // question ledger). Push/Pop are the only lowered kinds that touch SS implicitly;
        // Call/Ret are terminals and not lowered at all.
        if matches!(step.kind, DirectKind::Push { .. } | DirectKind::Pop { .. })
            && !cpu.stack_is_32bit()
        {
            break;
        }
        fetch_lens[instructions] = insn.len;
        operands[2 * instructions] = slot_immediate(&step.kind);
        operands[2 * instructions + 1] = slot_displacement(&step.kind);
        instructions += 1;
        guest_len += u32::from(insn.len);
        has_wide_accesses |= step.wide_access;
        read_segments |= step.read_segments;
        write_segments |= step.write_segments;
        kinds.push(step.kind);
        lin = lin.wrapping_add(u32::from(insn.len));
        match step.terminal {
            Some(UnitTerminal::Jcc { taken_delta }) => {
                is_self_loop = taken_delta == 0;
                break;
            }
            Some(UnitTerminal::Jmp | UnitTerminal::Call | UnitTerminal::Ret) => break,
            None => {}
        }
    }
    if instructions == 0 {
        return None;
    }
    Some(UnitLayout {
        guest_len: u16::try_from(guest_len).ok()?,
        fetch_lens,
        instructions: instructions as u8,
        has_wide_accesses,
        is_self_loop,
        read_segments,
        write_segments,
        kinds,
        operands,
    })
}

/// The operative immediate for one slot's table entry: `insn.imm` verbatim for the genuine
/// immediate forms (the classifier stored it verbatim, decoder-width-extended per m1,
/// including 0x83's sign extension), the structural constant for forms whose count is
/// implied by the encoding (0xd1's shift-by-1 carries no immediate byte, so D3 can never
/// patch it), and 0 for slots with no immediate (never loaded).
fn slot_immediate(kind: &DirectKind) -> u32 {
    match *kind {
        DirectKind::MovImm { imm, .. }
        | DirectKind::AluImm { imm, .. }
        | DirectKind::TestImmReg { imm, .. }
        | DirectKind::TestImmMem { imm, .. } => imm,
        DirectKind::MovImmByte { imm, .. } | DirectKind::AluByteImm { imm, .. } => u32::from(imm),
        DirectKind::Shift { count, .. } => u32::from(count),
        DirectKind::DoubleShiftReg {
            count: direct::ShiftCount::Immediate(count),
            ..
        } => u32::from(count),
        DirectKind::Lea { addr, .. } => addr.disp,
        // C1c: a store's or push's immediate source is an ordinary F4-governed operand
        // immediate, distinct from the same slot's displacement lane (design section 1.3
        // items 2 and 8).
        DirectKind::Store {
            source: direct::StoreSource::Imm(imm),
            ..
        }
        | DirectKind::AluMemDest {
            source: direct::StoreSource::Imm(imm),
            ..
        }
        | DirectKind::Push {
            source: direct::StoreSource::Imm(imm),
        } => imm,
        _ => 0,
    }
}

/// The addressing-mode displacement lane for one slot's table entry (design section 1.2:
/// a displacement is a guest immediate exactly like an ALU operand, so it loads from the
/// table for the identical D3 re-stamp reason). Zero for non-memory forms (never loaded).
/// `Lea` keeps its displacement in the IMMEDIATE lane (C1b's established layout, its only
/// operand); the memory forms use this second lane so a form carrying both an operand
/// immediate and a displacement (0xc6/0xc7 store-immediate) has a home for each.
fn slot_displacement(kind: &DirectKind) -> u32 {
    match *kind {
        DirectKind::Load { addr, .. }
        | DirectKind::Store { addr, .. }
        | DirectKind::AluMemSource { addr, .. }
        | DirectKind::AluMemDest { addr, .. }
        | DirectKind::TestImmMem { addr, .. }
        | DirectKind::DoubleShiftMem { addr, .. }
        | DirectKind::RmwIncDec { addr, .. } => addr.disp,
        _ => 0,
    }
}

/// Admission states, mirroring the Direct cache's `BlockState` roles: `Seen` (first
/// encounter recorded), `Compiled` (a live unit descriptor), `Dormant` (parked; G1 heat
/// demotion or a failed install).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ClifUnitState {
    Seen,
    Compiled(u32),
    Dormant,
}

/// The clif unit cache (decision D-C1.1): key-based lookup plus per-unit descriptors. No
/// hot-path accelerator table yet: a C1a shell unit executes nothing natively, so every
/// entry immediately side-exits and the lookup is never on a hot native-to-native path;
/// the accelerator is deferred to the sub-slice whose units actually retire instructions
/// (recorded deviation, mirrors the "if warranted" wording of the deliverable).
#[derive(Default)]
pub(crate) struct ClifUnitCache {
    entries: HashMap<ClifUnitKey, ClifUnitState, U32BuildHasher>,
    units: Vec<ClifUnitDescriptor>,
    /// Invalidation generation (review finding B1): bumped whenever a unit dies (a code
    /// write overlapping compiled spans, or a wholesale clear). `run_clif_unit` snapshots
    /// it before entering a unit; the call-out shim's exit latch compares live vs snapshot
    /// so an IN-FLIGHT unit whose own remaining bytes were just rewritten by its call-out
    /// exits instead of running stale lowering (the SMC choke alone only protects the NEXT
    /// entry).
    pub(crate) generation: u64,
    /// C1a diagnostics, excluded from CpuGsw equality through the enclosing type (F-A8).
    pub(crate) units_admitted: u64,
    pub(crate) heat_demotions: u64,
}

impl ClifUnitCache {
    pub(crate) fn state(&self, key: ClifUnitKey) -> Option<ClifUnitState> {
        self.entries.get(&key).copied()
    }

    pub(crate) fn note_seen(&mut self, key: ClifUnitKey) {
        self.entries.entry(key).or_insert(ClifUnitState::Seen);
    }

    pub(crate) fn park_dormant(&mut self, key: ClifUnitKey) {
        self.entries.insert(key, ClifUnitState::Dormant);
        self.heat_demotions = self.heat_demotions.saturating_add(1);
    }

    /// Park a key Dormant for a non-heat reason (G4 cover failure, a failed compile or
    /// install), mirroring Direct's plain `dormant()`: only a `Seen` key transitions, and the
    /// heat-demotion diagnostic is untouched since no heat gate fired.
    pub(crate) fn dormant(&mut self, key: ClifUnitKey) {
        if self.entries.get(&key) == Some(&ClifUnitState::Seen) {
            self.entries.insert(key, ClifUnitState::Dormant);
        }
    }

    /// G1 recovery: a heat-demoted Dormant whose entry-chunk stamp has aged out returns to
    /// `Seen`, mirroring `BlockCache::lift_cold_smc_dormant`.
    pub(crate) fn lift_cold_dormant(
        &mut self,
        heat: &mut SmcHeatMap,
        key: ClifUnitKey,
        epoch: u32,
    ) {
        if self.entries.get(&key) == Some(&ClifUnitState::Dormant)
            && heat.take_stale_stamp(key.physical, epoch)
        {
            self.entries.insert(key, ClifUnitState::Seen);
        }
    }

    /// Install a compiled unit descriptor for a key previously recorded `Seen`. Registers the
    /// unit's guest physical range with the SHARED `NativeCodeWatch` (design section 2.5's
    /// D-C1c.1, the M5 registration deliverable): a clif unit's resident bytes must read as
    /// watched the instant it becomes reachable, exactly mirroring Direct's own
    /// `acquire_range` call at install time (`direct.rs:793`/`845`), since an unregistered
    /// unit's own inline-lowered store would pass the code-watch check straight through a
    /// still-resident, still-executable copy of itself.
    pub(crate) fn install(
        &mut self,
        watch: &mut NativeCodeWatch,
        descriptor: ClifUnitDescriptor,
    ) -> Option<u32> {
        if self.entries.get(&descriptor.key) != Some(&ClifUnitState::Seen) {
            return None;
        }
        let index = u32::try_from(self.units.len()).ok()?;
        let key = descriptor.key;
        watch.acquire_range(key.physical, u32::from(descriptor.guest_len));
        self.units.push(descriptor);
        self.entries.insert(key, ClifUnitState::Compiled(index));
        self.units_admitted = self.units_admitted.saturating_add(1);
        Some(index)
    }

    pub(crate) fn unit(&self, index: u32) -> Option<&ClifUnitDescriptor> {
        self.units.get(index as usize)
    }

    /// Wholesale drop. Releases every currently-installed unit's watch registration first
    /// (the M5 discipline: every eviction path releases what it acquired), rather than
    /// relying on a coincidentally-paired wholesale `NativeCodeWatch::clear()` elsewhere,
    /// since this cache and Direct's block cache release independently and a Direct clear
    /// does not always accompany a clif clear (`core.rs`'s two callers are always paired
    /// today, but the cache must stay correct on its own, not by that coincidence).
    pub(crate) fn clear(&mut self, watch: &mut NativeCodeWatch) {
        // Only entries still reachable as `Compiled` hold a live registration: `units` is
        // append-only (an invalidated slot's descriptor merely goes unreachable, mirroring
        // Direct's own arena-fill note), so releasing every stored descriptor here would
        // double-release ranges an earlier `invalidate_physical_range` already released.
        for state in self.entries.values() {
            if let ClifUnitState::Compiled(index) = state
                && let Some(unit) = self.units.get(*index as usize)
            {
                watch.release_range(unit.key.physical, u32::from(unit.guest_len));
            }
        }
        self.entries.clear();
        self.units.clear();
        self.generation = self.generation.wrapping_add(1);
    }

    /// SMC/code-invalidation for compiled clif units (new in C1b: a C1a shell executed
    /// nothing, so staleness could not matter; a lowered unit executes real guest code and
    /// MUST die when its bytes change). Drops every entry whose physical span overlaps the
    /// written range; `Seen`/`Dormant` states are dropped too (their walk-derived layout may
    /// be stale). A linear scan is fine at C1b's correctness tier; the per-page index is a
    /// later perf item.
    ///
    /// Arena-fill behavior (review finding m3, recorded): an invalidated unit's arena span
    /// is never freed (its descriptor merely goes unreachable), so a long SMC-heavy run
    /// ratchets the clif arena toward full. On fill, `ClifBackend::finalize` returns `None`
    /// and new keys park Dormant, i.e. reject-and-interpret: sound, merely slower. Span
    /// reuse/compaction is future work (the C1 plan's single-page compaction item).
    pub(crate) fn invalidate_physical_range(
        &mut self,
        watch: &mut NativeCodeWatch,
        start: u32,
        len: u32,
    ) {
        if len == 0 {
            return;
        }
        let end = start.saturating_add(len);
        let before = self.entries.len();
        let units = &self.units;
        self.entries.retain(|key, state| {
            let span = match state {
                ClifUnitState::Compiled(index) => Self::unit_span(units, *index, *key),
                // Conservative page-overlap drop for non-compiled states (no recorded
                // guest_len exists for them).
                ClifUnitState::Seen | ClifUnitState::Dormant => (
                    key.physical & !0xfff,
                    (key.physical & !0xfff).saturating_add(0x1000),
                ),
            };
            let overlaps = !(span.1 <= start || span.0 >= end);
            // M5: release the watch registration exactly when a COMPILED unit is the one
            // dropped, mirroring Direct's `retire_block` releasing on eviction; `Seen`/
            // `Dormant` keys never acquired a registration in the first place.
            if overlaps && let ClifUnitState::Compiled(_) = state {
                watch.release_range(span.0, span.1 - span.0);
            }
            !overlaps
        });
        if self.entries.len() != before {
            // B1: an in-flight unit whose entry just died must not resume its lowered
            // slots; the generation mismatch trips the call-out exit latch.
            self.generation = self.generation.wrapping_add(1);
        }
    }

    fn unit_span(units: &[ClifUnitDescriptor], index: u32, key: ClifUnitKey) -> (u32, u32) {
        let len = units
            .get(index as usize)
            .map_or(0x1000, |unit| u32::from(unit.guest_len));
        (key.physical, key.physical.saturating_add(len))
    }
}
