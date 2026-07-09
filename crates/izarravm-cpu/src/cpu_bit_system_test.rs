// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

/// One golden end-state for a bit-manipulation case (task A10). BT/BTS/BTR/BTC, BSF/BSR,
/// SHLD/SHRD, CMPXCHG, and XADD all set flags (CF for BT-family, ZF for BSF/BSR/CMPXCHG, the
/// full ALU set for SHLD/SHRD/CMPXCHG/XADD), write registers, and — for the memory r/m forms —
/// write memory, so this captures the full register file, eflags, eip, memory-write deltas, and
/// the InstructionPrefetch fetch count. `eip` proves decode consumed the right number of bytes
/// (incl. the 0F second byte and the imm8 for 0F BA/A4/AC); `fetch` proves each instruction byte
/// was charged exactly once.
struct BitManipGolden {
    name: &'static str,
    code: &'static [u8],
    gpr: [u32; 8],
    eflags: u32,
    eip: u32,
    deltas: &'static [(usize, u8)],
    fetch: usize,
}

/// Seed for the bit-manipulation golden battery. Real-mode, DS=0, with a scratch word region
/// the memory r/m forms address. Registers are chosen so each op has a non-trivial, observable
/// result: BX=3 (a bit index that exercises CF and the set/reset/toggle write-backs), CX=0x0008
/// (so the BTR/BTC register cases find bit 3 already set), and a known pattern at the scratch
/// region for the memory BT-walk cases. The instruction is placed at offset 0; the scratch
/// region starts at 0x40.
fn bitmanip_seed(cpu: &mut CpuGsw) {
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    // AX=0x0034 (accumulator for CMPXCHG: matches the planted dest 0x0034 so the equal branch
    // fires), CX=0x0008 (bit 3 set, for BTR/BTC register cases), DX=0x0506, BX=3 (bit index /
    // CMPXCHG-XADD source), SP=0x00f0, BP=0x0010, SI=0x0008, DI=0x0018.
    cpu.write_reg16(Reg16::Ax, 0x0034);
    cpu.write_reg16(Reg16::Cx, 0x0008);
    cpu.write_reg16(Reg16::Dx, 0x0506);
    cpu.write_reg16(Reg16::Bx, 0x0003);
    cpu.write_reg16(Reg16::Sp, 0x00f0);
    cpu.write_reg16(Reg16::Bp, 0x0010);
    cpu.write_reg16(Reg16::Si, 0x0008);
    cpu.write_reg16(Reg16::Di, 0x0018);
    // eflags: only the always-set reserved bit 1.
    cpu.registers.eflags = 0x02;
}

/// Lay the instruction bytes at offset 0 and plant the scratch data the memory r/m forms read.
/// Word at 0x40 = 0x1234 (the BTS positive-index walk lands in the NEXT word at 0x42, proving
/// the bit-offset addressing), byte at 0x40 = 0x34 also serves as the CMPXCHG/XADD byte dest.
fn bitmanip_seed_mem(mem: &mut [u8], code: &[u8]) {
    mem[..code.len()].copy_from_slice(code);
    // Scratch words: 0x40 = 0x1234, 0x42 = 0x0000, 0x44 = 0xffff (so a positive walk into 0x42
    // sets a bit in a zero word, observable as a clean single-byte delta).
    mem[0x40..0x42].copy_from_slice(&0x1234u16.to_le_bytes());
    mem[0x42..0x44].copy_from_slice(&0x0000u16.to_le_bytes());
    mem[0x44..0x46].copy_from_slice(&0xffffu16.to_le_bytes());
}

