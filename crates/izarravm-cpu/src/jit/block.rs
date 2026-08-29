// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Generic forward-decode basic-block builder. Starting at any hot linear PC,
//! `build_block` walks the decode cache forward, collecting continuable slots until the first
//! block terminator. It once fed the region JIT's compiler (`try_admit`); that execution engine is
//! gone, and the builder's sole surviving consumer is the poll-loop scanner (`build_poll_loop`),
//! which walks the same slot table to recognize a handful of certified device-poll shapes.
//!
//! ## What the builder still vouches for (the poll scanner's invariants)
//!
//! - The block is a maximal run of interior-eligible slots. `continuable` (resolved once at decode,
//!   `block_continuable`) covers MOST of the spec §2.9 terminator predicate, inverted: it excludes
//!   control-flow mutators, CR/DR/segment/paging changers, HLT, far transfers, INT/IRET, MOV-CR/DR,
//!   LGDT.., OUT, INS/OUTS, and the clock readers RDTSC/WRMSR (all non-continuable). IN and
//!   TEST-acc-imm ARE admitted as interior continuations in the Approximate class (the
//!   poll-loop win); they are runtime step-breaks, not compile-time terminators.
//! - Only the TERMINAL slot may transfer control, change interrupt visibility, or replace DS. An interior slot
//!   must fall through to `lin+len` (else the next slot's snapshot would not be what runs), so
//!   branches / near RET / near indirect CALL/JMP always end the block. A relative branch whose
//!   static target is the entry is a self-loop back-edge (the drawcolumn case, generalized).
//! - `continuable` alone is NOT the terminator predicate: it admits STI/CLI/POPF, segment loads,
//!   and the SS-loads (the interpreter runs them inline with a per-instruction interrupt check),
//!   so those IF/shadow changers also end the block as its terminal slot
//!   (`changes_interrupt_visibility`). This is the spec §2.9 behavioral predicate, enumerated from
//!   the interpreter's own IF-writer and shadow-arming sites.
//! - Every slot's decode is live in the cache (generation-current), unprefixed, and contained in
//!   the entry's 4 KB page.

use crate::{
    AddressSize, CpuGsw, CpuPersona, DecodeGroup, DecodedInsn, DecodedOperand, OperandSize,
    PollBranchShape, PollLoop, PollMaskSource, PollMemoryFields, PollPortSource, Prefixes,
    SegmentIndex,
};

use super::step::Slot;

/// Empirically-derived (not a per-opcode sum) core-clock cost of one iteration
/// of the certified memory-poll shape (M1: `CMP r32,DS:[disp32]` (6 bytes) +
/// `Jcc rel8` (2 bytes) back to entry), measured from the interpreter's own
/// per-iteration charge for these exact bytes and pinned by the machine-side
/// state+timing identity test
/// `memory_poll_skip_matches_the_interpreter_at_batch_boundaries`. Mirrors how
/// 17/21/28 were pinned for the io shapes: not derivable from the direct-JIT
/// `DirectKind::raw_clocks` table, which has no entry for a memory-operand CMP
/// (that path never reaches native codegen).
const MEMORY_POLL_RAW_CORE_CLOCKS: u64 = 5;

/// Cap on a built block's slot count, to bound the scan. A block that reaches the cap ends
/// linearly (its tail is interpreted); the poll shapes the scanner looks for are far smaller.
const MAX_BLOCK_SLOTS: usize = 128;

/// Whether an instruction transfers control (so it cannot be an interior slot: after it, EIP is
/// not `lin+len`). These are exactly the `continuable` forms that do not fall through:
/// - the whole Branch group (Jcc / JMP rel / LOOP / JCXZ / CALL rel),
/// - near RET (0xC2/0xC3),
/// - the near indirect CALL (0xFF /2) and JMP (0xFF /4) within ControlFlow (its /0 /1 /6 forms
///   are INC/DEC/PUSH r/m, which DO fall through and stay interior).
///
/// Everything else `continuable` (ALU/DataMove/Stack/Group/FlagsMisc/BitManip/CondMove/Fpu/
/// StringOps, plus the Approximate-class IN and TEST) falls through and is interior.
pub(crate) fn is_control_transfer(insn: &DecodedInsn) -> bool {
    match insn.group {
        DecodeGroup::Branch => true,
        DecodeGroup::ControlFlow => {
            matches!(insn.opcode, 0xc2 | 0xc3)
                || (insn.opcode == 0xff && matches!(insn.modrm.map(|m| m.reg), Some(2) | Some(4)))
        }
        _ => false,
    }
}

