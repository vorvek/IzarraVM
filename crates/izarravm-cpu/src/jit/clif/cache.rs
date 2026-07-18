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
use std::sync::Arc;

use super::super::code_watch::NativeCodeWatch;
use super::super::direct::{
    self, DirectKind, MAX_BLOCK_INSTRUCTIONS, SegmentLayout, SmcHeatMap, UnitTerminal,
};
use super::super::links::{BlockPortal, LinkCell, LinkSource, LinkTarget};
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
    /// C1d: the leading run ends in a natively-retired `Jmp`/`Jcc` terminal, so a normal
    /// full-run exit lands at a FRESH target instruction (Direct's `Run` continuation
    /// shape: no interpret-one-first skip) rather than at an un-retired stop slot.
    pub(crate) terminal: bool,
    /// C1e (design section 1.2): the per-slot ENCODED displacement and immediate byte
    /// counts, decoder-recorded (never value-derived: a disp8 of -1 and a disp32 of
    /// 0xFFFFFFFF are indistinguishable by value, only by consumed width). The slot's
    /// tail (its restampable region) is the LAST `disp_len[i] + imm_len[i]` bytes;
    /// everything before is structural (prefixes/opcode/ModRM/SIB) and a write there
    /// kills. Terminal slots (Jmp/Jcc/Call/Ret) store 0/0 deliberately: their
    /// branch-offset bytes are BAKED into the compiled code and the successor records,
    /// not loaded from the operand table, so no tail patch can repair them (a C1e
    /// implementation correction to the design's generic tail rule; Kill is always
    /// sound).
    pub(crate) disp_len: [u8; MAX_BLOCK_INSTRUCTIONS],
    pub(crate) imm_len: [u8; MAX_BLOCK_INSTRUCTIONS],
    /// C1e (design section 2.1): the per-slot immediate extension rule for restamp
    /// re-reads (see `ImmExtend`; displacement re-reads need no discriminant: disp8 is
    /// always sign-extended, disp32/moffs full-width).
    pub(crate) imm_extend: [ImmExtend; MAX_BLOCK_INSTRUCTIONS],
    /// C1e lane-routing bit i set when slot i is `Lea`, whose displacement VALUE lives in
    /// the IMMEDIATE lane (`slot_immediate`'s `Lea` arm, C1b's established layout), so a
    /// displacement restamp must patch `operands[2*i]`, not the `[2*i+1]` lane the memory
    /// forms use. Patching the wrong lane would leave the loaded lane stale: silent
    /// divergence, exactly what the routing bit prevents.
    pub(crate) lea_mask: u32,
    /// C1e extension-routing bit i set when slot i is a moffs form (0xA0-0xA3): its
    /// 2-byte offset is ZERO-extended (`fetch_moffs`), while every other disp16 is
    /// sign-extended (`parse_16bit_address`), and `disp_len == 2` alone cannot tell the
    /// two apart (the same value-width collapse as 0x81/0x83 on the immediate side).
    pub(crate) moffs_mask: u32,
    /// C1e (design section 2.1, review m1): the host pointer to the unit's code PAGE,
    /// captured from the SAME `direct_page(key.physical, InstructionPrefetch)` cover
    /// check G4 admission already performs, so the restamp's post-write re-read goes
    /// through the certified physical-RAM mapping with no re-translation. Stable for the
    /// unit's life: a data-aperture remap never moves machine RAM, and every mapping
    /// change that could (`note_direct_map_changed`, A20) wholesale-clears the clif
    /// cache first.
    pub(crate) code_host: usize,
    /// C1d (design section 3.6/M2): the statically-known link targets of the terminal
    /// `Jmp`/`Jcc` edges. Slot 0 the taken/only edge, slot 1 the `Jcc` not-taken
    /// fall-through, matching Direct's two-successor convention. A self-referential taken
    /// edge stores `None` (it never links in C1d; the edge stays a side exit).
    pub(crate) successors: [Option<LinkTarget>; 2],
}

/// C1e (design section 2.1): the extension rule a restamp applies when re-reading an
/// immediate field from guest memory, recorded at walk time from `insn.opcode` and
/// `insn.operand_size` while the raw `DecodedInsn` is in scope (review finding B1:
/// `DirectKind` CANNOT supply this, because classify collapses 0x81/0x83 into identical
/// kinds, losing both the width and the sign-extension distinction).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ImmExtend {
    /// No immediate bytes (or a slot whose tail is structurally baked: terminals).
    #[default]
    None,
    /// One byte, zero-extended (the plain byte-op forms: 0xB0-0xB7, the 0x80 group,
    /// 0xC6, 0xA8, 0xF6, the shift/double-shift count bytes).
    ZeroByte,
    /// One byte, SIGN-extended (0x83's imm8 group and 0x6A push imm8).
    SignByte,
    /// Two bytes little-endian, zero-extended (the 0x66-gated word immediates and the
    /// 16-bit moffs forms 0xA0-0xA3, whose offset is an unsigned 16-bit address).
    /// UNREACHABLE today, deliberately kept: `classify`'s word whitelist
    /// (`classify.rs`, the `0x39|0x3b|0x40..=0x4f|0x89|0x8b|0xff` gate) admits no
    /// word-IMMEDIATE form, so no admitted slot records `imm_len == 2`; the arm is
    /// pinned by a unit test below so a future word-imm admission cannot silently
    /// inherit the wrong extension.
    Word,
    /// Two bytes little-endian, SIGN-extended (a ModRM disp16: `parse_16bit_address`
    /// sign-extends both the mode-2 and the mode-0/rm-6 disp16 into its i32, distinct
    /// from the zero-extending moffs form above; the walk tells them apart by opcode).
    /// UNREACHABLE today for the same keep-it-pinned reason: `direct_addr` rejects
    /// 16-bit address-size forms outright, so no admitted slot records a 2-byte
    /// displacement.
    SignWord,
    /// Four bytes little-endian, full width.
    Dword,
}

