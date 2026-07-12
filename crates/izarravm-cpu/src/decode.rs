// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

/// Whether a decoded group is safe to run as a cached continuation: it either falls through or is a
/// relative branch whose target is just the next live EIP. It must not touch a port, change CS or
/// system state, or halt. String ops (REP included) are admitted one level up in `block_continuable`
/// with their own justification. The executor still checks step breaks, interrupts, faults, and the
/// batch clock cap after every instruction.
fn block_straight_line(g: DecodeGroup) -> bool {
    matches!(
        g,
        DecodeGroup::Alu
            | DecodeGroup::DataMove
            | DecodeGroup::Stack
            | DecodeGroup::Group
            | DecodeGroup::Branch
            | DecodeGroup::FlagsMisc
            | DecodeGroup::BitManip
            | DecodeGroup::CondMove
            | DecodeGroup::Fpu
    )
}

/// Whether a decoded instruction may run as a cached continuation. Group-keyed for the
/// straight-line groups (`block_straight_line`); additionally admits, BY OPCODE within the
/// `ControlFlow` group, the forms that cannot halt, touch a port, or change CS:
/// near RET (0xC3), near RET imm16 (0xC2), and the 0xFF group-5 forms that stay near —
/// the plain fall-through INC r/m (/0), DEC r/m (/1), and PUSH r/m (/6) plus the near
/// indirect CALL (/2) and JMP (/4). (The bench probe showed /6 PUSH r/m alone was ~360k
/// of whetstone's ~360k run breaks: procedure-argument pushes, not transfers at all.)
/// Still ending the run: far RET (0xCA/0xCB), the far directs (0x9A/0xEA), the far
/// indirects (0xFF /3 and /5), the undefined /7 (#UD path), and INT3/INT n/INTO
/// (0xCC-0xCE) / IRET (0xCF) — they load CS or dispatch through the IDT. The continuation
/// follows the new EIP exactly as taken relative branches already do; every
/// per-continuation break check (step break, interrupt transition, clock cap,
/// decode-cache re-peek at the new linear EIP, page-local decode) is unchanged, and a
/// faulting stack read or segment-limit hit still routes through `finish_instruction`'s
/// rewind-and-deliver exactly as on the one-instruction path.
///
/// The IN forms (0xe4 IN AL,imm8; 0xe5 IN AX/EAX,imm8;
/// 0xec IN AL,DX; 0xed IN AX/EAX,DX) within `DecodeGroup::PortIo`, but ONLY when `level`
/// is in the Approximate timing class (I486/I586): a lazy port read (`MachineBus::read_io`)
/// no longer sets `io_touched` for the VGA status ports, so an IN reaching those ports no
/// longer needs to end the run to keep device state exact, letting a poll loop chain as
/// continuations instead of paying a full run restart every iteration. The OUT forms
/// (0xe6/0xe7/0xee/0xef) stay terminators: a write always sets `io_touched` (no lazy write
/// path exists), so admitting them would end the run right after anyway while widening the
/// blast radius for no benefit. INS/OUTS stay terminators too.
///
/// The same Approximate-class gate also admits the TEST accumulator-immediate forms
/// (0xa8 TEST AL,imm8; 0xa9 TEST AX/EAX,imm) within `DecodeGroup::Misc`. Their Misc
/// routing is a decode-classification artifact of the odd opcode neighborhood they share
/// with the BCD/string/HLT one-offs (see `route_group`'s A14 block), not a semantic
/// property: they are pure flag-writing ALU ops (AND-for-flags, no write-back), no memory,
/// no ModRM, no port, no control transfer, and their immediate is fully pre-parsed at
/// decode -- strictly simpler than the ALU forms `block_straight_line` already admits.
/// They matter because the canonical vretrace poll idiom is `IN; TEST AL,imm8; Jcc; JMP`:
/// with IN admitted but TEST still a terminator, every poll iteration ends its run at the
/// TEST and pays a full run restart, which measured at about the cost of the batch
/// epilogue the lazy port read had just eliminated (poll-3da measured 0.204/0.051).
/// NO other Misc opcode is admitted: the BCD adjusts, AAM/AAD (#DE path), SALC/XLAT
/// (memory read), INS/OUTS (port + string), and HLT all stay terminators.
///
/// Gated on `persona` (not a runtime bus flag) so the Accurate 386 class keeps
/// byte-identical batch structure: `block_continuable` is called once
/// per decode, and `CpuGsw::set_mode` unconditionally invalidates the decode cache
/// (`self.decode_cache.invalidate_and_clear_code_marks()`), so every decode-cache line is
/// re-decoded -- and this
/// admission re-resolved -- after any mode change.
fn block_continuable(
    group: DecodeGroup,
    opcode: u16,
    modrm: Option<ModRm>,
    persona: CpuPersona,
) -> bool {
    if block_straight_line(group) {
        return true;
    }
    // String ops (MOVS/CMPS/STOS/LODS/SCAS, REP or not) fall through, never touch a port
    // (INS/OUTS are Misc and stay terminators), and never change CS. A budgeted REP may return
    // after a bounded chunk. The run loop stops at that return, exposes the REP start EIP, and
    // resumes the saved decode only after the machine's event and interrupt checks. A faulting
    // iteration still routes through finish_instruction's original-instruction rewind.
    if group == DecodeGroup::StringOps {
        return true;
    }
    if group == DecodeGroup::PortIo {
        // Only the IN forms, only in the Approximate class; see the doc comment above.
        return matches!(persona, CpuPersona::I486 | CpuPersona::I586)
            && matches!(opcode, 0xe4 | 0xe5 | 0xec | 0xed);
    }
    if group == DecodeGroup::Misc {
        // Only TEST AL/AX/EAX,imm, only in the Approximate class; see the doc
        // comment above. Everything else in the Misc bucket stays a terminator.
        return matches!(persona, CpuPersona::I486 | CpuPersona::I586)
            && matches!(opcode, 0xa8 | 0xa9);
    }
    if group != DecodeGroup::ControlFlow {
        return false;
    }
    matches!(opcode, 0xc2 | 0xc3)
        || (opcode == 0xff && matches!(modrm, Some(m) if matches!(m.reg, 0 | 1 | 2 | 4 | 6)))
}