/// The bit-manipulation differential battery (task A10). Captured from the PRIOR fused reference
/// (`execute_instruction_legacy`) via `regen_bitmanip_goldens`; see `alu_golden_cases` for the
/// full capture recipe. Never edit by hand — re-run the regen from the pre-split commit
/// (parent 430a6051) WHILE the fused arms (0F A3/AB/B3/BB/BA/BC/BD/A4/A5/AC/AD/B0/B1/C0/C1)
/// still exist in `execute_two_byte`, then paste, then delete the fused arms.
fn bitmanip_golden_cases() -> &'static [BitManipGolden] {
    &[
        // BT CX, BX (0F A3 D9): test bit BX=3 of CX=0x0008 (bit 3 set) -> CF=1, no write.
        BitManipGolden {
            name: "bt cx,bx (0f a3 d9)",
            code: &[15, 163, 217],
            gpr: [52, 8, 1286, 3, 240, 16, 8, 24],
            eflags: 0x3,
            eip: 0x3,
            deltas: &[],
            fetch: 4,
        },
        // BTS CX, BX (0F AB D9): set bit 3 of CX=0x0008 (already set) -> CF=1, CX unchanged.
        BitManipGolden {
            name: "bts cx,bx (0f ab d9)",
            code: &[15, 171, 217],
            gpr: [52, 8, 1286, 3, 240, 16, 8, 24],
            eflags: 0x3,
            eip: 0x3,
            deltas: &[],
            fetch: 4,
        },
        // BTR CX, BX (0F B3 D9): reset bit 3 of CX=0x0008 -> CF=1 (old bit), CX=0x0000.
        BitManipGolden {
            name: "btr cx,bx (0f b3 d9)",
            code: &[15, 179, 217],
            gpr: [52, 0, 1286, 3, 240, 16, 8, 24],
            eflags: 0x3,
            eip: 0x3,
            deltas: &[],
            fetch: 4,
        },
        // BTC CX, BX (0F BB D9): toggle bit 3 of CX=0x0008 -> CF=1 (old), CX=0x0000.
        BitManipGolden {
            name: "btc cx,bx (0f bb d9)",
            code: &[15, 187, 217],
            gpr: [52, 0, 1286, 3, 240, 16, 8, 24],
            eflags: 0x3,
            eip: 0x3,
            deltas: &[],
            fetch: 4,
        },
        // BTS [0x40], BX (0F AB 1E 40 00): BX=3 -> sets bit 3 of the word at 0x40=0x1234.
        // (No walk: index 3 < 16, lands in the first word.) 0x1234 has bit 3 clear, so the low
        // byte goes 0x34 -> 0x3c (=60): delta (64, 60). CF=0 (old bit clear).
        BitManipGolden {
            name: "bts [0x40],bx no-walk (0f ab 1e 40 00)",
            code: &[15, 171, 30, 64, 0],
            gpr: [52, 8, 1286, 3, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x5,
            deltas: &[(64, 60)],
            fetch: 6,
        },
        // BTS [0x40], DX with DX=16 -> bit index 16 walks to the NEXT word at 0x42 (the subtle
        // BT-memory case): sets bit 0 of the 0x0000 word at 0x42, so the delta is at byte 66
        // (=0x42), NOT the base 0x40. This is the load-bearing assertion for bit-offset
        // addressing: the write must land in the adjacent element. DX is overridden to 16.
        BitManipGolden {
            name: "bts [0x40],dx walk-to-next-word (0f ab 16 40 00)",
            code: &[15, 171, 22, 64, 0],
            gpr: [52, 8, 16, 3, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x5,
            deltas: &[(66, 1)],
            fetch: 6,
        },
        // BTS [0x40], imm8=5 (0F BA 2E 40 00 05): /5=BTS, fixed imm8 index 5 -> bit 5 of the
        // word at 0x40=0x1234 is already set, so CF=1 and NO memory write (no delta). Proves the
        // imm8 form addresses the base word and the unchanged-write path.
        BitManipGolden {
            name: "bts [0x40],5 (0f ba 2e 40 00 05)",
            code: &[15, 186, 46, 64, 0, 5],
            gpr: [52, 8, 1286, 3, 240, 16, 8, 24],
            eflags: 0x3,
            eip: 0x6,
            deltas: &[],
            fetch: 7,
        },
        // BT CX, imm8=3 (0F BA E1 03): /4=BT, mod=3 rm=CX -> CF = bit 3 of CX=0x0008 = 1.
        BitManipGolden {
            name: "bt cx,3 (0f ba e1 03)",
            code: &[15, 186, 225, 3],
            gpr: [52, 8, 1286, 3, 240, 16, 8, 24],
            eflags: 0x3,
            eip: 0x4,
            deltas: &[],
            fetch: 5,
        },
        // BSF BX, CX (0F BC D9): CX=0x0008 -> lowest set bit index 3 into BX, ZF=0.
        BitManipGolden {
            name: "bsf bx,cx (0f bc d9)",
            code: &[15, 188, 217],
            gpr: [52, 8, 1286, 3, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x3,
            deltas: &[],
            fetch: 4,
        },
        // BSR BX, CX (0F BD D9): CX=0x0008 -> highest set bit index 3 into BX, ZF=0.
        BitManipGolden {
            name: "bsr bx,cx (0f bd d9)",
            code: &[15, 189, 217],
            gpr: [52, 8, 1286, 3, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x3,
            deltas: &[],
            fetch: 4,
        },
        // BSF BX, CX with CX=0 (0F BC D9, CX overridden to 0): ZF=1 (eflags 0x42), BX preserved
        // at its preset 0xbeef (=48879). Proves the zero-source path leaves the destination.
        BitManipGolden {
            name: "bsf bx,cx zero-src (0f bc d9)",
            code: &[15, 188, 217],
            gpr: [52, 0, 1286, 48879, 240, 16, 8, 24],
            eflags: 0x42,
            eip: 0x3,
            deltas: &[],
            fetch: 4,
        },
        // SHLD AX, BX, imm8=4 (0F A4 D8 04): mod=3 reg=BX rm=AX. AX=0x0034, BX=3 -> shifts AX
        // left 4, filling from BX's high bits -> AX=0x0340 (=832). Proves the imm8 count + flags.
        BitManipGolden {
            name: "shld ax,bx,4 (0f a4 d8 04)",
            code: &[15, 164, 216, 4],
            gpr: [832, 8, 1286, 3, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x4,
            deltas: &[],
            fetch: 5,
        },
        // SHRD AX, BX, imm8=4 (0F AC D8 04): shifts AX right 4, filling from BX's low bits ->
        // AX=0x3003 (=12291), CF=1 + PF (eflags 0x6).
        BitManipGolden {
            name: "shrd ax,bx,4 (0f ac d8 04)",
            code: &[15, 172, 216, 4],
            gpr: [12291, 8, 1286, 3, 240, 16, 8, 24],
            eflags: 0x6,
            eip: 0x4,
            deltas: &[],
            fetch: 5,
        },
        // SHLD AX, BX, CL (0F A5 D8): CL=8 (CX=0x0008 -> CL=8) -> shift AX left 8 -> AX=0x3400
        // (=13312).
        BitManipGolden {
            name: "shld ax,bx,cl (0f a5 d8)",
            code: &[15, 165, 216],
            gpr: [13312, 8, 1286, 3, 240, 16, 8, 24],
            eflags: 0x6,
            eip: 0x3,
            deltas: &[],
            fetch: 4,
        },
        // SHRD AX, BX, CL (0F AD D8): CL=8 -> shift AX right 8 -> AX=0x0300 (=768).
        BitManipGolden {
            name: "shrd ax,bx,cl (0f ad d8)",
            code: &[15, 173, 216],
            gpr: [768, 8, 1286, 3, 240, 16, 8, 24],
            eflags: 0x6,
            eip: 0x3,
            deltas: &[],
            fetch: 4,
        },
        // CMPXCHG [0x40], BL byte form (0F B0 1E 40 00): AL=0x34 == dest byte 0x34 -> equal:
        // ZF=1 (eflags 0x46), store BL=3 into [0x40]: delta (64, 3). The equal branch + write.
        BitManipGolden {
            name: "cmpxchg [0x40],bl equal (0f b0 1e 40 00)",
            code: &[15, 176, 30, 64, 0],
            gpr: [52, 8, 1286, 3, 240, 16, 8, 24],
            eflags: 0x46,
            eip: 0x5,
            deltas: &[(64, 3)],
            fetch: 6,
        },
        // CMPXCHG CX, BX word form (0F B1 D9): AX=0x0034 != CX=0x0008 -> unequal: ZF=0
        // (eflags 0x12), load CX into AX (AX=0x0008). Register dest, the unequal re-write.
        BitManipGolden {
            name: "cmpxchg cx,bx unequal (0f b1 d9)",
            code: &[15, 177, 217],
            gpr: [8, 8, 1286, 3, 240, 16, 8, 24],
            eflags: 0x12,
            eip: 0x3,
            deltas: &[],
            fetch: 4,
        },
        // XADD BL, CL byte form (0F C0 CB): mod=3 reg=CL(1) rm=BL(3). dest=BL=3, src=CL=8 ->
        // BL=11, CL=3 (old dest), flags like ADD(3,8).
        BitManipGolden {
            name: "xadd bl,cl (0f c0 cb)",
            code: &[15, 192, 203],
            gpr: [52, 3, 1286, 11, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x3,
            deltas: &[],
            fetch: 4,
        },
        // XADD [0x40], CX word form (0F C1 0E 40 00): dest=word[0x40]=0x1234, src=CX=0x0008 ->
        // [0x40]=0x123c (low byte 0x34 -> 0x3c=60: delta (64, 60)), CX=0x1234 (=4660, old dest),
        // flags like ADD. Proves the memory XADD path.
        BitManipGolden {
            name: "xadd [0x40],cx (0f c1 0e 40 00)",
            code: &[15, 193, 14, 64, 0],
            gpr: [52, 4660, 1286, 3, 240, 16, 8, 24],
            eflags: 0x6,
            eip: 0x5,
            deltas: &[(64, 60)],
            fetch: 6,
        },
    ]
}

/// Per-case register overrides applied AFTER `bitmanip_seed`, so a few cases can drive an
/// operand the default seed doesn't cover (the BT-memory walk needs DX=16; the BSF zero-source
/// case needs CX=0). Applied identically on both the split and the fused (regen) path so the
/// goldens stay a faithful differential. Returns None when the default seed suffices.
fn bitmanip_case_override(name: &str, cpu: &mut CpuGsw) {
    match name {
        "bts [0x40],dx walk-to-next-word (0f ab 16 40 00)" => {
            // DX=16 so the bit index walks one 16-bit element past 0x40, into the word at 0x42.
            cpu.write_reg16(Reg16::Dx, 16);
        }
        "bsf bx,cx zero-src (0f bc d9)" => {
            cpu.write_reg16(Reg16::Cx, 0);
            cpu.write_reg16(Reg16::Bx, 0xbeef); // preset so "destination unchanged" is visible
        }
        _ => {}
    }
}

#[test]
fn bitmanip_split_matches_golden_across_ops() {
    // The bit-manipulation opcodes (BT/BTS/BTR/BTC reg+imm8, BSF/BSR, SHLD/SHRD imm8+CL,
    // CMPXCHG, XADD) are converted to the decode/execute split, so their fused arms are deleted
    // and they can no longer be diffed against a fused executor in-tree. Run each case through
    // cycle() (the split) and assert the architectural end-state against goldens captured from
    // the pre-split fused path via `regen_bitmanip_goldens`. The register file proves the
    // set/reset/toggle write-backs, BSF/BSR indices, double-shift results, and the CMPXCHG/XADD
    // exchanges; eflags proves CF (BT-family), ZF (BSF/BSR/CMPXCHG), and the ALU flags
    // (SHLD/SHRD/CMPXCHG/XADD); the memory deltas prove the memory r/m write path — crucially
    // the BT-memory walk lands the write in the ADJACENT word, not the base word; eip + fetch
    // prove decode consumed and charged every byte (0F prefix + ModRM + imm8) exactly once.
    for g in bitmanip_golden_cases() {
        let mut mem = vec![0u8; 0x100];
        bitmanip_seed_mem(&mut mem, g.code);
        let initial = mem.clone();

        let mut split = CpuGsw::default();
        bitmanip_seed(&mut split);
        bitmanip_case_override(g.name, &mut split);
        let mut sbus = TestBus::with_memory(mem);
        let _ = split.cycle(&mut sbus);

        assert_eq!(split.registers.gpr, g.gpr, "gpr mismatch for {}", g.name);
        assert_eq!(split.eflags(), g.eflags, "eflags mismatch for {}", g.name);
        assert_eq!(split.registers.eip, g.eip, "eip mismatch for {}", g.name);
        let deltas: Vec<(usize, u8)> = sbus
            .memory
            .iter()
            .enumerate()
            .filter(|(i, b)| **b != initial[*i])
            .map(|(i, b)| (i, *b))
            .collect();
        assert_eq!(deltas, g.deltas, "memory-write mismatch for {}", g.name);
        assert_eq!(
            seam_fetch_count(&sbus),
            g.fetch,
            "instruction-fetch cycle count mismatch for {} (seam must charge fetches once)",
            g.name
        );
    }
}

/// Regenerate `bitmanip_golden_cases` from the fused reference. Ignored by default.
/// Run WHILE the bit-manipulation fused arms still exist in `execute_two_byte`
/// (i.e. the parent commit 430a6051):
///   git worktree add ../regen-a10 430a6051
///   cd ../regen-a10
///   cargo test -p izarravm-cpu --lib regen_bitmanip_goldens -- --ignored --nocapture
/// then paste the output over `bitmanip_golden_cases` and only then delete the fused arms.
#[test]
#[ignore = "prints golden literals; run with --ignored --nocapture against the fused reference"]
fn regen_bitmanip_goldens() {
    for g in bitmanip_golden_cases() {
        let mut mem = vec![0u8; 0x100];
        bitmanip_seed_mem(&mut mem, g.code);
        let initial = mem.clone();

        let mut fused = CpuGsw::default();
        bitmanip_seed(&mut fused);
        bitmanip_case_override(g.name, &mut fused);
        let mut fbus = TestBus::with_memory(mem);
        fused.begin_instruction();
        if exec_one_split(&mut fused, &mut fbus).is_err() {
            println!(
                "            // TODO regen {}: fused path unavailable here; run against the base commit",
                g.name
            );
            continue;
        }
        let deltas: Vec<(usize, u8)> = fbus
            .memory
            .iter()
            .enumerate()
            .filter(|(i, b)| **b != initial[*i])
            .map(|(i, b)| (i, *b))
            .collect();
        let fetch = seam_fetch_count(&fbus);
        println!(
            "            BitManipGolden {{ name: {:?}, code: &{:?}, gpr: {:?}, eflags: {:#x}, eip: {:#x}, deltas: &{:?}, fetch: {} }},",
            g.name,
            g.code,
            fused.registers.gpr,
            fused.eflags(),
            fused.registers.eip,
            deltas,
            fetch,
        );
    }
}

// ── Task A11: condmove golden battery ────────────────────────────────────────────────────────

/// One golden end-state for a condmove case (task A11). CMOVcc, SETcc, and IMUL reg,r/m all
/// touch the register file and/or memory and leave eflags unchanged (CMOVcc/SETcc) or set
/// CF/OF (IMUL), so this captures the full register file, eflags, eip, memory-write deltas,
/// and the InstructionPrefetch fetch count. `eip` proves decode consumed the right number of
/// bytes (incl. the 0F second byte and the ModRM+displacement); `fetch` proves each byte
/// was charged exactly once.
struct CondMoveGolden {
    name: &'static str,
    code: &'static [u8],
    gpr: [u32; 8],
    eflags: u32,
    eip: u32,
    deltas: &'static [(usize, u8)],
    fetch: usize,
}

/// Seed for the condmove golden battery. Real-mode, DS=0, 16-bit addressing. AX=5, BX=3,
/// CX=0x0100, DX=0x4000; eflags has ZF=0 (only the reserved bit-1). Scratch memory at
/// 0x40 holds the word 0x0003 (CMOVcc memory source); byte at 0x50 is zero (SETcc mem dest).
fn condmove_seed(cpu: &mut CpuGsw) {
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 5);
    cpu.write_reg16(Reg16::Bx, 3);
    cpu.write_reg16(Reg16::Cx, 0x0100);
    cpu.write_reg16(Reg16::Dx, 0x4000);
    cpu.write_reg16(Reg16::Sp, 0x00f0);
    cpu.write_reg16(Reg16::Bp, 0x0010);
    cpu.write_reg16(Reg16::Si, 0x0008);
    cpu.write_reg16(Reg16::Di, 0x0018);
    cpu.registers.eflags = 0x02; // ZF=0
}

fn condmove_seed_mem(mem: &mut [u8], code: &[u8]) {
    mem[..code.len()].copy_from_slice(code);
    mem[0x40..0x42].copy_from_slice(&3u16.to_le_bytes()); // word 3 for CMOVcc memory source
}

/// The condmove differential battery (task A11). Captured from the PRIOR fused reference
/// (`execute_instruction_legacy`) via `regen_condmove_goldens` (parent commit 93bdff3f) WHILE
/// the fused arms (CMOVcc 0x40-0x4F, SETcc 0x90-0x9F, IMUL 0xAF) still existed in
/// `execute_two_byte`. Never edit by hand — re-run the regen from the pre-split commit.
fn condmove_golden_cases() -> &'static [CondMoveGolden] {
    &[
        // SETcc false: SETZ AL (0F 94 C0): ZF=0 → condition false → AL=0 (AX=0x0000).
        CondMoveGolden {
            name: "setz al false (0f 94 c0)",
            code: &[15, 148, 192],
            gpr: [0, 256, 16384, 3, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x3,
            deltas: &[],
            fetch: 4,
        },
        // SETcc true: SETNZ BL (0F 95 C3): ZF=0 → condition true → BL=1 (BX=0x0001).
        CondMoveGolden {
            name: "setnz bl true (0f 95 c3)",
            code: &[15, 149, 195],
            gpr: [5, 256, 16384, 1, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x3,
            deltas: &[],
            fetch: 4,
        },
        // SETcc mem false: SETZ [0x50] (0F 94 1E 50 00): ZF=0 → write 0 to [0x50] (no delta, mem
        // already 0). Proves the byte-wide memory write fires even for the false condition.
        CondMoveGolden {
            name: "setz [0x50] false (0f 94 1e 50 00)",
            code: &[15, 148, 30, 80, 0],
            gpr: [5, 256, 16384, 3, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x5,
            deltas: &[],
            fetch: 6,
        },
        // SETcc mem true: SETNZ [0x50] (0F 95 1E 50 00): ZF=0 → write 1 to [0x50]; delta (80, 1).
        CondMoveGolden {
            name: "setnz [0x50] true (0f 95 1e 50 00)",
            code: &[15, 149, 30, 80, 0],
            gpr: [5, 256, 16384, 3, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x5,
            deltas: &[(80, 1)],
            fetch: 6,
        },
        // CMOVcc false: CMOVZ AX, BX (0F 44 C3): ZF=0 → condition false → AX unchanged (=5).
        CondMoveGolden {
            name: "cmovz ax,bx false (0f 44 c3)",
            code: &[15, 68, 195],
            gpr: [5, 256, 16384, 3, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x3,
            deltas: &[],
            fetch: 4,
        },
        // CMOVcc true: CMOVNZ AX, BX (0F 45 C3): ZF=0 → condition true → AX = BX = 3.
        CondMoveGolden {
            name: "cmovnz ax,bx true (0f 45 c3)",
            code: &[15, 69, 195],
            gpr: [3, 256, 16384, 3, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x3,
            deltas: &[],
            fetch: 4,
        },
        // CMOVcc mem false: CMOVZ AX, [0x40] (0F 44 06 40 00): ZF=0 → AX unchanged; the
        // memory source is still read (architectural: memory operand is always fetched).
        CondMoveGolden {
            name: "cmovz ax,[0x40] false (0f 44 06 40 00)",
            code: &[15, 68, 6, 64, 0],
            gpr: [5, 256, 16384, 3, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x5,
            deltas: &[],
            fetch: 6,
        },
        // CMOVcc mem true: CMOVNZ AX, [0x40] (0F 45 06 40 00): ZF=0 → AX = [0x40] = 3.
        CondMoveGolden {
            name: "cmovnz ax,[0x40] true (0f 45 06 40 00)",
            code: &[15, 69, 6, 64, 0],
            gpr: [3, 256, 16384, 3, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x5,
            deltas: &[],
            fetch: 6,
        },
        // IMUL no overflow: IMUL AX, BX (0F AF C3): 5*3=15, fits in 16 bits → CF=OF=0.
        CondMoveGolden {
            name: "imul ax,bx no-overflow (0f af c3)",
            code: &[15, 175, 195],
            gpr: [15, 256, 16384, 3, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x3,
            deltas: &[],
            fetch: 4,
        },
        // IMUL overflow: IMUL CX, DX (0F AF CA): 0x0100*0x4000=0x400000, truncated to
        // CX=0x0000 → CF=OF=1 (eflags 0x803: bit11=OF, bit1=reserved, bit0=CF).
        CondMoveGolden {
            name: "imul cx,dx overflow (0f af ca)",
            code: &[15, 175, 202],
            gpr: [5, 0, 16384, 3, 240, 16, 8, 24],
            eflags: 0x803,
            eip: 0x3,
            deltas: &[],
            fetch: 4,
        },
    ]
}

#[test]
fn condmove_split_matches_golden_across_ops() {
    // The condmove opcodes (CMOVcc, SETcc, IMUL reg,r/m) are converted to the decode/execute
    // split, so their fused arms are deleted and they can no longer be diffed against a fused
    // executor in-tree. Run each case through cycle() (the split) and assert the architectural
    // end-state against goldens captured from the pre-split fused path (parent 93bdff3f) via
    // `regen_condmove_goldens`. The register file proves SETcc byte writes (true/false both
    // register and memory), CMOVcc destination changed-or-unchanged, and IMUL product;
    // eflags proves SETcc/CMOVcc leave flags unchanged and IMUL sets CF/OF on overflow;
    // the memory deltas prove SETcc writes a 0 or 1 correctly; eip + fetch prove decode
    // consumed and charged every byte (0F prefix + ModRM + displacement) exactly once.
    for g in condmove_golden_cases() {
        let mut mem = vec![0u8; 0x100];
        condmove_seed_mem(&mut mem, g.code);
        let initial = mem.clone();

        let mut split = CpuGsw::default();
        condmove_seed(&mut split);
        let mut sbus = TestBus::with_memory(mem);
        let _ = split.cycle(&mut sbus);

        assert_eq!(split.registers.gpr, g.gpr, "gpr mismatch for {}", g.name);
        assert_eq!(split.eflags(), g.eflags, "eflags mismatch for {}", g.name);
        assert_eq!(split.registers.eip, g.eip, "eip mismatch for {}", g.name);
        let deltas: Vec<(usize, u8)> = sbus
            .memory
            .iter()
            .enumerate()
            .filter(|(i, b)| **b != initial[*i])
            .map(|(i, b)| (i, *b))
            .collect();
        assert_eq!(deltas, g.deltas, "memory-write mismatch for {}", g.name);
        assert_eq!(
            seam_fetch_count(&sbus),
            g.fetch,
            "instruction-fetch cycle count mismatch for {} (seam must charge fetches once)",
            g.name
        );
    }
}

/// Regenerate `condmove_golden_cases` from the fused reference. Ignored by default.
/// Run WHILE the condmove fused arms still exist in `execute_two_byte`
/// (i.e. the parent commit 93bdff3f):
///   git worktree add ../regen-a11 93bdff3f
///   cd ../regen-a11
///   cargo test -p izarravm-cpu --lib regen_condmove_goldens -- --ignored --nocapture
/// then paste the output over `condmove_golden_cases` and only then delete the fused arms.
#[test]
#[ignore = "prints golden literals; run with --ignored --nocapture against the fused reference"]
fn regen_condmove_goldens() {
    for g in condmove_golden_cases() {
        let mut mem = vec![0u8; 0x100];
        condmove_seed_mem(&mut mem, g.code);
        let initial = mem.clone();

        let mut fused = CpuGsw::default();
        condmove_seed(&mut fused);
        let mut fbus = TestBus::with_memory(mem);
        fused.begin_instruction();
        if exec_one_split(&mut fused, &mut fbus).is_err() {
            println!(
                "            // TODO regen {}: fused path unavailable here; run against the base commit",
                g.name
            );
            continue;
        }
        let deltas: Vec<(usize, u8)> = fbus
            .memory
            .iter()
            .enumerate()
            .filter(|(i, b)| **b != initial[*i])
            .map(|(i, b)| (i, *b))
            .collect();
        let fetch = seam_fetch_count(&fbus);
        println!(
            "            CondMoveGolden {{ name: {:?}, code: &{:?}, gpr: {:?}, eflags: {:#x}, eip: {:#x}, deltas: &{:?}, fetch: {} }},",
            g.name,
            g.code,
            fused.registers.gpr,
            fused.eflags(),
            fused.registers.eip,
            deltas,
            fetch,
        );
    }
}

// ── Task A12: system / descriptor-table / segment-load golden battery ──────────────────────────

/// One golden end-state for a system / descriptor-table / segment-load case (task A12). These
/// opcodes change a heterogeneous set of architectural state — GPRs (SLDT/STR/SMSW/LAR/LSL store
/// a selector/limit; LES/LDS load the offset), eflags (VERR/VERW/LAR/LSL set ZF), memory
/// (SGDT/SIDT store the pseudo-descriptor, SMSW r/m16 stores to memory), the descriptor tables
/// (LGDT/LIDT), the control registers (MOV CR, LMSW, CLTS), the LDTR/TR selectors (LLDT/LTR), and
/// the ES/DS segment registers (LES/LDS) — so the golden captures all of them. `eip` proves
/// decode consumed the right byte count (incl. the 0F second byte + ModRM + displacement);
/// `fetch` proves each instruction byte was charged exactly once.
struct SystemSegGolden {
    name: &'static str,
    code: &'static [u8],
    /// Whether the case runs in protected mode (CR0.PE set, the seeded GDT live).
    protected: bool,
    gpr: [u32; 8],
    eflags: u32,
    eip: u32,
    deltas: &'static [(usize, u8)],
    fetch: usize,
    cr0: u32,
    gdtr_base: u32,
    gdtr_limit: u16,
    idtr_base: u32,
    idtr_limit: u16,
    ldtr_sel: u16,
    tr_sel: u16,
    es_sel: u16,
    ds_sel: u16,
}

/// Seed for the system/segment golden battery. Real or protected mode (CR0.PE per the case),
/// 16-bit addressing, DS=0. A GDT lives at base 0x100, limit 0xff, with descriptors planted at
/// selectors 0x08 (a present readable data segment, access 0x92, byte-granular limit 0xffff),
/// 0x10 (a present available 386 TSS, access 0x89), and 0x18 (a present LDT system descriptor,
/// access 0x82). CR0 carries TS|MP (0x0A) plus PE when protected. gdtr/idtr/ldtr/tr start at
/// known values so the load ops (LGDT/LIDT/LLDT/LTR) and the store ops (SGDT/SIDT/SLDT/STR/SMSW)
/// both have an observable before/after. Registers: CX=0x0008 (a selector operand for LAR/LSL/
/// LLDT/LTR/VERR/VERW), the rest a fixed pattern.
fn system_seg_seed(cpu: &mut CpuGsw, protected: bool) {
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Es, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x0005);
    cpu.write_reg16(Reg16::Cx, 0x0008);
    cpu.write_reg16(Reg16::Dx, 0x4000);
    cpu.write_reg16(Reg16::Bx, 0x0003);
    cpu.write_reg16(Reg16::Sp, 0x00f0);
    cpu.write_reg16(Reg16::Bp, 0x0010);
    cpu.write_reg16(Reg16::Si, 0x0008);
    cpu.write_reg16(Reg16::Di, 0x0018);
    cpu.registers.eflags = 0x02;
    cpu.gdtr = DescriptorTable {
        base: 0x100,
        limit: 0xff,
    };
    cpu.idtr = DescriptorTable {
        base: 0x900,
        limit: 0x3ff,
    };
    cpu.ldtr.selector = 0x0028;
    cpu.tr.selector = 0x0038;
    cpu.control.cr0 = CR0_TS | CR0_MP;
    if protected {
        cpu.control.cr0 |= CR0_PE;
    }
}

/// Plant the instruction bytes plus the GDT descriptors and the scratch the memory forms read.
fn system_seg_seed_mem(mem: &mut [u8], code: &[u8]) {
    mem[..code.len()].copy_from_slice(code);
    // GDT at 0x100. Selector 0x08: present readable data segment (access 0x92), limit 0xffff.
    mem[0x108..0x10c].copy_from_slice(&0x0000_ffffu32.to_le_bytes());
    mem[0x10c..0x110].copy_from_slice(&0x0000_9200u32.to_le_bytes());
    // Selector 0x10: present available 386 TSS (access 0x89), base 0x0005_0000, limit 0x0067.
    mem[0x110..0x114].copy_from_slice(&0x0000_0067u32.to_le_bytes());
    mem[0x114..0x118].copy_from_slice(&0x0005_8900u32.to_le_bytes());
    // Selector 0x18: present LDT system descriptor (access 0x82), base 0x0006_0000, limit 0x0fff.
    mem[0x118..0x11c].copy_from_slice(&0x0000_0fffu32.to_le_bytes());
    mem[0x11c..0x120].copy_from_slice(&0x0006_8200u32.to_le_bytes());
    // A 6-byte GDTR/IDTR pseudo-descriptor image at 0x40 (limit 0x00ff, base 0x0000_1000) for
    // LGDT/LIDT, and bounds [10, 20] at 0x80/0x84 for BOUND, and a far pointer 0x09:0x1234 at
    // 0x90 for LES/LDS.
    mem[0x40..0x46].copy_from_slice(&[0xff, 0x00, 0x00, 0x10, 0x00, 0x00]);
    mem[0x80..0x82].copy_from_slice(&10u16.to_le_bytes());
    mem[0x82..0x84].copy_from_slice(&20u16.to_le_bytes());
    mem[0x90..0x92].copy_from_slice(&0x1234u16.to_le_bytes()); // offset
    mem[0x92..0x94].copy_from_slice(&0x0009u16.to_le_bytes()); // selector (RPL 1 -> sel 0x08)
}

/// The system/segment differential battery (task A12). Captured from the PRIOR fused reference
/// (`execute_instruction_legacy` -> `execute_two_byte`/`dispatch_opcode`) via
/// `regen_system_seg_goldens` (parent commit b0a4262d) WHILE the fused arms (0F 00/01/02/03/06/
/// 20/22, BOUND 0x62, LES/LDS 0xc4/0xc5) still existed. Never edit by hand — re-run the regen
/// from the pre-split commit.
fn system_seg_golden_cases() -> &'static [SystemSegGolden] {
    // Captured verbatim from the fused reference at parent b0a4262d via
    // `regen_system_seg_goldens` (run in a throwaway worktree). Never edit by hand.
    &[
        SystemSegGolden {
            name: "smsw ax (0f 01 e0)",
            code: &[15, 1, 224],
            protected: false,
            gpr: [10, 8, 16384, 3, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x3,
            deltas: &[],
            fetch: 4,
            cr0: 0xa,
            gdtr_base: 0x100,
            gdtr_limit: 0xff,
            idtr_base: 0x900,
            idtr_limit: 0x3ff,
            ldtr_sel: 0x28,
            tr_sel: 0x38,
            es_sel: 0x0,
            ds_sel: 0x0,
        },
        SystemSegGolden {
            name: "smsw [0x60] (0f 01 26 60 00)",
            code: &[15, 1, 38, 96, 0],
            protected: false,
            gpr: [5, 8, 16384, 3, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x5,
            deltas: &[(96, 10)],
            fetch: 6,
            cr0: 0xa,
            gdtr_base: 0x100,
            gdtr_limit: 0xff,
            idtr_base: 0x900,
            idtr_limit: 0x3ff,
            ldtr_sel: 0x28,
            tr_sel: 0x38,
            es_sel: 0x0,
            ds_sel: 0x0,
        },
        SystemSegGolden {
            name: "lmsw ax (0f 01 f0)",
            code: &[15, 1, 240],
            protected: false,
            gpr: [5, 8, 16384, 3, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x3,
            deltas: &[],
            fetch: 4,
            cr0: 0x5,
            gdtr_base: 0x100,
            gdtr_limit: 0xff,
            idtr_base: 0x900,
            idtr_limit: 0x3ff,
            ldtr_sel: 0x28,
            tr_sel: 0x38,
            es_sel: 0x0,
            ds_sel: 0x0,
        },
        SystemSegGolden {
            name: "clts (0f 06)",
            code: &[15, 6],
            protected: false,
            gpr: [5, 8, 16384, 3, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x2,
            deltas: &[],
            fetch: 3,
            cr0: 0x2,
            gdtr_base: 0x100,
            gdtr_limit: 0xff,
            idtr_base: 0x900,
            idtr_limit: 0x3ff,
            ldtr_sel: 0x28,
            tr_sel: 0x38,
            es_sel: 0x0,
            ds_sel: 0x0,
        },
        SystemSegGolden {
            name: "sgdt [0x60] (0f 01 06 60 00)",
            code: &[15, 1, 6, 96, 0],
            protected: false,
            gpr: [5, 8, 16384, 3, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x5,
            deltas: &[(96, 255), (99, 1)],
            fetch: 6,
            cr0: 0xa,
            gdtr_base: 0x100,
            gdtr_limit: 0xff,
            idtr_base: 0x900,
            idtr_limit: 0x3ff,
            ldtr_sel: 0x28,
            tr_sel: 0x38,
            es_sel: 0x0,
            ds_sel: 0x0,
        },
        SystemSegGolden {
            name: "sidt [0x60] (0f 01 0e 60 00)",
            code: &[15, 1, 14, 96, 0],
            protected: false,
            gpr: [5, 8, 16384, 3, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x5,
            deltas: &[(96, 255), (97, 3), (99, 9)],
            fetch: 6,
            cr0: 0xa,
            gdtr_base: 0x100,
            gdtr_limit: 0xff,
            idtr_base: 0x900,
            idtr_limit: 0x3ff,
            ldtr_sel: 0x28,
            tr_sel: 0x38,
            es_sel: 0x0,
            ds_sel: 0x0,
        },
        SystemSegGolden {
            name: "lgdt [0x40] (0f 01 16 40 00)",
            code: &[15, 1, 22, 64, 0],
            protected: false,
            gpr: [5, 8, 16384, 3, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x5,
            deltas: &[],
            fetch: 6,
            cr0: 0xa,
            gdtr_base: 0x1000,
            gdtr_limit: 0xff,
            idtr_base: 0x900,
            idtr_limit: 0x3ff,
            ldtr_sel: 0x28,
            tr_sel: 0x38,
            es_sel: 0x0,
            ds_sel: 0x0,
        },
        SystemSegGolden {
            name: "lidt [0x40] (0f 01 1e 40 00)",
            code: &[15, 1, 30, 64, 0],
            protected: false,
            gpr: [5, 8, 16384, 3, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x5,
            deltas: &[],
            fetch: 6,
            cr0: 0xa,
            gdtr_base: 0x100,
            gdtr_limit: 0xff,
            idtr_base: 0x1000,
            idtr_limit: 0xff,
            ldtr_sel: 0x28,
            tr_sel: 0x38,
            es_sel: 0x0,
            ds_sel: 0x0,
        },
        SystemSegGolden {
            name: "mov eax,cr0 (0f 20 c0)",
            code: &[15, 32, 192],
            protected: false,
            gpr: [10, 8, 16384, 3, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x3,
            deltas: &[],
            fetch: 4,
            cr0: 0xa,
            gdtr_base: 0x100,
            gdtr_limit: 0xff,
            idtr_base: 0x900,
            idtr_limit: 0x3ff,
            ldtr_sel: 0x28,
            tr_sel: 0x38,
            es_sel: 0x0,
            ds_sel: 0x0,
        },
        SystemSegGolden {
            name: "mov cr2,eax (0f 22 d0)",
            code: &[15, 34, 208],
            protected: false,
            gpr: [5, 8, 16384, 3, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x3,
            deltas: &[],
            fetch: 4,
            cr0: 0xa,
            gdtr_base: 0x100,
            gdtr_limit: 0xff,
            idtr_base: 0x900,
            idtr_limit: 0x3ff,
            ldtr_sel: 0x28,
            tr_sel: 0x38,
            es_sel: 0x0,
            ds_sel: 0x0,
        },
        SystemSegGolden {
            name: "bound ax,[0x80] in-range (62 06 80 00)",
            code: &[98, 6, 128, 0],
            protected: false,
            gpr: [15, 8, 16384, 3, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x4,
            deltas: &[],
            fetch: 5,
            cr0: 0xa,
            gdtr_base: 0x100,
            gdtr_limit: 0xff,
            idtr_base: 0x900,
            idtr_limit: 0x3ff,
            ldtr_sel: 0x28,
            tr_sel: 0x38,
            es_sel: 0x0,
            ds_sel: 0x0,
        },
        SystemSegGolden {
            name: "les bx,[0x90] (c4 1e 90 00)",
            code: &[196, 30, 144, 0],
            protected: false,
            gpr: [5, 8, 16384, 4660, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x4,
            deltas: &[],
            fetch: 5,
            cr0: 0xa,
            gdtr_base: 0x100,
            gdtr_limit: 0xff,
            idtr_base: 0x900,
            idtr_limit: 0x3ff,
            ldtr_sel: 0x28,
            tr_sel: 0x38,
            es_sel: 0x9,
            ds_sel: 0x0,
        },
        SystemSegGolden {
            name: "lds bx,[0x90] (c5 1e 90 00)",
            code: &[197, 30, 144, 0],
            protected: false,
            gpr: [5, 8, 16384, 4660, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x4,
            deltas: &[],
            fetch: 5,
            cr0: 0xa,
            gdtr_base: 0x100,
            gdtr_limit: 0xff,
            idtr_base: 0x900,
            idtr_limit: 0x3ff,
            ldtr_sel: 0x28,
            tr_sel: 0x38,
            es_sel: 0x0,
            ds_sel: 0x9,
        },
        SystemSegGolden {
            name: "sldt ax (0f 00 c0)",
            code: &[15, 0, 192],
            protected: true,
            gpr: [40, 8, 16384, 3, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x3,
            deltas: &[],
            fetch: 4,
            cr0: 0xb,
            gdtr_base: 0x100,
            gdtr_limit: 0xff,
            idtr_base: 0x900,
            idtr_limit: 0x3ff,
            ldtr_sel: 0x28,
            tr_sel: 0x38,
            es_sel: 0x0,
            ds_sel: 0x0,
        },
        SystemSegGolden {
            name: "str ax (0f 00 c8)",
            code: &[15, 0, 200],
            protected: true,
            gpr: [56, 8, 16384, 3, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x3,
            deltas: &[],
            fetch: 4,
            cr0: 0xb,
            gdtr_base: 0x100,
            gdtr_limit: 0xff,
            idtr_base: 0x900,
            idtr_limit: 0x3ff,
            ldtr_sel: 0x28,
            tr_sel: 0x38,
            es_sel: 0x0,
            ds_sel: 0x0,
        },
        SystemSegGolden {
            name: "lldt cx=0x18 (0f 00 d1)",
            code: &[15, 0, 209],
            protected: true,
            gpr: [5, 24, 16384, 3, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x3,
            deltas: &[],
            fetch: 4,
            cr0: 0xb,
            gdtr_base: 0x100,
            gdtr_limit: 0xff,
            idtr_base: 0x900,
            idtr_limit: 0x3ff,
            ldtr_sel: 0x18,
            tr_sel: 0x38,
            es_sel: 0x0,
            ds_sel: 0x0,
        },
        SystemSegGolden {
            name: "verr cx (0f 00 e1)",
            code: &[15, 0, 225],
            protected: true,
            gpr: [5, 8, 16384, 3, 240, 16, 8, 24],
            eflags: 0x42,
            eip: 0x3,
            deltas: &[],
            fetch: 4,
            cr0: 0xb,
            gdtr_base: 0x100,
            gdtr_limit: 0xff,
            idtr_base: 0x900,
            idtr_limit: 0x3ff,
            ldtr_sel: 0x28,
            tr_sel: 0x38,
            es_sel: 0x0,
            ds_sel: 0x0,
        },
        SystemSegGolden {
            name: "verw cx (0f 00 e9)",
            code: &[15, 0, 233],
            protected: true,
            gpr: [5, 8, 16384, 3, 240, 16, 8, 24],
            eflags: 0x42,
            eip: 0x3,
            deltas: &[],
            fetch: 4,
            cr0: 0xb,
            gdtr_base: 0x100,
            gdtr_limit: 0xff,
            idtr_base: 0x900,
            idtr_limit: 0x3ff,
            ldtr_sel: 0x28,
            tr_sel: 0x38,
            es_sel: 0x0,
            ds_sel: 0x0,
        },
        SystemSegGolden {
            name: "lar ax,cx (0f 02 c1)",
            code: &[15, 2, 193],
            protected: true,
            gpr: [37376, 8, 16384, 3, 240, 16, 8, 24],
            eflags: 0x42,
            eip: 0x3,
            deltas: &[],
            fetch: 4,
            cr0: 0xb,
            gdtr_base: 0x100,
            gdtr_limit: 0xff,
            idtr_base: 0x900,
            idtr_limit: 0x3ff,
            ldtr_sel: 0x28,
            tr_sel: 0x38,
            es_sel: 0x0,
            ds_sel: 0x0,
        },
        SystemSegGolden {
            name: "lsl ax,cx (0f 03 c1)",
            code: &[15, 3, 193],
            protected: true,
            gpr: [65535, 8, 16384, 3, 240, 16, 8, 24],
            eflags: 0x42,
            eip: 0x3,
            deltas: &[],
            fetch: 4,
            cr0: 0xb,
            gdtr_base: 0x100,
            gdtr_limit: 0xff,
            idtr_base: 0x900,
            idtr_limit: 0x3ff,
            ldtr_sel: 0x28,
            tr_sel: 0x38,
            es_sel: 0x0,
            ds_sel: 0x0,
        },
    ]
}

/// Per-case register overrides applied AFTER `system_seg_seed`. LLDT needs CX pointing at the
/// LDT system descriptor (selector 0x18); BOUND and LES/LDS need their default seed. Applied
/// identically on the split and the regen (fused) path so the goldens stay a faithful diff.
fn system_seg_case_override(name: &str, cpu: &mut CpuGsw) {
    if name == "lldt cx=0x18 (0f 00 d1)" {
        cpu.write_reg16(Reg16::Cx, 0x18);
    }
    if name == "bound ax,[0x80] in-range (62 06 80 00)" {
        cpu.write_reg16(Reg16::Ax, 15);
    }
}

fn assert_system_seg_state(cpu: &CpuGsw, g: &SystemSegGolden) {
    assert_eq!(cpu.registers.gpr, g.gpr, "gpr mismatch for {}", g.name);
    assert_eq!(cpu.eflags(), g.eflags, "eflags mismatch for {}", g.name);
    assert_eq!(cpu.registers.eip, g.eip, "eip mismatch for {}", g.name);
    assert_eq!(cpu.control.cr0, g.cr0, "cr0 mismatch for {}", g.name);
    assert_eq!(
        cpu.gdtr.base, g.gdtr_base,
        "gdtr.base mismatch for {}",
        g.name
    );
    assert_eq!(
        cpu.gdtr.limit, g.gdtr_limit,
        "gdtr.limit mismatch for {}",
        g.name
    );
    assert_eq!(
        cpu.idtr.base, g.idtr_base,
        "idtr.base mismatch for {}",
        g.name
    );
    assert_eq!(
        cpu.idtr.limit, g.idtr_limit,
        "idtr.limit mismatch for {}",
        g.name
    );
    assert_eq!(
        cpu.ldtr.selector, g.ldtr_sel,
        "ldtr selector mismatch for {}",
        g.name
    );
    assert_eq!(
        cpu.tr.selector, g.tr_sel,
        "tr selector mismatch for {}",
        g.name
    );
    assert_eq!(
        cpu.registers.segment(SegmentIndex::Es).selector,
        g.es_sel,
        "es selector mismatch for {}",
        g.name
    );
    assert_eq!(
        cpu.registers.segment(SegmentIndex::Ds).selector,
        g.ds_sel,
        "ds selector mismatch for {}",
        g.name
    );
}

#[test]
fn system_seg_split_matches_golden_across_ops() {
    // The system / descriptor-table / segment-load opcodes (0F 00/01/02/03/06/20/22, BOUND,
    // LES/LDS) are converted to the decode/execute split, so their fused arms are deleted and
    // they can no longer be diffed against a fused executor in-tree. Run each case through the
    // split (`exec_one_split`) and assert the architectural end-state — GPRs, eflags, the
    // control register, the GDTR/IDTR, the LDTR/TR selectors, and the ES/DS segment selectors —
    // against goldens captured from the pre-split fused path (parent b0a4262d) via
    // `regen_system_seg_goldens`. eip + fetch prove decode consumed and charged every byte (0F
    // prefix + ModRM + displacement) exactly once; the memory deltas prove the SGDT/SIDT/SMSW
    // store path; the CR/descriptor/segment fields prove the load ops drove the right state
    // through the reused leaf helpers (so the TLB/code-cache invalidation hooks still fire).
    for g in system_seg_golden_cases() {
        let mut mem = vec![0u8; 0x200];
        system_seg_seed_mem(&mut mem, g.code);
        let initial = mem.clone();

        let mut split = CpuGsw::default();
        system_seg_seed(&mut split, g.protected);
        system_seg_case_override(g.name, &mut split);
        let mut sbus = TestBus::with_memory(mem);
        exec_one_split(&mut split, &mut sbus).unwrap();

        assert_system_seg_state(&split, g);
        let deltas: Vec<(usize, u8)> = sbus
            .memory
            .iter()
            .enumerate()
            .filter(|(i, b)| **b != initial[*i])
            .map(|(i, b)| (i, *b))
            .collect();
        assert_eq!(deltas, g.deltas, "memory-write mismatch for {}", g.name);
        assert_eq!(
            seam_fetch_count(&sbus),
            g.fetch,
            "instruction-fetch cycle count mismatch for {} (seam must charge fetches once)",
            g.name
        );
    }
}

/// Regenerate `system_seg_golden_cases` from the fused reference. Ignored by default.
/// Run WHILE the system/segment fused arms still exist (parent commit b0a4262d):
///   git worktree add ../regen-a12 b0a4262d
///   cd ../regen-a12
///   # paste this test + the cases/seed/struct in, then:
///   cargo test -p izarravm-cpu --lib regen_system_seg_goldens -- --ignored --nocapture
/// then paste the output over `system_seg_golden_cases` and only then delete the fused arms.
#[test]
#[ignore = "prints golden literals; run with --ignored --nocapture against the fused reference"]
fn regen_system_seg_goldens() {
    for g in system_seg_golden_cases() {
        let mut mem = vec![0u8; 0x200];
        system_seg_seed_mem(&mut mem, g.code);
        let initial = mem.clone();

        let mut fused = CpuGsw::default();
        system_seg_seed(&mut fused, g.protected);
        system_seg_case_override(g.name, &mut fused);
        let mut fbus = TestBus::with_memory(mem);
        fused.begin_instruction();
        if exec_one_split(&mut fused, &mut fbus).is_err() {
            println!(
                "            // TODO regen {}: fused path unavailable here; run against the base commit",
                g.name
            );
            continue;
        }
        let deltas: Vec<(usize, u8)> = fbus
            .memory
            .iter()
            .enumerate()
            .filter(|(i, b)| **b != initial[*i])
            .map(|(i, b)| (i, *b))
            .collect();
        let fetch = seam_fetch_count(&fbus);
        println!(
            "            SystemSegGolden {{ name: {:?}, code: &{:?}, protected: {}, gpr: {:?}, eflags: {:#x}, eip: {:#x}, deltas: &{:?}, fetch: {}, cr0: {:#x}, gdtr_base: {:#x}, gdtr_limit: {:#x}, idtr_base: {:#x}, idtr_limit: {:#x}, ldtr_sel: {:#x}, tr_sel: {:#x}, es_sel: {:#x}, ds_sel: {:#x} }},",
            g.name,
            g.code,
            g.protected,
            fused.registers.gpr,
            fused.eflags(),
            fused.registers.eip,
            deltas,
            fetch,
            fused.control.cr0,
            fused.gdtr.base,
            fused.gdtr.limit,
            fused.idtr.base,
            fused.idtr.limit,
            fused.ldtr.selector,
            fused.tr.selector,
            fused.registers.segment(SegmentIndex::Es).selector,
            fused.registers.segment(SegmentIndex::Ds).selector,
        );
    }
}