/// C1e (design section 5): what one `invalidate_physical_range` call did, returned so
/// `note_code_write_hit` can feed the churn counters and the G1 heat map (design 2.2c:
/// only KILLS heat; restamps are the cheap survivor path and contribute nothing).
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ClifInvalidateOutcome {
    /// Compiled units retired by the Kill verdict (leading-byte or multi-slot writes).
    pub(crate) kills: u32,
    /// Compiled units whose operand lane(s) were patched in place, no retirement.
    pub(crate) restamps: u32,
    /// `Seen`/`Dormant` entries dropped by the conservative page-overlap rule (no member
    /// layout exists to classify against).
    pub(crate) kills_no_layout: u32,
    /// The subset of `kills` escalated by the coarse multi-slot rule (design 1.3: every
    /// touched slot was individually tail-confined, but more than one slot was touched).
    pub(crate) kills_multi_slot: u32,
}

/// Per-slot verdict aggregation for one write against one compiled unit (design 1.1).
enum WriteVerdict {
    /// A leading/structural byte (prefix/opcode/ModRM/SIB, or a terminal's baked tail)
    /// was touched: the compiled code's structure may be stale.
    Kill,
    /// Every touched byte was tail-confined per slot, but more than one slot was touched
    /// (the coarse rule accepted by the design review, section 1.3/Q1).
    KillMultiSlot,
    /// Exactly one slot touched, tail bytes only: patch its lane(s) in place.
    Restamp {
        slot: usize,
        /// The slot's byte offset from the unit's physical anchor.
        slot_off: u32,
    },
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
    /// The unresolved-sentinel descriptor (C1d design section 3.3b): `entry` is the
    /// resolver trampoline, `operands` the static all-zeros table; every other field is
    /// inert filler that nothing reads (zero leading, zero profiles, an inert segment
    /// snapshot). The branch-free transfer thunk treats this exactly like a real landing
    /// record: it loads `entry` and computes `operands`' address, and the hop lands in the
    /// trampoline, which returns the unresolved disposition.
    pub(crate) fn sentinel(trampoline_entry: usize) -> Self {
        Self {
            key: ClifUnitKey {
                linear: 0,
                physical: 0,
                mode_key: 0,
            },
            guest_len: 0,
            fetch_lens: [0; MAX_BLOCK_INSTRUCTIONS],
            instructions: 0,
            segment_layout: SegmentLayout::inert(),
            memory_cpl3: false,
            has_wide_accesses: false,
            is_self_loop: false,
            entry: trampoline_entry,
            operands: [0; 2 * MAX_BLOCK_INSTRUCTIONS],
            leading: 0,
            x87_mask: 0,
            cum_raw_before: [0; MAX_BLOCK_INSTRUCTIONS],
            cum_lowered_before: [0; MAX_BLOCK_INSTRUCTIONS],
            raw_clocks_total: 0,
            lowered_total: 0,
            cum_access_before: [ClifAccessCounts::default(); MAX_BLOCK_INSTRUCTIONS],
            access_total: ClifAccessCounts::default(),
            terminal: false,
            disp_len: [0; MAX_BLOCK_INSTRUCTIONS],
            imm_len: [0; MAX_BLOCK_INSTRUCTIONS],
            imm_extend: [ImmExtend::None; MAX_BLOCK_INSTRUCTIONS],
            lea_mask: 0,
            moffs_mask: 0,
            code_host: 0,
            successors: [None; 2],
        }
    }

    /// C1d's clif `link_compatible` (design section 8b as amended by N2): equal mode key,
    /// equal CPL model, x87 PARITY on `x87_mask != 0` (Direct's `has_x87` equality clause
    /// mirrored; the TOP clauses stay dropped because clif bakes no TOP assumption, the
    /// recorded G5 omission), and full CS plus full data-segment equality. Subsumes the
    /// per-entry guards G4m/G6/G7/G8/G10 for linked transfers per the section 8b table.
    pub(crate) fn link_compatible(&self, target: &Self) -> bool {
        self.key.mode_key == target.key.mode_key
            && self.memory_cpl3 == target.memory_cpl3
            && (self.x87_mask != 0) == (target.x87_mask != 0)
            && self.segment_layout.link_compatible(target.segment_layout)
    }