impl CpuGsw {
    /// The single routing authority for the decode/execute split: classify an opcode into the
    /// group whose dedicated split path handles it, or `Fallback` for the shared fused dispatch.
    /// `decode` and `execute_decoded` both call this and match on the result, so the predicate
    /// lives in exactly one place. `prefixes` is taken (unused for the ALU group) because future
    /// groups route on it (e.g. the 0x0F two-byte map, or operand-size-sensitive forms).
    pub(super) fn route_group(opcode: u16, _prefixes: Prefixes) -> DecodeGroup {
        // Two-byte (0F) map — the ONE place the `& 0xff00 == 0x0f00` predicate lives. `decode`
        // folds the second byte into `opcode` as 0x0F00 | second, so a 0F opcode is classified by
        // its low byte. MOVZX/MOVSX (0F B6/B7/BE/BF) are data movement and run through the split;
        // every other 0F opcode is `TwoByteFallback` (the un-converted fused `execute_two_byte`).
        // Handled first so the single-byte predicates below never see a 0F-high-byte value.
        if opcode & 0xff00 == 0x0f00 {
            return match opcode & 0xff {
                0xb6 | 0xb7 | 0xbe | 0xbf => DecodeGroup::DataMove,
                // Two-byte Jcc near (0F 80-0F 8F, rel16/32). The branch group (task A6a) handles
                // these; every other 0F opcode stays on the un-converted fused path.
                0x80..=0x8f => DecodeGroup::Branch,
                // Two-byte bit-manipulation block (task A10): BT/BTS/BTR/BTC reg (A3/AB/B3/BB),
                // BT/BTS/BTR/BTC imm8 (BA group 8), BSF/BSR (BC/BD), SHLD/SHRD (A4/A5/AC/AD),
                // CMPXCHG (B0/B1), XADD (C0/C1). Every one is a ModRM r/m form.
                0xa3 | 0xab | 0xb3 | 0xbb | 0xba | 0xbc | 0xbd | 0xa4 | 0xa5 | 0xac | 0xad
                | 0xb0 | 0xb1 | 0xc0 | 0xc1 => DecodeGroup::BitManip,
                // SETcc and two-operand IMUL. Integer CMOVcc is a P6 instruction and stays
                // outside the P55C contract.
                0x90..=0x9f | 0xaf => DecodeGroup::CondMove,
                // System / descriptor-table / segment block (task A12), 0F forms: the descriptor
                // groups 0F 00 (group 6) and 0F 01 (group 7), LAR/LSL (0F 02/03), CLTS (0F 06),
                // MOV reg,CR / MOV CR,reg (0F 20/22), MOV reg,DR / MOV DR,reg (0F 21/23, ledger
                // row 25), and LSS/LFS/LGS (0F B2/B4/B5, a far-pointer load like LES/LDS but into
                // SS/FS/GS).
                0x00 | 0x01 | 0x02 | 0x03 | 0x06 | 0x20 | 0x21 | 0x22 | 0x23 | 0xb2 | 0xb4
                | 0xb5 => DecodeGroup::SystemSeg,
                // Heterogeneous one-off 0F block (task A14): the no-operand system/serializing/CPU-id
                // ops INVD/WBINVD (08/09), WRMSR/RDTSC/RDMSR (30/31/32),
                // CPUID (A2), BSWAP (C8-CF); CMPXCHG8B (C7, a ModRM form); PUSH/POP FS/GS
                // (A0/A1/A8/A9, 386+, mirroring the one-byte ES/SS/DS segment push/pop arms in
                // `execute_stack_decoded`); and the whole MMX block (`is_mmx_two_byte`). 0F AA
                // (RSM) is unimplemented and stays TwoByteFallback.
                0x08
                | 0x09
                | 0x30
                | 0x31
                | 0x32
                | 0xa0
                | 0xa1
                | 0xa2
                | 0xa8
                | 0xa9
                | 0xc7
                | 0xc8..=0xcf => DecodeGroup::Misc,
                second if is_mmx_two_byte(second as u8) => DecodeGroup::Misc,
                _ => DecodeGroup::TwoByteFallback,
            };
        }
        // ALU block: ADD/OR/ADC/SBB/AND/SUB/XOR/CMP, forms 0-5 (`op = (opcode>>3)&7`,
        // `form = opcode & 7`; forms 6/7 are the segment PUSH/POP and are NOT ALU).
        if opcode < 0x40 && (opcode & 0x07) < 6 {
            return DecodeGroup::Alu;
        }
        // Single-byte data-movement block. Listed explicitly (not a range) because the surrounding
        // opcodes are unrelated: 0x8f is POP r/m, 0xa4-0xaf are the string ops, 0xc4/0xc5 are
        // LES/LDS. 0x90-0x97 is XCHG reg,(E)AX with 0x90 = NOP. The MOVZX/MOVSX two-byte forms are
        // intentionally absent (see `DecodeGroup::DataMove`).
        if matches!(
            opcode,
            0x86 | 0x87
                | 0x88
                | 0x89
                | 0x8a
                | 0x8b
                | 0x8c
                | 0x8d
                | 0x8e
                | 0x90..=0x97
                | 0xa0..=0xa3
                | 0xb0..=0xbf
                | 0xc6
                | 0xc7
        ) {
            return DecodeGroup::DataMove;
        }
        // Stack block: PUSH/POP reg, PUSH/POP seg, PUSH imm, POP r/m, PUSHA/POPA,
        // PUSHF/POPF, ENTER/LEAVE. 0xFF (group 5, which includes PUSH r/m /6) is a
        // separate multi-sub-op group handled as a unit by task A5 — do NOT list it here.
        if matches!(
            opcode,
            0x06 | 0x07 | 0x0e | 0x16 | 0x17 | 0x1e | 0x1f | 0x50
                ..=0x5f | 0x60 | 0x61 | 0x68 | 0x6a | 0x8f | 0x9c | 0x9d | 0xc8 | 0xc9
        ) {
            return DecodeGroup::Stack;
        }
        // Arithmetic /ext groups 1-4 (every one a ModRM whose `reg` selects the sub-op): group 1
        // ALU r/m,imm (0x80-0x83), group 2 shift/rotate (0xc0/0xc1/0xd0-0xd3), group 3 TEST/NOT/
        // NEG/MUL/IMUL/DIV/IDIV (0xf6/0xf7), group 4 INC/DEC byte (0xfe). 0xff (group 5) is the
        // indirect-CALL/JMP control-flow group and stays on Fallback — do NOT list it here.
        if matches!(
            opcode,
            0x80..=0x83 | 0xc0 | 0xc1 | 0xd0..=0xd3 | 0xf6 | 0xf7 | 0xfe
        ) {
            return DecodeGroup::Group;
        }
        // Relative-displacement + loop control flow (task A6a): Jcc short (0x70-0x7f), the loop/JCXZ
        // branches (0xe0-0xe3), CALL near (0xe8), JMP near (0xe9), JMP short (0xeb). The two-byte
        // Jcc near forms are routed in the 0F block above.
        if matches!(opcode, 0x70..=0x7f | 0xe0..=0xe3 | 0xe8 | 0xe9 | 0xeb) {
            return DecodeGroup::Branch;
        }
        // Far/indirect/RET/INT control flow + 0xff group 5 (task A6b): CALL/JMP far direct
        // (0x9a/0xea), RET/RETF with and without an imm16 release (0xc2/0xc3/0xca/0xcb), INT3/INT n/
        // INTO/IRET (0xcc-0xcf), and 0xff (group 5: INC/DEC r/m, near/far indirect CALL/JMP, PUSH
        // r/m, /7 #UD). These change CS/segment state and are delivered through the existing
        // far-call/far-jump/ret/retf/interrupt/IRET helpers, which the executor reuses verbatim.
        if matches!(
            opcode,
            0x9a | 0xc2 | 0xc3 | 0xca | 0xcb | 0xcc | 0xcd | 0xce | 0xcf | 0xea | 0xff
        ) {
            return DecodeGroup::ControlFlow;
        }
        // Flags + misc register block (task A7): TEST r/m,reg (0x84/0x85), INC/DEC reg (0x40-0x4f),
        // CBW/CWDE (0x98), CWD/CDQ (0x99), SAHF/LAHF (0x9e/0x9f), and the single flag-bit ops
        // CMC/CLC/STC/CLI/STI/CLD/STD (0xf5/0xf8-0xfd). None carry an immediate; only 0x84/0x85
        // carry a ModRM (parsed in `decode`).
        if matches!(
            opcode,
            0x40..=0x4f
                | 0x84
                | 0x85
                | 0x98
                | 0x99
                | 0x9e
                | 0x9f
                | 0xf5
                | 0xf8
                | 0xf9
                | 0xfa
                | 0xfb
                | 0xfc
                | 0xfd
        ) {
            return DecodeGroup::FlagsMisc;
        }
        // String operations (task A8): MOVS (0xa4/0xa5), CMPS (0xa6/0xa7), STOS (0xaa/0xab), LODS
        // (0xac/0xad), SCAS (0xae/0xaf). None carry a ModRM or an immediate. 0xa8/0xa9 (TEST AL/AX,imm)
        // sit between them and are deliberately excluded — they are not string ops and route to Misc.
        if matches!(opcode, 0xa4..=0xa7 | 0xaa..=0xaf) {
            return DecodeGroup::StringOps;
        }
        // Port I/O block (task A9): IN AL imm8 (0xe4), IN AX/EAX imm8 (0xe5), OUT imm8 AL (0xe6),
        // OUT imm8 AX/EAX (0xe7), IN AL DX (0xec), IN AX/EAX DX (0xed), OUT DX AL (0xee),
        // OUT DX AX/EAX (0xef). 0xe0-0xe3 are the loop/JCXZ branches (DecodeGroup::Branch) and are
        // already routed above; 0xe8/0xe9/0xeb are CALL/JMP (also Branch). The INS/OUTS forms
        // (0x6c-0x6f) are NOT listed here — they route to Misc.
        if matches!(opcode, 0xe4..=0xe7 | 0xec..=0xef) {
            return DecodeGroup::PortIo;
        }
        // System / descriptor-table / segment block (task A12), single-byte forms: BOUND r,m
        // (0x62), ARPL r/m16,r16 (0x63), and LES/LDS (0xc4/0xc5). Each is a ModRM r/m form whose
        // memory operand decode pre-parses; the far pointer for LES/LDS is read from memory at
        // execute time.
        if matches!(opcode, 0x62 | 0x63 | 0xc4 | 0xc5) {
            return DecodeGroup::SystemSeg;
        }
        // x87 FPU block (task A13): the eight escape opcodes 0xD8-0xDF (each a ModRM r/m or
        // register form) and WAIT/FWAIT (0x9B, no ModRM). `decode` fetches the ModRM once (and the
        // addressing descriptor for the mod != 3 memory forms); the executor reproduces the
        // fused #MF gate and calls the existing `execute_fpu_register`/`execute_fpu_memory`.
        if matches!(opcode, 0x9b | 0xd8..=0xdf) {
            return DecodeGroup::Fpu;
        }
        // Heterogeneous one-off single-byte block (task A14): BCD adjust DAA/DAS/AAA/AAS
        // (0x27/0x2f/0x37/0x3f), three-operand IMUL (0x69/0x6b), string port I/O INS/OUTS
        // (0x6c-0x6f), TEST AL/AX,imm (0xa8/0xa9), AAM/AAD (0xd4/0xd5), SALC/XLAT (0xd6/0xd7),
        // and HLT (0xf4). 0xf1 remains unimplemented and stays on Fallback.
        if matches!(
            opcode,
            0x27 | 0x2f | 0x37 | 0x3f | 0x69 | 0x6b | 0x6c
                ..=0x6f | 0xa8 | 0xa9 | 0xd4 | 0xd5 | 0xd6 | 0xd7 | 0xf4
        ) {
            return DecodeGroup::Misc;
        }
        DecodeGroup::Fallback
    }