/// The precise transfer shape the unit simulator records for `insn` starting at linear `lin` in the
/// code segment based at `cs_base`. This hook emits the EXACT kind of every control transfer; the
/// sim's `effective_kind` then lowers the rich kinds per its config (the default config lowers all
/// four to `Indirect`, preserving v1 semantics). The classification:
/// - a relative near JMP (0xEB / 0xE9) or Jcc (short 0x70-0x7F, near 0x0F80-0x0F8F) has a statically
///   computable target and is `DirectNear`;
/// - a near CALL rel (0xE8) is `CallNear` with the same statically computable target (recursion, not
///   a loop back-edge);
/// - a LOOP/LOOPcc/JCXZ near branch (0xE0-0xE3) is `LoopNear` with the same rel8 target arithmetic;
/// - a near indirect CALL (0xFF /2) is `CallIndirect` (no static target);
/// - a near RET (0xC2/0xC3) is `Return`;
/// - every other `is_control_transfer` form (the near indirect JMP 0xFF /4) is `Indirect`;
/// - a non-control-transfer instruction is `None`.
///
/// Reuses `is_control_transfer` as the single control-flow authority, so the far / INT / IRET /
/// RETF terminators (not `continuable`, hence never `is_control_transfer` here) fall to `None` and
/// are closed by the sim's `is_terminator` check.
///
/// Target convention: the sim keys everything by LINEAR address, but `relative_jump` masks the new
/// EIP with `operand_size.mask()` (a 16-bit-operand branch truncates the IP within the segment), so
/// the delta arithmetic runs in offset-within-segment space and only then rebases: `target =
/// cs_base + ((lin - cs_base + len + imm) & mask)`. Without the mask a wrapping 16-bit branch would
/// record a wrong target. `CallNear` and `LoopNear` use the IDENTICAL arithmetic (they too carry a
/// sign-extended relative displacement in `insn.imm`).
pub(crate) fn observed_transfer(
    insn: &DecodedInsn,
    lin: u32,
    cs_base: u32,
) -> super::unit_sim::TransferKind {
    use super::unit_sim::TransferKind;
    if !is_control_transfer(insn) {
        return TransferKind::None;
    }
    // The masked, rebased linear target shared by DirectNear / CallNear / LoopNear (all three carry
    // a sign-extended relative displacement in `insn.imm`).
    let relative_target = || {
        let target_eip = lin
            .wrapping_sub(cs_base)
            .wrapping_add(u32::from(insn.len))
            .wrapping_add(insn.imm)
            & insn.operand_size.mask();
        cs_base.wrapping_add(target_eip)
    };
    match insn.opcode {
        0xeb | 0xe9 | 0x70..=0x7f | 0x0f80..=0x0f8f => TransferKind::DirectNear {
            target: relative_target(),
        },
        0xe8 => TransferKind::CallNear {
            target: relative_target(),
        },
        0xe0..=0xe3 => TransferKind::LoopNear {
            target: relative_target(),
        },
        0xc2 | 0xc3 => TransferKind::Return,
        // Near indirect CALL is 0xFF /2; the only other `is_control_transfer` 0xFF form is /4 (near
        // indirect JMP), which stays `Indirect`.
        0xff if matches!(insn.modrm.map(|m| m.reg), Some(2)) => TransferKind::CallIndirect,
        _ => TransferKind::Indirect,
    }
}

/// Whether the decoded operand is a memory reference (as opposed to a register or no operand).
fn operand_is_mem(insn: &DecodedInsn) -> bool {
    matches!(insn.operand, Some(DecodedOperand::Mem(_)))
}

/// Whether `insn` MAY write memory, for the rung-P poll-wait detector (feature `jit`, diagnostic
/// only; consumed exclusively via `ObservedInsn::writes_memory` at rung P). CONSERVATIVE: it returns
/// `false` only for forms we can confirm never store to memory, and `true` for everything else,
/// because a false positive merely under-detects a poll loop (safe) while a false negative could
/// elide a loop that mutates state (unsafe). The recognized non-writers cover the shapes a device
/// poll loop is built from - port IN, TEST/CMP against memory, register ALU, memory loads, branches.
/// A CALL / PUSH / string writer / anything unrecognized is disqualifying (`true`).
pub(crate) fn writes_memory(insn: &DecodedInsn) -> bool {
    match insn.group {
        // Relative branches and port IN/OUT never touch memory operands.
        DecodeGroup::Branch | DecodeGroup::PortIo => false,
        // TEST r/m,reg (0x84/0x85) reads; INC/DEC reg, CBW.., the flag ops, SAHF/LAHF write no memory.
        DecodeGroup::FlagsMisc => false,
        // ALU forms 0-5: a store to a memory r/m happens only for the store-direction forms 0/1 of a
        // non-CMP operation with a memory operand. CMP (operation 7) and the load/accumulator forms
        // (2-5) never write memory.
        DecodeGroup::Alu => {
            let operation = (insn.opcode >> 3) & 7;
            let form = insn.opcode & 7;
            operation != 7 && matches!(form, 0 | 1) && operand_is_mem(insn)
        }
        // DataMove: the store-direction MOV/XCHG r/m forms write memory when the operand is memory;
        // MOV moffs,accumulator (0xa2/0xa3) always writes memory. Loads (0x8a/0x8b/0xa0/0xa1),
        // LEA (0x8d), MOV Sreg (0x8e), MOV reg,imm, XCHG reg,eAX, and MOVZX/MOVSX write no memory.
        DecodeGroup::DataMove => match insn.opcode {
            0x88 | 0x89 | 0x8c | 0x86 | 0x87 | 0xc6 | 0xc7 => operand_is_mem(insn),
            0xa2 | 0xa3 => true,
            _ => false,
        },
        // The /ext groups 1-4: a register-form r/m writes no memory. For a memory r/m, CMP (group 1
        // reg 7) and TEST (group 3 reg 0/1) read only; every other sub-op (ALU-imm, shift/rotate,
        // NOT/NEG/MUL/IMUL/DIV read-modify, INC/DEC) is treated as a memory write (MUL/DIV are
        // conservatively included though they only read the operand).
        DecodeGroup::Group => {
            if !operand_is_mem(insn) {
                return false;
            }
            let reg = insn.modrm.map_or(0, |m| m.reg);
            match insn.opcode {
                0x80..=0x83 => reg != 7,
                0xf6 | 0xf7 => reg >= 2,
                _ => true,
            }
        }
        // The heterogeneous catch-all: only the accumulator-immediate TEST forms (0xA8/0xA9) are
        // confirmed non-writers here, and they are exactly the common poll-loop compare (Doom's
        // `in al,dx; test al,8; jz`). Everything else in Misc (string port I/O, IMUL-imm, BCD, XLAT,
        // CPUID/RDTSC, ...) is conservatively disqualifying.
        DecodeGroup::Misc => !matches!(insn.opcode, 0xa8 | 0xa9),
        // Stack (push/pop/call/enter), control flow (call/int push), string ops (movs/stos), FPU,
        // system/segment, and every remaining form: conservatively disqualifying.
        _ => true,
    }
}

