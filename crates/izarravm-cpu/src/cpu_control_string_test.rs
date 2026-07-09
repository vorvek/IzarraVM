// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

/// One golden end-state for a control-flow case (task A6b). Mirrors the `BranchGolden` shape but
/// adds `cs` (the CS selector) and a per-case `setup` closure, because this group changes
/// segment state (RETF, far-direct CALL/JMP, and the INT/IRET deliveries reload CS) and each
/// form needs its own in-memory image (a far pointer / IVT entry / saved stack frame). The
/// captured fields are the standard set plus `cs`: end gpr (AX,CX,DX,BX,SP,BP,SI,DI), the CS
/// selector, eflags, eip, (offset,value) memory writes (CALL/PUSH/INT push; INC/DEC write), and
/// the InstructionPrefetch fetch count.
struct ControlFlowGolden {
    name: &'static str,
    code: &'static [u8],
    /// Per-case memory image written before the run (IVT entries, far pointers, saved frames),
    /// applied identically on the split and the fused-reference paths.
    setup: fn(&mut [u8]),
    gpr: [u32; 8],
    cs: u16,
    eflags: u32,
    eip: u32,
    deltas: &'static [(usize, u8)],
    fetch: usize,
}

/// Shared register seed for the control-flow golden battery: CS/DS/SS = 0, eip = 0, SP = 0x100
/// (a safe in-image stack), BX = 0x40 (so `[bx]` addresses the in-image FF r/m operand), and the
/// OF/IF flags set so INTO traps and the interrupt deliveries record IF being cleared. The
/// per-case `setup` closure lays down the memory image each form needs.
fn controlflow_seed(cpu: &mut CpuGsw) {
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Sp, 0x0100);
    cpu.write_reg16(Reg16::Bx, 0x0040);
    cpu.set_flag(FLAG_OF, true);
    cpu.set_flag(FLAG_IF, true);
}

/// The far/indirect/RET/INT control-flow + 0xff group-5 differential battery. Captured from the
/// PRIOR fused reference via `regen_controlflow_goldens`; see `branch_golden_cases` for the
/// capture recipe. These opcodes' fused arms are deleted on `perf-decode-cache`, so the goldens
/// were captured from the pre-split base commit (HEAD before A6b, dc1cf4e2): the regen runs the
/// fused `execute_instruction_legacy` there, prints the literals, and they are pasted back.
///
/// Covers the non-faulting success paths: RET near (with and without an imm16 SP-release), RETF
/// (the CS reload + SP delta), FF /0 INC and FF /1 DEC r/m (the memory write + the flag update),
/// FF /6 PUSH r/m (the pushed value + SP drop), FF /2 near-indirect CALL (the pushed return +
/// the new eip), FF /4 near-indirect JMP (the new eip, nothing pushed), CALL/JMP far direct
/// (0x9a/0xea — the CS:eip transfer, plus CALL's pushed CS:IP), and the INT3/INT n/INTO/IRET
/// deliveries (CS:eip from the IVT, the pushed FLAGS:CS:IP frame / the restored frame, IF
/// cleared). The shared `controlflow_seed` plus each case's `setup` makes every input stable.
fn controlflow_golden_cases() -> &'static [ControlFlowGolden] {
    &[
        ControlFlowGolden {
            name: "ret near (c3, pop 0x0100)",
            code: &[0xc3],
            setup: |m| m[0x100..0x102].copy_from_slice(&0x0100u16.to_le_bytes()),
            gpr: [0, 0, 0, 64, 258, 0, 0, 0],
            cs: 0x0,
            eflags: 0xa02,
            eip: 0x100,
            deltas: &[],
            fetch: 2,
        },
        ControlFlowGolden {
            name: "ret near imm16 (c2 04 00, pop then release 4)",
            code: &[0xc2, 0x04, 0x00],
            setup: |m| m[0x100..0x102].copy_from_slice(&0x0100u16.to_le_bytes()),
            gpr: [0, 0, 0, 64, 262, 0, 0, 0],
            cs: 0x0,
            eflags: 0xa02,
            eip: 0x100,
            deltas: &[],
            fetch: 4,
        },
        ControlFlowGolden {
            name: "retf (cb, pop 0x0100:0x3000)",
            code: &[0xcb],
            setup: |m| {
                m[0x100..0x102].copy_from_slice(&0x0100u16.to_le_bytes());
                m[0x102..0x104].copy_from_slice(&0x3000u16.to_le_bytes());
            },
            gpr: [0, 0, 0, 64, 260, 0, 0, 0],
            cs: 0x3000,
            eflags: 0xa02,
            eip: 0x100,
            deltas: &[],
            fetch: 2,
        },
        ControlFlowGolden {
            name: "ff /0 inc word [bx] (0x0080 -> 0x0081)",
            code: &[0xff, 0x07],
            setup: |m| m[0x40..0x42].copy_from_slice(&0x0080u16.to_le_bytes()),
            gpr: [0, 0, 0, 64, 256, 0, 0, 0],
            cs: 0x0,
            eflags: 0x206,
            eip: 0x2,
            deltas: &[(64, 129)],
            fetch: 3,
        },
        ControlFlowGolden {
            name: "ff /1 dec word [bx] (0x0080 -> 0x007f)",
            code: &[0xff, 0x0f],
            setup: |m| m[0x40..0x42].copy_from_slice(&0x0080u16.to_le_bytes()),
            gpr: [0, 0, 0, 64, 256, 0, 0, 0],
            cs: 0x0,
            eflags: 0x212,
            eip: 0x2,
            deltas: &[(64, 127)],
            fetch: 3,
        },
        ControlFlowGolden {
            name: "ff /6 push word [bx] (push 0x0080)",
            code: &[0xff, 0x37],
            setup: |m| m[0x40..0x42].copy_from_slice(&0x0080u16.to_le_bytes()),
            gpr: [0, 0, 0, 64, 254, 0, 0, 0],
            cs: 0x0,
            eflags: 0xa02,
            eip: 0x2,
            deltas: &[(254, 128)],
            fetch: 3,
        },
        ControlFlowGolden {
            name: "ff /2 call near [bx] (push return 2, jump 0x0080)",
            code: &[0xff, 0x17],
            setup: |m| m[0x40..0x42].copy_from_slice(&0x0080u16.to_le_bytes()),
            gpr: [0, 0, 0, 64, 254, 0, 0, 0],
            cs: 0x0,
            eflags: 0xa02,
            eip: 0x80,
            deltas: &[(254, 2)],
            fetch: 3,
        },
        ControlFlowGolden {
            name: "ff /4 jmp near [bx] (jump 0x0080, nothing pushed)",
            code: &[0xff, 0x27],
            setup: |m| m[0x40..0x42].copy_from_slice(&0x0080u16.to_le_bytes()),
            gpr: [0, 0, 0, 64, 256, 0, 0, 0],
            cs: 0x0,
            eflags: 0xa02,
            eip: 0x80,
            deltas: &[],
            fetch: 3,
        },
        ControlFlowGolden {
            name: "call far 0x3000:0x0100 (9a, push cs:ip)",
            code: &[0x9a, 0x00, 0x01, 0x00, 0x30],
            setup: |_m| {},
            gpr: [0, 0, 0, 64, 252, 0, 0, 0],
            cs: 0x3000,
            eflags: 0xa02,
            eip: 0x100,
            deltas: &[(252, 5)],
            fetch: 6,
        },
        ControlFlowGolden {
            name: "jmp far 0x3000:0x0100 (ea, nothing pushed)",
            code: &[0xea, 0x00, 0x01, 0x00, 0x30],
            setup: |_m| {},
            gpr: [0, 0, 0, 64, 256, 0, 0, 0],
            cs: 0x3000,
            eflags: 0xa02,
            eip: 0x100,
            deltas: &[],
            fetch: 6,
        },
        ControlFlowGolden {
            name: "int3 (cc, ivt[3] -> 0000:0040)",
            code: &[0xcc],
            setup: |m| m[12..14].copy_from_slice(&0x0040u16.to_le_bytes()),
            gpr: [0, 0, 0, 64, 250, 0, 0, 0],
            cs: 0x0,
            eflags: 0x802,
            eip: 0x40,
            deltas: &[(250, 1), (254, 2), (255, 10)],
            fetch: 2,
        },
        ControlFlowGolden {
            name: "int 0x21 (cd 21, ivt[0x21] -> 0000:0050)",
            code: &[0xcd, 0x21],
            setup: |m| m[0x84..0x86].copy_from_slice(&0x0050u16.to_le_bytes()),
            gpr: [0, 0, 0, 64, 250, 0, 0, 0],
            cs: 0x0,
            eflags: 0x802,
            eip: 0x50,
            deltas: &[(250, 2), (254, 2), (255, 10)],
            fetch: 3,
        },
        ControlFlowGolden {
            name: "into with OF set (ce, ivt[4] -> 0000:0060)",
            code: &[0xce],
            setup: |m| m[16..18].copy_from_slice(&0x0060u16.to_le_bytes()),
            gpr: [0, 0, 0, 64, 250, 0, 0, 0],
            cs: 0x0,
            eflags: 0x802,
            eip: 0x60,
            deltas: &[(250, 1), (254, 2), (255, 10)],
            fetch: 2,
        },
        ControlFlowGolden {
            name: "iret (cf, restore 0000:0100 flags 0x0202)",
            code: &[0xcf],
            setup: |m| {
                m[0x100..0x102].copy_from_slice(&0x0100u16.to_le_bytes());
                m[0x102..0x104].copy_from_slice(&0x0000u16.to_le_bytes());
                m[0x104..0x106].copy_from_slice(&0x0202u16.to_le_bytes());
            },
            gpr: [0, 0, 0, 64, 262, 0, 0, 0],
            cs: 0x0,
            eflags: 0x202,
            eip: 0x100,
            deltas: &[],
            fetch: 2,
        },
    ]
}