    /// Stage B fetch front-end. Returns the decoded instruction for the current linear EIP, served
    /// from the decode cache on a hit (re-decode skipped) or decoded once and cached on a miss. On a
    /// hit, `decode` does not run, so this replays the instruction-fetch clocks `decode` would have
    /// charged and advances eip past the instruction, leaving the CPU in exactly the state the miss
    /// path produces before `execute_decoded` runs (eip at the instruction end; the same fetch bus
    /// cycles charged). The prefetch window is not touched on a hit because `execute_decoded` reads
    /// operands over the data bus, never the instruction stream.
    pub(super) fn fetch_decoded<B: CpuBus>(
        &mut self,
        bus: &mut B,
        lin: u32,
    ) -> ExecResult<DecodedInsn> {
        let cs = self.registers.cs();
        if let Some(insn) = self.decode_cache.get(lin, cs.default_size_32) {
            // Live fetch-limit recheck: the line may have been cached under a larger CS limit
            // (CS loads no longer flush the cache). A violation falls through to `decode`,
            // which enforces the fault at exactly the byte the fetch would have crossed.
            if Self::fetch_within_limit(self.registers.eip, insn.len, cs.limit) {
                self.charge_cached_fetch(bus, lin, insn.len)?;
                return Ok(insn);
            }
        }
        let insn = self.decode(bus)?;
        self.perf.decode_misses += 1;
        // A LOCK-prefixed instruction is never cached: `decode` runs `check_lock_target`, which both
        // peeks the lock target over the bus (charging fetch clocks that are NOT part of `len`, so a
        // cached replay would under-charge them) and raises #UD for a non-lockable target. Replaying
        // it from the cache would skip both. LOCK is rare, so re-decoding it every time is free.
        if !insn.prefixes.lock {
            // `put` owns both cache insertion and SMC-watch acquisition. Decode just warmed the
            // first code-page translation, so resolving the physical start is a cache hit (and the
            // identity map without paging). Page-straddling instructions remain uncached because
            // their next linear page can map to a noncontiguous physical page.
            let physical = self.translate_code_linear(bus, lin)?;
            let inserted = self
                .decode_cache
                .put(lin, insn, cs.default_size_32, physical);
            #[cfg(feature = "jit")]
            if let Some(slot) = inserted.evicted_slot {
                if self.decode_cache.line_count() == self.jit_direct.decode_slot_count() {
                    self.jit_direct.suspend_decode_slot(slot as usize);
                } else {
                    // Test-only cache replacement can change the direct-map shape. Hide every
                    // portal until root dispatch validates the current lines.
                    self.jit_direct.invalidate_translation();
                }
            }
            #[cfg(not(feature = "jit"))]
            let _ = inserted;
        }
        Ok(insn)
    }

    /// Charge the instruction-fetch bus clocks for a decode-cache HIT and advance eip past the
    /// instruction. This is an I-CACHE HIT: the (already-decoded) instruction is served from the
    /// instruction cache, so `charge_physical_instruction_fetch_run` charges it as a SINGLE I-cache
    /// access for cacheable RAM (the bus collapses the run to one cycle there; ROM/device code stays
    /// per byte). The decode line supplies its translated physical start; linear observation stays
    /// on `note_code_fetch_linear`.
    ///
    /// Calibration note (B-T8/B-T9): the COLD decode path (`decode` -> `fetch_u8`) still charges one
    /// fetch cycle per byte PLUS the opcode double-charge (`read_prefixes` peeks the opcode, then
    /// `decode` re-fetches it), i.e. `len + 1` cycles. This warm replay no longer mirrors that: the
    /// `len + 1` per-byte charge and the opcode double-charge are slow-bus/decode-time artifacts, not
    /// I-cache costs. Charging them on every execution floored the fast modes' Dhrystone/Sieve far
    /// below their era bands. A warm hit costs one I-cache access; the cold decode legitimately costs
    /// more. Over a benchmark loop the warm replay dominates, so the per-mode metric reflects the
    /// I-cache cost. The first (cold) execution costing more is physically correct and guest-invisible
    /// (it changes only the bus-clock metric, never a result).
    pub(super) fn charge_cached_fetch<B: CpuBus>(
        &mut self,
        bus: &mut B,
        lin: u32,
        len: u8,
    ) -> ExecResult<()> {
        bus.note_code_fetch_linear(lin);
        let d = self.registers.cs().default_size_32;
        let physical = match self.decode_cache.line_phys_start(lin, d) {
            Some(physical) => physical,
            None => self.translate_code_linear(bus, lin)?,
        };
        let count = u32::from(len);
        let first_count = count.min(0x1000 - (lin & 0x0fff));
        if first_count == count {
            bus.charge_physical_instruction_fetch_run(physical, count)?;
        } else {
            let tail_linear = lin.wrapping_add(first_count);
            let tail_physical = self.translate_code_linear(bus, tail_linear)?;
            if tail_physical == physical.wrapping_add(first_count) {
                bus.charge_physical_instruction_fetch_run(physical, count)?;
            } else {
                bus.charge_physical_instruction_fetch_run(physical, first_count)?;
                bus.charge_physical_instruction_fetch_run(tail_physical, count - first_count)?;
            }
        }
        self.registers.eip = self.registers.eip.wrapping_add(u32::from(len));
        Ok(())
    }

    /// Whether an instruction of `len` bytes starting at `eip` fetches entirely within the CS
    /// `limit`. The cached-hit counterpart of the per-byte limit check `decode`'s `fetch_u8`
    /// performs: a `false` here must MISS to `decode` so the #GP is raised at the same byte.
    #[inline]
    pub(super) fn fetch_within_limit(eip: u32, len: u8, limit: u32) -> bool {
        // `limit - (len - 1)` is the last start offset whose full fetch stays inside; a limit
        // smaller than `len - 1` admits no start at all (checked_sub catches it).
        match limit.checked_sub(u32::from(len) - 1) {
            Some(last_ok_start) => eip <= last_ok_start,
            None => false,
        }
    }