/// Whether an instruction changes interrupt visibility (IF/TF) or arms the one-instruction
/// interrupt shadow. These are `continuable` (the interpreter runs them inline, with its
/// per-instruction interrupt-transition check), but the region DEFERS that check to the whole-
/// region boundary, so an IF-writer cannot sit INSIDE a region: a mid-region transition would be
/// seen a slot too late. They therefore end the block as its TERMINAL slot (spec §2.9 category b):
/// the region executes the change last and returns, so `run_straight_line`'s own post-region
/// interrupt-transition check fires at exactly the boundary the interpreter would break at (for
/// STI and the SS-loads, the armed shadow correctly suppresses that check until the next
/// interpreted instruction consumes it). The set:
/// - STI (0xFB) / CLI (0xFA): write IF (STI also arms the shadow).
/// - POPF/POPFD (0x9D): write IF and TF from the stack.
/// - POP SS (0x17), MOV SS (0x8E /2), LSS (0x0F B2): arm the SS-load shadow (386 PRM 11-16).
///   Only the SS destination arms it.
pub(crate) fn changes_interrupt_visibility(insn: &DecodedInsn) -> bool {
    match insn.opcode {
        0xfa | 0xfb | 0x9d | 0x17 | 0x0fb2 => true,
        0x8e => matches!(insn.modrm.map(|m| m.reg), Some(2)),
        _ => false,
    }
}

/// Whether an instruction replaces DS. Native byte-memory probes are admitted only for flat DS and
/// entry checks cannot protect a later slot if an interior instruction changes that descriptor.
pub(crate) fn changes_native_memory_context(insn: &DecodedInsn) -> bool {
    insn.opcode == 0x1f || (insn.opcode == 0x8e && insn.modrm.is_some_and(|modrm| modrm.reg == 3))
}

/// The static target of a relative branch that could be a self-loop back-edge, in linear space,
/// or `None` when the instruction is not such a branch. The Branch-group relative forms (Jcc /
/// JMP rel / LOOP / JCXZ) store the sign-extended displacement in `insn.imm`, so the target is
/// `lin + len + imm`. CALL rel (0xE8) is excluded: it pushes a return address, so a "call to
/// entry" is recursion, not a loop back-edge, and the native back-edge would drop the push.
fn loop_back_edge_target(insn: &DecodedInsn, lin: u32) -> Option<u32> {
    if insn.group != DecodeGroup::Branch || insn.opcode == 0xe8 {
        return None;
    }
    Some(lin.wrapping_add(u32::from(insn.len)).wrapping_add(insn.imm))
}