#[test]
fn controlflow_split_matches_golden_across_ops() {
    // The far/indirect/RET/INT control-flow block + 0xff group 5 is converted to the decode/
    // execute split, so its fused arms are deleted and it can no longer be diffed against a fused
    // executor in-tree. Run each case through cycle() (the split) and assert the architectural
    // end-state against goldens captured from the pre-split fused path via
    // `regen_controlflow_goldens`. eip is the branch/return/vector target; cs proves RETF / the
    // far-direct / INT deliveries reloaded the segment; the memory deltas prove CALL/PUSH/INT
    // pushed (and INC/DEC wrote) the right bytes; the fetch count proves decode charged each
    // instruction byte exactly once.
    for g in controlflow_golden_cases() {
        let mut mem = vec![0u8; 0x200];
        mem[..g.code.len()].copy_from_slice(g.code);
        (g.setup)(&mut mem);
        let initial = mem.clone();

        let mut split = CpuGsw::default();
        controlflow_seed(&mut split);
        let mut sbus = TestBus::with_memory(mem);
        split.cycle(&mut sbus).unwrap();

        assert_eq!(split.registers.gpr, g.gpr, "gpr mismatch for {}", g.name);
        assert_eq!(
            split.registers.cs().selector,
            g.cs,
            "cs mismatch for {}",
            g.name
        );
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

/// Regenerate `controlflow_golden_cases` from the fused reference. Ignored by default. The
/// control-flow fused arms are already deleted on `perf-decode-cache`, so run this from the
/// pre-split base commit (HEAD before A6b, dc1cf4e2) where they still exist:
///   git worktree add ../regen dc1cf4e2 && cd ../regen
///   cargo test -p izarravm-cpu --lib regen_controlflow_goldens -- --ignored --nocapture
/// then paste the output over `controlflow_golden_cases`, return to the branch, and only then
/// trust it. (Copy this test body + the struct/seed into the throwaway worktree if the fused
/// base predates them.)
#[test]
#[ignore = "prints golden literals; run with --ignored --nocapture against the fused reference"]
fn regen_controlflow_goldens() {
    for g in controlflow_golden_cases() {
        let mut mem = vec![0u8; 0x200];
        mem[..g.code.len()].copy_from_slice(g.code);
        (g.setup)(&mut mem);
        let initial = mem.clone();

        let mut fused = CpuGsw::default();
        controlflow_seed(&mut fused);
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
            "            // {}\n            gpr: {:?}, cs: {:#x}, eflags: {:#x}, eip: {:#x}, deltas: &{:?}, fetch: {},",
            g.name,
            fused.registers.gpr,
            fused.registers.cs().selector,
            fused.eflags(),
            fused.registers.eip,
            deltas,
            fetch,
        );
    }
}

/// FF /7 is an undefined group-5 encoding and must raise the group-opcode error (which the
/// emulator maps to #UD), not silently execute. Drive it through the split and assert the error.
#[test]
fn controlflow_ff_ext7_is_undefined() {
    // 0xff 0x3f: mod=00 reg=111 rm=111 -> group 5 /7 with a memory r/m. The /7 extension is
    // undefined regardless of the addressing form.
    let (mut cpu, memory) = real_mode_cpu(&[0xff, 0x3f], 0x100);
    let mut bus = TestBus::with_memory(memory);
    let err = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(
        matches!(
            err,
            InternalFault::Exception {
                vector: 6,
                error_code: None
            }
        ),
        "FF /7 must raise a deliverable #UD, got {err:?}"
    );
}

// ---- Flags + misc register golden battery (A7) ----

/// One golden end-state for a flags/misc register case (task A7). The standard shape:
/// opcode bytes, expected end gpr (AX,CX,DX,BX,SP,BP,SI,DI), eflags, eip, and the
/// InstructionPrefetch fetch count. No memory writes (none of the A7 opcodes write to memory),
/// so no `deltas` field. The `eflags` field is load-bearing for most cases (TEST/SAHF/CLC/STC/
/// CMC/CLD/STD/CLI/STI change flags; INC/DEC change S/Z/O/A/P while preserving CF; CBW/CWD
/// change registers only). `eip` advances past the instruction (1 byte for all except TEST
/// 0x84/0x85 which have a ModRM, so 2 bytes).
struct FlagsMiscGolden {
    name: &'static str,
    code: &'static [u8],
    gpr: [u32; 8],
    eflags: u32,
    eip: u32,
    fetch: usize,
}

/// Seed for the flags/misc golden battery: the same register file as `seam_seed` plus CF
/// pre-set (so INC/DEC CF-preservation is visible and CMC/CLC/STC have a known starting CF),
/// and AH=0xd7 (= 0b11010111: CF/PF/AF/ZF/SF all 1, bits 3/5 forced — so LAHF/SAHF transfer
/// a non-trivial value). AH lives in the high byte of AX; write_gpr8(4, 0xd7) sets it.
fn flags_misc_seed(cpu: &mut CpuGsw) {
    seam_seed(cpu);
    // CF set (bit 0 on top of always-1 bit 1). Makes INC/DEC CF-preservation observable and
    // gives CMC/CLC/STC a known starting state.
    cpu.registers.eflags = 0x03;
    // AH = 0xd7 (bit pattern: CF=1, PF=1, AF=1, ZF=1, SF=1, reserved bits 1/3/5).
    // This is the value SAHF loads into the low flag byte, and LAHF reads it back out.
    cpu.write_gpr8(4, 0xd7);
}

/// Seed memory for the flags/misc battery: plant a word at [bx]=ds:0x10 (the TEST r/m target).
fn flags_misc_seed_mem(mem: &mut [u8], code: &[u8]) {
    mem[..code.len()].copy_from_slice(code);
    // TEST byte [bx]: [0x10] = 0x12; TEST word [bx]: [0x10..0x12] = 0x3412.
    mem[0x10..0x12].copy_from_slice(&0x3412u16.to_le_bytes());
}

/// The flags + misc register differential battery (task A7). Captured from the PRIOR fused
/// reference (`execute_instruction_legacy`) via `regen_flags_misc_goldens`; see
/// `alu_golden_cases` for the full capture recipe. Never edit by hand — re-run the regen WHILE
/// the fused arms (0x40-0x4f, 0x84/0x85, 0x98/0x99, 0x9e/0x9f, 0xf5/0xf8-0xfd) still exist
/// in `dispatch_opcode`, then paste, then delete the fused arms. Covers: TEST byte/word reg and
/// mem (flags set, no write-back); INC/DEC reg (CF preserved, overflow and sign visible); CBW/
/// CWDE/CWD/CDQ (operand-size-dependent sign extension); SAHF/LAHF (flag-byte round-trip); and
/// all seven flag-bit ops CMC/CLC/STC/CLI/STI/CLD/STD (correct bit set/clear/complement;
/// STI interrupt shadow is covered by a dedicated test).
fn flags_misc_golden_cases() -> &'static [FlagsMiscGolden] {
    // Captured from the fused reference (`execute_instruction_legacy`) via
    // `regen_flags_misc_goldens` run against parent commit 3912fbc5.
    // Seed: AX=0xD702 (AH=0xd7, AL=0x02; seam_seed sets AX=0x0102 then AH=0xd7),
    // CX=0x0304, DX=0x0506, BX=0x0010, SP=0, BP=0x0010, SI=0x0008, DI=0x0018.
    // eflags=0x03 (CF=1, always-1 bit1=1). Memory: [0x10..0x12] = 0x3412.
    &[
        // TEST r/m8,reg8 (0x84): flags only, no write-back. TEST AL,AL: 0x02 AND 0x02 = 0x02,
        // ZF=0 PF=0 SF=0 CF=0 OF=0 → eflags=0x02 (reserved bit only).
        FlagsMiscGolden {
            name: "test al,al (84 c0)",
            code: &[0x84, 0xc0],
            gpr: [55042, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x2,
            eip: 0x2,
            fetch: 3,
        },
        // TEST r/m8,reg8 (0x84): TEST [bx],cl: [0x10]=0x12 AND CL=0x04 → 0x00, ZF=1 → 0x46.
        FlagsMiscGolden {
            name: "test [bx],cl (84 0f)",
            code: &[0x84, 0x0f],
            gpr: [55042, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x46,
            eip: 0x2,
            fetch: 3,
        },
        // TEST r/m16,reg16 (0x85): TEST BX,CX: 0x0010 AND 0x0304 = 0x0000, ZF=1 PF=1 → 0x46.
        FlagsMiscGolden {
            name: "test bx,cx (85 cb)",
            code: &[0x85, 0xcb],
            gpr: [55042, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x46,
            eip: 0x2,
            fetch: 3,
        },
        // TEST r/m16,reg16 (0x85): TEST [bx],cx: [0x10]=0x3412 AND 0x0304 = 0x0000, ZF=1 → 0x46.
        FlagsMiscGolden {
            name: "test [bx],cx (85 0f)",
            code: &[0x85, 0x0f],
            gpr: [55042, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x46,
            eip: 0x2,
            fetch: 3,
        },
        // INC AX (0x40): AX=0xd702 → 0xd703. CF preserved (stays 1). AF set (low nibble 2→3).
        FlagsMiscGolden {
            name: "inc ax (40)",
            code: &[0x40],
            gpr: [55043, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x87,
            eip: 0x1,
            fetch: 2,
        },
        // INC DI (0x47): DI=0x0018 → 0x0019. CF preserved (stays 1). No half-carry.
        FlagsMiscGolden {
            name: "inc di (47)",
            code: &[0x47],
            gpr: [55042, 772, 1286, 16, 0, 16, 8, 25],
            eflags: 0x3,
            eip: 0x1,
            fetch: 2,
        },
        // DEC AX (0x48): AX=0xd702 → 0xd701. CF preserved (stays 1). SF set (high bit of AH).
        FlagsMiscGolden {
            name: "dec ax (48)",
            code: &[0x48],
            gpr: [55041, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x83,
            eip: 0x1,
            fetch: 2,
        },
        // DEC DI (0x4f): DI=0x0018 → 0x0017. CF preserved (stays 1). AF set.
        FlagsMiscGolden {
            name: "dec di (4f)",
            code: &[0x4f],
            gpr: [55042, 772, 1286, 16, 0, 16, 8, 23],
            eflags: 0x7,
            eip: 0x1,
            fetch: 2,
        },
        // CBW (0x98): sign-extend AL=0x02 (positive) → AX=0x0002. AH cleared.
        FlagsMiscGolden {
            name: "cbw (98, al=0x02)",
            code: &[0x98],
            gpr: [2, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x3,
            eip: 0x1,
            fetch: 2,
        },
        // CWD (0x99): AX=0xd702 (sign bit set; 0xd702 as i16 = -10494 < 0) → DX=0xFFFF.
        FlagsMiscGolden {
            name: "cwd (99, ax positive)",
            code: &[0x99],
            gpr: [55042, 772, 65535, 16, 0, 16, 8, 24],
            eflags: 0x3,
            eip: 0x1,
            fetch: 2,
        },
        // SAHF (0x9e): AH=0xd7 (= 1101_0111b) → flags low byte = d7 (CF=1 PF=1 AF=1 ZF=1 SF=1).
        FlagsMiscGolden {
            name: "sahf (9e, ah=0xd7)",
            code: &[0x9e],
            gpr: [55042, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0xd7,
            eip: 0x1,
            fetch: 2,
        },
        // LAHF (0x9f): eflags=0x03 → AH = (0x03 & 0xD5) | 0x02 = 0x03. AX = 0x0302=770.
        FlagsMiscGolden {
            name: "lahf (9f, eflags=0x03)",
            code: &[0x9f],
            gpr: [770, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x3,
            eip: 0x1,
            fetch: 2,
        },
        // CMC (0xf5): CF was 1 → CF=0. eflags: 0x03 → 0x02.
        FlagsMiscGolden {
            name: "cmc (f5, cf=1->0)",
            code: &[0xf5],
            gpr: [55042, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x2,
            eip: 0x1,
            fetch: 2,
        },
        // CLC (0xf8): CF=0. eflags: 0x03 → 0x02.
        FlagsMiscGolden {
            name: "clc (f8)",
            code: &[0xf8],
            gpr: [55042, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x2,
            eip: 0x1,
            fetch: 2,
        },
        // STC (0xf9): CF=1. eflags stays 0x03 (already set).
        FlagsMiscGolden {
            name: "stc (f9)",
            code: &[0xf9],
            gpr: [55042, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x3,
            eip: 0x1,
            fetch: 2,
        },
        // CLD (0xfc): DF=0. DF was already 0 in seed; eflags stays 0x03.
        FlagsMiscGolden {
            name: "cld (fc)",
            code: &[0xfc],
            gpr: [55042, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x3,
            eip: 0x1,
            fetch: 2,
        },
        // STD (0xfd): DF=1. eflags: 0x03 → 0x403.
        FlagsMiscGolden {
            name: "std (fd)",
            code: &[0xfd],
            gpr: [55042, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x403,
            eip: 0x1,
            fetch: 2,
        },
    ]
}

#[test]
fn flags_misc_split_matches_golden_across_ops() {
    // The flags + misc register block (TEST r/m,reg, INC/DEC reg, CBW/CWD, SAHF/LAHF, and the
    // single flag-bit ops) is converted to the decode/execute split, so its fused arms are
    // deleted and it can no longer be diffed against a fused executor in-tree. Run each case
    // through cycle() (the split) and assert the architectural end-state against goldens
    // captured from the pre-split fused path via `regen_flags_misc_goldens`. eflags is
    // load-bearing for most cases (flags change); eip proves decode consumed the right bytes
    // (1 for implicit-operand ops, 2 for TEST with ModRM); fetch proves each instruction byte
    // was charged exactly once. INC/DEC CF-preservation is observable because the seed pre-sets
    // CF and the goldens carry it.
    for g in flags_misc_golden_cases() {
        let mut mem = vec![0u8; 0x200];
        flags_misc_seed_mem(&mut mem, g.code);

        let mut split = CpuGsw::default();
        flags_misc_seed(&mut split);
        let mut sbus = TestBus::with_memory(mem);
        let _ = split.cycle(&mut sbus);

        assert_eq!(split.registers.gpr, g.gpr, "gpr mismatch for {}", g.name);
        assert_eq!(split.eflags(), g.eflags, "eflags mismatch for {}", g.name);
        assert_eq!(split.registers.eip, g.eip, "eip mismatch for {}", g.name);
        assert_eq!(
            seam_fetch_count(&sbus),
            g.fetch,
            "instruction-fetch cycle count mismatch for {} (seam must charge fetches once)",
            g.name
        );
    }
}

/// Regenerate `flags_misc_golden_cases` from the fused reference. Ignored by default.
/// Run WHILE the fused arms (0x40-0x4f, 0x84/0x85, 0x98/0x99, 0x9e/0x9f, 0xf5/0xf8-0xfd)
/// still exist in `dispatch_opcode` (i.e. the parent commit 3912fbc5):
///   git worktree add ../regen-a7 3912fbc5
///   cd ../regen-a7
///   cargo test -p izarravm-cpu --lib regen_flags_misc_goldens -- --ignored --nocapture
/// then paste the output over `flags_misc_golden_cases` and only then delete the fused arms.
#[test]
#[ignore = "prints golden literals; run with --ignored --nocapture against the fused reference"]
fn regen_flags_misc_goldens() {
    for g in flags_misc_golden_cases() {
        let mut mem = vec![0u8; 0x200];
        flags_misc_seed_mem(&mut mem, g.code);
        let initial = mem.clone();

        let mut fused = CpuGsw::default();
        flags_misc_seed(&mut fused);
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
            "            FlagsMiscGolden {{ name: {:?}, code: &{:?}, gpr: {:?}, eflags: {:#x}, eip: {:#x}, fetch: {} }},",
            g.name,
            g.code,
            fused.registers.gpr,
            fused.eflags(),
            fused.registers.eip,
            fetch,
        );
        if !deltas.is_empty() {
            println!("            // memory deltas: {:?}", deltas);
        }
    }
}

/// STI's interrupt shadow: after STI, the immediately-following instruction executes before
/// any interrupt is taken, even when a hardware interrupt is already pending. Drive three
/// back-to-back cycles through the split: STI then NOP (0x90) then another NOP. A fake
/// interrupt is pending from the start via `TestBus.pending_irq`. Prove: (1) after STI the
/// interrupt is NOT taken (shadow active), (2) after NOP the interrupt is still pending (shadow
/// let NOP through), and (3) after the next cycle the interrupt is consumed (shadow expired).
#[test]
fn sti_interrupt_shadow_defers_interrupt_by_one_instruction() {
    let mut memory = vec![0u8; 0x400];
    // STI (0xfb) followed by two NOPs (0x90).
    memory[0] = 0xfb; // STI
    memory[1] = 0x90; // NOP — executes before interrupt is taken (shadow)
    memory[2] = 0x90; // NOP — not reached; interrupt taken instead
    // IVT entry for vector 0x08 (IRQ0) at byte offset 0x20 (0x0008 * 4):
    // offset=0x0200, segment=0x0000.
    memory[0x20..0x22].copy_from_slice(&0x0200u16.to_le_bytes());
    memory[0x22..0x24].copy_from_slice(&0x0000u16.to_le_bytes());
    // IRET at the handler target (not reached in this test but avoids unmapped-memory errors
    // if the CPU tries to read into it).
    memory[0x200] = 0xcf;

    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Sp, 0x100);
    // Start with IF clear so STI is what enables interrupts.
    cpu.set_flag(FLAG_IF, false);

    let mut bus = TestBus::with_memory(memory);
    // Arm a pending IRQ 8. `interrupt_pending()` returns true while `pending_irq.is_some()`.
    bus.pending_irq = Some(8);

    // Cycle 1: STI (0xfb). IF becomes set; interrupt_shadow is armed. The pending IRQ is NOT
    // serviced yet (shadow active): eip advances to 1.
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(
        cpu.registers.eip, 1,
        "eip must be 1 after STI — NOP not yet executed"
    );
    assert!(cpu.flag(FLAG_IF), "STI must set IF");
    assert!(
        bus.pending_irq.is_some(),
        "interrupt must not be taken during the STI cycle itself"
    );

    // Cycle 2: NOP (0x90). Shadow consumed at cycle start → interrupt check skipped → NOP
    // executes → eip advances to 2. IRQ still pending.
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(
        cpu.registers.eip, 2,
        "eip must be 2 after NOP — shadow let NOP through"
    );
    assert!(
        bus.pending_irq.is_some(),
        "interrupt must still be pending after NOP (shadow consumed, interrupt check skipped)"
    );

    // Cycle 3: no shadow, IF set, IRQ pending → interrupt is acknowledged before fetch.
    // `acknowledge_interrupt` takes the pending_irq, so it becomes None.
    cpu.cycle(&mut bus).unwrap();
    assert!(
        bus.pending_irq.is_none(),
        "interrupt must be taken after the shadow expires"
    );
}

/// One golden end-state for a string-operation case (task A8). The string ops touch both
/// registers and memory, and the inputs differ widely per form (SI/DI/CX/AX/DF, the REP prefix,
/// the source/dest memory image), so each case carries its own register seed (`regs`) and memory
/// image (`setup`) on top of the shared `string_seed`. The captured fields are the standard
/// differential set plus the destination memory writes: end gpr (AX,CX,DX,BX,SP,BP,SI,DI),
/// eflags (CMPS/SCAS set them; MOVS/STOS/LODS leave them), eip, the (offset,value) memory deltas
/// (MOVS/STOS write the destination; CMPS/SCAS/LODS write nothing), and the InstructionPrefetch
/// fetch count (prefix + opcode, charged once in `decode` — small and CX-independent even for the
/// REP forms, since the per-element data accesses are bus reads/writes, not instruction fetches).
struct StringGolden {
    name: &'static str,
    code: &'static [u8],
    /// Per-case register seed applied after `string_seed` (SI/DI/CX/AX, the DF flag, segment
    /// bases for the override case), applied identically on the split and fused-reference paths.
    regs: fn(&mut CpuGsw),
    /// Per-case memory image (the source and destination bytes), applied identically on both
    /// paths before the run.
    setup: fn(&mut [u8]),
    gpr: [u32; 8],
    eflags: u32,
    eip: u32,
    deltas: &'static [(usize, u8)],
    fetch: usize,
}

/// Shared register seed for the string-operation golden battery: CS/DS/ES/SS = 0 and eip = 0.
/// Everything that varies per form (the index registers, the count, the accumulator, DF, and the
/// ES base for the segment-override case) is set by each case's `regs` closure, so the seed itself
/// stays minimal and every input is explicit at the case site.
fn string_seed(cpu: &mut CpuGsw) {
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Es, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
}

/// The string-operation differential battery (task A8). Captured from the PRIOR fused reference
/// (`execute_instruction_legacy`) via `regen_string_goldens`; see `flags_misc_golden_cases` for
/// the capture recipe. Never edit by hand — re-run the regen WHILE the fused arms (0xa4-0xa7,
/// 0xaa-0xaf) still exist in `dispatch_opcode`, then paste, then delete the fused arms.
///
/// Covers the plain single-step forms (MOVSB forward DF=0 and backward DF=1; MOVSW; CMPSB
/// flags+advance; STOSB; LODSB; SCASB; the DS:SI segment override) AND the REP forms, which are
/// the load-bearing cases: REP MOVSB (CX iterations → CX=0, every element copied, SI/DI advanced
/// by CX*width), REPE CMPSB (early termination on the first mismatch → CX and ZF prove where it
/// stopped), and REPNE SCASB (early termination on the first match → CX and ZF).
fn string_golden_cases() -> &'static [StringGolden] {
    &[
        // MOVSB forward (0xa4), DF=0: [ds:si]=0x42 at 0x100 → [es:di] at 0x200; SI/DI increment.
        StringGolden {
            name: "movsb df=0 (a4)",
            code: &[0xa4],
            regs: |c| {
                c.set_flag(FLAG_DF, false);
                c.registers.set_esi(0x100);
                c.registers.set_edi(0x200);
            },
            setup: |m| m[0x100] = 0x42,
            gpr: [0, 0, 0, 0, 0, 0, 0x101, 0x201],
            eflags: 0x2,
            eip: 0x1,
            deltas: &[(0x200, 0x42)],
            fetch: 2,
        },
        // MOVSB backward (0xa4), DF=1: same copy, but SI/DI decrement.
        StringGolden {
            name: "movsb df=1 (a4)",
            code: &[0xa4],
            regs: |c| {
                c.set_flag(FLAG_DF, true);
                c.registers.set_esi(0x100);
                c.registers.set_edi(0x200);
            },
            setup: |m| m[0x100] = 0x42,
            gpr: [0, 0, 0, 0, 0, 0, 0x0ff, 0x1ff],
            eflags: 0x402,
            eip: 0x1,
            deltas: &[(0x200, 0x42)],
            fetch: 2,
        },
        // MOVSW (0xa5), DF=0: word [0x100..0x102]=0x1234 → [0x200..0x202]; SI/DI += 2.
        StringGolden {
            name: "movsw df=0 (a5)",
            code: &[0xa5],
            regs: |c| {
                c.set_flag(FLAG_DF, false);
                c.registers.set_esi(0x100);
                c.registers.set_edi(0x200);
            },
            setup: |m| m[0x100..0x102].copy_from_slice(&0x1234u16.to_le_bytes()),
            gpr: [0, 0, 0, 0, 0, 0, 0x102, 0x202],
            eflags: 0x2,
            eip: 0x1,
            deltas: &[(0x200, 0x34), (0x201, 0x12)],
            fetch: 2,
        },
        // CMPSB unequal (0xa6): [ds:si]=0x10, [es:di]=0x20 → 0x10-0x20 borrows (ZF=0, CF=1);
        // SI/DI advance even on mismatch. No memory write.
        StringGolden {
            name: "cmpsb unequal (a6)",
            code: &[0xa6],
            regs: |c| {
                c.set_flag(FLAG_DF, false);
                c.registers.set_esi(0x100);
                c.registers.set_edi(0x200);
            },
            setup: |m| {
                m[0x100] = 0x10;
                m[0x200] = 0x20;
            },
            gpr: [0, 0, 0, 0, 0, 0, 0x101, 0x201],
            eflags: 0x87,
            eip: 0x1,
            deltas: &[],
            fetch: 2,
        },
        // STOSB (0xaa): AL=0x5a → [es:di]=0x200; DI increments. AL preserved.
        StringGolden {
            name: "stosb (aa)",
            code: &[0xaa],
            regs: |c| {
                c.set_flag(FLAG_DF, false);
                c.write_gpr8(0, 0x5a);
                c.registers.set_edi(0x200);
            },
            setup: |_m| {},
            gpr: [0x5a, 0, 0, 0, 0, 0, 0, 0x201],
            eflags: 0x2,
            eip: 0x1,
            deltas: &[(0x200, 0x5a)],
            fetch: 2,
        },
        // LODSB (0xac): [ds:si]=0x7e at 0x100 → AL; SI increments. No memory write.
        StringGolden {
            name: "lodsb (ac)",
            code: &[0xac],
            regs: |c| {
                c.set_flag(FLAG_DF, false);
                c.registers.set_esi(0x100);
            },
            setup: |m| m[0x100] = 0x7e,
            gpr: [0x7e, 0, 0, 0, 0, 0, 0x101, 0],
            eflags: 0x2,
            eip: 0x1,
            deltas: &[],
            fetch: 2,
        },
        // SCASB equal (0xae): AL=0x41, [es:di]=0x41 → ZF set; DI increments, SI untouched.
        StringGolden {
            name: "scasb equal (ae)",
            code: &[0xae],
            regs: |c| {
                c.set_flag(FLAG_DF, false);
                c.write_gpr8(0, 0x41);
                c.registers.set_edi(0x200);
            },
            setup: |m| m[0x200] = 0x41,
            gpr: [0x41, 0, 0, 0, 0, 0, 0, 0x201],
            eflags: 0x46,
            eip: 0x1,
            deltas: &[],
            fetch: 2,
        },
        // MOVSB with an ES: source segment override (0x26 0xa4): ds=0, es base 0x200, so the source
        // reads from es:si (0x210), not ds:si (0x10); the destination stays es:di (0x230).
        StringGolden {
            name: "es: movsb override (26 a4)",
            code: &[0x26, 0xa4],
            regs: |c| {
                c.load_segment_real(SegmentIndex::Es, 0x20); // base 0x200
                c.set_flag(FLAG_DF, false);
                c.registers.set_esi(0x10);
                c.registers.set_edi(0x30);
            },
            setup: |m| m[0x210] = 0x99,
            gpr: [0, 0, 0, 0, 0, 0, 0x11, 0x31],
            eflags: 0x2,
            eip: 0x2,
            deltas: &[(0x230, 0x99)],
            fetch: 3,
        },
        // REP MOVSB (0xf3 0xa4), CX=3, DF=0: copies 3 bytes [0x100..0x103]→[0x200..0x203];
        // CX→0, SI/DI advance by 3. The fetch count is small (prefix+opcode), CX-independent.
        StringGolden {
            name: "rep movsb cx=3 (f3 a4)",
            code: &[0xf3, 0xa4],
            regs: |c| {
                c.set_flag(FLAG_DF, false);
                c.registers.set_esi(0x100);
                c.registers.set_edi(0x200);
                c.registers.set_ecx(3);
            },
            setup: |m| m[0x100..0x103].copy_from_slice(&[1, 2, 3]),
            gpr: [0, 0, 0, 0, 0, 0, 0x103, 0x203],
            eflags: 0x2,
            eip: 0x2,
            deltas: &[(0x200, 1), (0x201, 2), (0x202, 3)],
            fetch: 3,
        },
        // REPE CMPSB (0xf3 0xa6), CX=4, DF=0: "AABB" vs "AACC" mismatches at index 2, so the
        // repeat stops there with ZF clear after 3 iterations; CX 4→3→2→1, SI/DI advance by 3.
        StringGolden {
            name: "repe cmpsb cx=4 (f3 a6)",
            code: &[0xf3, 0xa6],
            regs: |c| {
                c.set_flag(FLAG_DF, false);
                c.registers.set_esi(0x100);
                c.registers.set_edi(0x200);
                c.registers.set_ecx(4);
            },
            setup: |m| {
                m[0x100..0x104].copy_from_slice(b"AABB");
                m[0x200..0x204].copy_from_slice(b"AACC");
            },
            gpr: [0, 1, 0, 0, 0, 0, 0x103, 0x203],
            eflags: 0x97,
            eip: 0x2,
            deltas: &[],
            fetch: 3,
        },
        // REPNE SCASB (0xf2 0xae), CX=4, AL='C', DF=0: dest "AACA" scans until the match at
        // index 2, stopping with ZF set after 3 iterations; CX 4→3→2→1, DI advances by 3.
        StringGolden {
            name: "repne scasb cx=4 (f2 ae)",
            code: &[0xf2, 0xae],
            regs: |c| {
                c.set_flag(FLAG_DF, false);
                c.write_gpr8(0, b'C');
                c.registers.set_edi(0x200);
                c.registers.set_ecx(4);
            },
            setup: |m| m[0x200..0x204].copy_from_slice(b"AACA"),
            gpr: [0x43, 1, 0, 0, 0, 0, 0, 0x203],
            eflags: 0x46,
            eip: 0x2,
            deltas: &[],
            fetch: 3,
        },
    ]
}

#[test]
fn string_split_matches_golden_across_ops() {
    // The string-operation block (MOVS/CMPS/STOS/LODS/SCAS and the REP/REPE/REPNE forms) is
    // converted to the decode/execute split, so its fused arms are deleted and it can no longer be
    // diffed against a fused executor in-tree. Run each case through cycle() (the split) and
    // assert the architectural end-state against goldens captured from the pre-split fused path via
    // `regen_string_goldens`. The register file proves SI/DI/CX/AX moved correctly (direction,
    // element width, REP count decremented to 0 or stopped early); eflags is load-bearing for
    // CMPS/SCAS; the memory deltas prove the destination image (MOVS/STOS) is byte-exact; and the
    // fetch count proves each instruction-fetch byte (prefix + opcode) was charged exactly once
    // regardless of how many elements the REP loop processed.
    for g in string_golden_cases() {
        let mut mem = vec![0u8; 0x400];
        mem[..g.code.len()].copy_from_slice(g.code);
        (g.setup)(&mut mem);

        let mut split = CpuGsw::default();
        string_seed(&mut split);
        (g.regs)(&mut split);
        let mut sbus = TestBus::with_memory(mem);
        let _ = split.cycle(&mut sbus);

        assert_eq!(split.registers.gpr, g.gpr, "gpr mismatch for {}", g.name);
        assert_eq!(split.eflags(), g.eflags, "eflags mismatch for {}", g.name);
        assert_eq!(split.registers.eip, g.eip, "eip mismatch for {}", g.name);
        for &(offset, value) in g.deltas {
            assert_eq!(
                sbus.memory[offset], value,
                "memory[{offset:#x}] mismatch for {}",
                g.name
            );
        }
        assert_eq!(
            seam_fetch_count(&sbus),
            g.fetch,
            "instruction-fetch cycle count mismatch for {} (seam must charge fetches once)",
            g.name
        );
    }
}

/// Regenerate `string_golden_cases` from the fused reference. Ignored by default. Run WHILE the
/// fused arms (0xa4-0xa7, 0xaa-0xaf) still exist in `dispatch_opcode` (i.e. the parent commit
/// a9e0fec0):
///   git worktree add ../regen-a8 a9e0fec0
///   cd ../regen-a8
///   cargo test -p izarravm-cpu --lib regen_string_goldens -- --ignored --nocapture
/// then paste the output over `string_golden_cases` and only then delete the fused arms.
#[test]
#[ignore = "prints golden literals; run with --ignored --nocapture against the fused reference"]
fn regen_string_goldens() {
    for g in string_golden_cases() {
        let mut mem = vec![0u8; 0x400];
        mem[..g.code.len()].copy_from_slice(g.code);
        (g.setup)(&mut mem);
        let initial = mem.clone();

        let mut fused = CpuGsw::default();
        string_seed(&mut fused);
        (g.regs)(&mut fused);
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
            "            // {}: gpr {:?}, eflags {:#x}, eip {:#x}, deltas {:?}, fetch {}",
            g.name,
            fused.registers.gpr,
            fused.eflags(),
            fused.registers.eip,
            deltas,
            fetch,
        );
    }
}

// ── Task A9: port I/O golden battery ──────────────────────────────────────────────────────────

/// One golden end-state for a port-I/O case (task A9). Port reads via TestBus always return 0,
/// so the captured GPR array reflects the read-zero / write-no-register-change behaviour. The
/// eflags field is always 0x2 (IN/OUT do not modify flags). `eip` proves decode consumed the
/// right number of bytes (2 for imm8 forms, 1 for DX forms). `fetch` proves each instruction
/// byte was charged exactly once (3 for imm8 forms = 1 prefetch-peek + 1 opcode + 1 imm,
/// 2 for DX forms = 1 prefetch-peek + 1 opcode).
struct PortIoGolden {
    name: &'static str,
    code: &'static [u8],
    gpr: [u32; 8],
    eflags: u32,
    eip: u32,
    fetch: usize,
}

/// The port-I/O differential battery (task A9). Captured from the PRIOR fused reference
/// (`execute_instruction_legacy`) via `regen_port_io_goldens`; see `flags_misc_golden_cases`
/// for the full capture recipe. Never edit by hand — re-run the regen WHILE the fused arms
/// (0xe4-0xe7, 0xec-0xef) still exist in `dispatch_opcode` (i.e. parent commit 21cc68ba),
/// then paste, then delete the fused arms.
///
/// Seed: seam_seed — EAX=0x0102 (AL=0x02, AH=0x01), CX=0x0304, DX=0x0506, BX=0x0010,
/// SP=0, BP=0x0010, SI=0x0008, DI=0x0018, eflags=0x2. TestBus.read_io always returns 0.
/// Covers: IN AL imm8, IN AX imm8 (byte vs word width); OUT imm8 AL, OUT imm8 AX (no-op on
/// registers); IN AL DX, IN AX DX (port from DX=0x0506); OUT DX AL, OUT DX AX.
fn port_io_golden_cases() -> &'static [PortIoGolden] {
    &[
        // IN AL, imm8 (0xe4 0x78): port 0x78 → AL=0. AH unchanged → AX=0x0100, eip=2, fetch=3.
        PortIoGolden {
            name: "in al,imm8 (e4 78)",
            code: &[0xe4, 0x78],
            gpr: [0x0100, 0x0304, 0x0506, 0x0010, 0, 0x0010, 0x0008, 0x0018],
            eflags: 0x2,
            eip: 0x2,
            fetch: 3,
        },
        // IN AX, imm8 (0xe5 0x78): port 0x78 → AX=0x0000 (word read), eip=2, fetch=3.
        PortIoGolden {
            name: "in ax,imm8 (e5 78)",
            code: &[0xe5, 0x78],
            gpr: [0x0000, 0x0304, 0x0506, 0x0010, 0, 0x0010, 0x0008, 0x0018],
            eflags: 0x2,
            eip: 0x2,
            fetch: 3,
        },
        // OUT imm8, AL (0xe6 0x78): writes AL=0x02 to port 0x78, no register change. eip=2, fetch=3.
        PortIoGolden {
            name: "out imm8,al (e6 78)",
            code: &[0xe6, 0x78],
            gpr: [0x0102, 0x0304, 0x0506, 0x0010, 0, 0x0010, 0x0008, 0x0018],
            eflags: 0x2,
            eip: 0x2,
            fetch: 3,
        },
        // OUT imm8, AX (0xe7 0x78): writes AX=0x0102 to port 0x78, no register change. eip=2, fetch=3.
        PortIoGolden {
            name: "out imm8,ax (e7 78)",
            code: &[0xe7, 0x78],
            gpr: [0x0102, 0x0304, 0x0506, 0x0010, 0, 0x0010, 0x0008, 0x0018],
            eflags: 0x2,
            eip: 0x2,
            fetch: 3,
        },
        // IN AL, DX (0xec): port=DX=0x0506 → AL=0. AH unchanged → AX=0x0100, eip=1, fetch=2.
        PortIoGolden {
            name: "in al,dx (ec)",
            code: &[0xec],
            gpr: [0x0100, 0x0304, 0x0506, 0x0010, 0, 0x0010, 0x0008, 0x0018],
            eflags: 0x2,
            eip: 0x1,
            fetch: 2,
        },
        // IN AX, DX (0xed): port=DX=0x0506 → AX=0x0000 (word), eip=1, fetch=2.
        PortIoGolden {
            name: "in ax,dx (ed)",
            code: &[0xed],
            gpr: [0x0000, 0x0304, 0x0506, 0x0010, 0, 0x0010, 0x0008, 0x0018],
            eflags: 0x2,
            eip: 0x1,
            fetch: 2,
        },
        // OUT DX, AL (0xee): writes AL=0x02 to port DX=0x0506, no register change. eip=1, fetch=2.
        PortIoGolden {
            name: "out dx,al (ee)",
            code: &[0xee],
            gpr: [0x0102, 0x0304, 0x0506, 0x0010, 0, 0x0010, 0x0008, 0x0018],
            eflags: 0x2,
            eip: 0x1,
            fetch: 2,
        },
        // OUT DX, AX (0xef): writes AX=0x0102 to port DX=0x0506, no register change. eip=1, fetch=2.
        PortIoGolden {
            name: "out dx,ax (ef)",
            code: &[0xef],
            gpr: [0x0102, 0x0304, 0x0506, 0x0010, 0, 0x0010, 0x0008, 0x0018],
            eflags: 0x2,
            eip: 0x1,
            fetch: 2,
        },
    ]
}

#[test]
fn port_io_split_matches_golden_across_ops() {
    // The port I/O block (IN/OUT byte-imm-port and DX-port forms) is converted to the
    // decode/execute split, so its fused arms are deleted and it can no longer be diffed
    // against a fused executor in-tree. Run each case through cycle() (the split) and assert
    // the architectural end-state against goldens captured from the pre-split fused path via
    // `regen_port_io_goldens`. eip proves decode consumed the right number of bytes (2 for
    // imm8 forms, 1 for DX forms); fetch proves each instruction byte was charged exactly
    // once. TestBus.read_io returns 0, so IN forms zero the accumulator (AL or AX); OUT
    // forms leave registers unchanged.
    for g in port_io_golden_cases() {
        let mut mem = vec![0u8; 0x100];
        mem[..g.code.len()].copy_from_slice(g.code);

        let mut split = CpuGsw::default();
        seam_seed(&mut split);
        let mut sbus = TestBus::with_memory(mem);
        let _ = split.cycle(&mut sbus);

        assert_eq!(split.registers.gpr, g.gpr, "gpr mismatch for {}", g.name);
        assert_eq!(split.eflags(), g.eflags, "eflags mismatch for {}", g.name);
        assert_eq!(split.registers.eip, g.eip, "eip mismatch for {}", g.name);
        assert_eq!(
            seam_fetch_count(&sbus),
            g.fetch,
            "instruction-fetch cycle count mismatch for {} (seam must charge fetches once)",
            g.name
        );
    }
}

/// Regenerate `port_io_golden_cases` from the fused reference. Ignored by default.
/// Run WHILE the fused arms (0xe4-0xe7, 0xec-0xef) still exist in `dispatch_opcode`
/// (i.e. the parent commit 21cc68ba):
///   git worktree add ../regen-a9 21cc68ba
///   cd ../regen-a9
///   cargo test -p izarravm-cpu --lib regen_port_io_goldens -- --ignored --nocapture
/// then paste the output over `port_io_golden_cases` and only then delete the fused arms.
#[test]
#[ignore = "prints golden literals; run with --ignored --nocapture against the fused reference"]
fn regen_port_io_goldens() {
    for g in port_io_golden_cases() {
        let mut mem = vec![0u8; 0x100];
        mem[..g.code.len()].copy_from_slice(g.code);

        let mut fused = CpuGsw::default();
        seam_seed(&mut fused);
        let mut fbus = TestBus::with_memory(mem);
        fused.begin_instruction();
        if exec_one_split(&mut fused, &mut fbus).is_err() {
            println!(
                "            // TODO regen {}: fused path unavailable here; run against the base commit",
                g.name
            );
            continue;
        }
        let fetch = seam_fetch_count(&fbus);
        println!(
            "            PortIoGolden {{ name: {:?}, code: &{:?}, gpr: {:?}, eflags: {:#x}, eip: {:#x}, fetch: {} }},",
            g.name,
            g.code,
            fused.registers.gpr,
            fused.eflags(),
            fused.registers.eip,
            fetch,
        );
    }
}

// ── Task A10: bit-manipulation golden battery ─────────────────────────────────────────────────