    /// Stage A of the decode/execute split. Reads the prefixes and opcode (mirroring the top
    /// of the legacy fused path) and, for the opcodes already converted to the split, parses
    /// the ModRM + addressing-mode descriptor up front. Opcodes still on the legacy path leave
    /// `modrm`/`operand` as `None`; `execute_decoded` hands them to the shared fused dispatch,
    /// which re-reads their ModRM/immediates from the post-opcode eip.
    ///
    /// Clock note (rule 2): decode's real `fetch_u8` reads charge the instruction-fetch clocks
    /// for the prefixes + opcode exactly once. `execute_decoded` charges nothing extra: the
    /// split opcode runs from the pre-decoded operand, and the legacy fallback continues the
    /// fused dispatch from where decode left off (it does NOT re-read the prefixes/opcode).
    pub(super) fn decode<B: CpuBus>(&mut self, bus: &mut B) -> ExecResult<DecodedInsn> {
        let start_eip = self.registers.eip;
        let prefixes = self.read_prefixes(bus)?;
        let opcode = self.fetch_u8(bus)?;
        if prefixes.lock {
            // The LOCK check runs on the first opcode byte and peeks (does not consume) the byte
            // after it — for 0F that peek is the second opcode byte, so it must happen before the
            // second-byte fetch below, exactly as the fused path ordered it.
            self.check_lock_target(bus, opcode)?;
        }
        let operand_size = self.operand_size(prefixes);
        let address_size = self.address_size(prefixes);

        // The two-byte (0F) decode convention. When the first byte is 0F, read the second byte
        // here — charging its instruction-fetch exactly once — and fold it into `insn.opcode` as
        // `0x0F00 | second`. Every later 0F group routes on this combined value, and the fused
        // fallback (`execute_two_byte`) consumes the second byte from `insn.opcode as u8` rather
        // than re-reading it. The persona #UD gate applies once, right after the read.
        let opcode = if opcode == 0x0f {
            let second = self.fetch_u8(bus)?;
            self.check_two_byte_isa_gate(second)?;
            0x0f00u16 | u16::from(second)
        } else {
            u16::from(opcode)
        };

        // The single `route_group` authority runs ONCE here; the result is stored in the insn so
        // `execute_decoded` matches the variant directly rather than re-classifying the opcode.
        let group = Self::route_group(opcode, prefixes);

        let mut insn = DecodedInsn {
            // `len` is a placeholder here; the single finalize after the group pre-parse below
            // overwrites it with the real consumed length (prefixes + opcode + operands).
            len: 0,
            prefixes,
            opcode,
            operand_size,
            address_size,
            modrm: None,
            operand: None,
            imm: 0,
            imm2: 0,
            group,
            // Placeholder; the finalize below resolves it once the ModRM (the 0xFF /ext
            // discriminator) has been pre-parsed.
            continuable: false,
        };

        // Pre-parse the operands of converted groups, dispatching on the group resolved above.
        match group {
            DecodeGroup::Alu => {
                // ALU block. Forms 0-3 carry a ModRM: parse it + its addressing-mode descriptor now
                // (the descriptor reads instruction bytes only, so it stays cacheable). Forms 4/5
                // carry an accumulator immediate: fetch it here (charging its fetch clocks once) so
                // the executor consumes `imm` without re-reading. `op = (opcode>>3)&7`, `form = &7`.
                let form = opcode & 0x07;
                if form < 4 {
                    let modrm = self.fetch_modrm(bus)?;
                    let operand = self.parse_addressing_mode(bus, prefixes, address_size, modrm)?;
                    insn.modrm = Some(modrm);
                    insn.operand = Some(operand);
                } else if form == 4 {
                    insn.imm = u32::from(self.fetch_u8(bus)?);
                } else {
                    insn.imm = self.fetch_immediate(bus, operand_size)?;
                }
            }
            DecodeGroup::DataMove => {
                // Data-movement block. The arms split by how the operand is encoded; the byte
                // budget each consumes here is what the executor must NOT re-fetch. The 0F
                // MOVZX/MOVSX forms (0x0Fb6/b7/be/bf) carry a plain ModRM, like the single-byte
                // ModRM forms below.
                match opcode {
                    // ModRM r/m forms: MOV r/m<->reg/Sreg, LEA, XCHG r/m (single byte) and
                    // MOVZX/MOVSX (two byte). Parse the ModRM + its addressing-mode descriptor
                    // (instruction bytes only, so it stays cacheable).
                    0x86..=0x8e | 0x0fb6 | 0x0fb7 | 0x0fbe | 0x0fbf => {
                        let modrm = self.fetch_modrm(bus)?;
                        let operand =
                            self.parse_addressing_mode(bus, prefixes, address_size, modrm)?;
                        insn.modrm = Some(modrm);
                        insn.operand = Some(operand);
                    }
                    // MOV r/m,imm (group 11). The displacement (if any) precedes the immediate in
                    // the encoding, so parse the operand first, then fetch the immediate. Only
                    // reg=000 is a defined encoding; for any other reg field the fused handler
                    // faults *before* decoding the operand or immediate, so do the same here and
                    // leave `operand`/`imm` unparsed (the executor re-detects the bad reg and
                    // raises the identical group-opcode error with the same bytes consumed).
                    0xc6 => {
                        let modrm = self.fetch_modrm(bus)?;
                        insn.modrm = Some(modrm);
                        if modrm.reg == 0 {
                            let operand =
                                self.parse_addressing_mode(bus, prefixes, address_size, modrm)?;
                            insn.operand = Some(operand);
                            insn.imm = u32::from(self.fetch_u8(bus)?);
                        }
                    }
                    0xc7 => {
                        let modrm = self.fetch_modrm(bus)?;
                        insn.modrm = Some(modrm);
                        if modrm.reg == 0 {
                            let operand =
                                self.parse_addressing_mode(bus, prefixes, address_size, modrm)?;
                            insn.operand = Some(operand);
                            insn.imm = self.fetch_immediate(bus, operand_size)?;
                        }
                    }
                    // MOV (E)AX<->moffs: a direct displacement (address-size wide), no ModRM.
                    0xa0..=0xa3 => {
                        insn.imm = self.fetch_moffs(bus, address_size)?;
                    }
                    // MOV r8,imm8.
                    0xb0..=0xb7 => {
                        insn.imm = u32::from(self.fetch_u8(bus)?);
                    }
                    // MOV r16/32,imm16/32.
                    0xb8..=0xbf => {
                        insn.imm = self.fetch_immediate(bus, operand_size)?;
                    }
                    // XCHG reg,(E)AX (0x90-0x97): no operand bytes; 0x90 is NOP (XCHG AX,AX).
                    _ => {}
                }
            }
            DecodeGroup::Stack => {
                // Stack block. Most opcodes carry no extra encoded bytes; only four sub-cases
                // fetch operand bytes here (all others are either register-encoded or implied).
                match insn.opcode as u8 {
                    // 0x68 PUSH imm16/32: fetch the full-width immediate; executor pushes it.
                    0x68 => {
                        insn.imm = self.fetch_immediate(bus, operand_size)?;
                    }
                    // 0x6a PUSH imm8: fetch one byte; executor sign-extends to operand width.
                    0x6a => {
                        insn.imm = u32::from(self.fetch_u8(bus)?);
                    }
                    // 0x8f POP r/m (group 1A): fetch ModRM + addressing descriptor. For
                    // reg!=0 (undefined encoding) leave `operand` as None so the executor can
                    // re-detect the bad reg field and raise the identical error with the same
                    // bytes consumed (mirrors the group-11 approach in DataMove).
                    0x8f => {
                        let modrm = self.fetch_modrm(bus)?;
                        insn.modrm = Some(modrm);
                        if modrm.reg == 0 {
                            let operand =
                                self.parse_addressing_mode(bus, prefixes, address_size, modrm)?;
                            insn.operand = Some(operand);
                        }
                    }
                    // 0xc8 ENTER imm16, imm8: frame size into `imm`, nesting level into `imm2`
                    // (masked to 5 bits here so the executor doesn't have to repeat it).
                    0xc8 => {
                        insn.imm = u32::from(self.fetch_u16(bus)?);
                        insn.imm2 = u32::from(self.fetch_u8(bus)? & 0x1f);
                    }
                    // All other stack opcodes (PUSH/POP reg, PUSH/POP seg, PUSHA/POPA,
                    // PUSHF/POPF, LEAVE) carry no extra encoded bytes.
                    _ => {}
                }
            }
            DecodeGroup::Group => {
                // Arithmetic /ext groups 1-4. Every opcode here is a ModRM whose `reg` field is
                // the sub-op selector; parse the ModRM + addressing descriptor (instruction bytes
                // only, so it stays cacheable) for all of them. Then fetch the immediate ONLY for
                // the opcodes that carry one, mirroring each fused handler's fetch order exactly so
                // the bytes consumed (and thus the fetch clocks charged) are byte-identical:
                //   - group 1 (0x80-0x83): always an immediate. 0x80/0x82 imm8, 0x81 imm16/32,
                //     0x83 a sign-extended imm8 (sign-extend here so the executor takes `imm` as-is,
                //     matching the fused handler which sign-extended at fetch time).
                //   - group 2 count-by-imm8 (0xc0/0xc1): always one imm8 count byte. The 1/CL forms
                //     (0xd0-0xd3) and group 4 (0xfe) carry NO immediate.
                //   - group 3 (0xf6/0xf7): an immediate ONLY for the TEST sub-op. The fused
                //     reference implements TEST as `reg == 0` alone (the `reg == 1` alias is NOT a
                //     TEST there — it falls through to UnsupportedGroupOpcode and consumes no
                //     immediate), so we match it exactly: fetch the immediate only for `reg == 0`.
                //     NOT/NEG/MUL/IMUL/DIV/IDIV (and the undefined reg==1) have none, so the byte
                //     budget here depends on `reg`. Getting this conditional wrong mis-charges the
                //     fetch and diverges from the fused path.
                let modrm = self.fetch_modrm(bus)?;
                let operand = self.parse_addressing_mode(bus, prefixes, address_size, modrm)?;
                insn.modrm = Some(modrm);
                insn.operand = Some(operand);
                match opcode {
                    0x80 | 0x82 => insn.imm = u32::from(self.fetch_u8(bus)?),
                    0x81 => insn.imm = self.fetch_immediate(bus, operand_size)?,
                    0x83 => insn.imm = sign_extend_u8(self.fetch_u8(bus)?),
                    0xc0 | 0xc1 => insn.imm = u32::from(self.fetch_u8(bus)?),
                    0xf6 if modrm.reg == 0 => insn.imm = u32::from(self.fetch_u8(bus)?),
                    0xf7 if modrm.reg == 0 => insn.imm = self.fetch_immediate(bus, operand_size)?,
                    // 0xd0-0xd3 (count 1/CL), 0xfe (INC/DEC), and 0xf6/0xf7 with reg!=0 carry no
                    // immediate after the ModRM.
                    _ => {}
                }
            }
            DecodeGroup::Branch => {
                // Relative-displacement + loop control flow. Every opcode here carries a relative
                // displacement and nothing else; fetch it now (charging its fetch clocks once) and
                // store it sign-extended to i32 in `insn.imm`. The executor replays the SAME
                // `relative_jump(disp, operand_size)` math the fused path used, so the byte width of
                // the sign-extension is what matters and is matched per-opcode here:
                //   - rel8 (Jcc short 0x70-0x7f, the loop/JCXZ branches 0xe0-0xe3, JMP short 0xeb):
                //     one displacement byte, sign-extended.
                //   - rel16/32 (CALL near 0xe8, JMP near 0xe9, two-byte Jcc near 0x0F80-0x0F8F):
                //     operand-size-wide displacement, sign-extended (matching `fetch_relative`).
                // Storing the displacement (not the target) keeps the eip-relative computation in
                // the executor, where eip is already at the instruction end.
                match insn.opcode {
                    0x70..=0x7f | 0xe0..=0xe3 | 0xeb => {
                        insn.imm = self.fetch_i8(bus)? as i32 as u32;
                    }
                    // 0xe8/0xe9 (single byte) and 0x0F80-0x0F8F (two byte) take an operand-size-wide
                    // relative displacement.
                    _ => {
                        insn.imm = self.fetch_relative(bus, operand_size)? as u32;
                    }
                }
            }
            DecodeGroup::ControlFlow => {
                // Far/indirect/RET/INT control flow + 0xff group 5. Each form reads exactly the bytes
                // its fused handler read, in the same order, so the fetch clocks are byte-identical:
                match insn.opcode as u8 {
                    // 0x9a CALL far direct / 0xea JMP far direct: a far pointer immediate — the
                    // offset (operand-size wide) THEN the 16-bit selector, exactly as the fused
                    // handler fetched them. Store the offset in `imm` and the selector in `imm2`; the
                    // executor reconstructs the same far target.
                    0x9a | 0xea => {
                        insn.imm = match operand_size {
                            OperandSize::Word => u32::from(self.fetch_u16(bus)?),
                            OperandSize::Dword => self.fetch_u32(bus)?,
                        };
                        insn.imm2 = u32::from(self.fetch_u16(bus)?);
                    }
                    // 0xc2 RET near imm16 / 0xca RETF imm16: the 16-bit stack-release count is part
                    // of the instruction stream and is fetched BEFORE the executor pops, so read it
                    // here. (The operand size only selects the pop width, not the release width.)
                    0xc2 | 0xca => {
                        insn.imm = u32::from(self.fetch_u16(bus)?);
                    }
                    // 0xcd INT n: the imm8 vector. Read it here; the executor reuses it. (The V86
                    // IOPL check is part of execution, not decode, so it stays in the executor.)
                    0xcd => {
                        insn.imm = u32::from(self.fetch_u8(bus)?);
                    }
                    // 0xff group 5: parse the ModRM + addressing descriptor (instruction bytes only,
                    // so it stays cacheable). The /ext is `modrm.reg`. The indirect CALL/JMP read
                    // their target FROM MEMORY at execute time (resolved against live registers), so
                    // decode captures ONLY the descriptor here — never the target.
                    0xff => {
                        let modrm = self.fetch_modrm(bus)?;
                        let operand =
                            self.parse_addressing_mode(bus, prefixes, address_size, modrm)?;
                        insn.modrm = Some(modrm);
                        insn.operand = Some(operand);
                    }
                    // 0xc3 RET near, 0xcb RETF, 0xcc INT3, 0xce INTO, 0xcf IRET: no encoded operand.
                    _ => {}
                }
            }
            DecodeGroup::FlagsMisc => {
                // Flags + misc register block. Only TEST r/m,reg (0x84/0x85) carries a ModRM; parse
                // it + the addressing-mode descriptor here (instruction bytes only, stays cacheable).
                // Every other A7 opcode carries no encoded operand after the opcode byte — the
                // register/flag operands are implicit (reg field encoded in the opcode or implied).
                match opcode {
                    0x84 | 0x85 => {
                        let modrm = self.fetch_modrm(bus)?;
                        let operand =
                            self.parse_addressing_mode(bus, prefixes, address_size, modrm)?;
                        insn.modrm = Some(modrm);
                        insn.operand = Some(operand);
                    }
                    // INC/DEC reg (0x40-0x4f), CBW/CWDE (0x98), CWD/CDQ (0x99), SAHF/LAHF
                    // (0x9e/0x9f), CMC/CLC/STC/CLI/STI/CLD/STD (0xf5/0xf8-0xfd): no operand bytes.
                    _ => {}
                }
            }
            DecodeGroup::StringOps => {
                // String operations (MOVS/CMPS/STOS/LODS/SCAS). No ModRM, no immediate: the operands
                // are all implicit (DS:SI source, ES:DI destination, the accumulator), so there is
                // nothing to pre-parse here. The REP/REPNE prefix and any segment override were
                // already read into `insn.prefixes` by `read_prefixes` at the top of `decode`; the
                // executor passes them straight through to `run_string`. The element width is derived
                // from the opcode's low bit (byte vs operand-size) in the executor, not the stream.
            }
            DecodeGroup::PortIo => {
                // Port I/O block. The imm8 forms (0xe4-0xe7) carry one port-number byte after the
                // opcode; read it here (charging its instruction-fetch exactly once) and store it in
                // `insn.imm`. The DX forms (0xec-0xef) carry no extra bytes — the port comes from DX
                // at execute time. No ModRM in any form.
                // The imm8 forms carry one port-number byte; the DX forms (0xec..=0xef) do not.
                if let 0xe4..=0xe7 = opcode {
                    insn.imm = u32::from(self.fetch_u8(bus)?);
                }
            }
            DecodeGroup::BitManip => {
                // Two-byte bit-manipulation block. Every opcode is a ModRM r/m form; parse the
                // ModRM + addressing descriptor (instruction bytes only, so it stays cacheable)
                // for all of them. The reg field is the source register for BT/BTS/BTR/BTC reg,
                // SHLD/SHRD, CMPXCHG, and XADD; the destination register for BSF/BSR; and the
                // sub-op selector (the /ext) for the 0F BA group. The bit-offset-adjusted memory
                // address for the BT-memory reg form is computed at EXECUTE from the live reg bit
                // index (in `bit_string_op`), so decode captures only the base descriptor here.
                let modrm = self.fetch_modrm(bus)?;
                let operand = self.parse_addressing_mode(bus, prefixes, address_size, modrm)?;
                insn.modrm = Some(modrm);
                insn.operand = Some(operand);
                // Three forms carry an imm8 AFTER the ModRM+displacement: 0F BA (the bit index)
                // and the SHLD/SHRD imm8 variants 0F A4/AC (the shift count). The CL-count forms
                // 0F A5/AD and the reg-index/reg-source forms carry no immediate.
                if let 0xba | 0xa4 | 0xac = insn.opcode & 0xff {
                    insn.imm = u32::from(self.fetch_u8(bus)?);
                }
            }
            DecodeGroup::CondMove => {
                // SETcc / two-operand IMUL block. Every opcode in
                // this group is a ModRM r/m form with no immediate after the ModRM+displacement.
                // Parse the ModRM + addressing descriptor (instruction bytes only, so it stays
                // cacheable); the executor reads `modrm.reg` (the IMUL destination) and the
                // r/m operand at execute time. No imm8 is ever present, so no `insn.imm` fetch.
                let modrm = self.fetch_modrm(bus)?;
                let operand = self.parse_addressing_mode(bus, prefixes, address_size, modrm)?;
                insn.modrm = Some(modrm);
                insn.operand = Some(operand);
            }
            DecodeGroup::SystemSeg => {
                // System / descriptor-table / segment block (task A12). Every opcode here except
                // CLTS (0F 06) carries a ModRM; the /ext (`modrm.reg`) selects the sub-op for the
                // 0F 00 / 0F 01 groups. Parse the ModRM + addressing descriptor (instruction bytes
                // only, so it stays cacheable). None carry an immediate after the ModRM.
                match insn.opcode {
                    // CLTS: no encoded operand.
                    0x0f06 => {}
                    // MOV reg,CR / MOV CR,reg (0F 20/22) and MOV reg,DR / MOV DR,reg (0F 21/23):
                    // the ModRM is always a register form (the `reg` field is the CR/DR number,
                    // `rm` the GPR). The fused path fetches ONLY the ModRM byte and #UDs when
                    // `mode != 3` BEFORE touching any addressing byte, so do the same here: fetch
                    // the ModRM, store it, and DO NOT parse an addressing mode (a non-register
                    // `mode` is rejected in the executor with no extra fetch).
                    0x0f20..=0x0f23 => {
                        let modrm = self.fetch_modrm(bus)?;
                        insn.modrm = Some(modrm);
                    }
                    // The 0F 00/01/02/03 groups, BOUND (0x62), and LES/LDS (0xc4/0xc5): a normal
                    // ModRM r/m form. Parse the ModRM + its addressing descriptor.
                    _ => {
                        let modrm = self.fetch_modrm(bus)?;
                        let operand =
                            self.parse_addressing_mode(bus, prefixes, address_size, modrm)?;
                        insn.modrm = Some(modrm);
                        insn.operand = Some(operand);
                    }
                }
            }
            DecodeGroup::Fpu => {
                // x87 FPU block (task A13). WAIT/FWAIT (0x9B) has no ModRM — nothing to pre-parse.
                // Each escape opcode (0xD8-0xDF) carries a ModRM: fetch it once here. The fused
                // handler treated `mod == 3` as the register form (it dispatched on the raw ModRM
                // byte WITHOUT decoding an addressing mode) and `mod != 3` as a memory operand (it
                // decoded the addressing mode). Mirror that split exactly so the same instruction
                // bytes are consumed and charged once: store the ModRM always, and parse the
                // addressing descriptor ONLY for the memory forms. No FPU opcode carries an
                // immediate after the ModRM.
                if opcode != 0x9b {
                    let modrm = self.fetch_modrm(bus)?;
                    if modrm.mode != 3 {
                        let operand =
                            self.parse_addressing_mode(bus, prefixes, address_size, modrm)?;
                        insn.operand = Some(operand);
                    }
                    insn.modrm = Some(modrm);
                }
            }
            DecodeGroup::Misc => {
                // The heterogeneous one-off block (task A14). Each opcode reads exactly the bytes
                // its fused handler read, in the same order, so the fetch clocks stay byte-identical.
                match insn.opcode {
                    // Three-operand IMUL: a ModRM r/m form THEN an immediate (operand-size-wide for
                    // 0x69, sign-extended imm8 for 0x6b). Parse the ModRM + addressing descriptor
                    // (instruction bytes only, so it stays cacheable), then fetch the immediate.
                    0x69 => {
                        let modrm = self.fetch_modrm(bus)?;
                        let operand =
                            self.parse_addressing_mode(bus, prefixes, address_size, modrm)?;
                        insn.modrm = Some(modrm);
                        insn.operand = Some(operand);
                        insn.imm = self.fetch_immediate(bus, operand_size)?;
                    }
                    0x6b => {
                        let modrm = self.fetch_modrm(bus)?;
                        let operand =
                            self.parse_addressing_mode(bus, prefixes, address_size, modrm)?;
                        insn.modrm = Some(modrm);
                        insn.operand = Some(operand);
                        insn.imm = sign_extend_u8(self.fetch_u8(bus)?);
                    }
                    // AAM/AAD (0xd4/0xd5): the imm8 base (TEST AL,imm8 0xa8 likewise): fetch one byte.
                    0xa8 | 0xd4 | 0xd5 => {
                        insn.imm = u32::from(self.fetch_u8(bus)?);
                    }
                    // TEST AX/EAX,imm (0xa9): an operand-size-wide accumulator immediate.
                    0xa9 => {
                        insn.imm = self.fetch_immediate(bus, operand_size)?;
                    }
                    // CMPXCHG8B (0F C7 /1): a ModRM r/m (m64) form, no immediate. Parse the ModRM +
                    // addressing descriptor; the executor re-detects the register form / bad /ext.
                    0x0fc7 => {
                        let modrm = self.fetch_modrm(bus)?;
                        let operand =
                            self.parse_addressing_mode(bus, prefixes, address_size, modrm)?;
                        insn.modrm = Some(modrm);
                        insn.operand = Some(operand);
                    }
                    // The MMX shift-by-immediate forms (0F 71/72/73). The fused path read ONLY the
                    // ModRM byte and then the imm8 count — it never decoded an addressing mode (these
                    // are register-form, `modrm.rm` is the target). Mirror that exactly so the byte
                    // budget matches even the malformed mode != 3 encoding: ModRM, then imm8, with no
                    // addressing-descriptor parse.
                    0x0f71..=0x0f73 => {
                        let modrm = self.fetch_modrm(bus)?;
                        insn.modrm = Some(modrm);
                        insn.imm = u32::from(self.fetch_u8(bus)?);
                    }
                    // The rest of the MMX block, except EMMS (0F 77), which has no ModRM and falls to
                    // the no-operand arm below. Every other MMX opcode is a ModRM r/m form: parse the
                    // ModRM + addressing descriptor. (MOVD/MOVQ and the Pxxx forms carry no immediate.)
                    op if op != 0x0f77 && op & 0xff00 == 0x0f00 && is_mmx_two_byte(op as u8) => {
                        let modrm = self.fetch_modrm(bus)?;
                        let operand =
                            self.parse_addressing_mode(bus, prefixes, address_size, modrm)?;
                        insn.modrm = Some(modrm);
                        insn.operand = Some(operand);
                    }
                    // Every other one-off carries no encoded operand after the opcode byte(s):
                    // the BCD adjusts (0x27/0x2f/0x37/0x3f), SALC/XLAT (0xd6/0xd7), INS/OUTS
                    // (0x6c-0x6f), HLT (0xf4), EMMS (0F 77), and the no-operand 0F system/serializing/
                    // CPU-id ops (08/09/30/31/32/a2/c8-cf). XLAT reads memory at execute from
                    // live registers; the rest take implicit/register/no operands.
                    _ => {}
                }
            }
            // Both fallback groups pre-parse nothing in `decode` (the second 0F byte was already
            // folded into `insn.opcode` above): their executors re-read any ModRM/immediate from the
            // post-opcode eip in the shared fused dispatch.
            DecodeGroup::Fallback | DecodeGroup::TwoByteFallback => {}
        }

        // Finalize `len` once, after every group's pre-parse, so a converted group never has to
        // re-write it: a group's match arm only fetches its operand bytes; this single assignment
        // captures the total bytes `decode` consumed (prefixes + opcode + operands). Any future
        // early `Ok` return before this line would skip BOTH `len` and `continuable`.
        insn.len = self.registers.eip.wrapping_sub(start_eip) as u8;
        // Resolve the continuation gate once per decode (the ModRM is in by now), so the
        // per-continuation check in `run_straight_line` reads a single cached flag.
        insn.continuable = block_continuable(insn.group, insn.opcode, insn.modrm, self.persona());

        Ok(insn)
    }