/// Forward-decode a basic block starting at `entry_lin`, returning its slot table and whether it
/// is a self-loop. `None` when the entry is cold (the interpreter warms it; a later scan retries)
/// or the entry itself is a terminator (nothing to build). Mirrors `run_straight_line`'s own
/// continuation gate: warm, unprefixed, `continuable`, and contained in the entry's code page.
///
/// The block extends until the first control transfer (which becomes the terminal slot) or until
/// the next instruction is a terminator / page-crosses / is cold (the last sequential slot is then
/// terminal).
pub(crate) fn build_block(cpu: &CpuGsw, entry_lin: u32, d: bool) -> Option<(Vec<Slot>, bool)> {
    let mut slots: Vec<Slot> = Vec::new();
    let mut lin = entry_lin;
    let entry_page = entry_lin & !0xfff;
    let is_loop = loop {
        if lin & !0xfff != entry_page {
            break false;
        }
        let insn = match cpu.decode_cache.get(lin, d) {
            Some(i) => i,
            None => break false, // cold ahead: the block is whatever we have so far (linear).
        };
        let physical = cpu.decode_cache.line_phys_start(lin, d)?;
        // The run loop's own continuation gate: unprefixed + continuable + page-local. A slot that
        // fails it is a terminator; the block ends before it (linear).
        if insn.prefixes != Prefixes::default() || !insn.continuable {
            break false;
        }
        if (lin & 0xfff) + u32::from(insn.len) > 0x1000 {
            break false;
        }
        // An instruction ends the block as its terminal slot if it transfers control (after it
        // EIP is not `lin+len`) or changes interrupt visibility (the region defers its interrupt
        // check to the boundary, so an IF/shadow change must be the last thing it does).
        let ends_block = is_control_transfer(&insn)
            || changes_interrupt_visibility(&insn)
            || changes_native_memory_context(&insn);
        let this_lin = lin;
        lin = lin.wrapping_add(u32::from(insn.len));
        slots.push(Slot {
            insn,
            lin: this_lin,
            physical,
        });
        if ends_block {
            // Terminal slot. `is_loop` only when it is a relative branch back to the entry; an
            // interrupt-visibility change is never a back-edge, so it ends a linear block.
            break loop_back_edge_target(&insn, this_lin) == Some(entry_lin);
        }
        if slots.len() >= MAX_BLOCK_SLOTS {
            break false; // cap reached; end linearly, the tail interprets.
        }
    };
    if slots.is_empty() {
        return None; // the entry is a terminator: nothing to build.
    }
    Some((slots, is_loop))
}

/// Whether every slot's last byte is inside the LIVE code segment's limit.
///
/// **Under the 16-bit slice (design D1) this is also the IP-WRAP catcher**, and that role
/// is load-bearing rather than incidental. `build_block` walks LINEARLY, so a shape that
/// wraps IP produces a slot at `cs.base + 0x10000 + k`, whose `slot_eip` is `>= 0x10000`.
/// `fetch_within_limit` is `eip <= limit - (len - 1)`, so with the `cs.limit <= 0xFFFF`
/// admission term in force (the 3-slot arm's `!d` branch) that slot fails here and the
/// shape refuses `NegativeVolatile`. No certified slot can then start at or past
/// `0x10000`, and none can have a last byte above `0xFFFF` -- which is why the admission
/// term delegates the boundary arithmetic here instead of carrying its own `slot_eip +
/// len` compare, and why it has no off-by-one at the segment top.
fn poll_slots_within_live_cs(cpu: &CpuGsw, slots: &[Slot]) -> bool {
    let cs = cpu.registers.cs();
    slots.iter().all(|slot| {
        let slot_eip = slot.lin.wrapping_sub(cs.base);
        CpuGsw::fetch_within_limit(slot_eip, slot.insn.len, cs.limit)
    })
}

type PollFetches = ([(u32, u32, u8); 6], u8);

fn poll_fetches(slots: &[Slot]) -> Option<PollFetches> {
    let fetch_count = u8::try_from(slots.len()).ok()?;
    if fetch_count > 6 {
        return None;
    }
    let mut fetches = [(0, 0, 0); 6];
    for (fetch, slot) in fetches.iter_mut().zip(slots) {
        *fetch = (slot.lin, slot.physical, slot.insn.len);
    }
    Some((fetches, fetch_count))
}

fn exact_poll_test_branch(test: &Slot, branch: &Slot) -> Option<(u8, bool)> {
    if test.insn.opcode != 0xa8 || test.insn.len != 2 || branch.insn.len != 2 {
        return None;
    }
    let mask = u8::try_from(test.insn.imm).ok()?;
    if !matches!(mask, 0x01 | 0x08) || !matches!(branch.insn.opcode, 0x74 | 0x75) {
        return None;
    }
    Some((mask, branch.insn.opcode == 0x74))
}

/// One backward-scan probe's outcome, split so the negative cache only ever
/// stores results that are pure functions of the page's warm decode lines
/// and the code-segment d bit.
pub(crate) enum PollScanOutcome {
    Found(PollLoop),
    /// No shape matched, for code-byte-only reasons: cacheable until the
    /// next warm-line insert on the page.
    NegativeCacheable,
    /// A structural shape matched but a register or segment check failed
    /// (3-slot EDX port source, within-live-CS limits). The same bytes can
    /// classify differently under other register/segment state: never cache.
    /// Forward rule: any new register- or segment-dependent check added to a
    /// shape must return NegativeVolatile, never NegativeCacheable.
    NegativeVolatile,
}

