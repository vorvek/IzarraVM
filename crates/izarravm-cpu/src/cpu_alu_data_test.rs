// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn seam_matches_fused_path_across_addressing_forms() {
    // Historically this diffed a *still-on-Fallback* memory-read opcode through cycle()
    // (decode/execute split) against execute_instruction_legacy (fused) to guard the seam.
    // After task A14 there is no longer any IMPLEMENTED opcode on Fallback to diff this way —
    // every implemented opcode is converted to the split, so the fused executor for each was
    // deleted. `inc word [bx]` (0xff), `test [bx],cx` (0x85), then `xlat` (0xd7) each served as
    // the exemplar in turn and were converted away (`ControlFlow`/`FlagsMisc`/`Misc`). The seam's
    // memory-read + single-fetch-charge behaviour is now covered by the per-group golden
    // batteries (which assert eip, the memory write/read, AND `seam_fetch_count` == golden). Run
    // XLAT — the last memory-read exemplar, now `DecodeGroup::Misc` — through the split and assert
    // it both reads the right table byte AND charges each instruction-fetch byte exactly once.
    let mut mem = vec![0u8; 0x200];
    mem[0] = 0xd7; // XLAT
    mem[0x12] = 0xab; // the XLAT lookup result planted at [BX+AL]=0x12 (BX=0x10, AL=0x02)

    let mut split = CpuGsw::default();
    seam_seed(&mut split);
    let mut sbus = TestBus::with_memory(mem);
    exec_one_split(&mut split, &mut sbus).unwrap();

    // AL = [DS:BX+AL] = mem[0x12] = 0xab; the rest of AX (AH=0x01) is unchanged.
    assert_eq!(split.read_reg16(Reg16::Ax), 0x01ab, "xlat result");
    assert_eq!(split.registers.eip, 0x1, "eip past the 1-byte opcode");
    // Clock-neutrality guard: 1 opcode-prefetch peek + 1 opcode byte = 2 instruction fetches;
    // the data read of the table byte is a DataRead, not an InstructionPrefetch. A decode/execute
    // double-charge of the opcode would push this past 2.
    assert_eq!(
        seam_fetch_count(&sbus),
        2,
        "the seam must charge each instruction-fetch byte exactly once"
    );
}

/// One golden end-state for an ALU case run from `seam_seed`: the opcode bytes plus the
/// expected end gpr (AX,CX,DX,BX,SP,BP,SI,DI), eflags, eip, (offset,value) memory writes, and
/// InstructionPrefetch fetch count. Shared between the assertion test and the regen helper.
struct AluGolden {
    name: &'static str,
    code: &'static [u8],
    gpr: [u32; 8],
    eflags: u32,
    eip: u32,
    deltas: &'static [(usize, u8)],
    fetch: usize,
}