    /// Resolve a pre-decoded ModRM r/m form into its `(ModRm, RmOperand)`: the ModRM (for its `reg`
    /// field) plus the r/m operand resolved against the live registers — a register operand as-is, a
    /// memory descriptor with its effective address recomputed now (`resolve_addr_mode` reads only
    /// base/index registers, no instruction bytes). Centralizes the `decode`-populated `.expect`s so
    /// each group executor doesn't repeat them.
    ///
    /// Shared by every group whose decode arm pre-parses a ModRM (ALU, data-move, and the stack /
    /// group1-5 / bit / system / FPU groups to come): the panic location already names the calling
    /// executor, so the messages stay group-agnostic. Calling this when decode did NOT populate
    /// `modrm`/`operand` (i.e. a non-ModRM form) is a routing bug and panics by design.
    pub(super) fn resolve_decoded_modrm_operand(&self, insn: &DecodedInsn) -> (ModRm, RmOperand) {
        let modrm = insn.modrm.expect("ModRM r/m form decoded with a ModRM");
        let operand = match insn
            .operand
            .expect("ModRM r/m form decoded with an operand")
        {
            DecodedOperand::Reg(index) => RmOperand::Register(index),
            DecodedOperand::Mem(addr) => self.resolve_addr_mode(&addr),
        };
        (modrm, operand)
    }