/// Recognize only the reviewed 3DA poll-loop shapes from the same warm decode-cache
/// view used by compiled blocks. Re-running this before every span makes an SMC
/// restamp replace the descriptor rather than reusing stale bytes or addresses.
/// Slot opcodes here are mirrored in poll_head_possible's set; extending the
/// shapes requires extending that set -- with ONE documented exemption, the
/// `sixteen_bit_ok`-only D1b test slot (`0x84`), which cannot reach that
/// prefilter at all. See `poll_head_possible`'s own doc.
///
/// The 3-slot IO arm is the only one that admits a 16-bit code segment, and only
/// when the caller passes `sixteen_bit_ok` (the Direct call-out under
/// `IZARRAVM_DIRECT_POLL_SKIP_16`). No byte of that shape decodes differently
/// under `CS.D = 0`: `0xEC` is fixed 1 byte and always an 8-bit port read into
/// AL, the accumulator `TEST` forms and `Jcc rel8` are operand-size-invariant
/// encodings of fixed length, and `raw_core_clocks: 17` is likewise
/// operand-size independent (`IN_PORT_CORE_CLOCKS` 12 + `TEST` 2 + taken `Jcc`
/// 3, from the same constants the interpreter charges). The other two shape
/// families carry real `OperandSize::Dword` / `AddressSize::Dword` terms, whose
/// unprefixed 16-bit forms are DIFFERENT instructions with different lengths, so
/// they stay 32-bit-only.
///
/// `current` is the scan's ORIGIN -- the instruction start every `at_head` computed
/// here is measured against -- and it is a caller-supplied value rather than a fresh
/// `cpu.linear_eip()` read so `build_poll_loop_from` can call this with the same
/// origin for every `entry` candidate it tries walking backward from `current`. The
/// interpreter's own caller (`build_poll_loop`) passes `cpu.linear_eip()`, so its
/// `at_head` reads exactly as before; a call-out helper passes the slot's own linear
/// EIP instead (GP2 poll-skip design, obligation 1).
///
/// `sixteen_bit_ok` is the 16-bit poll certification slice's admission parameter
/// (design D1/D1b, review round-1 MAJOR-5). It is `false` at every INTERPRETER call
/// site and `true` only at the Direct call-out under `IZARRAVM_DIRECT_POLL_SKIP_16`,
/// which is what keeps `CpuGsw::poll_loop`'s path byte-for-byte unchanged. A parameter
/// rather than a shared screen, deliberately: dropping `poll_head_possible`'s `!d`
/// would have widened the interpreter's poll path, and no ladder arm runs the
/// interpreter.
fn build_poll_loop_at(
    cpu: &CpuGsw,
    entry: u32,
    current: u32,
    sixteen_bit_ok: bool,
) -> PollScanOutcome {
    let _ = sixteen_bit_ok;
    let d = cpu.registers.cs().default_size_32;
    let Some((slots, is_loop)) = build_block(cpu, entry, d) else {
        return PollScanOutcome::NegativeCacheable;
    };

    // 3-slot direct shape. Structure first, register check second, so a
    // register failure is reported volatile instead of cached.
    if let [input, test, branch] = slots.as_slice()
        && (d || sixteen_bit_ok)
        && is_loop
        && input.insn.opcode == 0xec
        && input.insn.len == 1
        && loop_back_edge_target(&branch.insn, branch.lin) == Some(entry)
    {
        let Some((mask, branch_when_zero)) = exact_poll_test_branch(test, branch) else {
            return PollScanOutcome::NegativeCacheable;
        };
        // D-O1, the 16-bit slice's ONE added admission term. `CS.D = 0` with a limit
        // above 0xFFFF -- a 16-bit protected-mode code segment with G=1, or any
        // descriptor whose D bit and limit disagree -- is the single reachable state
        // where IP wraps at 0xFFFF and the limit check does NOT catch it. It is also
        // what makes the call-out's unmasked scan anchor (`cs.base + eip + slot_delta`)
        // exact, because the compile walk's own guarantee is written in terms of
        // `cs.limit`. VOLATILE, never cacheable: `cs.limit` is a segment fact and the
        // negative cache is keyed on `(lin, d)` alone, so a cached refusal here would
        // poison the entry for every other segment state over the same bytes.
        if !d && cpu.registers.cs().limit > 0xffff {
            return PollScanOutcome::NegativeVolatile;
        }
        if cpu.registers.edx() as u16 != 0x03da {
            return PollScanOutcome::NegativeVolatile;
        }
        if !poll_slots_within_live_cs(cpu, &slots) {
            return PollScanOutcome::NegativeVolatile;
        }
        let Some((fetches, fetch_count)) = poll_fetches(&slots) else {
            return PollScanOutcome::NegativeCacheable;
        };
        return PollScanOutcome::Found(PollLoop {
            fetches,
            fetch_count,
            port_source: PollPortSource::CurrentDx,
            branch_shape: PollBranchShape::Direct,
            status_mask: mask,
            mask_source: PollMaskSource::Immediate(mask),
            branch_when_zero,
            raw_core_clocks: 17,
            at_head: current == entry,
            memory: None,
        });
    }

    // 2-slot memory-compare shape (M1: `CMP r32,DS:[disp32]` with no base and
    // no index register; terminal `Jcc rel8` back to entry). The bare-disp32
    // restriction is the safety condition for register/segment invariance
    // (hazard e in the design doc): with no base and no index, the effective
    // linear address depends on NO GPR at all, only DS's base/limit, which
    // `poll_slots_within_live_cs` re-checks fresh on every call. Unconditional
    // under IZARRAVM_POLL_SKIP like every other certified shape (the
    // campaign-only IZARRAVM_POLL_SKIP_MEMORY sub-flag was folded back after
    // the memory-marginal proof was accepted).
    if let [cmp, branch] = slots.as_slice()
        && d
        && is_loop
        && cmp.insn.opcode == 0x3b
        && cmp.insn.len == 6
        && cmp.insn.operand_size == OperandSize::Dword
        && matches!(branch.insn.opcode, 0x74 | 0x75)
        && branch.insn.len == 2
        && loop_back_edge_target(&branch.insn, branch.lin) == Some(entry)
    {
        let Some(modrm) = cmp.insn.modrm else {
            return PollScanOutcome::NegativeCacheable;
        };
        let Some(DecodedOperand::Mem(addr)) = cmp.insn.operand else {
            return PollScanOutcome::NegativeCacheable;
        };
        if addr.address_size != AddressSize::Dword
            || addr.segment != SegmentIndex::Ds
            || addr.base.is_some()
            || addr.index.is_some()
        {
            return PollScanOutcome::NegativeCacheable;
        }
        if !poll_slots_within_live_cs(cpu, &slots) {
            return PollScanOutcome::NegativeVolatile;
        }
        let Some((fetches, fetch_count)) = poll_fetches(&slots) else {
            return PollScanOutcome::NegativeCacheable;
        };
        let ds_base = cpu.registers.segment(SegmentIndex::Ds).base;
        let linear = ds_base.wrapping_add(addr.disp as u32);
        return PollScanOutcome::Found(PollLoop {
            fetches,
            fetch_count,
            port_source: PollPortSource::CurrentDx,
            branch_shape: PollBranchShape::Direct,
            status_mask: 0,
            mask_source: PollMaskSource::Immediate(0),
            branch_when_zero: false,
            raw_core_clocks: MEMORY_POLL_RAW_CORE_CLOCKS,
            at_head: current == entry,
            memory: Some(PollMemoryFields {
                linear,
                width: 4,
                comparand_gpr: modrm.reg,
                spins_while_equal: branch.insn.opcode == 0x74,
            }),
        });
    }

    // 5/6-slot shapes: the structural predicate chain is unchanged; each
    // structural mismatch becomes NegativeCacheable and each
    // poll_slots_within_live_cs failure becomes NegativeVolatile.
    let [setup, clear, input, test, branch] = slots.as_slice() else {
        return PollScanOutcome::NegativeCacheable;
    };
    if !d
        || setup.insn.opcode != 0x89
        || setup.insn.len != 2
        || setup.insn.operand_size != OperandSize::Dword
        || clear.insn.opcode != 0x29
        || clear.insn.len != 2
        || clear.insn.operand_size != OperandSize::Dword
        || clear
            .insn
            .modrm
            .is_none_or(|modrm| (modrm.mode, modrm.reg, modrm.rm) != (3, 0, 0))
        || input.insn.opcode != 0xec
        || input.insn.len != 1
    {
        return PollScanOutcome::NegativeCacheable;
    }
    let port_source = match setup
        .insn
        .modrm
        .map(|modrm| (modrm.mode, modrm.reg, modrm.rm))
    {
        Some((3, 3, 2)) => PollPortSource::Ebx,
        Some((3, 1, 2)) => PollPortSource::Ecx,
        _ => return PollScanOutcome::NegativeCacheable,
    };
    let Some((mask, branch_when_zero)) = exact_poll_test_branch(test, branch) else {
        return PollScanOutcome::NegativeCacheable;
    };
    let Some(branch_target) = loop_back_edge_target(&branch.insn, branch.lin) else {
        return PollScanOutcome::NegativeCacheable;
    };
    if is_loop && branch_target == entry {
        if !poll_slots_within_live_cs(cpu, &slots) {
            return PollScanOutcome::NegativeVolatile;
        }
        let Some((fetches, fetch_count)) = poll_fetches(&slots) else {
            return PollScanOutcome::NegativeCacheable;
        };
        return PollScanOutcome::Found(PollLoop {
            fetches,
            fetch_count,
            port_source,
            branch_shape: PollBranchShape::Direct,
            status_mask: mask,
            mask_source: PollMaskSource::Immediate(mask),
            branch_when_zero,
            raw_core_clocks: 21,
            at_head: current == entry,
            memory: None,
        });
    }

    let Some(jmp_entry) = entry.checked_add(9) else {
        return PollScanOutcome::NegativeCacheable;
    };
    let Some(exit) = entry.checked_add(11) else {
        return PollScanOutcome::NegativeCacheable;
    };
    if is_loop
        || branch_target != exit
        || jmp_entry & !0x0fff != entry & !0x0fff
        || exit.wrapping_sub(1) & !0x0fff != entry & !0x0fff
    {
        return PollScanOutcome::NegativeCacheable;
    }
    let Some((jmp_slots, jmp_is_loop)) = build_block(cpu, jmp_entry, true) else {
        return PollScanOutcome::NegativeCacheable;
    };
    let [jmp] = jmp_slots.as_slice() else {
        return PollScanOutcome::NegativeCacheable;
    };
    if jmp_is_loop
        || jmp.insn.opcode != 0xeb
        || jmp.insn.len != 2
        || loop_back_edge_target(&jmp.insn, jmp.lin) != Some(entry)
    {
        return PollScanOutcome::NegativeCacheable;
    }
    if !poll_slots_within_live_cs(cpu, &slots)
        || !poll_slots_within_live_cs(cpu, std::slice::from_ref(jmp))
    {
        return PollScanOutcome::NegativeVolatile;
    }
    let Some((mut fetches, _)) = poll_fetches(&slots) else {
        return PollScanOutcome::NegativeCacheable;
    };
    fetches[5] = (jmp.lin, jmp.physical, jmp.insn.len);
    PollScanOutcome::Found(PollLoop {
        fetches,
        fetch_count: 6,
        port_source,
        branch_shape: PollBranchShape::PairedJmp,
        status_mask: mask,
        mask_source: PollMaskSource::Immediate(mask),
        branch_when_zero,
        raw_core_clocks: 28,
        at_head: current == entry,
        memory: None,
    })
}