    /// G8 chain form (design section 8): all six segments regardless of use, because a
    /// resolved chain validates every body reachable through this unit's own link cells,
    /// not only the segments this unit's own instructions touched.
    pub(crate) fn chain_descriptors_match(&self, cpu: &CpuGsw) -> bool {
        self.segment_layout.all_data_matches(cpu)
    }

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
    /// C1d: the terminal's static link targets (design section 3.6), slot 0 taken/only,
    /// slot 1 the `Jcc` fall-through; `None` for self-referential edges and non-linking
    /// terminals.
    pub(crate) successors: [Option<LinkTarget>; 2],
    /// C1e: the per-slot decoder-recorded operand byte counts and extension rules (see
    /// the descriptor's field docs).
    pub(crate) disp_len: [u8; MAX_BLOCK_INSTRUCTIONS],
    pub(crate) imm_len: [u8; MAX_BLOCK_INSTRUCTIONS],
    pub(crate) imm_extend: [ImmExtend; MAX_BLOCK_INSTRUCTIONS],
    pub(crate) lea_mask: u32,
    pub(crate) moffs_mask: u32,
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
    let mut successors: [Option<LinkTarget>; 2] = [None; 2];
    let mut disp_len = [0u8; MAX_BLOCK_INSTRUCTIONS];
    let mut imm_len = [0u8; MAX_BLOCK_INSTRUCTIONS];
    let mut imm_extend = [ImmExtend::None; MAX_BLOCK_INSTRUCTIONS];
    let mut lea_mask = 0u32;
    let mut moffs_mask = 0u32;
    let mode_key = cpu.jit_mode_key();
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
        // C1e: the restamp metadata, captured while the raw DecodedInsn is in scope (the
        // B1 rule: the extension discriminant comes from insn.opcode/operand_size, never
        // from DirectKind, which collapses 0x81/0x83). Terminal kinds zero their tail
        // deliberately: their branch-offset bytes are baked into compiled EIP
        // materialization and the successor records, not table-loaded, so any touch must
        // Kill (see the descriptor field doc).
        if step.terminal.is_some() {
            disp_len[instructions] = 0;
            imm_len[instructions] = 0;
            imm_extend[instructions] = ImmExtend::None;
        } else {
            disp_len[instructions] = insn.disp_len;
            imm_len[instructions] = insn.imm_len;
            imm_extend[instructions] = match insn.imm_len {
                0 => ImmExtend::None,
                // The only classifiable sign-extending imm8 forms are 0x83's group and
                // 0x6A push (0x6B imul also sign-extends but `classify` has no arm for
                // it, so it can never occupy a lowered slot); everything else with one
                // immediate byte is a plain zero-extended byte operand or a shift count.
                1 => {
                    if matches!(insn.opcode, 0x83 | 0x6a) {
                        ImmExtend::SignByte
                    } else {
                        ImmExtend::ZeroByte
                    }
                }
                2 => ImmExtend::Word,
                _ => ImmExtend::Dword,
            };
            if matches!(step.kind, DirectKind::Lea { .. }) {
                lea_mask |= 1 << instructions;
            }
            if matches!(insn.opcode, 0xa0..=0xa3) {
                moffs_mask |= 1 << instructions;
            }
        }
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
                // Slot 0 the taken edge (None when self-referential, the m2 rule: that
                // edge never links and stays a side exit), slot 1 the fall-through.
                successors[0] = (taken_delta != 0).then_some(LinkTarget {
                    linear: entry_lin.wrapping_add(taken_delta),
                    mode_key,
                });
                successors[1] = Some(LinkTarget {
                    linear: lin,
                    mode_key,
                });
                break;
            }
            Some(UnitTerminal::Jmp) => {
                if let DirectKind::Jmp { target_delta } = step.kind {
                    successors[0] = (target_delta != 0).then_some(LinkTarget {
                        linear: entry_lin.wrapping_add(target_delta),
                        mode_key,
                    });
                }
                break;
            }
            Some(UnitTerminal::Call | UnitTerminal::Ret) => break,
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
        successors,
        disp_len,
        imm_len,
        imm_extend,
        lea_mask,
        moffs_mask,
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
        }
        | DirectKind::DoubleShiftMem {
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
    /// Boxed for ADDRESS STABILITY across Vec reallocation (C1d design M1: Box, not Arc;
    /// the cache stays sole owner, and the retire discipline orders every teardown so no
    /// dangling window needs shared ownership): portals publish descriptor ADDRESSES as
    /// landing records, so a descriptor must never move while a portal can name it. The
    /// `Vec<Box<..>>` is the point: `Vec<T>` would move the descriptors on realloc,
    /// dangling every published portal body.
    #[allow(clippy::vec_box)]
    units: Vec<Box<ClifUnitDescriptor>>,
    /// Whether the unit at each index is still reachable (retire clears this; the Vec is
    /// append-only within a generation, so indices stay stable).
    live: Vec<bool>,
    /// C1d link bookkeeping, the clif-owned instances of the shared `links` vocabulary
    /// (design section 5.2: the tables stay per-backend; only the types are shared).
    portals: Vec<Arc<BlockPortal>>,
    cells: Vec<[Arc<LinkCell>; 2]>,
    outbound: Vec<[Option<u32>; 2]>,
    inbound: HashMap<u32, Vec<LinkSource<u32>>>,
    waiting: HashMap<LinkTarget, Vec<LinkSource<u32>>>,
    linear_units: HashMap<LinkTarget, u32>,
    /// The clif sentinel PORTAL (design section 3.3b): its `body` permanently holds the
    /// backend's sentinel-descriptor address; every fresh cell points here (N1a) and every
    /// hide republishes through it. Created on first link-bearing install, torn down with
    /// the rest of the bookkeeping on `clear` (N1b: a new backend generation gets a new
    /// sentinel).
    sentinel_portal: Option<Arc<BlockPortal>>,
    /// The current backend generation's sentinel-descriptor address (the clif linked-ness
    /// predicate compares against THIS, never against zero, per the links module's
    /// mechanism-neutrality contract).
    sentinel_addr: usize,
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
        debug_assert!(
            matches!(self.entries.get(&key), Some(ClifUnitState::Seen) | None),
            "park_dormant called on a non-Seen entry"
        );
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
        cells: [Arc<LinkCell>; 2],
        sentinel_addr: usize,
    ) -> Option<u32> {
        if self.entries.get(&descriptor.key) != Some(&ClifUnitState::Seen) {
            return None;
        }
        debug_assert!(
            self.sentinel_addr == 0 || self.sentinel_addr == sentinel_addr,
            "one sentinel descriptor per backend generation"
        );
        self.sentinel_addr = sentinel_addr;
        let index = u32::try_from(self.units.len()).ok()?;
        let key = descriptor.key;
        watch.acquire_range(key.physical, u32::from(descriptor.guest_len));
        self.units.push(Box::new(descriptor));
        self.live.push(true);
        self.portals.push(Arc::new(BlockPortal::new()));
        self.cells.push(cells);
        self.outbound.push([None, None]);
        self.entries.insert(key, ClifUnitState::Compiled(index));
        self.units_admitted = self.units_admitted.saturating_add(1);
        // The make_link_visible port (direct.rs:1520-1541, minus Direct's link-epoch
        // machinery, which exists for partial translation invalidation; clif's
        // invalidations are wholesale per N1(b)): register the landing key, resolve this
        // unit's own successors, resolve anyone waiting on this key, then publish this
        // unit's own DESCRIPTOR ADDRESS as the portal body (the landing record, design
        // section 3.3).
        let target = LinkTarget {
            linear: key.linear,
            mode_key: key.mode_key,
        };
        self.linear_units.insert(target, index);
        self.resolve_successors(index);
        self.resolve_waiting(target, index);
        let body = std::ptr::from_ref::<ClifUnitDescriptor>(&*self.units[index as usize]) as usize;
        self.portals[index as usize].publish(body);
        Some(index)
    }

    /// The clif sentinel portal, created on first use with the backend's sentinel
    /// descriptor address as its permanent body (design section 3.3b).
    pub(crate) fn sentinel_portal(&mut self, sentinel_addr: usize) -> Arc<BlockPortal> {
        debug_assert!(
            self.sentinel_addr == 0 || self.sentinel_addr == sentinel_addr,
            "one sentinel descriptor per backend generation"
        );
        self.sentinel_addr = sentinel_addr;
        if self.sentinel_portal.is_none() {
            let portal = Arc::new(BlockPortal::new());
            portal.publish(sentinel_addr);
            self.sentinel_portal = Some(portal);
        }
        self.sentinel_portal
            .clone()
            .expect("sentinel portal was just created")
    }

    /// The clif linked-ness predicate (design section 3.3b's consequence bullet): a cell
    /// is linked when its portal is not the sentinel portal AND the portal body is not the
    /// sentinel descriptor's address. Never a zero-compare: zero is Direct's mechanism.
    fn cell_linked(&self, index: usize, slot: usize) -> bool {
        let Some(cells) = self.cells.get(index) else {
            return false;
        };
        let portal_addr = cells[slot]
            .portal
            .load(std::sync::atomic::Ordering::Acquire);
        if let Some(sentinel) = &self.sentinel_portal
            && portal_addr == sentinel.address()
        {
            return false;
        }
        // A live cache owns every published portal in stable Arc storage; every cell is
        // repointed at the sentinel before that storage drops (N1(b)).
        let body = unsafe { &*(portal_addr as *const BlockPortal) }
            .body
            .load(std::sync::atomic::Ordering::Acquire);
        body != self.sentinel_addr && body != 0
    }

    /// The dynamic `has_link` predicate for the chain guards (design section 8): does
    /// either of this unit's outbound cells currently resolve to a linked portal? Mirrors
    /// Direct's `has_linked_successor` with the sentinel-address comparison.
    pub(crate) fn has_linked_successor(&self, index: u32) -> bool {
        self.cell_linked(index as usize, 0) || self.cell_linked(index as usize, 1)
    }

    /// resolve_successors port (direct.rs:1346-1365): try to link each statically-known
    /// successor of `source`; queue unresolved edges into `waiting`.
    fn resolve_successors(&mut self, source: u32) {
        let successors = self.units[source as usize].successors;
        for (slot, successor) in successors.into_iter().enumerate() {
            let Some(successor) = successor else {
                continue;
            };
            if let Some(target) = self.linear_units.get(&successor).copied()
                && self.try_link(source, slot as u8, target)
            {
                continue;
            }
            self.waiting.entry(successor).or_default().push(LinkSource {
                block: source,
                slot: slot as u8,
            });
        }
    }

    /// resolve_waiting port (direct.rs:1367-1382): link every queued edge waiting on this
    /// key; keep still-unresolvable live sources queued.
    fn resolve_waiting(&mut self, key: LinkTarget, target: u32) {
        let Some(waiting) = self.waiting.remove(&key) else {
            return;
        };
        let mut unresolved = Vec::new();
        for source in waiting {
            if !self.try_link(source.block, source.slot, target)
                && self.live.get(source.block as usize).copied() == Some(true)
            {
                unresolved.push(source);
            }
        }
        if !unresolved.is_empty() {
            self.waiting.insert(key, unresolved);
        }
    }

    /// try_link_inner port (direct.rs:1388-1430), static edges only (clif links no dynamic
    /// RET targets in C1d): both ends live, `link_compatible` (the section 8b subsumption
    /// clauses), then repoint the cell at the target's portal and record the edge.
    fn try_link(&mut self, source: u32, slot: u8, target: u32) -> bool {
        let source_index = source as usize;
        let target_index = target as usize;
        if self.live.get(source_index).copied() != Some(true)
            || self.live.get(target_index).copied() != Some(true)
            || !self.units[source_index].link_compatible(&self.units[target_index])
        {
            return false;
        }
        let slot_index = usize::from(slot);
        if self.outbound[source_index][slot_index] == Some(target) {
            return true;
        }
        self.unlink_outbound(source, slot);
        self.cells[source_index][slot_index].set(self.portals[target_index].as_ref());
        self.outbound[source_index][slot_index] = Some(target);
        self.inbound.entry(target).or_default().push(LinkSource {
            block: source,
            slot,
        });
        true
    }

    /// unlink_outbound port (direct.rs:1432-1449), with the N1(a) repoint discipline: a
    /// clif cell is NEVER left at the zero-portal default, so where Direct calls
    /// `clear()`, clif repoints at the sentinel portal.
    fn unlink_outbound(&mut self, source: u32, slot: u8) {
        let source_index = source as usize;
        let slot_index = usize::from(slot);
        let sentinel = self
            .sentinel_portal
            .clone()
            .expect("linked cells exist only after the sentinel portal");
        let Some(target) = self.outbound[source_index][slot_index].take() else {
            self.cells[source_index][slot_index].set(sentinel.as_ref());
            return;
        };
        self.cells[source_index][slot_index].set(sentinel.as_ref());
        if let Some(inbound) = self.inbound.get_mut(&target) {
            inbound.retain(|link| !(link.block == source && link.slot == slot));
            if inbound.is_empty() {
                self.inbound.remove(&target);
            }
        }
    }

    /// unlink_block port (direct.rs:1451-1482): tear down every edge naming `index`, in
    /// Direct's exact sequence: drop this unit's waiting queue entries, unregister the
    /// landing key, repoint every inbound predecessor's cell at the sentinel and re-queue
    /// it into `waiting` when the predecessor still names this successor, then unlink this
    /// unit's own outbound edges.
    fn unlink_unit(&mut self, index: u32) {
        self.remove_waiting_sources(index);
        let key = self.units[index as usize].key;
        let target_key = LinkTarget {
            linear: key.linear,
            mode_key: key.mode_key,
        };
        if self.linear_units.get(&target_key) == Some(&index) {
            self.linear_units.remove(&target_key);
        }
        let sentinel = self
            .sentinel_portal
            .clone()
            .expect("linked cells exist only after the sentinel portal");
        if let Some(inbound) = self.inbound.remove(&index) {
            for link in inbound {
                let source_index = link.block as usize;
                if self.live.get(source_index).copied() == Some(true) {
                    self.cells[source_index][usize::from(link.slot)].set(sentinel.as_ref());
                    self.outbound[source_index][usize::from(link.slot)] = None;
                    if let Some(successor) =
                        self.units[source_index].successors[usize::from(link.slot)]
                    {
                        self.waiting.entry(successor).or_default().push(link);
                    }
                }
            }
        }
        for slot in 0..2 {
            self.unlink_outbound(index, slot);
        }
        self.remove_waiting_sources(index);
    }

    fn remove_waiting_sources(&mut self, index: u32) {
        self.waiting.retain(|_, sources| {
            sources.retain(|source| source.block != index);
            !sources.is_empty()
        });
    }

    /// retire port (direct.rs:1491-1511, the section 7 reversal of the earlier no-retire
    /// default): hide this unit's own portal by publishing the sentinel descriptor's
    /// address (ONE Release store hides every inbound edge, design section 3.3b), unlink
    /// every edge in Direct's sequence, mark the unit dead, and release its watch
    /// registration. The boxed descriptor stays allocated (M1's address stability: an
    /// in-flight chain compiled before this call may still hold its address; the portal
    /// hide is what prevents any FUTURE transfer from landing in it).
    fn retire_unit(&mut self, watch: &mut NativeCodeWatch, index: u32) {
        if self.live.get(index as usize).copied() != Some(true) {
            return;
        }

        let key = self.units[index as usize].key;
        let guest_len = self.units[index as usize].guest_len;
        if self.sentinel_addr != 0 {
            self.portals[index as usize].publish(self.sentinel_addr);
            self.unlink_unit(index);
        }
        self.live[index as usize] = false;
        watch.release_range(key.physical, u32::from(guest_len));
    }

    /// The current backend generation's sentinel-descriptor address (zero before any
    /// link-bearing install).
    pub(crate) fn sentinel_descriptor_addr(&self) -> usize {
        self.sentinel_addr
    }

    /// Map a landing-record address back to a unit index (the Rust-side resolution of the
    /// chain trace the compiled thunks record; design section 4.3). Linear over the live
    /// units, fine at the correctness tier.
    pub(crate) fn unit_index_by_descriptor_addr(&self, addr: usize) -> Option<u32> {
        self.units.iter().enumerate().find_map(|(index, unit)| {
            (std::ptr::from_ref::<ClifUnitDescriptor>(&**unit) as usize == addr)
                .then_some(index as u32)
        })
    }

    pub(crate) fn unit(&self, index: u32) -> Option<&ClifUnitDescriptor> {
        self.units.get(index as usize).map(|unit| &**unit)
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
                && self.live.get(*index as usize).copied() == Some(true)
                && let Some(unit) = self.units.get(*index as usize)
            {
                watch.release_range(unit.key.physical, u32::from(unit.guest_len));
            }
        }
        self.entries.clear();
        // N1(b): a wholesale reset tears down ALL link bookkeeping together with the
        // units, never units alone (a reset that left resolved cells standing would leave
        // portals publishing freed descriptors). The sentinel portal and address go too: a
        // new backend generation means a new arena, a new trampoline, a new sentinel.
        self.units.clear();
        self.live.clear();
        self.portals.clear();
        self.cells.clear();
        self.outbound.clear();
        self.inbound.clear();
        self.waiting.clear();
        self.linear_units.clear();
        self.sentinel_portal = None;
        self.sentinel_addr = 0;
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
    ///
    /// C1e restructures the per-unit verdict from unconditional Kill to the D3 tail-byte
    /// classifier (design section 1): a write confined to one slot's displacement/
    /// immediate tail RESTAMPS the live descriptor's operand lane(s) in place (the unit,
    /// its watch registration, its links, and its cache state all survive untouched);
    /// anything touching a leading/structural byte, or more than one slot, still kills.
    ///
    /// Scan-cost caveat (C1e deliverable, recorded): this runs a LINEAR scan of every
    /// cache entry per SMC write that reaches the choke, on top of the per-slot
    /// classification for each overlapping unit. Fine at current unit counts (the
    /// SmcHeatMap demotes the pathological churn cases before the cache can grow hot
    /// spans), but a restamp-heavy workload with thousands of resident units would want
    /// the per-page index the C1b comment above already names as the later perf item.
    pub(crate) fn invalidate_physical_range(
        &mut self,
        watch: &mut NativeCodeWatch,
        start: u32,
        len: u32,
    ) -> ClifInvalidateOutcome {
        let mut outcome = ClifInvalidateOutcome::default();
        if len == 0 {
            return outcome;
        }
        let end = start.saturating_add(len);
        let before = self.entries.len();
        // Three-way pass split so the retire-and-unlink sequence (section 7: hide the
        // portal via one Release store of the sentinel address, repoint and re-queue every
        // inbound predecessor, unlink outbound, release the watch) and the restamp patches
        // both run with full &mut access AFTER the shared entry scan.
        let mut dying: Vec<(ClifUnitKey, Option<u32>)> = Vec::new();
        let mut restamps: Vec<(u32, usize, u32)> = Vec::new();
        for (key, state) in &self.entries {
            match state {
                ClifUnitState::Compiled(index) => {
                    let span = Self::unit_span(&self.units, *index, *key);
                    if span.1 <= start || span.0 >= end {
                        continue;
                    }
                    let Some(unit) = self.units.get(*index as usize) else {
                        dying.push((*key, Some(*index)));
                        outcome.kills += 1;
                        continue;
                    };
                    match Self::classify_write(unit, start, end) {
                        WriteVerdict::Kill => {
                            dying.push((*key, Some(*index)));
                            outcome.kills += 1;
                        }
                        WriteVerdict::KillMultiSlot => {
                            dying.push((*key, Some(*index)));
                            outcome.kills += 1;
                            outcome.kills_multi_slot += 1;
                        }
                        WriteVerdict::Restamp { slot, slot_off } => {
                            restamps.push((*index, slot, slot_off));
                            outcome.restamps += 1;
                        }
                    }
                }
                // Conservative page-overlap drop for non-compiled states (no recorded
                // guest_len or slot layout exists for them).
                ClifUnitState::Seen | ClifUnitState::Dormant => {
                    let page = key.physical & !0xfff;
                    if !(page.saturating_add(0x1000) <= start || page >= end) {
                        dying.push((*key, None));
                        outcome.kills_no_layout += 1;
                    }
                }
            }
        }
        for (key, index) in dying {
            if let Some(index) = index {
                self.retire_unit(watch, index);
            }
            self.entries.remove(&key);
        }
        // The restamp branch deliberately touches NOTHING but the operand lanes (design
        // 2.2/m4): no `retire_unit`, no watch release (the surviving unit's bytes must
        // STAY watched or the next SMC write to them would bypass this classifier
        // entirely), no cache-state transition, no link churn.
        let restamped = !restamps.is_empty();
        for (index, slot, slot_off) in restamps {
            if let Some(unit) = self.units.get_mut(index as usize) {
                Self::restamp_slot(unit, slot, slot_off);
            }
        }
        if self.entries.len() != before || restamped {
            // B1: an in-flight unit whose entry just died must not resume its lowered
            // slots; the generation mismatch trips the call-out exit latch.
            //
            // DESIGN REVERSAL, section 3.2's own trigger clause fired (C1e
            // implementation finding): the design argued a PURE restamp need not bump
            // because the operand-table loads are non-readonly `MemFlagsData::trusted()`
            // loads that cranelift's alias model may not hoist across the x87 call-out's
            // `call_indirect`. MEASURED FALSE on cranelift 0.133.1: the C1e in-flight
            // battery (`clif_restamp_in_flight_callout_patch_is_observed_by_later_slots`)
            // observed a compiled unit consume a PRE-PATCH operand after a mid-call-out
            // restamp had already stored the new lane value, i.e. the optimizer DID
            // reorder the trusted() load relative to the call. Section 3.2's closing
            // paragraph mandates the reversal for exactly this evidence: a restamp now
            // bumps the generation, so an in-flight unit exits at the call-out return and
            // the interpreter (and every FRESH native entry, which loads the patched
            // lane) observes the new operand. Everything else the restamp saves is
            // preserved: no re-walk, no re-lower, no re-install, no link churn, no watch
            // churn, no heat, no demotion; the bump costs one side exit for the
            // in-flight entry only.
            self.generation = self.generation.wrapping_add(1);
        }
        outcome
    }

    /// The D3 tail-byte classifier (design section 1): per touched slot, a write into the
    /// leading/structural bytes (`fetch_lens[i] - disp_len[i] - imm_len[i]` at the front)
    /// kills; a write confined to the operand tail restamps; more than one touched slot
    /// kills coarsely (Q1). Terminal slots recorded 0/0 tails at walk time, so any touch
    /// on them is a leading touch.
    fn classify_write(unit: &ClifUnitDescriptor, start: u32, end: u32) -> WriteVerdict {
        let mut off = 0u32;
        let mut touched: Option<(usize, u32)> = None;
        let mut multi = false;
        for i in 0..usize::from(unit.instructions) {
            let len = u32::from(unit.fetch_lens[i]);
            let s0 = unit.key.physical.wrapping_add(off);
            let s1 = s0.wrapping_add(len);
            off += len;
            if end <= s0 || start >= s1 {
                continue;
            }
            let tail = u32::from(unit.disp_len[i]) + u32::from(unit.imm_len[i]);
            // The write's low byte WITHIN this slot; anything below the tail boundary is
            // a structural touch.
            if start.max(s0) < s1 - tail {
                return WriteVerdict::Kill;
            }
            if touched.is_some() {
                multi = true;
            } else {
                touched = Some((i, off - len));
            }
        }
        match touched {
            _ if multi => WriteVerdict::KillMultiSlot,
            Some((slot, slot_off)) => WriteVerdict::Restamp { slot, slot_off },
            // Defensive: the span overlap guaranteed at least one slot intersects (slots
            // tile the unit's span exactly); kill rather than ignore if that ever breaks.
            None => WriteVerdict::Kill,
        }
    }

    /// The restamp action (design section 2): re-read the slot's FULL operand tail from
    /// physical RAM post-write (guest memory is the canonical merged state, so re-reading
    /// both sub-fields is always correct no matter which bytes the triggering write
    /// touched) and patch the descriptor's lane(s) in place. The value transformation
    /// reproduces the DECODER's own extension semantics exactly (the correctness cliff:
    /// a zero-extending restamp of 0x83's sign-extended imm8 would silently corrupt every
    /// later read of the operand).
    fn restamp_slot(unit: &mut ClifUnitDescriptor, slot: usize, slot_off: u32) {
        // An x87 call-out slot is never lowered and loads no lane (its call-out re-fetches
        // its own bytes through the interpreter, which the decode-cache invalidation
        // refreshed independently); leave its lanes at their walk-time zeros rather than
        // write values nothing reads.
        if unit.x87_mask & (1u32 << slot) != 0 {
            return;
        }
        let disp_len = usize::from(unit.disp_len[slot]);
        let imm_len = usize::from(unit.imm_len[slot]);
        let tail = disp_len + imm_len;
        if tail == 0 {
            return;
        }
        let leading = usize::from(unit.fetch_lens[slot]) - tail;
        let base =
            unit.code_host + (unit.key.physical & 0xfff) as usize + slot_off as usize + leading;
        // SAFETY: `code_host` is the host pointer of the direct RAM page the G4 admission
        // cover check certified for the unit's WHOLE `guest_len` span (captured in run.rs
        // at install), the slot tail lies inside that span by construction, and every
        // mapping change that could move machine RAM wholesale-clears this cache first
        // (see the `code_host` field doc), so the pointer is live and in bounds.
        let bytes = unsafe { core::slice::from_raw_parts(base as *const u8, tail) };
        if disp_len > 0 {
            let ext = match disp_len {
                1 => ImmExtend::SignByte,
                2 if unit.moffs_mask & (1u32 << slot) != 0 => ImmExtend::Word,
                2 => ImmExtend::SignWord,
                _ => ImmExtend::Dword,
            };
            let value = Self::extend_bytes(&bytes[..disp_len], ext);
            // Lane routing: `Lea` keeps its displacement in the IMMEDIATE lane (C1b's
            // layout, `slot_immediate`'s `Lea` arm); the memory forms use the second lane.
            let lane = if unit.lea_mask & (1u32 << slot) != 0 {
                2 * slot
            } else {
                2 * slot + 1
            };
            unit.operands[lane] = value;
        }
        if imm_len > 0 {
            unit.operands[2 * slot] = Self::extend_bytes(&bytes[disp_len..], unit.imm_extend[slot]);
        }
    }

    /// The decoder's extension semantics, reproduced byte-for-byte (design 2.1): the lane
    /// must end up exactly what a fresh decode-and-classify of the patched bytes would
    /// have stored (`sign_extend_u8` extends to the FULL 32 bits regardless of operand
    /// size, so `SignByte` needs no word-size split).
    fn extend_bytes(bytes: &[u8], ext: ImmExtend) -> u32 {
        match ext {
            ImmExtend::None => 0,
            ImmExtend::ZeroByte => u32::from(bytes[0]),
            ImmExtend::SignByte => bytes[0] as i8 as i32 as u32,
            ImmExtend::Word => u32::from(u16::from_le_bytes([bytes[0], bytes[1]])),
            ImmExtend::SignWord => u16::from_le_bytes([bytes[0], bytes[1]]) as i16 as i32 as u32,
            ImmExtend::Dword => u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
        }
    }

    fn unit_span(units: &[Box<ClifUnitDescriptor>], index: u32, key: ClifUnitKey) -> (u32, u32) {
        let len = units
            .get(index as usize)
            .map_or(0x1000, |unit| u32::from(unit.guest_len));
        (key.physical, key.physical.saturating_add(len))
    }
}