    /// Apply a guest-visible ISA generation requirement. The 386 rejects 486 and P55C
    /// additions, the 486 rejects P55C additions, and instructions outside the P55C contract
    /// always #UD.
    ///
    /// `decode` applies this once, right after reading the second 0F byte — the same logical point
    /// (and eip) the fused path faulted at — so both the converted split path and the un-converted
    /// fused fallback share a single gate.
    pub(super) fn require_isa_generation(&self, required: IsaGeneration) -> ExecResult<()> {
        if persona_supports(self.persona(), required) {
            return Ok(());
        }
        Err(InternalFault::Exception {
            vector: 6,
            error_code: None,
        })
    }

    fn check_two_byte_isa_gate(&self, second: u8) -> ExecResult<()> {
        self.require_isa_generation(two_byte_isa_generation(second))
    }

    fn read_prefixes<B: CpuBus>(&mut self, bus: &mut B) -> ExecResult<Prefixes> {
        let mut prefixes = Prefixes::default();
        loop {
            let eip = self.registers.eip;
            let byte = self.fetch_u8(bus)?;
            match byte {
                0x26 => prefixes.segment_override = Some(SegmentIndex::Es),
                0x2e => prefixes.segment_override = Some(SegmentIndex::Cs),
                0x36 => prefixes.segment_override = Some(SegmentIndex::Ss),
                0x3e => prefixes.segment_override = Some(SegmentIndex::Ds),
                0x64 => prefixes.segment_override = Some(SegmentIndex::Fs),
                0x65 => prefixes.segment_override = Some(SegmentIndex::Gs),
                // A prefix is idempotent: repeating 66h/67h keeps the override on,
                // it does not toggle it back off (so 66 66 op stays operand-size).
                0x66 => prefixes.operand_size_override = true,
                0x67 => prefixes.address_size_override = true,
                0xf0 => prefixes.lock = true,
                0xf3 => prefixes.rep = Some(RepKind::Repe),
                0xf2 => prefixes.rep = Some(RepKind::Repne),
                _ => {
                    self.registers.eip = eip;
                    return Ok(prefixes);
                }
            }
        }
    }