/// Find an exact poll shape containing `current`. The bounded backward scan stays in
/// `current`'s code page and accepts only captured slot starts. A negative aggregates
/// the per-entry outcomes: cacheable only when every probe on the page rejected for
/// code-byte reasons, volatile if any probe hit a register or segment gate.
///
/// EXTRACTED FOR THE CALL-OUT-SITE POLL SKIP, and the parameter is the whole point.
/// Inside a Direct block `registers.eip` is the BLOCK-ENTRY value throughout (side
/// exits install the slot's EIP from a compiled delta; nothing updates `registers.eip`
/// per slot), so a classifier that read `cpu.linear_eip()` from inside a call-out
/// helper would scan around the wrong address and certify nothing. The slot's own
/// guest linear EIP is a compile-time constant the emitter hands the helper, and THAT
/// is what must drive both the scan origin and every `at_head` computed against it.
///
/// `build_poll_loop(cpu)` is this with `cpu.linear_eip()`, so the interpreter path is
/// byte-for-byte unchanged.
pub(crate) fn build_poll_loop_from(
    cpu: &CpuGsw,
    current: u32,
    sixteen_bit_ok: bool,
) -> PollScanOutcome {
    let page = current & !0x0fff;
    let mut volatile_seen = false;
    for back in 0..=9u32 {
        let Some(entry) = current.checked_sub(back) else {
            break;
        };
        if entry & !0x0fff != page {
            break;
        }
        match build_poll_loop_at(cpu, entry, current, sixteen_bit_ok) {
            PollScanOutcome::Found(poll) => {
                if (0..poll.fetch_count()).any(|index| {
                    poll.fetch(index)
                        .is_some_and(|(linear, _, _)| linear == current)
                }) {
                    return PollScanOutcome::Found(poll);
                }
                // A shape exists here but does not contain the current EIP:
                // that fact is a pure function of the code bytes, so the
                // aggregate negative stays cacheable.
            }
            PollScanOutcome::NegativeCacheable => {}
            PollScanOutcome::NegativeVolatile => volatile_seen = true,
        }
    }
    if volatile_seen {
        PollScanOutcome::NegativeVolatile
    } else {
        PollScanOutcome::NegativeCacheable
    }
}