#[cfg(test)]
mod restamp_extension_tests {
    use super::*;

    /// C1e: the restamp re-read must reproduce the DECODER's extension semantics
    /// byte-for-byte (design section 2.1's correctness cliff). The Word/SignWord arms are
    /// currently unreachable through admission (no word-immediate or 16-bit-addressing
    /// form classifies) but are pinned here so a future admission widening cannot
    /// silently misextend.
    #[test]
    fn extend_bytes_reproduces_the_decoder_rules() {
        assert_eq!(
            ClifUnitCache::extend_bytes(&[0xf0], ImmExtend::ZeroByte),
            0x0000_00f0
        );
        // sign_extend_u8 extends to the FULL 32 bits regardless of operand size.
        assert_eq!(
            ClifUnitCache::extend_bytes(&[0xf0], ImmExtend::SignByte),
            0xffff_fff0
        );
        assert_eq!(
            ClifUnitCache::extend_bytes(&[0x34, 0xf0], ImmExtend::Word),
            0x0000_f034
        );
        assert_eq!(
            ClifUnitCache::extend_bytes(&[0x34, 0xf0], ImmExtend::SignWord),
            0xffff_f034
        );
        assert_eq!(
            ClifUnitCache::extend_bytes(&[0x78, 0x56, 0x34, 0xf2], ImmExtend::Dword),
            0xf234_5678
        );
        assert_eq!(ClifUnitCache::extend_bytes(&[], ImmExtend::None), 0);
    }
}