    fn peek_u8<B: CpuBus>(&mut self, bus: &mut B, offset: u32) -> ExecResult<u8> {
        self.read_memory_u8(
            bus,
            SegmentIndex::Cs,
            offset,
            BusAccessKind::InstructionPrefetch,
        )
    }

    fn check_lock_target<B: CpuBus>(&mut self, bus: &mut B, opcode: u8) -> ExecResult<()> {
        // The byte after the opcode sits at eip (the ModRM, or for 0F the second opcode byte).
        // Peeking re-reads an instruction byte; in real mode it changes no register or memory
        // state. Under paging it may set the page-table accessed bit, as the following fetch
        // would anyway.
        let eip = self.registers.eip;
        let lockable = match opcode {
            // ALU r/m, reg (destination is r/m): ADD/OR/ADC/SBB/AND/SUB/XOR, and XCHG.
            0x00 | 0x01 | 0x08 | 0x09 | 0x10 | 0x11 | 0x18 | 0x19 | 0x20 | 0x21 | 0x28 | 0x29
            | 0x30 | 0x31 | 0x86 | 0x87 => self.peek_u8(bus, eip)? >> 6 != 3,
            // Group ALU 80/81/83: /0..6 write r/m; /7 is CMP (read only, not lockable).
            0x80 | 0x81 | 0x83 => {
                let modrm = self.peek_u8(bus, eip)?;
                modrm >> 6 != 3 && (modrm >> 3) & 7 != 7
            }
            // F6/F7: /2 NOT, /3 NEG write r/m; the other sub-ops do not.
            0xf6 | 0xf7 => {
                let modrm = self.peek_u8(bus, eip)?;
                modrm >> 6 != 3 && matches!((modrm >> 3) & 7, 2 | 3)
            }
            // FE/FF: /0 INC, /1 DEC write r/m; FF /2..7 are CALL/JMP/PUSH (not lockable).
            0xfe | 0xff => {
                let modrm = self.peek_u8(bus, eip)?;
                modrm >> 6 != 3 && matches!((modrm >> 3) & 7, 0 | 1)
            }
            0x0f => {
                let second = self.peek_u8(bus, eip)?;
                match second {
                    // BTS/BTR/BTC r/m, reg write r/m; BT (A3) only reads.
                    0xab | 0xb3 | 0xbb => self.peek_u8(bus, eip.wrapping_add(1))? >> 6 != 3,
                    // BA: /5 BTS, /6 BTR, /7 BTC write; /4 BT only reads.
                    0xba => {
                        let modrm = self.peek_u8(bus, eip.wrapping_add(1))?;
                        modrm >> 6 != 3 && matches!((modrm >> 3) & 7, 5..=7)
                    }
                    // CMPXCHG (B0/B1) and XADD (C0/C1) read-modify-write the r/m destination, so
                    // LOCK is allowed only with a memory operand. The register-dest form is #UD.
                    0xb0 | 0xb1 | 0xc0 | 0xc1 => self.peek_u8(bus, eip.wrapping_add(1))? >> 6 != 3,
                    // CMPXCHG8B (C7 /1) likewise read-modify-writes its m64. LOCK needs a memory
                    // operand; the register form is #UD with or without LOCK.
                    0xc7 => self.peek_u8(bus, eip.wrapping_add(1))? >> 6 != 3,
                    // BSWAP (C8+r) has a register destination and no memory form; INVD (08) and
                    // WBINVD (09) take no operand. LOCK on any of them is #UD (the false arm).
                    _ => false,
                }
            }
            _ => false,
        };
        if lockable {
            Ok(())
        } else {
            Err(InternalFault::Exception {
                vector: 6,
                error_code: None,
            })
        }
    }

    fn operand_size(&self, prefixes: Prefixes) -> OperandSize {
        let default_32 = self.registers.cs().default_size_32;
        if default_32 ^ prefixes.operand_size_override {
            OperandSize::Dword
        } else {
            OperandSize::Word
        }
    }

    fn address_size(&self, prefixes: Prefixes) -> AddressSize {
        let default_32 = self.registers.cs().default_size_32;
        if default_32 ^ prefixes.address_size_override {
            AddressSize::Dword
        } else {
            AddressSize::Word
        }
    }

    fn code_linear_for_offset(&self, offset: u32, width: u32) -> ExecResult<u32> {
        let descriptor = self.registers.cs();
        if descriptor.base == 0 && descriptor.limit == u32::MAX {
            return Ok(offset);
        }
        if offset > descriptor.limit
            || offset.saturating_add(width.saturating_sub(1)) > descriptor.limit
        {
            // 386 PRM 9.9.13: exceeding the CS limit on an instruction fetch is an
            // ordinary #GP(0), not a host-fatal error. This must reach `finish_instruction`
            // as `InternalFault::Exception` (rewind + `deliver_exception`, which already
            // reflects faults into a V86 monitor) rather than `InternalFault::Cpu`, whose
            // `SegmentLimit` variant propagates straight out of `cycle` and halts the machine.
            return Err(InternalFault::Exception {
                vector: 13,
                error_code: Some(0),
            });
        }
        Ok(descriptor.base.wrapping_add(offset))
    }

    fn translate_code_linear<B: CpuBus>(&mut self, bus: &mut B, linear: u32) -> ExecResult<u32> {
        let cs = self.registers.cs();
        let page = linear >> 12;
        if self.code_page.valid && self.code_page.cs == cs && self.code_page.linear_page == page {
            return Ok(self.code_page.physical_page | (linear & 0x0fff));
        }
        let physical = self.translate_linear(bus, linear, false)?;
        self.code_page = CodePageCache {
            valid: true,
            cs,
            linear_page: page,
            physical_page: physical & 0xffff_f000,
        };
        Ok(physical)
    }

    fn refill_prefetch<B: CpuBus>(
        &mut self,
        bus: &mut B,
        offset: u32,
        linear: u32,
    ) -> ExecResult<()> {
        let cs = self.registers.cs();
        let physical = self.translate_code_linear(bus, linear)?;
        let page_remaining = 0x1000 - (linear as usize & 0x0fff);
        let linear_remaining = (u32::MAX - linear) as usize + 1;
        let segment_remaining = if cs.base == 0 && cs.limit == u32::MAX {
            PREFETCH_WINDOW_BYTES
        } else {
            (cs.limit - offset + 1) as usize
        };
        let mut len = PREFETCH_WINDOW_BYTES
            .min(page_remaining)
            .min(linear_remaining)
            .min(segment_remaining);
        let mut bytes = [0u8; PREFETCH_WINDOW_BYTES];
        len = bus.prefetch_memory(physical, &mut bytes[..len])?;
        if len == 0 {
            return Err(BusError::UnmappedMemory { address: physical }.into());
        }
        self.perf.slow_prefetch_refills += 1;
        self.prefetch.bytes[..len].copy_from_slice(&bytes[..len]);
        self.prefetch.cs = cs;
        self.prefetch.linear_base = linear;
        self.prefetch.physical_base = physical;
        self.prefetch.len = len as u8;
        Ok(())
    }