/// Find an exact poll shape containing the current instruction start.
/// `build_poll_loop_from(cpu, cpu.linear_eip())` -- see that function's doc for why the
/// origin is a parameter rather than an internal read.
pub(crate) fn build_poll_loop(cpu: &CpuGsw) -> PollScanOutcome {
    // `sixteen_bit_ok: false` -- the INTERPRETER's value, unconditionally. See
    // `build_poll_loop_at`'s doc: 16-bit admission is scoped to the Direct call-out,
    // and a partial revert of that slice cannot silently open this path.
    build_poll_loop_from(cpu, cpu.linear_eip(), false)
}

/// Loop-head prefilter: whether the current boundary could possibly be
/// inside a certified poll shape. Every shape requires 32-bit code (d) and
/// every Found's fetch set contains the current EIP as a slot START, whose
/// opcode is one of the fixed set below (3-slot: IN/TEST/Jcc; 5-slot adds
/// MOV 0x89 and SUB 0x29; paired adds JMP 0xEB; the memory shape M1 adds CMP
/// 0x3B, shared with the io shapes' Jcc 0x74/0x75). A cold line cannot be a
/// slot (build_block chains only warm lines), and warming goes through
/// `put`, which bumps the page insert generation guarding any cached
/// negative. Containment is structural, so this rejection is a
/// code-byte-only fact under EVERY register state, even when a nearby
/// shape would scan as register-volatile. Extending the shape table in build_poll_loop_at requires
/// extending this set; the every-phase tests
/// exact_setup_poll_shapes_cover_sources_senses_and_every_phase and
/// exact_memory_poll_shape_covers_senses_and_every_phase fail if any slot
/// opcode goes missing here.
fn poll_head_possible(cpu: &CpuGsw, lin: u32, d: bool) -> bool {
    if !d {
        return false;
    }
    match cpu.decode_cache.get(lin, d) {
        Some(insn) => matches!(
            insn.opcode,
            0x89 | 0x29 | 0xec | 0xa8 | 0x74 | 0x75 | 0xeb | 0x3b
        ),
        None => false,
    }
}