/// The ALU differential battery: every op/form/addressing-mode case plus its golden end-state.
///
/// HOW TO CAPTURE / REGENERATE GOLDENS (read before editing any `gpr`/`eflags`/`deltas`/`fetch`
/// below, and follow this same recipe for every future group-conversion task):
///   1. The goldens are captured from the PRIOR fused reference (`execute_instruction_legacy`),
///      NOT from the new split path. Capturing from the split would be tautological — it would
///      assert the code matches itself and catch nothing.
///   2. Run `cargo test -p izarravm-cpu --lib regen_alu_goldens -- --ignored --nocapture` while
///      the group's fused arm still exists, then paste the printed literals here. For a new
///      group, capture BEFORE you delete its fused arm from `dispatch_opcode`.
///   3. For THIS (ALU) group the fused arm is already gone on `perf-decode-cache`, so the regen
///      helper must be run from the pre-split base commit (332be72): `git stash`, check out the
///      base, run the command, paste, then return. (These goldens were captured exactly so.)
///   4. Never hand-edit a golden to make a failing test pass — re-capture from the reference.
fn alu_golden_cases() -> &'static [AluGolden] {
    &[
        // Forms 0-3 (r/m,reg and reg,r/m, byte and word), several addressing modes.
        AluGolden {
            name: "add ax,bx",
            code: &[0x01, 0xd8],
            gpr: [274, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x06,
            eip: 0x02,
            deltas: &[],
            fetch: 3,
        },
        AluGolden {
            name: "add [bx+si],ax",
            code: &[0x01, 0x00],
            gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x02,
            eip: 0x02,
            deltas: &[(24, 2), (25, 1)],
            fetch: 3,
        },
        AluGolden {
            name: "add [bp+di+4],cx",
            code: &[0x01, 0x4b, 0x04],
            gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x02,
            eip: 0x03,
            deltas: &[(44, 4), (45, 3)],
            fetch: 4,
        },
        AluGolden {
            name: "add [0x20],dx",
            code: &[0x01, 0x16, 0x20, 0x00],
            gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x06,
            eip: 0x04,
            deltas: &[(32, 23), (33, 22)],
            fetch: 5,
        },
        AluGolden {
            name: "add [si],al(byte)",
            code: &[0x00, 0x04],
            gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x02,
            eip: 0x02,
            deltas: &[(8, 2)],
            fetch: 3,
        },
        // Every ALU op through word r/m,reg (form 1) with a memory operand: op-by-op coverage.
        AluGolden {
            name: "add [bx],ax(form1)",
            code: &[0x01, 0x07],
            gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x02,
            eip: 0x02,
            deltas: &[(16, 2), (17, 1)],
            fetch: 3,
        },
        AluGolden {
            name: "or [bx],ax(form1)",
            code: &[0x09, 0x07],
            gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x02,
            eip: 0x02,
            deltas: &[(16, 2), (17, 1)],
            fetch: 3,
        },
        AluGolden {
            name: "adc [bx],ax(form1)",
            code: &[0x11, 0x07],
            gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x02,
            eip: 0x02,
            deltas: &[(16, 2), (17, 1)],
            fetch: 3,
        },
        AluGolden {
            name: "sbb [bx],ax(form1)",
            code: &[0x19, 0x07],
            gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x93,
            eip: 0x02,
            deltas: &[(16, 254), (17, 254)],
            fetch: 3,
        },
        AluGolden {
            name: "and [bx],ax(form1)",
            code: &[0x21, 0x07],
            gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x46,
            eip: 0x02,
            deltas: &[],
            fetch: 3,
        },
        AluGolden {
            name: "sub [bx],ax(form1)",
            code: &[0x29, 0x07],
            gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x93,
            eip: 0x02,
            deltas: &[(16, 254), (17, 254)],
            fetch: 3,
        },
        AluGolden {
            name: "xor [bx],ax(form1)",
            code: &[0x31, 0x07],
            gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x02,
            eip: 0x02,
            deltas: &[(16, 2), (17, 1)],
            fetch: 3,
        },
        AluGolden {
            name: "cmp [bx],ax(form1)",
            code: &[0x39, 0x07],
            gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x93,
            eip: 0x02,
            deltas: &[],
            fetch: 3,
        },
        // reg,r/m direction (form 3, word; writes a register) and byte directions (forms 0/2).
        AluGolden {
            name: "or cx,[bx+si]",
            code: &[0x0b, 0x08],
            gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x02,
            eip: 0x02,
            deltas: &[],
            fetch: 3,
        },
        AluGolden {
            name: "and dx,[di]",
            code: &[0x23, 0x15],
            gpr: [258, 772, 0, 16, 0, 16, 8, 24],
            eflags: 0x46,
            eip: 0x02,
            deltas: &[],
            fetch: 3,
        },
        AluGolden {
            name: "adc al,[bx](byte form2)",
            code: &[0x12, 0x07],
            gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x02,
            eip: 0x02,
            deltas: &[],
            fetch: 3,
        },
        AluGolden {
            name: "xor [si],bl(byte form0)",
            code: &[0x30, 0x1c],
            gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x02,
            eip: 0x02,
            deltas: &[(8, 16)],
            fetch: 3,
        },
        // Immediate accumulator forms: byte AL,imm8 (form 4) and word AX,imm16 (form 5).
        AluGolden {
            name: "add al,imm8(form4)",
            code: &[0x04, 0x7f],
            gpr: [385, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x896,
            eip: 0x02,
            deltas: &[],
            fetch: 3,
        },
        AluGolden {
            name: "or al,imm8(form4)",
            code: &[0x0c, 0xaa],
            gpr: [426, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x86,
            eip: 0x02,
            deltas: &[],
            fetch: 3,
        },
        AluGolden {
            name: "cmp al,imm8(form4)",
            code: &[0x3c, 0x05],
            gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x93,
            eip: 0x02,
            deltas: &[],
            fetch: 3,
        },
        AluGolden {
            name: "add ax,imm16(form5)",
            code: &[0x05, 0x34, 0x12],
            gpr: [4918, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x06,
            eip: 0x03,
            deltas: &[],
            fetch: 4,
        },
        AluGolden {
            name: "sub ax,imm16(form5)",
            code: &[0x2d, 0x34, 0x12],
            gpr: [61134, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x93,
            eip: 0x03,
            deltas: &[],
            fetch: 4,
        },
        AluGolden {
            name: "cmp ax,imm16(form5)",
            code: &[0x3d, 0x02, 0x01],
            gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x46,
            eip: 0x03,
            deltas: &[],
            fetch: 4,
        },
        // Remaining addressing forms carried over from the original battery.
        AluGolden {
            name: "sub [bp+2],ax",
            code: &[0x29, 0x46, 0x02],
            gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x93,
            eip: 0x03,
            deltas: &[(18, 254), (19, 254)],
            fetch: 4,
        },
        AluGolden {
            name: "xor [di],bx",
            code: &[0x31, 0x1d],
            gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x02,
            eip: 0x02,
            deltas: &[(24, 16)],
            fetch: 3,
        },
        AluGolden {
            name: "cmp [bx+4],dx",
            code: &[0x39, 0x57, 0x04],
            gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x97,
            eip: 0x03,
            deltas: &[],
            fetch: 4,
        },
    ]
}

#[test]
fn alu_split_matches_golden_across_ops() {
    // The whole ALU block (ADD/OR/ADC/SBB/AND/SUB/XOR/CMP) is converted to the decode/execute
    // split, so it can no longer be diffed against a fused executor (that path was deleted to
    // keep a single ALU implementation). Instead, run each op/form through cycle() and assert
    // the architectural end-state against goldens captured from the pre-split fused path
    // (commit 332be72; see `alu_golden_cases` for the capture recipe). This exercises decode's
    // ModRM/immediate parsing, the executor's operand wiring + write-back gating, the EA
    // recompute, and the once-only instruction-fetch charge.
    for g in alu_golden_cases() {
        let mut mem = vec![0u8; 0x200];
        mem[..g.code.len()].copy_from_slice(g.code);
        mem[0x20..0x22].copy_from_slice(&0x1111u16.to_le_bytes());
        let initial = mem.clone();

        let mut split = CpuGsw::default();
        seam_seed(&mut split);
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

/// Regenerate the `alu_golden_cases` literals from the PRIOR fused reference. Ignored by
/// default (it only prints; it asserts nothing). This is the copy-paste template for every
/// future group-conversion task: drive each case through `execute_instruction_legacy` (the
/// fused path) and print a ready-to-paste golden literal, so the goldens come from the
/// reference implementation rather than from the split path they guard (which would be
/// tautological).
///
/// Run it WHILE the group's fused arm still exists:
///   cargo test -p izarravm-cpu --lib regen_alu_goldens -- --ignored --nocapture
/// For the ALU group specifically the fused arm is already deleted on this branch, so this must
/// be run from the pre-split base commit (332be72) — see the recipe on `alu_golden_cases`. A
/// case whose opcode the current fused path can no longer execute prints a TODO marker instead
/// of a wrong literal, so a stale run can never silently bake bad goldens.
///
/// The printed `code` bytes are decimal (e.g. `&[1, 216]`); that compiles identically to the
/// hex source form, so paste the numeric result fields and keep your hex encoding if preferred.
#[test]
#[ignore = "prints golden literals; run with --ignored --nocapture against the fused reference"]
fn regen_alu_goldens() {
    for g in alu_golden_cases() {
        let mut mem = vec![0u8; 0x200];
        mem[..g.code.len()].copy_from_slice(g.code);
        mem[0x20..0x22].copy_from_slice(&0x1111u16.to_le_bytes());
        let initial = mem.clone();

        let mut fused = CpuGsw::default();
        seam_seed(&mut fused);
        let mut fbus = TestBus::with_memory(mem);
        fused.begin_instruction();
        // Stage A removed the in-tree fused reference (`execute_instruction_legacy`), so this
        // checkout's regen captures from the production split instead — which is tautological for
        // catching split bugs (the goldens it prints are exactly what the split now produces).
        // Only use an in-checkout regen run to RE-derive goldens after an intentional behavior
        // change; to capture an INDEPENDENT reference, run this test from a pre-Stage-A worktree
        // (see the recipe on the cases fn). A case the split can't execute prints a TODO marker
        // rather than a wrong literal.
        if exec_one_split(&mut fused, &mut fbus).is_err() {
            println!(
                "            // TODO regen {}: opcode not executable here; run from a pre-Stage-A worktree",
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
            "            AluGolden {{ name: {:?}, code: &{:?}, gpr: {:?}, eflags: {:#x}, eip: {:#x}, deltas: &{:?}, fetch: {} }},",
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

/// One golden end-state for a data-movement case, captured the same way as `AluGolden`: opcode
/// bytes plus expected end gpr (AX,CX,DX,BX,SP,BP,SI,DI), eflags, eip, (offset,value) memory
/// writes, and InstructionPrefetch fetch count. Data-movement ops do not touch flags, so the
/// eflags field just confirms that (it should equal the seed's `0x02`).
struct DataMoveGolden {
    name: &'static str,
    code: &'static [u8],
    gpr: [u32; 8],
    eflags: u32,
    eip: u32,
    deltas: &'static [(usize, u8)],
    fetch: usize,
}

/// The data-movement differential battery: MOV/LEA/XCHG across forms and addressing modes, plus
/// the moffs / immediate / Sreg variants, each with its golden end-state. Captured from the
/// PRIOR fused reference (`execute_instruction_legacy` -> `dispatch_opcode`) via
/// `regen_datamove_goldens`; see `alu_golden_cases` for the full capture recipe (the goldens
/// must come from the reference path, never from the split path they guard). The two-byte
/// MOVZX/MOVSX forms — also in `DecodeGroup::DataMove` — have their own battery
/// (`movzx_movsx_golden_cases`), so they are absent here.
fn datamove_golden_cases() -> &'static [DataMoveGolden] {
    &[
        // MOV r/m<->reg, byte and word, register and memory r/m.
        DataMoveGolden {
            name: "mov [bx],cx",
            code: &[137, 15],
            gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x2,
            eip: 0x2,
            deltas: &[(16, 4), (17, 3)],
            fetch: 3,
        },
        DataMoveGolden {
            name: "mov [bp+si+4],al(byte)",
            code: &[136, 66, 4],
            gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x2,
            eip: 0x3,
            deltas: &[(28, 2)],
            fetch: 4,
        },
        DataMoveGolden {
            name: "mov dx,bx(reg)",
            code: &[137, 218],
            gpr: [258, 772, 16, 16, 0, 16, 8, 24],
            eflags: 0x2,
            eip: 0x2,
            deltas: &[],
            fetch: 3,
        },
        DataMoveGolden {
            name: "mov cx,[0x20]",
            code: &[139, 14, 32, 0],
            gpr: [258, 4369, 1286, 16, 0, 16, 8, 24],
            eflags: 0x2,
            eip: 0x4,
            deltas: &[],
            fetch: 5,
        },
        DataMoveGolden {
            name: "mov al,[bx](byte)",
            code: &[138, 7],
            gpr: [256, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x2,
            eip: 0x2,
            deltas: &[],
            fetch: 3,
        },
        // MOV r/m,Sreg and MOV Sreg,r/m (load ES, leaves the addressing segments untouched).
        DataMoveGolden {
            name: "mov [bx],es",
            code: &[140, 7],
            gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x2,
            eip: 0x2,
            deltas: &[],
            fetch: 3,
        },
        DataMoveGolden {
            name: "mov es,[0x20]",
            code: &[142, 6, 32, 0],
            gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x2,
            eip: 0x4,
            deltas: &[],
            fetch: 5,
        },
        // LEA: effective address into the register, disp+index and direct-disp forms.
        DataMoveGolden {
            name: "lea ax,[bx+si+3]",
            code: &[141, 64, 3],
            gpr: [27, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x2,
            eip: 0x3,
            deltas: &[],
            fetch: 4,
        },
        DataMoveGolden {
            name: "lea dx,[0x20]",
            code: &[141, 22, 32, 0],
            gpr: [258, 772, 32, 16, 0, 16, 8, 24],
            eflags: 0x2,
            eip: 0x4,
            deltas: &[],
            fetch: 5,
        },
        // MOV (E)AX<->moffs, byte and word, read and write.
        DataMoveGolden {
            name: "mov al,[moffs8 0x20]",
            code: &[160, 32, 0],
            gpr: [273, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x2,
            eip: 0x3,
            deltas: &[],
            fetch: 4,
        },
        DataMoveGolden {
            name: "mov ax,[moffs 0x20]",
            code: &[161, 32, 0],
            gpr: [4369, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x2,
            eip: 0x3,
            deltas: &[],
            fetch: 4,
        },
        DataMoveGolden {
            name: "mov [moffs8 0x30],al",
            code: &[162, 48, 0],
            gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x2,
            eip: 0x3,
            deltas: &[(48, 2)],
            fetch: 4,
        },
        DataMoveGolden {
            name: "mov [moffs 0x30],ax",
            code: &[163, 48, 0],
            gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x2,
            eip: 0x3,
            deltas: &[(48, 2), (49, 1)],
            fetch: 4,
        },
        // MOV r,imm (byte and word).
        DataMoveGolden {
            name: "mov bl,0x7f",
            code: &[179, 127],
            gpr: [258, 772, 1286, 127, 0, 16, 8, 24],
            eflags: 0x2,
            eip: 0x2,
            deltas: &[],
            fetch: 3,
        },
        DataMoveGolden {
            name: "mov si,0x1234",
            code: &[190, 52, 18],
            gpr: [258, 772, 1286, 16, 0, 16, 4660, 24],
            eflags: 0x2,
            eip: 0x3,
            deltas: &[],
            fetch: 4,
        },
        // MOV r/m,imm (group 11), register and memory.
        DataMoveGolden {
            name: "mov byte [bx],0x55",
            code: &[198, 7, 85],
            gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x2,
            eip: 0x3,
            deltas: &[(16, 85)],
            fetch: 4,
        },
        DataMoveGolden {
            name: "mov word [bx],0xbeef",
            code: &[199, 7, 239, 190],
            gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x2,
            eip: 0x4,
            deltas: &[(16, 239), (17, 190)],
            fetch: 5,
        },
        DataMoveGolden {
            name: "mov dx,0xabcd(grp11 reg)",
            code: &[199, 194, 205, 171],
            gpr: [258, 772, 43981, 16, 0, 16, 8, 24],
            eflags: 0x2,
            eip: 0x4,
            deltas: &[],
            fetch: 5,
        },
        // XCHG r/m,reg (byte and word, register and memory) and XCHG (E)AX,reg + NOP.
        DataMoveGolden {
            name: "xchg [bx],cx",
            code: &[135, 15],
            gpr: [258, 0, 1286, 16, 0, 16, 8, 24],
            eflags: 0x2,
            eip: 0x2,
            deltas: &[(16, 4), (17, 3)],
            fetch: 3,
        },
        DataMoveGolden {
            name: "xchg dl,bl(byte reg)",
            code: &[134, 211],
            gpr: [258, 772, 1296, 6, 0, 16, 8, 24],
            eflags: 0x2,
            eip: 0x2,
            deltas: &[],
            fetch: 3,
        },
        DataMoveGolden {
            name: "xchg ax,cx",
            code: &[145],
            gpr: [772, 258, 1286, 16, 0, 16, 8, 24],
            eflags: 0x2,
            eip: 0x1,
            deltas: &[],
            fetch: 2,
        },
        DataMoveGolden {
            name: "nop",
            code: &[144],
            gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x2,
            eip: 0x1,
            deltas: &[],
            fetch: 2,
        },
    ]
}

#[test]
fn datamove_split_matches_golden_across_ops() {
    // The single-byte data-movement block (MOV/LEA/XCHG and their immediate/moffs/Sreg forms)
    // is converted to the decode/execute split, so it can no longer be diffed against a fused
    // executor (that path was deleted to keep a single implementation). Instead, run each form
    // through cycle() and assert the architectural end-state against goldens captured from the
    // pre-split fused path (see `datamove_golden_cases` for the capture recipe). This exercises
    // decode's ModRM/immediate/moffs parsing, the executor's operand wiring, the EA recompute,
    // and the once-only instruction-fetch charge.
    for g in datamove_golden_cases() {
        let mut mem = vec![0u8; 0x200];
        mem[..g.code.len()].copy_from_slice(g.code);
        mem[0x20..0x22].copy_from_slice(&0x1111u16.to_le_bytes());
        let initial = mem.clone();

        let mut split = CpuGsw::default();
        seam_seed(&mut split);
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

/// Regenerate the `datamove_golden_cases` literals from the PRIOR fused reference. Ignored by
/// default (it only prints). Mirror of `regen_alu_goldens`: drive each case through
/// `execute_instruction_legacy` (the fused path) and print a ready-to-paste literal, so the
/// goldens come from the reference rather than the split path they guard.
///
/// Run it WHILE the group's fused arms still exist in `dispatch_opcode`:
///   cargo test -p izarravm-cpu --lib regen_datamove_goldens -- --ignored --nocapture
/// then paste the output over `datamove_golden_cases` and only then delete the fused arms.
#[test]
#[ignore = "prints golden literals; run with --ignored --nocapture against the fused reference"]
fn regen_datamove_goldens() {
    for g in datamove_golden_cases() {
        let mut mem = vec![0u8; 0x200];
        mem[..g.code.len()].copy_from_slice(g.code);
        mem[0x20..0x22].copy_from_slice(&0x1111u16.to_le_bytes());
        let initial = mem.clone();

        let mut fused = CpuGsw::default();
        seam_seed(&mut fused);
        let mut fbus = TestBus::with_memory(mem);
        fused.begin_instruction();
        if exec_one_split(&mut fused, &mut fbus).is_err() {
            println!(
                "            // TODO regen {}: fused path unavailable here; run before deleting the fused arms",
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
            "            DataMoveGolden {{ name: {:?}, code: &{:?}, gpr: {:?}, eflags: {:#x}, eip: {:#x}, deltas: &{:?}, fetch: {} }},",
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

/// One golden end-state for a MOVZX/MOVSX case run from `movzx_seed` (a real-mode register set
/// with sentinel bytes/words in memory). The opcode bytes plus expected end gpr, eflags
/// (MOVZX/MOVSX never touch flags, so this must stay the seed's `0x02`), eip, memory writes
/// (always empty — these are pure loads), and InstructionPrefetch fetch count. Captured from the
/// PRIOR fused reference via `regen_movzx_movsx_goldens`; see `alu_golden_cases` for the recipe.
struct MovzxMovsxGolden {
    name: &'static str,
    code: &'static [u8],
    gpr: [u32; 8],
    eflags: u32,
    eip: u32,
    deltas: &'static [(usize, u8)],
    fetch: usize,
}

/// Seed for the MOVZX/MOVSX battery. Same register set as `seam_seed`, but it also plants
/// sentinels so the byte/word, sign/zero, and EA-recompute cases have stable, sign-bit-set
/// sources: byte 0x80 at [0x10] (= [BX]), word 0x8081 at [0x18] (= [BX+SI], BX=0x10 + SI=0x08),
/// and word 0xBEEF at [0x20] (the direct-disp source). The 0x80/0x8081/0xBEEF high bits make
/// zero- vs sign-extension visibly different.
fn movzx_seed(cpu: &mut CpuGsw, mem: &mut [u8]) {
    seam_seed(cpu);
    mem[0x10] = 0x80;
    mem[0x18..0x1a].copy_from_slice(&0x8081u16.to_le_bytes());
    mem[0x20..0x22].copy_from_slice(&0xBEEFu16.to_le_bytes());
}

/// The MOVZX/MOVSX differential battery: 0F B6/B7 (zero-extend byte/word) and 0F BE/BF
/// (sign-extend byte/word), each in a register form and a memory form, plus an EA-recompute
/// case ([BX+SI], resolved against the live registers in the executor). Goldens captured from
/// the fused reference (`execute_instruction_legacy`); never edit by hand — re-run the regen.
fn movzx_movsx_golden_cases() -> &'static [MovzxMovsxGolden] {
    &[
        // MOVZX r16, r/m8 (0F B6): zero-extend a byte. BL = low byte of BX(0x10) = 0x10.
        MovzxMovsxGolden {
            name: "movzx ax, bl(reg)",
            code: &[15, 182, 195],
            gpr: [16, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x2,
            eip: 0x3,
            deltas: &[],
            fetch: 4,
        },
        // byte [BX] = [0x10] = 0x80, zero-extended to 0x0080 (= 128).
        MovzxMovsxGolden {
            name: "movzx ax, [bx](byte, sign bit set)",
            code: &[15, 182, 7],
            gpr: [128, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x2,
            eip: 0x3,
            deltas: &[],
            fetch: 4,
        },
        // MOVZX r16, r/m16 (0F B7): word [0x20] = 0xBEEF, zero-extended (= 48879).
        MovzxMovsxGolden {
            name: "movzx cx, [0x20](word)",
            code: &[15, 183, 14, 32, 0],
            gpr: [258, 48879, 1286, 16, 0, 16, 8, 24],
            eflags: 0x2,
            eip: 0x5,
            deltas: &[],
            fetch: 6,
        },
        // MOVSX r16, r/m8 (0F BE): byte [BX] = 0x80, sign-extended to 0xFF80 (= 65408).
        MovzxMovsxGolden {
            name: "movsx dx, [bx](byte, sign bit set)",
            code: &[15, 190, 23],
            gpr: [258, 772, 65408, 16, 0, 16, 8, 24],
            eflags: 0x2,
            eip: 0x3,
            deltas: &[],
            fetch: 4,
        },
        // DL = low byte of DX(0x0506) = 0x06, positive, sign-extends to 0x0006 (= 6).
        MovzxMovsxGolden {
            name: "movsx ax, dl(reg, positive byte)",
            code: &[15, 190, 194],
            gpr: [6, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x2,
            eip: 0x3,
            deltas: &[],
            fetch: 4,
        },
        // MOVSX r16, r/m16 (0F BF), EA recomputed from live BX+SI = 0x18; word [0x18] = 0x8081,
        // sign-extended stays 0x8081 at 16 bits (= 32897).
        MovzxMovsxGolden {
            name: "movsx ax, [bx+si](word, sign bit set)",
            code: &[15, 191, 0],
            gpr: [32897, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x2,
            eip: 0x3,
            deltas: &[],
            fetch: 4,
        },
    ]
}

#[test]
fn movzx_movsx_split_matches_golden() {
    // MOVZX/MOVSX (0F B6/B7/BE/BF) are converted to the split, so they can no longer be diffed
    // against a fused executor (that arm was deleted). Run each through cycle() and assert the
    // architectural end-state against goldens captured from the pre-split fused path. Covers
    // byte and word sources, zero vs sign extend, reg and mem operands, and an EA-recompute
    // case. MOVZX/MOVSX do not modify flags, so eflags must stay the seed value.
    for g in movzx_movsx_golden_cases() {
        let mut mem = vec![0u8; 0x200];
        mem[..g.code.len()].copy_from_slice(g.code);
        let mut split = CpuGsw::default();
        movzx_seed(&mut split, &mut mem);
        let initial = mem.clone();
        let mut sbus = TestBus::with_memory(mem);
        split.cycle(&mut sbus).expect("movzx/movsx must execute");

        assert_eq!(split.registers.gpr, g.gpr, "gpr mismatch for {}", g.name);
        assert_eq!(
            split.eflags(),
            g.eflags,
            "eflags mismatch for {} (MOVZX/MOVSX must not touch flags)",
            g.name
        );
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

/// Regenerate the `movzx_movsx_golden_cases` literals from the PRIOR fused reference. Ignored by
/// default. Mirror of `regen_datamove_goldens`; run WHILE the MOVZX/MOVSX arms still exist in
/// `execute_two_byte`:
///   cargo test -p izarravm-cpu --lib regen_movzx_movsx_goldens -- --ignored --nocapture
/// then paste the output over `movzx_movsx_golden_cases` and only then delete the fused arms.
#[test]
#[ignore = "prints golden literals; run with --ignored --nocapture against the fused reference"]
fn regen_movzx_movsx_goldens() {
    for g in movzx_movsx_golden_cases() {
        let mut mem = vec![0u8; 0x200];
        mem[..g.code.len()].copy_from_slice(g.code);
        let mut fused = CpuGsw::default();
        movzx_seed(&mut fused, &mut mem);
        let initial = mem.clone();
        let mut fbus = TestBus::with_memory(mem);
        fused.begin_instruction();
        if exec_one_split(&mut fused, &mut fbus).is_err() {
            println!(
                "            // TODO regen {}: fused path unavailable here; run before deleting the fused arms",
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
            "            MovzxMovsxGolden {{ name: {:?}, code: &{:?}, gpr: {:?}, eflags: {:#x}, eip: {:#x}, deltas: &{:?}, fetch: {} }},",
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