    fn fetch_u8<B: CpuBus>(&mut self, bus: &mut B) -> ExecResult<u8> {
        let offset = self.registers.eip;
        let cs = self.registers.cs();
        let linear = self.code_linear_for_offset(offset, 1)?;
        // Observation seam: the machine recognizes its BIOS INT stub landings
        // by the LINEAR fetch address (a paging guest may shadow the stub
        // page, so the physical address cannot identify it).
        bus.note_code_fetch_linear(linear);
        if let Some((value, physical)) = self.fetch_page.get(cs, linear) {
            self.perf.fetch_page_hits += 1;
            bus.charge_instruction_fetch(physical)?;
            self.registers.eip = self.registers.eip.wrapping_add(1);
            return Ok(value);
        }
        self.perf.fetch_page_misses += 1;
        if let Some((value, physical)) = self.prefetch.get(cs, linear) {
            bus.charge_instruction_fetch(physical)?;
            self.registers.eip = self.registers.eip.wrapping_add(1);
            return Ok(value);
        }
        let physical = self.translate_code_linear(bus, linear)?;
        if let Some(page) = bus.direct_page(physical, BusAccessKind::InstructionPrefetch)? {
            self.perf.direct_page_hits += 1;
            self.fetch_page.put(cs, linear, page);
            let (value, physical) = self
                .fetch_page
                .get(cs, linear)
                .expect("fetch page refilled");
            bus.charge_instruction_fetch(physical)?;
            self.registers.eip = self.registers.eip.wrapping_add(1);
            return Ok(value);
        }
        self.perf.direct_page_misses += 1;
        self.refill_prefetch(bus, offset, linear)?;
        let (value, physical) = self.prefetch.get(cs, linear).expect("prefetch refilled");
        bus.charge_instruction_fetch(physical)?;
        self.registers.eip = self.registers.eip.wrapping_add(1);
        Ok(value)
    }

    fn fetch_i8<B: CpuBus>(&mut self, bus: &mut B) -> ExecResult<i8> {
        Ok(self.fetch_u8(bus)? as i8)
    }

    fn fetch_u16<B: CpuBus>(&mut self, bus: &mut B) -> ExecResult<u16> {
        let low = self.fetch_u8(bus)?;
        let high = self.fetch_u8(bus)?;
        Ok(u16::from_le_bytes([low, high]))
    }

    fn fetch_u32<B: CpuBus>(&mut self, bus: &mut B) -> ExecResult<u32> {
        let b0 = self.fetch_u8(bus)?;
        let b1 = self.fetch_u8(bus)?;
        let b2 = self.fetch_u8(bus)?;
        let b3 = self.fetch_u8(bus)?;
        Ok(u32::from_le_bytes([b0, b1, b2, b3]))
    }

    fn fetch_immediate<B: CpuBus>(
        &mut self,
        bus: &mut B,
        operand_size: OperandSize,
    ) -> ExecResult<u32> {
        match operand_size {
            OperandSize::Word => Ok(u32::from(self.fetch_u16(bus)?)),
            OperandSize::Dword => self.fetch_u32(bus),
        }
    }

    fn fetch_relative<B: CpuBus>(
        &mut self,
        bus: &mut B,
        operand_size: OperandSize,
    ) -> ExecResult<i32> {
        match operand_size {
            OperandSize::Word => Ok(i32::from(self.fetch_u16(bus)? as i16)),
            OperandSize::Dword => Ok(self.fetch_u32(bus)? as i32),
        }
    }

    fn fetch_moffs<B: CpuBus>(
        &mut self,
        bus: &mut B,
        address_size: AddressSize,
    ) -> ExecResult<u32> {
        match address_size {
            AddressSize::Word => Ok(u32::from(self.fetch_u16(bus)?)),
            AddressSize::Dword => self.fetch_u32(bus),
        }
    }

    fn fetch_modrm<B: CpuBus>(&mut self, bus: &mut B) -> ExecResult<ModRm> {
        let value = self.fetch_u8(bus)?;
        Ok(ModRm {
            mode: value >> 6,
            reg: (value >> 3) & 0x07,
            rm: value & 0x07,
        })
    }

    /// Parse a ModRM addressing mode into a descriptor. Reads only instruction bytes
    /// (displacement, SIB) and never a general register, so the result can be replayed after
    /// the registers change. The effective offset is computed later by `resolve_addr_mode`.
    fn parse_addressing_mode<B: CpuBus>(
        &mut self,
        bus: &mut B,
        prefixes: Prefixes,
        address_size: AddressSize,
        modrm: ModRm,
    ) -> ExecResult<DecodedOperand> {
        if modrm.mode == 3 {
            return Ok(DecodedOperand::Reg(modrm.rm));
        }

        let mut addr = match address_size {
            AddressSize::Word => self.parse_16bit_address(bus, modrm)?,
            AddressSize::Dword => self.parse_32bit_address(bus, modrm)?,
        };
        if let Some(segment) = prefixes.segment_override {
            addr.segment = segment;
        }
        Ok(DecodedOperand::Mem(addr))
    }

    /// Resolve an addressing-mode descriptor into a live memory operand by reading the base
    /// and index registers now. Reads only general registers (no instruction bytes), so it is
    /// safe to call repeatedly on a cached descriptor.
    #[inline]
    pub(super) fn resolve_memory_addr_mode(&self, addr: &AddrMode) -> MemoryOperand {
        let disp = addr.disp as u32;
        let offset = match addr.address_size {
            AddressSize::Word => {
                let base = match addr.base {
                    Some(reg) => u32::from(self.read_gpr16(reg)),
                    None => 0,
                };
                let index = match addr.index {
                    Some(reg) => u32::from(self.read_gpr16(reg)),
                    None => 0,
                };
                let sum = base.wrapping_add(index).wrapping_add(disp);
                (sum as u16) as u32
            }
            AddressSize::Dword => {
                let base = match addr.base {
                    Some(reg) => self.read_gpr32(reg),
                    None => 0,
                };
                let index = match addr.index {
                    Some(reg) => {
                        let value = self.read_gpr32(reg);
                        if addr.scale == 1 {
                            value
                        } else {
                            value.wrapping_mul(u32::from(addr.scale))
                        }
                    }
                    None => 0,
                };
                base.wrapping_add(index).wrapping_add(disp)
            }
        };
        MemoryOperand {
            segment: addr.segment,
            offset,
        }
    }

    #[inline]
    fn resolve_addr_mode(&self, addr: &AddrMode) -> RmOperand {
        RmOperand::Memory(self.resolve_memory_addr_mode(addr))
    }

    fn parse_16bit_address<B: CpuBus>(
        &mut self,
        bus: &mut B,
        modrm: ModRm,
    ) -> ExecResult<AddrMode> {
        // 16-bit addressing combines a fixed pair of registers; encode each pair as the
        // descriptor's (base, index) with scale 1. bx=3, bp=5, si=6, di=7.
        let mut uses_bp = false;
        let (base, index) = match modrm.rm {
            0 => (Some(3), Some(6)), // bx+si
            1 => (Some(3), Some(7)), // bx+di
            2 => {
                uses_bp = true;
                (Some(5), Some(6)) // bp+si
            }
            3 => {
                uses_bp = true;
                (Some(5), Some(7)) // bp+di
            }
            4 => (None, Some(6)),                 // si
            5 => (None, Some(7)),                 // di
            6 if modrm.mode == 0 => (None, None), // disp16 only
            6 => {
                uses_bp = true;
                (Some(5), None) // bp
            }
            _ => (Some(3), None), // bx
        };

        let disp = match modrm.mode {
            0 if modrm.rm == 6 => i32::from(self.fetch_u16(bus)? as i16),
            0 => 0,
            1 => self.fetch_i8(bus)? as i32,
            2 => i32::from(self.fetch_u16(bus)? as i16),
            _ => 0,
        };

        let segment = if uses_bp {
            SegmentIndex::Ss
        } else {
            SegmentIndex::Ds
        };
        Ok(AddrMode {
            segment,
            base,
            index,
            scale: 1,
            disp,
            address_size: AddressSize::Word,
        })
    }

    fn parse_32bit_address<B: CpuBus>(
        &mut self,
        bus: &mut B,
        modrm: ModRm,
    ) -> ExecResult<AddrMode> {
        let mut base_reg = None;
        let mut index_reg = None;
        let mut scale = 1u8;

        if modrm.rm == 4 {
            let sib = self.fetch_u8(bus)?;
            scale = 1 << (sib >> 6);
            let idx = (sib >> 3) & 0x07;
            if idx != 4 {
                index_reg = Some(idx);
            }
            let base = sib & 0x07;
            if !(modrm.mode == 0 && base == 5) {
                base_reg = Some(base);
            }
        } else if !(modrm.mode == 0 && modrm.rm == 5) {
            base_reg = Some(modrm.rm);
        }

        let disp = match modrm.mode {
            0 if base_reg.is_none() => self.fetch_u32(bus)? as i32,
            0 => 0,
            1 => self.fetch_i8(bus)? as i32,
            2 => self.fetch_u32(bus)? as i32,
            _ => 0,
        };
        let segment = if matches!(base_reg, Some(4 | 5)) {
            SegmentIndex::Ss
        } else {
            SegmentIndex::Ds
        };
        Ok(AddrMode {
            segment,
            base: base_reg,
            index: index_reg,
            scale,
            disp,
            address_size: AddressSize::Dword,
        })
    }
}