impl CpuGsw {
    /// Conservative machine-level poll-skip entry gate. This mirrors the direct
    /// IN permission fast path and rejects every state where the interpreter
    /// would consult the modeled TSS I/O permission bitmap.
    pub fn poll_skip_eligible(&self) -> bool {
        matches!(self.persona(), CpuPersona::I486 | CpuPersona::I586)
            && !self.jit_direct.backend_enabled()
            && !self.profile.enabled
            && !crate::run::diff_trace_enabled()
            && !self.interrupt_shadow
            && (!self.is_protected_mode()
                || (!self.is_v86_mode() && self.current_privilege_level() <= self.iopl()))
    }

    /// Classify the current warm loop head. Positives are rebuilt on every
    /// call (SMC restamps must replace descriptors). Structural negatives
    /// are answered from the negative cache when the page's insert
    /// generation is unchanged; register-dependent negatives are never
    /// cached. `&mut self` mutates host bookkeeping only (cache slots and
    /// perf counters), never guest state. The loop-head prefilter answers
    /// the dominant non-poll boundaries before any cache probe or scan.
    /// Tally what `poll_loop` WOULD have answered here, without answering it and without
    /// touching anything it maintains.
    ///
    /// Deliberately does NOT call `poll_loop`: that wrapper bumps `poll_head_prefilter_rejects`
    /// and writes the negative cache, and a probe that moved either would be measuring itself.
    /// It calls the two read-only pieces directly, both of which take `&CpuGsw`.
    ///
    /// Deliberately does NOT consult `poll_skip_eligible` either. That predicate is false
    /// whenever the Direct backend is enabled, which is precisely the configuration this probe
    /// exists to measure; gating on it would return "no" on every fixture and answer nothing.
    /// The persona and privilege terms of that predicate are still worth knowing, so they are
    /// applied here in the same order minus the backend term.
    #[cfg(feature = "poll-head-probe")]
    pub fn probe_poll_head(&mut self) {
        let eligible_ignoring_backend = matches!(
            self.persona(),
            crate::CpuPersona::I486 | crate::CpuPersona::I586
        ) && !self.interrupt_shadow
            && (!self.is_protected_mode()
                || (!self.is_v86_mode() && self.current_privilege_level() <= self.iopl()));
        if !eligible_ignoring_backend {
            self.poll_head_probe.head_line_cold += 1;
            return;
        }
        let lin = self.linear_eip();
        let d = self.registers.cs().default_size_32;
        if !poll_head_possible(self, lin, d) {
            self.poll_head_probe.prefilter_reject += 1;
            return;
        }
        match build_poll_loop(self) {
            PollScanOutcome::Found(poll) => {
                self.poll_head_probe.found += 1;
                if let Some((linear, _, _)) = poll.fetch(0) {
                    self.poll_head_probe.last_found_head = linear;
                }
            }
            PollScanOutcome::NegativeCacheable => self.poll_head_probe.negative_cacheable += 1,
            PollScanOutcome::NegativeVolatile => self.poll_head_probe.negative_volatile += 1,
        }
    }

    /// Read the probe tally. Diagnostic only.
    #[cfg(feature = "poll-head-probe")]
    pub fn poll_head_probe(&self) -> crate::PollHeadProbeCounters {
        self.poll_head_probe
    }

    pub fn poll_loop(&mut self) -> Option<PollLoop> {
        if !self.poll_skip_eligible() {
            return None;
        }
        let lin = self.linear_eip();
        let d = self.registers.cs().default_size_32;
        if !poll_head_possible(self, lin, d) {
            self.perf.poll_head_prefilter_rejects += 1;
            return None;
        }
        if self.poll_neg_cache_enabled && self.decode_cache.poll_negative_live(lin, d) {
            self.perf.poll_neg_cache_hits += 1;
            return None;
        }
        match build_poll_loop(self) {
            PollScanOutcome::Found(poll) => Some(poll),
            PollScanOutcome::NegativeCacheable => {
                if self.poll_neg_cache_enabled {
                    self.perf.poll_neg_cache_stores += 1;
                    self.decode_cache.record_poll_negative(lin, d);
                }
                None
            }
            PollScanOutcome::NegativeVolatile => {
                self.perf.poll_neg_cache_volatile += 1;
                None
            }
        }
    }
}
